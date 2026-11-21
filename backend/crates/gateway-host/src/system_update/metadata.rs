//! 分页读取公开发行版本，并按精确 tag 查询有界发行详情

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use reqwest::{Client, StatusCode, Url, header::LINK};
use semver::Version;
use serde::Deserialize;
use tokio::sync::{Mutex, OnceCell};
use tokio::time::Instant;

use super::release::GitHubRelease;
use super::{OperationError, SystemUpdateConfig, invalid, upstream};

const MAX_RELEASE_PAGES: usize = 8;
const MAX_METADATA_SIZE: usize = 4 * 1024 * 1024;

#[derive(Default)]
pub(super) struct Metadata {
    client: OnceCell<Client>,
    limited_until: Mutex<Option<Cooldown>>,
}

#[derive(Deserialize)]
struct ListedRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
}

struct MetadataResponse {
    bytes: Vec<u8>,
    has_next_page: bool,
}

#[derive(Clone, Copy)]
struct Cooldown {
    until: Instant,
    retry_at: DateTime<Utc>,
}

impl Cooldown {
    fn warning(self) -> OperationError {
        upstream("GitHub 暂时限制了更新检查").with_retry_at(self.retry_at)
    }
}

impl Metadata {
    pub(super) async fn versions(
        &self,
        config: &SystemUpdateConfig,
    ) -> Result<Arc<[Version]>, OperationError> {
        let base = releases_url(config)?;
        let mut versions = Vec::new();
        for page in 1..=MAX_RELEASE_PAGES {
            let mut url = base.clone();
            url.query_pairs_mut()
                .append_pair("per_page", "100")
                .append_pair("page", &page.to_string());
            let response = self
                .read(url)
                .await?
                .ok_or_else(|| upstream("找不到更新仓库的发行列表"))?;
            let releases: Vec<ListedRelease> = serde_json::from_slice(&response.bytes)
                .map_err(|error| upstream("GitHub 发行列表格式无效").with_source(error))?;
            versions.extend(releases.into_iter().filter_map(|release| {
                let version = Version::parse(release.tag_name.strip_prefix('v')?).ok()?;
                let prerelease = !version.pre.is_empty();
                (!release.draft && release.prerelease == prerelease).then_some(version)
            }));
            if !response.has_next_page {
                return Ok(versions.into());
            }
        }
        Err(upstream("GitHub 发行列表超出检查页数上限"))
    }

    pub(super) async fn release(
        &self,
        config: &SystemUpdateConfig,
        version: &Version,
    ) -> Result<Option<Arc<GitHubRelease>>, OperationError> {
        let mut url = releases_url(config)?;
        let prerelease = !version.pre.is_empty();
        let version = version.to_string();
        let tag = format!("v{version}");
        url.path_segments_mut()
            .map_err(|()| invalid("发行查询地址无效"))?
            .extend(["tags", &tag]);
        let Some(response) = self.read(url).await? else {
            // 当前运行版本可以没有公开说明；已选升级候选缺失属于短期查询失败
            if config.version.trim_start_matches('v') == version {
                return Ok(None);
            }
            return Err(upstream("目标 Release 不存在"));
        };
        let release: GitHubRelease = serde_json::from_slice(&response.bytes)
            .map_err(|error| upstream("GitHub 发行信息格式无效").with_source(error))?;
        if release.tag_name != tag || release.draft || release.prerelease != prerelease {
            return Err(upstream("GitHub 发行身份与目标版本不符"));
        }
        if config.version.trim_start_matches('v') != version {
            release.install_assets(&version, &config.github_api_base)?;
        }
        Ok(Some(Arc::new(release)))
    }

    async fn read(&self, url: Url) -> Result<Option<MetadataResponse>, OperationError> {
        if let Some(active) = self
            .limited_until
            .lock()
            .await
            .filter(|active| active.until > Instant::now())
        {
            return Err(active.warning());
        }
        let client = self
            .client
            .get_or_try_init(|| async {
                gateway_egress::http_client(None)
                    .map_err(|_| super::error::invalid("部署代理配置不合法"))?
                    .user_agent("codex-proxy-rs")
                    .timeout(Duration::from_secs(10))
                    .connect_timeout(Duration::from_secs(5))
                    .redirect(reqwest::redirect::Policy::none())
                    .retry(reqwest::retry::never())
                    .build()
                    .map_err(|e| super::error::http("无法初始化发行查询连接", e))
            })
            .await?;
        let mut response = client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .map_err(|e| super::error::http("无法查询 GitHub 发行版本", e))?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !status.is_success() {
            let headers = response.headers();
            let header = |name: &str| headers.get(name).and_then(|h| h.to_str().ok());
            let retry = header("retry-after").and_then(|s| s.parse::<u64>().ok());
            let exhausted = header("x-ratelimit-remaining") == Some("0");
            if status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::FORBIDDEN {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let reset = if exhausted {
                    header("x-ratelimit-reset")
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(|v| v.saturating_sub(now))
                } else {
                    None
                };
                let seconds = retry
                    .into_iter()
                    .chain(reset)
                    .max()
                    .unwrap_or(60)
                    .clamp(1, 86400);
                let duration = Duration::from_secs(seconds);
                let limit = Cooldown {
                    until: Instant::now() + duration,
                    retry_at: Utc::now() + duration,
                };
                let mut guard = self.limited_until.lock().await;
                let active = guard.filter(|old| old.until > limit.until).unwrap_or(limit);
                *guard = Some(active);
                let warning = active.warning();
                return Err(match response.error_for_status_ref() {
                    Err(error) => warning.with_source(error.without_url()),
                    Ok(_) => warning,
                });
            }
            return Err(upstream(format!(
                "GitHub 发行查询失败（HTTP {}）",
                status.as_u16()
            )));
        }
        let too_large = || upstream("GitHub 发行元数据超出大小限制");
        if response
            .content_length()
            .is_some_and(|size| size > MAX_METADATA_SIZE as u64)
        {
            return Err(too_large());
        }
        // 只读取是否还有下一页，后续请求继续使用固定仓库地址
        let has_next_page = response.headers().get_all(LINK).iter().any(|header| {
            header.to_str().is_ok_and(|header| {
                header.split(',').any(|link| {
                    link.split(';')
                        .skip(1)
                        .any(|parameter| parameter.trim() == r#"rel="next""#)
                })
            })
        });
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| super::error::http("读取 GitHub 发行元数据失败", e))?
        {
            if chunk.len() > MAX_METADATA_SIZE - bytes.len() {
                return Err(too_large());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Some(MetadataResponse {
            bytes,
            has_next_page,
        }))
    }
}

fn releases_url(config: &SystemUpdateConfig) -> Result<Url, OperationError> {
    let repository = config
        .update_repository
        .as_deref()
        .ok_or_else(|| invalid("未配置更新仓库"))?;
    let mut url = Url::parse(&config.github_api_base).map_err(|_| invalid("发行查询地址无效"))?;
    url.path_segments_mut()
        .map_err(|()| invalid("发行查询地址无效"))?
        .pop_if_empty()
        .extend(repository.split('/'))
        .push("releases");
    Ok(url)
}
