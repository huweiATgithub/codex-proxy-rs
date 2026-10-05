//! 验证 OpenAI 选号容量、亲和排队与硬性不可用后的切换

use futures::{FutureExt, future::BoxFuture};
use gateway_core::account::AccountRuntimeSignals;
use gateway_core::concurrency::{ConcurrencyQueuePolicy, QueueRejection};
use gateway_core::engine::policy::{
    AccountScheduleDecision, AccountScheduleInput, ModelRouteDecision, ModelRouteInput,
    RequestPolicyContext, RequestPolicyFault, RequestPolicyPlan,
};
use gateway_core::runtime::extensions::{ExtensionSetId, ExtensionSetLease, ExtensionSetReference};

use super::*;

fn busy_signal(in_flight: u32, last_started_at: Option<SystemTime>) -> AccountRuntimeSignals {
    AccountRuntimeSignals {
        in_flight,
        last_started_at,
        quota_reset_at: None,
        quota_remaining_rank: None,
        cooldown: None,
        failure_rate_basis_points: None,
        first_output_latency_ms: None,
    }
}

async fn bind_first(affinity: &MemorySessionAffinity) -> ProviderSessionAffinityKey {
    let key = ProviderSessionAffinityKey::try_new("capacity-session").unwrap();
    affinity
        .bind(
            &ProviderKind::new("openai").unwrap(),
            &key,
            &ProviderAccountId::new("acct_first").unwrap(),
            Duration::from_secs(60),
        )
        .await
        .unwrap();
    key
}

fn capacity_attempt(
    policy: AccountSelectionPolicy,
    request_policy: Option<RequestPolicyContext>,
) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new("req_capacity_contract").unwrap(),
            ClientApiKeyId::new("key_codex_contract").unwrap(),
        )
        .with_request_policy(request_policy),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(5),
        policy,
        AccountAttemptContext::new(BTreeSet::new(), None, None)
            .with_account_scope(contract_account_scope()),
        None,
        CancellationToken::new(),
    )
}

#[test]
fn saturated_snapshot_is_capacity_unavailable_even_with_exhausted_accounts() {
    for interval in [false, true] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_first", "test-first");
        create_account(&store, "acct_second", "test-second");
        let exhausted =
            block_on(store.get_account(&ProviderAccountId::new("acct_second").unwrap()))
                .unwrap()
                .unwrap();
        persist_quota_exhaustion(&store, &exhausted, None);
        let leases = Arc::new(TestLeaseCoordinator::default());
        leases.signals.lock().unwrap().insert(
            ProviderAccountId::new("acct_first").unwrap(),
            busy_signal(
                if interval { 0 } else { u32::MAX },
                interval.then(SystemTime::now),
            ),
        );
        let selector = selector(&store, leases.clone());
        let request_attempt = capacity_attempt(
            AccountSelectionPolicy::new(
                RotationStrategy::Smart,
                NonZeroU32::new(2).unwrap(),
                Duration::from_secs(2),
            ),
            None,
        );
        let url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();
        let error = block_on(selector.select(&SelectCodexCredential {
            upstream_model: "gpt-5.4",
            request_url: &url,
            attempt: &request_attempt,
            session_affinity_key: None,
        }))
        .unwrap_err();
        assert!(
            matches!(error, CredentialSelectionError::CapacityUnavailable { .. }),
            "{error:?}"
        );
        assert!(leases.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn affinity_queue_waits_for_the_original_account_for_local_capacity_blockers() {
    for blocker in ["lease", "concurrency", "interval"] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_first", "test-first");
        create_account(&store, "acct_second", "test-second");
        let leases = Arc::new(TestLeaseCoordinator::default());
        let first = ProviderAccountId::new("acct_first").unwrap();
        if blocker == "lease" {
            leases.busy_accounts.lock().unwrap().insert(first.clone());
        } else {
            leases.signals.lock().unwrap().insert(
                first.clone(),
                busy_signal(
                    if blocker == "concurrency" { 2 } else { 0 },
                    (blocker == "interval").then(SystemTime::now),
                ),
            );
        }
        let affinity = Arc::new(MemorySessionAffinity::default());
        let key = bind_first(&affinity).await;
        let selector = selector_with_affinity(&store, leases.clone(), affinity.clone());
        let request_attempt = capacity_attempt(
            AccountSelectionPolicy::new(
                RotationStrategy::Smart,
                NonZeroU32::new(2).unwrap(),
                Duration::from_secs(2),
            )
            .with_queue(ConcurrencyQueuePolicy {
                max_waiting: 1,
                timeout: Duration::from_secs(2),
            }),
            None,
        );
        let url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();
        let request = SelectCodexCredential {
            upstream_model: "gpt-5.4",
            request_url: &url,
            attempt: &request_attempt,
            session_affinity_key: Some(&key),
        };
        let mut pending = Box::pin(selector.select(&request));
        assert!(
            pending.as_mut().now_or_never().is_none(),
            "blocker={blocker}"
        );
        leases.signals.lock().unwrap().clear();
        leases.busy_accounts.lock().unwrap().clear();
        let selected = pending.await.unwrap();
        assert_eq!(selected.account_id(), &first);
        assert!(selected.affinity_hit());
        assert_eq!(selected.escape_reason(), None);
        assert!(!selected.account_switch());
        assert!(
            leases
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.account_id() == &first)
        );
    }
}

