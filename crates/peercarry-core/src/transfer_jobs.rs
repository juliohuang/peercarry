//! Bounded, persistent user initiated transfer queue.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    model::{Entry, EntryKind},
    paths,
    server::AppState,
};

const MAX_JOBS: usize = 128;
const MAX_QUEUE_BYTES: u64 = 16 * 1024 * 1024;
const DEFAULT_TTL: u64 = 86_400;
const MIN_TTL: u64 = 60;
const MAX_TTL: u64 = 7 * 86_400;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Send,
    Download,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    WaitingForPeer,
    Paused,
    Completed,
    Failed,
    Cancelled,
    Expired,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileResult {
    pub index: usize,
    pub name: String,
    pub status: String,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferJob {
    pub id: String,
    pub operation: Operation,
    pub entry_id: String,
    pub node_id: Option<String>,
    pub target_name: Option<String>,
    pub entry_kind: EntryKind,
    pub status: JobStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub files: Vec<FileResult>,
    #[serde(skip)]
    pub entry: Option<Entry>,
    #[serde(default)]
    pub retry_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub generation: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRequest {
    operation: Operation,
    #[serde(default)]
    node_id: Option<String>,
    #[serde(default)]
    entry_id: Option<String>,
    #[serde(default)]
    expires_in_secs: Option<u64>,
}
#[derive(Serialize, Deserialize)]
struct DiskState {
    jobs: Vec<TransferJob>,
    entries: HashMap<String, Entry>,
}
#[derive(Clone)]
struct Service {
    app: AppState,
    root: PathBuf,
    state: Arc<Mutex<DiskState>>,
    cancels: Arc<Mutex<HashMap<String, CancellationToken>>>,
    recovery_error: Option<String>,
    storage_error: Arc<Mutex<Option<String>>>,
}

pub fn router(app: AppState) -> Router<AppState> {
    let root = app
        .config
        .storage
        .data_dir
        .clone()
        .unwrap_or_else(paths::data_dir)
        .join("transfer-jobs");
    router_with_dir(app, root)
}

/// Explicit storage-root variant for isolated tests and embedding.
pub fn router_with_dir(app: AppState, root: PathBuf) -> Router<AppState> {
    let (disk, recovery_error) = match load(&root) {
        Ok(mut disk) => {
            for j in &mut disk.jobs {
                if j.status == JobStatus::Running {
                    j.status = if j.operation == Operation::Download {
                        JobStatus::Paused
                    } else {
                        JobStatus::Queued
                    };
                }
            }
            if let Err(e) = persist(&root, &disk) {
                (disk, Some(format!("cannot persist recovered queue: {e}")))
            } else {
                (disk, None)
            }
        }
        Err(e) => {
            tracing::error!("transfer queue recovery failed: {e}");
            (
                DiskState {
                    jobs: vec![],
                    entries: HashMap::new(),
                },
                Some(e.to_string()),
            )
        }
    };
    let svc = Service {
        app: app.clone(),
        root,
        state: Arc::new(Mutex::new(disk)),
        cancels: Arc::new(Mutex::new(HashMap::new())),
        recovery_error,
        storage_error: Arc::new(Mutex::new(None)),
    };
    tokio::spawn(worker(svc.clone()));
    Router::new()
        .route("/v1/transfers", get(list).post(create))
        .route("/v1/transfers/{id}", get(detail).delete(remove))
        .route("/v1/transfers/{id}/cancel", post(cancel))
        .route("/v1/transfers/{id}/retry", post(retry))
        .with_state(svc)
        .layer(middleware::from_fn(crate::ai_monitor::local_gate))
}

fn persist(root: &Path, state: &DiskState) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(state).map_err(std::io::Error::other)?;
    if bytes.len() as u64 > MAX_QUEUE_BYTES {
        return Err(std::io::Error::other(
            "transfer queue storage limit exceeded",
        ));
    }
    std::fs::create_dir_all(root)?;
    let temp = root.join("jobs.json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&temp)?;
    use std::io::Write;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(temp, root.join("jobs.json"))
}
fn load(root: &Path) -> std::io::Result<DiskState> {
    let path = root.join("jobs.json");
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > MAX_QUEUE_BYTES {
            return Err(std::io::Error::other(
                "transfer queue file exceeds storage limit",
            ));
        }
    }
    let bytes = match std::fs::read(path) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DiskState {
                jobs: vec![],
                entries: HashMap::new(),
            })
        }
        Err(e) => return Err(e),
    };
    let mut state: DiskState = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
    state.jobs.truncate(MAX_JOBS);
    Ok(state)
}
fn snapshot(j: &TransferJob) -> TransferJob {
    let mut j = j.clone();
    j.entry = None;
    j
}
fn storage_failed(s: &Service) -> Option<Response> {
    let error = storage_error(s);
    error.map(|e| {
        fail(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("transfer storage unavailable: {e}"),
        )
    })
}
fn storage_error(s: &Service) -> Option<String> {
    s.recovery_error
        .clone()
        .or_else(|| s.storage_error.lock().ok().and_then(|x| x.clone()))
}
fn disable_storage(s: &Service, e: &std::io::Error) {
    if let Ok(mut state) = s.storage_error.lock() {
        *state = Some(e.to_string());
    }
}
fn fail<T: Into<String>>(status: StatusCode, message: T) -> Response {
    (status, Json(json!({"error":message.into()}))).into_response()
}
async fn list(State(s): State<Service>) -> Response {
    if let Some(r) = storage_failed(&s) {
        return r;
    }
    match s.state.lock() {
        Ok(state) => Json(state.jobs.iter().map(snapshot).collect::<Vec<_>>()).into_response(),
        Err(_) => fail(StatusCode::INTERNAL_SERVER_ERROR, "queue lock unavailable"),
    }
}
async fn detail(State(s): State<Service>, AxumPath(id): AxumPath<String>) -> Response {
    if let Some(r) = storage_failed(&s) {
        return r;
    }
    match s.state.lock() {
        Ok(state) => state
            .jobs
            .iter()
            .find(|j| j.id == id)
            .map(|j| Json(snapshot(j)).into_response())
            .unwrap_or_else(|| fail(StatusCode::NOT_FOUND, "transfer not found")),
        Err(_) => fail(StatusCode::INTERNAL_SERVER_ERROR, "queue lock unavailable"),
    }
}
async fn create(State(s): State<Service>, Json(req): Json<CreateRequest>) -> Response {
    if let Some(r) = storage_failed(&s) {
        return r;
    }
    let ttl = req.expires_in_secs.unwrap_or(DEFAULT_TTL);
    if !(MIN_TTL..=MAX_TTL).contains(&ttl) {
        return fail(
            StatusCode::BAD_REQUEST,
            "expires_in_secs must be 60..604800",
        );
    }
    let mut discovered_target = None;
    let entry = match req.operation {
        Operation::Send => {
            if req.node_id.as_deref().is_none_or(str::is_empty) {
                return fail(StatusCode::BAD_REQUEST, "send requires node_id");
            }
            let discovered = match tokio::time::timeout(
                Duration::from_secs(12),
                crate::tailscale::peers(s.app.config.network.port),
            )
            .await
            {
                Ok(Ok(p)) => p,
                Ok(Err(e)) => return fail(StatusCode::BAD_GATEWAY, e.to_string()),
                Err(_) => return fail(StatusCode::GATEWAY_TIMEOUT, "device discovery timeout"),
            };
            let mut matches = discovered
                .iter()
                .filter(|p| Some(p.node_id.as_str()) == req.node_id.as_deref());
            let Some(selected) = matches.next() else {
                return fail(
                    StatusCode::NOT_FOUND,
                    "node_id must exactly match one discovered device",
                );
            };
            if matches.next().is_some() {
                return fail(
                    StatusCode::NOT_FOUND,
                    "node_id must exactly match one discovered device",
                );
            }
            discovered_target = Some(selected.host.clone());
            if let Some(id) = req.entry_id.as_deref() {
                match s.app.engine.find(id).await {
                    Ok(Some(e)) => e,
                    Ok(None) => return fail(StatusCode::NOT_FOUND, "entry not found"),
                    Err(e) => return fail(StatusCode::BAD_GATEWAY, e.to_string()),
                }
            } else {
                match s.app.engine.capture().await {
                    Ok(c) => c.entry,
                    Err(e) => return fail(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()),
                }
            }
        }
        Operation::Download => {
            if req.node_id.is_some() {
                return fail(StatusCode::BAD_REQUEST, "download does not accept node_id");
            }
            let Some(id) = req.entry_id.as_deref() else {
                return fail(StatusCode::BAD_REQUEST, "download requires entry_id");
            };
            match s.app.engine.find(id).await {
                Ok(Some(e)) if e.kind == EntryKind::Files && !e.origin.node_id.is_empty() && e.origin.node_id != s.app.origin.node_id => e,
                Ok(Some(e)) if e.kind == EntryKind::Files => return fail(StatusCode::UNPROCESSABLE_ENTITY,"this file entry has no verifiable remote source; use the original file on this device"),
                Ok(Some(_)) => {
                    return fail(
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "entry is not a Files entry",
                    )
                }
                Ok(None) => return fail(StatusCode::NOT_FOUND, "entry not found"),
                Err(e) => return fail(StatusCode::BAD_GATEWAY, e.to_string()),
            }
        }
    };
    let now = Utc::now();
    let id = Uuid::new_v4().to_string();
    let target_name = if req.operation == Operation::Send {
        discovered_target.or_else(|| req.node_id.clone())
    } else {
        Some(entry.origin.host.clone())
    };
    let job = TransferJob {
        id: id.clone(),
        operation: req.operation,
        entry_id: entry.id.clone(),
        node_id: req.node_id,
        target_name,
        entry_kind: entry.kind,
        status: JobStatus::Queued,
        created_at: now,
        updated_at: now,
        expires_at: now + chrono::Duration::seconds(ttl as i64),
        error: None,
        files: entry
            .files
            .as_deref()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(index, f)| FileResult {
                index,
                name: f.name.clone(),
                status: "pending".into(),
                bytes: 0,
                total_bytes: Some(f.size),
                path: None,
                error: None,
            })
            .collect(),
        entry: Some(entry.clone()),
        retry_at: None,
        attempts: 0,
        generation: 0,
    };
    let mut state = match s.state.lock() {
        Ok(v) => v,
        Err(_) => return fail(StatusCode::INTERNAL_SERVER_ERROR, "queue lock unavailable"),
    };
    if state.jobs.len() >= MAX_JOBS {
        return fail(StatusCode::TOO_MANY_REQUESTS, "transfer queue is full");
    }
    state.entries.insert(id.clone(), entry);
    state.jobs.push(job.clone());
    if let Err(e) = persist(&s.root, &state) {
        disable_storage(&s, &e);
        state.jobs.pop();
        state.entries.remove(&id);
        return fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot persist transfer: {e}"),
        );
    }
    drop(state);
    (StatusCode::ACCEPTED, Json(snapshot(&job))).into_response()
}
async fn remove(State(s): State<Service>, AxumPath(id): AxumPath<String>) -> Response {
    if let Some(r) = storage_failed(&s) {
        return r;
    }
    let mut state = match s.state.lock() {
        Ok(v) => v,
        Err(_) => return fail(StatusCode::INTERNAL_SERVER_ERROR, "queue lock unavailable"),
    };
    let Some(i) = state.jobs.iter().position(|j| j.id == id) else {
        return fail(StatusCode::NOT_FOUND, "transfer not found");
    };
    if !matches!(
        state.jobs[i].status,
        JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled | JobStatus::Expired
    ) {
        return fail(
            StatusCode::CONFLICT,
            "only terminal transfers can be deleted",
        );
    }
    if s.cancels.lock().is_ok_and(|c| c.contains_key(&id)) {
        return fail(StatusCode::CONFLICT, "transfer is still stopping");
    }
    let old = state.jobs.remove(i);
    let old_entry = state.entries.remove(&id);
    if let Err(e) = persist(&s.root, &state) {
        disable_storage(&s, &e);
        state.jobs.insert(i, old);
        if let Some(e) = old_entry {
            state.entries.insert(id, e);
        }
        return fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    StatusCode::NO_CONTENT.into_response()
}
async fn cancel(State(s): State<Service>, AxumPath(id): AxumPath<String>) -> Response {
    if let Some(r) = storage_failed(&s) {
        return r;
    }
    let mut state = match s.state.lock() {
        Ok(v) => v,
        Err(_) => return fail(StatusCode::INTERNAL_SERVER_ERROR, "queue lock unavailable"),
    };
    let Some(i) = state.jobs.iter().position(|j| j.id == id) else {
        return fail(StatusCode::NOT_FOUND, "transfer not found");
    };
    if matches!(
        state.jobs[i].status,
        JobStatus::Completed | JobStatus::Failed | JobStatus::Cancelled | JobStatus::Expired
    ) {
        return fail(StatusCode::CONFLICT, "transfer is terminal");
    }
    let old = state.jobs[i].clone();
    let j = &mut state.jobs[i];
    j.status = JobStatus::Cancelled;
    j.updated_at = Utc::now();
    j.generation = j.generation.saturating_add(1);
    j.error = Some("cancelled by user".into());
    if let Err(e) = persist(&s.root, &state) {
        disable_storage(&s, &e);
        state.jobs[i] = old;
        return fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    let result = snapshot(&state.jobs[i]);
    drop(state);
    if let Ok(c) = s.cancels.lock() {
        if let Some(t) = c.get(&id) {
            t.cancel();
        }
    }
    Json(result).into_response()
}
async fn retry(State(s): State<Service>, AxumPath(id): AxumPath<String>) -> Response {
    if let Some(r) = storage_failed(&s) {
        return r;
    }
    let mut state = match s.state.lock() {
        Ok(v) => v,
        Err(_) => return fail(StatusCode::INTERNAL_SERVER_ERROR, "queue lock unavailable"),
    };
    let Some(i) = state.jobs.iter().position(|j| j.id == id) else {
        return fail(StatusCode::NOT_FOUND, "transfer not found");
    };
    if s.cancels.lock().is_ok_and(|c| c.contains_key(&id)) {
        return fail(StatusCode::CONFLICT, "previous attempt is still stopping");
    }
    let j = &mut state.jobs[i];
    if j.status == JobStatus::Completed {
        return fail(StatusCode::CONFLICT, "completed transfer cannot be retried");
    }
    if !matches!(
        j.status,
        JobStatus::Failed | JobStatus::Cancelled | JobStatus::Paused | JobStatus::WaitingForPeer
    ) {
        return Json(snapshot(j)).into_response();
    }
    if j.expires_at <= Utc::now() {
        return fail(StatusCode::GONE, "transfer expired");
    }
    let old = j.clone();
    j.status = JobStatus::Queued;
    j.error = None;
    j.retry_at = None;
    j.updated_at = Utc::now();
    j.generation = j.generation.saturating_add(1);
    for f in &mut j.files {
        if f.status != "completed" {
            f.status = "pending".into();
            f.error = None;
        }
    }
    if let Err(e) = persist(&s.root, &state) {
        disable_storage(&s, &e);
        state.jobs[i] = old;
        return fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
    }
    Json(snapshot(&state.jobs[i])).into_response()
}

async fn worker(s: Service) {
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if storage_error(&s).is_some() {
            continue;
        }
        let selected = {
            let mut st = match s.state.lock() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let now = Utc::now();
            let mut changed = false;
            for j in &mut st.jobs {
                if !matches!(
                    j.status,
                    JobStatus::Completed
                        | JobStatus::Failed
                        | JobStatus::Cancelled
                        | JobStatus::Expired
                ) && j.expires_at <= now
                {
                    j.status = JobStatus::Expired;
                    j.updated_at = now;
                    j.error = Some("transfer expired".into());
                    changed = true;
                }
            }
            if changed {
                if let Err(e) = persist(&s.root, &st) {
                    disable_storage(&s, &e);
                    continue;
                }
            }
            let pick = st.jobs.iter().position(|j| {
                j.status == JobStatus::Queued
                    || (j.status == JobStatus::WaitingForPeer
                        && j.retry_at.is_none_or(|t| t <= now))
            });
            pick.and_then(|i| {
                let old = st.jobs[i].clone();
                let j = &mut st.jobs[i];
                j.status = JobStatus::Running;
                j.updated_at = now;
                j.attempts = j.attempts.saturating_add(1);
                j.generation = j.generation.saturating_add(1);
                let copy = j.clone();
                let task_id = j.id.clone();
                let _ = j;
                let e = st.entries.get(&task_id).cloned();
                if let Err(err) = persist(&s.root, &st) {
                    if let Some(current) = st.jobs.get_mut(i) {
                        *current = old;
                    }
                    disable_storage(&s, &err);
                    return None;
                }
                Some((copy, e))
            })
        };
        let Some((job, entry)) = selected else {
            continue;
        };
        let Some(_activity) = crate::maintenance::enter() else {
            if let Ok(mut st) = s.state.lock() {
                if let Some(j) = st.jobs.iter_mut().find(|x| x.id == job.id) {
                    if j.status == JobStatus::Running {
                        j.status = JobStatus::Queued;
                        j.updated_at = Utc::now();
                        if let Err(e) = persist(&s.root, &st) {
                            disable_storage(&s, &e);
                        }
                    }
                }
            }
            continue;
        };
        let token = CancellationToken::new();
        if let Ok(mut c) = s.cancels.lock() {
            c.insert(job.id.clone(), token.clone());
        }
        let stale = if let Ok(st) = s.state.lock() {
            st.jobs
                .iter()
                .find(|x| x.id == job.id)
                .is_none_or(|x| x.status != JobStatus::Running || x.generation != job.generation)
        } else {
            true
        };
        if stale {
            token.cancel();
        }
        let deadline = tokio::time::Instant::now()
            + job
                .expires_at
                .signed_duration_since(Utc::now())
                .to_std()
                .unwrap_or_default();
        let expiry_cancel = token.clone();
        let expiry_task = tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            expiry_cancel.cancel();
        });
        let result = if let Some(entry) = entry {
            execute(&s, &job, &entry, &token).await
        } else {
            Err((false, "captured transfer snapshot missing".into()))
        };
        expiry_task.abort();
        if let Ok(mut c) = s.cancels.lock() {
            c.remove(&job.id);
        }
        let mut st = match s.state.lock() {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(j) = st.jobs.iter_mut().find(|j| j.id == job.id) {
            if j.status != JobStatus::Running || j.generation != job.generation {
                continue;
            }
            j.updated_at = Utc::now();
            if j.expires_at <= Utc::now() {
                j.status = JobStatus::Expired;
                j.error = Some("transfer expired".into());
            } else {
                match result {
                    Ok(()) => {
                        j.status = JobStatus::Completed;
                        j.error = None
                    }
                    Err((retryable, msg)) => {
                        j.error = Some(msg);
                        if retryable && j.expires_at > Utc::now() {
                            j.status = JobStatus::WaitingForPeer;
                            let delay = 2u64.saturating_pow(j.attempts.min(10)).min(1800);
                            j.retry_at = Some(Utc::now() + chrono::Duration::seconds(delay as i64));
                        } else {
                            j.status = JobStatus::Failed;
                        }
                    }
                }
            }
        }
        if let Err(e) = persist(&s.root, &st) {
            disable_storage(&s, &e);
        }
    }
}
type JobError = (bool, String);
fn retryable_peer_error(error: &crate::error::Error) -> bool {
    match error {
        crate::error::Error::PeerUnreachable { .. } => true,
        crate::error::Error::Network(message) => {
            let message = message.to_ascii_lowercase();
            [
                "connect",
                "connection refused",
                "connection reset",
                "timed out",
                "timeout",
                "temporarily unavailable",
            ]
            .iter()
            .any(|needle| message.contains(needle))
        }
        _ => false,
    }
}
async fn execute(
    s: &Service,
    j: &TransferJob,
    e: &Entry,
    cancel: &CancellationToken,
) -> Result<(), JobError> {
    match j.operation {
        Operation::Send => {
            if cancel.is_cancelled() {
                return Err((false, "cancelled".into()));
            }
            let peers = tokio::time::timeout(
                Duration::from_secs(12),
                crate::tailscale::peers(s.app.config.network.port),
            )
            .await
            .map_err(|_| (true, "device discovery timeout".into()))?
            .map_err(|e| (true, e.to_string()))?;
            let id = j.node_id.as_deref().unwrap_or("");
            let mut found = peers.iter().filter(|p| p.node_id == id);
            let Some(peer) = found.next() else {
                return Err((true, "selected device is not currently discovered".into()));
            };
            if found.next().is_some() {
                return Err((false, "selected device id is ambiguous".into()));
            }
            if !peer.online {
                return Err((true, "selected device is offline".into()));
            }
            let addr = peer
                .addr_with_port(s.app.config.network.port)
                .ok_or((true, "selected device address unavailable".into()))?;
            if cancel.is_cancelled() {
                return Err((false, "cancelled".into()));
            }
            let hello =
                tokio::time::timeout(Duration::from_secs(8), s.app.engine.client.hello(&addr))
                    .await
                    .map_err(|_| (true, "target identity check timeout".into()))?
                    .map_err(|x| (retryable_peer_error(&x), x.to_string()))?;
            if hello.node_id != peer.node_id {
                return Err((false, "target identity mismatch".into()));
            }
            if let Ok(mut st) = s.state.lock() {
                if let Some(current) = st.jobs.iter_mut().find(|x| x.id == j.id) {
                    current.target_name = Some(peer.host.clone());
                    current.updated_at = Utc::now();
                }
            }
            if cancel.is_cancelled() {
                return Err((false, "cancelled".into()));
            }
            tokio::select! { _=cancel.cancelled()=>return Err((false,"cancelled".into())), outcome=tokio::time::timeout(Duration::from_secs(30), s.app.engine.client.push(&addr,e))=>outcome.map_err(|_|(true,"send timeout".into()))? }
                .map_err(|err| (retryable_peer_error(&err),err.to_string()))
        }
        Operation::Download => download(s, j, e, cancel).await,
    }
}
async fn download(
    s: &Service,
    j: &TransferJob,
    e: &Entry,
    cancel: &CancellationToken,
) -> Result<(), JobError> {
    if cancel.is_cancelled() {
        return Err((false, "cancelled".into()));
    }
    let dir = s.app.engine.download_dir();
    let files = e
        .files
        .as_deref()
        .ok_or((false, "files metadata missing".into()))?;
    // Stored file paths are never opened. Only a currently discovered remote
    // source with a verified node identity can provide the indexed payload.
    if e.origin.node_id.is_empty() || e.origin.node_id == s.app.origin.node_id {
        return Err((
            false,
            "this file entry has no verifiable remote source".into(),
        ));
    }
    let source = {
        let id = e.origin.node_id.as_str();
        let peers = tokio::time::timeout(
            Duration::from_secs(12),
            crate::tailscale::peers(s.app.config.network.port),
        )
        .await
        .map_err(|_| (true, "device discovery timeout".into()))?
        .map_err(|x| (true, x.to_string()))?;
        let mut matches = peers.iter().filter(|p| p.node_id == id);
        let Some(p) = matches.next() else {
            return Err((true, "source device is not currently discovered".into()));
        };
        if matches.next().is_some() {
            return Err((false, "source device id is ambiguous".into()));
        };
        if !p.online {
            return Err((true, "source device is offline".into()));
        };
        p.clone()
    };
    for (index, _f) in files.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err((false, "cancelled".into()));
        }
        let prior = j.files.get(index);
        if prior.is_some_and(|x| x.status == "completed")
            && prior
                .and_then(|x| x.path.as_deref())
                .is_some_and(|p| Path::new(p).exists())
        {
            continue;
        }
        {
            let p = &source;
            let hello = tokio::time::timeout(
                Duration::from_secs(8),
                s.app.engine.client.hello(
                    p.addr_with_port(s.app.config.network.port)
                        .as_deref()
                        .unwrap_or(""),
                ),
            )
            .await
            .map_err(|_| (true, "source identity check timeout".into()))?
            .map_err(|x| (retryable_peer_error(&x), x.to_string()))?;
            if hello.node_id != p.node_id {
                return Err((false, "source identity mismatch".into()));
            }
            let addr = p
                .addr_with_port(s.app.config.network.port)
                .ok_or((true, "source address unavailable".into()))?;
            let source_entries = tokio::time::timeout(
                Duration::from_secs(12),
                s.app.engine.client.list(&addr, 200),
            )
            .await
            .map_err(|_| (true, "source entry verification timeout".into()))?
            .map_err(|x| (retryable_peer_error(&x), x.to_string()))?;
            let Some(source_entry) = source_entries.iter().find(|x| x.id == e.id) else {
                return Err((false, "selected source does not hold this entry".into()));
            };
            if source_entry.origin.node_id != p.node_id
                || source_entry.kind != EntryKind::Files
                || source_entry
                    .files
                    .as_ref()
                    .is_none_or(|xs| xs.len() != files.len())
            {
                return Err((
                    false,
                    "source entry identity or file metadata mismatch".into(),
                ));
            }
            if cancel.is_cancelled() {
                return Err((false, "cancelled".into()));
            }
            let svc = s.clone();
            let job_id = j.id.clone();
            let cb = move |bytes, total| update_progress(&svc, &job_id, index, bytes, total);
            let path = tokio::time::timeout(
                Duration::from_secs(3600),
                s.app.engine.client.download_file_observed(
                    &addr,
                    &e.id,
                    index,
                    &dir,
                    Some(&cb),
                    Some(cancel),
                ),
            )
            .await
            .map_err(|_| (false, "file download timed out; retry manually".into()))?
            .map_err(|x| (retryable_peer_error(&x), x.to_string()))?;
            if !path.exists() {
                return Err((false, "download output disappeared".into()));
            }
            if !update_file_result(s, &j.id, index, Some(path), "completed", None) {
                return Err((false, "cannot persist completed file result".into()));
            }
            if cancel.is_cancelled() {
                return Err((false, "cancelled; completed files were retained".into()));
            }
        }
    }
    Ok(())
}
fn update_progress(s: &Service, id: &str, index: usize, bytes: u64, total: Option<u64>) {
    if let Ok(mut state) = s.state.lock() {
        if let Some(job) = state.jobs.iter_mut().find(|j| j.id == id) {
            if let Some(file) = job.files.get_mut(index) {
                file.status = "downloading".into();
                file.bytes = bytes;
                file.total_bytes = total;
                job.updated_at = Utc::now();
            }
        }
    }
}
fn update_file_result(
    s: &Service,
    id: &str,
    index: usize,
    path: Option<PathBuf>,
    status: &str,
    error: Option<String>,
) -> bool {
    if let Ok(mut state) = s.state.lock() {
        if let Some(job) = state.jobs.iter_mut().find(|j| j.id == id) {
            if let Some(file) = job.files.get_mut(index) {
                file.status = status.into();
                file.path = path.map(|p| p.to_string_lossy().into_owned());
                file.error = error;
                job.updated_at = Utc::now();
                if let Err(e) = persist(&s.root, &state) {
                    disable_storage(s, &e);
                    return false;
                }
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_job_store_write_fails_without_publishing_file() {
        let root = std::env::temp_dir().join(format!("peercarry-job-limit-{}", Uuid::new_v4()));
        let entry = Entry {
            id: "oversized".into(),
            kind: EntryKind::Text,
            origin: crate::model::Origin {
                host: "test".into(),
                node_id: "test".into(),
                addr: String::new(),
                os: String::new(),
            },
            created_at: Utc::now(),
            text: Some("x".repeat(MAX_QUEUE_BYTES as usize)),
            blob: None,
            thumb: None,
            files: None,
            preview: String::new(),
            pinned: false,
            size: MAX_QUEUE_BYTES,
            holder: None,
        };
        let state = DiskState {
            jobs: Vec::new(),
            entries: HashMap::from([("task".into(), entry)]),
        };
        assert!(persist(&root, &state).is_err());
        assert!(!root.join("jobs.json").exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
