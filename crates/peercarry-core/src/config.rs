//! Configuration loading and defaults.
//!
//! The file lives at `<data_dir>/config.toml` and is created with defaults on
//! first run. Every section is optional in the file, so a partial config is
//! valid.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::paths;

pub const DEFAULT_PORT: u16 = 5199;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub node: NodeConfig,
    pub network: NetworkConfig,
    pub limits: LimitsConfig,
    pub storage: StorageConfig,
    /// Explicitly authorized mobile upload clients. Disabled by default.
    pub mobile: MobileConfig,
    pub updates: UpdateSettings,
    /// Shared secret only for AI task metadata. Empty disables remote feeds.
    #[serde(default)]
    pub ai_token: Option<String>,
    /// Apps this machine offers for launching, by peers or locally.
    pub apps: Vec<AppEntry>,
}

/// One launchable application registered on this machine.
///
/// Peers refer to it by `name` only: the path and arguments never travel in a
/// launch request, so a peer cannot execute anything outside this list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppEntry {
    /// Short identifier. No colons or whitespace - it travels inside menu
    /// ids and CLI selectors.
    pub name: String,
    /// Executable path on this machine.
    pub path: PathBuf,
    /// Fixed arguments, always applied from this config; peers cannot supply
    /// any.
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MobileConfig {
    pub enabled: bool,
    pub devices: Vec<MobileDevice>,
    pub max_storage_bytes: u64,
    pub max_sessions: usize,
    pub session_ttl_secs: u64,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct MobileDevice {
    pub id: String,
    pub token: String,
}

/// Update scheduling, endpoints, and the trusted release signing public key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "UpdateSettingsInput")]
pub struct UpdateSettings {
    pub auto_check: bool,
    pub check_interval_hours: u64,
    pub source: String,
    pub domestic_url: String,
    pub github_url: String,
    pub custom_url: String,
    pub public_key: String,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        let domestic = option_env!("PEERCARRY_UPDATE_DOMESTIC_URL").unwrap_or("");
        Self {
            auto_check: true,
            check_interval_hours: 6,
            source: if domestic.is_empty() {
                "github"
            } else {
                "domestic"
            }
            .into(),
            domestic_url: domestic.into(),
            github_url: option_env!("PEERCARRY_UPDATE_GITHUB_URL")
                .unwrap_or("")
                .into(),
            custom_url: String::new(),
            public_key: option_env!("PEERCARRY_UPDATE_PUBLIC_KEY")
                .unwrap_or("")
                .into(),
        }
    }
}

// Keep missing trust material empty until the selected source is known. In
// particular, a custom URL must never inherit the official signing key.
#[derive(Deserialize, Default)]
#[serde(default)]
struct UpdateSettingsInput {
    auto_check: Option<bool>,
    check_interval_hours: Option<u64>,
    source: Option<String>,
    domestic_url: Option<String>,
    github_url: String,
    custom_url: String,
    public_key: String,
}

impl From<UpdateSettingsInput> for UpdateSettings {
    fn from(input: UpdateSettingsInput) -> Self {
        let defaults = Self::default();
        let mut settings = Self {
            auto_check: input.auto_check.unwrap_or(defaults.auto_check),
            check_interval_hours: input
                .check_interval_hours
                .unwrap_or(defaults.check_interval_hours),
            source: input.source.unwrap_or(defaults.source.clone()),
            domestic_url: input.domestic_url.unwrap_or(defaults.domestic_url.clone()),
            github_url: input.github_url,
            custom_url: input.custom_url,
            public_key: input.public_key,
        };
        settings.restore_official_defaults(&defaults);
        settings
    }
}

impl UpdateSettings {
    /// Bound the polling interval even for hand-edited configuration files.
    pub fn check_interval_hours(&self) -> u64 {
        self.check_interval_hours.clamp(1, 168)
    }

