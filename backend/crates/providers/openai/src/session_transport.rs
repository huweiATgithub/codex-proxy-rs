//! 各线程独立的连续 WS 失败预算；耗尽后保持 HTTP，空闲过期后释放状态

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use gateway_core::policy::ClientApiKeyId;
use sha2::{Digest, Sha256};
use tokio::time::Instant;

use crate::transport::protocol::responses::CodexResponsesRequest;
use crate::transport::request::derive_conversation_anchor;

const SESSION_IDLE_RETENTION: Duration = Duration::from_secs(8 * 60 * 60);
const MAX_SESSION_TRANSPORTS: usize = 16_384;

/// 传输预算按客户端与线程隔离，不能使用根会话的账号绑定键
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct CodexSessionTransportKey(String);

impl CodexSessionTransportKey {
    pub(crate) fn key_hash(&self) -> &str {
        &self.0[..12]
    }
}

pub(crate) fn derive_codex_session_transport_key(
    request: &CodexResponsesRequest,
    client_api_key_id: &ClientApiKeyId,
) -> Option<CodexSessionTransportKey> {
    let (source, anchor) = request
        .client_thread_id
        .as_deref()
        .map(str::trim)
        .filter(|thread_id| !thread_id.is_empty())
        .map(|thread_id| ("thread", thread_id.to_owned()))
        .or_else(|| derive_conversation_anchor(request))?;
    let mut hasher = Sha256::new();
    hasher.update(b"codex-thread-transport-v1\0");
    hasher.update(client_api_key_id.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(source.as_bytes());
    hasher.update(b"\0");
    hasher.update(anchor.as_bytes());
    Some(CodexSessionTransportKey(hex::encode(hasher.finalize())))
}

#[derive(Clone, Default)]
pub(crate) struct CodexSessionTransportRecovery {
    sessions: Arc<Mutex<HashMap<CodexSessionTransportKey, SessionTransport>>>,
}

struct SessionTransport {
    state: TransportState,
    last_used: Instant,
}

enum TransportState {
    WebSocket { failures: u32 },
    Http,
}

impl CodexSessionTransportRecovery {
    pub(crate) fn uses_http(&self, key: &CodexSessionTransportKey) -> bool {
        let now = Instant::now();
        let mut sessions = self.lock();
        let Some(session) = sessions.get_mut(key) else {
            return false;
        };
        if now.saturating_duration_since(session.last_used) >= SESSION_IDLE_RETENTION {
            sessions.remove(key);
            return false;
        }
        session.last_used = now;
        matches!(session.state, TransportState::Http)
    }

    /// 返回连续失败是否已耗尽首发加重试预算
    pub(crate) fn record_websocket_failure(
        &self,
        key: &CodexSessionTransportKey,
        max_retries: u32,
    ) -> bool {
        self.update(key, |state| match state {
            TransportState::WebSocket { failures } if failures < max_retries => {
                TransportState::WebSocket {
                    failures: failures + 1,
                }
            }
            TransportState::WebSocket { .. } | TransportState::Http => TransportState::Http,
        })
    }

    pub(crate) fn disable_websocket(&self, key: &CodexSessionTransportKey) {
        self.update(key, |_| TransportState::Http);
    }

    pub(crate) fn websocket_succeeded(&self, key: &CodexSessionTransportKey) {
        self.lock().remove(key);
    }

    fn update(
        &self,
        key: &CodexSessionTransportKey,
        update: impl FnOnce(TransportState) -> TransportState,
    ) -> bool {
        let now = Instant::now();
        let mut sessions = self.lock();
        let previous = sessions
            .remove(key)
            .filter(|session| {
                now.saturating_duration_since(session.last_used) < SESSION_IDLE_RETENTION
            })
            .map_or(TransportState::WebSocket { failures: 0 }, |session| {
                session.state
            });
        if sessions.len() >= MAX_SESSION_TRANSPORTS {
            sessions.retain(|_, session| {
                now.saturating_duration_since(session.last_used) < SESSION_IDLE_RETENTION
            });
            if sessions.len() >= MAX_SESSION_TRANSPORTS
                && let Some(oldest) = sessions
                    .iter()
                    .min_by_key(|(_, session)| session.last_used)
                    .map(|(key, _)| key.clone())
            {
                sessions.remove(&oldest);
            }
        }
        let state = update(previous);
        let uses_http = matches!(state, TransportState::Http);
        sessions.insert(
            key.clone(),
            SessionTransport {
                state,
                last_used: now,
            },
        );
        uses_http
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<CodexSessionTransportKey, SessionTransport>> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
