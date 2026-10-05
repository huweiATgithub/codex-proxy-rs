//! 验证共享账号绑定的根线程与子线程分别维护传输回退状态

use super::*;

#[tokio::test]
async fn child_http_fallback_should_not_disable_sibling_or_root_websocket() {
    const ACCOUNT: &str = "acct_thread_spawn_affinity";
    const SESSION: &str = "thread-transport-root";
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, ACCOUNT).await;
    let affinity = Arc::new(MemorySessionAffinity::default());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream listener");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let server = tokio::spawn(async move {
        let (mut opening, _) = listener.accept().await.expect("accept child WS opening");
        let request = capture_http_request(&mut opening).await;
        assert!(String::from_utf8_lossy(&request).starts_with("GET /codex/responses"));
        let body = r#"{"error":{"code":"upgrade_required","message":"use HTTP"}}"#;
        opening
            .write_all(
                format!(
                    "HTTP/1.1 426 Upgrade Required\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .expect("reject child WebSocket");
        drop(opening);

        for (index, websocket) in [false, true, true, false].into_iter().enumerate() {
            let (mut stream, _) = listener.accept().await.expect("accept thread request");
            if websocket {
                let mut stream = accept_codex_test_websocket(stream).await;
                let request = stream
                    .next()
                    .await
                    .expect("WebSocket request")
                    .expect("valid WebSocket request");
                let payload: Value =
                    serde_json::from_str(request.to_text().expect("WebSocket request text"))
                        .expect("WebSocket request JSON");
                assert_eq!(payload.get("type"), Some(&json!("response.create")));
                stream
                    .send(Message::Text(
                        json!({
                            "type": "response.completed",
                            "response": {
                                "id": format!("resp_thread_transport_{index}"),
                                "model": "gpt-5.4",
                                "status": "completed",
                                "output": [],
                                "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
                            }
                        })
                        .to_string()
                        .into(),
                    ))
                    .await
                    .expect("complete sibling or root WebSocket");
            } else {
                let request = capture_http_request(&mut stream).await;
                assert!(String::from_utf8_lossy(&request).starts_with("POST /codex/responses"));
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{CAPTURE_COMPLETED_SSE}",
                            CAPTURE_COMPLETED_SSE.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .expect("complete child HTTP request");
            }
        }
    });
    let provider = provider_with_affinity_and_base_url(&store, Arc::clone(&affinity), base_url);
    let operation = |thread| {
        Operation::Generate(generate_with_session_context(
            SESSION,
            Some(thread),
            (thread != SESSION).then_some(r#"{"subagent_kind":"thread_spawn"}"#),
        ))
    };
    let mut first = Arc::clone(&provider)
        .execute(
            planned_request("openai", operation("child-http")),
            context("req_child_upgrade_required", CancellationToken::new()),
        )
        .await
        .expect("prepare child WebSocket");
    let error = loop {
        match first.next().await {
            Some(Ok(_)) => {}
            Some(Err(error)) => break error,
            None => panic!("426 opening must surface a fallback signal"),
        }
    };
    assert_eq!(
        error.pre_delivery_retry(),
        Some(PreDeliveryRetry::SameAccountTransportFallback)
    );
    drop(first);

    for (request_id, thread, transport) in [
        ("req_child_http", "child-http", "http_sse"),
        ("req_sibling_ws", "child-websocket", "websocket"),
        ("req_root_ws", SESSION, "websocket"),
        ("req_child_still_http", "child-http", "http_sse"),
    ] {
        let mut stream = Arc::clone(&provider)
            .execute(
                planned_request("openai", operation(thread)),
                context(request_id, CancellationToken::new()),
            )
            .await
            .expect("prepare thread request");
        assert_eq!(stream.metadata().provider_account_id().as_str(), ACCOUNT);
        assert_eq!(stream.metadata().transport().as_str(), transport);
        let mut completed = false;
        while let Some(event) = stream.next().await {
            let event = event.expect("thread response");
            completed |= event.wire_event().is_some_and(|wire| {
                wire.data().get("type").and_then(Value::as_str) == Some("response.completed")
            });
        }
        assert!(
            completed,
            "{request_id} must finish on its selected transport"
        );
    }
    assert_eq!(affinity.binding_count(), 1);
    server.await.expect("thread transport server");
}
