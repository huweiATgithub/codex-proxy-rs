//! AttemptContext 驱动的 Codex 账号选择与 Redis lease port

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use gateway_core::account::affinity::{
    self, AccountBinding, AffinityAccountState, AffinityResolution, AffinitySnapshot,
    AffinitySwitchReason, AffinityUpdate,
};
use gateway_core::account::{
    AccountCandidate, AccountCapacitySnapshot, AccountEligibilityPolicy, AccountErrorReason,
    AccountFeedbackStats, AccountRuntimeSignals, AccountSelectionContext, AccountSelector,
    AccountStatus, CredentialState, ProviderAccount, ProviderAccountId, QuotaEvidence,
};
use gateway_core::concurrency::{CapacityWait, ConcurrencyWaitQueue, QueueRejection, WaitPriority};
use gateway_core::engine::{AttemptContext, ContinuationAttempt, policy::AccountPolicyError};
use gateway_core::provider_ports::{
    ProviderLeaseAcquisition, ProviderLeaseGuard, ProviderLeasePort, ProviderLeaseRequest,
    ProviderSchedulingLeaseRequest, ProviderSessionAffinityKey, ProviderSessionAffinityPort,
    ProviderSessionExclusionPort, ProviderSessionExclusions, ProviderStoreError,
};
use gateway_core::routing::ProviderKind;
use secrecy::ExposeSecret;
use thiserror::Error;
use url::Url;

use super::affinity::CodexSessionAffinity;
use super::cookie::CodexCookiePolicy;
use super::quota::CodexCredentialQuotaService;
use super::refresh::refresh_recovery_deadline;
use super::repository::{CodexCredentialRepository, CredentialRepositoryError};
use super::security::CodexRuntimeAuthentication;
use super::types::{
    CODEX_AUTHENTICATION_KIND_OAUTH, CodexCookie, CodexCookieCaptureOutcome, RuntimeCodexCookie,
};

const CLOUDFLARE_RECOVERY_STALE_AFTER: Duration = Duration::from_secs(60 * 60);
const CLOUDFLARE_CHALLENGE_BACKOFF: [Duration; 4] = [
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(90),
    Duration::from_secs(120),
];
const CLOUDFLARE_PATH_BLOCK_THRESHOLD: u32 = 3;
const SESSION_AFFINITY_TIMEOUT: Duration = Duration::from_millis(100);
const CYBER_POLICY_SESSION_TTL: Duration = Duration::from_secs(60 * 60);
const MAX_ACCOUNT_SNAPSHOT_RETRIES: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexAccountFailure {
    /// Access token 已被上游明确判定为过期或失效
    CredentialExpired,
    /// 账号需要完成身份验证后才能继续使用
    IdentityVerificationRequired,
    /// 账号、workspace 或 organization 已被封禁或停用
    Banned,
    /// 账号信用额度已耗尽
    QuotaExhausted,
    /// 当前用量窗口已耗尽；到重置时间后可自动恢复
    UsageLimitExhausted {
        /// 上游返回的窗口绝对重置时刻
        reset_at: Option<SystemTime>,
    },
    /// 账号触发临时用量限制
    RateLimited {
        /// 上游明确返回的最短冷却时长
        retry_after: Option<Duration>,
    },
    /// Cloudflare challenge 要求账号进入递增冷却
    CloudflareChallenge {
        /// 上游明确返回的最短冷却时长
        retry_after: Option<Duration>,
    },
    /// Cloudflare 对当前上游路径返回空 404
    CloudflarePathBlocked,
}

#[derive(Debug, Clone, Copy)]
struct RiskRecoveryState {
    challenge_count: u32,
    path_block_count: u32,
    observed_at: SystemTime,
}

#[derive(Debug, Clone, Copy)]
enum CookieRecovery {
    ExpireAt(SystemTime),
    Clear,
}

pub struct SelectCodexCredential<'a> {
    pub upstream_model: &'a str,
    pub request_url: &'a Url,
    pub attempt: &'a AttemptContext,
    pub session_affinity_key: Option<&'a ProviderSessionAffinityKey>,
}

pub(crate) struct SelectCodexProviderEndpointCredential<'a> {
    pub request_url: &'a Url,
    pub attempt: &'a AttemptContext,
    pub session_affinity: Option<&'a CodexSessionAffinity>,
    /// 本端点的上游模型；提供后按账号模型权限过滤候选（如 live 语音）。
    pub upstream_model: Option<&'a str>,
    /// 本端点只接受 OAuth 凭据时排除 API Key 账号，避免混合池选中后必然失败。
    pub requires_oauth: bool,
}

struct CredentialSelectionInput<'a> {
    requires_websocket: bool,
    requires_oauth: bool,
    request_url: &'a Url,
    attempt: &'a AttemptContext,
    session_affinity_key: Option<&'a ProviderSessionAffinityKey>,
    session_affinity_observation: Option<&'a CodexSessionAffinity>,
    /// Codex Guardian 自动审批请求；仅在配置了预留名额时获得预留与队列优先
    guardian: bool,
}

#[derive(Clone)]
pub(crate) struct CodexCyberPolicyScope {
    key: ProviderSessionAffinityKey,
    state: Option<ProviderSessionExclusions>,
}

pub struct CodexCredentialSelector {
    waiting: ConcurrencyWaitQueue<ProviderAccountId>,
    provider_kind: ProviderKind,
    repository: CodexCredentialRepository,
    leases: Arc<dyn ProviderLeasePort>,
    session_affinity: Arc<dyn ProviderSessionAffinityPort>,
    session_exclusions: Arc<dyn ProviderSessionExclusionPort>,
    quota: Arc<CodexCredentialQuotaService>,
    cookie_policy: CodexCookiePolicy,
    risk_recovery: Mutex<HashMap<String, RiskRecoveryState>>,
    account_feedback: Arc<AccountFeedbackStats>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AffinityTelemetry {
    affinity_hit: bool,
    escape_reason: Option<AffinitySwitchReason>,
    account_switch: bool,
}

impl CodexCredentialSelector {
    #[must_use]
    // 选择器显式持有各能力边界，避免把 Provider 私有服务重新包装成通用容器
    #[expect(clippy::too_many_arguments)]
    pub fn new(
        provider_kind: ProviderKind,
        repository: CodexCredentialRepository,
        leases: Arc<dyn ProviderLeasePort>,
        session_affinity: Arc<dyn ProviderSessionAffinityPort>,
        session_exclusions: Arc<dyn ProviderSessionExclusionPort>,
        quota: Arc<CodexCredentialQuotaService>,
        account_feedback: Arc<AccountFeedbackStats>,
        cookie_policy: CodexCookiePolicy,
    ) -> Self {
        Self {
            provider_kind,
            repository,
            leases,
            session_affinity,
            session_exclusions,
            quota,
            cookie_policy,
            risk_recovery: Mutex::new(HashMap::new()),
            waiting: ConcurrencyWaitQueue::default(),
            account_feedback,
        }
    }