    fn restore_official_defaults(&mut self, defaults: &Self) {
        if self.source == "github"
            && !defaults.github_url.is_empty()
            && (self.github_url.is_empty() || self.github_url == defaults.github_url)
        {
            if self.github_url.is_empty() {
                self.github_url.clone_from(&defaults.github_url);
            }
            if self.public_key.is_empty() {
                self.public_key.clone_from(&defaults.public_key);
            }
        }
    }

    pub fn manifest_url(&self) -> std::result::Result<&str, String> {
        let url = match self.source.as_str() {
            "domestic" => &self.domestic_url,
            "github" => &self.github_url,
            "custom" => &self.custom_url,
            _ => return Err("unknown update source".into()),
        };
        if url.is_empty() || self.public_key.is_empty() {
            return Err("configure an update URL and trusted public key in Settings / 请先配置更新地址和可信公钥".into());
        }
        Ok(url)
    }
}

impl std::fmt::Debug for MobileDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MobileDevice")
            .field("id", &self.id)
            .field("token", &"[redacted]")
            .finish()
    }
}

impl Default for MobileConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            devices: Vec::new(),
            max_storage_bytes: 8 * 1024 * 1024 * 1024,
            max_sessions: 128,
            session_ttl_secs: 24 * 60 * 60,
        }
    }
}

impl MobileConfig {
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.max_storage_bytes == 0 || self.max_sessions == 0 || self.session_ttl_secs == 0 {
            return Err("mobile upload limits must be positive".into());
        }
        let mut ids = std::collections::HashSet::new();
        for device in &self.devices {
            if device.id.is_empty()
                || device.id.len() > 64
                || !device
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
                || !ids.insert(device.id.as_str())
            {
                return Err(
                    "mobile device IDs must be unique, 1..64 ASCII letters, digits, '-' or '_'"
                        .into(),
                );
            }
            if device.token.len() < 32
                || device.token.len() > 256
                || !device.token.bytes().all(|c| c.is_ascii_graphic())
            {
                return Err("mobile device tokens must contain 32..256 printable non-space ASCII characters".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeConfig {
    /// Display name shown to other peers. Defaults to the system host name.
    pub name: Option<String>,
    /// Participate in peer discovery. When false the node is invisible but can
    /// still be contacted directly.
    pub discoverable: bool,
    /// Let other nodes write to this machine's clipboard through
    /// `POST /v1/actions/apply`. Off by default - a peer silently overwriting
    /// your clipboard is exactly the conflict peercarry exists to avoid.
    pub allow_remote_apply: bool,
    /// Permit peers to explicitly capture the current clipboard into history.
    pub allow_remote_capture: bool,
    /// Let other nodes launch apps registered in `[[apps]]` on this machine
    /// through `POST /v1/actions/launch`. Off by default: this is remote
    /// execution, however narrowly channeled - only registered names, never
    /// paths or arguments from the peer.
    pub allow_remote_launch: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    /// Port the HTTP service listens on, and the port assumed for peers.
    pub port: u16,
    /// Force a bind address. `None` binds to the Tailscale IP when available,
    /// otherwise to `0.0.0.0`.
    pub bind: Option<String>,
    /// Shared secret. When set, every request must carry it. Tailscale already
    /// encrypts and authenticates the transport, so this is a second layer for
    /// shared tailnets.
    pub auth_token: Option<String>,
    pub request_timeout_secs: u64,
    pub connect_timeout_secs: u64,
    /// Maximum idle time between chunks of a large transfer.
    pub transfer_idle_timeout_secs: u64,
    /// Maximum time to wait for a transfer response to begin.
    pub transfer_prepare_timeout_secs: u64,
    /// Probe all online peers in parallel when building the aggregated list.
    pub discovery_concurrency: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LimitsConfig {
    /// Images smaller than this travel inline with the entry metadata.
    /// Anything larger is stored locally and streamed on demand.
    pub inline_image_bytes: u64,
    /// Text smaller than this travels inline.
    pub inline_text_bytes: u64,
    /// Refuse to transfer a single payload larger than this.
    pub max_transfer_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Entries kept per node. Oldest unpinned entries are dropped first.
    pub history_limit: usize,
    /// Longest edge of the preview generated for image entries. Thumbnails
    /// are tiny PNGs kept alongside the payload so menus and future UIs can
    /// show what an image is without downloading it.
    pub thumbnail_px: u32,
    /// Capturing something already in the history replaces the stale entry
    /// with the fresh capture instead of adding a copy. Pinned entries are
    /// never replaced.
    pub dedupe: bool,
    pub download_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            node: NodeConfig::default(),
            network: NetworkConfig::default(),
            limits: LimitsConfig::default(),
            storage: StorageConfig::default(),
            mobile: MobileConfig::default(),
            updates: UpdateSettings::default(),
            ai_token: None,
            apps: Vec::new(),
        }
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            name: None,
            discoverable: true,
            allow_remote_apply: false,
            allow_remote_capture: false,
            allow_remote_launch: false,
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            bind: None,
            auth_token: None,
            request_timeout_secs: 30,
            connect_timeout_secs: 3,
            transfer_idle_timeout_secs: 60,
            transfer_prepare_timeout_secs: 600,
            discovery_concurrency: 16,
        }
    }
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            inline_image_bytes: 1024 * 1024,            // 1 MiB
            inline_text_bytes: 256 * 1024,              // 256 KiB
            max_transfer_bytes: 2 * 1024 * 1024 * 1024, // 2 GiB
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            history_limit: 500,
            thumbnail_px: 128,
            dedupe: true,
            download_dir: None,
            data_dir: None,
        }
    }
}

