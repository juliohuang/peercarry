//! Peer discovery through the `tailscale` CLI.
//!
//! `tailscale status --json` gives us every node in the tailnet with its
//! Tailscale IP, host name, OS and online flag. That is enough to build a
//! peer list without running a discovery service of our own.

use std::collections::HashMap;

use serde::Deserialize;

use crate::error::{Error, Result};
use crate::model::Peer;

#[derive(Debug, Clone, Deserialize)]
pub struct Node {
    #[serde(rename = "ID")]
    pub id: Option<String>,
    #[serde(rename = "HostName")]
    pub host_name: Option<String>,
    #[serde(rename = "DNSName")]
    pub dns_name: Option<String>,
    #[serde(rename = "OS")]
    pub os: Option<String>,
    #[serde(rename = "TailscaleIPs")]
    pub tailscale_ips: Option<Vec<String>>,
    #[serde(rename = "Online")]
    pub online: Option<bool>,
}

impl Node {
    /// First IPv4 Tailscale address (the `100.x.y.z` one).
    pub fn ipv4(&self) -> Option<&str> {
        self.tailscale_ips
            .as_ref()?
            .iter()
            .find(|ip| !ip.contains(':'))
            .map(|s| s.as_str())
    }
}

#[derive(Debug, Deserialize)]
pub struct Status {
    #[serde(rename = "Self")]
    pub this: Option<Node>,
    #[serde(rename = "Peer")]
    pub peers: Option<HashMap<String, Node>>,
}

/// Locate the `tailscale` binary, which is not always on `PATH`.
fn binary() -> String {
    if let Ok(custom) = std::env::var("PEERCARRY_TAILSCALE") {
        return custom;
    }
    for candidate in [
        "tailscale",
        "/usr/local/bin/tailscale",
        "/opt/homebrew/bin/tailscale",
        "/Applications/Tailscale.app/Contents/MacOS/tailscale",
        "C:\\Program Files\\Tailscale\\tailscale.exe",
    ] {
        if which_exists(candidate) {
            return candidate.to_string();
        }
    }
    "tailscale".to_string()
}

fn which_exists(candidate: &str) -> bool {
    if candidate.contains(std::path::MAIN_SEPARATOR) {
        return std::path::Path::new(candidate).exists();
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(candidate).exists()))
        .unwrap_or(false)
}

/// Run `tailscale status --json` and parse the result.
pub async fn status() -> Result<Status> {
    let program = binary();
    let mut command = tokio::process::Command::new(&program);
    // Background discovery must not flash a console when called by the tray.
    #[cfg(target_os = "windows")]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let out = command
        .args(["status", "--json"])
        .output()
        .await
        .map_err(|e| Error::Tailscale(format!("cannot execute {program}: {e}")))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Error::Tailscale(format!(
            "{program} status --json failed: {}",
            stderr.trim()
        )));
    }

    serde_json::from_slice(&out.stdout)
        .map_err(|e| Error::Tailscale(format!("cannot parse tailscale status: {e}")))
}

/// This machine's own tailnet node.
pub async fn self_node() -> Result<Option<Node>> {
    Ok(status().await?.this)
}

/// Our own Tailscale IPv4, used as the default bind address.
pub async fn self_ipv4() -> Result<Option<String>> {
    Ok(self_node()
        .await?
        .and_then(|n| n.ipv4().map(|s| s.to_string())))
}

/// Every desktop peer in the tailnet, with its service address resolved.
///
/// Only `macOS`, `windows` and `linux` nodes are returned: phones and tablets
/// cannot run the daemon, and probing them would just burn timeouts.
pub async fn peers(port: u16) -> Result<Vec<Peer>> {
    let status = status().await?;
    let mut peers: Vec<Peer> = Vec::new();

    for node in status.peers.unwrap_or_default().into_values() {
        let os = node.os.clone().unwrap_or_default();
        if !matches!(os.as_str(), "macOS" | "windows" | "linux") {
            continue;
        }
        let Some(ipv4) = node.ipv4().map(|s| s.to_string()) else {
            continue;
        };

        peers.push(Peer {
            node_id: node.id.clone().unwrap_or_default(),
            host: node.host_name.clone().unwrap_or_else(|| ipv4.clone()),
            dns_name: node
                .dns_name
                .clone()
                .unwrap_or_default()
                .trim_end_matches('.')
                .to_string(),
            os,
            addresses: node.tailscale_ips.clone().unwrap_or_default(),
            online: node.online.unwrap_or(false),
            addr: Some(format!("http://{ipv4}:{port}")),
        });
    }

    peers.sort_by(|a, b| a.host.cmp(&b.host));
    Ok(peers)
}

impl Peer {
    /// Service URL, preferring the address resolved during discovery.
    pub fn addr_with_port(&self, port: u16) -> Option<String> {
        if let Some(addr) = &self.addr {
            return Some(addr.clone());
        }
        let ipv4 = self.addresses.iter().find(|ip| !ip.contains(':'))?;
        Some(format!("http://{ipv4}:{port}"))
    }
}