#[tokio::test]
async fn affinity_without_queue_rejects_saturated_owner_despite_free_account() {
    for interval in [false, true] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_first", "test-first");
        create_account(&store, "acct_second", "test-second");
        let first = ProviderAccountId::new("acct_first").unwrap();
        let leases = Arc::new(TestLeaseCoordinator::default());
        leases.signals.lock().unwrap().insert(
            first.clone(),
            busy_signal(if interval { 0 } else { 2 }, interval.then(SystemTime::now)),
        );
        let affinity = Arc::new(MemorySessionAffinity::default());
        let key = bind_first(&affinity).await;
        let selector = selector_with_affinity(&store, leases.clone(), affinity.clone());
        let request_attempt = capacity_attempt(
            AccountSelectionPolicy::new(
                RotationStrategy::Smart,
                NonZeroU32::new(2).unwrap(),
                Duration::from_secs(2),
            ),
            None,
        );
        let url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();
        let error = selector
            .select(&SelectCodexCredential {
                upstream_model: "gpt-5.4",
                request_url: &url,
                attempt: &request_attempt,
                session_affinity_key: Some(&key),
            })
            .await
            .unwrap_err();
        assert!(
            matches!(error, CredentialSelectionError::CapacityUnavailable { .. }),
            "interval={interval}: {error:?}"
        );
        assert!(leases.requests.lock().unwrap().is_empty());
        assert_eq!(
            affinity
                .load(&ProviderKind::new("openai").unwrap(), &key)
                .await
                .unwrap()
                .unwrap()
                .account_id(),
            &first
        );
    }
}

#[tokio::test]
async fn affinity_queue_full_and_timeout_preserve_binding_despite_free_fallback() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "test-first");
    create_account(&store, "acct_second", "test-second");
    let first = ProviderAccountId::new("acct_first").unwrap();
    let leases = Arc::new(TestLeaseCoordinator::default());
    leases.busy_accounts.lock().unwrap().insert(first.clone());
    let affinity = Arc::new(MemorySessionAffinity::default());
    let key = bind_first(&affinity).await;
    let selector = selector_with_affinity(&store, leases.clone(), affinity.clone());
    let head_attempt = queued_attempt(Duration::from_secs(2));
    let next_attempt = queued_attempt(Duration::from_secs(2));
    let url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();
    let request = SelectCodexCredential {
        upstream_model: "gpt-5.4",
        request_url: &url,
        attempt: &head_attempt,
        session_affinity_key: Some(&key),
    };
    let mut head = Box::pin(selector.select(&request));
    assert!(head.as_mut().now_or_never().is_none());
    // 槽位已空，但新请求不能越过旧等待者，也不能转而抢占另一账号
    leases.busy_accounts.lock().unwrap().clear();
    let rejected = selector
        .select(&SelectCodexCredential {
            attempt: &next_attempt,
            ..request
        })
        .await
        .unwrap_err();
    assert!(matches!(
        rejected,
        CredentialSelectionError::QueueRejected(QueueRejection::Full)
    ));
    drop(head);

    leases.busy_accounts.lock().unwrap().insert(first.clone());
    let timeout_attempt = queued_attempt(Duration::from_millis(20));
    let timeout = selector
        .select(&SelectCodexCredential {
            attempt: &timeout_attempt,
            ..request
        })
        .await
        .unwrap_err();
    assert!(matches!(
        timeout,
        CredentialSelectionError::QueueRejected(QueueRejection::Timeout)
    ));
    assert_eq!(
        affinity
            .load(&ProviderKind::new("openai").unwrap(), &key)
            .await
            .unwrap()
            .unwrap()
            .account_id(),
        &first
    );
    leases.busy_accounts.lock().unwrap().clear();
    let selected = selector
        .select(&SelectCodexCredential {
            attempt: &next_attempt,
            ..request
        })
        .await
        .unwrap();
    assert_eq!(selected.account_id(), &first);
    assert!(
        leases
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.account_id() == &first)
    );
}

