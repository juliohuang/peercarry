//! Local-first notebook replication over the authenticated Tailscale peer mesh.

use std::collections::{BTreeMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::rejection::QueryRejection;
use axum::extract::{ConnectInfo, Path as AxumPath, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::time::{self, MissedTickBehavior};
use uuid::Uuid;

use crate::config::Config;
use crate::notebook::{ApiError, NotebookItem, NotebookKind, NotebookTask};
use crate::server::AppState;
use crate::store::{NotebookResolveOutcome, Store, SyncApplyOutcome};
use crate::tailscale::{self, Node, Status as TailscaleStatus};

pub(crate) const SYNC_PROTOCOL: u8 = 1;
pub(crate) const MAX_SYNC_PEERS: usize = 16;
pub(crate) const MAX_CLOCK_REPLICAS: usize = 64;
pub(crate) const MAX_SYNC_HEADS: usize = 16;
pub(crate) const MAX_SYNC_VERSION_BYTES: usize = 320 * 1024;
pub(crate) const MAX_SYNC_CONTENT_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_SYNC_PERSISTED_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_SYNC_PAGE_ITEMS: usize = 16;
pub(crate) const MAX_SYNC_PAGE_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_SYNC_PAGES_PER_RUN: usize = 32;

const MAX_TITLE_CHARS: usize = 200;
const MAX_TASK_TEXT_BYTES: usize = 4_096;
const MAX_TASKS: usize = 500;
const MAX_NOTE_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SyncContent {
    pub id: String,
    pub kind: NotebookKind,
    pub title: String,
    pub markdown: String,
    pub tasks: Vec<NotebookTask>,
    pub pinned: bool,
    pub trashed: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SyncVersion {
    pub version_id: String,
    pub clock: BTreeMap<String, u64>,
    pub author_replica_id: String,
    pub content: SyncContent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StoredSyncHead {
    pub version_id: String,
    pub clock: BTreeMap<String, u64>,
    pub author_replica_id: String,
    /// The selected display head is stored by reference to the visible note.
    /// Other concurrent heads keep their complete content here.
    pub content: Option<SyncContent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NoteSyncMetadata {
    pub primary_version_id: String,
    pub heads: BTreeMap<String, StoredSyncHead>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SyncSettings {
    pub enabled: bool,
    pub peer_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PeerSyncState {
    Disabled,
    Offline,
    Checking,
    Checked,
    Pending,
    Unsupported,
    Denied,
    Error,
    QuotaExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PeerProgress {
    pub cursor: Option<String>,
    pub state: PeerSyncState,
    pub last_success_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub failures: u8,
    pub retry_after: Option<DateTime<Utc>>,
    pub host: Option<String>,
}

impl Default for PeerProgress {
    fn default() -> Self {
        Self {
            cursor: None,
            state: PeerSyncState::Offline,
            last_success_at: None,
            error: None,
            failures: 0,
            retry_after: None,
            host: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedSyncState {
    pub replica_id: String,
    #[serde(default)]
    pub settings_generation: u64,
    pub settings: SyncSettings,
    pub peers: BTreeMap<String, PeerProgress>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsRequest {
    enabled: bool,
    peer_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunRequest {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolveRequest {
    expected_revision: u64,
    version_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FeedQuery {
    after: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SyncFeedResponse {
    protocol: u8,
    items: Vec<SyncFeedItem>,
    next_after: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SyncFeedItem {
    note_id: String,
    versions: Vec<SyncVersion>,
}

#[derive(Debug, Serialize)]
struct ConflictVersionResponse {
    version_id: String,
    device_id: String,
    kind: NotebookKind,
    title: String,
    markdown: String,
    tasks: Vec<NotebookTask>,
    pinned: bool,
    trashed: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct PeerStatusResponse {
    node_id: String,
    host: Option<String>,
    state: PeerSyncState,
    last_success_at: Option<DateTime<Utc>>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct SyncStatusResponse {
    enabled: bool,
    ready: bool,
    needs_authentication: bool,
    peer_ids: Vec<String>,
    conflicted_items: usize,
    peers: Vec<PeerStatusResponse>,
}

#[derive(Debug, Clone)]
struct DiscoveredPeer {
    node_id: String,
    host: String,
    online: bool,
    ipv4: Option<Ipv4Addr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotebookSnapshot {
    pub item: NotebookItem,
    pub version_id: String,
    /// Number of non-primary branches retained for explicit resolution.
    pub conflict_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClockOrder {
    Before,
    Equal,
    After,
    Concurrent,
}

#[derive(Serialize)]
struct VersionDigest<'a> {
    protocol: u8,
    clock: &'a BTreeMap<String, u64>,
    author_replica_id: &'a str,
    content: &'a SyncContent,
}

pub(crate) fn content_from_item(item: &NotebookItem) -> SyncContent {
    SyncContent {
        id: item.id.clone(),
        kind: item.kind,
        title: item.title.clone(),
        markdown: item.markdown.clone(),
        tasks: item.tasks.clone(),
        pinned: item.pinned,
        trashed: item.trashed,
        created_at: item.created_at,
        updated_at: item.updated_at,
    }
}

pub(crate) fn item_from_content(content: &SyncContent, revision: u64) -> NotebookItem {
    NotebookItem {
        id: content.id.clone(),
        kind: content.kind,
        title: content.title.clone(),
        markdown: content.markdown.clone(),
        tasks: content.tasks.clone(),
        pinned: content.pinned,
        trashed: content.trashed,
        revision,
        created_at: content.created_at,
        updated_at: content.updated_at,
        version_id: None,
        conflict_count: 0,
    }
}

pub(crate) fn make_version(
    content: SyncContent,
    clock: BTreeMap<String, u64>,
    author_replica_id: String,
) -> crate::error::Result<SyncVersion> {
    validate_content(&content)?;
    validate_clock(&clock, &author_replica_id)?;
    let digest = VersionDigest {
        protocol: SYNC_PROTOCOL,
        clock: &clock,
        author_replica_id: &author_replica_id,
        content: &content,
    };
    let encoded = serde_json::to_vec(&digest)?;
    if encoded.len() > MAX_SYNC_VERSION_BYTES {
        return Err(crate::error::Error::TooLarge {
            size: encoded.len() as u64,
            limit: MAX_SYNC_VERSION_BYTES as u64,
        });
    }
    let version = SyncVersion {
        version_id: hex::encode(Sha256::digest(encoded)),
        clock,
        author_replica_id,
        content,
    };
    Ok(version)
}

pub(crate) fn initial_version(
    item: &NotebookItem,
    replica_id: &str,
) -> crate::error::Result<SyncVersion> {
    let clock = BTreeMap::from([(replica_id.to_owned(), 1)]);
    make_version(content_from_item(item), clock, replica_id.to_owned())
}

pub(crate) fn next_local_version(
    item: &NotebookItem,
    base: &BTreeMap<String, u64>,
    replica_id: &str,
) -> crate::error::Result<SyncVersion> {
    let mut clock = base.clone();
    increment_clock(&mut clock, replica_id)?;
    make_version(content_from_item(item), clock, replica_id.to_owned())
}

pub(crate) fn resolve_version(
    item_id: &str,
    heads: &[SyncVersion],
    version_id: &str,
    replica_id: &str,
    revision: u64,
) -> crate::error::Result<(NotebookItem, SyncVersion)> {
    let selected = heads
        .iter()
        .find(|version| version.version_id == version_id)
        .ok_or_else(invalid_sync_data)?;
    let mut item = item_from_content(&selected.content, revision);
    item.id = item_id.to_owned();
    let mut clock: BTreeMap<String, u64> = BTreeMap::new();
    for head in heads {
        validate_version(item_id, head)?;
        for (replica, counter) in &head.clock {
            clock
                .entry(replica.clone())
                .and_modify(|current| *current = (*current).max(*counter))
                .or_insert(*counter);
        }
    }
    increment_clock(&mut clock, replica_id)?;
    let version = make_version(content_from_item(&item), clock, replica_id.to_owned())?;
    Ok((item, version))
}

pub(crate) fn materialize_heads(
    item: &NotebookItem,
    metadata: &NoteSyncMetadata,
) -> crate::error::Result<Vec<SyncVersion>> {
    if metadata.heads.is_empty()
        || metadata.heads.len() > MAX_SYNC_HEADS
        || !metadata.heads.contains_key(&metadata.primary_version_id)
    {
        return Err(invalid_sync_data());
    }
    let visible_content = content_from_item(item);
    let mut versions = Vec::with_capacity(metadata.heads.len());
    for (version_id, stored) in &metadata.heads {
        if version_id != &stored.version_id {
            return Err(invalid_sync_data());
        }
        let content = match &stored.content {
            Some(content) => content.clone(),
            None if version_id == &metadata.primary_version_id => visible_content.clone(),
            None => return Err(invalid_sync_data()),
        };
        let version = SyncVersion {
            version_id: stored.version_id.clone(),
            clock: stored.clock.clone(),
            author_replica_id: stored.author_replica_id.clone(),
            content,
        };
        validate_version(&item.id, &version)?;
        versions.push(version);
    }
    let primary = versions
        .iter()
        .find(|version| version.version_id == metadata.primary_version_id)
        .ok_or_else(invalid_sync_data)?;
    if primary.content != visible_content
        || choose_primary(&versions).map(|version| version.version_id.as_str())
            != Some(metadata.primary_version_id.as_str())
    {
        return Err(invalid_sync_data());
    }
    Ok(versions)
}

pub(crate) fn metadata_from_heads(
    versions: &[SyncVersion],
) -> crate::error::Result<NoteSyncMetadata> {
    if versions.is_empty() || versions.len() > MAX_SYNC_HEADS {
        return Err(invalid_sync_data());
    }
    let primary = choose_primary(versions).ok_or_else(invalid_sync_data)?;
    let mut heads = BTreeMap::new();
    for version in versions {
        validate_version(&version.content.id, version)?;
        heads.insert(
            version.version_id.clone(),
            StoredSyncHead {
                version_id: version.version_id.clone(),
                clock: version.clock.clone(),
                author_replica_id: version.author_replica_id.clone(),
                content: (version.version_id != primary.version_id)
                    .then(|| version.content.clone()),
            },
        );
    }
    Ok(NoteSyncMetadata {
        primary_version_id: primary.version_id.clone(),
        heads,
    })
}

pub(crate) fn choose_primary(versions: &[SyncVersion]) -> Option<&SyncVersion> {
    versions
        .iter()
        .min_by_key(|version| (version.content.trashed, version.version_id.as_str()))
}

pub(crate) fn merge_version(
    note_id: &str,
    heads: &mut Vec<SyncVersion>,
    incoming: SyncVersion,
) -> crate::error::Result<bool> {
    validate_version(note_id, &incoming)?;
    if let Some(existing) = heads
        .iter()
        .find(|version| version.version_id == incoming.version_id)
    {
        if existing == &incoming {
            return Ok(false);
        }
        return Err(invalid_sync_data());
    }
    if heads
        .iter()
        .any(|existing| compare_clocks(&existing.clock, &incoming.clock) == ClockOrder::After)
    {
        return Ok(false);
    }
    heads.retain(|existing| compare_clocks(&incoming.clock, &existing.clock) != ClockOrder::After);
    if heads.len() >= MAX_SYNC_HEADS {
        return Err(crate::error::Error::TooLarge {
            size: (heads.len() + 1) as u64,
            limit: MAX_SYNC_HEADS as u64,
        });
    }
    heads.push(incoming);
    heads.sort_by(|left, right| left.version_id.cmp(&right.version_id));
    Ok(true)
}

pub(crate) fn increment_clock(
    clock: &mut BTreeMap<String, u64>,
    replica_id: &str,
) -> crate::error::Result<()> {
    if canonical_uuid(replica_id).as_deref() != Some(replica_id) {
        return Err(invalid_sync_data());
    }
    if !clock.contains_key(replica_id) && clock.len() >= MAX_CLOCK_REPLICAS {
        return Err(crate::error::Error::TooLarge {
            size: (clock.len() + 1) as u64,
            limit: MAX_CLOCK_REPLICAS as u64,
        });
    }
    let counter = clock.entry(replica_id.to_owned()).or_insert(0);
    *counter = counter.checked_add(1).ok_or_else(invalid_sync_data)?;
    Ok(())
}

pub(crate) fn validate_version(note_id: &str, version: &SyncVersion) -> crate::error::Result<()> {
    if canonical_uuid(note_id).as_deref() != Some(note_id)
        || version.content.id != note_id
        || !valid_sha256(&version.version_id)
    {
        return Err(invalid_sync_data());
    }
    validate_content(&version.content)?;
    validate_clock(&version.clock, &version.author_replica_id)?;
    let expected = make_version(
        version.content.clone(),
        version.clock.clone(),
        version.author_replica_id.clone(),
    )?;
    if expected.version_id != version.version_id {
        return Err(invalid_sync_data());
    }
    Ok(())
}

pub(crate) fn validate_content(content: &SyncContent) -> crate::error::Result<()> {
    if canonical_uuid(&content.id).as_deref() != Some(content.id.as_str())
        || content.title.chars().count() > MAX_TITLE_CHARS
        || content.tasks.len() > MAX_TASKS
        || (content.kind == NotebookKind::Markdown && !content.tasks.is_empty())
        || (content.kind == NotebookKind::Todo && !content.markdown.is_empty())
    {
        return Err(invalid_sync_data());
    }
    let mut task_ids = std::collections::HashSet::with_capacity(content.tasks.len());
    for task in &content.tasks {
        let Some(task_id) = canonical_uuid(&task.id) else {
            return Err(invalid_sync_data());
        };
        if task_id != task.id || task.text.len() > MAX_TASK_TEXT_BYTES || !task_ids.insert(task_id)
        {
            return Err(invalid_sync_data());
        }
    }
    let bytes = serde_json::to_vec(content)?;
    if bytes.len() > MAX_NOTE_BYTES {
        return Err(crate::error::Error::TooLarge {
            size: bytes.len() as u64,
            limit: MAX_NOTE_BYTES as u64,
        });
    }
    Ok(())
}

pub(crate) fn validate_clock(
    clock: &BTreeMap<String, u64>,
    author_replica_id: &str,
) -> crate::error::Result<()> {
    if clock.is_empty()
        || clock.len() > MAX_CLOCK_REPLICAS
        || canonical_uuid(author_replica_id).as_deref() != Some(author_replica_id)
        || !clock.contains_key(author_replica_id)
    {
        return Err(invalid_sync_data());
    }
    for (replica_id, counter) in clock {
        if canonical_uuid(replica_id).as_deref() != Some(replica_id) || *counter == 0 {
            return Err(invalid_sync_data());
        }
    }
    Ok(())
}

pub(crate) fn compare_clocks(
    left: &BTreeMap<String, u64>,
    right: &BTreeMap<String, u64>,
) -> ClockOrder {
    let mut greater = false;
    let mut less = false;
    for key in left.keys().chain(right.keys()) {
        let left_count = left.get(key).copied().unwrap_or(0);
        let right_count = right.get(key).copied().unwrap_or(0);
        greater |= left_count > right_count;
        less |= left_count < right_count;
        if greater && less {
            return ClockOrder::Concurrent;
        }
    }
    match (greater, less) {
        (true, false) => ClockOrder::After,
        (false, true) => ClockOrder::Before,
        (false, false) => ClockOrder::Equal,
        (true, true) => ClockOrder::Concurrent,
    }
}

pub(crate) fn cursor_for(note_id: &str, version_id: &str) -> String {
    format!("{note_id}:{version_id}")
}

pub(crate) fn parse_cursor(cursor: &str) -> Option<(String, String)> {
    let (note_id, version_id) = cursor.split_once(':')?;
    if canonical_uuid(note_id).as_deref() != Some(note_id) || !valid_sha256(version_id) {
        return None;
    }
    Some((note_id.to_owned(), version_id.to_owned()))
}

pub(crate) fn local_router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/notebook/sync/settings",
            get(get_settings).post(put_settings),
        )
        .route("/v1/notebook/sync/status", get(get_sync_status))
        .route("/v1/notebook/sync/run", post(request_sync_run))
        .route("/v1/notebook/{id}/conflicts", get(get_conflicts))
        .route("/v1/notebook/{id}/resolve", post(resolve_conflict))
        .layer(middleware::from_fn(crate::notebook::no_store))
        .layer(middleware::from_fn(crate::notebook::notebook_gate))
}

pub(crate) fn network_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/v1/notebook-sync/records", get(sync_records))
        .layer(middleware::from_fn(crate::notebook::no_store))
        .layer(middleware::from_fn_with_state(state, network_gate))
}

async fn get_settings(State(state): State<AppState>) -> Response {
    match state.store.sync_settings() {
        Ok(settings) => Json(settings).into_response(),
        Err(_) => ApiError::Storage.into_response(),
    }
}

async fn put_settings(State(state): State<AppState>, request: Request) -> Response {
    let request: SettingsRequest = match crate::notebook::parse_json(request).await {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    if request.peer_ids.len() > MAX_SYNC_PEERS {
        return ApiError::Invalid("too_many_peers", "最多可选择 16 台设备").into_response();
    }
    let unique_ids: HashSet<_> = request.peer_ids.iter().collect();
    if unique_ids.len() != request.peer_ids.len()
        || request.peer_ids.iter().any(|id| !valid_node_id(id))
    {
        return ApiError::Invalid("invalid_peers", "设备选择无效").into_response();
    }
    if request.enabled && request.peer_ids.is_empty() {
        return ApiError::Invalid("no_peers_selected", "开启同步前请先选择设备").into_response();
    }
    let auth_ready = state
        .config
        .network
        .auth_token
        .as_deref()
        .is_some_and(|token| token.len() >= 32);
    if request.enabled && !auth_ready {
        return ApiError::Conflict {
            error: "authentication_required",
            message: "自动同步需要至少 32 字节的服务认证令牌",
            current_revision: None,
        }
        .into_response();
    }

    let current_settings = match state.store.sync_settings() {
        Ok(settings) => settings,
        Err(_) => return ApiError::Storage.into_response(),
    };
    let only_previously_discovered = request
        .peer_ids
        .iter()
        .all(|id| current_settings.peer_ids.contains(id));
    if only_previously_discovered {
        let settings = SyncSettings {
            enabled: request.enabled,
            peer_ids: request.peer_ids,
        };
        return match state.store.update_sync_settings(settings.clone()) {
            Ok(()) => Json(settings).into_response(),
            Err(_) => ApiError::Storage.into_response(),
        };
    }

    let status = match bounded_tailscale_status().await {
        Some(status) => status,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "device discovery unavailable",
            )
                .into_response()
        }
    };
    let peers = discovered_peers(&status);
    let own_id = status.this.as_ref().and_then(|node| node.id.as_deref());
    if request
        .peer_ids
        .iter()
        .any(|id| Some(id.as_str()) == own_id || !peers.iter().any(|peer| peer.node_id == *id))
    {
        return ApiError::Invalid("unknown_peer", "设备必须来自当前 Tailscale 发现列表")
            .into_response();
    }

    let settings = SyncSettings {
        enabled: request.enabled,
        peer_ids: request.peer_ids,
    };
    match state.store.update_sync_settings(settings.clone()) {
        Ok(()) => Json(settings).into_response(),
        Err(_) => ApiError::Storage.into_response(),
    }
}

async fn get_sync_status(State(state): State<AppState>) -> Response {
    let persisted = match state.store.sync_state() {
        Ok(value) => value,
        Err(_) => return ApiError::Storage.into_response(),
    };
    let auth_ready = state
        .config
        .network
        .auth_token
        .as_deref()
        .is_some_and(|token| token.len() >= 32);
    let discovery = bounded_tailscale_status().await;
    let peers = discovery.as_ref().map(discovered_peers).unwrap_or_default();
    let mut peer_statuses = Vec::with_capacity(persisted.settings.peer_ids.len());
    for node_id in &persisted.settings.peer_ids {
        let discovered = peers.iter().find(|peer| peer.node_id == *node_id);
        let progress = persisted.peers.get(node_id);
        let state = if !persisted.settings.enabled {
            PeerSyncState::Disabled
        } else if !auth_ready {
            PeerSyncState::Denied
        } else if discovered.is_none_or(|peer| !peer.online) {
            PeerSyncState::Offline
        } else if discovered.is_some_and(|peer| peer.ipv4.is_none()) {
            PeerSyncState::Error
        } else {
            progress.map_or(PeerSyncState::Offline, |progress| progress.state)
        };
        let error = if persisted.settings.enabled && !auth_ready {
            Some("authentication_required".to_string())
        } else if discovery.is_none() {
            Some("discovery_unavailable".to_string())
        } else if discovered.is_some_and(|peer| peer.online && peer.ipv4.is_none()) {
            Some("ambiguous_peer_address".to_string())
        } else {
            progress.and_then(|progress| progress.error.clone())
        };
        peer_statuses.push(PeerStatusResponse {
            node_id: node_id.clone(),
            host: discovered.map(|peer| peer.host.clone()),
            state,
            last_success_at: progress.and_then(|progress| progress.last_success_at),
            error,
        });
    }
    let conflicted_items = match state.store.notebook_conflict_count() {
        Ok(count) => count,
        Err(_) => return ApiError::Storage.into_response(),
    };
    Json(SyncStatusResponse {
        enabled: persisted.settings.enabled,
        ready: persisted.settings.enabled
            && auth_ready
            && !persisted.settings.peer_ids.is_empty()
            && discovery.is_some(),
        needs_authentication: persisted.settings.enabled && !auth_ready,
        peer_ids: persisted.settings.peer_ids,
        conflicted_items,
        peers: peer_statuses,
    })
    .into_response()
}

async fn request_sync_run(State(state): State<AppState>, request: Request) -> Response {
    let _: RunRequest = match crate::notebook::parse_json(request).await {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    if state.store.sync_state().is_err() {
        return ApiError::Storage.into_response();
    }
    state.store.sync_notifier().notify_one();
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "requested": true })),
    )
        .into_response()
}

async fn get_conflicts(
    State(state): State<AppState>,
    path: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
) -> Response {
    let id = match crate::notebook::parse_path_id(path) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let versions = match state.store.notebook_conflict_versions(&id) {
        Ok(Some(versions)) => versions,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(_) => return ApiError::Storage.into_response(),
    };
    let versions = versions
        .into_iter()
        .map(|version| ConflictVersionResponse {
            version_id: version.version_id,
            device_id: version.author_replica_id,
            kind: version.content.kind,
            title: version.content.title,
            markdown: version.content.markdown,
            tasks: version.content.tasks,
            pinned: version.content.pinned,
            trashed: version.content.trashed,
            created_at: version.content.created_at,
            updated_at: version.content.updated_at,
        })
        .collect::<Vec<_>>();
    Json(serde_json::json!({ "versions": versions })).into_response()
}

async fn resolve_conflict(
    State(state): State<AppState>,
    path: std::result::Result<AxumPath<String>, axum::extract::rejection::PathRejection>,
    request: Request,
) -> Response {
    let id = match crate::notebook::parse_path_id(path) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let request: ResolveRequest = match crate::notebook::parse_json(request).await {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    match state
        .store
        .resolve_notebook_conflict(&id, request.expected_revision, &request.version_id)
    {
        Ok(NotebookResolveOutcome::Resolved) => match state.store.get_notebook_snapshot(&id) {
            Ok(Some(snapshot)) => notebook_item_response(snapshot, StatusCode::OK),
            _ => ApiError::Storage.into_response(),
        },
        Ok(NotebookResolveOutcome::NotFound) => ApiError::NotFound.into_response(),
        Ok(NotebookResolveOutcome::Conflict { current_revision }) => ApiError::Conflict {
            error: "revision_conflict",
            message: "笔记已更新，请重新加载冲突版本",
            current_revision: Some(current_revision),
        }
        .into_response(),
        Ok(NotebookResolveOutcome::VersionNotFound) => ApiError::Conflict {
            error: "version_not_found",
            message: "所选版本已不在当前冲突分支中",
            current_revision: None,
        }
        .into_response(),
        Ok(NotebookResolveOutcome::NotConflicted) => ApiError::Conflict {
            error: "not_conflicted",
            message: "笔记当前没有冲突版本",
            current_revision: None,
        }
        .into_response(),
        Err(crate::error::Error::TooLarge { .. }) => ApiError::QuotaExceeded.into_response(),
        Err(_) => ApiError::Storage.into_response(),
    }
}

fn notebook_item_response(snapshot: NotebookSnapshot, status: StatusCode) -> Response {
    #[derive(Serialize)]
    struct ItemResponse<'a> {
        #[serde(flatten)]
        item: &'a NotebookItem,
        version_id: &'a str,
        conflict_count: usize,
    }
    let body = ItemResponse {
        item: &snapshot.item,
        version_id: &snapshot.version_id,
        conflict_count: snapshot.conflict_count,
    };
    (status, Json(body)).into_response()
}

async fn sync_records(
    State(state): State<AppState>,
    query: std::result::Result<Query<FeedQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(query) => query,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid query").into_response(),
    };
    let limit = query.limit.unwrap_or(MAX_SYNC_PAGE_ITEMS);
    if limit == 0 || limit > MAX_SYNC_PAGE_ITEMS {
        return (StatusCode::BAD_REQUEST, "invalid page limit").into_response();
    }
    let after = match query.after.as_deref() {
        Some(value) => match parse_cursor(value) {
            Some(cursor) => Some(cursor),
            None => return (StatusCode::BAD_REQUEST, "invalid cursor").into_response(),
        },
        None => None,
    };
    let mut versions = match state.store.all_notebook_sync_versions() {
        Ok(versions) => versions,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "sync storage error").into_response(),
    };
    versions.retain(|(note_id, version)| {
        after.as_ref().is_none_or(|cursor| {
            (note_id.as_str(), version.version_id.as_str()) > (cursor.0.as_str(), cursor.1.as_str())
        })
    });
    let mut items = Vec::new();
    for (note_id, version) in versions.iter().take(limit) {
        items.push(SyncFeedItem {
            note_id: note_id.clone(),
            versions: vec![version.clone()],
        });
        let last = items.last().and_then(|item| item.versions.first()).unwrap();
        let provisional = SyncFeedResponse {
            protocol: SYNC_PROTOCOL,
            items: items.clone(),
            next_after: Some(cursor_for(note_id, &last.version_id)),
        };
        match serde_json::to_vec(&provisional) {
            Ok(bytes) if bytes.len() <= MAX_SYNC_PAGE_BYTES => {}
            _ => {
                items.pop();
                if items.is_empty() {
                    return (StatusCode::PAYLOAD_TOO_LARGE, "sync version too large")
                        .into_response();
                }
                break;
            }
        }
    }
    let has_more = items.len() < versions.len();
    let next_after = if has_more {
        let Some(last) = items.last() else {
            return (StatusCode::PAYLOAD_TOO_LARGE, "sync version too large").into_response();
        };
        Some(cursor_for(&last.note_id, &last.versions[0].version_id))
    } else {
        None
    };
    let response = SyncFeedResponse {
        protocol: SYNC_PROTOCOL,
        items,
        next_after,
    };
    match serde_json::to_vec(&response) {
        Ok(bytes) if bytes.len() <= MAX_SYNC_PAGE_BYTES => Json(response).into_response(),
        _ => (StatusCode::PAYLOAD_TOO_LARGE, "sync page too large").into_response(),
    }
}

async fn network_gate(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let source = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip());
    #[cfg(test)]
    let injected_status = test_tailscale_status(&request);
    #[cfg(not(test))]
    let injected_status: Option<TailscaleStatus> = None;
    let status = match injected_status {
        Some(status) => Some(status),
        None => bounded_tailscale_status().await,
    };
    let Some(status) = status else {
        return StatusCode::FORBIDDEN.into_response();
    };
    let Some(node_id) = authenticated_peer(
        request.headers(),
        state.config.network.auth_token.as_deref(),
        source,
        &status,
    ) else {
        return StatusCode::FORBIDDEN.into_response();
    };
    match state.store.peer_is_selected(&node_id) {
        Ok(true) => next.run(request).await,
        _ => StatusCode::FORBIDDEN.into_response(),
    }
}

#[cfg(test)]
#[derive(Clone)]
struct TestTailscaleStatus(serde_json::Value);

#[cfg(test)]
fn test_tailscale_status(request: &Request) -> Option<TailscaleStatus> {
    request
        .extensions()
        .get::<TestTailscaleStatus>()
        .and_then(|status| serde_json::from_value(status.0.clone()).ok())
}

fn authenticated_peer(
    headers: &HeaderMap,
    expected_token: Option<&str>,
    source: Option<IpAddr>,
    status: &TailscaleStatus,
) -> Option<String> {
    let expected = expected_token.filter(|token| token.len() >= 32)?;
    let provided = headers
        .get(crate::protocol::TOKEN_HEADER)
        .and_then(|value| value.to_str().ok());
    if provided != Some(expected)
        || headers.contains_key(header::ORIGIN)
        || headers.contains_key("sec-fetch-site")
    {
        return None;
    }
    resolve_peer_for_source(status, source?)
}

async fn bounded_tailscale_status() -> Option<TailscaleStatus> {
    match time::timeout(Duration::from_secs(5), tailscale::status()).await {
        Ok(Ok(status)) => Some(status),
        _ => None,
    }
}

fn resolve_peer_for_source(status: &TailscaleStatus, source: IpAddr) -> Option<String> {
    if !is_tailscale_ip(source) {
        return None;
    }
    let mut matches = status.peers.as_ref()?.values().filter_map(|node| {
        let os = node.os.as_deref().unwrap_or_default();
        if !matches!(os, "macOS" | "windows" | "linux") {
            return None;
        }
        let id = node.id.as_deref()?;
        let found = node
            .tailscale_ips
            .as_ref()?
            .iter()
            .filter_map(|address| IpAddr::from_str(address).ok())
            .any(|address| address == source);
        found.then_some(id)
    });
    let id = matches.next()?;
    if matches.next().is_some() || !valid_node_id(id) {
        return None;
    }
    Some(id.to_owned())
}

fn is_tailscale_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => (u32::from(ip) & 0xffc0_0000) == u32::from(Ipv4Addr::new(100, 64, 0, 0)),
        IpAddr::V6(ip) => ip.octets()[..6] == [0xfd, 0x7a, 0x11, 0x5c, 0xa1, 0xe0],
    }
}

fn valid_node_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control)
}

fn discovered_peers(status: &TailscaleStatus) -> Vec<DiscoveredPeer> {
    let mut peers = status
        .peers
        .as_ref()
        .into_iter()
        .flat_map(|peers| peers.values())
        .filter_map(|node: &Node| {
            if !matches!(node.os.as_deref(), Some("macOS" | "windows" | "linux")) {
                return None;
            }
            let node_id = node.id.clone()?;
            if !valid_node_id(&node_id) {
                return None;
            }
            let ipv4 = node
                .tailscale_ips
                .iter()
                .flatten()
                .filter_map(|value| Ipv4Addr::from_str(value).ok())
                .find(|address| is_tailscale_ip(IpAddr::V4(*address)));
            Some(DiscoveredPeer {
                node_id,
                host: node
                    .host_name
                    .clone()
                    .filter(|host| !host.is_empty())
                    .or_else(|| ipv4.map(|address| address.to_string()))
                    .unwrap_or_else(|| "Tailscale peer".to_string()),
                online: node.online.unwrap_or(false),
                ipv4,
            })
        })
        .collect::<Vec<_>>();
    let mut seen_ids = HashSet::new();
    let mut duplicate_ids = HashSet::new();
    for peer in &peers {
        if !seen_ids.insert(peer.node_id.clone()) {
            duplicate_ids.insert(peer.node_id.clone());
        }
    }
    peers.retain(|peer| !duplicate_ids.contains(&peer.node_id));
    let mut seen_ips = HashSet::new();
    let mut duplicate_ips = HashSet::new();
    for peer in peers.iter().filter_map(|peer| peer.ipv4) {
        if !seen_ips.insert(peer) {
            duplicate_ips.insert(peer);
        }
    }
    for peer in &mut peers {
        if peer.ipv4.is_some_and(|ip| duplicate_ips.contains(&ip)) {
            peer.ipv4 = None;
        }
    }
    peers.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    peers
}

pub(crate) async fn run_worker(config: Arc<Config>, store: Arc<Store>) {
    let client = match reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(8))
        .build()
    {
        Ok(client) => client,
        Err(_) => {
            tracing::error!("notebook sync client could not be initialized");
            return;
        }
    };
    let notifier = store.sync_notifier();
    let mut interval = time::interval(Duration::from_secs(15));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        let forced = tokio::select! {
            _ = interval.tick() => false,
            _ = notifier.notified() => true,
        };

        let state = match store.sync_state() {
            Ok(state) => state,
            Err(_) => continue,
        };
        if !state.settings.enabled || state.settings.peer_ids.is_empty() {
            continue;
        }
        let generation = state.settings_generation;
        let Some(token) = config
            .network
            .auth_token
            .as_deref()
            .filter(|token| token.len() >= 32)
        else {
            continue;
        };

        let discovered = match bounded_tailscale_status().await {
            Some(status) => discovered_peers(&status),
            None => {
                for node_id in &state.settings.peer_ids {
                    set_peer_error(
                        &store,
                        node_id,
                        generation,
                        PeerSyncState::Error,
                        "discovery_unavailable",
                    );
                }
                continue;
            }
        };
        for node_id in &state.settings.peer_ids {
            let current = match store.sync_state() {
                Ok(current)
                    if current.settings.enabled
                        && current.settings_generation == generation
                        && current.settings.peer_ids.iter().any(|id| id == node_id) =>
                {
                    current
                }
                _ => break,
            };
            let Some(peer) = discovered.iter().find(|peer| peer.node_id == *node_id) else {
                set_peer_error(
                    &store,
                    node_id,
                    generation,
                    PeerSyncState::Offline,
                    "peer_unavailable",
                );
                continue;
            };
            if !peer.online {
                set_peer_error(
                    &store,
                    node_id,
                    generation,
                    PeerSyncState::Offline,
                    "peer_offline",
                );
                continue;
            }
            if peer.ipv4.is_none() {
                set_peer_error(
                    &store,
                    node_id,
                    generation,
                    PeerSyncState::Error,
                    "ambiguous_peer_address",
                );
                continue;
            }
            let progress = current.peers.get(node_id).cloned().unwrap_or_default();
            if !forced
                && progress
                    .retry_after
                    .is_some_and(|deadline| deadline > Utc::now())
            {
                continue;
            }
            let _ = store.update_peer_progress(node_id, generation, |progress| {
                progress.state = PeerSyncState::Checking;
                progress.error = None;
            });
            sync_peer_pages(
                &client,
                config.network.port,
                token,
                &store,
                node_id,
                generation,
                peer,
            )
            .await;
        }
    }
}

fn set_peer_error(store: &Store, node_id: &str, generation: u64, state: PeerSyncState, code: &str) {
    let _ = store.update_peer_progress(node_id, generation, |progress| {
        progress.state = state;
        progress.error = Some(code.to_string());
        progress.failures = progress.failures.saturating_add(1);
        let exponent = u32::from(progress.failures.min(7));
        let delay = 1i64.checked_shl(exponent).unwrap_or(120).min(120);
        progress.retry_after = Some(Utc::now() + chrono::Duration::seconds(delay));
    });
}

async fn sync_peer_pages(
    client: &reqwest::Client,
    port: u16,
    token: &str,
    store: &Store,
    node_id: &str,
    generation: u64,
    peer: &DiscoveredPeer,
) {
    let Some(ip) = peer.ipv4 else {
        return;
    };
    let Some(endpoint) = feed_endpoint(ip, port) else {
        set_peer_error(
            store,
            node_id,
            generation,
            PeerSyncState::Error,
            "invalid_peer",
        );
        return;
    };
    sync_peer_pages_at_endpoint(client, endpoint, token, store, node_id, generation).await;
}

async fn sync_peer_pages_at_endpoint(
    client: &reqwest::Client,
    endpoint: reqwest::Url,
    token: &str,
    store: &Store,
    node_id: &str,
    generation: u64,
) {
    for _ in 0..MAX_SYNC_PAGES_PER_RUN {
        let current = match store.sync_state() {
            Ok(current)
                if current.settings.enabled
                    && current.settings_generation == generation
                    && current.settings.peer_ids.iter().any(|id| id == node_id) =>
            {
                current
            }
            _ => return,
        };
        let expected_cursor = current
            .peers
            .get(node_id)
            .and_then(|progress| progress.cursor.clone());
        let url = feed_request_url(endpoint.clone(), expected_cursor.as_deref());
        let response = match client
            .get(url)
            .header(crate::protocol::TOKEN_HEADER, token)
            .send()
            .await
        {
            Ok(response) => response,
            Err(_) => {
                set_peer_error(
                    store,
                    node_id,
                    generation,
                    PeerSyncState::Error,
                    "network_error",
                );
                return;
            }
        };
        match response.status() {
            StatusCode::NOT_FOUND => {
                set_peer_error(
                    store,
                    node_id,
                    generation,
                    PeerSyncState::Unsupported,
                    "unsupported_peer",
                );
                return;
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                set_peer_error(
                    store,
                    node_id,
                    generation,
                    PeerSyncState::Denied,
                    "peer_denied",
                );
                return;
            }
            StatusCode::PAYLOAD_TOO_LARGE => {
                set_peer_error(
                    store,
                    node_id,
                    generation,
                    PeerSyncState::QuotaExceeded,
                    "peer_page_too_large",
                );
                return;
            }
            status if !status.is_success() => {
                set_peer_error(
                    store,
                    node_id,
                    generation,
                    PeerSyncState::Error,
                    "peer_error",
                );
                return;
            }
            _ => {}
        }
        let bytes = match read_bounded_response(response).await {
            Ok(bytes) => bytes,
            Err(ResponseReadError::TooLarge) => {
                set_peer_error(
                    store,
                    node_id,
                    generation,
                    PeerSyncState::QuotaExceeded,
                    "peer_page_too_large",
                );
                return;
            }
            Err(ResponseReadError::Network) => {
                set_peer_error(
                    store,
                    node_id,
                    generation,
                    PeerSyncState::Error,
                    "network_error",
                );
                return;
            }
        };
        let page: SyncFeedResponse = match serde_json::from_slice(&bytes) {
            Ok(page) => page,
            Err(_) => {
                set_peer_error(
                    store,
                    node_id,
                    generation,
                    PeerSyncState::Error,
                    "invalid_response",
                );
                return;
            }
        };
        let versions = match validate_feed_page(&page, expected_cursor.as_deref()) {
            Ok(versions) => versions,
            Err(_) => {
                set_peer_error(
                    store,
                    node_id,
                    generation,
                    PeerSyncState::Error,
                    "invalid_response",
                );
                return;
            }
        };
        if !store.peer_is_selected(node_id).unwrap_or(false) {
            return;
        }
        let completed = page.next_after.is_none();
        let next_cursor = page.next_after.clone();
        match store.apply_sync_page(
            node_id,
            generation,
            expected_cursor.as_deref(),
            next_cursor,
            completed,
            versions,
        ) {
            Ok(SyncApplyOutcome::Applied) => {
                if completed {
                    return;
                }
            }
            Ok(SyncApplyOutcome::SettingsRevoked | SyncApplyOutcome::CursorChanged) => return,
            Err(crate::error::Error::TooLarge { .. }) => {
                set_peer_error(
                    store,
                    node_id,
                    generation,
                    PeerSyncState::QuotaExceeded,
                    "local_quota_exceeded",
                );
                return;
            }
            Err(_) => {
                set_peer_error(
                    store,
                    node_id,
                    generation,
                    PeerSyncState::Error,
                    "merge_failed",
                );
                return;
            }
        }
    }
}

fn feed_endpoint(ip: Ipv4Addr, port: u16) -> Option<reqwest::Url> {
    if port == 0 || !is_tailscale_ip(IpAddr::V4(ip)) {
        return None;
    }
    reqwest::Url::parse(&format!("http://{ip}:{port}/v1/notebook-sync/records")).ok()
}

fn feed_request_url(mut endpoint: reqwest::Url, after: Option<&str>) -> reqwest::Url {
    endpoint.set_query(None);
    let mut query = endpoint.query_pairs_mut();
    query.append_pair("limit", &MAX_SYNC_PAGE_ITEMS.to_string());
    if let Some(after) = after {
        query.append_pair("after", after);
    }
    drop(query);
    endpoint
}

enum ResponseReadError {
    TooLarge,
    Network,
}

async fn read_bounded_response(
    response: reqwest::Response,
) -> std::result::Result<Vec<u8>, ResponseReadError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_SYNC_PAGE_BYTES as u64)
    {
        return Err(ResponseReadError::TooLarge);
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| ResponseReadError::Network)?;
        if bytes.len().saturating_add(chunk.len()) > MAX_SYNC_PAGE_BYTES {
            return Err(ResponseReadError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn validate_feed_page(
    page: &SyncFeedResponse,
    after: Option<&str>,
) -> crate::error::Result<Vec<(String, SyncVersion)>> {
    if page.protocol != SYNC_PROTOCOL || page.items.len() > MAX_SYNC_PAGE_ITEMS {
        return Err(invalid_sync_data());
    }
    let after_key = after.and_then(parse_cursor);
    if after.is_some() && after_key.is_none() {
        return Err(invalid_sync_data());
    }
    let mut versions = Vec::with_capacity(page.items.len());
    let mut previous: Option<(String, String)> = after_key;
    for item in &page.items {
        if item.versions.len() != 1 {
            return Err(invalid_sync_data());
        }
        let version = item.versions[0].clone();
        validate_version(&item.note_id, &version)?;
        let key = (item.note_id.clone(), version.version_id.clone());
        if previous.as_ref().is_some_and(|previous| {
            (key.0.as_str(), key.1.as_str()) <= (previous.0.as_str(), previous.1.as_str())
        }) {
            return Err(invalid_sync_data());
        }
        previous = Some(key);
        versions.push((item.note_id.clone(), version));
    }
    match (&page.next_after, versions.last()) {
        (Some(cursor), Some((note_id, version)))
            if parse_cursor(cursor).as_ref()
                == Some(&(note_id.clone(), version.version_id.clone())) => {}
        (Some(_), _) => return Err(invalid_sync_data()),
        (None, _) => {}
    }
    Ok(versions)
}

fn canonical_uuid(value: &str) -> Option<String> {
    Uuid::parse_str(value)
        .ok()
        .map(|uuid| uuid.hyphenated().to_string())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn invalid_sync_data() -> crate::error::Error {
    crate::error::Error::Storage("invalid notebook sync data".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{NotebookWriteOutcome, Store};
    use crate::{engine::Engine, model::Origin, server::AppState};
    use axum::body::{Body, Bytes};
    use chrono::Utc;
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;

    const TEST_TOKEN: &str = "notebook-sync-network-test-token-32-bytes";
    const FEED_PATH: &str = "/v1/notebook-sync/records";
    type PageHook = Arc<dyn Fn() + Send + Sync>;

    struct TestFeedServer {
        endpoint: reqwest::Url,
        request_count: Arc<AtomicUsize>,
        task: Option<tokio::task::JoinHandle<()>>,
    }

    impl TestFeedServer {
        async fn start(
            store: Arc<Store>,
            source_ip: IpAddr,
            status: serde_json::Value,
            after_first_page: Option<PageHook>,
        ) -> Self {
            let mut config = Config::default();
            config.network.auth_token = Some(TEST_TOKEN.to_string());
            let config = Arc::new(config);
            let origin = Origin {
                host: "notebook-sync-test".to_string(),
                node_id: "notebook-sync-test-node".to_string(),
                addr: "http://127.0.0.1".to_string(),
                os: "test".to_string(),
            };
            let engine =
                Arc::new(Engine::new(config.clone(), store.clone(), origin.clone()).unwrap());
            let state = AppState {
                store,
                config,
                origin,
                engine,
            };
            let request_count = Arc::new(AtomicUsize::new(0));
            let server_request_count = request_count.clone();
            let app = Router::new()
                .merge(network_router(state.clone()))
                .layer(middleware::from_fn(
                    move |mut request: Request, next: Next| {
                        let status = status.clone();
                        let request_count = server_request_count.clone();
                        let after_first_page = after_first_page.clone();
                        async move {
                            let is_feed = request.uri().path() == FEED_PATH;
                            request
                                .extensions_mut()
                                .insert(ConnectInfo(SocketAddr::new(source_ip, 12345)));
                            request.extensions_mut().insert(TestTailscaleStatus(status));
                            let response = next.run(request).await;
                            if is_feed && request_count.fetch_add(1, Ordering::SeqCst) == 0 {
                                if let Some(hook) = after_first_page {
                                    hook();
                                }
                            }
                            response
                        }
                    },
                ))
                .with_state(state);
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            Self {
                endpoint: reqwest::Url::parse(&format!("http://{address}{FEED_PATH}")).unwrap(),
                request_count,
                task: Some(task),
            }
        }

        async fn stop(mut self) {
            if let Some(task) = self.task.take() {
                task.abort();
                let _ = task.await;
            }
        }
    }

    impl Drop for TestFeedServer {
        fn drop(&mut self) {
            if let Some(task) = &self.task {
                task.abort();
            }
        }
    }

    struct RawFeedServer {
        endpoint: reqwest::Url,
        task: Option<tokio::task::JoinHandle<()>>,
    }

    impl RawFeedServer {
        async fn start(status: StatusCode, body: Bytes, stream_body: bool) -> Self {
            let app = Router::new().route(
                FEED_PATH,
                get(move || {
                    let body = body.clone();
                    async move {
                        let body = if stream_body {
                            Body::from_stream(futures_util::stream::iter(vec![
                                Ok::<_, Infallible>(body),
                            ]))
                        } else {
                            Body::from(body)
                        };
                        Response::builder().status(status).body(body).unwrap()
                    }
                }),
            );
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            Self {
                endpoint: reqwest::Url::parse(&format!("http://{address}{FEED_PATH}")).unwrap(),
                task: Some(task),
            }
        }

        async fn stop(mut self) {
            if let Some(task) = self.task.take() {
                task.abort();
                let _ = task.await;
            }
        }
    }

    impl Drop for RawFeedServer {
        fn drop(&mut self) {
            if let Some(task) = &self.task {
                task.abort();
            }
        }
    }

    fn source_status(node_id: &str, source_ip: IpAddr) -> serde_json::Value {
        serde_json::json!({
            "Peer": {
                "test-source": {
                    "ID": node_id,
                    "OS": "linux",
                    "TailscaleIPs": [source_ip.to_string()],
                    "Online": true
                }
            }
        })
    }

    fn test_http_client() -> reqwest::Client {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap()
    }

    fn test_store_at() -> (Store, std::path::PathBuf) {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/session-notebook-sync")
            .join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("state.redb");
        (Store::open_isolated_for_tests(&path).unwrap(), path)
    }

    fn test_store() -> Store {
        test_store_at().0
    }

    fn item(kind: NotebookKind, title: &str) -> NotebookItem {
        NotebookItem {
            id: Uuid::new_v4().hyphenated().to_string(),
            kind,
            title: title.to_string(),
            markdown: String::new(),
            tasks: if kind == NotebookKind::Todo {
                vec![NotebookTask {
                    id: Uuid::new_v4().hyphenated().to_string(),
                    text: "sync this task".to_string(),
                    done: false,
                }]
            } else {
                Vec::new()
            },
            pinned: false,
            trashed: false,
            revision: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version_id: None,
            conflict_count: 0,
        }
    }

    fn select_peer(store: &Store, node_id: &str) -> u64 {
        store
            .update_sync_settings(SyncSettings {
                enabled: true,
                peer_ids: vec![node_id.to_string()],
            })
            .unwrap();
        store.sync_state().unwrap().settings_generation
    }

    fn versions_for(store: &Store, id: &str) -> Vec<SyncVersion> {
        store
            .all_notebook_sync_versions()
            .unwrap()
            .into_iter()
            .filter_map(|(note_id, version)| (note_id == id).then_some(version))
            .collect()
    }

    fn exchange_one(destination: &Store, source_node: &str, id: &str, version: SyncVersion) {
        let generation = select_peer(destination, source_node);
        assert_eq!(
            destination
                .apply_sync_page(
                    source_node,
                    generation,
                    None,
                    None,
                    true,
                    vec![(id.to_string(), version)],
                )
                .unwrap(),
            SyncApplyOutcome::Applied
        );
    }

    fn local_edit(store: &Store, id: &str, edit: impl FnOnce(&mut NotebookItem)) {
        let snapshot = store.get_notebook_snapshot(id).unwrap().unwrap();
        let expected = snapshot.item.revision;
        let mut item = snapshot.item;
        edit(&mut item);
        item.revision += 1;
        item.updated_at = Utc::now();
        assert_eq!(
            store
                .update_notebook_local(&item, expected, 1_000, 32 * 1024 * 1024)
                .unwrap(),
            NotebookWriteOutcome::Stored
        );
    }

    #[test]
    fn todo_edit_trash_restore_exchange_is_idempotent_and_persists() {
        let (first, first_path) = test_store_at();
        let second = test_store();
        let created = item(NotebookKind::Todo, "shared todo");
        let id = created.id.clone();
        assert_eq!(
            first
                .create_notebook_local(&created, 1_000, 32 * 1024 * 1024)
                .unwrap(),
            NotebookWriteOutcome::Stored
        );
        exchange_one(
            &second,
            "node-first",
            &id,
            versions_for(&first, &id)[0].clone(),
        );

        local_edit(&second, &id, |item| item.tasks[0].done = true);
        exchange_one(
            &first,
            "node-second",
            &id,
            versions_for(&second, &id)[0].clone(),
        );
        let first_revision = first
            .get_notebook_snapshot(&id)
            .unwrap()
            .unwrap()
            .item
            .revision;
        assert!(
            first
                .get_notebook_snapshot(&id)
                .unwrap()
                .unwrap()
                .item
                .tasks[0]
                .done
        );
        exchange_one(
            &first,
            "node-second",
            &id,
            versions_for(&second, &id)[0].clone(),
        );
        assert_eq!(
            first
                .get_notebook_snapshot(&id)
                .unwrap()
                .unwrap()
                .item
                .revision,
            first_revision,
            "idempotent sync must not create a local edit"
        );

        local_edit(&first, &id, |item| item.trashed = true);
        exchange_one(
            &second,
            "node-first",
            &id,
            versions_for(&first, &id)[0].clone(),
        );
        assert!(
            second
                .get_notebook_snapshot(&id)
                .unwrap()
                .unwrap()
                .item
                .trashed
        );
        local_edit(&second, &id, |item| item.trashed = false);
        exchange_one(
            &first,
            "node-second",
            &id,
            versions_for(&second, &id)[0].clone(),
        );
        let snapshot = first.get_notebook_snapshot(&id).unwrap().unwrap();
        assert!(!snapshot.item.trashed);
        assert_eq!(snapshot.conflict_count, 0);

        let sync_state_before_restart = first.sync_state().unwrap();
        drop(first);
        let reopened = Store::open_isolated_for_tests(&first_path).unwrap();
        let persisted = reopened.get_notebook_snapshot(&id).unwrap().unwrap();
        assert!(!persisted.item.trashed);
        assert_eq!(persisted.conflict_count, 0);
        assert!(persisted.item.tasks[0].done);
        assert_eq!(reopened.sync_state().unwrap(), sync_state_before_restart);
    }

    #[test]
    fn concurrent_branches_require_resolution_and_late_old_version_cannot_resurrect() {
        let first = test_store();
        let second = test_store();
        let created = item(NotebookKind::Markdown, "base");
        let id = created.id.clone();
        first
            .create_notebook_local(&created, 1_000, 32 * 1024 * 1024)
            .unwrap();
        exchange_one(
            &second,
            "node-first",
            &id,
            versions_for(&first, &id)[0].clone(),
        );
        local_edit(&first, &id, |item| item.title = "first branch".to_string());
        let old_first_branch = versions_for(&first, &id)[0].clone();
        local_edit(&second, &id, |item| {
            item.title = "second branch".to_string()
        });
        let second_branch = versions_for(&second, &id)[0].clone();
        exchange_one(&first, "node-second", &id, second_branch.clone());

        let conflict = first.get_notebook_snapshot(&id).unwrap().unwrap();
        assert_eq!(conflict.conflict_count, 1);
        let mut attempted = conflict.item.clone();
        attempted.title = "must not overwrite conflict".to_string();
        attempted.revision += 1;
        assert!(matches!(
            first
                .update_notebook_local(&attempted, conflict.item.revision, 1_000, 32 * 1024 * 1024)
                .unwrap(),
            NotebookWriteOutcome::SyncConflict { .. }
        ));
        assert_eq!(
            first
                .resolve_notebook_conflict(&id, conflict.item.revision, &second_branch.version_id)
                .unwrap(),
            NotebookResolveOutcome::Resolved
        );
        let resolved = first.get_notebook_snapshot(&id).unwrap().unwrap();
        assert_eq!(resolved.item.title, "second branch");
        assert_eq!(resolved.conflict_count, 0);
        let before_late = resolved.item.revision;
        exchange_one(&first, "node-second", &id, old_first_branch);
        let after_late = first.get_notebook_snapshot(&id).unwrap().unwrap();
        assert_eq!(after_late.item.revision, before_late);
        assert_eq!(after_late.item.title, "second branch");
        assert_eq!(after_late.conflict_count, 0);
    }

    #[test]
    fn concurrent_conflict_resolutions_commit_at_most_once() {
        let first = Arc::new(test_store());
        let second = test_store();
        let created = item(NotebookKind::Markdown, "resolve race base");
        let id = created.id.clone();
        first
            .create_notebook_local(&created, 1_000, 32 * 1024 * 1024)
            .unwrap();
        exchange_one(
            &second,
            "node-first",
            &id,
            versions_for(&first, &id)[0].clone(),
        );
        local_edit(&first, &id, |item| item.title = "first branch".to_string());
        local_edit(&second, &id, |item| {
            item.title = "second branch".to_string()
        });
        exchange_one(
            &first,
            "node-second",
            &id,
            versions_for(&second, &id)[0].clone(),
        );
        let conflict = first.get_notebook_snapshot(&id).unwrap().unwrap();
        assert_eq!(conflict.conflict_count, 1);
        let revision = conflict.item.revision;
        let version_ids = first
            .notebook_conflict_versions(&id)
            .unwrap()
            .unwrap()
            .into_iter()
            .map(|version| version.version_id)
            .collect::<Vec<_>>();
        assert_eq!(version_ids.len(), 2);

        let barrier = Arc::new(std::sync::Barrier::new(version_ids.len() + 1));
        let workers = version_ids
            .into_iter()
            .map(|version_id| {
                let store = first.clone();
                let barrier = barrier.clone();
                let id = id.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .resolve_notebook_conflict(&id, revision, &version_id)
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let outcomes = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == NotebookResolveOutcome::Resolved)
                .count(),
            1
        );
        let resolved = first.get_notebook_snapshot(&id).unwrap().unwrap();
        assert_eq!(resolved.conflict_count, 0);
        assert_eq!(resolved.item.revision, revision + 1);
        assert!(matches!(
            resolved.item.title.as_str(),
            "first branch" | "second branch"
        ));
    }

    #[test]
    fn settings_generation_revokes_an_in_flight_page_after_reselection() {
        let store = test_store();
        let item = item(NotebookKind::Markdown, "in flight");
        let id = item.id.clone();
        let replica = Uuid::new_v4().hyphenated().to_string();
        let version = make_version(
            content_from_item(&item),
            BTreeMap::from([(replica.clone(), 1)]),
            replica,
        )
        .unwrap();
        let old_generation = select_peer(&store, "node-source");
        store
            .update_sync_settings(SyncSettings {
                enabled: false,
                peer_ids: Vec::new(),
            })
            .unwrap();
        let current_generation = select_peer(&store, "node-source");
        assert!(current_generation > old_generation);
        assert_eq!(
            store
                .apply_sync_page(
                    "node-source",
                    old_generation,
                    None,
                    None,
                    true,
                    vec![(id.clone(), version)],
                )
                .unwrap(),
            SyncApplyOutcome::SettingsRevoked
        );
        assert!(store.get_notebook_snapshot(&id).unwrap().is_none());
    }

    #[test]
    fn feed_gate_binds_authentication_to_one_discovered_tailnet_ip() {
        let status: TailscaleStatus = serde_json::from_value(serde_json::json!({
            "Self": { "ID": "self", "TailscaleIPs": ["100.64.0.1"] },
            "Peer": {
                "one": { "ID": "node-one", "OS": "linux", "TailscaleIPs": ["100.64.0.2"], "Online": true }
            }
        }))
        .unwrap();
        let token = "x".repeat(32);
        let mut headers = HeaderMap::new();
        headers.insert(crate::protocol::TOKEN_HEADER, token.parse().unwrap());
        let source = Some(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2)));
        assert_eq!(
            authenticated_peer(&headers, Some(&token), source, &status).as_deref(),
            Some("node-one")
        );
        assert!(authenticated_peer(&headers, Some("short"), source, &status).is_none());
        assert!(authenticated_peer(
            &headers,
            Some(&token),
            Some(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))),
            &status
        )
        .is_none());
        headers.insert(header::ORIGIN, "http://peer.example".parse().unwrap());
        assert!(authenticated_peer(&headers, Some(&token), source, &status).is_none());
        headers.remove(header::ORIGIN);
        headers.insert("sec-fetch-site", "same-origin".parse().unwrap());
        assert!(authenticated_peer(&headers, Some(&token), source, &status).is_none());

        let ambiguous: TailscaleStatus = serde_json::from_value(serde_json::json!({
            "Peer": {
                "one": { "ID": "node-one", "OS": "linux", "TailscaleIPs": ["100.64.0.2"], "Online": true },
                "two": { "ID": "node-two", "OS": "windows", "TailscaleIPs": ["100.64.0.2"], "Online": true }
            }
        }))
        .unwrap();
        headers.remove("sec-fetch-site");
        assert!(authenticated_peer(&headers, Some(&token), source, &ambiguous).is_none());
    }

    #[test]
    fn feed_validation_rejects_bad_hashes_and_non_advancing_cursors() {
        let item = item(NotebookKind::Markdown, "feed");
        let replica = Uuid::new_v4().hyphenated().to_string();
        let version = initial_version(&item, &replica).unwrap();
        let cursor = cursor_for(&item.id, &version.version_id);
        let valid = SyncFeedResponse {
            protocol: SYNC_PROTOCOL,
            items: vec![SyncFeedItem {
                note_id: item.id.clone(),
                versions: vec![version.clone()],
            }],
            next_after: Some(cursor.clone()),
        };
        assert!(validate_feed_page(&valid, None).is_ok());
        assert!(validate_feed_page(&valid, Some(&cursor)).is_err());
        let mut invalid = valid;
        let version_id = &mut invalid.items[0].versions[0].version_id;
        let replacement = if version_id.starts_with('f') {
            "0"
        } else {
            "f"
        };
        version_id.replace_range(..1, replacement);
        invalid.next_after = Some(cursor_for(
            &item.id,
            &invalid.items[0].versions[0].version_id,
        ));
        assert!(validate_feed_page(&invalid, None).is_err());
    }

    #[tokio::test]
    async fn network_router_binds_feed_access_to_token_source_and_selection() {
        let (store, db_path) = test_store_at();
        let root = db_path.parent().unwrap().to_path_buf();
        let store = Arc::new(store);
        select_peer(&store, "selected-peer");
        let tailnet_ip = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 3));
        let server = TestFeedServer::start(
            store.clone(),
            tailnet_ip,
            source_status("selected-peer", tailnet_ip),
            None,
        )
        .await;
        let client = test_http_client();

        assert_eq!(
            client
                .get(server.endpoint.clone())
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN,
            "missing token must be rejected"
        );
        for token in ["wrong-token", "short"] {
            assert_eq!(
                client
                    .get(server.endpoint.clone())
                    .header(crate::protocol::TOKEN_HEADER, token)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            client
                .get(server.endpoint.clone())
                .header(crate::protocol::TOKEN_HEADER, TEST_TOKEN)
                .header(header::ORIGIN, "https://untrusted.example")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            client
                .get(server.endpoint.clone())
                .header(crate::protocol::TOKEN_HEADER, TEST_TOKEN)
                .header("sec-fetch-site", "same-origin")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            client
                .get(format!("{}?after=not-a-cursor", server.endpoint))
                .header(crate::protocol::TOKEN_HEADER, TEST_TOKEN)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            client
                .get(format!("{}?node_id=selected-peer", server.endpoint))
                .header(crate::protocol::TOKEN_HEADER, TEST_TOKEN)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST,
            "self-reported identity is not a supported feed parameter"
        );

        store.update_sync_settings(SyncSettings::default()).unwrap();
        assert_eq!(
            client
                .get(server.endpoint.clone())
                .header(crate::protocol::TOKEN_HEADER, TEST_TOKEN)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN,
            "disabled sync must reject inbound reads"
        );
        select_peer(&store, "different-peer");
        assert_eq!(
            client
                .get(server.endpoint.clone())
                .header(crate::protocol::TOKEN_HEADER, TEST_TOKEN)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN,
            "an unselected source must not read the feed"
        );
        server.stop().await;

        select_peer(&store, "selected-peer");
        let spoofed = TestFeedServer::start(
            store.clone(),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9)),
            source_status("selected-peer", tailnet_ip),
            None,
        )
        .await;
        assert_eq!(
            client
                .get(format!("{}?node_id=selected-peer", spoofed.endpoint))
                .header(crate::protocol::TOKEN_HEADER, TEST_TOKEN)
                .header("x-peercarry-node-id", "selected-peer")
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN,
            "a self-reported node id cannot replace the source-IP identity"
        );
        spoofed.stop().await;
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn network_pages_round_trip_between_isolated_replicas_and_rescan_source_changes() {
        let (sender, sender_path) = test_store_at();
        let sender_root = sender_path.parent().unwrap().to_path_buf();
        let sender = Arc::new(sender);
        let (receiver, receiver_path) = test_store_at();
        let receiver_root = receiver_path.parent().unwrap().to_path_buf();
        let receiver = Arc::new(receiver);
        let mut sent_items = Vec::new();
        for index in 0..(MAX_SYNC_PAGE_ITEMS + 1) {
            let note = item(NotebookKind::Markdown, &format!("network page {index}"));
            sender
                .create_notebook_local(&note, 1_000, 32 * 1024 * 1024)
                .unwrap();
            sent_items.push(note);
        }
        let changed_id = sender.all_notebook_sync_versions().unwrap()[0].0.clone();
        select_peer(&sender, "receiver-node");
        let generation = select_peer(&receiver, "sender-node");
        let sender_for_hook = sender.clone();
        let changed_id_for_hook = changed_id.clone();
        let hook: PageHook = Arc::new(move || {
            let snapshot = sender_for_hook
                .get_notebook_snapshot(&changed_id_for_hook)
                .unwrap()
                .unwrap();
            let mut changed = snapshot.item;
            changed.title = "changed after the first encoded page".to_string();
            changed.revision += 1;
            changed.updated_at = Utc::now();
            assert_eq!(
                sender_for_hook
                    .update_notebook_local(&changed, changed.revision - 1, 1_000, 32 * 1024 * 1024,)
                    .unwrap(),
                NotebookWriteOutcome::Stored
            );
        });
        let source_ip = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 3));
        let server = TestFeedServer::start(
            sender.clone(),
            source_ip,
            source_status("receiver-node", source_ip),
            Some(hook),
        )
        .await;
        let client = test_http_client();

        sync_peer_pages_at_endpoint(
            &client,
            server.endpoint.clone(),
            TEST_TOKEN,
            &receiver,
            "sender-node",
            generation,
        )
        .await;
        let first_pass = receiver.sync_state().unwrap().peers["sender-node"].clone();
        assert_eq!(first_pass.state, PeerSyncState::Checked);
        assert_eq!(first_pass.cursor, None);
        assert_eq!(
            receiver.list_notebook_snapshots().unwrap().len(),
            MAX_SYNC_PAGE_ITEMS + 1
        );
        assert_ne!(
            receiver
                .get_notebook_snapshot(&changed_id)
                .unwrap()
                .unwrap()
                .item
                .title,
            "changed after the first encoded page"
        );

        // A completed scan restarts from the beginning next time, so an edit
        // made behind the first page cursor is picked up on the following run.
        sync_peer_pages_at_endpoint(
            &client,
            server.endpoint.clone(),
            TEST_TOKEN,
            &receiver,
            "sender-node",
            generation,
        )
        .await;
        let synced = receiver
            .get_notebook_snapshot(&changed_id)
            .unwrap()
            .unwrap();
        assert_eq!(synced.item.title, "changed after the first encoded page");
        assert_eq!(versions_for(&receiver, &changed_id).len(), 1);
        assert!(receiver.sync_state().unwrap().peers["sender-node"]
            .last_success_at
            .is_some());
        assert!(server.request_count.load(Ordering::SeqCst) >= 4);

        server.stop().await;
        drop(receiver);
        drop(sender);
        std::fs::remove_dir_all(receiver_root).unwrap();
        std::fs::remove_dir_all(sender_root).unwrap();
    }

    #[tokio::test]
    async fn network_page_revoked_while_in_flight_is_not_merged() {
        let (sender, sender_path) = test_store_at();
        let sender_root = sender_path.parent().unwrap().to_path_buf();
        let sender = Arc::new(sender);
        let (receiver, receiver_path) = test_store_at();
        let receiver_root = receiver_path.parent().unwrap().to_path_buf();
        let receiver = Arc::new(receiver);
        let note = item(NotebookKind::Markdown, "revoked during request");
        let note_id = note.id.clone();
        sender
            .create_notebook_local(&note, 1_000, 32 * 1024 * 1024)
            .unwrap();
        select_peer(&sender, "receiver-node");
        let generation = select_peer(&receiver, "sender-node");
        let receiver_for_hook = receiver.clone();
        let hook: PageHook = Arc::new(move || {
            receiver_for_hook
                .update_sync_settings(SyncSettings::default())
                .unwrap();
        });
        let source_ip = IpAddr::V4(Ipv4Addr::new(100, 64, 0, 3));
        let server = TestFeedServer::start(
            sender.clone(),
            source_ip,
            source_status("receiver-node", source_ip),
            Some(hook),
        )
        .await;

        sync_peer_pages_at_endpoint(
            &test_http_client(),
            server.endpoint.clone(),
            TEST_TOKEN,
            &receiver,
            "sender-node",
            generation,
        )
        .await;
        assert!(!receiver.sync_state().unwrap().settings.enabled);
        assert!(receiver.get_notebook_snapshot(&note_id).unwrap().is_none());
        assert!(receiver
            .sync_state()
            .unwrap()
            .peers
            .get("sender-node")
            .is_none_or(|progress| progress.cursor.is_none()));

        server.stop().await;
        drop(receiver);
        drop(sender);
        std::fs::remove_dir_all(receiver_root).unwrap();
        std::fs::remove_dir_all(sender_root).unwrap();
    }

    #[tokio::test]
    async fn old_peer_404_can_retry_and_streaming_response_size_is_bounded() {
        let (store, db_path) = test_store_at();
        let root = db_path.parent().unwrap().to_path_buf();
        let store = Arc::new(store);
        let generation = select_peer(&store, "old-peer");
        let client = test_http_client();

        let old_peer = RawFeedServer::start(StatusCode::NOT_FOUND, Bytes::new(), false).await;
        sync_peer_pages_at_endpoint(
            &client,
            old_peer.endpoint.clone(),
            TEST_TOKEN,
            &store,
            "old-peer",
            generation,
        )
        .await;
        let failed = store.sync_state().unwrap().peers["old-peer"].clone();
        assert_eq!(failed.state, PeerSyncState::Unsupported);
        assert_eq!(failed.error.as_deref(), Some("unsupported_peer"));
        assert_eq!(failed.failures, 1);
        assert!(failed.retry_after.is_some());
        assert!(failed.cursor.is_none());
        old_peer.stop().await;

        let empty_page = SyncFeedResponse {
            protocol: SYNC_PROTOCOL,
            items: Vec::new(),
            next_after: None,
        };
        let retry = RawFeedServer::start(
            StatusCode::OK,
            Bytes::from(serde_json::to_vec(&empty_page).unwrap()),
            false,
        )
        .await;
        sync_peer_pages_at_endpoint(
            &client,
            retry.endpoint.clone(),
            TEST_TOKEN,
            &store,
            "old-peer",
            generation,
        )
        .await;
        let recovered = store.sync_state().unwrap().peers["old-peer"].clone();
        assert_eq!(recovered.state, PeerSyncState::Checked);
        assert_eq!(recovered.failures, 0);
        assert!(recovered.retry_after.is_none());
        retry.stop().await;

        let oversized = RawFeedServer::start(
            StatusCode::OK,
            Bytes::from(vec![b'x'; MAX_SYNC_PAGE_BYTES + 1]),
            true,
        )
        .await;
        sync_peer_pages_at_endpoint(
            &client,
            oversized.endpoint.clone(),
            TEST_TOKEN,
            &store,
            "old-peer",
            generation,
        )
        .await;
        let bounded = store.sync_state().unwrap().peers["old-peer"].clone();
        assert_eq!(bounded.state, PeerSyncState::QuotaExceeded);
        assert_eq!(bounded.error.as_deref(), Some("peer_page_too_large"));
        assert!(bounded.cursor.is_none());
        assert!(store.list_notebook_snapshots().unwrap().is_empty());
        oversized.stop().await;
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn invalid_backward_and_capacity_pages_do_not_advance_or_partially_merge() {
        let client = test_http_client();
        let (invalid_store, invalid_path) = test_store_at();
        let invalid_root = invalid_path.parent().unwrap().to_path_buf();
        let invalid_store = Arc::new(invalid_store);
        let generation = select_peer(&invalid_store, "source-node");

        let mut first = item(NotebookKind::Markdown, "first in invalid page");
        first.id = "00000000-0000-4000-8000-000000000001".to_string();
        let mut second = item(NotebookKind::Markdown, "bad hash in page");
        second.id = "00000000-0000-4000-8000-000000000002".to_string();
        let replica = Uuid::new_v4().hyphenated().to_string();
        let first_version = initial_version(&first, &replica).unwrap();
        let mut invalid_version = initial_version(&second, &replica).unwrap();
        let replacement = if invalid_version.version_id.starts_with('a') {
            'b'
        } else {
            'a'
        };
        invalid_version
            .version_id
            .replace_range(..1, &replacement.to_string());
        let invalid_page = SyncFeedResponse {
            protocol: SYNC_PROTOCOL,
            items: vec![
                SyncFeedItem {
                    note_id: first.id.clone(),
                    versions: vec![first_version],
                },
                SyncFeedItem {
                    note_id: second.id.clone(),
                    versions: vec![invalid_version.clone()],
                },
            ],
            next_after: Some(cursor_for(&second.id, &invalid_version.version_id)),
        };
        let invalid_server = RawFeedServer::start(
            StatusCode::OK,
            Bytes::from(serde_json::to_vec(&invalid_page).unwrap()),
            false,
        )
        .await;
        sync_peer_pages_at_endpoint(
            &client,
            invalid_server.endpoint.clone(),
            TEST_TOKEN,
            &invalid_store,
            "source-node",
            generation,
        )
        .await;
        assert!(invalid_store.list_notebook_snapshots().unwrap().is_empty());
        let invalid_progress = invalid_store.sync_state().unwrap().peers["source-node"].clone();
        assert!(invalid_progress.cursor.is_none());
        assert_eq!(invalid_progress.error.as_deref(), Some("invalid_response"));
        invalid_server.stop().await;
        drop(invalid_store);
        std::fs::remove_dir_all(invalid_root).unwrap();

        let (backward_store, backward_path) = test_store_at();
        let backward_root = backward_path.parent().unwrap().to_path_buf();
        let backward_store = Arc::new(backward_store);
        let baseline = item(NotebookKind::Markdown, "baseline");
        backward_store
            .create_notebook_local(&baseline, 1_000, 32 * 1024 * 1024)
            .unwrap();
        let baseline_version = versions_for(&backward_store, &baseline.id)[0].clone();
        let generation = select_peer(&backward_store, "source-node");
        let baseline_cursor = cursor_for(&baseline.id, &baseline_version.version_id);
        assert_eq!(
            backward_store
                .apply_sync_page(
                    "source-node",
                    generation,
                    None,
                    Some(baseline_cursor.clone()),
                    false,
                    vec![(baseline.id.clone(), baseline_version.clone())],
                )
                .unwrap(),
            SyncApplyOutcome::Applied
        );
        let backward_page = SyncFeedResponse {
            protocol: SYNC_PROTOCOL,
            items: vec![SyncFeedItem {
                note_id: baseline.id.clone(),
                versions: vec![baseline_version],
            }],
            next_after: Some(baseline_cursor.clone()),
        };
        let backward_server = RawFeedServer::start(
            StatusCode::OK,
            Bytes::from(serde_json::to_vec(&backward_page).unwrap()),
            false,
        )
        .await;
        sync_peer_pages_at_endpoint(
            &client,
            backward_server.endpoint.clone(),
            TEST_TOKEN,
            &backward_store,
            "source-node",
            generation,
        )
        .await;
        let progress = backward_store.sync_state().unwrap().peers["source-node"].clone();
        assert_eq!(progress.cursor.as_deref(), Some(baseline_cursor.as_str()));
        assert_eq!(progress.error.as_deref(), Some("invalid_response"));
        assert_eq!(backward_store.list_notebook_snapshots().unwrap().len(), 1);
        backward_server.stop().await;
        drop(backward_store);
        std::fs::remove_dir_all(backward_root).unwrap();

        let (capacity_store, capacity_path) = test_store_at();
        let capacity_root = capacity_path.parent().unwrap().to_path_buf();
        let capacity_store = Arc::new(capacity_store);
        let mut baseline = item(NotebookKind::Markdown, "cursor anchor");
        baseline.id = "00000000-0000-4000-8000-000000000001".to_string();
        let mut conflicted = item(NotebookKind::Markdown, "head capacity");
        conflicted.id = "00000000-0000-4000-8000-000000000002".to_string();
        for note in [&baseline, &conflicted] {
            capacity_store
                .create_notebook_local(note, 1_000, 32 * 1024 * 1024)
                .unwrap();
        }
        let generation = select_peer(&capacity_store, "source-node");
        let conflicted_content = content_from_item(&conflicted);
        for _ in 0..(MAX_SYNC_HEADS - 1) {
            let replica = Uuid::new_v4().hyphenated().to_string();
            let version = make_version(
                conflicted_content.clone(),
                BTreeMap::from([(replica.clone(), 1)]),
                replica,
            )
            .unwrap();
            capacity_store
                .apply_sync_page(
                    "source-node",
                    generation,
                    None,
                    None,
                    true,
                    vec![(conflicted.id.clone(), version)],
                )
                .unwrap();
        }
        let baseline_version = versions_for(&capacity_store, &baseline.id)[0].clone();
        let baseline_cursor = cursor_for(&baseline.id, &baseline_version.version_id);
        capacity_store
            .apply_sync_page(
                "source-node",
                generation,
                None,
                Some(baseline_cursor.clone()),
                false,
                vec![(baseline.id.clone(), baseline_version)],
            )
            .unwrap();
        assert_eq!(
            versions_for(&capacity_store, &conflicted.id).len(),
            MAX_SYNC_HEADS
        );

        let overflow_replica = Uuid::new_v4().hyphenated().to_string();
        let overflow_version = make_version(
            conflicted_content,
            BTreeMap::from([(overflow_replica.clone(), 1)]),
            overflow_replica,
        )
        .unwrap();
        let overflow_cursor = cursor_for(&conflicted.id, &overflow_version.version_id);
        let capacity_page = SyncFeedResponse {
            protocol: SYNC_PROTOCOL,
            items: vec![SyncFeedItem {
                note_id: conflicted.id.clone(),
                versions: vec![overflow_version],
            }],
            next_after: Some(overflow_cursor),
        };
        let capacity_server = RawFeedServer::start(
            StatusCode::OK,
            Bytes::from(serde_json::to_vec(&capacity_page).unwrap()),
            false,
        )
        .await;
        sync_peer_pages_at_endpoint(
            &client,
            capacity_server.endpoint.clone(),
            TEST_TOKEN,
            &capacity_store,
            "source-node",
            generation,
        )
        .await;
        let progress = capacity_store.sync_state().unwrap().peers["source-node"].clone();
        assert_eq!(progress.state, PeerSyncState::QuotaExceeded);
        assert_eq!(progress.error.as_deref(), Some("local_quota_exceeded"));
        assert_eq!(progress.cursor.as_deref(), Some(baseline_cursor.as_str()));
        assert_eq!(
            versions_for(&capacity_store, &conflicted.id).len(),
            MAX_SYNC_HEADS
        );
        assert_eq!(
            capacity_store
                .get_notebook_snapshot(&conflicted.id)
                .unwrap()
                .unwrap()
                .conflict_count,
            MAX_SYNC_HEADS - 1
        );
        capacity_server.stop().await;
        drop(capacity_store);
        std::fs::remove_dir_all(capacity_root).unwrap();
    }

    #[test]
    fn equal_vector_with_different_content_is_preserved_as_a_conflict() {
        let original = item(NotebookKind::Markdown, "equal clock");
        let replica = Uuid::new_v4().hyphenated().to_string();
        let clock = BTreeMap::from([(replica.clone(), 1)]);
        let first =
            make_version(content_from_item(&original), clock.clone(), replica.clone()).unwrap();
        let mut divergent = original.clone();
        divergent.title = "different content".to_string();
        let second = make_version(content_from_item(&divergent), clock, replica).unwrap();
        let mut heads = vec![first];
        assert!(merge_version(&original.id, &mut heads, second).unwrap());
        assert_eq!(heads.len(), 2);
        assert_eq!(choose_primary(&heads).unwrap().content.id, original.id);
    }

    #[test]
    fn clock_and_head_limits_reject_without_dropping_existing_versions() {
        let content = content_from_item(&item(NotebookKind::Markdown, "bounded"));
        let replicas = (0..MAX_CLOCK_REPLICAS + 1)
            .map(|_| Uuid::new_v4().hyphenated().to_string())
            .collect::<Vec<_>>();
        let clock = replicas
            .iter()
            .cloned()
            .map(|replica| (replica, 1))
            .collect::<BTreeMap<_, _>>();
        assert!(validate_clock(&clock, &replicas[0]).is_err());

        let mut heads = Vec::new();
        let mut overflow = None;
        for index in 0..=MAX_SYNC_HEADS {
            let replica = Uuid::new_v4().hyphenated().to_string();
            let version = make_version(
                content.clone(),
                BTreeMap::from([(replica.clone(), 1)]),
                replica,
            )
            .unwrap();
            match merge_version(&content.id, &mut heads, version.clone()) {
                Ok(true) => {}
                Err(error) => overflow = Some(error),
                other => panic!("unexpected merge result at branch {index}: {other:?}"),
            }
        }
        assert!(overflow.is_some());
        assert_eq!(heads.len(), MAX_SYNC_HEADS);
        for head in &heads {
            validate_version(&content.id, head).unwrap();
        }
    }

    #[tokio::test]
    async fn worker_task_can_be_cancelled_cleanly() {
        let store = Arc::new(test_store());
        let worker = tokio::spawn(run_worker(Arc::new(Config::default()), store));
        tokio::task::yield_now().await;
        worker.abort();
        assert!(worker.await.unwrap_err().is_cancelled());
    }
}
