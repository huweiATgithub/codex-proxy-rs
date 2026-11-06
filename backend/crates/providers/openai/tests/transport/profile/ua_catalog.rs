//! 验证 UA 发布目录解析、刷新、完整身份跟随与离线恢复

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use gateway_core::account::OpaqueProviderData;
use gateway_core::provider_ports::{ProviderCatalogCacheKey, ProviderCatalogScope};
use gateway_core::routing::ProviderKind;
use provider_openai::transport::profile::identity::{
    CatalogClient, CatalogProfile, RequestProfileSelection,
};
use provider_openai::transport::profile::ua_catalog::{
    UaCatalogError, UaCatalogService, UaCatalogSource, UaCatalogState, UaCatalogTransport,
    parse_ua_matrix,
};
use provider_openai::transport::profile::{CodexWireProfile, CodexWireProfileState};
use serde_json::{Value, json};

use crate::support::catalog_cache;

fn cli_matrix(version: &str, terminal: &str) -> Value {
    json!({
        "schema_version":1, "codex_version":version,
        "platforms":{"linux-ubuntu-x64":{
            "CLI":format!("codex-tui/{version} (Ubuntu 24.4.0; x86_64) {terminal} (codex-tui; {version})"),
            "Exec":format!("codex_exec/{version} (Ubuntu 24.4.0; x86_64) {terminal} (codex_exec; {version})"),
        }}
    })
}

fn profiled_cli_matrix(version: &str, schema: u8, herdr: &str) -> Value {
    let mut matrix = cli_matrix(version, "xterm-256color");
    matrix["schema_version"] = json!(schema);
    let clients = |terminal| cli_matrix(version, terminal)["platforms"]["linux-ubuntu-x64"].clone();
    matrix["platforms"]["linux-ubuntu-x64"]["profiles"] = json!({
        "WindowsTerminal": clients("WindowsTerminal"),
        "vscode": clients("vscode/1.141.0"),
        "herdr": clients(herdr),
    });
    if schema == 3 {
        let platform = matrix["platforms"]["linux-ubuntu-x64"]
            .as_object_mut()
            .unwrap();
        let default = json!({"CLI": platform.remove("CLI").unwrap(), "Exec": platform.remove("Exec").unwrap()});
        platform["profiles"]["xterm-256color"] = default;
    }
    matrix
}

#[test]
fn terminal_profiles_preserve_exact_uas_in_both_matrix_layouts() {
    for schema in [2, 3] {
        let mut matrix = profiled_cli_matrix("0.162.1", schema, "herdr/0.9.3");
        matrix["platforms"]["linux-ubuntu-x64"]["profiles"]["vscode"] = Value::Null;
        let entries = parse_ua_matrix(
            UaCatalogSource::Cli,
            "0.162.1",
            &serde_json::to_vec(&matrix).unwrap(),
        )
        .unwrap();
        assert_eq!(entries.len(), 6);
        for client in [CatalogClient::Cli, CatalogClient::Exec] {
            let entry = entries
                .iter()
                .find(|entry| {
                    entry.client == client && entry.profile == Some(CatalogProfile::Herdr)
                })
                .unwrap();
            let field = if client == CatalogClient::Cli {
                "CLI"
            } else {
                "Exec"
            };
            assert_eq!(
                entry.user_agent,
                matrix["platforms"]["linux-ubuntu-x64"]["profiles"]["herdr"][field]
            );
        }
    }
}

#[test]
fn terminal_profiles_reject_mislabeled_incomplete_and_unknown_data() {
    let baseline = profiled_cli_matrix("0.162.1", 3, "herdr/0.9.3");
    for malformed in [
        {
            let mut value = baseline.clone();
            value["platforms"]["linux-ubuntu-x64"]["profiles"]["herdr"]["CLI"] =
                value["platforms"]["linux-ubuntu-x64"]["profiles"]["xterm-256color"]["CLI"].clone();
            value
        },
        {
            let mut value = baseline.clone();
            value["platforms"]["linux-ubuntu-x64"]["profiles"]["xterm-256color"] = Value::Null;
            value
        },
        {
            let mut value = baseline.clone();
            value["platforms"]["linux-ubuntu-x64"]["profiles"]
                .as_object_mut()
                .unwrap()
                .remove("vscode");
            value
        },
        {
            let mut value = baseline.clone();
            value["platforms"]["linux-ubuntu-x64"]["profiles"]["unknown"] = Value::Null;
            value
        },
        {
            let mut value = baseline;
            value["schema_version"] = json!(2);
            value
        },
    ] {
        assert!(
            parse_ua_matrix(
                UaCatalogSource::Cli,
                "0.162.1",
                &serde_json::to_vec(&malformed).unwrap()
            )
            .is_err()
        );
    }
}