    pub async fn select(
        &self,
        request: &SelectCodexCredential<'_>,
    ) -> Result<CodexCredentialLease, CredentialSelectionError> {
        let input = CredentialSelectionInput {
            requires_websocket: false,
            requires_oauth: false,
            request_url: request.request_url,
            attempt: request.attempt,
            session_affinity_key: request.session_affinity_key,
            session_affinity_observation: None,
            guardian: false,
        };
        self.select_inner(&input, None, Some(request.upstream_model))
            .await
    }

    pub(crate) async fn select_with_cyber_policy(
        &self,
        request: &SelectCodexCredential<'_>,
        cyber_policy_session_key: Option<&ProviderSessionAffinityKey>,
        session_affinity_observation: Option<&CodexSessionAffinity>,
        requires_websocket: bool,
        guardian: bool,
    ) -> Result<CodexCredentialLease, CredentialSelectionError> {
        let input = CredentialSelectionInput {
            requires_websocket,
            requires_oauth: false,
            request_url: request.request_url,
            attempt: request.attempt,
            session_affinity_key: request.session_affinity_key,
            session_affinity_observation,
            guardian,
        };
        self.select_inner(
            &input,
            cyber_policy_session_key,
            Some(request.upstream_model),
        )
        .await
    }

    /// 中间件完成请求改写后，用真实 OpenAI 会话事实复验已持有的租约
    ///
    /// 此处只复用既有亲和与 cyber-policy 端口，不再次选号；冲突必须在发送前失败，
    /// 避免同一 attempt 持有旧租约时重入账号选择
    pub(crate) async fn validate_translated_selection(
        &self,
        lease: &mut CodexCredentialLease,
        session_affinity: Option<&CodexSessionAffinity>,
        cyber_policy_session_key: Option<&ProviderSessionAffinityKey>,
    ) -> Result<(), CredentialSelectionError> {
        let selected_account = lease.account.id().clone();
        if let Some(affinity) = session_affinity {
            let current = self.lookup_session_affinity(affinity.key()).await?;
            if current
                .as_ref()
                .is_some_and(|binding| binding.account_id() != &selected_account)
            {
                return Err(CredentialSelectionError::NoEligibleCredential);
            }
            let resolution = affinity::resolve(match current.as_ref() {
                Some(binding) => AffinitySnapshot::Bound {
                    binding,
                    state: AffinityAccountState::Retain,
                },
                None => AffinitySnapshot::Absent,
            });
            if !self
                .admit_session_affinity(affinity.key(), resolution, &selected_account)
                .await?
            {
                return Err(CredentialSelectionError::NoEligibleCredential);
            }
        }

        let cyber_policy_scope = if session_affinity.is_none() {
            self.prepare_cyber_policy_scope(cyber_policy_session_key)
                .await
        } else {
            None
        };
        if cyber_policy_scope
            .as_ref()
            .and_then(|scope| scope.state.as_ref())
            .is_some_and(|state| state.excluded_accounts().contains(&selected_account))
        {
            return Err(CredentialSelectionError::NoEligibleCredential);
        }
        lease.cyber_policy_scope = cyber_policy_scope;
        Ok(())
    }

    /// 为不属于 Responses 文本模型目录的 Provider 原生端点选择账号
    ///
    /// 账号范围、健康度、配额、并发租约、cookie 与认证准备仍走同一套选择链路；
    /// 原生端点默认没有 Responses 模型，不套用管理员配置的文本模型权限。
    /// 端点有明确上游模型（如 live 语音）时通过 `upstream_model` 让账号
    /// 模型权限参与候选过滤；`requires_oauth` 限定本端点支持的认证类型。
    pub(crate) async fn select_for_provider_endpoint(
        &self,
        request: &SelectCodexProviderEndpointCredential<'_>,
    ) -> Result<CodexCredentialLease, CredentialSelectionError> {
        let input = CredentialSelectionInput {
            requires_websocket: false,
            requires_oauth: request.requires_oauth,
            request_url: request.request_url,
            attempt: request.attempt,
            session_affinity_key: request.session_affinity.map(CodexSessionAffinity::key),
            session_affinity_observation: request.session_affinity,
            guardian: false,
        };
        self.select_inner(&input, None, request.upstream_model)
            .await
    }

