//! 单个宿主实例共享发行版本列表和通道详情，版本探测只读内存

use gateway_admin::model::system::{
    SystemUpdateChannel as Channel, SystemUpdateCheck as Check, SystemUpdateDetail,
};
use semver::Version;
use std::{collections::VecDeque, sync::Arc, time::Duration};
use tokio::{
    sync::{Mutex, RwLock},
    time::{Instant, timeout},
};

use super::diagnostics::Diagnostics;
use super::metadata::Metadata;
use super::release::{
    GitHubRelease, base_update_detail, detail_from_release, update_target_allowed,
};
use super::{OperationError, SystemUpdateConfig, conflict, upstream};

const NORMAL: Duration = Duration::from_secs(1200);
const REFRESH: Duration = Duration::from_secs(60);
const BUDGET: Duration = Duration::from_secs(20);

struct Entry<T> {
    at: Instant,
    result: Result<T, OperationError>,
}

struct Slot<T> {
    value: RwLock<Option<Entry<T>>>,
    fetching: Mutex<()>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self {
            value: RwLock::new(None),
            fetching: Mutex::new(()),
        }
    }
}

impl<T: Clone> Slot<T> {
    async fn cached(&self, age: Duration) -> Option<Result<T, OperationError>> {
        let value = self.value.read().await;
        let entry = value.as_ref()?;
        let ttl = match &entry.result {
            Err(error) if error.retry_at().is_some_and(|at| at <= chrono::Utc::now()) => {
                return None;
            }
            Err(_) => REFRESH,
            Ok(_) => age,
        };
        (entry.at.elapsed() < ttl).then(|| entry.result.clone())
    }

    async fn get(
        &self,
        age: Duration,
        fetch: impl Future<Output = Result<T, OperationError>>,
        diagnostics: &Diagnostics,
    ) -> Result<(T, bool), OperationError> {
        if let Some(result) = self.cached(age).await {
            return result.map(|value| (value, true));
        }
        let guard = self.fetching.lock().await;
        if let Some(result) = self.cached(age).await {
            return result.map(|value| (value, true));
        }
        let result = fetch.await;
        *self.value.write().await = Some(Entry {
            at: Instant::now(),
            result: result.clone(),
        });
        drop(guard);
        if let Err(error) = &result {
            diagnostics.record("check_release", None, error).await;
        }
        result.map(|value| (value, false))
    }
}

type ReleaseSlot = Arc<Slot<Option<Arc<GitHubRelease>>>>;

#[derive(Default)]
pub(super) struct ReleaseCache {
    metadata: Metadata,
    versions: Slot<Arc<[Version]>>,
    releases: Mutex<VecDeque<(Version, ReleaseSlot)>>,
}

impl ReleaseCache {
    pub(super) async fn local(
        &self,
        config: &SystemUpdateConfig,
        channel: Channel,
    ) -> SystemUpdateDetail {
        if let Some(reason) = config.update_support_error() {
            return base_update_detail(config, channel, Some(reason), None);
        }
        let Some(versions) = self.versions.cached(NORMAL).await else {
            return base_update_detail(config, channel, None, None);
        };
        let summary = versions.and_then(|versions| candidate(config, channel, &versions));
        match summary {
            Ok((mut detail, target)) => {
                if detail.has_update {
                    let slot = self
                        .releases
                        .lock()
                        .await
                        .iter()
                        .find(|(v, _)| v == &target)
                        .map(|(_, s)| Arc::clone(s));
                    let checked = match slot {
                        Some(slot) => slot.cached(NORMAL).await,
                        None => None,
                    };
                    match checked {
                        Some(Ok(Some(_))) => {}
                        Some(Err(error)) => {
                            return base_update_detail(config, channel, None, Some(&error));
                        }
                        _ => return base_update_detail(config, channel, None, None),
                    }
                }
                detail.cached = true;
                detail
            }
            Err(error) => base_update_detail(config, channel, None, Some(&error)),
        }
    }

    pub(super) async fn detail(
        &self,
        config: &SystemUpdateConfig,
        channel: Channel,
        check: Check,
        diagnostics: &Diagnostics,
    ) -> Result<SystemUpdateDetail, OperationError> {
        if let Some(reason) = config.update_support_error() {
            return Ok(base_update_detail(config, channel, Some(reason), None));
        }
        let age = if check == Check::Refresh {
            REFRESH
        } else {
            NORMAL
        };
        timeout(BUDGET, async {
            let (versions, cached) = self
                .versions
                .get(age, self.metadata.versions(config), diagnostics)
                .await?;
            let (mut detail, target) = candidate(config, channel, &versions)?;
            detail.cached = cached;
            if check != Check::Summary || detail.has_update {
                let (release, reused) = self.exact(config, &target, age, diagnostics).await?;
                if let Some(release) = release
                    && check != Check::Summary
                {
                    detail = detail_from_release(config, &release, channel);
                }
                detail.cached = cached && reused;
            }
            Ok(detail)
        })
        .await
        .map_err(|_| upstream("检查更新超时"))?
    }

    async fn exact(
        &self,
        config: &SystemUpdateConfig,
        target: &Version,
        age: Duration,
        diagnostics: &Diagnostics,
    ) -> Result<(Option<Arc<GitHubRelease>>, bool), OperationError> {
        // 四个普通通道和当前版本最多需要五份详情，相同 tag 跨通道共享
        let slot = {
            let mut entries = self.releases.lock().await;
            let slot = entries
                .iter()
                .position(|(tag, _)| tag == target)
                .and_then(|i| entries.remove(i))
                .map(|(_, slot)| slot)
                .unwrap_or_default();
            if entries.len() == 5 {
                entries.pop_front();
            }
            entries.push_back((target.clone(), Arc::clone(&slot)));
            slot
        };
        slot.get(age, self.metadata.release(config, target), diagnostics)
            .await
    }

    pub(super) async fn install(
        &self,
        config: &SystemUpdateConfig,
        channel: Channel,
        confirmed: &str,
        diagnostics: &Diagnostics,
    ) -> Result<Arc<GitHubRelease>, OperationError> {
        // 安装前重新选择候选并复核精确 Release
        timeout(BUDGET, async {
            let (versions, _) = self
                .versions
                .get(Duration::ZERO, self.metadata.versions(config), diagnostics)
                .await?;
            let (detail, target) = candidate(config, channel, &versions)?;
            if !detail.has_update || detail.latest_version != confirmed {
                return Err(conflict("远端最新版本已变化，请重新检查并确认"));
            }
            self.exact(config, &target, Duration::ZERO, diagnostics)
                .await?
                .0
                .ok_or_else(|| upstream("目标 Release 不存在"))
        })
        .await
        .map_err(|_| upstream("安装前版本复核超时"))?
    }
}

fn candidate(
    config: &SystemUpdateConfig,
    channel: Channel,
    versions: &[Version],
) -> Result<(SystemUpdateDetail, Version), OperationError> {
    let current = Version::parse(config.version.trim_start_matches('v'))
        .map_err(|_| conflict("当前版本号无效"))?;
    let mut detail = base_update_detail(config, channel, None, None);
    let Some(target) = versions
        .iter()
        .filter(|version| update_target_allowed(&current, version, channel))
        .max_by(|left, right| left.cmp_precedence(right))
    else {
        return Ok((detail, current));
    };
    if versions
        .iter()
        .any(|version| version != target && version.cmp_precedence(target).is_eq())
    {
        return Err(upstream("GitHub 有多个发行版本并列为当前通道的最新版本"));
    }
    detail.latest_version = target.to_string();
    detail.has_update = true;
    Ok((detail, target.clone()))
}
