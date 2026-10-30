//! HTTP service exposed to the tailnet.
//!
//! The daemon binds to its Tailscale address by default, so the service is
//! reachable from the tailnet but never from the local LAN or the internet.

use std::ffi::OsString;
use std::future::{Future, IntoFuture};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Path as AxumPath, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::Stream;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt, SeekFrom};
use tokio::net::TcpListener;
use tokio_util::io::ReaderStream;

use crate::config::Config;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::model::{Entry, Origin};
use crate::paths;
use crate::protocol;
use crate::store::Store;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub config: Arc<Config>,
    /// Identity stamped onto entries captured by this node.
    pub origin: Origin,
    /// Drives clipboard capture/apply. Kept in the daemon so all clipboard
    /// access happens in one long-lived process.
    pub engine: Arc<Engine>,
}

/// Stream wrapper that deletes a temporary file once the body is drained.
///
/// Used for directory payloads, which are tarred to a temp file before being
/// streamed so memory usage stays flat regardless of directory size.
struct CleanupStream<S> {
    inner: S,
    path: Option<PathBuf>,
}

impl<S> Stream for CleanupStream<S>
where
    S: Stream + Unpin,
{
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let result = Pin::new(&mut this.inner).poll_next(cx);
        if matches!(result, Poll::Ready(None)) {
            if let Some(path) = this.path.take() {
                tracing::debug!("removing temp archive {}", path.display());
                let _ = std::fs::remove_file(&path);
            }
        }
        result
    }
}

/// Canonical OS label, matching the casing used by `tailscale status`.
pub fn os_label() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macOS",
        "windows" => "windows",
        "linux" => "linux",
        other => other,
    }
}

/// Pick the bind address: explicit config, then Tailscale, then loopback.
pub async fn bind_addr(config: &Config) -> SocketAddr {
    if let Some(bind) = &config.network.bind {
        if let Ok(addr) = bind.parse::<SocketAddr>() {
            return addr;
        }
        if let Ok(ip) = bind.parse::<IpAddr>() {
            return SocketAddr::new(ip, config.network.port);
        }
        tracing::warn!("ignoring invalid bind address {bind:?}");
    }

    match crate::tailscale::self_ipv4().await {
        Ok(Some(ip)) => {
            if let Ok(parsed) = ip.parse::<IpAddr>() {
                return SocketAddr::new(parsed, config.network.port);
            }
        }
        Ok(None) => tracing::warn!("no Tailscale IPv4 found"),
        Err(e) => tracing::warn!("tailscale lookup failed: {e}"),
    }

    tracing::info!(
        "no Tailscale address; using loopback (mobile pairing uses its own TLS service)"
    );
    SocketAddr::from(([127, 0, 0, 1], config.network.port))
}

/// Identity this node stamps onto the entries it captures.
pub async fn build_origin(config: &Config, bind: SocketAddr) -> Origin {
    let node = crate::tailscale::self_node().await.ok().flatten();
    Origin {
        host: config.display_name(),
        node_id: node.and_then(|n| n.id).unwrap_or_default(),
        addr: format!("http://{bind}"),
        os: os_label().to_string(),
    }
}

/// Reject requests that do not carry the configured shared secret.
async fn auth_middleware(State(state): State<AppState>, request: Request, next: Next) -> Response {
    // The desktop UI runs as the local OS user. Grant only its explicit routes
    // a same-origin loopback path; never embed the peer token in HTML/storage.
    if trusted_desktop_request(&request) {
        return next.run(request).await;
    }
    let Some(expected) = state.config.network.auth_token.as_deref() else {
        return next.run(request).await;
    };
    if expected.is_empty() {
        return next.run(request).await;
    }

    let provided = request
        .headers()
        .get(protocol::TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            request
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
        });

    if provided == Some(expected) {
        next.run(request).await
    } else {
        tracing::warn!("rejected unauthorized request from a peer");
        StatusCode::UNAUTHORIZED.into_response()
    }
}

pub(crate) fn trusted_desktop_request(request: &Request) -> bool {
    let path = request.uri().path();
    let notebook_path = path == "/v1/notebook" || path.starts_with("/v1/notebook/");
    let desktop_path = matches!(
        path,
        "/" | "/quick"
            | "/assets/ui-locale.js"
            | "/assets/transfer-center.js"
            | "/assets/notebook.js"
            | "/assets/notebook.css"
            | "/assets/desktop-theme.css"
            | "/v1/timeline"
            | "/v1/peers"
            | "/v1/actions/apply"
            | "/v1/actions/pin"
            | "/v1/actions/fetch-clipboard"
            | "/v1/desktop/settings"
            | "/v1/desktop/settings/restart"
            | "/v1/desktop/language"
            | "/v1/ai/overview"
            | "/v1/ai/receiver"
            | "/v1/ai/tools"
            | "/v1/ai/bind"
            | "/v1/mobile/status"
            | "/v1/mobile/pairing"
            | "/v1/mobile/stop"
    ) || notebook_path
        || path
            .strip_prefix("/v1/mobile/devices/")
            .is_some_and(|id| !id.is_empty() && !id.contains('/'))
        || path == "/v1/transfers"
        || path.starts_with("/v1/transfers/");
    if !desktop_path || !is_loopback_request(request) {
        return false;
    }
    let Some(host) = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
    else {
        return false;
    };
    let local_host = host.parse::<axum::http::uri::Authority>().is_ok_and(|a| {
        a.host() == "localhost"
            || a.host()
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    let same_origin = request
        .headers()
        .get(header::ORIGIN)
        .is_none_or(|o| o.to_str().ok() == Some(format!("http://{host}").as_str()));
    let same_site = request
        .headers()
        .get("sec-fetch-site")
        .is_none_or(|s| s == "same-origin" || s == "none");
    // A user may follow the saved loopback homepage link after changing its
    // port or bind address. Permit only activated, top-level GET navigation
    // to static HTML here; every API and embedded frame keeps its own gate.
    let activated_home_navigation = path == "/"
        && request.method() == axum::http::Method::GET
        && request.headers().get_all(header::HOST).iter().count() == 1
        && !request.headers().contains_key(header::ORIGIN)
        && request
            .headers()
            .get("sec-fetch-mode")
            .is_some_and(|v| v == "navigate")
        && request
            .headers()
            .get("sec-fetch-dest")
            .is_some_and(|v| v == "document")
        && request
            .headers()
            .get("sec-fetch-user")
            .is_some_and(|v| v == "?1")
        && request
            .headers()
            .get("sec-fetch-site")
            .is_some_and(|v| v == "same-site" || v == "cross-site");
    local_host && same_origin && (same_site || activated_home_navigation)
}

pub(crate) async fn hello(State(state): State<AppState>) -> impl IntoResponse {
    Json(protocol::HelloResponse {
        protocol: protocol::PROTOCOL_VERSION,
        host: state.origin.host.clone(),
        node_id: state.origin.node_id.clone(),
        os: state.origin.os.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        capabilities: if state.config.mobile.enabled {
            vec!["mobile-upload-v1".into()]
        } else {
            Vec::new()
        },
    })
}

/// Names of the apps this machine offers for launching.
///
/// Only names travel: paths and arguments stay in the local config.
async fn list_apps(State(state): State<AppState>) -> impl IntoResponse {
    let mut names: Vec<String> = state.config.apps.iter().map(|a| a.name.clone()).collect();
    names.sort();
    Json(names)
}

async fn list_entries(
    State(state): State<AppState>,
    Query(query): Query<protocol::ListQuery>,
) -> impl IntoResponse {
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let kind = match query.kind.as_deref() {
        Some("text") => Some(crate::model::EntryKind::Text),
        Some("image") => Some(crate::model::EntryKind::Image),
        Some("files") => Some(crate::model::EntryKind::Files),
        _ => None,
    };

    match state.store.list(limit, kind) {
        Ok(entries) => Json(entries).into_response(),
        Err(e) => {
            tracing::error!("list failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

/// The built-in timeline page: search box over every machine's history.
/// Clipboard writes stay loopback-gated, so the page has full power only
/// in the browser of the machine running the daemon.
async fn timeline_ui() -> Html<&'static str> {
    Html(include_str!("../timeline.html"))
}

async fn quick_ui() -> Html<&'static str> {
    Html(include_str!("../quick.html"))
}

async fn desktop_ui_style() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../desktop-theme.css"),
    )
}

async fn locale_ui_script() -> impl IntoResponse {
    (
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "no-store"),
        ],
        format!(
            "window.__PEERCARRY_LOCALE__ = {};\n{}\n{}",
            serde_json::to_string(&crate::locale::current_details())
                .expect("language metadata is serializable"),
            include_str!("../ui-locale.js"),
            include_str!("../device-preferences.js")
        ),
    )
}