    async fn select_inner(
        &self,
        request: &CredentialSelectionInput<'_>,
        cyber_policy_session_key: Option<&ProviderSessionAffinityKey>,
        upstream_model: Option<&str>,
    ) -> Result<CodexCredentialLease, CredentialSelectionError> {
        let queue_policy = request.attempt.account_selection_policy().queue_policy();
        // 预留名额只对普通请求生效；Guardian 可用满全部名额，并在账号队列中排在普通请求之前
        let reserve = request
            .attempt
            .account_selection_policy()
            .openai_guardian_reserved_concurrency();
        let prioritized = request.guardian && reserve > 0;
        let reserved_concurrency = if prioritized { 0 } else { reserve };
        let mut waiting = CapacityWait::new(
            &self.waiting,
            queue_policy,
            request.attempt.deadline().at(),
            request.attempt.concurrency_wait_budget(),
        )
        .with_priority(if prioritized {
            WaitPriority::High
        } else {
            WaitPriority::Normal
        });
        let binding_key = request
            .session_affinity_key
            .filter(|_| !request.attempt.is_diagnostic_required_account());
        if binding_key.is_some() {
            waiting = waiting.with_periodic_recheck();
        }
        let continuation_account = match request.attempt.continuation_attempt() {
            ContinuationAttempt::Native => request
                .attempt
                .continuation()
                .and_then(gateway_core::engine::continuation::ContinuationBinding::pinned)
                .map(|continuation| continuation.account().clone()),
            ContinuationAttempt::ReplayOwner => request
                .attempt
                .account_state_owner()
                .filter(|owner| owner.provider() == &self.provider_kind)
                .map(|owner| owner.account().clone()),
            ContinuationAttempt::None | ContinuationAttempt::ReplayAny => None,
        };
        // 已绑定会话由当前 owner 裁决；旧链的账号只在 Provider 续接校验中使用
        let continuation_account = continuation_account.filter(|_| binding_key.is_none());
        let required_account = request.attempt.required_account().cloned();
        if required_account
            .as_ref()
            .zip(continuation_account.as_ref())
            .is_some_and(|(required, continuation)| required != continuation)
        {
            return Err(CredentialSelectionError::NoEligibleCredential);
        }
        let pinned_account = required_account.or(continuation_account);
        let mut snapshot_retries = 0;
        'capacity: loop {
            if request.attempt.cancellation().is_cancelled()
                || request.attempt.deadline().is_elapsed()
            {
                return Err(CredentialSelectionError::CapacityUnavailable { retry_after: None });
            }
            let diagnostic = request.attempt.is_diagnostic_required_account();
            let observed_binding = match binding_key {
                Some(key) => self.lookup_session_affinity(key).await?,
                None => None,
            };
            let binding_state = match observed_binding.as_ref() {
                Some(binding) => self.bound_account_state(binding.account_id()).await?,
                None => AffinityAccountState::Retain,
            };
            let resolution = affinity::resolve(match observed_binding.as_ref() {
                Some(binding) => AffinitySnapshot::Bound {
                    binding,
                    state: binding_state,
                },
                None => AffinitySnapshot::Absent,
            });
            let owner = match resolution {
                AffinityResolution::Retain(binding) => Some(binding.account_id()),
                AffinityResolution::Initialize | AffinityResolution::Switch { .. } => None,
                AffinityResolution::Reject(_) => {
                    return Err(CredentialSelectionError::NoEligibleCredential);
                }
            };
            if owner
                .zip(pinned_account.as_ref())
                .is_some_and(|(owner, pinned)| owner != pinned)
            {
                return Err(CredentialSelectionError::NoEligibleCredential);
            }
            let fixed_account = owner.or(pinned_account.as_ref());
            let mut accounts = self.repository.list_for_provider().await?;
            // store 侧常规调度列表不包含停用账号；管理端诊断要对固定账号执行真实上游
            // 验证，先补回 required 账号，再统一检查认证类型和传输能力
            if diagnostic
                && let Some(required) = request.attempt.required_account()
                && !accounts.iter().any(|account| account.id() == required)
                && let Some(account) = self
                    .repository
                    .store()
                    .get_account(required)
                    .await
                    .map_err(|_| CredentialSelectionError::Store)?
            {
                accounts.push(account);
            }
            let mut model_access_rejected = 0_usize;
            let accounts = accounts
                .into_iter()
                .filter(|account| {
                    account.provider() == &self.provider_kind
                        && (diagnostic
                            || request
                                .attempt
                                .account_scope()
                                .is_some_and(|scope| scope.allows(account.id())))
                        && (!request.requires_oauth
                            || account.authentication_kind() == CODEX_AUTHENTICATION_KIND_OAUTH)
                        && (diagnostic
                            || upstream_model.is_none_or(|upstream_model| {
                                let allowed =
                                    request.attempt.account_scope().is_some_and(|scope| {
                                        scope.allows_model(account.id(), upstream_model)
                                    });
                                if !allowed {
                                    model_access_rejected += 1;
                                }
                                allowed
                            }))
                })
                .collect::<Vec<_>>();
            let mut eligible = Vec::with_capacity(accounts.len());
            for account in accounts {
                if request.requires_websocket && fixed_account.is_none_or(|id| id == account.id()) {
                    let runtime = match self.repository.load_runtime_credential(&account).await {
                        Ok(runtime) => runtime,
                        Err(CredentialRepositoryError::RevisionConflict) => {
                            retry_account_snapshot(
                                request.attempt,
                                &account,
                                &mut snapshot_retries,
                            )?;
                            continue 'capacity;
                        }
                        // 非固定账号的损坏凭据不能阻断其余账号的传输资格检查
                        Err(CredentialRepositoryError::InvalidCredentialData)
                            if fixed_account.is_none() =>
                        {
                            continue;
                        }
                        Err(error) => return Err(error.into()),
                    };
                    if runtime.transport == super::ResponsesTransport::Http {
                        continue;
                    }
                }
                eligible.push(account);
            }
            let accounts = eligible;
            if owner.is_some_and(|owner| !accounts.iter().any(|account| account.id() == owner)) {
                // 请求范围、模型和传输限制不能伪装成账号失效来迁移绑定
                return Err(CredentialSelectionError::NoEligibleCredential);
            }
            if model_access_rejected > 0 && request.attempt.trace().is_enabled() {
                request.attempt.trace().record(
                    "account.model_access",
                    serde_json::json!({"rejectedCount": model_access_rejected}),
                );
            }
            if !diagnostic {
                self.quota.prepare_scheduling(&accounts).await;
            }
            let mut rate_limits = HashMap::with_capacity(accounts.len());
            if !diagnostic {
                for account in &accounts {
                    let until = self.quota.cooldown(account.id()).await.unwrap_or(None);
                    rate_limits.insert(account.id().clone(), until);
                }
            }
            let account_ids = accounts
                .iter()
                .map(|account| account.id().clone())
                .collect::<Vec<_>>();
            let scheduling = self
                .leases
                .load_state(
                    request.attempt.client_api_key_ref(),
                    &self.provider_kind,
                    &account_ids,
                )
                .await?;
            let round_robin_cursor = scheduling.round_robin_cursor();
            let candidates = accounts
                .into_iter()
                .map(|account| {
                    let health = self
                        .account_feedback
                        .scheduling_signals(&self.provider_kind, account.id());
                    let signals = scheduling
                        .signals()
                        .get(account.id())
                        .cloned()
                        .unwrap_or(AccountRuntimeSignals {
                            in_flight: 0,
                            last_started_at: None,
                            quota_reset_at: None,
                            quota_remaining_rank: None,
                            cooldown: None,
                            failure_rate_basis_points: None,
                            first_output_latency_ms: None,
                        })
                        .with_provider_quota(self.quota.scheduling_signals(&account))
                        .with_rate_limit(rate_limits.get(account.id()).copied().flatten())
                        .with_runtime_health(health.0, health.1);
                    AccountCandidate { account, signals }
                })
                .collect::<Vec<_>>();
            let cyber_policy_scope = if binding_key.is_none() {
                self.prepare_cyber_policy_scope(cyber_policy_session_key)
                    .await
            } else {
                None
            };
            let mut excluded = request.attempt.excluded_accounts().clone();
            if let Some(state) = cyber_policy_scope
                .as_ref()
                .and_then(|scope| scope.state.as_ref())
            {
                excluded.extend(state.excluded_accounts().iter().cloned());
            }
            if let Some(owner) = owner {
                // 单次尝试的排除列表不能改变已确认 owner；其当前健康状态仍参与资格检查
                excluded.remove(owner);
            }
            if let Some(required) = fixed_account {
                for candidate in &candidates {
                    if candidate.account.id() != required {
                        excluded.insert(candidate.account.id().clone());
                    }
                }
            }
            let mut shortest_retry = None;
            let base_excluded = excluded.clone();
            let policy = request.attempt.account_selection_policy();

            loop {
                let preferred = fixed_account.cloned();
                let mut context = AccountSelectionContext {
                    policy,
                    now: SystemTime::now(),
                    excluded_accounts: excluded.clone(),
                    preferred_account: preferred.clone(),
                    preferred_account_overrides_weight: true,
                    round_robin_cursor,
                    eligibility: if diagnostic {
                        AccountEligibilityPolicy::BypassForDiagnostic
                    } else {
                        AccountEligibilityPolicy::Enforce
                    },
                    account_scope: request.attempt.account_scope().cloned(),
                    reserved_concurrency,
                };
                let wait_context = AccountSelectionContext {
                    excluded_accounts: base_excluded.clone(),
                    ..context.clone()
                };
                let wait_candidates = AccountSelector.wait_candidates(&candidates, &wait_context);
                let capacity = AccountSelector.capacity_snapshot(&candidates, &context);
                for candidate in &candidates {
                    if !waiting.can_try(candidate.account.id()) {
                        context
                            .excluded_accounts
                            .insert(candidate.account.id().clone());
                    }
                }
                let selection_result = if owner.is_some() {
                    // 绑定已确定账号，不再把固定 owner 交给评分或插件选号
                    Ok(AccountSelector.select(&candidates, &context))
                } else {
                    request
                        .attempt
                        .select_account(&self.provider_kind, upstream_model, &candidates, &context)
                        .await
                };
                let selection = match selection_result {
                    Ok(selection) => selection,
                    Err(AccountPolicyError::StaleCandidate) => continue 'capacity,
                    Err(AccountPolicyError::Rejected) => {
                        return Err(CredentialSelectionError::PolicyRejected);
                    }
                    Err(AccountPolicyError::Fault) => {
                        return Err(CredentialSelectionError::PolicyUnavailable);
                    }
                };
                request.attempt.trace().account_selection(
                    &candidates,
                    &context,
                    selection.as_ref(),
                );
                let Some(selection) = selection else {
                    if !diagnostic && queue_policy.max_waiting > 0 && !wait_candidates.is_empty() {
                        AccountSelector
                            .wait_for_capacity(
                                &mut waiting,
                                &wait_candidates,
                                &candidates,
                                &wait_context,
                            )
                            .await
                            .map_err(|error| {
                                tracing::info!(
                                    request_id = request.attempt.request_id().as_str(),
                                    queue_layer = "account",
                                    queue_wait_ms = waiting.elapsed().as_millis() as u64,
                                    reason = %error,
                                    "OpenAI 账号排队请求被拒绝"
                                );
                                CredentialSelectionError::QueueRejected(error)
                            })?;
                        continue 'capacity;
                    }
                    // 只判断本次账号范围；空池、认证失效和租约失败不能伪装成额度耗尽
                    // 使用最初的排除集合，避免本轮临时跳过的繁忙账号丢失其可恢复语义
                    let mut statuses = candidates
                        .iter()
                        .filter(|candidate| !base_excluded.contains(candidate.account.id()))
                        .map(|candidate| {
                            candidate
                                .account
                                .status_projection(context.now, candidate.signals.cooldown)
                                .status
                        });
                    let quota_exhausted = !diagnostic
                        && statuses.next() == Some(AccountStatus::QuotaExhausted)
                        && statuses.all(|status| status == AccountStatus::QuotaExhausted);
                    return if !wait_candidates.is_empty() || shortest_retry.is_some() {
                        Err(CredentialSelectionError::CapacityUnavailable {
                            retry_after: shortest_retry,
                        })
                    } else if quota_exhausted {
                        Err(CredentialSelectionError::QuotaExhausted)
                    } else {
                        Err(CredentialSelectionError::NoEligibleCredential)
                    };
                };
                let selected = selection.candidate();
                let account = candidates
                    .iter()
                    .find(|candidate| candidate.account.id() == selected.account.id())
                    .map(|candidate| candidate.account.clone())
                    .ok_or(CredentialSelectionError::InvalidCredential)?;
                // 额度观测等并发更新会使整个账号快照失效，必须重新选号并校验资格
                // 在占用租约和请求间隔前完成校验，避免重读被自己的异步释放挡住
                let runtime = match self.repository.load_runtime_credential(&account).await {
                    Ok(runtime) => runtime,
                    Err(CredentialRepositoryError::RevisionConflict) => {
                        retry_account_snapshot(request.attempt, &account, &mut snapshot_retries)?;
                        continue 'capacity;
                    }
                    Err(error) => return Err(error.into()),
                };
                // 凭据重读和插件选号都可能挂起，取得租约前再次让位于新队首
                if !waiting.can_try(account.id()) {
                    excluded.insert(account.id().clone());
                    continue;
                }
                let allows_account_state_mutation = !diagnostic || account.enabled();
                match self
                    .leases
                    .try_acquire(ProviderLeaseRequest::Scheduling(
                        ProviderSchedulingLeaseRequest::new(
                            self.provider_kind.clone(),
                            account.id().clone(),
                            account.revision(),
                            context.concurrency_limit(&account),
                            policy.request_interval(),
                            request.attempt.deadline(),
                        )
                        .with_cancellation(request.attempt.cancellation().clone()),
                    ))
                    .await?
                {
                    ProviderLeaseAcquisition::Busy { retry_after } => {
                        shortest_retry = minimum_duration(shortest_retry, retry_after);
                        excluded.insert(account.id().clone());
                    }
                    ProviderLeaseAcquisition::Acquired(guard) => {
                        if let Some(key) = binding_key
                            && !self
                                .admit_session_affinity(key, resolution, account.id())
                                .await?
                        {
                            drop(guard);
                            continue 'capacity;
                        }
                        let affinity_telemetry = AffinityTelemetry {
                            affinity_hit: observed_binding
                                .as_ref()
                                .is_some_and(|binding| binding.account_id() == account.id()),
                            escape_reason: match resolution {
                                AffinityResolution::Switch { reason, .. } => Some(reason),
                                _ => None,
                            },
                            account_switch: observed_binding
                                .as_ref()
                                .is_some_and(|binding| binding.account_id() != account.id()),
                        };
                        let affinity_observation = request.session_affinity_observation;
                        tracing::info!(
                            request_id = %request.attempt.request_id(),
                            attempt_index = request.attempt.attempt_index().get(),
                            rotation_strategy = policy.strategy().as_str(),
                            account_id = %account.id(),
                            affinity_hit = affinity_telemetry.affinity_hit,
                            escape_reason = affinity_telemetry
                                .escape_reason
                                .map_or("", affinity_switch_reason_name),
                            account_switch = affinity_telemetry.account_switch,
                            affinity_key_hash = affinity_observation
                                .map_or("", CodexSessionAffinity::key_hash),
                            affinity_anchor_source = affinity_observation
                                .map_or("", CodexSessionAffinity::anchor_source),
                            affinity_anchor = affinity_observation
                                .map_or("", CodexSessionAffinity::anchor),
                            session_id = affinity_observation
                                .map(CodexSessionAffinity::session_id)
                                .unwrap_or(""),
                            session_id_present = affinity_observation.is_some(),
                            "OpenAI account selected"
                        );
                        let cookies = runtime
                            .cookies
                            .into_iter()
                            .filter(|cookie| {
                                cookie
                                    .expires_at
                                    .is_none_or(|expires| expires > chrono::Utc::now())
                                    && self.cookie_policy.may_replay(
                                        request.request_url,
                                        &cookie.domain,
                                        &cookie.path,
                                        cookie.host_only,
                                        cookie.secure,
                                    )
                            })
                            .collect();
                        if !waiting.elapsed().is_zero() {
                            request.attempt.trace().record(
                                "account.queue.acquired",
                                serde_json::json!({"waitMs": waiting.elapsed().as_millis() as u64}),
                            );
                        }
                        return Ok(CodexCredentialLease {
                            installation_id: runtime.installation_id,
                            transport: runtime.transport,
                            account,
                            authentication: runtime.authentication,
                            cookies,
                            cyber_policy_scope,
                            allows_account_state_mutation,
                            affinity_telemetry,
                            capacity: capacity.map(AccountCapacitySnapshot::with_acquired_request),
                            _guard: guard,
                        });
                    }
                }
            }
        }
    }

