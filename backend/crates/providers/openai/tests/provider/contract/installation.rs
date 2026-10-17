//! 安装身份在 HTTP、WebSocket 与独立端点上的元数据一致性及配置切换合同

use super::*;
use crate::support::MemoryClientInstallations;
use gateway_core::provider_ports::ProviderClientInstallationKey;
use std::sync::atomic::Ordering;

const ACCOUNT_ID: &str = "acct_provider_contract";
const CLIENT_KEY_ID: &str = "key_installation_contract";
const TURN_METADATA: &str = r#"{"installation_id":"downstream-installation","future":true}"#;

fn installation_provider(
    store: &Arc<MemoryAccountStore>,
    base_url: String,
    installations: Arc<MemoryClientInstallations>,
) -> Arc<CodexProvider> {
    provider_and_quota_with_runtime_ports_and_installations(
        store,
        Arc::new(MemorySessionAffinity::default()),
        base_url,
        Arc::new(TestLeaseCoordinator::default()),
        0,
        Arc::new(MemoryCooldownPort::new()),
        crate::support::runtime_policy(),
        installations,
    )
    .0
}

fn independent_configuration(enabled: bool, user_agent: &str) -> OpaqueProviderData {
    OpaqueProviderData::new(
        json!({
            "mode": "custom",
            "userAgent": user_agent,
            "perKeyInstallationId": enabled,
        })
        .as_object()
        .unwrap()
        .clone(),
    )
}

#[derive(Debug)]
struct RewriteUserAgent;

impl MiddlewarePlan for RewriteUserAgent {
    fn handle(
        &self,
        context: MiddlewareContext,
        request: MiddlewareRequest,
        next: MiddlewareNext,
    ) -> BoxFuture<'static, Result<MiddlewareResponse, MiddlewareError>> {
        assert_eq!(context.mount(), MiddlewareMount::Attempt);
        let (protocol, mut headers, body) = request.into_parts();
        headers.retain(|header| !header.name().eq_ignore_ascii_case("user-agent"));
        headers.push(MiddlewareHeader::new(
            "user-agent",
            Bytes::from_static(b"plugin-user-agent/1.0"),
        ));
        next.run(MiddlewareRequest::new(protocol, headers, body))
    }
}

fn installation_context(
    provider: &CodexProvider,
    request_id: &str,
    configuration: Option<&OpaqueProviderData>,
    rewrite_user_agent: bool,
) -> AttemptContext {
    let profile = provider
        .resolve_request_profile(configuration, configuration)
        .expect("resolve key request profile");
    let mut request = RequestAttemptContext::new(
        ModelRequestId::new(request_id).unwrap(),
        ClientApiKeyId::new(CLIENT_KEY_ID).unwrap(),
    )
    .with_request_profile(profile);
    if rewrite_user_agent {
        request = request.with_middleware(
            Some(FrozenMiddlewarePlan::new(
                Arc::new(RewriteUserAgent),
                ExtensionSetReference::new(
                    ExtensionSetId::new("installation-ua-rewrite".to_owned()).unwrap(),
                    Arc::new(TestExtensionLease),
                ),
            )),
            Arc::from([]),
            "/v1/responses".to_owned(),
            ClientTransport::HttpSse,
        );
    }
    AttemptContext::new(
        request,
        NonZeroU32::MIN,
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None)
            .with_account_scope(contract_account_scope()),
        None,
        CancellationToken::new(),
    )
}