fn desktop_matrix() -> Value {
    json!({
        "schema_version":1, "desktop_version":"26.917.62051",
        "platforms":{"linux-ubuntu-x64":{
            "codex_version":"0.155.0-alpha.16.3",
            "Desktop":"Codex Desktop/0.155.0-alpha.16.3 (Ubuntu 24.4.0; x86_64) unknown (Codex Desktop; 26.917.62051)"
        }}
    })
}

fn document(value: Value) -> OpaqueProviderData {
    OpaqueProviderData::new(value.as_object().unwrap().clone())
}

#[test]
fn matrices_preserve_exact_identity_and_reject_conflicting_release_metadata() {
    for (source, version, matrix, count) in [
        (
            UaCatalogSource::Desktop,
            "26.917.62051",
            desktop_matrix(),
            1,
        ),
        (
            UaCatalogSource::Cli,
            "0.156.1",
            cli_matrix("0.156.1", "xterm-256color"),
            2,
        ),
    ] {
        let entries =
            parse_ua_matrix(source, version, &serde_json::to_vec(&matrix).unwrap()).unwrap();
        assert_eq!(entries.len(), count);
        for entry in &entries {
            let field = match entry.client {
                CatalogClient::Desktop => "Desktop",
                CatalogClient::Cli => "CLI",
                CatalogClient::Exec => "Exec",
            };
            assert_eq!(
                entry.user_agent,
                matrix["platforms"]["linux-ubuntu-x64"][field]
            );
        }
        for malformed in [
            {
                let mut value = matrix.clone();
                value["schema_version"] = json!(2);
                value
            },
            {
                let mut value = matrix.clone();
                if source == UaCatalogSource::Desktop {
                    value["platforms"]["linux-ubuntu-x64"]["codex_version"] = json!("0.1.0");
                } else {
                    value["codex_version"] = json!("0.1.0");
                }
                value
            },
        ] {
            assert!(
                parse_ua_matrix(source, version, &serde_json::to_vec(&malformed).unwrap()).is_err()
            );
        }
        assert!(parse_ua_matrix(source, "0.1.0", &serde_json::to_vec(&matrix).unwrap()).is_err());
    }
}

#[derive(Clone)]
struct Release {
    version: String,
    asset: u64,
    terminal: String,
}

fn release(version: &str, asset: u64, terminal: &str) -> Release {
    Release {
        version: version.to_owned(),
        asset,
        terminal: terminal.to_owned(),
    }
}

struct CatalogTransport {
    cli: Mutex<Vec<Release>>,
    matrix_override: Mutex<Option<Value>>,
    fail_cli: AtomicBool,
    retire_ubuntu: AtomicBool,
    block_cli: AtomicBool,
    cli_blocked: tokio::sync::Notify,
    endless_pages: AtomicBool,
    listings: AtomicUsize,
    downloads: AtomicUsize,
}

impl Default for CatalogTransport {
    fn default() -> Self {
        Self {
            cli: Mutex::new(vec![
                release("0.9.0", 1, "xterm"),
                release("0.10.0", 2, "xterm"),
            ]),
            matrix_override: Mutex::new(None),
            fail_cli: AtomicBool::new(false),
            retire_ubuntu: AtomicBool::new(false),
            block_cli: AtomicBool::new(false),
            cli_blocked: tokio::sync::Notify::new(),
            endless_pages: AtomicBool::new(false),
            listings: AtomicUsize::new(0),
            downloads: AtomicUsize::new(0),
        }
    }
}