impl Config {
    /// Load from `<data_dir>/config.toml`, creating it with defaults if absent.
    pub fn load() -> anyhow::Result<Self> {
        let path = paths::config_path();
        if !path.exists() {
            let cfg = Config::default();
            cfg.save()?;
            return Ok(cfg);
        }
        let raw = std::fs::read_to_string(&path)?;
        let cfg: Config = toml::from_str(&raw)
            .map_err(|e| anyhow::anyhow!("invalid config at {}: {e}", path.display()))?;
        cfg.mobile.validate().map_err(anyhow::Error::msg)?;
        Ok(cfg)
    }

    /// Render the configuration as it would be written to disk.
    pub fn to_toml(&self) -> anyhow::Result<String> {
        Ok(toml::to_string_pretty(self)?)
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = paths::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, self.to_toml()?)?;
        Ok(())
    }

    /// Directory receiving pulled files.
    pub fn download_dir(&self) -> PathBuf {
        self.storage
            .download_dir
            .clone()
            .unwrap_or_else(paths::download_dir)
    }

    /// Resolved display name for this node.
    pub fn display_name(&self) -> String {
        self.node
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| {
                hostname::get()
                    .map(|h| h.to_string_lossy().to_string())
                    .unwrap_or_else(|_| "unknown".to_string())
            })
    }

    /// Registered app by name, case-insensitively.
    pub fn find_app(&self, name: &str) -> Option<&AppEntry> {
        let needle = name.trim().to_lowercase();
        self.apps.iter().find(|a| a.name.to_lowercase() == needle)
    }
}

/// Print a commented template to stdout, used by `sc config --print`.
pub fn template() -> String {
    let cfg = Config::default();
    let body = toml::to_string_pretty(&cfg).unwrap_or_default();
    format!(
        "# peercarry configuration\n\
         # Location: {}\n\
         # Every value below is the built-in default; delete a line to keep it.\n\n\
         {}",
        paths::config_path().display(),
        body
    )
}

#[cfg(test)]
mod mobile_tests {
    use super::*;