fn installation_generate(websocket: bool) -> Operation {
    let payload = ProtocolPayload::json_object(
        "openai",
        json!({
            "model": "gpt-5.4",
            "input": "hello",
            "session_id": "installation-session",
            "thread_id": "installation-session",
            "installation_id": "downstream-installation",
            "client_metadata": {
                "x-codex-installation-id": "downstream-installation",
                "x-codex-turn-metadata": TURN_METADATA,
            },
        })
        .as_object()
        .unwrap()
        .clone(),
    )
    .unwrap()
    .with_context(Map::from_iter([
        ("use_websocket".to_owned(), json!(websocket)),
        ("turn_metadata".to_owned(), json!(TURN_METADATA)),
    ]));
    let mut request = GenerateRequest::from_protocol_payload(payload);
    if websocket {
        request = request.with_provider_session_state(
            ProviderSessionState::new(
                "openai",
                json!({
                    "account_id": ACCOUNT_ID,
                    "conversation_id": "installation-conversation",
                    "continuation_scope": "persisted",
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .unwrap(),
        );
    }
    Operation::Generate(request)
}

fn mapping_key() -> ProviderClientInstallationKey {
    ProviderClientInstallationKey::new(
        ClientApiKeyId::new(CLIENT_KEY_ID).unwrap(),
        ProviderAccountId::new(ACCOUNT_ID).unwrap(),
    )
}

fn assert_body_identity(body: &Value, expected: &str) {
    assert_eq!(body["installation_id"], expected);
    assert_eq!(body["client_metadata"]["x-codex-installation-id"], expected);
    assert_eq!(
        serde_json::from_str::<Value>(
            body["client_metadata"]["x-codex-turn-metadata"]
                .as_str()
                .unwrap(),
        )
        .unwrap(),
        json!({"installation_id": expected, "future": true}),
    );
}

#[tokio::test]
async fn http_identity_follows_key_configuration_across_ua_rewrites_and_toggle_changes() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT_ID).await;
    let account_identity = store
        .repository()
        .load_complete_data(&store.account(ACCOUNT_ID).unwrap())
        .await
        .unwrap()
        .installation_id()
        .to_owned();
    let installations = Arc::new(MemoryClientInstallations::default());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(CAPTURE_COMPLETED_SSE, "text/event-stream"),
        )
        .expect(7)
        .mount(&server)
        .await;
    let provider = installation_provider(&store, server.uri(), Arc::clone(&installations));
    let global_user_agent = wire_profile().snapshot().user_agent();
    // 独立配置即使与全局 UA 完全相同，也必须使用 Key 的显式启用设置
    let disabled = independent_configuration(false, &global_user_agent);
    let enabled = independent_configuration(true, &global_user_agent);
    let edited = independent_configuration(true, "codex-tui/0.155.0");
    let mut pair_identity = None;
    for (index, (configuration, per_key)) in [
        (None, false),
        (Some(&disabled), false),
        (Some(&enabled), true),
        (Some(&edited), true),
        (Some(&disabled), false),
        (None, false),
        (Some(&enabled), true),
    ]
    .into_iter()
    .enumerate()
    {
        let mut stream = provider
            .clone()
            .execute(
                planned_request("openai", installation_generate(false)),
                installation_context(
                    &provider,
                    &format!("req_installation_http_{index}"),
                    configuration,
                    true,
                ),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.expect("HTTP response");
        }
        if per_key && pair_identity.is_none() {
            let identity = installations.mappings.lock().unwrap()[&mapping_key()];
            assert_eq!(identity.get_version_num(), 4);
            assert_ne!(identity.to_string(), account_identity);
            pair_identity = Some(identity.to_string());
        }
        let expected = if per_key {
            pair_identity.as_deref().unwrap()
        } else {
            &account_identity
        };
        let requests = server.received_requests().await.unwrap();
        let request = requests.last().unwrap();
        assert_eq!(request.headers["user-agent"], "plugin-user-agent/1.0");
        assert_body_identity(&captured_request_body(request), expected);
        let metadata = captured_header_values(request, "x-codex-turn-metadata");
        assert_eq!(metadata.len(), 1);
        assert_eq!(
            serde_json::from_slice::<Value>(&metadata[0]).unwrap(),
            json!({"installation_id": expected, "future": true}),
        );
        let initialized = usize::from(index >= 2);
        assert_eq!(installations.reads.load(Ordering::SeqCst), initialized);
        assert_eq!(installations.inserts.load(Ordering::SeqCst), initialized);
    }
}