impl UaCatalogTransport for CatalogTransport {
    fn releases(
        &self,
        source: UaCatalogSource,
        page: usize,
    ) -> BoxFuture<'_, Result<Vec<u8>, UaCatalogError>> {
        Box::pin(async move {
            self.listings.fetch_add(1, Ordering::SeqCst);
            if source == UaCatalogSource::Cli && self.fail_cli.load(Ordering::SeqCst) {
                return Err(UaCatalogError::Fetch);
            }
            if source == UaCatalogSource::Cli && self.block_cli.load(Ordering::SeqCst) {
                self.cli_blocked.notify_one();
                return std::future::pending().await;
            }
            let releases = if source == UaCatalogSource::Desktop {
                vec![release("26.917.62051", 3, "unknown")]
            } else if self.endless_pages.load(Ordering::SeqCst) {
                (0..100)
                    .map(|index| {
                        release(
                            &format!("0.{}.0", page * 100 + index),
                            (page * 100 + index) as u64,
                            "xterm",
                        )
                    })
                    .collect()
            } else {
                self.cli.lock().unwrap().clone()
            };
            let metadata: Vec<_> = releases.iter().map(|release| json!({
                "tag_name":format!("v{}", release.version), "draft":false, "prerelease":false,
                "assets":[{"name":"ua-matrix.json","id":release.asset,"updated_at":"2026-09-24T00:00:00Z","size":1024,"state":"uploaded"}]
            })).collect();
            Ok(serde_json::to_vec(&metadata).unwrap())
        })
    }

    fn matrix<'a>(
        &'a self,
        source: UaCatalogSource,
        tag: &'a str,
    ) -> BoxFuture<'a, Result<Vec<u8>, UaCatalogError>> {
        Box::pin(async move {
            self.downloads.fetch_add(1, Ordering::SeqCst);
            let value = if source == UaCatalogSource::Desktop {
                desktop_matrix()
            } else {
                let cli = self.cli.lock().unwrap();
                let release = cli
                    .iter()
                    .find(|release| tag == format!("v{}", release.version))
                    .unwrap();
                let mut matrix = self
                    .matrix_override
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| cli_matrix(&release.version, &release.terminal));
                if self.retire_ubuntu.load(Ordering::SeqCst) {
                    let mut platform = matrix["platforms"]
                        .as_object_mut()
                        .unwrap()
                        .remove("linux-ubuntu-x64")
                        .unwrap();
                    for entry in ["CLI", "Exec"] {
                        platform[entry] = json!(
                            platform[entry]
                                .as_str()
                                .unwrap()
                                .replace("Ubuntu 24.4.0", "Alpine Linux 3.24.1")
                        );
                    }
                    matrix["platforms"]["linux-alpine-x64"] = platform;
                }
                matrix
            };
            Ok(serde_json::to_vec(&value).unwrap())
        })
    }
}

