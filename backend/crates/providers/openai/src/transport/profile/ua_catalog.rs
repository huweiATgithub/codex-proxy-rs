//! 用户发布的完整 UA 目录；与官方制品核验和账号 Desktop 身份分别管理。

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::{StreamExt as _, TryStreamExt as _, future::BoxFuture};
use gateway_core::account::OpaqueProviderData;
use gateway_core::provider_ports::{
    ProviderCatalogCacheKey, ProviderCatalogCachePort, ProviderCatalogScope,
};
use gateway_core::routing::ProviderKind;
use reqwest::{Client, Url, redirect::Policy};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::time::Instant;

use super::CodexWireProfileState;
use super::identity::{CatalogClient, CatalogIdentity, UaCatalogEntry};

const RELEASE_LIMIT: usize = 20;
const PAGE_SIZE: usize = 100;
const PAGE_LIMIT: usize = 10;
const MATRIX_MAX_BYTES: usize = 64 * 1024;
const METADATA_MAX_BYTES: usize = 2 * 1024 * 1024;
const CACHE_MAX_BYTES: usize = 1024 * 1024;
const CACHE_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const REFRESH_COOLDOWN: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UaCatalogSource {
    Desktop,
    Cli,
}

impl UaCatalogSource {
    const ALL: [Self; 2] = [Self::Desktop, Self::Cli];

    const fn index(self) -> usize {
        match self {
            Self::Desktop => 0,
            Self::Cli => 1,
        }
    }