    async fn lookup_session_affinity(
        &self,
        key: &ProviderSessionAffinityKey,
    ) -> Result<Option<AccountBinding>, CredentialSelectionError> {
        tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            self.session_affinity.load(&self.provider_kind, key),
        )
        .await
        .map_err(|_| CredentialSelectionError::Store)?
        .map_err(|_| CredentialSelectionError::Store)
    }

    async fn admit_session_affinity(
        &self,
        key: &ProviderSessionAffinityKey,
        resolution: AffinityResolution<'_>,
        selected: &ProviderAccountId,
    ) -> Result<bool, CredentialSelectionError> {
        let result = tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            resolution.admit(
                self.session_affinity.as_ref(),
                &self.provider_kind,
                key,
                selected,
            ),
        )
        .await
        .map_err(|_| CredentialSelectionError::Store)?
        .map_err(|_| CredentialSelectionError::Store)?;
        Ok(matches!(result, AffinityUpdate::Applied(_)))
    }

    async fn bound_account_state(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<AffinityAccountState, CredentialSelectionError> {
        // 单独查询权威目录，不能把请求过滤后的候选缺失视为账号删除
        let account = self
            .repository
            .store()
            .get_account(account_id)
            .await
            .map_err(|_| CredentialSelectionError::Store)?;
        Ok(match account {
            None => AffinityAccountState::Switch(AffinitySwitchReason::AccountDeleted),
            Some(account) if account.provider() != &self.provider_kind => {
                AffinityAccountState::RequestIncompatible
            }
            Some(account) => confirmed_binding_state(&account, SystemTime::now()),
        })
    }

    async fn prepare_cyber_policy_scope(
        &self,
        key: Option<&ProviderSessionAffinityKey>,
    ) -> Option<CodexCyberPolicyScope> {
        let key = key?.clone();
        let state = match tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            self.session_exclusions.load(&self.provider_kind, &key),
        )
        .await
        {
            Ok(Ok(state)) => state,
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "OpenAI cyber policy state read failed open");
                None
            }
            Err(_) => {
                tracing::warn!(
                    timeout_ms = SESSION_AFFINITY_TIMEOUT.as_millis(),
                    "OpenAI cyber policy state read timed out"
                );
                None
            }
        };
        Some(CodexCyberPolicyScope { key, state })
    }

    pub(crate) async fn record_cyber_policy_failure(
        &self,
        scope: Option<&CodexCyberPolicyScope>,
        account: &ProviderAccount,
    ) {
        let Some(scope) = scope else {
            return;
        };
        match tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            self.session_exclusions.record_failure(
                &self.provider_kind,
                &scope.key,
                account.id(),
                CYBER_POLICY_SESSION_TTL,
            ),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(
                    account_id = %account.id(),
                    error = %error,
                    "OpenAI cyber policy exclusion write failed open"
                );
            }
            Err(_) => {
                tracing::warn!(
                    account_id = %account.id(),
                    timeout_ms = SESSION_AFFINITY_TIMEOUT.as_millis(),
                    "OpenAI cyber policy exclusion write timed out"
                );
            }
        }
    }

    pub(crate) async fn observe_cyber_policy_success(&self, scope: Option<&CodexCyberPolicyScope>) {
        let Some(scope) = scope.filter(|scope| scope.state.is_some()) else {
            return;
        };
        let Some(state) = scope.state.as_ref() else {
            return;
        };
        match tokio::time::timeout(
            SESSION_AFFINITY_TIMEOUT,
            self.session_exclusions
                .clear(&self.provider_kind, &scope.key, state.revision()),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "OpenAI cyber policy exclusion clear failed open");
            }
            Err(_) => {
                tracing::warn!(
                    timeout_ms = SESSION_AFFINITY_TIMEOUT.as_millis(),
                    "OpenAI cyber policy exclusion clear timed out"
                );
            }
        }
    }

    pub async fn record_failure(
        &self,
        account: &ProviderAccount,
        failure: CodexAccountFailure,
        message: Option<String>,
    ) -> Result<(), CredentialSelectionError> {
        let now = SystemTime::now();
        let message = message.filter(|value| !value.trim().is_empty());
        match failure {
            CodexAccountFailure::CredentialExpired => {
                if account.authentication_kind() == CODEX_AUTHENTICATION_KIND_OAUTH
                    && account.has_refresh_token()
                    && account
                        .access_token_expires_at()
                        .is_some_and(|expires_at| expires_at <= now)
                    && refresh_recovery_deadline(account.access_token_expires_at())
                        .is_some_and(|deadline| deadline > now)
                {
                    tracing::info!(
                        account_id = %account.id(),
                        access_token_expires_at = ?account.access_token_expires_at()
                            .map(chrono::DateTime::<chrono::Utc>::from),
                        recovery_deadline = ?refresh_recovery_deadline(account.access_token_expires_at())
                            .map(chrono::DateTime::<chrono::Utc>::from),
                        "OpenAI access token expired; retaining account for bounded OAuth refresh recovery"
                    );
                    return self
                        .apply_credential_state(
                            account,
                            CredentialState::Ready,
                            AccountErrorReason::AccessTokenExpired,
                            now,
                            message,
                        )
                        .await;
                }
                self.apply_credential_state(
                    account,
                    CredentialState::Expired,
                    AccountErrorReason::AccessTokenExpired,
                    now,
                    message,
                )
                .await
            }
            CodexAccountFailure::IdentityVerificationRequired => {
                self.apply_credential_state(
                    account,
                    CredentialState::Invalid,
                    AccountErrorReason::AccountUnverified,
                    now,
                    message,
                )
                .await
            }
            CodexAccountFailure::Banned => {
                self.apply_credential_state(
                    account,
                    CredentialState::Banned,
                    AccountErrorReason::AccountBanned,
                    now,
                    message,
                )
                .await
            }
            CodexAccountFailure::QuotaExhausted => self
                .quota
                .record_confirmed_exhaustion(account, QuotaEvidence::PaymentRequired, None, now)
                .await
                .map_err(|_| CredentialSelectionError::Store),
            CodexAccountFailure::UsageLimitExhausted { reset_at } => self
                .quota
                .record_confirmed_exhaustion(
                    account,
                    QuotaEvidence::UsageLimitReached,
                    reset_at,
                    now,
                )
                .await
                .map_err(|_| CredentialSelectionError::Store),
            // 429：临时限流只写运行时冷却，不改变凭据或额度事实
            CodexAccountFailure::RateLimited { retry_after } => {
                self.quota
                    .apply_rate_limit_429(account, retry_after, now)
                    .await
                    .map_err(|_| CredentialSelectionError::Store)?;
                Ok(())
            }
            // Cloudflare 挑战：内存退避表（记录风险计数），不写账号事实
            CodexAccountFailure::CloudflareChallenge { retry_after } => {
                let delay = self.cloudflare_challenge_delay(account.id(), now, retry_after);
                let recovery = now
                    .checked_add(delay)
                    .map_or(CookieRecovery::Clear, CookieRecovery::ExpireAt);
                self.apply_cookie_recovery(account, recovery).await?;
                Ok(())
            }
            // Cloudflare 路径被封：连续超阈值才置 Invalid；否则内存退避
            CodexAccountFailure::CloudflarePathBlocked => {
                let blocked = self.record_cloudflare_path_block(account.id(), now);
                if blocked >= CLOUDFLARE_PATH_BLOCK_THRESHOLD {
                    self.apply_credential_state(
                        account,
                        CredentialState::Invalid,
                        AccountErrorReason::CredentialInvalid,
                        now,
                        message,
                    )
                    .await?;
                }
                self.apply_cookie_recovery(account, CookieRecovery::Clear)
                    .await?;
                Ok(())
            }
        }
    }

    async fn apply_credential_state(
        &self,
        account: &ProviderAccount,
        credential_state: CredentialState,
        error_reason: AccountErrorReason,
        observed_at: SystemTime,
        message: Option<String>,
    ) -> Result<(), CredentialSelectionError> {
        self.repository
            .apply_state_with_reason(
                account,
                credential_state,
                observed_at,
                Some(error_reason),
                message,
            )
            .await?;
        Ok(())
    }

    pub async fn record_success(&self, account: &ProviderAccount) {
        self.restore_recoverable_account_state(account).await;
        self.risk_recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(account.id().as_str());
    }

    async fn restore_recoverable_account_state(&self, account: &ProviderAccount) {
        let Ok(current) = self.current_account(account.id()).await else {
            return;
        };
        if current.provider() != &self.provider_kind
            || current.revision() != account.revision()
            || !current.enabled()
        {
            return;
        }
        let observed_at = SystemTime::now();
        if current.credential_state() != CredentialState::Ready
            && let Err(error) = self
                .repository
                .apply_state(&current, CredentialState::Ready, observed_at)
                .await
        {
            tracing::warn!(
                account_id = %account.id(),
                error = %error,
                "OpenAI credential recovery after successful upstream response failed"
            );
        }
        if let Err(error) = self
            .quota
            .record_successful_inference(&current, observed_at)
            .await
        {
            tracing::warn!(
                account_id = %account.id(),
                error = %error,
                "OpenAI quota recovery after successful upstream response failed"
            );
        }
    }

    pub async fn current_account(
        &self,
        account_id: &ProviderAccountId,
    ) -> Result<ProviderAccount, CredentialSelectionError> {
        self.repository
            .store()
            .get_account(account_id)
            .await
            .map_err(|_| CredentialSelectionError::Store)?
            .ok_or(CredentialSelectionError::InvalidCredential)
    }

    pub async fn capture_response_cookies(
        &self,
        account: &ProviderAccount,
        response_origin: &Url,
        headers: &[String],
    ) -> Result<CodexCookieCaptureOutcome, CredentialSelectionError> {
        if account.authentication_kind() != CODEX_AUTHENTICATION_KIND_OAUTH {
            return Ok(CodexCookieCaptureOutcome {
                credential_revision: None,
                rejected: headers.len(),
            });
        }
        let parsed = self.cookie_policy.parse_response_headers(
            account.id().as_str(),
            account.revision().get(),
            response_origin,
            headers,
            chrono::Utc::now(),
        );
        if parsed.inputs.is_empty() {
            return Ok(CodexCookieCaptureOutcome {
                credential_revision: None,
                rejected: parsed.rejected,
            });
        }
        let mut data = self.repository.load_complete_data(account).await?;
        let Some(cookies) = data.cookies_mut() else {
            return Ok(CodexCookieCaptureOutcome {
                credential_revision: None,
                rejected: headers.len(),
            });
        };
        for input in parsed.inputs {
            let scope = self.cookie_policy.validate_capture(
                &input.response_origin,
                input.domain_attribute.as_deref(),
                &input.name,
                &input.path,
            )?;
            cookies.retain(|cookie| {
                !(cookie.name == input.name
                    && cookie.domain == scope.domain
                    && cookie.path == input.path)
            });
            if !input.delete {
                cookies.push(CodexCookie {
                    name: input.name,
                    value: input.value.expose_secret().to_owned(),
                    domain: scope.domain,
                    path: input.path,
                    host_only: scope.host_only,
                    secure: input.secure,
                    expires_at: input.expires_at,
                });
            }
        }
        let revision = self.repository.compare_and_swap_data(account, data).await?;
        Ok(CodexCookieCaptureOutcome {
            credential_revision: Some(revision.get()),
            rejected: parsed.rejected,
        })
    }

    fn cloudflare_challenge_delay(
        &self,
        account_id: &ProviderAccountId,
        now: SystemTime,
        retry_after: Option<Duration>,
    ) -> Duration {
        let mut recovery = self
            .risk_recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = active_risk_recovery(&mut recovery, account_id.as_str(), now);
        state.challenge_count = state.challenge_count.saturating_add(1);
        state.observed_at = now;
        let index = usize::try_from(state.challenge_count.saturating_sub(1))
            .unwrap_or(usize::MAX)
            .min(CLOUDFLARE_CHALLENGE_BACKOFF.len() - 1);
        retry_after
            .unwrap_or_default()
            .max(CLOUDFLARE_CHALLENGE_BACKOFF[index])
    }

    fn record_cloudflare_path_block(&self, account_id: &ProviderAccountId, now: SystemTime) -> u32 {
        let mut recovery = self
            .risk_recovery
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = active_risk_recovery(&mut recovery, account_id.as_str(), now);
        state.path_block_count = state.path_block_count.saturating_add(1);
        state.observed_at = now;
        state.path_block_count
    }

    async fn apply_cookie_recovery(
        &self,
        account: &ProviderAccount,
        recovery: CookieRecovery,
    ) -> Result<(), CredentialSelectionError> {
        let mut data = self.repository.load_complete_data(account).await?;
        if data.cookies().is_empty() {
            return Ok(());
        }
        let Some(cookies) = data.cookies_mut() else {
            return Ok(());
        };
        match recovery {
            CookieRecovery::ExpireAt(expires_at) => {
                let expires_at = chrono::DateTime::<chrono::Utc>::from(expires_at);
                for cookie in cookies {
                    cookie.expires_at = Some(
                        cookie
                            .expires_at
                            .map_or(expires_at, |current| current.min(expires_at)),
                    );
                }
            }
            CookieRecovery::Clear => cookies.clear(),
        }
        self.repository.compare_and_swap_data(account, data).await?;
        Ok(())
    }
}

