//! 完整 UA 的解析边界；发布目录与自定义身份共享配套请求头语义。

use chrono::DateTime;
use gateway_core::account::OpaqueProviderData;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::selection::{
    ClientKind, ClientProfileError, ClientProfileSelection, VersionMode, object,
};
use super::{CodexWireProfile, CodexWireProfileState};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CatalogClient {
    Desktop,
    Cli,
    Exec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UaCatalogEntry {
    pub client: CatalogClient,
    pub environment: String,
    pub release: String,
    pub user_agent: String,
}

/// 已解析的配置；没有 mode 的历史配置继续使用原有版本与平台合同。
#[derive(Debug, Clone)]
pub struct RequestProfileSelection(ParsedSelection);

#[derive(Debug, Clone)]
enum ParsedSelection {
    Legacy(ClientProfileSelection),
    Catalog {
        version_mode: VersionMode,
        identity: CatalogIdentity,
    },
    Custom(ExactIdentity),
}

#[derive(Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum SelectionDocument {
    Catalog {
        #[serde(rename = "versionMode")]
        version_mode: VersionMode,
        entry: UaCatalogEntry,
    },
    Custom {
        #[serde(rename = "userAgent")]
        user_agent: String,
        originator: Option<String>,
        #[serde(rename = "codexVersion")]
        codex_version: Option<String>,
    },
}

impl RequestProfileSelection {
    pub fn legacy(&self) -> Option<&ClientProfileSelection> {
        match &self.0 {
            ParsedSelection::Legacy(selection) => Some(selection),
            _ => None,
        }
    }

    pub fn version_mode(&self) -> VersionMode {
        match &self.0 {
            ParsedSelection::Legacy(selection) => selection.version_mode,
            ParsedSelection::Catalog { version_mode, .. } => *version_mode,
            ParsedSelection::Custom(_) => VersionMode::Fixed,
        }
    }

    pub fn parse(document: &OpaqueProviderData) -> Result<Self, ClientProfileError> {
        let fields = document.expose_to_provider();
        if !fields.contains_key("mode") {
            return ClientProfileSelection::parse(document)
                .map(ParsedSelection::Legacy)
                .map(Self);
        }
        let document = serde_json::from_value(Value::Object(fields.clone()))
            .map_err(|_| ClientProfileError::Invalid)?;
        let selection = match document {
            SelectionDocument::Catalog {
                version_mode,
                entry,
            } => ParsedSelection::Catalog {
                version_mode,
                identity: CatalogIdentity::parse(&entry)?,
            },
            SelectionDocument::Custom {
                user_agent,
                originator,
                codex_version,
            } => ParsedSelection::Custom(ExactIdentity::parse(
                &user_agent,
                originator.as_deref(),
                codex_version.as_deref(),
            )?),
        };
        Ok(Self(selection))
    }

    pub fn resolve(
        &self,
        state: &CodexWireProfileState,
    ) -> Result<CodexWireProfile, ClientProfileError> {
        match &self.0 {
            ParsedSelection::Legacy(selection) => selection.resolve(state),
            ParsedSelection::Catalog {
                version_mode,
                identity,
            } => Ok(identity
                .follow(*version_mode, state)?
                .identity
                .profile(state)),
            ParsedSelection::Custom(identity) => Ok(identity.profile(state)),
        }
    }

    pub(super) fn preview(
        &self,
        state: &CodexWireProfileState,
    ) -> Result<OpaqueProviderData, ClientProfileError> {
        match &self.0 {
            ParsedSelection::Legacy(selection) => state.preview_legacy_selection(selection),
            ParsedSelection::Catalog { version_mode, identity } => {
                let identity = identity.follow(*version_mode, state)?;
                let source = if identity.entry.client == CatalogClient::Desktop { "desktop" } else { "cli" };
                let catalog = state.catalog().snapshot();
                let status = catalog["sources"].as_array().and_then(|sources| {
                    sources.iter().find(|status| status["source"] == source)
                });
                identity.identity.preview(
                    state,
                    json!({"mode":"catalog", "versionMode":version_mode, "entry":identity.entry}),
                    "catalog",
                    status.and_then(|status| status.get("checkedAt")).cloned().unwrap_or(Value::Null),
                    status.and_then(|status| status.get("error")).cloned().unwrap_or(Value::Null),
                )
            }
            ParsedSelection::Custom(identity) => identity.preview(
                state,
                json!({"mode":"custom", "userAgent":identity.user_agent, "originator":identity.originator, "codexVersion":identity.codex_version}),
                "custom",
                Value::Null,
                Value::Null,
            ),
        }
    }
}

