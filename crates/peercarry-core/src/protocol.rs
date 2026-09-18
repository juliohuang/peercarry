// Legacy x-syncclip-* wire headers are stable for compatibility with existing peers.
//! Wire protocol shared by the daemon and its clients.
//!
//! Everything is plain HTTP/1.1 over the Tailscale interface. Tailscale
//! already provides encryption and node authentication; `auth_token` adds an
//! optional second factor for shared tailnets.
//!
//! | route                                | purpose                        |
//! |--------------------------------------|--------------------------------|
//! | `GET  /v1/hello`                     | identity + liveness probe      |
//! | `GET  /v1/entries`                   | this node's history            |
//! | `POST /v1/entries`                   | accept a pushed entry          |
//! | `GET  /v1/entries/{id}`              | single entry                   |
//! | `DELETE /v1/entries/{id}`            | drop an entry                  |
//! | `GET  /v1/blobs/{hash}`              | image payload                  |
//! | `GET  /v1/entries/{id}/files/{idx}`  | file payload, streamed         |
//! | `GET  /v1/apps`                      | names of launchable apps       |
//!
//! File routes take an **entry id and index**, never a raw path, so a peer can
//! only read files that were explicitly captured into history.

use serde::{Deserialize, Serialize};

use crate::model::Entry;

pub const API_PREFIX: &str = "/v1";

/// Header carrying the shared secret.
pub const TOKEN_HEADER: &str = "x-syncclip-token";

/// Real file name of a streamed file payload.
pub const FILENAME_HEADER: &str = "x-syncclip-filename";

/// Set on a capture response when the clipboard was already in the history,
/// so nothing new was written.
pub const DEDUPED_HEADER: &str = "x-syncclip-deduped";

/// `1` when the streamed payload is a tar archive of a directory.
pub const IS_DIR_HEADER: &str = "x-syncclip-is-dir";

/// Advertised protocol version. Bumped on incompatible changes so mismatched
/// daemons can warn instead of silently misbehaving.
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloResponse {
    pub protocol: u32,
    pub host: String,
    pub node_id: String,
    pub os: String,
    pub version: String,
    /// Optional additive features; absent on older peers.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListQuery {
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub kind: Option<String>,
}

/// `GET /v1/timeline` - the aggregated, searchable history behind the
/// built-in timeline page and `peercarry search`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TimelineQuery {
    /// Substring to look for; matches against previews, inline text, file
    /// names and device names, case-insensitively.
    #[serde(default)]
    pub q: Option<String>,
    /// Entries fetched per machine before merging.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Cap on the merged result.
    #[serde(default)]
    pub max: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushResponse {
    pub accepted: usize,
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub ok: bool,
    pub entries: usize,
}

/// `POST /v1/actions/apply`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApplyRequest {
    /// Entry id to look up. Optional when `entry` is supplied.
    #[serde(default)]
    pub id: Option<String>,
    /// Pre-resolved entry, so the daemon does not have to search peers again.
    #[serde(default)]
    pub entry: Option<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    pub ok: bool,
    pub kind: String,
    pub detail: String,
}

/// `POST /v1/actions/send`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SendRequest {
    /// Entry id to send. When absent the clipboard is captured first.
    #[serde(default)]
    pub id: Option<String>,
    /// Target host name substring. When absent, send to every online peer.
    #[serde(default)]
    pub host: Option<String>,
}

/// `POST /v1/actions/capture-files`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureFilesRequest {
    /// Absolute paths on this machine, recorded without touching the
    /// clipboard. Loopback-only like the other actions: it lets the caller
    /// read local files into the shareable history.
    pub paths: Vec<String>,
}

/// `POST /v1/actions/launch`
///
/// The request carries only a registered app *name*: the target machine
/// resolves it against its own `[[apps]]` config. Paths and arguments are
/// never accepted from the network.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchRequest {
    pub name: String,
}

/// `POST /v1/actions/pin`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinRequest {
    /// Full entry id (the CLI resolves selectors before calling).
    pub id: String,
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestartResponse {
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigSaved {
    pub ok: bool,
    /// True when any field needs a service restart to take effect. Only the
    /// download folder applies immediately.
    pub restart_recommended: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendResult {
    pub id: String,
    /// `(peer host, accepted)` pairs.
    pub targets: Vec<(String, bool)>,
}

/// Turn a model entry into the query string used by `GET /v1/entries`.
pub fn list_url(base: &str, limit: Option<usize>) -> String {
    let base = base.trim_end_matches('/');
    match limit {
        Some(limit) => format!("{base}/v1/entries?limit={limit}"),
        None => format!("{base}/v1/entries"),
    }
}

/// Convenience for clients that already hold an [`Entry`].
pub fn entry_file_url(base: &str, entry_id: &str, index: usize) -> String {
    format!(
        "{}/v1/entries/{}/files/{}",
        base.trim_end_matches('/'),
        entry_id,
        index
    )
}