fn confirmed_binding_state(account: &ProviderAccount, now: SystemTime) -> AffinityAccountState {
    use AffinitySwitchReason as Reason;
    let reason = if !account.enabled() {
        Some(Reason::AccountDisabled)
    } else if account.quota().is_exhausted() {
        Some(Reason::QuotaExhausted)
    } else if !account.has_refresh_token()
        && account
            .access_token_expires_at()
            .is_some_and(|expires_at| expires_at <= now)
    {
        Some(Reason::CredentialUnrecoverable)
    } else {
        match (account.credential_state(), account.last_error_reason()) {
            (CredentialState::Banned, _) => Some(Reason::AccountBlocked),
            (CredentialState::Invalid, Some(AccountErrorReason::AccountUnverified)) => {
                Some(Reason::AccountVerificationRequired)
            }
            (CredentialState::Expired, Some(AccountErrorReason::CredentialExpired)) => {
                Some(Reason::CredentialUnrecoverable)
            }
            (CredentialState::Expired, Some(AccountErrorReason::AccessTokenExpired))
                if !account.has_refresh_token()
                    || refresh_recovery_deadline(account.access_token_expires_at())
                        .is_some_and(|deadline| deadline <= now) =>
            {
                Some(Reason::CredentialUnrecoverable)
            }
            _ => None,
        }
    };
    reason.map_or(AffinityAccountState::Retain, AffinityAccountState::Switch)
}