/// 已核对客户端、环境和发布版本的目录身份；不把目录来源误作官方制品核验。
#[derive(Debug, Clone)]
pub struct CatalogIdentity {
    entry: UaCatalogEntry,
    release: semver::Version,
    identity: ExactIdentity,
}

impl CatalogIdentity {
    pub fn parse(entry: &UaCatalogEntry) -> Result<Self, ClientProfileError> {
        let release = parse_core_version(&entry.release)?;
        if !release.pre.is_empty() || !release.build.is_empty() {
            return Err(ClientProfileError::Invalid);
        }
        let identity = ExactIdentity::parse(&entry.user_agent, None, None)?;
        let expected_originator = match entry.client {
            CatalogClient::Desktop => "Codex Desktop",
            CatalogClient::Cli => "codex-tui",
            CatalogClient::Exec => "codex_exec",
        };
        let environment = identity
            .environment
            .as_ref()
            .ok_or(ClientProfileError::Invalid)?;
        if identity.originator != expected_originator
            || !environment.matches(&entry.environment)
            || !entry
                .user_agent
                .ends_with(&format!(" ({expected_originator}; {})", entry.release))
            || (entry.client != CatalogClient::Desktop && identity.codex_version != entry.release)
        {
            return Err(ClientProfileError::Invalid);
        }
        Ok(Self {
            entry: entry.clone(),
            release,
            identity,
        })
    }

    pub fn codex_version(&self) -> &str {
        &self.identity.codex_version
    }

    fn follow(
        &self,
        version_mode: VersionMode,
        state: &CodexWireProfileState,
    ) -> Result<Self, ClientProfileError> {
        if version_mode == VersionMode::Latest
            && let Some(entry) = state
                .catalog()
                .latest(self.entry.client, &self.entry.environment)
        {
            let candidate = Self::parse(&entry)?;
            if candidate.release >= self.release {
                return Ok(candidate);
            }
        }
        // 配置携带最后保存的完整条目，冷启动、目录缺项或回退都不能改变环境。
        Ok(self.clone())
    }
}

#[derive(Debug, Clone)]
struct ExactIdentity {
    user_agent: String,
    originator: String,
    codex_version: String,
    recognized: bool,
    environment: Option<UaEnvironment>,
    desktop_version: Option<String>,
}

impl ExactIdentity {
    fn parse(
        user_agent: &str,
        originator: Option<&str>,
        codex_version: Option<&str>,
    ) -> Result<Self, ClientProfileError> {
        if !safe_header(user_agent, 4096) {
            return Err(ClientProfileError::InvalidUserAgent);
        }
        let known = ["Codex Desktop", "codex-tui", "codex_exec", "codex_cli_rs"]
            .into_iter()
            .find_map(|name| {
                user_agent
                    .strip_prefix(name)
                    .and_then(|rest| rest.strip_prefix('/'))
                    .map(|rest| (name, rest))
            });
        let (originator, codex_version, recognized) = if let Some((name, rest)) = known {
            let version = rest.split_once(' ').map_or(rest, |(version, _)| version);
            parse_core_version(version)?;
            if originator.is_some_and(|value| value != name)
                || codex_version.is_some_and(|value| value != version)
            {
                return Err(ClientProfileError::CompanionHeadersConflict);
            }
            (name.to_owned(), version.to_owned(), true)
        } else {
            let originator = originator
                .filter(|value| safe_header(value, 128))
                .ok_or(ClientProfileError::CompanionHeadersRequired)?;
            let version = codex_version.ok_or(ClientProfileError::CompanionHeadersRequired)?;
            parse_core_version(version)?;
            (originator.to_owned(), version.to_owned(), false)
        };
        let environment = known.and_then(|(_, rest)| UaEnvironment::parse(rest));
        let desktop_version = (originator == "Codex Desktop")
            .then(|| {
                user_agent
                    .rsplit_once(" (Codex Desktop; ")
                    .and_then(|(_, version)| version.strip_suffix(')'))
                    .filter(|version| super::numeric_dotted_version(version))
                    .map(ToOwned::to_owned)
            })
            .flatten();
        Ok(Self {
            user_agent: user_agent.to_owned(),
            originator,
            codex_version,
            recognized,
            environment,
            desktop_version,
        })
    }

