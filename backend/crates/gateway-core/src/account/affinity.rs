//! 会话当前账号、封闭的切换依据与发送前条件写入合同

use std::fmt;
use std::time::Duration;

use futures::future::BoxFuture;

use super::ProviderAccountId;
use crate::identity::ProviderKind;

/// 只有成功的发送前裁决刷新此空闲有效期
pub const ACCOUNT_BINDING_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// 调用方已按客户端与会话身份隔离的不透明键
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionAffinityKey(String);

impl SessionAffinityKey {
    pub fn try_new(value: impl Into<String>) -> Result<Self, InvalidAffinityKey> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
            })
        {
            return Err(InvalidAffinityKey);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn expose_to_store(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SessionAffinityKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionAffinityKey([OPAQUE])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid account affinity key")]
pub struct InvalidAffinityKey;

/// 当前绑定的随机比较值；存储适配器在首绑或切换时生成，不承载历史
#[derive(Clone, PartialEq, Eq)]
pub struct BindingToken(String);

impl BindingToken {
    pub fn try_new(value: impl Into<String>) -> Result<Self, InvalidBindingToken> {
        let value = value.into();
        if value.len() != 32
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(InvalidBindingToken);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn expose_to_store(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BindingToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BindingToken([OPAQUE])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid account binding token")]
pub struct InvalidBindingToken;

/// 只表示读取时的当前绑定；发送资格还需要条件写入成功
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountBinding {
    account_id: ProviderAccountId,
    token: BindingToken,
}

impl AccountBinding {
    #[must_use]
    pub const fn new(account_id: ProviderAccountId, token: BindingToken) -> Self {
        Self { account_id, token }
    }

    #[must_use]
    pub const fn account_id(&self) -> &ProviderAccountId {
        &self.account_id
    }

    #[must_use]
    pub const fn token(&self) -> &BindingToken {
        &self.token
    }
}

/// Provider 已确认的账号级事实；请求特有失败和暂时容量不在此集合
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AffinitySwitchReason {
    QuotaExhausted,
    CredentialUnrecoverable,
    AccountBlocked,
    AccountVerificationRequired,
    AccountDisabled,
    AccountDeleted,
}

/// 调用方对当前 owner 的资格投影；暂忙仍保留 owner，容量等待归选号器
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AffinityAccountState {
    Retain,
    Switch(AffinitySwitchReason),
    RequestIncompatible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AffinitySnapshot<'a> {
    Absent,
    Bound {
        binding: &'a AccountBinding,
        state: AffinityAccountState,
    },
}

/// 决策只限定候选范围；任何发送仍必须以原观察完成原子比较
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AffinityResolution<'a> {
    Initialize,
    Switch {
        binding: &'a AccountBinding,
        reason: AffinitySwitchReason,
    },
    Retain(&'a AccountBinding),
    Reject(&'a AccountBinding),
}

impl AffinityResolution<'_> {
    /// 凭据与容量已就绪后才调用；决策限定账号及预期代次，统一在此续期
    pub async fn admit(
        self,
        store: &dyn AffinityStore,
        provider: &ProviderKind,
        key: &SessionAffinityKey,
        selected_account: &ProviderAccountId,
    ) -> Result<AffinityUpdate, AffinityStoreError> {
        let expected = match self {
            Self::Initialize => None,
            Self::Switch { binding, .. } => Some(binding),
            Self::Retain(binding) if binding.account_id() == selected_account => Some(binding),
            Self::Retain(_) | Self::Reject(_) => {
                return Err(AffinityStoreError::new(
                    AffinityStoreErrorKind::InvalidData,
                    "admit selected account",
                ));
            }
        };
        store
            .compare_and_set(
                provider,
                key,
                expected,
                selected_account,
                ACCOUNT_BINDING_TTL,
            )
            .await
    }
}

#[must_use]
pub const fn resolve(snapshot: AffinitySnapshot<'_>) -> AffinityResolution<'_> {
    match snapshot {
        AffinitySnapshot::Absent => AffinityResolution::Initialize,
        AffinitySnapshot::Bound { binding, state } => match state {
            AffinityAccountState::Retain => AffinityResolution::Retain(binding),
            AffinityAccountState::Switch(reason) => AffinityResolution::Switch { binding, reason },
            AffinityAccountState::RequestIncompatible => AffinityResolution::Reject(binding),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AffinityUpdate {
    Applied(AccountBinding),
    Conflict(Option<AccountBinding>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AffinityStoreErrorKind {
    Unavailable,
    InvalidData,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("account affinity {operation} failed: {kind:?}")]
pub struct AffinityStoreError {
    kind: AffinityStoreErrorKind,
    operation: &'static str,
}

impl AffinityStoreError {
    #[must_use]
    pub const fn new(kind: AffinityStoreErrorKind, operation: &'static str) -> Self {
        Self { kind, operation }
    }

    #[must_use]
    pub const fn kind(&self) -> AffinityStoreErrorKind {
        self.kind
    }
}

/// 只读观察与发送前裁决；响应完成不应持有此写入能力
pub trait AffinityStore: Send + Sync {
    fn load<'a>(
        &'a self,
        provider: &'a ProviderKind,
        key: &'a SessionAffinityKey,
    ) -> BoxFuture<'a, Result<Option<AccountBinding>, AffinityStoreError>>;

    /// 缺失只匹配 `None`；已绑定必须匹配完整 token 和账号
    ///
    /// 同账号只续期，首绑或换号生成新 token。冲突不写入或续期，返回当前观察
    fn compare_and_set<'a>(
        &'a self,
        provider: &'a ProviderKind,
        key: &'a SessionAffinityKey,
        expected: Option<&'a AccountBinding>,
        selected_account: &'a ProviderAccountId,
        ttl: Duration,
    ) -> BoxFuture<'a, Result<AffinityUpdate, AffinityStoreError>>;
}