async fn desktop_language() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Json(crate::locale::current_details()),
    )
}

async fn transfer_ui_script() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        include_str!("../transfer-center.js"),
    )
}

/// Current configuration for the settings page. Loopback only: the file on
/// disk is readable by the local user anyway, `auth_token` included.
async fn get_config(State(state): State<AppState>) -> impl IntoResponse {
    Json(state.config.as_ref().clone())
}

/// Empty optional values mean "unset" everywhere in the TOML file; a text
/// field cleared in the settings page arrives as "".
fn normalize_config(config: &mut Config) {
    fn trim(value: &mut Option<String>) {
        *value = value
            .take()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
    }
    trim(&mut config.node.name);
    trim(&mut config.network.bind);
    trim(&mut config.network.auth_token);
    if config
        .storage
        .download_dir
        .as_ref()
        .is_some_and(|d| d.as_os_str().is_empty())
    {
        config.storage.download_dir = None;
    }
    if config
        .storage
        .data_dir
        .as_ref()
        .is_some_and(|d| d.as_os_str().is_empty())
    {
        config.storage.data_dir = None;
    }
    for app in &mut config.apps {
        app.name = app.name.trim().to_string();
    }
}

pub(crate) fn validate_config(config: &Config) -> std::result::Result<(), String> {
    if config
        .ai_token
        .as_deref()
        .is_some_and(|t| !t.is_empty() && t.len() < 32)
    {
        return Err("AI shared token must contain at least 32 characters".into());
    }
    config.mobile.validate()?;
    if config.network.port == 0 {
        return Err("port must not be 0".into());
    }
    if config.limits.inline_image_bytes == 0
        || config.limits.inline_text_bytes == 0
        || config.limits.max_transfer_bytes == 0
    {
        return Err("byte limits must be positive".into());
    }
    if config.storage.history_limit == 0 {
        return Err("history limit must be positive".into());
    }
    if config.storage.thumbnail_px == 0 {
        return Err("thumbnail size must be positive".into());
    }
    let mut seen = std::collections::HashSet::new();
    for app in &config.apps {
        if app.name.is_empty() {
            return Err("app name must not be empty".into());
        }
        if app.path.as_os_str().is_empty() {
            return Err(format!("app '{}' needs a path", app.name));
        }
        if !seen.insert(app.name.to_lowercase()) {
            return Err(format!("duplicate app name: {}", app.name));
        }
    }
    Ok(())
}

/// Save the settings page's edits to config.toml. The download folder also
/// applies to the running service; everything else needs a restart because
/// it is read once at startup.
async fn put_config(State(state): State<AppState>, Json(mut config): Json<Config>) -> Response {
    normalize_config(&mut config);
    if let Err(e) = validate_config(&config) {
        return (StatusCode::BAD_REQUEST, e).into_response();
    }
    if let Err(e) = config.save() {
        tracing::error!("config save failed: {e}");
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    match &config.storage.download_dir {
        Some(dir) => state.engine.set_download_dir(dir.clone()),
        None => state.engine.clear_download_dir(),
    }
    tracing::info!("configuration updated from the settings page");
    Json(protocol::ConfigSaved {
        ok: true,
        restart_recommended: true,
    })
    .into_response()
}

/// `POST /v1/actions/restart` - hand the service over to a fresh process so
/// startup-time config takes effect. Preserve the launch mode and identify
/// this process so the successor waits for all of its handles to close.
pub(crate) async fn action_restart() -> Response {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("cannot resolve exe: {e}"),
            )
                .into_response()
        }
    };
    let Some(parent) = RestartParent::current() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "cannot identify restart parent",
        )
            .into_response();
    };
    static PENDING: AtomicBool = AtomicBool::new(false);
    if PENDING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error":"restart_pending", "message":"Restart already requested."
            })),
        )
            .into_response();
    }
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let mut cmd = restart_command(&exe, &arguments, parent);
    match cmd.spawn() {
        Ok(_) => {
            // Give the response time to reach the browser before the
            // listener and the store lock go away.
            tokio::spawn(async {
                tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                std::process::exit(0);
            });
            Json(protocol::RestartResponse { ok: true }).into_response()
        }
        Err(e) => {
            PENDING.store(false, Ordering::Release);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("spawn failed: {e}"),
            )
                .into_response()
        }
    }
}

const RESTART_PARENT_PID: &str = "PEERCARRY_RESTART_PARENT_PID";
const RESTART_PARENT_START: &str = "PEERCARRY_RESTART_PARENT_START";

#[derive(Clone, Copy)]
struct RestartParent {
    pid: u32,
    started_at: u64,
}

impl RestartParent {
    fn current() -> Option<Self> {
        let pid = std::process::id();
        let mut system = sysinfo::System::new();
        let process_id = sysinfo::Pid::from_u32(pid);
        system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[process_id]),
            true,
            sysinfo::ProcessRefreshKind::nothing(),
        );
        let started_at = system.process(process_id)?.start_time();
        (started_at > 0).then_some(Self { pid, started_at })
    }

    fn parse(pid: Option<&str>, started_at: Option<&str>) -> Result<Option<Self>> {
        match (pid, started_at) {
            (None, None) => Ok(None),
            (Some(pid), Some(started_at)) => {
                let pid = pid.parse::<u32>().ok().filter(|value| *value > 0);
                let started_at = started_at.parse::<u64>().ok().filter(|value| *value > 0);
                match (pid, started_at) {
                    (Some(pid), Some(started_at)) => Ok(Some(Self { pid, started_at })),
                    _ => Err(Error::Network("invalid restart handoff".into())),
                }
            }
            _ => Err(Error::Network("incomplete restart handoff".into())),
        }
    }
}

fn restart_command(
    executable: &Path,
    arguments: &[OsString],
    parent: RestartParent,
) -> std::process::Command {
    let mut command = std::process::Command::new(executable);
    command
        .args(arguments)
        .env("PEERCARRY_RESTART", "1")
        .env(RESTART_PARENT_PID, parent.pid.to_string())
        .env(RESTART_PARENT_START, parent.started_at.to_string());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command
}