#[tokio::test(start_paused = true)]
async fn follow_and_cache_restore_keep_profiles_separate_and_legacy_defaults_readable() {
    let state = CodexWireProfileState::new(CodexWireProfile::default());
    let transport = Arc::new(CatalogTransport::default());
    let cache = catalog_cache();
    let service = UaCatalogService::with_transport(
        ProviderKind::new("openai").unwrap(),
        state.catalog().clone(),
        cache.clone(),
        transport.clone(),
    )
    .unwrap();
    service.refresh().await.unwrap();
    let legacy_entry = state
        .catalog()
        .latest(
            CatalogClient::Cli,
            "linux-ubuntu-x64",
            Some(CatalogProfile::Xterm),
        )
        .unwrap();
    assert!(legacy_entry.profile.is_none());
    let legacy = RequestProfileSelection::parse(&document(
        json!({"mode":"catalog","versionMode":"latest","entry":legacy_entry}),
    ))
    .unwrap();

    *transport.cli.lock().unwrap() = vec![release("0.11.0", 5, "xterm-256color")];
    *transport.matrix_override.lock().unwrap() =
        Some(profiled_cli_matrix("0.11.0", 3, "herdr/0.9.3"));
    tokio::time::advance(Duration::from_secs(61)).await;
    service.refresh().await.unwrap();
    let herdr = state
        .catalog()
        .latest(
            CatalogClient::Cli,
            "linux-ubuntu-x64",
            Some(CatalogProfile::Herdr),
        )
        .unwrap();
    let fixed = RequestProfileSelection::parse(&document(
        json!({"mode":"catalog","versionMode":"fixed","entry":herdr}),
    ))
    .unwrap();
    let follow = RequestProfileSelection::parse(&document(
        json!({"mode":"catalog","versionMode":"latest","entry":herdr}),
    ))
    .unwrap();
    assert!(
        legacy
            .resolve(&state)
            .unwrap()
            .user_agent()
            .contains(" xterm-256color ")
    );

    *transport.cli.lock().unwrap() = vec![release("0.12.0", 6, "xterm-256color")];
    *transport.matrix_override.lock().unwrap() =
        Some(profiled_cli_matrix("0.12.0", 3, "herdr/0.9.4"));
    tokio::time::advance(Duration::from_secs(61)).await;
    service.refresh().await.unwrap();
    assert!(
        follow
            .resolve(&state)
            .unwrap()
            .user_agent()
            .contains("herdr/0.9.4")
    );
    assert_eq!(
        fixed.resolve(&state).unwrap().user_agent(),
        herdr.user_agent
    );
    assert!(
        legacy
            .resolve(&state)
            .unwrap()
            .user_agent()
            .starts_with("codex-tui/0.12.0")
    );

    let restored = CodexWireProfileState::new(CodexWireProfile::default());
    UaCatalogService::with_transport(
        ProviderKind::new("openai").unwrap(),
        restored.catalog().clone(),
        cache,
        transport,
    )
    .unwrap()
    .restore()
    .await;
    assert_eq!(
        follow.resolve(&restored).unwrap().user_agent(),
        follow.resolve(&state).unwrap().user_agent()
    );
    assert_eq!(
        restored.catalog().snapshot()["entries"]
            .as_array()
            .unwrap()
            .len(),
        9
    );
}

#[tokio::test(start_paused = true)]
async fn follow_updates_complete_entry_while_pin_custom_and_failed_refresh_keep_identity() {
    let state = CodexWireProfileState::new(CodexWireProfile::default());
    let cache = catalog_cache();
    let transport = Arc::new(CatalogTransport::default());
    let service = UaCatalogService::with_transport(
        ProviderKind::new("openai").unwrap(),
        state.catalog().clone(),
        cache.clone(),
        transport.clone(),
    )
    .unwrap();
    let (first, repeated) = tokio::join!(service.refresh(), service.refresh());
    first.unwrap();
    repeated.unwrap();
    assert_eq!(transport.listings.load(Ordering::SeqCst), 2);
    let entry = state
        .catalog()
        .latest(
            CatalogClient::Cli,
            "linux-ubuntu-x64",
            Some(CatalogProfile::Xterm),
        )
        .unwrap();
    assert_eq!(entry.release, "0.10.0");
    let fixed = document(json!({"mode":"catalog","versionMode":"fixed","entry":entry}));
    let follow = document(json!({"mode":"catalog","versionMode":"latest","entry":entry}));
    let custom = document(json!({"mode":"custom","userAgent":entry.user_agent}));
    let frozen = RequestProfileSelection::parse(&follow)
        .unwrap()
        .resolve(&state)
        .unwrap();

    // 同一 release 的资产可以纠正完整 UA；固定与自定义仍保留保存时的快照
    *transport.cli.lock().unwrap() = vec![release("0.10.0", 4, "screen-256color")];
    tokio::time::advance(Duration::from_secs(61)).await;
    service.refresh().await.unwrap();
    assert_eq!(transport.downloads.load(Ordering::SeqCst), 4);
    let corrected = state
        .catalog()
        .latest(
            CatalogClient::Cli,
            "linux-ubuntu-x64",
            Some(CatalogProfile::Xterm),
        )
        .unwrap();
    assert!(corrected.user_agent.contains("screen-256color"));
    for stable in [&fixed, &custom] {
        assert_eq!(
            RequestProfileSelection::parse(stable)
                .unwrap()
                .resolve(&state)
                .unwrap()
                .user_agent(),
            entry.user_agent
        );
    }
    assert_eq!(frozen.user_agent(), entry.user_agent);
    let preview = state.preview_selection(&follow).unwrap();
    assert_eq!(
        preview.expose_to_provider()["configuration"]["entry"]["userAgent"],
        corrected.user_agent
    );

    *transport.cli.lock().unwrap() = vec![release("0.11.0", 5, "new-terminal")];
    tokio::time::advance(Duration::from_secs(61)).await;
    service.refresh().await.unwrap();
    let latest = RequestProfileSelection::parse(&follow)
        .unwrap()
        .resolve(&state)
        .unwrap();
    assert!(latest.user_agent().contains("codex-tui/0.11.0"));
    assert!(latest.user_agent().contains("new-terminal"));
    transport.fail_cli.store(true, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(61)).await;
    assert!(service.refresh().await.is_err());
    assert_eq!(
        RequestProfileSelection::parse(&follow)
            .unwrap()
            .resolve(&state)
            .unwrap()
            .user_agent(),
        latest.user_agent()
    );
    let snapshot = state.catalog().snapshot();
    assert!(snapshot["sources"][0]["error"].is_null());
    assert!(snapshot["sources"][1]["error"].is_string());

    let restored = UaCatalogState::default();
    let offline = UaCatalogService::with_transport(
        ProviderKind::new("openai").unwrap(),
        restored.clone(),
        cache,
        transport.clone(),
    )
    .unwrap();
    let calls = transport.listings.load(Ordering::SeqCst);
    offline.restore().await;
    assert_eq!(transport.listings.load(Ordering::SeqCst), calls);
    assert_eq!(
        restored
            .latest(
                CatalogClient::Cli,
                "linux-ubuntu-x64",
                Some(CatalogProfile::Xterm)
            )
            .unwrap()
            .user_agent,
        latest.user_agent()
    );
}

