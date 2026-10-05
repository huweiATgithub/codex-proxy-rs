//! OpenAI 逻辑会话到 Store 不透明账号绑定键及诊断上下文的单向派生

use gateway_core::engine::middleware::MiddlewareHeader;
use gateway_core::operation::{ProviderHttpRequest, RawJsonPayload};
use gateway_core::policy::ClientApiKeyId;
use gateway_core::provider_ports::ProviderSessionAffinityKey;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::transport::protocol::responses::CodexResponsesRequest;

const AFFINITY_KEY_HASH_LENGTH: usize = 12;

/// 一次请求派生出的账号亲和键及其结构化日志上下文
pub(crate) struct CodexSessionAffinity {
    key: ProviderSessionAffinityKey,
    key_hash: String,
    session_id: String,
}

impl CodexSessionAffinity {
    #[must_use]
    pub(crate) const fn key(&self) -> &ProviderSessionAffinityKey {
        &self.key
    }

    #[must_use]
    pub(crate) fn key_hash(&self) -> &str {
        &self.key_hash
    }

    /// 返回可持久化的客户端作用域不透明会话关联值
    #[must_use]
    pub(crate) fn persistence_hash(&self) -> &str {
        self.key.expose_to_store()
    }

    #[must_use]
    pub(crate) const fn anchor_source(&self) -> &'static str {
        "root-session"
    }

    #[must_use]
    pub(crate) fn anchor(&self) -> &str {
        &self.session_id
    }

    #[must_use]
    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// 将原始 response ID 投影为客户端作用域的不可逆关联值
#[must_use]
pub(crate) fn derive_previous_response_id_hash(
    previous_response_id: &str,
    client_api_key_id: &ClientApiKeyId,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"codex-previous-response-observation-v1\0");
    hasher.update(client_api_key_id.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(previous_response_id.as_bytes());
    hex::encode(hasher.finalize())
}

pub(crate) fn derive_codex_session_affinity(
    request: &CodexResponsesRequest,
    client_api_key_id: &ClientApiKeyId,
) -> Option<CodexSessionAffinity> {
    let session_id = non_empty(request.client_logical_session_id.as_deref())?.to_owned();
    session_affinity(session_id, client_api_key_id)
}

/// 原始 JSON 端点只读取会话身份，发送时仍保留原始字节
/// Search 的 `id` 是官方
/// 根 session_id，必须与 Responses 共用命名空间，不能另建一份账号亲和
pub(crate) fn derive_codex_endpoint_session_affinity(
    payload: &RawJsonPayload,
    client_api_key_id: &ClientApiKeyId,
    body_session_field: &str,
) -> Option<CodexSessionAffinity> {
    let body = serde_json::from_slice::<Map<String, Value>>(payload.body()).unwrap_or_default();
    let session_id = gateway_protocol::openai::codex_account_session_id(
        &body,
        payload.context(),
        body_session_field,
    )?;
    session_affinity(session_id, client_api_key_id)
}

/// Live 正文可能是 SDP 或 multipart，只从发送时有效的显式会话头取得绑定
/// 中间件覆盖同名协议头，多值头采用首值；优先 session-id，再回退 x-session-id
pub(crate) fn derive_codex_live_session_affinity(
    request: &ProviderHttpRequest,
    middleware_headers: &[MiddlewareHeader],
    client_api_key_id: &ClientApiKeyId,
) -> Option<CodexSessionAffinity> {
    let session_id = ["session-id", "x-session-id"]
        .into_iter()
        .find_map(|name| {
            let value = middleware_headers
                .iter()
                .find(|header| header.name().eq_ignore_ascii_case(name))
                .map(MiddlewareHeader::value)
                .or_else(|| {
                    request
                        .headers()
                        .iter()
                        .find(|header| header.name().eq_ignore_ascii_case(name))
                        .map(gateway_core::operation::ProviderHttpHeader::value)
                })?;
            non_empty(std::str::from_utf8(value).ok())
        })?;
    session_affinity(session_id.to_owned(), client_api_key_id)
}

fn session_affinity(
    session_id: String,
    client_api_key_id: &ClientApiKeyId,
) -> Option<CodexSessionAffinity> {
    let session_key = opaque_affinity_key("root-session", &session_id)?;
    let key = opaque_affinity_key(
        "client-session",
        &format!(
            "{}\0{}",
            client_api_key_id.as_str(),
            session_key.expose_to_store()
        ),
    )?;
    let key_hash = short_key_hash(&key);
    Some(CodexSessionAffinity {
        key,
        key_hash,
        session_id,
    })
}

fn short_key_hash(key: &ProviderSessionAffinityKey) -> String {
    // 亲和键本身已经是 SHA-256；日志沿用 WebSocket 诊断的 12 位短哈希长度
    key.expose_to_store()
        .chars()
        .take(AFFINITY_KEY_HASH_LENGTH)
        .collect()
}

fn opaque_affinity_key(domain: &str, value: &str) -> Option<ProviderSessionAffinityKey> {
    let mut hasher = Sha256::new();
    hasher.update(b"codex-session-affinity-v1\0");
    hasher.update(domain.as_bytes());
    hasher.update(b"\0");
    hasher.update(value.as_bytes());
    ProviderSessionAffinityKey::try_new(hex::encode(hasher.finalize())).ok()
}

/// 恢复旧版 `cyber_policy` 的会话隔离键
///
/// 它只接受显式 session/conversation 或客户端明确给出的 prompt cache key，避免将
/// 请求内容哈希误当成长会话；`previous_response_id` 续写不参与该策略
pub(crate) fn derive_codex_cyber_policy_session_key(
    request: &CodexResponsesRequest,
    client_api_key_id: &ClientApiKeyId,
) -> Option<ProviderSessionAffinityKey> {
    if request.previous_response_id().is_some() {
        return None;
    }
    let session_id = non_empty(request.client_session_id.as_deref())
        .or_else(|| non_empty(request.client_conversation_id.as_deref()))
        .or_else(|| {
            request
                .explicit_prompt_cache_key
                .then(|| request.prompt_cache_key())
                .flatten()
                .and_then(|value| non_empty(Some(value)))
        })?;
    let mut hasher = Sha256::new();
    hasher.update(b"cyber-policy-session\0");
    hasher.update(client_api_key_id.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(session_id.as_bytes());
    ProviderSessionAffinityKey::try_new(hex::encode(hasher.finalize())).ok()
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}