#[tokio::test]
async fn affinity_queue_rechecks_hard_unavailability_before_switching_accounts() {
    for quota_exhausted in [false, true] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_first", "test-first");
        create_account(&store, "acct_second", "test-second");
        let first = ProviderAccountId::new("acct_first").unwrap();
        let leases = Arc::new(TestLeaseCoordinator::default());
        leases.busy_accounts.lock().unwrap().insert(first.clone());
        let affinity = Arc::new(MemorySessionAffinity::default());
        let key = bind_first(&affinity).await;
        let selector = selector_with_affinity(&store, leases, affinity.clone());
        let request_attempt = queued_attempt(Duration::from_secs(2));
        let url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();
        let request = SelectCodexCredential {
            upstream_model: "gpt-5.4",
            request_url: &url,
            attempt: &request_attempt,
            session_affinity_key: Some(&key),
        };
        let mut pending = Box::pin(selector.select(&request));
        assert!(pending.as_mut().now_or_never().is_none());
        let account = store.get_account(&first).await.unwrap().unwrap();
        if quota_exhausted {
            persist_quota_exhaustion(&store, &account, None);
        } else {
            persist_credential_state(&store, &account, CredentialState::Banned);
        }
        let selected = pending.await.unwrap();
        assert_eq!(selected.account_id().as_str(), "acct_second");
        assert!(selected.account_switch());
        assert_eq!(
            affinity
                .load(&ProviderKind::new("openai").unwrap(), &key)
                .await
                .unwrap()
                .unwrap()
                .account_id(),
            selected.account_id()
        );
    }
}

#[tokio::test]
async fn non_head_waiter_follows_current_binding_while_original_account_stays_busy() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "test-first");
    create_account(&store, "acct_second", "test-second");
    let provider = ProviderKind::new("openai").unwrap();
    let first = ProviderAccountId::new("acct_first").unwrap();
    let second = ProviderAccountId::new("acct_second").unwrap();
    let leases = Arc::new(TestLeaseCoordinator::default());
    leases.busy_accounts.lock().unwrap().insert(first.clone());
    let affinity = Arc::new(MemorySessionAffinity::default());
    let follower_key = bind_first(&affinity).await;
    let head_key = ProviderSessionAffinityKey::try_new("other-session").unwrap();
    affinity
        .bind(&provider, &head_key, &first, Duration::from_secs(60))
        .await
        .unwrap();
    let selector = selector_with_affinity(&store, leases.clone(), affinity.clone());
    let queue_policy = account_policy().with_queue(ConcurrencyQueuePolicy {
        max_waiting: 2,
        timeout: Duration::from_secs(2),
    });
    let head_attempt = capacity_attempt(queue_policy, None);
    let follower_attempt = capacity_attempt(queue_policy, None);
    let url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();
    let head_request = SelectCodexCredential {
        upstream_model: "gpt-5.4",
        request_url: &url,
        attempt: &head_attempt,
        session_affinity_key: Some(&head_key),
    };
    let follower_request = SelectCodexCredential {
        attempt: &follower_attempt,
        session_affinity_key: Some(&follower_key),
        ..head_request
    };
    let mut head = Box::pin(selector.select(&head_request));
    assert!(head.as_mut().now_or_never().is_none());
    let mut follower = Box::pin(selector.select(&follower_request));
    assert!(follower.as_mut().now_or_never().is_none());

    // 模拟同会话的另一请求已提交换号；旧队首所属会话与容量保持不变
    affinity
        .bind(&provider, &follower_key, &second, Duration::from_secs(60))
        .await
        .unwrap();
    let selected = tokio::time::timeout(Duration::from_millis(500), follower)
        .await
        .expect("non-head waiter must recheck the current binding")
        .unwrap();
    assert_eq!(selected.account_id(), &second);
    assert!(head.as_mut().now_or_never().is_none());
    assert!(leases.busy_accounts.lock().unwrap().contains(&first));
    assert_eq!(
        affinity
            .load(&provider, &head_key)
            .await
            .unwrap()
            .unwrap()
            .account_id(),
        &first
    );
}