#[tokio::test(start_paused = true)]
async fn retired_environment_does_not_block_other_updates_or_change_the_saved_selection() {
    let state = CodexWireProfileState::new(CodexWireProfile::default());
    let transport = Arc::new(CatalogTransport::default());
    let service = UaCatalogService::with_transport(
        ProviderKind::new("openai").unwrap(),
        state.catalog().clone(),
        catalog_cache(),
        transport.clone(),
    )
    .unwrap();
    service.refresh().await.unwrap();
    let entry = state
        .catalog()
        .latest(
            CatalogClient::Cli,
            "linux-ubuntu-x64",
            Some(CatalogProfile::Xterm),
        )
        .unwrap();
    let follow = RequestProfileSelection::parse(&document(
        json!({"mode":"catalog", "versionMode":"latest", "entry":entry}),
    ))
    .unwrap();

    transport.retire_ubuntu.store(true, Ordering::SeqCst);
    *transport.cli.lock().unwrap() = vec![release("0.11.0", 5, "xterm")];
    tokio::time::advance(Duration::from_secs(61)).await;
    service.refresh().await.unwrap();
    assert_eq!(
        state
            .catalog()
            .latest(
                CatalogClient::Cli,
                "linux-alpine-x64",
                Some(CatalogProfile::Xterm)
            )
            .unwrap()
            .release,
        "0.11.0"
    );
    assert!(
        state
            .catalog()
            .latest(
                CatalogClient::Cli,
                "linux-ubuntu-x64",
                Some(CatalogProfile::Xterm)
            )
            .is_none()
    );
    assert_eq!(
        follow.resolve(&state).unwrap().user_agent(),
        entry.user_agent
    );
}

#[tokio::test(start_paused = true)]
async fn cancelled_refresh_releases_the_lock_and_keeps_the_last_successful_identity() {
    let state = UaCatalogState::default();
    let transport = Arc::new(CatalogTransport::default());
    let service = Arc::new(
        UaCatalogService::with_transport(
            ProviderKind::new("openai").unwrap(),
            state.clone(),
            catalog_cache(),
            transport.clone(),
        )
        .unwrap(),
    );
    service.refresh().await.unwrap();
    let saved = state
        .latest(
            CatalogClient::Cli,
            "linux-ubuntu-x64",
            Some(CatalogProfile::Xterm),
        )
        .unwrap();
    tokio::time::advance(Duration::from_secs(61)).await;
    transport.block_cli.store(true, Ordering::SeqCst);
    let refresh_service = service.clone();
    let refresh = tokio::spawn(async move { refresh_service.refresh().await });
    transport.cli_blocked.notified().await;
    refresh.abort();
    assert!(refresh.await.unwrap_err().is_cancelled());
    let calls = transport.listings.load(Ordering::SeqCst);
    assert!(matches!(
        service.refresh().await,
        Err(UaCatalogError::Fetch)
    ));
    assert_eq!(transport.listings.load(Ordering::SeqCst), calls);
    assert_eq!(
        state.latest(
            CatalogClient::Cli,
            "linux-ubuntu-x64",
            Some(CatalogProfile::Xterm)
        ),
        Some(saved)
    );

    transport.block_cli.store(false, Ordering::SeqCst);
    *transport.cli.lock().unwrap() = vec![release("0.11.0", 5, "xterm")];
    tokio::time::advance(Duration::from_secs(61)).await;
    service.refresh().await.unwrap();
    assert_eq!(
        state
            .latest(
                CatalogClient::Cli,
                "linux-ubuntu-x64",
                Some(CatalogProfile::Xterm)
            )
            .unwrap()
            .release,
        "0.11.0"
    );
}