const fn affinity_switch_reason_name(reason: AffinitySwitchReason) -> &'static str {
    match reason {
        AffinitySwitchReason::QuotaExhausted => "quota_exhausted",
        AffinitySwitchReason::CredentialUnrecoverable => "credential_unrecoverable",
        AffinitySwitchReason::AccountBlocked => "account_blocked",
        AffinitySwitchReason::AccountVerificationRequired => "account_verification_required",
        AffinitySwitchReason::AccountDisabled => "account_disabled",
        AffinitySwitchReason::AccountDeleted => "account_deleted",
    }
}

fn active_risk_recovery<'a>(
    recovery: &'a mut HashMap<String, RiskRecoveryState>,
    account_id: &str,
    now: SystemTime,
) -> &'a mut RiskRecoveryState {
    recovery.retain(|_, state| match now.duration_since(state.observed_at) {
        Ok(elapsed) => elapsed <= CLOUDFLARE_RECOVERY_STALE_AFTER,
        Err(_) => true,
    });
    recovery
        .entry(account_id.to_owned())
        .or_insert(RiskRecoveryState {
            challenge_count: 0,
            path_block_count: 0,
            observed_at: now,
        })
}

impl fmt::Debug for CodexCredentialSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCredentialSelector")
            .field("repository", &"ProviderAccountStore")
            .field("leases", &"ProviderLeasePort")
            .field("quota", &"CodexCredentialQuotaService")
            .field("cookie_policy", &self.cookie_policy)
            .finish()
    }
}