#[tokio::test]
async fn queue_enabled_without_affinity_still_uses_a_free_account() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_first", "test-first");
    create_account(&store, "acct_second", "test-second");
    let leases = Arc::new(TestLeaseCoordinator::default());
    leases.signals.lock().unwrap().insert(
        ProviderAccountId::new("acct_first").unwrap(),
        busy_signal(2, None),
    );
    let selector = selector(&store, leases);
    let request_attempt = queued_attempt(Duration::from_secs(2));
    let url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();
    let selected = selector
        .select(&SelectCodexCredential {
            upstream_model: "gpt-5.4",
            request_url: &url,
            attempt: &request_attempt,
            session_affinity_key: None,
        })
        .await
        .unwrap();
    assert_eq!(selected.account_id().as_str(), "acct_second");
}

#[derive(Debug)]
struct Scheduler {
    explicit: bool,
}

impl RequestPolicyPlan for Scheduler {
    fn route_model(
        &self,
        _: ModelRouteInput,
    ) -> BoxFuture<'static, Result<ModelRouteDecision, RequestPolicyFault>> {
        Box::pin(async { Ok(ModelRouteDecision::Unhandled) })
    }

    fn schedule_account(
        &self,
        _: AccountScheduleInput,
    ) -> BoxFuture<'static, Result<AccountScheduleDecision, RequestPolicyFault>> {
        let decision = if self.explicit {
            AccountScheduleDecision::Pick(ProviderAccountId::new("acct_second").unwrap())
        } else {
            AccountScheduleDecision::Delegate
        };
        Box::pin(async move { Ok(decision) })
    }
}

struct ExtensionLease;

impl ExtensionSetLease for ExtensionLease {
    fn is_ready(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn affinity_wait_preserves_owner_despite_explicit_policy_choice() {
    for explicit in [false, true] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_first", "test-first");
        create_account(&store, "acct_second", "test-second");
        let leases = Arc::new(TestLeaseCoordinator::default());
        leases.signals.lock().unwrap().insert(
            ProviderAccountId::new("acct_first").unwrap(),
            busy_signal(2, None),
        );
        let affinity = Arc::new(MemorySessionAffinity::default());
        let key = bind_first(&affinity).await;
        let selector = selector_with_affinity(&store, leases.clone(), affinity);
        let request_attempt = capacity_attempt(
            account_policy().with_queue(ConcurrencyQueuePolicy {
                max_waiting: 1,
                timeout: Duration::from_secs(2),
            }),
            Some(RequestPolicyContext::new(
                Arc::new(Scheduler { explicit }),
                ExtensionSetReference::new(
                    ExtensionSetId::new("capacity-test".into()).unwrap(),
                    Arc::new(ExtensionLease),
                ),
                ModelRequestId::new("req_capacity_contract").unwrap(),
                ClientApiKeyId::new("key_codex_contract").unwrap(),
                vec![],
            )),
        );
        let url = Url::parse(OFFICIAL_CODEX_BASE_URL).unwrap();
        let request = SelectCodexCredential {
            upstream_model: "gpt-5.4",
            request_url: &url,
            attempt: &request_attempt,
            session_affinity_key: Some(&key),
        };
        let mut pending = Box::pin(selector.select(&request));
        assert!(pending.as_mut().now_or_never().is_none());
        leases.signals.lock().unwrap().clear();
        let selected = pending.await.unwrap();
        assert_eq!(selected.account_id().as_str(), "acct_first");
        assert!(
            leases
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| request.account_id().as_str() == "acct_first")
        );
    }
}