    #[test]
    fn old_config_disables_mobile_uploads() {
        let config: Config = toml::from_str("[node]\ndiscoverable = true").unwrap();
        assert!(!config.mobile.enabled);
        assert!(config.mobile.devices.is_empty());
        config.mobile.validate().unwrap();
    }

    #[test]
    fn device_credentials_validate_and_debug_redacts_tokens() {
        let device = MobileDevice {
            id: "phone_1".into(),
            token: "a".repeat(32),
        };
        assert!(!format!("{device:?}").contains(&device.token));
        let mut config = MobileConfig {
            devices: vec![device.clone()],
            ..Default::default()
        };
        config.validate().unwrap();
        config.devices.push(device);
        assert!(config.validate().is_err());
        config.devices.pop();
        config.devices[0].token = "short".into();
        assert!(config.validate().is_err());
    }
}

#[cfg(test)]
mod update_tests {
    use super::*;

    #[test]
    fn old_configs_enable_six_hour_checks_and_respect_opt_out() {
        let config: Config = toml::from_str("[updates]\nsource = 'github'").unwrap();
        assert!(config.updates.auto_check);
        assert_eq!(config.updates.check_interval_hours(), 6);
        let settings: UpdateSettings =
            toml::from_str("auto_check = false\ncheck_interval_hours = 0").unwrap();
        assert!(!settings.auto_check);
        assert_eq!(settings.check_interval_hours(), 1);
        let settings: UpdateSettings = toml::from_str("check_interval_hours = 999").unwrap();
        assert_eq!(settings.check_interval_hours(), 168);
    }

    #[test]
    fn legacy_empty_official_values_use_built_in_defaults() {
        let settings: UpdateSettings =
            toml::from_str("source = 'github'\ngithub_url = ''\npublic_key = ''").unwrap();
        let defaults = UpdateSettings::default();
        assert_eq!(settings.github_url, defaults.github_url);
        assert_eq!(
            settings.public_key,
            if defaults.github_url.is_empty() {
                ""
            } else {
                &defaults.public_key
            }
        );
    }

    #[test]
    fn official_migration_preserves_explicit_keys_and_unrelated_sources() {
        let defaults = UpdateSettings {
            github_url: "https://official.example/manifest.json".into(),
            public_key: "official-key".into(),
            ..Default::default()
        };
        for url in ["", defaults.github_url.as_str()] {
            let mut settings = UpdateSettings {
                source: "github".into(),
                github_url: url.into(),
                public_key: String::new(),
                ..Default::default()
            };
            settings.restore_official_defaults(&defaults);
            assert_eq!(settings.github_url, defaults.github_url);
            assert_eq!(settings.public_key, defaults.public_key);
            settings.public_key = "user-key".into();
            settings.restore_official_defaults(&defaults);
            assert_eq!(settings.public_key, "user-key");
        }
        for (source, url) in [
            ("custom", ""),
            ("domestic", ""),
            ("github", "https://other.example/manifest.json"),
        ] {
            let mut settings = UpdateSettings {
                source: source.into(),
                github_url: url.into(),
                public_key: String::new(),
                ..Default::default()
            };
            settings.restore_official_defaults(&defaults);
            assert!(settings.public_key.is_empty());
            assert_eq!(settings.github_url, url);
        }
    }

    #[test]
    fn non_official_sources_never_inherit_missing_public_keys() {
        for raw in [
            "source = 'custom'",
            "source = 'domestic'",
            "source = 'github'\ngithub_url = 'https://other.example/manifest.json'",
        ] {
            let settings: UpdateSettings = toml::from_str(raw).unwrap();
            assert!(settings.public_key.is_empty());
        }
        let settings: UpdateSettings = serde_json::from_str(r#"{"source":"github","github_url":"https://other.example/manifest.json","public_key":"user-key","auto_check":false}"#).unwrap();
        assert_eq!(settings.public_key, "user-key");
        assert!(!settings.auto_check);
    }
}