    const fn repo(self) -> &'static str {
        match self {
            Self::Desktop => "codex-desktop-ua",
            Self::Cli => "codex-ua",
        }
    }

    const fn cache_scope(self) -> &'static str {
        match self {
            Self::Desktop => "ua-release-catalog-v1-desktop",
            Self::Cli => "ua-release-catalog-v1-cli",
        }
    }

    fn accepts(self, client: CatalogClient) -> bool {
        matches!(
            (self, client),
            (Self::Desktop, CatalogClient::Desktop)
                | (Self::Cli, CatalogClient::Cli | CatalogClient::Exec)
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct UaCatalogState(Arc<RwLock<[SourceState; 2]>>);

#[derive(Debug, Clone, Default)]
struct SourceState {
    catalog: Option<Arc<CachedCatalog>>,
    checked_at: Option<DateTime<Utc>>,
    error: Option<String>,
}

impl UaCatalogState {
    #[must_use]
    pub fn latest(&self, client: CatalogClient, environment: &str) -> Option<UaCatalogEntry> {
        let source = if client == CatalogClient::Desktop {
            UaCatalogSource::Desktop
        } else {
            UaCatalogSource::Cli
        };
        self.current(source)?.releases.iter().find_map(|release| {
            release
                .entries
                .iter()
                .find(|entry| entry.client == client && entry.environment == environment)
                .cloned()
        })
    }

    #[must_use]
    pub fn snapshot(&self) -> serde_json::Value {
        let state = self
            .0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entries: Vec<_> = state
            .iter()
            .filter_map(|source| source.catalog.as_ref())
            .flat_map(|catalog| catalog.releases.iter())
            .flat_map(|release| &release.entries)
            .collect();
        let sources: Vec<_> = UaCatalogSource::ALL
            .iter()
            .map(|source| {
                let current = &state[source.index()];
                serde_json::json!({
                    "source": source,
                    "checkedAt": current.checked_at,
                    "updatedAt": current.catalog.as_ref().map(|catalog| catalog.updated_at),
                    "error": current.error,
                })
            })
            .collect();
        serde_json::json!({"entries": entries, "sources": sources, "releaseLimit": RELEASE_LIMIT})
    }

    fn current(&self, source: UaCatalogSource) -> Option<Arc<CachedCatalog>> {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[source.index()]
        .catalog
        .clone()
    }

    fn record(
        &self,
        source: UaCatalogSource,
        result: Result<CachedCatalog, UaCatalogError>,
        checked_at: DateTime<Utc>,
    ) {
        let mut state = self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = &mut state[source.index()];
        state.checked_at = Some(checked_at);
        match result {
            Ok(catalog) => {
                state.catalog = Some(Arc::new(catalog));
                state.error = None;
            }
            Err(error) => state.error = Some(error.to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CachedCatalog {
    schema_version: u8,
    source: UaCatalogSource,
    updated_at: DateTime<Utc>,
    releases: Vec<CachedRelease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CachedRelease {
    release: String,
    asset_id: u64,
    asset_updated_at: DateTime<Utc>,
    entries: Vec<UaCatalogEntry>,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    id: u64,
    name: String,
    updated_at: DateTime<Utc>,
    size: usize,
    state: String,
}

struct ReleaseAsset {
    tag: String,
    version: semver::Version,
    asset: GithubAsset,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DesktopMatrix {
    schema_version: u8,
    desktop_version: String,
    platforms: BTreeMap<String, DesktopPlatform>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DesktopPlatform {
    codex_version: String,
    #[serde(rename = "Desktop")]
    desktop: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CliMatrix {
    schema_version: u8,
    codex_version: String,
    platforms: BTreeMap<String, CliPlatform>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CliPlatform {
    #[serde(rename = "CLI")]
    cli: String,
    #[serde(rename = "Exec")]
    exec: String,
}

/// 在目录边界同时核对发布版本、环境与完整请求身份。
pub fn parse_ua_matrix(
    source: UaCatalogSource,
    release: &str,
    bytes: &[u8],
) -> Result<Vec<UaCatalogEntry>, UaCatalogError> {
    stable_version(release)?;
    if bytes.len() > MATRIX_MAX_BYTES {
        return Err(UaCatalogError::Invalid);
    }
    let mut entries = Vec::new();
    match source {
        UaCatalogSource::Desktop => {
            let matrix: DesktopMatrix =
                serde_json::from_slice(bytes).map_err(|_| UaCatalogError::Invalid)?;
            if matrix.schema_version != 1 || matrix.desktop_version != release {
                return Err(UaCatalogError::Invalid);
            }
            for (environment, platform) in matrix.platforms {
                let entry = UaCatalogEntry {
                    client: CatalogClient::Desktop,
                    environment,
                    release: release.to_owned(),
                    user_agent: platform.desktop,
                };
                validate_entry(&entry, &platform.codex_version)?;
                entries.push(entry);
            }
        }
        UaCatalogSource::Cli => {
            let matrix: CliMatrix =
                serde_json::from_slice(bytes).map_err(|_| UaCatalogError::Invalid)?;
            if matrix.schema_version != 1 || matrix.codex_version != release {
                return Err(UaCatalogError::Invalid);
            }
            for (environment, platform) in matrix.platforms {
                for (client, user_agent) in [
                    (CatalogClient::Cli, platform.cli),
                    (CatalogClient::Exec, platform.exec),
                ] {
                    let entry = UaCatalogEntry {
                        client,
                        environment: environment.clone(),
                        release: release.to_owned(),
                        user_agent,
                    };
                    validate_entry(&entry, release)?;
                    entries.push(entry);
                }
            }
        }
    }
    if entries.is_empty() || entries.len() > 64 {
        return Err(UaCatalogError::Invalid);
    }
    Ok(entries)
}

fn validate_entry(entry: &UaCatalogEntry, core: &str) -> Result<(), UaCatalogError> {
    let identity = CatalogIdentity::parse(entry).map_err(|_| UaCatalogError::Invalid)?;
    if identity.codex_version() != core {
        return Err(UaCatalogError::Invalid);
    }
    Ok(())
}

fn stable_version(value: &str) -> Result<semver::Version, UaCatalogError> {
    let version = semver::Version::parse(value).map_err(|_| UaCatalogError::Invalid)?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        return Err(UaCatalogError::Invalid);
    }
    Ok(version)
}

/// 仅提供固定发布源的元数据和 compact 资产，缓存与解析归 service 所有。
pub trait UaCatalogTransport: Send + Sync {
    fn releases(
        &self,
        source: UaCatalogSource,
        page: usize,
    ) -> BoxFuture<'_, Result<Vec<u8>, UaCatalogError>>;
    fn matrix<'a>(
        &'a self,
        source: UaCatalogSource,
        tag: &'a str,
    ) -> BoxFuture<'a, Result<Vec<u8>, UaCatalogError>>;
}

struct GithubUaCatalogTransport(Client);

impl GithubUaCatalogTransport {
    fn new() -> Result<Self, UaCatalogError> {
        let client = Client::builder()
            .https_only(true)
            .no_proxy()
            .user_agent("codex-proxy-rs-ua-catalog")
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 || !allowed_url(attempt.url()) {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(|_| UaCatalogError::Fetch)?;
        Ok(Self(client))
    }

    async fn download(&self, url: &str, maximum: usize) -> Result<Vec<u8>, UaCatalogError> {
        let url = Url::parse(url).map_err(|_| UaCatalogError::Fetch)?;
        if !allowed_url(&url) {
            return Err(UaCatalogError::Fetch);
        }
        let response = self
            .0
            .get(url)
            .send()
            .await
            .map_err(|_| UaCatalogError::Fetch)?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|size| size > maximum as u64)
        {
            return Err(UaCatalogError::Fetch);
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| UaCatalogError::Fetch)?;
            if bytes.len().saturating_add(chunk.len()) > maximum {
                return Err(UaCatalogError::Invalid);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

fn allowed_url(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && matches!(
            url.host_str(),
            Some(
                "api.github.com"
                    | "github.com"
                    | "release-assets.githubusercontent.com"
                    | "objects.githubusercontent.com"
            )
        )
}

impl UaCatalogTransport for GithubUaCatalogTransport {
    fn releases(
        &self,
        source: UaCatalogSource,
        page: usize,
    ) -> BoxFuture<'_, Result<Vec<u8>, UaCatalogError>> {
        Box::pin(async move {
            self.download(&format!("https://api.github.com/repos/huweiATgithub/{}/releases?per_page={PAGE_SIZE}&page={page}", source.repo()), METADATA_MAX_BYTES).await
        })
    }

    fn matrix<'a>(
        &'a self,
        source: UaCatalogSource,
        tag: &'a str,
    ) -> BoxFuture<'a, Result<Vec<u8>, UaCatalogError>> {
        Box::pin(async move {
            // tag 已由稳定三元版本解析，不使用元数据中的任意下载地址。
            self.download(
                &format!(
                    "https://github.com/huweiATgithub/{}/releases/download/{tag}/ua-matrix.json",
                    source.repo()
                ),
                MATRIX_MAX_BYTES,
            )
            .await
        })
    }
}

pub struct UaCatalogService {
    state: UaCatalogState,
    cache: Arc<dyn ProviderCatalogCachePort>,
    keys: [ProviderCatalogCacheKey; 2],
    transport: Arc<dyn UaCatalogTransport>,
    refresh: Mutex<Option<(Instant, Result<(), UaCatalogError>)>>,
}

impl UaCatalogService {
    pub fn new(
        provider: ProviderKind,
        state: CodexWireProfileState,
        cache: Arc<dyn ProviderCatalogCachePort>,
    ) -> Result<Self, UaCatalogError> {
        Self::with_transport(
            provider,
            state.catalog().clone(),
            cache,
            Arc::new(GithubUaCatalogTransport::new()?),
        )
    }

    pub fn with_transport(
        provider: ProviderKind,
        state: UaCatalogState,
        cache: Arc<dyn ProviderCatalogCachePort>,
        transport: Arc<dyn UaCatalogTransport>,
    ) -> Result<Self, UaCatalogError> {
        let key = |source: UaCatalogSource| {
            ProviderCatalogScope::new(source.cache_scope())
                .map(|scope| ProviderCatalogCacheKey::new(provider.clone(), scope))
                .map_err(|_| UaCatalogError::Cache)
        };
        Ok(Self {
            state,
            cache,
            keys: [key(UaCatalogSource::Desktop)?, key(UaCatalogSource::Cli)?],
            transport,
            refresh: Mutex::new(None),
        })
    }

    pub async fn restore(&self) {
        for source in UaCatalogSource::ALL {
            let result = match self.cache.read(&self.keys[source.index()]).await {
                Ok(Some(value)) => decode_cache(source, value),
                Ok(None) => continue,
                Err(_) => Err(UaCatalogError::Cache),
            };
            let checked_at = result
                .as_ref()
                .map_or_else(|_| Utc::now(), |catalog| catalog.updated_at);
            self.state.record(source, result, checked_at);
        }
    }

    pub async fn refresh(&self) -> Result<(), UaCatalogError> {
        let mut refresh = self.refresh.lock().await;
        if let Some((at, result)) = *refresh
            && at.elapsed() < REFRESH_COOLDOWN
        {
            return result;
        }
        *refresh = Some((Instant::now(), Err(UaCatalogError::Fetch)));
        let (desktop, cli) = tokio::join!(
            self.refresh_source(UaCatalogSource::Desktop),
            self.refresh_source(UaCatalogSource::Cli)
        );
        let result = desktop.and(cli);
        *refresh = Some((Instant::now(), result));
        result
    }

    async fn refresh_source(&self, source: UaCatalogSource) -> Result<(), UaCatalogError> {
        let result = async {
            let catalog = tokio::time::timeout(Duration::from_secs(120), self.fetch_source(source))
                .await
                .map_err(|_| UaCatalogError::Fetch)??;
            let value = serde_json::to_value(&catalog).map_err(|_| UaCatalogError::Cache)?;
            if serde_json::to_vec(&value)
                .map_err(|_| UaCatalogError::Cache)?
                .len()
                > CACHE_MAX_BYTES
            {
                return Err(UaCatalogError::Invalid);
            }
            let serde_json::Value::Object(fields) = value else {
                return Err(UaCatalogError::Cache);
            };
            self.cache
                .replace(
                    &self.keys[source.index()],
                    &OpaqueProviderData::new(fields),
                    CACHE_TTL,
                )
                .await
                .map_err(|_| UaCatalogError::Cache)?;
            Ok(catalog)
        }
        .await;
        let status = result.as_ref().map(|_| ()).map_err(|error| *error);
        self.state.record(source, result, Utc::now());
        status
    }

    async fn fetch_source(&self, source: UaCatalogSource) -> Result<CachedCatalog, UaCatalogError> {
        let assets = self.release_assets(source).await?;
        let previous = self.state.current(source);
        let releases = futures::stream::iter(assets.into_iter().map(|asset| {
            let previous = previous.as_ref();
            async move {
                let release = asset.version.to_string();
                if let Some(cached) = previous.and_then(|catalog| {
                    catalog.releases.iter().find(|cached| {
                        cached.release == release
                            && cached.asset_id == asset.asset.id
                            && cached.asset_updated_at == asset.asset.updated_at
                    })
                }) {
                    return Ok(cached.clone());
                }
                let bytes = self.transport.matrix(source, &asset.tag).await?;
                let entries = parse_ua_matrix(source, &release, &bytes)?;
                Ok::<_, UaCatalogError>(CachedRelease {
                    release,
                    asset_id: asset.asset.id,
                    asset_updated_at: asset.asset.updated_at,
                    entries,
                })
            }
        }))
        .buffered(4)
        .try_collect::<Vec<_>>()
        .await?;
        if let Some(previous) = previous.as_ref() {
            for entry in previous
                .releases
                .iter()
                .flat_map(|release| &release.entries)
            {
                let latest = releases
                    .iter()
                    .flat_map(|release| &release.entries)
                    .find(|next| {
                        next.client == entry.client && next.environment == entry.environment
                    })
                    .ok_or(UaCatalogError::Invalid)?;
                if stable_version(&latest.release)? < stable_version(&entry.release)? {
                    return Err(UaCatalogError::Invalid);
                }
            }
        }
        Ok(CachedCatalog {
            schema_version: 1,
            source,
            updated_at: Utc::now(),
            releases,
        })
    }

    async fn release_assets(
        &self,
        source: UaCatalogSource,
    ) -> Result<Vec<ReleaseAsset>, UaCatalogError> {
        let mut assets = Vec::new();
        let mut versions = BTreeSet::new();
        for page in 1..=PAGE_LIMIT {
            let bytes = self.transport.releases(source, page).await?;
            if bytes.len() > METADATA_MAX_BYTES {
                return Err(UaCatalogError::Invalid);
            }
            let releases: Vec<GithubRelease> =
                serde_json::from_slice(&bytes).map_err(|_| UaCatalogError::Invalid)?;
            let complete = releases.len() < PAGE_SIZE;
            if releases.len() > PAGE_SIZE || (!complete && page == PAGE_LIMIT) {
                return Err(UaCatalogError::Limit);
            }
            for release in releases {
                if release.draft || release.prerelease {
                    continue;
                }
                let raw_version = release
                    .tag_name
                    .strip_prefix('v')
                    .unwrap_or(&release.tag_name);
                let Ok(version) = stable_version(raw_version) else {
                    continue;
                };
                let mut matching = release
                    .assets
                    .into_iter()
                    .filter(|asset| asset.name == "ua-matrix.json" && asset.state == "uploaded");
                let Some(asset) = matching.next() else {
                    continue;
                };
                if matching.next().is_some()
                    || asset.size > MATRIX_MAX_BYTES
                    || asset.id == 0
                    || !versions.insert(version.clone())
                {
                    return Err(UaCatalogError::Invalid);
                }
                assets.push(ReleaseAsset {
                    tag: release.tag_name,
                    version,
                    asset,
                });
            }
            if complete {
                break;
            }
        }
        if assets.is_empty() {
            return Err(UaCatalogError::Invalid);
        }
        assets.sort_by(|left, right| right.version.cmp(&left.version));
        assets.truncate(RELEASE_LIMIT);
        Ok(assets)
    }
}

fn decode_cache(
    source: UaCatalogSource,
    value: OpaqueProviderData,
) -> Result<CachedCatalog, UaCatalogError> {
    let bytes =
        serde_json::to_vec(value.expose_to_provider()).map_err(|_| UaCatalogError::Cache)?;
    if bytes.len() > CACHE_MAX_BYTES {
        return Err(UaCatalogError::Invalid);
    }
    let catalog: CachedCatalog =
        serde_json::from_slice(&bytes).map_err(|_| UaCatalogError::Invalid)?;
    if catalog.schema_version != 1
        || catalog.source != source
        || catalog.releases.is_empty()
        || catalog.releases.len() > RELEASE_LIMIT
    {
        return Err(UaCatalogError::Invalid);
    }
    let mut previous = None;
    for release in &catalog.releases {
        let version = stable_version(&release.release)?;
        if previous
            .as_ref()
            .is_some_and(|previous| previous <= &version)
            || release.entries.is_empty()
            || release.entries.len() > 64
        {
            return Err(UaCatalogError::Invalid);
        }
        previous = Some(version);
        let mut identities = BTreeSet::new();
        for entry in &release.entries {
            if entry.release != release.release
                || !source.accepts(entry.client)
                || !identities.insert((entry.client, &entry.environment))
            {
                return Err(UaCatalogError::Invalid);
            }
            CatalogIdentity::parse(entry).map_err(|_| UaCatalogError::Invalid)?;
        }
    }
    Ok(catalog)
}

#[derive(Debug, Clone, Copy, thiserror::Error)]
pub enum UaCatalogError {
    #[error("UA 发布列表检查失败，请稍后重试")]
    Fetch,
    #[error("UA 发布资料格式、版本或环境不匹配")]
    Invalid,
    #[error("UA 发布列表超过可检查范围，保留上次目录")]
    Limit,
    #[error("UA 发布目录缓存读写失败")]
    Cache,
}
