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

/// Read-only update endpoints and the trusted release signing public key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateSettings {
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

impl UpdateSettings {
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
