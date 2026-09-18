//! Local AI hook events.
//!
//! Events contain only bounded metadata. Prompts, tool arguments, source code,
//! and command output deliberately never enter this store.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use axum::extract::{rejection::JsonRejection, ConnectInfo, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use chrono::{SecondsFormat, Utc};
use redb::{Database, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::paths;
use crate::protocol;
use crate::server::AppState;

const MAX_EVENTS: usize = 1000;
const MAX_BODY: usize = 16 * 1024;
const TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("ai_events");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiEvent {
    pub id: String,
    pub sequence: u64,
    pub tool: String,
    pub session_id: String,
    pub project: Option<String>,
    pub hook: String,
    pub timestamp: String,
    pub received_at: String,
    pub node: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AiEventInput {
    pub id: String,
    pub tool: String,
    pub session_id: String,
    pub project: Option<String>,
    pub hook: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AiEventError {
    #[error("invalid event: {0}")]
    Invalid(&'static str),
    #[error("event storage failed: {0}")]
    Storage(String),
}

type DbHandle = Arc<Mutex<Database>>;
static DATABASES: OnceLock<Mutex<HashMap<PathBuf, DbHandle>>> = OnceLock::new();

fn database(path: &Path) -> Result<DbHandle, AiEventError> {
    let map = DATABASES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut entries = map
        .lock()
        .map_err(|_| AiEventError::Storage("database registry poisoned".into()))?;
    if let Some(db) = entries.get(path) {
        return Ok(db.clone());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| AiEventError::Storage(e.to_string()))?;
    }
    let db = Database::create(path).map_err(|e| AiEventError::Storage(e.to_string()))?;
    let handle = Arc::new(Mutex::new(db));
    entries.insert(path.to_path_buf(), handle.clone());
    Ok(handle)
}

fn validate(input: &AiEventInput) -> Result<(), AiEventError> {
    if !matches!(input.tool.as_str(), "codex" | "zcode") {
        return Err(AiEventError::Invalid("tool must be codex or zcode"));
    }
    if input.session_id.is_empty()
        || input.session_id.len() > 256
        || input.session_id.chars().any(char::is_control)
    {
        return Err(AiEventError::Invalid("session_id must be 1..256 bytes"));
    }
    if !matches!(
        input.hook.as_str(),
        "UserPromptSubmit"
            | "Stop"
            | "PermissionRequest"
            | "PostToolUse"
            | "PostToolUseFailure"
            | "Interrupt"
            | "SessionEnd"
    ) {
        return Err(AiEventError::Invalid("unsupported hook"));
    }
    if input
        .project
        .as_ref()
        .is_some_and(|p| p.len() > 512 || p.chars().any(char::is_control))
    {
        return Err(AiEventError::Invalid("project is too long"));
    }
    Ok(())
}

fn basename(project: Option<&str>) -> Option<String> {
    project.and_then(|p| {
        Path::new(p)
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
    })
}

/// Insert an event, atomically deduplicating by its caller supplied UUID.
pub fn ingest_event(
    path: &Path,
    input: AiEventInput,
    node: impl Into<String>,
) -> Result<AiEvent, AiEventError> {
    validate(&input)?;
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let id = Uuid::parse_str(&input.id).map_err(|_| AiEventError::Invalid("id must be a UUID"))?;
    let mut event = AiEvent {
        id: id.to_string(),
        sequence: 0,
        tool: input.tool,
        session_id: input.session_id,
        project: basename(input.project.as_deref()),
        hook: input.hook,
        timestamp: timestamp.clone(),
        received_at: timestamp,
        node: node.into(),
    };
    let db = database(path)?;
    let db = db
        .lock()
        .map_err(|_| AiEventError::Storage("database lock poisoned".into()))?;
    let tx = db
        .begin_write()
        .map_err(|e| AiEventError::Storage(e.to_string()))?;
    {
        let mut table = tx
            .open_table(TABLE)
            .map_err(|e| AiEventError::Storage(e.to_string()))?;
        let key = event.id.to_string();
        if let Some(existing) = table
            .get(key.as_str())
            .map_err(|e| AiEventError::Storage(e.to_string()))?
        {
            return serde_json::from_slice(existing.value())
                .map_err(|e| AiEventError::Storage(e.to_string()));
        }
        for item in table
            .iter()
            .map_err(|e| AiEventError::Storage(e.to_string()))?
        {
            let (_, value) = item.map_err(|e| AiEventError::Storage(e.to_string()))?;
            let prior: AiEvent = serde_json::from_slice(value.value())
                .map_err(|e| AiEventError::Storage(e.to_string()))?;
            event.sequence = event.sequence.max(prior.sequence);
        }
        event.sequence = event.sequence.saturating_add(1);
        let bytes = serde_json::to_vec(&event).map_err(|e| AiEventError::Storage(e.to_string()))?;
        table
            .insert(key.as_str(), bytes.as_slice())
            .map_err(|e| AiEventError::Storage(e.to_string()))?;
    }
    tx.commit()
        .map_err(|e| AiEventError::Storage(e.to_string()))?;
    drop(db);
    prune(path)?;
    Ok(event)
}

fn prune(path: &Path) -> Result<(), AiEventError> {
    let db = database(path)?;
    let db = db
        .lock()
        .map_err(|_| AiEventError::Storage("database lock poisoned".into()))?;
    let tx = db
        .begin_write()
        .map_err(|e| AiEventError::Storage(e.to_string()))?;
    let mut all = Vec::new();
    {
        let table = tx
            .open_table(TABLE)
            .map_err(|e| AiEventError::Storage(e.to_string()))?;
        for item in table
            .iter()
            .map_err(|e| AiEventError::Storage(e.to_string()))?
        {
            let (key, value) = item.map_err(|e| AiEventError::Storage(e.to_string()))?;
            let event: AiEvent = serde_json::from_slice(value.value())
                .map_err(|e| AiEventError::Storage(e.to_string()))?;
            all.push((event.sequence, key.value().to_owned()));
        }
    }
    if all.len() > MAX_EVENTS {
        all.sort_by(|a, b| a.0.cmp(&b.0));
        let mut table = tx
            .open_table(TABLE)
            .map_err(|e| AiEventError::Storage(e.to_string()))?;
        let remove_count = all.len() - MAX_EVENTS;
        for (_, key) in all.into_iter().take(remove_count) {
            table
                .remove(key.as_str())
                .map_err(|e| AiEventError::Storage(e.to_string()))?;
        }
    }
    tx.commit()
        .map_err(|e| AiEventError::Storage(e.to_string()))
}

/// Return newest events first, bounded to `MAX_EVENTS`.
pub fn list_events(path: &Path) -> Result<Vec<AiEvent>, AiEventError> {
    let db = database(path)?;
    let db = db
        .lock()
        .map_err(|_| AiEventError::Storage("database lock poisoned".into()))?;
    let tx = db
        .begin_read()
        .map_err(|e| AiEventError::Storage(e.to_string()))?;
    let table = match tx.open_table(TABLE) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
        Err(e) => return Err(AiEventError::Storage(e.to_string())),
    };
    let mut events = Vec::new();
    for item in table
        .iter()
        .map_err(|e| AiEventError::Storage(e.to_string()))?
    {
        let (_, value) = item.map_err(|e| AiEventError::Storage(e.to_string()))?;
        events.push(
            serde_json::from_slice(value.value())
                .map_err(|e| AiEventError::Storage(e.to_string()))?,
        );
    }
    events.sort_by(|a: &AiEvent, b: &AiEvent| b.sequence.cmp(&a.sequence));
    Ok(events)
}

/// Consume one-shot JSON hook files from `<data_dir>/ai-outbox`.
/// Invalid files remain on disk for inspection and retry; successfully
/// ingested or duplicate events are removed after the redb transaction.
pub fn drain_outbox(data_dir: &Path, node: impl Into<String>) -> Result<usize, AiEventError> {
    let dir = data_dir.join("ai-outbox");
    let node = node.into();
    let mut drained = 0;
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(AiEventError::Storage(e.to_string())),
    };
    let mut entries = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().and_then(|e| e.to_str()) == Some("json"))
        .filter(|entry| {
            entry
                .file_type()
                .map(|kind| kind.is_file())
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| {
        (
            entry.metadata().and_then(|m| m.modified()).ok(),
            entry.file_name(),
        )
    });
    for entry in entries.into_iter().take(256) {
        let path = entry.path();
        if entry
            .metadata()
            .map(|m| m.len() > MAX_BODY as u64)
            .unwrap_or(true)
        {
            continue;
        }
        let input: AiEventInput = match std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        {
            Some(input) => input,
            None => continue,
        };
        match ingest_event(&data_dir.join("ai-events.redb"), input, node.clone()) {
            Ok(_) => {}
            Err(AiEventError::Invalid(_)) => continue,
            Err(error) => return Err(error),
        }
        std::fs::remove_file(path).map_err(|e| AiEventError::Storage(e.to_string()))?;
        drained += 1;
    }
    Ok(drained)
}

fn data_path(_state: &AppState) -> PathBuf {
    paths::data_dir().join("ai-events.redb")
}

fn loopback(addr: Option<&SocketAddr>) -> bool {
    addr.is_some_and(|a| a.ip().is_loopback())
}

fn token_ok(state: &AppState, headers: &HeaderMap) -> bool {
    let shared = state
        .config
        .network
        .auth_token
        .as_deref()
        .filter(|token| !token.is_empty())
        .is_some_and(|expected| {
            headers
                .get(protocol::TOKEN_HEADER)
                .and_then(|v| v.to_str().ok())
                == Some(expected)
        });
    let ai = state
        .config
        .ai_token
        .as_deref()
        .filter(|token| token.len() >= 32)
        .is_some_and(|expected| {
            headers
                .get("x-syncclip-ai-token")
                .and_then(|v| v.to_str().ok())
                == Some(expected)
        });
    shared || ai
}

fn reject_origin(headers: &HeaderMap) -> bool {
    headers.get(header::ORIGIN).is_some()
}

async fn post_event(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    input: Result<Json<AiEventInput>, JsonRejection>,
) -> Response {
    if !loopback(Some(&addr))
        || reject_origin(&headers)
        || headers
            .get("x-syncclip-ai-hook")
            .and_then(|v| v.to_str().ok())
            != Some("1")
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Json(input) = match input {
        Ok(input) => input,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    match ingest_event(&data_path(&state), input, state.origin.host.clone()) {
        Ok(event) => (StatusCode::CREATED, Json(event)).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn get_events(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if !loopback(Some(&addr)) && !token_ok(&state, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match list_events(&data_path(&state)) {
        Ok(events) => Json(events).into_response(),
        Err(e) => e.into_response(),
    }
}

impl IntoResponse for AiEventError {
    fn into_response(self) -> Response {
        let status = if matches!(self, Self::Invalid(_)) {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        (status, self.to_string()).into_response()
    }
}

/// Routes to merge into the daemon's main router.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/ai/events", post(post_event).get(get_events))
        .layer(axum::extract::DefaultBodyLimit::max(MAX_BODY))
}
