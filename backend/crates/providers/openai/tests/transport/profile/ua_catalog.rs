use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use gateway_core::account::OpaqueProviderData;
use gateway_core::provider_ports::{ProviderCatalogCacheKey, ProviderCatalogScope};
use gateway_core::routing::ProviderKind;
use provider_openai::transport::profile::identity::{CatalogClient, RequestProfileSelection};
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
    fail_cli: AtomicBool,
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
            fail_cli: AtomicBool::new(false),
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
                cli_matrix(&release.version, &release.terminal)
            };
            Ok(serde_json::to_vec(&value).unwrap())
        })
    }
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
        .latest(CatalogClient::Cli, "linux-ubuntu-x64")
        .unwrap();
    assert_eq!(entry.release, "0.10.0");
    let fixed = document(json!({"mode":"catalog","versionMode":"fixed","entry":entry}));
    let follow = document(json!({"mode":"catalog","versionMode":"latest","entry":entry}));
    let custom = document(json!({"mode":"custom","userAgent":entry.user_agent}));
    let frozen = RequestProfileSelection::parse(&follow)
        .unwrap()
        .resolve(&state)
        .unwrap();

    // 同一 release 的资产可以纠正完整 UA；固定与自定义仍保留保存时的快照。
    *transport.cli.lock().unwrap() = vec![release("0.10.0", 4, "screen-256color")];
    tokio::time::advance(Duration::from_secs(61)).await;
    service.refresh().await.unwrap();
    assert_eq!(transport.downloads.load(Ordering::SeqCst), 4);
    let corrected = state
        .catalog()
        .latest(CatalogClient::Cli, "linux-ubuntu-x64")
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
            .latest(CatalogClient::Cli, "linux-ubuntu-x64")
            .unwrap()
            .user_agent,
        latest.user_agent()
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
            .latest(CatalogClient::Cli, "linux-ubuntu-x64")
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
            .latest(CatalogClient::Cli, "linux-ubuntu-x64")
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
        .latest(CatalogClient::Cli, "linux-ubuntu-x64")
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
            .latest(CatalogClient::Cli, "linux-ubuntu-x64")
            .unwrap(),
        previous
    );
    assert!(state.snapshot()["sources"][1]["error"].is_string());
}