/// Aggregated, optionally filtered history - the searchable timeline behind
/// the built-in page and `peercarry search`.
async fn timeline(
    State(state): State<AppState>,
    Query(query): Query<protocol::TimelineQuery>,
) -> impl IntoResponse {
    let per_peer = query.limit.unwrap_or(200).clamp(1, 500);
    let max = query.max.unwrap_or(1000).clamp(1, 5000);
    match state.engine.list_all(per_peer).await {
        Ok(entries) => {
            let mut entries: Vec<Entry> = entries
                .into_iter()
                .filter(|e| match query.q.as_deref() {
                    Some(q) => e.matches(q),
                    None => true,
                })
                .take(max)
                .collect();
            // `list_all` sorts when it merges, but keep the invariant local
            // so the endpoint does not depend on that detail.
            entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
            Json(entries).into_response()
        }
        Err(e) => {
            tracing::error!("timeline failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

pub(crate) async fn push_entry(
    State(state): State<AppState>,
    Json(mut entry): Json<Entry>,
) -> impl IntoResponse {
    // Reject oversized inline payloads before they touch the store.
    if let Some(blob) = &entry.blob {
        if let Some(inline) = &blob.inline {
            if inline.len() as u64 > state.config.limits.max_transfer_bytes {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "inline payload exceeds limit",
                )
                    .into_response();
            }
        }
    }
    if entry.text.as_ref().map(|t| t.len() as u64) > Some(state.config.limits.max_transfer_bytes) {
        return (StatusCode::PAYLOAD_TOO_LARGE, "text payload exceeds limit").into_response();
    }

    entry.preview = entry.build_preview();
    let id = entry.id.clone();

    // A peer that re-captured content it already sent carries a fresh id.
    // Drop our stale copy from the same origin so the refreshed record takes
    // its place instead of piling up duplicates. A pinned copy is a local
    // promise to keep that record: the identical payload is already here, so
    // the push is acknowledged without storing a second one.
    if state.config.storage.dedupe {
        let fingerprint = entry.fingerprint();
        let stale = state
            .store
            .list(state.config.storage.history_limit.max(1), None)
            .ok()
            .and_then(|entries| {
                entries.into_iter().find(|e| {
                    e.id != entry.id
                        && e.fingerprint() == fingerprint
                        && e.origin.host == entry.origin.host
                })
            });
        match stale {
            Some(stale) if stale.pinned => {
                tracing::info!(
                    "pushed entry {} matches pinned copy {}; not stored",
                    id,
                    stale.id
                );
                return Json(protocol::PushResponse { accepted: 0, id }).into_response();
            }
            Some(stale) => match state.store.delete(&stale.id) {
                Ok(true) => tracing::info!("replaced stale copy {} with pushed entry", stale.id),
                Ok(false) => {}
                Err(e) => tracing::warn!("could not drop stale copy {}: {e}", stale.id),
            },
            None => {}
        }
    }

    match state.store.insert(&entry) {
        Ok(()) => {
            if let Err(e) = state.store.prune(state.config.storage.history_limit) {
                tracing::warn!("prune failed: {e}");
            }
            Json(protocol::PushResponse { accepted: 1, id }).into_response()
        }
        Err(e) => {
            tracing::error!("insert failed: {e}");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

async fn get_entry(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> impl IntoResponse {
    match state.store.get(&id) {
        Ok(Some(entry)) => Json(entry).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn delete_entry(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> impl IntoResponse {
    match state.store.delete(&id) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `GET /v1/entries/{id}/thumb` - the small preview of an image entry.
///
/// Read-only and tiny, so peers may ask for it directly. When the entry is
/// not held here the bytes are fetched from whichever node captured it.
async fn get_thumb(State(state): State<AppState>, AxumPath(id): AxumPath<String>) -> Response {
    match state.engine.thumbnail_for(&id).await {
        Ok(bytes) => Response::builder()
            .header(header::CONTENT_TYPE, "image/png")
            .header(header::CACHE_CONTROL, "private, max-age=31536000")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(e) => error_response(e),
    }
}

async fn get_blob(
    State(state): State<AppState>,
    AxumPath(hash): AxumPath<String>,
) -> impl IntoResponse {
    // Guard against path traversal: only hex hashes reach the filesystem.
    if !hash.chars().all(|c| c.is_ascii_hexdigit()) || hash.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    match state.store.open_blob(&hash) {
        Ok(Some(file)) => {
            let stream = ReaderStream::new(tokio::fs::File::from_std(file));
            Response::builder()
                .header(header::CONTENT_TYPE, "application/octet-stream")
                .header(header::CACHE_CONTROL, "private, max-age=31536000")
                .body(Body::from_stream(stream))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Stream a captured file (or a tar of a captured directory).
///
/// The path comes from the stored entry, never from the request, so a peer
/// cannot turn this into an arbitrary file read.
async fn get_entry_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    AxumPath((id, index)): AxumPath<(String, usize)>,
) -> impl IntoResponse {
    let entry = match state.store.get(&id) {
        Ok(Some(entry)) => entry,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    let file = match entry.files.as_ref().and_then(|files| files.get(index)) {
        Some(file) => file.clone(),
        None => return StatusCode::NOT_FOUND.into_response(),
    };

    let path = PathBuf::from(&file.path);
    if file.is_dir {
        return stream_directory(&path).await;
    }
    stream_file(
        &path,
        &file.name,
        state.config.limits.max_transfer_bytes,
        &headers,
    )
    .await
}

const STREAM_BUFFER_SIZE: usize = 256 * 1024;

#[derive(Debug, PartialEq, Eq)]
enum ByteRange {
    Inclusive { start: u64, end: u64 },
    Unsatisfiable,
}

fn parse_range(value: &str, size: u64) -> ByteRange {
    let Some(spec) = value.strip_prefix("bytes=") else {
        return ByteRange::Unsatisfiable;
    };
    if spec.contains(',') || size == 0 {
        return ByteRange::Unsatisfiable;
    }
    let Some((start, end)) = spec.split_once('-') else {
        return ByteRange::Unsatisfiable;
    };
    if start.is_empty() {
        let Ok(length) = end.parse::<u64>() else {
            return ByteRange::Unsatisfiable;
        };
        if length == 0 {
            return ByteRange::Unsatisfiable;
        }
        return ByteRange::Inclusive {
            start: size.saturating_sub(length),
            end: size - 1,
        };
    }
    let Ok(start) = start.parse::<u64>() else {
        return ByteRange::Unsatisfiable;
    };
    if start >= size {
        return ByteRange::Unsatisfiable;
    }
    let end = match end {
        "" => size - 1,
        value => match value.parse::<u64>() {
            Ok(end) if end >= start => end.min(size - 1),
            _ => return ByteRange::Unsatisfiable,
        },
    };
    ByteRange::Inclusive { start, end }
}

fn metadata_signature(metadata: &std::fs::Metadata) -> (u64, u128) {
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    (metadata.len(), modified_nanos)
}

fn metadata_etag(digest: &[u8; 32]) -> String {
    format!("\"sha256-{}\"", hex::encode(digest))
}

async fn hash_file(file: &mut tokio::fs::File) -> std::io::Result<[u8; 32]> {
    file.seek(SeekFrom::Start(0)).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; STREAM_BUFFER_SIZE];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    file.seek(SeekFrom::Start(0)).await?;
    Ok(hasher.finalize().into())
}

fn if_range_allows(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_RANGE)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|if_range| if_range == etag)
}

async fn stream_file(
    path: &Path,
    name: &str,
    max_transfer_bytes: u64,
    headers: &HeaderMap,
) -> Response {
    match tokio::fs::File::open(path).await {
        Ok(mut file) => match file.metadata().await {
            Ok(metadata) => {
                let size = metadata.len();
                if size > max_transfer_bytes {
                    return (StatusCode::PAYLOAD_TOO_LARGE, "file exceeds transfer limit")
                        .into_response();
                }
                let signature_before = metadata_signature(&metadata);
                let digest = match hash_file(&mut file).await {
                    Ok(digest) => digest,
                    Err(e) => {
                        tracing::warn!("cannot hash {}: {e}", path.display());
                        return (StatusCode::INTERNAL_SERVER_ERROR, "cannot read file")
                            .into_response();
                    }
                };
                let signature_after = match file.metadata().await {
                    Ok(metadata) => metadata_signature(&metadata),
                    Err(e) => {
                        tracing::warn!("cannot restat {}: {e}", path.display());
                        return (StatusCode::CONFLICT, "file changed while reading")
                            .into_response();
                    }
                };
                if signature_before != signature_after {
                    return (StatusCode::CONFLICT, "file changed while reading").into_response();
                }
                let etag = metadata_etag(&digest);
                let requested_range = headers
                    .get(header::RANGE)
                    .and_then(|value| value.to_str().ok());
                let range_allowed =
                    requested_range.is_some_and(|_| if_range_allows(headers, &etag));
                let selected_range = requested_range
                    .filter(|_| range_allowed)
                    .map(|value| parse_range(value, size));

                let (status, offset, length, content_range) = match selected_range {
                    Some(ByteRange::Inclusive { start, end }) => (
                        StatusCode::PARTIAL_CONTENT,
                        start,
                        end - start + 1,
                        Some(format!("bytes {start}-{end}/{size}")),
                    ),
                    Some(ByteRange::Unsatisfiable) => {
                        return Response::builder()
                            .status(StatusCode::RANGE_NOT_SATISFIABLE)
                            .header(header::CONTENT_RANGE, format!("bytes */{size}"))
                            .header(header::ETAG, etag)
                            .body(Body::empty())
                            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
                    }
                    None => (StatusCode::OK, 0, size, None),
                };

                if offset > 0 && file.seek(SeekFrom::Start(offset)).await.is_err() {
                    return (StatusCode::INTERNAL_SERVER_ERROR, "cannot seek file").into_response();
                }
                let stream = ReaderStream::with_capacity(file.take(length), STREAM_BUFFER_SIZE);
                build_file_response(
                    Body::from_stream(stream),
                    name,
                    false,
                    length,
                    status,
                    Some(etag),
                    content_range,
                )
            }
            Err(e) => {
                tracing::warn!("cannot stat {}: {e}", path.display());
                (StatusCode::NOT_FOUND, "file no longer exists on the origin").into_response()
            }
        },
        Err(e) => {
            tracing::warn!("cannot open {}: {e}", path.display());
            (StatusCode::NOT_FOUND, "file no longer exists on the origin").into_response()
        }
    }
}

async fn stream_directory(path: &Path) -> Response {
    let tmp = paths::data_dir().join(format!(".tar-{}.tmp", uuid::Uuid::new_v4()));
    if let Some(parent) = tmp.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let dir = path.to_path_buf();
    let out = tmp.clone();
    let tarred = tokio::task::spawn_blocking(move || {
        let file = std::fs::File::create(&out)?;
        let mut builder = tar::Builder::new(file);
        builder.append_dir_all(".", &dir)?;
        builder.into_inner()?.sync_all()?;
        Ok::<(), Error>(())
    })
    .await;

    match tarred {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::error!("tar failed for {}: {e}", path.display());
            let _ = std::fs::remove_file(&tmp);
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
        Err(e) => {
            tracing::error!("tar task panicked: {e}");
            let _ = std::fs::remove_file(&tmp);
            return (StatusCode::INTERNAL_SERVER_ERROR, "tar failed").into_response();
        }
    }

    let size = std::fs::metadata(&tmp).map(|m| m.len()).unwrap_or(0);
    let name = path
        .file_name()
        .map(|n| format!("{}.tar", n.to_string_lossy()))
        .unwrap_or_else(|| "archive.tar".to_string());

    match tokio::fs::File::open(&tmp).await {
        Ok(file) => {
            let stream = ReaderStream::with_capacity(file, STREAM_BUFFER_SIZE);
            let wrapped = CleanupStream {
                inner: stream,
                path: Some(tmp),
            };
            build_file_response(
                Body::from_stream(wrapped),
                &name,
                true,
                size,
                StatusCode::OK,
                None,
                None,
            )
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

fn build_file_response(
    body: Body,
    name: &str,
    is_dir: bool,
    size: u64,
    status: StatusCode,
    etag: Option<String>,
    content_range: Option<String>,
) -> Response {
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(protocol::FILENAME_HEADER, name)
        .header(protocol::IS_DIR_HEADER, if is_dir { "1" } else { "0" })
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, size);
    if let Some(etag) = etag {
        builder = builder.header(header::ETAG, etag);
    }
    if let Some(content_range) = content_range {
        builder = builder.header(header::CONTENT_RANGE, content_range);
    }
    builder
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Block `POST /v1/actions/*` from the tailnet unless explicitly allowed.
///
/// Reading another node's history is fine by design; making that node's
/// clipboard change without its owner asking is not.
async fn local_only(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if state.config.node.allow_remote_apply {
        return next.run(request).await;
    }

    let is_loopback = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip().is_loopback())
        .unwrap_or(false);

    if is_loopback {
        next.run(request).await
    } else {
        tracing::warn!("rejected remote action request (enable node.allow_remote_apply to permit)");
        StatusCode::FORBIDDEN.into_response()
    }
}

fn error_response(error: Error) -> Response {
    match error {
        Error::Clipboard(_) => {
            (StatusCode::UNPROCESSABLE_ENTITY, error.to_string()).into_response()
        }
        Error::NotFound(_) => (StatusCode::NOT_FOUND, error.to_string()).into_response(),
        Error::Unauthorized | Error::PeerUnreachable { .. } => {
            StatusCode::BAD_GATEWAY.into_response()
        }
        _ => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

fn is_loopback_request(request: &Request) -> bool {
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip().is_loopback())
        .unwrap_or(false)
}

async fn capture_gate(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if is_loopback_request(&request) || state.config.node.allow_remote_capture {
        next.run(request).await
    } else {
        StatusCode::FORBIDDEN.into_response()
    }
}

async fn strict_local(request: Request, next: Next) -> Response {
    if is_loopback_request(&request) {
        next.run(request).await
    } else {
        StatusCode::FORBIDDEN.into_response()
    }
}

#[derive(serde::Deserialize)]
struct FetchClipboardRequest {
    node_id: String,
}

async fn list_peers(State(state): State<AppState>) -> Response {
    match crate::tailscale::peers(state.config.network.port).await {
        Ok(peers) => Json(peers).into_response(),
        Err(_) => (StatusCode::BAD_GATEWAY, "device discovery unavailable").into_response(),
    }
}

async fn action_fetch_clipboard(
    State(state): State<AppState>,
    Json(request): Json<FetchClipboardRequest>,
) -> Response {
    match state.engine.fetch_clipboard(&request.node_id).await {
        Ok(entry) => Json(entry).into_response(),
        Err(error) => error_response(error),
    }
}

/// Capture the current clipboard into this node's history.
async fn action_capture(State(state): State<AppState>) -> Response {
    match state.engine.capture().await {
        Ok(outcome) => {
            // The body stays an Entry so older clients are unaffected; the
            // duplicate flag rides along in a header they can ignore.
            let mut response = Json(outcome.entry).into_response();
            if outcome.deduped {
                response
                    .headers_mut()
                    .insert(protocol::DEDUPED_HEADER, HeaderValue::from_static("1"));
            }
            response
        }
        Err(e) => {
            tracing::warn!("capture failed: {e}");
            error_response(e)
        }
    }
}

/// Capture the given files/directories into this node's history.
///
/// Loopback-only (the actions layer enforces it): the request names local
/// paths, which must never be readable from the tailnet.
async fn action_capture_files(
    State(state): State<AppState>,
    Json(request): Json<protocol::CaptureFilesRequest>,
) -> Response {
    let paths: Vec<PathBuf> = request.paths.iter().map(PathBuf::from).collect();
    match state.engine.capture_files(&paths) {
        Ok(outcome) => {
            let mut response = Json(outcome.entry).into_response();
            if outcome.deduped {
                response
                    .headers_mut()
                    .insert(protocol::DEDUPED_HEADER, HeaderValue::from_static("1"));
            }
            response
        }
        Err(e) => {
            tracing::warn!("capture-files failed: {e}");
            error_response(e)
        }
    }
}

/// Put an entry onto this machine's clipboard.
async fn action_apply(
    State(state): State<AppState>,
    Json(request): Json<protocol::ApplyRequest>,
) -> Response {
    let entry = match request.entry {
        Some(entry) => Some(entry),
        None => match request.id.as_deref() {
            Some(id) => state.engine.find(id).await.ok().flatten(),
            None => None,
        },
    };

    let Some(entry) = entry else {
        return (StatusCode::NOT_FOUND, "entry not found").into_response();
    };

    match state.engine.apply(&entry).await {
        Ok(result) => Json(protocol::ActionResult {
            ok: true,
            kind: result.kind.as_str().to_string(),
            detail: result.detail,
        })
        .into_response(),
        Err(e) => {
            tracing::warn!("apply failed: {e}");
            error_response(e)
        }
    }
}

/// Capture (or reuse) an entry and push it to one peer or all of them.
async fn action_send(
    State(state): State<AppState>,
    Json(request): Json<protocol::SendRequest>,
) -> Response {
    let entry = match request.id.as_deref() {
        Some(id) => match state.engine.find(id).await {
            Ok(Some(entry)) => entry,
            _ => return (StatusCode::NOT_FOUND, "entry not found").into_response(),
        },
        None => match state.engine.capture().await {
            Ok(outcome) => outcome.entry,
            Err(e) => {
                tracing::warn!("capture before send failed: {e}");
                return error_response(e);
            }
        },
    };

    let outcomes = match request.host.as_deref() {
        Some(host) => {
            let peers = state.engine.peers().await;
            let needle = host.to_lowercase();
            let Some(peer) = peers.into_iter().find(|p| {
                p.host.to_lowercase().contains(&needle)
                    || p.dns_name.to_lowercase().contains(&needle)
            }) else {
                return (StatusCode::NOT_FOUND, format!("no peer matching {host}")).into_response();
            };
            let outcome = state.engine.send_to(&peer, &entry).await;
            vec![(peer.host.clone(), outcome)]
        }
        None => state.engine.broadcast(&entry).await,
    };

    Json(protocol::SendResult {
        id: entry.id.clone(),
        targets: outcomes
            .into_iter()
            .map(|(host, outcome)| {
                if let Err(e) = &outcome {
                    tracing::warn!("send to {host} failed: {e}");
                }
                (host, outcome.is_ok())
            })
            .collect(),
    })
    .into_response()
}

/// Pin (or unpin) an entry in this node's history.
///
/// Loopback-only like the other actions: pinning is a per-machine preference,
/// not something a peer should decide. Remote entries are copied into the
/// local store first so the pin actually protects a local payload.
async fn action_pin(
    State(state): State<AppState>,
    Json(request): Json<protocol::PinRequest>,
) -> Response {
    match state.engine.pin(&request.id, request.pinned).await {
        Ok(Some(entry)) => Json(entry).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "entry not found").into_response(),
        Err(e) => error_response(e),
    }
}

/// The on-disk config, falling back to the snapshot the daemon started with.
///
/// Launch requests read it per call, so `peercarry apps add` and flipping
/// `node.allow_remote_launch` take effect without restarting the daemon.
fn launch_config(state: &AppState) -> Config {
    Config::load().unwrap_or_else(|_| (*state.config).clone())
}

/// Gate `POST /v1/actions/launch`: loopback always, tailnet only when
/// `node.allow_remote_launch` is on. Separate from `local_only` because
/// launching a program is a bigger promise than applying a clipboard entry.
async fn launch_gate(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let is_loopback = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip().is_loopback())
        .unwrap_or(false);

    if is_loopback || launch_config(&state).node.allow_remote_launch {
        next.run(request).await
    } else {
        tracing::warn!(
            "rejected remote launch request (enable node.allow_remote_launch to permit)"
        );
        StatusCode::FORBIDDEN.into_response()
    }
}

/// Launch a registered app on this machine.
async fn action_launch(
    State(state): State<AppState>,
    Json(request): Json<protocol::LaunchRequest>,
) -> Response {
    let config = launch_config(&state);
    match config.find_app(&request.name) {
        Some(app) => match Engine::spawn_app(app) {
            Ok(detail) => Json(protocol::ActionResult {
                ok: true,
                kind: "launch".to_string(),
                detail,
            })
            .into_response(),
            Err(e) => {
                tracing::warn!("launch of {:?} failed: {e}", request.name);
                error_response(e)
            }
        },
        None => (
            StatusCode::NOT_FOUND,
            format!(
                "app {:?} is not registered here - add it with `peercarry apps add`",
                request.name
            ),
        )
            .into_response(),
    }
}

/// `POST /v1/actions/dedupe` - drop older copies of the same content.
async fn action_dedupe(State(state): State<AppState>) -> Response {
    match state.engine.dedupe() {
        Ok(removed) => Json(serde_json::json!({ "removed": removed })).into_response(),
        Err(e) => error_response(e),
    }
}

/// Block until the local daemon port can be bound again, i.e. the previous
/// process has released it. Part of the self-restart handoff: the fresh
/// process starts while the old one is still finishing its exit, and must
/// not race it for the listener or the store lock.
pub fn wait_for_local_port(port: u16, timeout: std::time::Duration) {
    let deadline = std::time::Instant::now() + timeout;
    while std::net::TcpListener::bind(("127.0.0.1", port)).is_err() {
        if std::time::Instant::now() >= deadline {
            tracing::warn!("port {port} still busy after {timeout:?}, starting anyway");
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }
}

/// Wait before opening redb in a replacement process. A saved configuration
/// may change the listen address, so the previous process identity, rather
/// than the new port, is authoritative. Legacy launches without a parent
/// identity retain a bounded port-based fallback.
pub fn wait_for_restart_handoff(port: u16, timeout: std::time::Duration) -> Result<()> {
    let pid = std::env::var(RESTART_PARENT_PID);
    let started_at = std::env::var(RESTART_PARENT_START);
    // Consume launch-only context; later tool/update subprocesses must not
    // inherit the identity of an earlier restart generation.
    std::env::remove_var(RESTART_PARENT_PID);
    std::env::remove_var(RESTART_PARENT_START);
    std::env::remove_var("PEERCARRY_RESTART");
    let optional = |value: std::result::Result<String, std::env::VarError>| match value {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => Err(Error::Network("invalid restart handoff".into())),
    };
    let pid = optional(pid)?;
    let started_at = optional(started_at)?;
    let parent = RestartParent::parse(pid.as_deref(), started_at.as_deref())?;
    let deadline = std::time::Instant::now() + timeout;
    if let Some(parent) = parent {
        if parent.pid == std::process::id() {
            return Err(Error::Network(
                "restart parent cannot be this process".into(),
            ));
        }
        let mut system = sysinfo::System::new();
        let process_id = sysinfo::Pid::from_u32(parent.pid);
        loop {
            system.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::Some(&[process_id]),
                true,
                sysinfo::ProcessRefreshKind::nothing(),
            );
            let still_alive = system.process(process_id).is_some_and(|process| {
                (process.start_time() == parent.started_at || process.start_time() == 0)
                    && process.status() != sysinfo::ProcessStatus::Zombie
            });
            if !still_alive {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::Network(
                    "previous process did not exit before restart deadline".into(),
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    while std::net::TcpListener::bind(("127.0.0.1", port)).is_err() {
        if std::time::Instant::now() >= deadline {
            return Err(Error::Network(
                "previous listener did not close before restart deadline".into(),
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Ok(())
}

pub fn router(state: AppState) -> Router {
    // Routes reachable from the tailnet.
    let shared = Router::new()
        .route("/v1/hello", get(hello))
        .route("/v1/apps", get(list_apps))
        .route("/v1/entries", get(list_entries).post(push_entry))
        .route("/v1/entries/{id}", get(get_entry).delete(delete_entry))
        .route("/v1/blobs/{hash}", get(get_blob))
        .route("/v1/entries/{id}/thumb", get(get_thumb))
        .route("/v1/entries/{id}/files/{index}", get(get_entry_file));

    // Routes that drive this machine's clipboard: local by default.
    let actions = Router::new()
        .route("/v1/actions/capture-files", post(action_capture_files))
        .route("/v1/actions/apply", post(action_apply))
        .route("/v1/actions/send", post(action_send))
        .route("/v1/actions/pin", post(action_pin))
        .route("/v1/actions/dedupe", post(action_dedupe))
        .route("/v1/actions/restart", post(action_restart))
        .layer(middleware::from_fn_with_state(state.clone(), local_only));

    let capture = Router::new()
        .route("/v1/actions/capture", post(action_capture))
        .layer(middleware::from_fn_with_state(state.clone(), capture_gate));
    let ai_local = Router::new()
        .route("/v1/ai/overview", get(crate::ai_monitor::overview))
        .route("/v1/ai/tools", get(crate::ai_monitor::tools))
        .route("/v1/ai/bind", post(crate::ai_monitor::bind_tool))
        .route("/v1/ai/receiver", post(crate::ai_monitor::receiver))
        .layer(axum::extract::DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn(crate::ai_monitor::local_gate));
    let fetch = Router::new()
        .route("/v1/peers", get(list_peers))
        .route("/v1/actions/fetch-clipboard", post(action_fetch_clipboard))
        .layer(middleware::from_fn(strict_local));

    let desktop_tools = Router::new()
        .route("/quick", get(quick_ui))
        .route("/v1/desktop/language", get(desktop_language))
        .route("/assets/desktop-theme.css", get(desktop_ui_style))
        .route("/assets/ui-locale.js", get(locale_ui_script))
        .route("/assets/transfer-center.js", get(transfer_ui_script))
        .layer(middleware::from_fn(crate::ai_monitor::local_gate));

    // Launching a program gets its own gate: loopback always, the tailnet
    // only after `node.allow_remote_launch`.
    let launch_routes = Router::new()
        .route("/v1/actions/launch", post(action_launch))
        .layer(middleware::from_fn_with_state(state.clone(), launch_gate));

    // The searchable timeline: its page and its feed stay loopback-only.
    // The feed aggregates every machine's metadata, and the page drives the
    // local clipboard - both are for the local user's browser. The settings
    // endpoints live here too: they expose `auth_token` and write the
    // config, which only the local user may do.
    let timeline = Router::new()
        .route("/", get(timeline_ui))
        .route("/v1/timeline", get(timeline))
        .route("/v1/config", get(get_config).post(put_config))
        .layer(middleware::from_fn_with_state(state.clone(), local_only));

    Router::new()
        .merge(shared)
        .merge(crate::ai_tasks::router())
        .merge(crate::uploads::router(state.clone()))
        .merge(crate::transfer_jobs::router(state.clone()))
        .merge(crate::mobile_connect::local_router(state.clone()))
        .merge(crate::notebook::router())
        .merge(crate::notebook_sync::local_router())
        .merge(crate::notebook_sync::network_router(state.clone()))
        .merge(crate::desktop_settings::router(&state))
        .merge(actions)
        .merge(capture)
        .merge(fetch)
        .merge(desktop_tools)
        .merge(ai_local)
        .merge(launch_routes)
        .merge(timeline)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .with_state(state)
        .layer(middleware::from_fn(track_activity))
        .layer(middleware::from_fn(crate::desktop_settings::outer_gate))
}

pub(crate) async fn track_activity(request: Request, next: Next) -> Response {
    use futures_util::StreamExt;
    let Some(activity) = crate::maintenance::enter() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "update in progress").into_response();
    };
    let response = next.run(request).await;
    let (parts, body) = response.into_parts();
    let stream = body.into_data_stream().map(move |chunk| {
        let _keep_until_body_finishes = &activity;
        chunk
    });
    Response::from_parts(parts, Body::from_stream(stream))
}

/// Run the service until the process is shut down.
pub async fn serve(config: Arc<Config>, store: Arc<Store>) -> Result<()> {
    tokio::spawn(crate::ai_monitor::run(config.clone()));
    let bind = bind_addr(&config).await;
    let origin = build_origin(&config, bind).await;

    tracing::info!(
        "peercarry {} listening on {} as {:?}",
        env!("CARGO_PKG_VERSION"),
        bind,
        origin.host
    );
    if config.network.auth_token.is_none() {
        tracing::warn!("no auth_token configured - relying on Tailscale for access control");
    }

    let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone())?);
    let state = AppState {
        store,
        config: config.clone(),
        origin,
        engine,
    };
    let sync_store = state.store.clone();
    let app = router(state);

    let listener = TcpListener::bind(bind).await?;
    // ConnectInfo lets the local-only middleware inspect the peer address.
    let mut tailnet = AbortOnDrop::new(tokio::spawn(
        axum::serve(
            listener,
            app.clone()
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .into_future(),
    ));

    // The advertised address is the Tailscale IP, which loopback clients
    // cannot reach. Listen on 127.0.0.1 as well so the local CLI and tray app
    // always have a way in.
    let local = if bind.ip().is_loopback() {
        None
    } else {
        let addr = SocketAddr::from(([127, 0, 0, 1], config.network.port));
        match TcpListener::bind(addr).await {
            Ok(listener) => {
                tracing::info!("also listening on {addr} for local clients");
                Some(AbortOnDrop::new(tokio::spawn(
                    axum::serve(
                        listener,
                        app.into_make_service_with_connect_info::<SocketAddr>(),
                    )
                    .into_future(),
                )))
            }
            Err(e) => {
                tracing::warn!("cannot bind {addr} for local clients: {e}");
                None
            }
        }
    };

    let outcome = with_worker(
        crate::notebook_sync::run_worker(config.clone(), sync_store),
        async move {
            match local {
                Some(mut handle) => tokio::select! {
                    result = tailnet.handle_mut() => result,
                    result = handle.handle_mut() => result,
                },
                None => tailnet.handle_mut().await,
            }
        },
    )
    .await;

    outcome
        .map_err(|e| Error::Network(format!("server task panicked: {e}")))?
        .map_err(|e| Error::Network(format!("server error: {e}")))
}

struct AbortOnDrop<T> {
    handle: Option<tokio::task::JoinHandle<T>>,
}

impl<T> AbortOnDrop<T> {
    fn new(handle: tokio::task::JoinHandle<T>) -> Self {
        Self {
            handle: Some(handle),
        }
    }

    async fn abort_and_join(&mut self) {
        if let Some(handle) = self.handle.as_mut() {
            handle.abort();
            let _ = handle.await;
        }
        self.handle.take();
    }

    fn handle_mut(&mut self) -> &mut tokio::task::JoinHandle<T> {
        self.handle
            .as_mut()
            .expect("task handle stays owned until the guard is dropped")
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = &self.handle {
            handle.abort();
        }
    }
}

async fn with_worker<W, S, O>(worker: W, server: S) -> O
where
    W: Future<Output = ()> + Send + 'static,
    S: Future<Output = O>,
{
    let mut owner = AbortOnDrop::new(tokio::spawn(worker));
    let output = server.await;
    owner.abort_and_join().await;
    output
}

/// Helper used by tests and the CLI to build headers carrying the token.
pub fn auth_headers(config: &Config) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(token) = &config.network.auth_token {
        if !token.is_empty() {
            if let Ok(value) = token.parse() {
                headers.insert(protocol::TOKEN_HEADER, value);
            }
        }
    }
    headers
}

/// Re-exported so callers do not depend on `Bytes` directly.
pub type PayloadChunk = Bytes;

#[cfg(test)]
mod tests {
    use crate::store::Store;
    use std::sync::Arc;

    #[test]
    fn cross_origin_local_navigation_only_admits_an_activated_home_document() {
        use super::*;
        for (path, method, dest, user, peer, origin, expected) in [
            ("/", "GET", "document", "?1", "127.0.0.1:1234", None, true),
            (
                "/v1/desktop/settings",
                "GET",
                "document",
                "?1",
                "127.0.0.1:1234",
                None,
                false,
            ),
            (
                "/v1/config",
                "GET",
                "document",
                "?1",
                "127.0.0.1:1234",
                None,
                false,
            ),
            ("/", "POST", "document", "?1", "127.0.0.1:1234", None, false),
            ("/", "GET", "iframe", "?1", "127.0.0.1:1234", None, false),
            ("/", "GET", "document", "", "127.0.0.1:1234", None, false),
            ("/", "GET", "document", "?1", "100.64.0.8:1234", None, false),
            (
                "/",
                "GET",
                "document",
                "?1",
                "127.0.0.1:1234",
                Some("http://other.example"),
                false,
            ),
        ] {
            for site in ["same-site", "cross-site"] {
                let mut request = Request::builder()
                    .method(method)
                    .uri(path)
                    .header(header::HOST, "127.0.0.1:5199")
                    .header("sec-fetch-site", site)
                    .header("sec-fetch-mode", "navigate")
                    .header("sec-fetch-dest", dest)
                    .header("sec-fetch-user", user)
                    .body(Body::empty())
                    .unwrap();
                request
                    .extensions_mut()
                    .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
                if let Some(origin) = origin {
                    request
                        .headers_mut()
                        .insert(header::ORIGIN, origin.parse().unwrap());
                }
                assert_eq!(
                    trusted_desktop_request(&request),
                    expected,
                    "{site} {method} {path}"
                );
            }
        }
    }

    #[test]
    fn desktop_theme_and_phone_controls_preserve_local_origin_boundary() {
        use super::*;
        for path in [
            "/assets/desktop-theme.css",
            "/v1/desktop/language",
            "/v1/mobile/status",
            "/v1/mobile/pairing",
            "/v1/mobile/stop",
            "/v1/mobile/devices/test-device",
        ] {
            for (peer, host, origin, site, expected) in [
                ("127.0.0.1:1234", "127.0.0.1:5199", None, "none", true),
                (
                    "127.0.0.1:1234",
                    "127.0.0.1:5199",
                    Some("http://127.0.0.1:5199"),
                    "same-origin",
                    true,
                ),
                ("100.64.0.8:1234", "127.0.0.1:5199", None, "none", false),
                ("127.0.0.1:1234", "other.example", None, "none", false),
                (
                    "127.0.0.1:1234",
                    "127.0.0.1:5199",
                    Some("https://other.example"),
                    "cross-site",
                    false,
                ),
            ] {
                let mut request = Request::builder()
                    .uri(path)
                    .header(header::HOST, host)
                    .header("sec-fetch-site", site)
                    .body(Body::empty())
                    .unwrap();
                request
                    .extensions_mut()
                    .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
                if let Some(origin) = origin {
                    request
                        .headers_mut()
                        .insert(header::ORIGIN, origin.parse().unwrap());
                }
                assert_eq!(trusted_desktop_request(&request), expected, "{peer} {host}");
            }
        }
    }

    #[test]
    fn restart_command_preserves_launch_mode_and_parent_identity() {
        use super::*;
        for arguments in [
            vec![
                OsString::from("--json"),
                OsString::from("-vv"),
                OsString::from("serve"),
            ],
            Vec::new(),
        ] {
            let parent = RestartParent {
                pid: 123,
                started_at: 456,
            };
            let command =
                restart_command(Path::new("synthetic application path"), &arguments, parent);
            assert_eq!(
                command.get_args().map(OsString::from).collect::<Vec<_>>(),
                arguments
            );
            let env: std::collections::BTreeMap<_, _> = command
                .get_envs()
                .map(|(key, value)| (key.to_os_string(), value.map(std::ffi::OsStr::to_os_string)))
                .collect();
            assert_eq!(
                env[std::ffi::OsStr::new("PEERCARRY_RESTART")],
                Some(OsString::from("1"))
            );
            assert_eq!(
                env[std::ffi::OsStr::new(RESTART_PARENT_PID)],
                Some(OsString::from("123"))
            );
            assert_eq!(
                env[std::ffi::OsStr::new(RESTART_PARENT_START)],
                Some(OsString::from("456"))
            );
        }
    }

    #[test]
    fn restart_parent_requires_a_complete_nonzero_identity() {
        use super::*;
        assert!(RestartParent::parse(None, None).unwrap().is_none());
        for (pid, start) in [
            (Some("123"), None),
            (None, Some("456")),
            (Some("0"), Some("456")),
            (Some("123"), Some("0")),
            (Some("invalid"), Some("456")),
        ] {
            assert!(RestartParent::parse(pid, start).is_err());
        }
        let parent = RestartParent::parse(Some("123"), Some("456"))
            .unwrap()
            .unwrap();
        assert_eq!((parent.pid, parent.started_at), (123, 456));
    }

    #[tokio::test]
    async fn locale_asset_includes_shared_device_preferences() {
        use super::*;

        let response = locale_ui_script().await.into_response();
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "application/javascript; charset=utf-8"
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        let (bootstrap, script) = text.split_once('\n').unwrap();
        let metadata: serde_json::Value = serde_json::from_str(
            bootstrap
                .strip_prefix("window.__PEERCARRY_LOCALE__ = ")
                .unwrap()
                .trim_end_matches(';'),
        )
        .unwrap();
        assert_eq!(metadata.as_object().unwrap().len(), 3);
        assert!(matches!(metadata["language"].as_str(), Some("zh" | "en")));
        assert_eq!(
            script,
            concat!(
                include_str!("../ui-locale.js"),
                "\n",
                include_str!("../device-preferences.js")
            )
        );
    }

    #[tokio::test]
    async fn remote_capture_permission_is_independent_and_fetch_stays_local() {
        use super::*;
        let root =
            std::env::temp_dir().join(format!("peercarry-capture-gate-{}", uuid::Uuid::new_v4()));
        let store = Arc::new(Store::open(&root.join("state.redb")).unwrap());
        for allowed in [false, true] {
            let mut config = Config::default();
            config.node.allow_remote_apply = true;
            config.node.allow_remote_capture = allowed;
            let config = Arc::new(config);
            let origin = Origin {
                host: "test".into(),
                node_id: "test".into(),
                addr: "http://127.0.0.1".into(),
                os: "test".into(),
            };
            let engine =
                Arc::new(Engine::new(config.clone(), store.clone(), origin.clone()).unwrap());
            let state = AppState {
                config,
                store: store.clone(),
                origin,
                engine,
            };
            // Stub handlers ensure tests never read or overwrite the system clipboard.
            let capture = Router::new()
                .route("/capture", post(|| async { StatusCode::NO_CONTENT }))
                .layer(middleware::from_fn_with_state(state, capture_gate));
            let app = Router::new()
                .merge(capture)
                .merge(
                    Router::new()
                        .route("/fetch", post(|| async { StatusCode::NO_CONTENT }))
                        .layer(middleware::from_fn(strict_local)),
                )
                .layer(middleware::from_fn(
                    |mut request: Request, next: Next| async move {
                        let addr = if request.headers().contains_key("test-local") {
                            "127.0.0.1:1234"
                        } else {
                            "100.64.0.2:1234"
                        };
                        request
                            .extensions_mut()
                            .insert(ConnectInfo(addr.parse::<SocketAddr>().unwrap()));
                        next.run(request).await
                    },
                ));
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            assert_eq!(
                client
                    .post(format!("{url}/capture"))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                if allowed {
                    StatusCode::NO_CONTENT
                } else {
                    StatusCode::FORBIDDEN
                }
            );
            assert_eq!(
                client
                    .post(format!("{url}/fetch"))
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::FORBIDDEN
            );
            for route in ["capture", "fetch"] {
                assert_eq!(
                    client
                        .post(format!("{url}/{route}"))
                        .header("test-local", "1")
                        .send()
                        .await
                        .unwrap()
                        .status(),
                    StatusCode::NO_CONTENT
                );
            }
            task.abort();
            let _ = task.await;
        }
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    use super::{if_range_allows, parse_range, ByteRange};
    use axum::http::{header, HeaderMap, HeaderValue, StatusCode};

    #[test]
    fn parses_single_byte_ranges() {
        assert_eq!(
            parse_range("bytes=10-19", 100),
            ByteRange::Inclusive { start: 10, end: 19 }
        );
        assert_eq!(
            parse_range("bytes=10-", 100),
            ByteRange::Inclusive { start: 10, end: 99 }
        );
        assert_eq!(
            parse_range("bytes=-10", 100),
            ByteRange::Inclusive { start: 90, end: 99 }
        );
    }

    #[test]
    fn clamps_end_and_rejects_invalid_ranges() {
        assert_eq!(
            parse_range("bytes=90-200", 100),
            ByteRange::Inclusive { start: 90, end: 99 }
        );
        assert_eq!(parse_range("bytes=100-", 100), ByteRange::Unsatisfiable);
        assert_eq!(parse_range("bytes=20-10", 100), ByteRange::Unsatisfiable);
        assert_eq!(parse_range("bytes=1-2,4-5", 100), ByteRange::Unsatisfiable);
        assert_eq!(parse_range("bytes=-0", 100), ByteRange::Unsatisfiable);
    }

    #[test]
    fn if_range_requires_the_current_etag() {
        let etag = r#""sha256-abc""#;
        let mut headers = HeaderMap::new();
        headers.insert(
            header::IF_RANGE,
            HeaderValue::from_static(r#""sha256-abc""#),
        );
        assert!(if_range_allows(&headers, etag));
        headers.insert(
            header::IF_RANGE,
            HeaderValue::from_static(r#""sha256-def""#),
        );
        assert!(!if_range_allows(&headers, etag));
    }

    #[tokio::test]
    async fn streams_range_with_strong_etag_and_ignores_stale_if_range() {
        let path =
            std::env::temp_dir().join(format!("peercarry-server-{}.bin", uuid::Uuid::new_v4()));
        let contents = b"0123456789";
        std::fs::write(&path, contents).unwrap();

        let mut range_headers = HeaderMap::new();
        range_headers.insert(header::RANGE, HeaderValue::from_static("bytes=2-4"));
        let response = super::stream_file(&path, "sample.bin", 100, &range_headers).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 2-4/10");
        let etag = response.headers()[header::ETAG].clone();
        assert!(etag.to_str().unwrap().starts_with("\"sha256-"));
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"234");

        let mut stale_headers = HeaderMap::new();
        stale_headers.insert(header::RANGE, HeaderValue::from_static("bytes=2-4"));
        stale_headers.insert(
            header::IF_RANGE,
            HeaderValue::from_static("\"sha256-stale\""),
        );
        let response = super::stream_file(&path, "sample.bin", 100, &stale_headers).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], contents);

        std::fs::remove_file(path).unwrap();
    }

    struct NotifyDrop(Option<tokio::sync::oneshot::Sender<()>>);

    impl Drop for NotifyDrop {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    fn worker_store() -> (Arc<Store>, std::path::PathBuf) {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/session-server-worker")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).unwrap();
        let store = Arc::new(Store::open_isolated_for_tests(&root.join("state.redb")).unwrap());
        (store, root)
    }

    #[tokio::test]
    async fn completed_server_aborts_and_joins_its_worker() {
        use super::*;
        use std::future::pending;

        let (store, root) = worker_store();
        let weak_store = Arc::downgrade(&store);
        let worker_store = store.clone();
        drop(store);
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let drop_signal = NotifyDrop(Some(dropped_tx));
        let worker = async move {
            let _drop_signal = drop_signal;
            let _store = worker_store;
            pending::<()>().await;
        };

        assert_eq!(with_worker(worker, async { 42 }).await, 42);
        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .expect("worker was not dropped")
            .expect("drop signal was lost");
        assert!(weak_store.upgrade().is_none(), "worker retained its Store");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn cancelling_server_future_aborts_its_worker_and_releases_store() {
        use super::*;
        use std::future::pending;

        let (store, root) = worker_store();
        let weak_store = Arc::downgrade(&store);
        let worker_store = store.clone();
        drop(store);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let drop_signal = NotifyDrop(Some(dropped_tx));
        let worker = async move {
            let _drop_signal = drop_signal;
            let _store = worker_store;
            let _ = started_tx.send(());
            pending::<()>().await;
        };

        let server = tokio::spawn(with_worker(worker, pending::<()>()));
        tokio::time::timeout(std::time::Duration::from_secs(1), started_rx)
            .await
            .expect("worker did not start")
            .expect("worker start signal was lost");
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .expect("worker was not aborted after server cancellation")
            .expect("drop signal was lost");
        assert!(weak_store.upgrade().is_none(), "worker retained its Store");
        std::fs::remove_dir_all(root).unwrap();
    }
}