#[tokio::test(start_paused = true)]
async fn history_is_bounded_sorted_numerically_and_incomplete_or_older_sources_are_rejected() {
    let state = UaCatalogState::default();
    let transport = Arc::new(CatalogTransport::default());
    *transport.cli.lock().unwrap() = (1..=22)
        .map(|minor| release(&format!("0.{minor}.0"), minor, "xterm"))
        .collect();
    let service = UaCatalogService::with_transport(
        ProviderKind::new("openai").unwrap(),
        state.clone(),
        catalog_cache(),
        transport.clone(),
    )
    .unwrap();
    service.refresh().await.unwrap();
    let snapshot = state.snapshot();
    let cli: Vec<_> = snapshot["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["client"] == "cli")
        .collect();
    assert_eq!(cli.len(), 20);
    assert_eq!(cli.first().unwrap()["release"], "0.22.0");
    assert_eq!(cli.last().unwrap()["release"], "0.3.0");

    *transport.cli.lock().unwrap() = vec![release("0.21.0", 21, "xterm")];
    tokio::time::advance(Duration::from_secs(61)).await;
    assert!(service.refresh().await.is_err());
    assert_eq!(
        state
            .latest(
                CatalogClient::Cli,
                "linux-ubuntu-x64",
                Some(CatalogProfile::Xterm)
            )
            .unwrap()
            .release,
        "0.22.0"
    );
    transport.endless_pages.store(true, Ordering::SeqCst);
    tokio::time::advance(Duration::from_secs(61)).await;
    assert!(matches!(
        service.refresh().await,
        Err(UaCatalogError::Limit)
    ));
    assert_eq!(
        state
            .latest(
                CatalogClient::Cli,
                "linux-ubuntu-x64",
                Some(CatalogProfile::Xterm)
            )
            .unwrap()
            .release,
        "0.22.0"
    );
}

#[tokio::test]
async fn invalid_cached_source_cannot_replace_a_valid_offline_identity() {
    let state = UaCatalogState::default();
    let cache = catalog_cache();
    let service = UaCatalogService::with_transport(
        ProviderKind::new("openai").unwrap(),
        state.clone(),
        cache.clone(),
        Arc::new(CatalogTransport::default()),
    )
    .unwrap();
    service.refresh().await.unwrap();
    let previous = state
        .latest(
            CatalogClient::Cli,
            "linux-ubuntu-x64",
            Some(CatalogProfile::Xterm),
        )
        .unwrap();
    let key = ProviderCatalogCacheKey::new(
        ProviderKind::new("openai").unwrap(),
        ProviderCatalogScope::new("ua-release-catalog-v1-cli").unwrap(),
    );
    let mut corrupt = Value::Object(
        cache
            .read(&key)
            .await
            .unwrap()
            .unwrap()
            .expose_to_provider()
            .clone(),
    );
    corrupt["releases"][0]["entries"][0]["userAgent"] = json!("malformed");
    cache
        .replace(&key, &document(corrupt), Duration::from_secs(60))
        .await
        .unwrap();
    service.restore().await;
    assert_eq!(
        state
            .latest(
                CatalogClient::Cli,
                "linux-ubuntu-x64",
                Some(CatalogProfile::Xterm)
            )
            .unwrap(),
        previous
    );
    assert!(state.snapshot()["sources"][1]["error"].is_string());
}
