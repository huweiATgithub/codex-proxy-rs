//! 会话账号绑定的发送前生效、续接隔离与账号状态清理合同测试

use super::*;

#[tokio::test]
async fn binding_hit_should_not_authorize_unowned_opaque_turn_state() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_unknown_turn_state").await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CAPTURE_COMPLETED_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = provider_with_affinity_and_base_url(&store, Arc::clone(&affinity), server.uri());
    let initial = Arc::clone(&provider)
        .execute(
            planned_request(
                "openai",
                Operation::Generate(generate_with_session_context(
                    "opaque-state-session",
                    None,
                    None,
                )),
            ),
            context("req_opaque_binding_seed", CancellationToken::new()),
        )
        .await
        .expect("initial session binding");
    drop(initial);
    let generation = GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            json!({
                "model": "gpt-5.4",
                "session_id": "opaque-state-session",
                "input": [{"role": "user", "content": "complete history"}],
                "turnState": "unknown-account-state",
                "client_metadata": {"x-codex-turn-state": "unknown-account-state"}
            })
            .as_object()
            .expect("request object")
            .clone(),
        )
        .expect("payload")
        .with_context(Map::from_iter([
            ("use_websocket".to_owned(), json!(false)),
            ("turn_id".to_owned(), json!("opaque-turn")),
            ("turn_state".to_owned(), json!("unknown-account-state")),
        ])),
    );
    let mut stream = provider
        .execute(
            planned_request("openai", Operation::Generate(generation)),
            context("req_unowned_opaque_binding_hit", CancellationToken::new()),
        )
        .await
        .expect("full history may use the bound account");
    while let Some(event) = stream.next().await {
        event.expect("successful full history response");
    }
    let requests = server.received_requests().await.expect("captured requests");
    let request = &requests[0];
    assert!(captured_header_values(request, "x-codex-turn-state").is_empty());
    let body = captured_request_body(request);
    assert!(body.get("turnState").is_none());
    assert!(
        body.pointer("/client_metadata/x-codex-turn-state")
            .is_none()
    );
    assert_eq!(affinity.binding_count(), 1);
}