    fn profile(&self, state: &CodexWireProfileState) -> CodexWireProfile {
        let environment = self.environment.as_ref();
        CodexWireProfile {
            client_kind: if self.originator == "Codex Desktop" {
                ClientKind::Desktop
            } else {
                ClientKind::Cli
            },
            originator: self.originator.clone(),
            codex_version: self.codex_version.clone(),
            desktop_version: self.desktop_version.clone().unwrap_or_default(),
            desktop_build: String::new(),
            os_type: environment
                .map(|value| value.os_type.clone())
                .unwrap_or_default(),
            os_version: environment
                .map(|value| value.os_version.clone())
                .unwrap_or_default(),
            arch: environment
                .map(|value| value.arch.clone())
                .unwrap_or_default(),
            terminal: environment
                .map(|value| value.terminal.clone())
                .unwrap_or_default(),
            exact_user_agent: Some(self.user_agent.clone()),
            residency: state.snapshot().residency,
            verified_at: DateTime::UNIX_EPOCH,
        }
    }

    fn preview(
        &self,
        state: &CodexWireProfileState,
        configuration: Value,
        version_source: &str,
        checked_at: Value,
        error: Value,
    ) -> Result<OpaqueProviderData, ClientProfileError> {
        let profile = self.profile(state);
        object(&json!({
            "configuration": configuration,
            "originator": profile.originator,
            "osType": profile.os_type,
            "osVersion": profile.os_version,
            "arch": profile.arch,
            "terminal": profile.terminal,
            "codexVersion": profile.codex_version,
            "desktopVersion": self.desktop_version,
            "desktopBuild": null,
            "userAgent": profile.user_agent(),
            "recognized": self.recognized,
            "versionSource": version_source,
            "verifiedAt": null,
            "checkedAt": checked_at,
            "error": error,
        }))
    }
}

#[derive(Debug, Clone)]
struct UaEnvironment {
    os_type: String,
    os_version: String,
    arch: String,
    terminal: String,
}

impl UaEnvironment {
    fn parse(rest: &str) -> Option<Self> {
        let (_, rest) = rest.split_once(" (")?;
        let (environment, tail) = rest.split_once(") ")?;
        let (os, arch) = environment.split_once("; ")?;
        let (os_type, os_version) = os.rsplit_once(' ')?;
        let terminal = tail.split_once(" (").map_or(tail, |(terminal, _)| terminal);
        if os_type.is_empty() || os_version.is_empty() || arch.is_empty() || terminal.is_empty() {
            return None;
        }
        Some(Self {
            os_type: os_type.to_owned(),
            os_version: os_version.to_owned(),
            arch: arch.to_owned(),
            terminal: terminal.to_owned(),
        })
    }

    fn matches(&self, environment: &str) -> bool {
        let Some((os, arch)) = environment.rsplit_once('-') else {
            return false;
        };
        let os_type = match os {
            "macos" => "Mac OS",
            "windows" => "Windows",
            "linux-ubuntu" => "Ubuntu",
            "linux-debian" => "Debian",
            "linux-fedora" => "Fedora",
            "linux-alpine" => "Alpine Linux",
            _ => return false,
        };
        self.os_type == os_type
            && match arch {
                "x64" => self.arch == "x86_64",
                "arm64" => matches!(self.arch.as_str(), "arm64" | "aarch64"),
                _ => false,
            }
    }
}

fn safe_header(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && value.bytes().all(|byte| (32..=126).contains(&byte))
}

fn parse_core_version(value: &str) -> Result<semver::Version, ClientProfileError> {
    if value.len() > 64 {
        return Err(ClientProfileError::Invalid);
    }
    semver::Version::parse(value).map_err(|_| ClientProfileError::Invalid)
}