pub struct CodexCredentialLease {
    transport: super::ResponsesTransport,
    account: ProviderAccount,
    authentication: CodexRuntimeAuthentication,
    cookies: Vec<RuntimeCodexCookie>,
    installation_id: String,
    cyber_policy_scope: Option<CodexCyberPolicyScope>,
    allows_account_state_mutation: bool,
    affinity_telemetry: AffinityTelemetry,
    capacity: Option<AccountCapacitySnapshot>,
    _guard: Box<dyn ProviderLeaseGuard>,
}

impl CodexCredentialLease {
    pub(crate) const fn transport(&self) -> super::ResponsesTransport {
        self.transport
    }

    #[must_use]
    pub const fn account(&self) -> &ProviderAccount {
        &self.account
    }

    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        self.account.id()
    }

    #[must_use]
    pub const fn authentication(&self) -> &CodexRuntimeAuthentication {
        &self.authentication
    }

    #[must_use]
    pub fn cookies(&self) -> &[RuntimeCodexCookie] {
        &self.cookies
    }

    #[must_use]
    pub fn installation_id(&self) -> &str {
        &self.installation_id
    }

    #[must_use]
    pub(crate) const fn cyber_policy_scope(&self) -> Option<&CodexCyberPolicyScope> {
        self.cyber_policy_scope.as_ref()
    }

    /// 禁用账号的管理端诊断必须只返回真实上游结果，不能回写账号侧状态
    #[must_use]
    pub(crate) const fn allows_account_state_mutation(&self) -> bool {
        self.allows_account_state_mutation
    }

    #[must_use]
    pub const fn affinity_hit(&self) -> bool {
        self.affinity_telemetry.affinity_hit
    }

    #[must_use]
    pub const fn escape_reason(&self) -> Option<&'static str> {
        match self.affinity_telemetry.escape_reason {
            Some(reason) => Some(affinity_switch_reason_name(reason)),
            None => None,
        }
    }

    #[must_use]
    pub const fn account_switch(&self) -> bool {
        self.affinity_telemetry.account_switch
    }

    #[must_use]
    pub const fn capacity_snapshot(&self) -> Option<AccountCapacitySnapshot> {
        self.capacity
    }
}