#[tokio::test]
async fn migrated_binding_should_reject_old_native_chain_and_accept_full_client_replay() {
    const OLD_ACCOUNT: &str = "acct_scope_old";
    const NEW_ACCOUNT: &str = "acct_scope_new";
    const SESSION: &str = "migrated-native-session";
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, OLD_ACCOUNT).await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CAPTURE_COMPLETED_SSE),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = provider_with_affinity_and_base_url(&store, Arc::clone(&affinity), server.uri());
    let operation = || {
        Operation::Generate(generate_with_session_context(
            SESSION,
            Some("root-thread"),
            None,
        ))
    };
    let old = Arc::clone(&provider)
        .execute(
            planned_request("openai", operation()),
            context("req_native_seed_old", CancellationToken::new()),
        )
        .await
        .expect("bind original account");
    assert_eq!(old.metadata().provider_account_id().as_str(), OLD_ACCOUNT);
    drop(old);
    create_account(&store, NEW_ACCOUNT).await;
    let old_id = ProviderAccountId::new(OLD_ACCOUNT).expect("old account ID");
    store
        .set_enabled(&old_id, false)
        .await
        .expect("disable old account");
    let switched = Arc::clone(&provider)
        .execute(
            planned_request("openai", operation()),
            context("req_native_migrate", CancellationToken::new()),
        )
        .await
        .expect("confirmed disable permits migration before send");
    assert_eq!(
        switched.metadata().provider_account_id().as_str(),
        NEW_ACCOUNT
    );
    drop(switched);
    store
        .set_enabled(&old_id, true)
        .await
        .expect("recover old account");

    let state = || {
        ProviderSessionState::new(
            "openai",
            Map::from_iter([
                ("account_id".to_owned(), json!(OLD_ACCOUNT)),
                (
                    "conversation_id".to_owned(),
                    json!("old-account-conversation"),
                ),
                ("client_turn_id".to_owned(), json!("same-client-turn")),
                ("turn_state".to_owned(), json!("old-account-state")),
                ("continuation_scope".to_owned(), json!("persisted")),
            ]),
        )
        .expect("old provider state")
    };
    let generation = |input: Value, previous: Option<&str>| {
        let mut body = Map::from_iter([
            ("model".to_owned(), json!("gpt-5.4")),
            ("session_id".to_owned(), json!(SESSION)),
            ("thread_id".to_owned(), json!("root-thread")),
            ("input".to_owned(), input),
            ("turnState".to_owned(), json!("old-account-state")),
        ]);
        if let Some(previous) = previous {
            body.insert("previous_response_id".to_owned(), json!(previous));
        }
        GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object("openai", body)
                .expect("payload")
                .with_context(Map::from_iter([
                    ("use_websocket".to_owned(), json!(false)),
                    ("turn_id".to_owned(), json!("same-client-turn")),
                    ("turn_state".to_owned(), json!("old-account-state")),
                ])),
        )
        .with_provider_session_state(state())
    };
    for continuation_attempt in [
        ContinuationAttempt::Native,
        ContinuationAttempt::ReplayOwner,
        ContinuationAttempt::ReplayAny,
    ] {
        let result = Arc::clone(&provider)
            .execute(
                planned_request(
                    "openai",
                    Operation::Generate(generation(
                        json!([{"role": "user", "content": "delta"}]),
                        Some("client-old-response"),
                    )),
                ),
                pinned_continuation_context(
                    "req_migrated_old_chain",
                    OLD_ACCOUNT,
                    "client-old-response",
                    "upstream-old-response",
                    1,
                    continuation_attempt,
                ),
            )
            .await;
        let Err(error) = result else {
            panic!("a native pin cannot restore the old session account");
        };
        assert_eq!(
            error.kind(),
            ProviderErrorKind::ContinuationRecoveryRequired
        );
        assert_eq!(error.send_state(), UpstreamSendState::NotSent);
        assert_eq!(
            error
                .client_visible_upstream_error()
                .and_then(|detail| detail.code()),
            Some("previous_response_not_found")
        );
    }
    assert!(
        server
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );

    let transcript = json!([
        {"role": "user", "content": "first request"},
        {"role": "assistant", "content": "previous answer"},
        {"role": "user", "content": "delta"}
    ]);
    let mut replay = provider
        .execute(
            planned_request(
                "openai",
                Operation::Generate(generation(transcript.clone(), None)),
            ),
            context_with_state_owner("req_migrated_full_replay", OLD_ACCOUNT),
        )
        .await
        .expect("complete client history starts a chain on the current account");
    assert_eq!(
        replay.metadata().provider_account_id().as_str(),
        NEW_ACCOUNT
    );
    while let Some(event) = replay.next().await {
        event.expect("full replay response");
    }
    let requests = server.received_requests().await.expect("replay request");
    let request = &requests[0];
    assert_eq!(
        captured_header_values(request, "chatgpt-account-id"),
        vec![b"chatgpt-acct_scope_new".to_vec()]
    );
    assert!(captured_header_values(request, "x-codex-turn-state").is_empty());
    let body = captured_request_body(request);
    assert_eq!(body["input"], transcript);
    assert!(body.get("previous_response_id").is_none());
    assert!(body.get("turnState").is_none());
    assert_eq!(affinity.binding_count(), 1);
}