#[tokio::test]
async fn reused_websocket_refreshes_body_identity_and_keeps_opening_handshake_identity() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT_ID).await;
    let account_identity = store
        .repository()
        .load_complete_data(&store.account(ACCOUNT_ID).unwrap())
        .await
        .unwrap()
        .installation_id()
        .to_owned();
    let installations = Arc::new(MemoryClientInstallations::default());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut opening = None;
        let mut websocket =
            crate::transport::accept_codex_test_websocket_with(socket, |request, _| {
                opening = Some(request.headers().clone())
            })
            .await;
        let mut bodies = Vec::new();
        for index in 0..3 {
            let frame = websocket.next().await.unwrap().unwrap();
            bodies.push(serde_json::from_str::<Value>(frame.to_text().unwrap()).unwrap());
            websocket
                .send(Message::Text(
                    json!({
                        "type": "response.completed",
                        "response": {
                            "id": format!("resp_installation_{index}"),
                            "model": "gpt-5.4", "status": "completed", "output": [],
                        },
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        }
        (opening.unwrap(), bodies)
    });
    let provider = installation_provider(&store, base_url, Arc::clone(&installations));
    let mut observed_pool = Vec::new();
    for (index, enabled) in [true, false, true].into_iter().enumerate() {
        let configuration =
            independent_configuration(enabled, &wire_profile().snapshot().user_agent());
        timeout(Duration::from_secs(5), async {
            let mut stream = provider
                .clone()
                .execute(
                    planned_request("openai", installation_generate(true)),
                    installation_context(
                        &provider,
                        &format!("req_installation_ws_{index}"),
                        Some(&configuration),
                        false,
                    ),
                )
                .await
                .unwrap();
            let mut pool = None;
            while let Some(event) = stream.next().await {
                if let Some(observation) = event.unwrap().response_observation() {
                    pool = observation.websocket_pool().or(pool);
                }
            }
            observed_pool.push(pool);
        })
        .await
        .expect("bounded WebSocket execution on the existing connection");
    }
    let (opening, bodies) = timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    let identity = installations.mappings.lock().unwrap()[&mapping_key()].to_string();
    assert_body_identity(&bodies[0], &identity);
    assert_body_identity(&bodies[1], &account_identity);
    assert_body_identity(&bodies[2], &identity);
    assert_eq!(
        serde_json::from_str::<Value>(opening["x-codex-turn-metadata"].to_str().unwrap()).unwrap(),
        json!({"installation_id": identity, "future": true}),
    );
    assert_eq!(
        observed_pool,
        vec![
            Some(WebSocketPoolKind::New),
            Some(WebSocketPoolKind::Reuse),
            Some(WebSocketPoolKind::Reuse),
        ],
    );
    assert_eq!(installations.reads.load(Ordering::SeqCst), 1);
    assert_eq!(installations.inserts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn standalone_images_preserve_bodies_and_search_scopes_the_shared_pair_identity() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT_ID).await;
    let installations = Arc::new(MemoryClientInstallations::default());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"output": "ok"})))
        .expect(3)
        .mount(&server)
        .await;
    let provider = installation_provider(&store, server.uri(), Arc::clone(&installations));
    let configuration = independent_configuration(true, &wire_profile().snapshot().user_agent());
    let raw = Bytes::from_static(br#"{ "prompt":"hello", "query":"hello", "future":1 }"#);
    for index in 0..3 {
        let mut payload = RawJsonPayload::new("openai", raw.clone()).unwrap();
        if index == 2 {
            payload = payload.with_context(Map::from_iter([(
                "turn_metadata".to_owned(),
                json!(TURN_METADATA),
            )]));
        }
        let operation = match index {
            0 => Operation::GenerateImage(ImageRequest::from_raw_json(
                ImageRequestKind::Generation,
                payload,
            )),
            1 => Operation::GenerateImage(ImageRequest::from_raw_json(
                ImageRequestKind::Edit,
                payload,
            )),
            _ => Operation::Search(StandaloneSearchRequest::from_raw_json(payload)),
        };
        let mut stream = provider
            .clone()
            .execute(
                planned_provider_endpoint_request("openai", operation),
                installation_context(
                    &provider,
                    &format!("req_installation_endpoint_{index}"),
                    Some(&configuration),
                    false,
                ),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.expect("standalone response");
        }
        assert_eq!(installations.mappings.lock().unwrap().len(), 1);
        assert_eq!(installations.reads.load(Ordering::SeqCst), 1);
        assert_eq!(installations.inserts.load(Ordering::SeqCst), 1);
    }
    let identity = installations.mappings.lock().unwrap()[&mapping_key()].to_string();
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);
    for (request, endpoint) in requests.iter().zip([
        "/codex/images/generations",
        "/codex/images/edits",
        "/codex/alpha/search",
    ]) {
        assert_eq!(request.url.path(), endpoint);
        assert_eq!(request.body, raw);
        let metadata = captured_header_values(request, "x-codex-turn-metadata");
        if endpoint == "/codex/alpha/search" {
            assert_eq!(metadata.len(), 1);
            assert_eq!(
                serde_json::from_slice::<Value>(&metadata[0]).unwrap(),
                json!({"installation_id": identity, "future": true}),
            );
        } else {
            assert!(metadata.is_empty());
        }
    }
}