impl fmt::Debug for CodexCredentialLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodexCredentialLease")
            .field("account", &self.account)
            .field("authentication", &"<redacted>")
            .field("cookies", &self.cookies)
            .field("installation_id", &"<pseudonymous>")
            .finish()
    }
}

fn retry_account_snapshot(
    attempt: &AttemptContext,
    account: &ProviderAccount,
    retries: &mut u32,
) -> Result<(), CredentialSelectionError> {
    let retry = *retries < MAX_ACCOUNT_SNAPSHOT_RETRIES;
    if retry {
        *retries += 1;
    }
    attempt.trace().record(
        "account.snapshot_conflict",
        serde_json::json!({
            "accountId": account.id().as_str(),
            "credentialRevision": account.revision().get(),
            "retry": retry,
            "retryCount": *retries,
            "maxRetries": MAX_ACCOUNT_SNAPSHOT_RETRIES,
        }),
    );
    if retry {
        Ok(())
    } else {
        Err(CredentialSelectionError::AccountSnapshotChanged)
    }
}

#[derive(Debug, Error)]
pub enum CredentialSelectionError {
    #[error(transparent)]
    QueueRejected(#[from] QueueRejection),
    #[error("no eligible Codex account")]
    NoEligibleCredential,
    #[error("all eligible Codex accounts have exhausted their quota")]
    QuotaExhausted,
    #[error("Codex account capacity is unavailable")]
    CapacityUnavailable { retry_after: Option<Duration> },
    #[error("Codex account data is invalid")]
    InvalidCredential,
    #[error("Codex account changed repeatedly during selection")]
    AccountSnapshotChanged,
    #[error("Codex account store is unavailable")]
    Store,
    #[error("Codex account lease runtime is unavailable")]
    Coordinator,
    #[error("Codex Cookie policy rejected the value")]
    CookiePolicy,
    #[error("account scheduling policy rejected the request")]
    PolicyRejected,
    #[error("account scheduling policy is unavailable")]
    PolicyUnavailable,
}

impl From<CredentialRepositoryError> for CredentialSelectionError {
    fn from(error: CredentialRepositoryError) -> Self {
        match error {
            CredentialRepositoryError::InvalidCredentialData => Self::InvalidCredential,
            CredentialRepositoryError::RevisionConflict | CredentialRepositoryError::Store => {
                Self::Store
            }
        }
    }
}

impl From<ProviderStoreError> for CredentialSelectionError {
    fn from(_: ProviderStoreError) -> Self {
        Self::Coordinator
    }
}

impl From<super::cookie::CookiePolicyError> for CredentialSelectionError {
    fn from(_: super::cookie::CookiePolicyError) -> Self {
        Self::CookiePolicy
    }
}

fn minimum_duration(current: Option<Duration>, candidate: Option<Duration>) -> Option<Duration> {
    match (current, candidate) {
        (Some(current), Some(candidate)) => Some(current.min(candidate)),
        (Some(current), None) => Some(current),
        (None, candidate) => candidate,
    }
}