#[tokio::test]
async fn full_client_history_should_reset_state_from_an_older_credential_revision() {
    let upstream = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    store
        .seed_api_key(
            "acct_provider_contract",
            upstream.uri(),
            provider_openai::credential::ResponsesTransport::Http,
        )
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(CAPTURE_COMPLETED_SSE),
        )
        .expect(1)
        .mount(&upstream)
        .await;
    let input = json!([
        {"role": "user", "content": "previous request"},
        {"role": "assistant", "content": "previous answer"},
        {"role": "user", "content": "new request"}
    ]);
    let generation = GenerateRequest::from_protocol_payload(
        ProtocolPayload::json_object(
            "openai",
            json!({"model": "gpt-5.4", "input": input, "turnState": "old-credential-state"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap()
        .with_context(Map::from_iter([
            ("turn_id".to_owned(), json!("same-credential-turn")),
            ("turn_state".to_owned(), json!("old-credential-state")),
        ])),
    )
    .with_provider_session_state(
        ProviderSessionState::new(
            "openai",
            json!({
                "account_id": "acct_provider_contract",
                "conversation_id": "old-credential-conversation",
                "credential_revision": 9,
                "continuation_scope": "persisted",
                "client_turn_id": "same-credential-turn",
                "turn_state": "old-credential-state"
            })
            .as_object()
            .unwrap()
            .clone(),
        )
        .unwrap(),
    );
    let mut stream = provider(&store)
        .execute(
            planned_request("openai", Operation::Generate(generation)),
            context_with_state_owner("req_new_chain_current_credential", "acct_provider_contract"),
        )
        .await
        .expect("complete history starts a new chain after credential revision changes");
    let mut state = None;
    while let Some(event) = stream.next().await {
        if let Some(update) = event.expect("full history response").session_update() {
            state = Some(update.payload().clone());
        }
    }
    let current_revision = store
        .account("acct_provider_contract")
        .unwrap()
        .revision()
        .get();
    let state = state.expect("current session state");
    assert_eq!(state["credential_revision"], json!(current_revision));
    let requests = upstream.received_requests().await.unwrap();
    let body = captured_request_body(&requests[0]);
    assert_eq!(body["input"], input);
    assert!(body.get("turnState").is_none());
    assert!(body.get("previous_response_id").is_none());
}

#[tokio::test]
async fn migrated_native_chain_should_request_replay_before_checking_http_owner_transport() {
    assert_native_chain_on_http_owner(false).await;
}

#[tokio::test]
async fn disabled_native_owner_should_migrate_to_http_account_before_requesting_replay() {
    assert_native_chain_on_http_owner(true).await;
}

async fn assert_native_chain_on_http_owner(migrate_during_continuation: bool) {
    const OLD_ACCOUNT: &str = "acct_scope_old";
    const CURRENT_ACCOUNT: &str = "acct_scope_new";
    const SESSION: &str = "http-owner-native-session";
    let api_server = MockServer::start().await;
    let oauth_server = MockServer::start().await;
    let store = Arc::new(MemoryAccountStore::default());
    if migrate_during_continuation {
        create_account(&store, OLD_ACCOUNT).await;
    } else {
        store
            .seed_api_key(
                CURRENT_ACCOUNT,
                api_server.uri(),
                provider_openai::credential::ResponsesTransport::Http,
            )
            .await;
    }
    let affinity = Arc::new(MemorySessionAffinity::default());
    let provider =
        provider_with_affinity_and_base_url(&store, Arc::clone(&affinity), oauth_server.uri());
    let full_request = || {
        planned_request(
            "openai",
            Operation::Generate(generate_with_session_context(SESSION, Some("thread"), None)),
        )
    };
    let initial = Arc::clone(&provider)
        .execute(
            full_request(),
            context("req_http_owner_seed", CancellationToken::new()),
        )
        .await
        .expect("bind the initial account before execution");
    assert_eq!(
        initial.metadata().provider_account_id().as_str(),
        if migrate_during_continuation {
            OLD_ACCOUNT
        } else {
            CURRENT_ACCOUNT
        }
    );
    assert!(initial.metadata().uses_session_account_binding());
    drop(initial);
    if migrate_during_continuation {
        store
            .seed_api_key(
                CURRENT_ACCOUNT,
                api_server.uri(),
                provider_openai::credential::ResponsesTransport::Http,
            )
            .await;
        store
            .set_enabled(&ProviderAccountId::new(OLD_ACCOUNT).unwrap(), false)
            .await
            .expect("disable the original native owner");
    } else {
        create_account(&store, OLD_ACCOUNT).await;
    }

    for (owner, expected_kind) in [
        (OLD_ACCOUNT, ProviderErrorKind::ContinuationRecoveryRequired),
        (CURRENT_ACCOUNT, ProviderErrorKind::Unsupported),
    ] {
        let generation = GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object(
                "openai",
                json!({
                    "model": "gpt-5.4",
                    "session_id": SESSION,
                    "thread_id": "thread",
                    "input": [{"role": "user", "content": "delta"}],
                    "previous_response_id": "client-local-response"
                })
                .as_object()
                .unwrap()
                .clone(),
            )
            .unwrap(),
        );
        // 原生 pin 默认是 connection-local，同账号时仍要求 WebSocket
        let result = Arc::clone(&provider)
            .execute(
                planned_request("openai", Operation::Generate(generation)),
                pinned_continuation_context(
                    "req_http_owner_native_chain",
                    owner,
                    "client-local-response",
                    "upstream-local-response",
                    1,
                    ContinuationAttempt::Native,
                ),
            )
            .await;
        let Err(error) = result else {
            panic!("connection-local delta must not send through the HTTP account");
        };
        assert_eq!(error.kind(), expected_kind);
        assert_eq!(error.send_state(), UpstreamSendState::NotSent);
        if owner == OLD_ACCOUNT {
            assert_eq!(
                error
                    .client_visible_upstream_error()
                    .and_then(|detail| detail.code()),
                Some("previous_response_not_found")
            );
        }
    }
    let current = provider
        .execute(
            full_request(),
            context("req_http_owner_after_rejection", CancellationToken::new()),
        )
        .await
        .expect("rejected native pins do not change the binding");
    assert_eq!(
        current.metadata().provider_account_id().as_str(),
        CURRENT_ACCOUNT
    );
    drop(current);
    assert_eq!(affinity.binding_count(), 1);
    assert!(api_server.received_requests().await.unwrap().is_empty());
    assert!(oauth_server.received_requests().await.unwrap().is_empty());
}
