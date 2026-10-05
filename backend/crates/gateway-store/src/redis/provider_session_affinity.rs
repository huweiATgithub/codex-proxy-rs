//! 当前会话账号绑定的 Redis 原子存储

use std::time::Duration;

use gateway_core::account::ProviderAccountId;
use gateway_core::account::affinity::{
    ACCOUNT_BINDING_TTL, AccountBinding, AffinityStore, AffinityStoreError, AffinityStoreErrorKind,
    AffinityUpdate, BindingToken, SessionAffinityKey,
};
use gateway_core::routing::ProviderKind;
use redis::aio::ConnectionManager;
use serde::{Deserialize, Serialize};

use crate::StoreResult;

use super::{namespace, resource_fingerprint};

// Rust 先解析完整记录，Lua 只比较已解析观察的原文，避免覆盖有损或未知字段
const COMPARE_AND_SET_SCRIPT: &str = r#"
local current = redis.pcall('GET', KEYS[1])
if type(current) == 'table' and current.err then
  return {2, false}
end
local matches = (ARGV[1] == 'absent' and not current)
  or (ARGV[1] == 'bound' and current == ARGV[2])
if not matches then
  return {0, current}
end
redis.call('PSETEX', KEYS[1], tonumber(ARGV[4]), ARGV[3])
return {1, ARGV[3]}
"#;

#[derive(Clone)]
pub struct RedisProviderSessionAffinityRepository {
    connection: ConnectionManager,
    namespace: String,
}

impl RedisProviderSessionAffinityRepository {
    pub fn new(connection: ConnectionManager, key_namespace: &str) -> StoreResult<Self> {
        Ok(Self {
            connection,
            namespace: namespace(key_namespace)?,
        })
    }

    fn key(
        &self,
        provider_kind: &ProviderKind,
        affinity_key: &SessionAffinityKey,
    ) -> Result<String, AffinityStoreError> {
        let scope = format!(
            "{}\0{}",
            provider_kind.as_str(),
            affinity_key.expose_to_store()
        );
        let fingerprint = resource_fingerprint("provider session affinity", &scope)
            .map_err(|_| invalid("encode key"))?;
        Ok(format!(
            "{}:scheduler:account-binding:{{{fingerprint}}}",
            self.namespace
        ))
    }

    async fn read(&self, key: &str) -> Result<Option<StoredBinding>, AffinityStoreError> {
        let mut connection = self.connection.clone();
        redis::cmd("GET")
            .arg(key)
            .query_async::<Option<String>>(&mut connection)
            .await
            .map_err(|error| {
                if error.code() == Some("WRONGTYPE")
                    || error.kind() == redis::ErrorKind::UnexpectedReturnType
                {
                    invalid("load")
                } else {
                    unavailable("load")
                }
            })?
            .map(StoredBinding::parse)
            .transpose()
    }
}

impl AffinityStore for RedisProviderSessionAffinityRepository {
    fn load<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a SessionAffinityKey,
    ) -> futures::future::BoxFuture<'a, Result<Option<AccountBinding>, AffinityStoreError>> {
        Box::pin(async move {
            self.read(&self.key(provider_kind, key)?)
                .await
                .map(|stored| stored.map(|stored| stored.binding))
        })
    }

    fn compare_and_set<'a>(
        &'a self,
        provider_kind: &'a ProviderKind,
        key: &'a SessionAffinityKey,
        expected: Option<&'a AccountBinding>,
        selected_account: &'a ProviderAccountId,
        ttl: Duration,
    ) -> futures::future::BoxFuture<'a, Result<AffinityUpdate, AffinityStoreError>> {
        Box::pin(async move {
            let ttl_millis = binding_ttl_millis(ttl)?;
            let key = self.key(provider_kind, key)?;
            let current = self.read(&key).await?;
            if current.as_ref().map(|stored| &stored.binding) != expected {
                return Ok(AffinityUpdate::Conflict(
                    current.map(|stored| stored.binding),
                ));
            }

            let token = match expected {
                Some(binding) if binding.account_id() == selected_account => {
                    binding.token().clone()
                }
                _ => BindingToken::try_new(uuid::Uuid::new_v4().simple().to_string())
                    .map_err(|_| invalid("generate token"))?,
            };
            let selected = AccountBinding::new(selected_account.clone(), token);
            let wire = WireBinding {
                account_id: selected.account_id().as_str().to_owned(),
                token: selected.token().expose_to_store().to_owned(),
            };
            let encoded = serde_json::to_string(&wire).map_err(|_| invalid("encode binding"))?;
            let mut connection = self.connection.clone();
            let (status, value) = redis::Script::new(COMPARE_AND_SET_SCRIPT)
                .key(key)
                .arg(if current.is_some() { "bound" } else { "absent" })
                .arg(current.as_ref().map_or("", |stored| stored.raw.as_str()))
                .arg(encoded)
                .arg(ttl_millis)
                .invoke_async::<(u8, Option<String>)>(&mut connection)
                .await
                .map_err(|_| unavailable("compare and set"))?;
            match status {
                1 => Ok(AffinityUpdate::Applied(selected)),
                0 => value
                    .map(StoredBinding::parse)
                    .transpose()
                    .map(|stored| AffinityUpdate::Conflict(stored.map(|stored| stored.binding))),
                _ => Err(invalid("compare and set")),
            }
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireBinding {
    account_id: String,
    token: String,
}

struct StoredBinding {
    raw: String,
    binding: AccountBinding,
}

impl StoredBinding {
    fn parse(raw: String) -> Result<Self, AffinityStoreError> {
        let wire: WireBinding =
            serde_json::from_str(&raw).map_err(|_| invalid("decode binding"))?;
        let account_id =
            ProviderAccountId::new(wire.account_id).map_err(|_| invalid("decode binding"))?;
        let token = BindingToken::try_new(wire.token).map_err(|_| invalid("decode binding"))?;
        Ok(Self {
            raw,
            binding: AccountBinding::new(account_id, token),
        })
    }
}

fn binding_ttl_millis(ttl: Duration) -> Result<u64, AffinityStoreError> {
    if ttl.as_millis() == 0 || ttl > ACCOUNT_BINDING_TTL {
        return Err(invalid("validate TTL"));
    }
    u64::try_from(ttl.as_millis()).map_err(|_| invalid("validate TTL"))
}

fn unavailable(operation: &'static str) -> AffinityStoreError {
    AffinityStoreError::new(AffinityStoreErrorKind::Unavailable, operation)
}

fn invalid(operation: &'static str) -> AffinityStoreError {
    AffinityStoreError::new(AffinityStoreErrorKind::InvalidData, operation)
}
