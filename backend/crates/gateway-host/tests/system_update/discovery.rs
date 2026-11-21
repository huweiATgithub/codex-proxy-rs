//! 用本地 HTTP 源验证分页、查询缓存、限流与安装前复核

use super::*;
use gateway_admin::model::system::SystemUpdateChannel;
use wiremock::matchers::query_param;

const NEXT_PAGE: &str = r#"<https://api.github.com/repositories/1/releases?page=2>; rel="next""#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_checks_share_pages_and_choose_the_highest_compatible_release() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let service = Arc::new(fixture.service(&server));
    Mock::given(path(RELEASES_PATH))
        .and(query_param("page", "1"))
        .and(query_param("per_page", "100"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("link", NEXT_PAGE)
                .set_body_json(listed_releases(&["1.1.0", "2.0.0"]))
                .set_delay(Duration::from_millis(80)),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path(RELEASES_PATH))
        .and(query_param("page", "2"))
        .and(query_param("per_page", "100"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(listed_releases(&["1.10.0", "1.9.0"])),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path("/repos/owner/repository/releases/tags/v1.10.0"))
        .respond_with(
            release_response("1.10.0", available_assets("1.10.0"))
                .set_delay(Duration::from_millis(30)),
        )
        .expect(1)
        .mount(&server)
        .await;
    assert!(!service.version().await.unwrap().update_cached);
    assert!(server.received_requests().await.unwrap().is_empty());
    let summary = service
        .update_detail(SystemUpdateCheck::Summary, None)
        .await
        .unwrap();
    assert_eq!(summary.latest_version, "1.10.0");
    assert!(summary.notes.is_none());
    assert_eq!(server.received_requests().await.unwrap().len(), 3);

    let mut callers = tokio::task::JoinSet::new();
    for i in 0..512 {
        let service = service.clone();
        callers.spawn(async move {
            let check = if i % 2 == 0 {
                SystemUpdateCheck::Refresh
            } else {
                SystemUpdateCheck::Detail
            };
            service.update_detail(check, None).await.unwrap()
        });
    }
    while let Some(detail) = callers.join_next().await {
        let detail = detail.unwrap();
        assert_eq!(detail.latest_version, "1.10.0");
        assert_eq!(detail.notes.as_deref(), Some("notes"));
    }
    for _ in 0..64 {
        assert!(service.version().await.unwrap().has_update);
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    // wiremock 析构时同步等待异步校验
    tokio::task::spawn_blocking(move || drop(server))
        .await
        .expect("verify mock expectations");
}

#[tokio::test]
async fn cold_summary_stampede_and_slow_source_do_not_block_runtime_probes() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let service = Arc::new(fixture.service(&server));
    let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
    Mock::given(path(RELEASES_PATH))
        .respond_with(move |_: &wiremock::Request| {
            started.send(()).unwrap();
            ResponseTemplate::new(200)
                .set_body_json(listed_releases(&[TARGET_VERSION]))
                .set_delay(Duration::from_millis(500))
        })
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(path(format!(
        "/repos/owner/repository/releases/tags/v{TARGET_VERSION}"
    )))
    .respond_with(release_response(
        TARGET_VERSION,
        available_assets(TARGET_VERSION),
    ))
    .expect(1)
    .mount(&server)
    .await;
    let mut callers = tokio::task::JoinSet::new();
    for _ in 0..256 {
        let service = service.clone();
        callers.spawn(async move {
            service
                .update_detail(SystemUpdateCheck::Summary, None)
                .await
                .unwrap()
        });
    }
    observed.recv().await.unwrap();
    tokio::time::timeout(Duration::from_millis(150), async {
        for _ in 0..128 {
            let version = service.version().await.unwrap();
            assert_eq!(version.version, "1.0.0");
            assert!(!version.update_cached);
        }
    })
    .await
    .expect("本地版本探测不能等待在途的 GitHub 查询");
    while let Some(summary) = callers.join_next().await {
        assert!(summary.unwrap().has_update);
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn api_cooldown_is_shared_and_summary_does_not_advertise_an_unverified_release() {
    for status in [403, 429] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let service = fixture.service(&server);
        mount_releases(&server, &["1.1.0", "1.2.0-beta.1"]).await;
        let limit = if status == 429 {
            ResponseTemplate::new(status).insert_header("retry-after", "300")
        } else {
            let reset = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
                + 300;
            ResponseTemplate::new(status)
                .insert_header("x-ratelimit-remaining", "0")
                .insert_header("x-ratelimit-reset", reset.to_string())
        };
        Mock::given(path("/repos/owner/repository/releases/tags/v1.1.0"))
            .respond_with(limit)
            .expect(1)
            .mount(&server)
            .await;
        let first = service
            .update_detail(SystemUpdateCheck::Refresh, None)
            .await
            .unwrap();
        assert!(first.warning.is_some());
        let retry_at = first.retry_at.expect("限流应提供重试时间");
        assert!((298..=300).contains(&(retry_at - chrono::Utc::now()).num_seconds()));
        // 发行列表缓存已到刷新时间，详情触发的 API 退避仍应阻止请求
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(61)).await;
        tokio::time::resume();
        for _ in 0..32 {
            let second = service
                .update_detail(SystemUpdateCheck::Refresh, Some(SystemUpdateChannel::Beta))
                .await
                .unwrap();
            assert_eq!(second.warning, first.warning);
            assert_eq!(second.retry_at, Some(retry_at));
            assert!(!second.has_update);
        }
        assert!(
            !service
                .update_detail(SystemUpdateCheck::Summary, None)
                .await
                .unwrap()
                .has_update
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
}

#[tokio::test]
async fn malformed_or_oversized_release_list_returns_a_cached_warning() {
    for body in [
        "{}".to_owned(),
        "[{}]".to_owned(),
        "x".repeat(4 * 1024 * 1024 + 1),
    ] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let service = fixture.service(&server);
        Mock::given(path(RELEASES_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .expect(1)
            .mount(&server)
            .await;
        for _ in 0..2 {
            let detail = service
                .update_detail(SystemUpdateCheck::Refresh, None)
                .await
                .unwrap();
            assert!(detail.warning.is_some());
            assert!(!detail.has_update);
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn partial_release_listing_does_not_offer_a_candidate() {
    for (fail_second_page, expected_requests, warning) in
        [(true, 2, "HTTP 502"), (false, 8, "页数上限")]
    {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let service = fixture.service(&server);
        Mock::given(path(RELEASES_PATH))
            .respond_with(move |request: &wiremock::Request| {
                if fail_second_page
                    && request
                        .url
                        .query_pairs()
                        .any(|(key, value)| key == "page" && value == "2")
                {
                    ResponseTemplate::new(502)
                } else {
                    ResponseTemplate::new(200)
                        .insert_header("link", NEXT_PAGE)
                        .set_body_json(listed_releases(&[TARGET_VERSION]))
                }
            })
            .expect(expected_requests)
            .mount(&server)
            .await;
        let detail = service
            .update_detail(SystemUpdateCheck::Summary, None)
            .await
            .unwrap();
        assert!(!detail.has_update);
        assert!(detail.warning.unwrap().contains(warning));
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            expected_requests as usize
        );
    }
}

#[tokio::test]
async fn different_release_tags_with_the_same_highest_precedence_require_resolution() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let service = fixture.service(&server);
    mount_releases(&server, &["1.1.0+first", "1.1.0+second"]).await;
    let detail = service
        .update_detail(SystemUpdateCheck::Summary, None)
        .await
        .unwrap();
    assert!(!detail.has_update);
    assert!(detail.warning.unwrap().contains("并列"));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn exact_release_requires_public_matching_identity_before_display() {
    for (status, tag, draft, prerelease) in [
        (404, "", false, false),
        (200, "v1.2.0", false, false),
        (200, "v1.1.0", true, false),
        (200, "v1.1.0", false, true),
    ] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let service = fixture.service(&server);
        mount_releases(&server, &["1.1.0"]).await;
        Mock::given(path("/repos/owner/repository/releases/tags/v1.1.0"))
            .respond_with(ResponseTemplate::new(status).set_body_json(
                serde_json::json!({"tag_name":tag,"draft":draft,"prerelease":prerelease}),
            ))
            .mount(&server)
            .await;
        let detail = service
            .update_detail(SystemUpdateCheck::Detail, None)
            .await
            .unwrap();
        assert!(!detail.has_update);
        assert!(detail.warning.is_some());
    }
}

#[tokio::test]
async fn install_rechecks_even_a_warm_candidate_and_never_downloads_a_changed_target() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let service = fixture.service(&server);
    let requests = AtomicUsize::new(0);
    Mock::given(path(RELEASES_PATH))
        .respond_with(move |_: &wiremock::Request| {
            let target = if requests.fetch_add(1, Ordering::SeqCst) == 0 {
                "1.1.0"
            } else {
                "1.2.0"
            };
            ResponseTemplate::new(200).set_body_json(listed_releases(&[target]))
        })
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(path("/repos/owner/repository/releases/tags/v1.1.0"))
        .respond_with(release_response("1.1.0", available_assets("1.1.0")))
        .expect(1)
        .mount(&server)
        .await;
    let detail = service
        .update_detail(SystemUpdateCheck::Detail, None)
        .await
        .unwrap();
    assert!(detail.has_update);
    let status = complete_update(&service, &detail.latest_version).await;
    assert_eq!(status.operation.status, SystemOperationStatus::Failed);
    assert!(status.operation.error.unwrap().contains("重新检查并确认"));
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn channels_pointing_to_one_release_share_its_detail() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let service = fixture.service(&server);
    mount_release_json(
        &server,
        TARGET_VERSION,
        available_assets(TARGET_VERSION),
        None,
    )
    .await;
    for channel in [
        SystemUpdateChannel::Stable,
        SystemUpdateChannel::Rc,
        SystemUpdateChannel::Beta,
        SystemUpdateChannel::Alpha,
    ] {
        assert!(
            service
                .update_detail(SystemUpdateCheck::Refresh, Some(channel))
                .await
                .unwrap()
                .has_update
        );
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn transient_release_absence_expires_and_refresh_interval_is_bounded() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let service = fixture.service(&server);
    Mock::given(path(RELEASES_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(listed_releases(&[TARGET_VERSION])))
        .expect(2)
        .mount(&server)
        .await;
    let calls = AtomicUsize::new(0);
    Mock::given(path(format!(
        "/repos/owner/repository/releases/tags/v{TARGET_VERSION}"
    )))
    .respond_with(move |_: &wiremock::Request| {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            ResponseTemplate::new(404)
        } else {
            release_response(TARGET_VERSION, available_assets(TARGET_VERSION))
        }
    })
    .expect(3)
    .mount(&server)
    .await;
    assert!(
        service
            .update_detail(SystemUpdateCheck::Detail, None)
            .await
            .unwrap()
            .warning
            .is_some()
    );
    assert!(
        service
            .update_detail(SystemUpdateCheck::Refresh, None)
            .await
            .unwrap()
            .warning
            .is_some()
    );
    // 只推进本进程缓存时钟，实际 HTTP 连接使用正常计时
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::resume();
    assert!(
        service
            .update_detail(SystemUpdateCheck::Detail, None)
            .await
            .unwrap()
            .has_update
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::resume();
    assert!(
        service
            .update_detail(SystemUpdateCheck::Detail, None)
            .await
            .unwrap()
            .cached
    );
    assert!(
        !service
            .update_detail(SystemUpdateCheck::Refresh, None)
            .await
            .unwrap()
            .cached
    );
    assert!(
        service
            .update_detail(SystemUpdateCheck::Refresh, None)
            .await
            .unwrap()
            .cached
    );
}

#[tokio::test]
async fn metadata_never_follows_redirects_or_accepts_oversized_release() {
    for status in [302, 200] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let service = fixture.service(&server);
        mount_releases(&server, &[TARGET_VERSION]).await;
        let response = if status == 302 {
            ResponseTemplate::new(status)
                .insert_header("location", format!("{}/untrusted", server.uri()))
        } else {
            ResponseTemplate::new(200).set_body_bytes(vec![b' '; 4 * 1024 * 1024 + 1])
        };
        Mock::given(path(format!(
            "/repos/owner/repository/releases/tags/v{TARGET_VERSION}"
        )))
        .respond_with(response)
        .expect(1)
        .mount(&server)
        .await;
        let detail = service
            .update_detail(SystemUpdateCheck::Detail, None)
            .await
            .unwrap();
        assert!(!detail.has_update);
        assert!(detail.warning.unwrap().contains(if status == 302 {
            "HTTP 302"
        } else {
            "大小限制"
        }));
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }
}

#[tokio::test]
async fn slow_metadata_times_out_without_retry_or_blocking_future_version_reads() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let service = Arc::new(fixture.service(&server));
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    Mock::given(path(RELEASES_PATH))
        .respond_with(move |_: &wiremock::Request| {
            sent.send(()).unwrap();
            ResponseTemplate::new(200).set_delay(Duration::from_secs(30))
        })
        .expect(1)
        .mount(&server)
        .await;
    let worker = Arc::clone(&service);
    let pending = tokio::spawn(async move {
        worker
            .update_detail(SystemUpdateCheck::Summary, None)
            .await
            .unwrap()
    });
    received.recv().await.unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(11)).await;
    let detail = pending.await.unwrap();
    tokio::time::resume();
    assert!(detail.warning.is_some());
    assert!(!detail.has_update);
    assert_eq!(service.version().await.unwrap().version, "1.0.0");
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn summary_and_local_probe_require_downloadable_target() {
    for assets in [
        vec![],
        available_assets("1.2.999"),
        vec![available_assets(TARGET_VERSION).remove(0)],
        {
            let mut assets = available_assets(TARGET_VERSION);
            assets[1]["size"] = 0.into();
            assets
        },
    ] {
        let server = MockServer::start().await;
        let fixture = Fixture::new();
        let service = fixture.service(&server);
        mount_release_json(&server, TARGET_VERSION, assets, None).await;
        let summary = service
            .update_detail(SystemUpdateCheck::Summary, None)
            .await
            .unwrap();
        assert!(summary.warning.is_some());
        assert!(!summary.has_update);
        assert!(!service.version().await.unwrap().has_update);
    }
}
