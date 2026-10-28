//! Local-only Markdown notes and Todo lists, separate from clipboard history.

use std::collections::HashSet;

use axum::body::to_bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path as AxumPath, Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::Error;
use crate::model::{Entry, EntryKind};
use crate::notebook_sync::NotebookSnapshot;
use crate::server::AppState;
use crate::store::{NotebookPublishOutcome, NotebookWriteOutcome};

const MAX_REQUEST_BYTES: usize = 512 * 1024;
pub(crate) const MAX_RECORD_BYTES: usize = 256 * 1024;
pub(crate) const MAX_NOTEBOOK_ITEMS: usize = 1_000;
pub(crate) const MAX_NOTEBOOK_TOTAL_BYTES: usize = 32 * 1024 * 1024;
const MAX_TITLE_CHARS: usize = 200;
const MAX_TASK_TEXT_BYTES: usize = 4_096;
const MAX_TASKS: usize = 500;
const MAX_SUMMARY_ITEMS: usize = 1_000;
const MAX_PREVIEW_CHARS: usize = 160;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NotebookKind {
    Markdown,
    Todo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotebookTask {
    pub id: String,
    pub text: String,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotebookItem {
    pub id: String,
    pub kind: NotebookKind,
    pub title: String,
    pub markdown: String,
    pub tasks: Vec<NotebookTask>,
    pub pinned: bool,
    pub trashed: bool,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Response-only sync metadata; the durable causal graph is in its own
    /// redb table and these fields are never accepted in write requests.
    #[serde(skip)]
    pub(crate) version_id: Option<String>,
    #[serde(skip)]
    pub(crate) conflict_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NotebookSummary {
    pub id: String,
    pub kind: NotebookKind,
    pub title: String,
    pub preview: String,
    pub pinned: bool,
    pub trashed: bool,
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub task_count: usize,
    pub completed_count: usize,
    pub version_id: String,
    pub conflict_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRequest {
    kind: NotebookKind,
    #[serde(default)]
    title: String,
    #[serde(default)]
    markdown: String,
    #[serde(default)]
    tasks: Vec<NotebookTask>,
    #[serde(default)]
    pinned: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateRequest {
    kind: NotebookKind,
    title: String,
    markdown: String,
    tasks: Vec<NotebookTask>,
    pinned: bool,
    expected_revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RevisionRequest {
    expected_revision: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    kind: Option<String>,
    q: Option<String>,
    trash: Option<bool>,
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
    message: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_revision: Option<u64>,
}

#[derive(Serialize)]
struct NotebookItemResponse<'a> {
    #[serde(flatten)]
    item: &'a NotebookItem,
    version_id: &'a str,
    conflict_count: usize,
}

pub(crate) enum ApiError {
    Invalid(&'static str, &'static str),
    PayloadTooLarge(&'static str, &'static str),
    NotFound,
    Conflict {
        error: &'static str,
        message: &'static str,
        current_revision: Option<u64>,
    },
    QuotaExceeded,
    Storage,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, error, message, current_revision) = match self {
            Self::Invalid(error, message) => (StatusCode::BAD_REQUEST, error, message, None),
            Self::PayloadTooLarge(error, message) => {
                (StatusCode::PAYLOAD_TOO_LARGE, error, message, None)
            }
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found", "笔记不存在", None),
            Self::Conflict {
                error,
                message,
                current_revision,
            } => (StatusCode::CONFLICT, error, message, current_revision),
            Self::QuotaExceeded => (
                StatusCode::CONFLICT,
                "quota_exceeded",
                "笔记空间已满，请整理后重试",
                None,
            ),
            Self::Storage => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_error",
                "本机存储失败",
                None,
            ),
        };
        (
            status,
            Json(ErrorBody {
                error,
                message,
                current_revision,
            }),
        )
            .into_response()
    }
}

/// Routes and assets for the desktop notebook UI. The caller supplies AppState
/// through the shared server router so all writes use the existing Store.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/notebook", get(list_items).post(create_item))
        .route(
            "/v1/notebook/{id}",
            get(get_item).put(update_item).delete(trash_item),
        )
        .route("/v1/notebook/{id}/restore", post(restore_item))
        .route("/v1/notebook/{id}/publish", post(publish_item))
        .route("/assets/notebook.js", get(notebook_script))
        .route("/assets/notebook.css", get(notebook_style))
        .layer(middleware::from_fn(notebook_gate))
        .layer(middleware::from_fn(no_store))
}

pub(crate) async fn notebook_gate(request: Request, next: Next) -> Response {
    if crate::server::trusted_desktop_request(&request) {
        next.run(request).await
    } else {
        (
            StatusCode::FORBIDDEN,
            "notebook is available only to the local desktop",
        )
            .into_response()
    }
}

pub(crate) async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn notebook_script() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        include_str!("../notebook.js"),
    )
}

async fn notebook_style() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        include_str!("../notebook.css"),
    )
}

async fn list_items(
    State(state): State<AppState>,
    query: std::result::Result<Query<ListQuery>, QueryRejection>,
) -> Response {
    let Query(query) = match query {
        Ok(query) => query,
        Err(_) => return ApiError::Invalid("invalid_query", "查询参数无效").into_response(),
    };
    let kind = match query.kind.as_deref() {
        None => None,
        Some("markdown") => Some(NotebookKind::Markdown),
        Some("todo") => Some(NotebookKind::Todo),
        Some(_) => return ApiError::Invalid("invalid_kind", "笔记类型无效").into_response(),
    };
    let trash = query.trash.unwrap_or(false);
    let query_text = query.q.unwrap_or_default().trim().to_lowercase();

    let records = match state.store.list_notebook_snapshots() {
        Ok(records) => records,
        Err(_) => return storage_failure(),
    };
    let mut summaries = Vec::with_capacity(records.len().min(MAX_SUMMARY_ITEMS));
    for snapshot in records {
        let item = &snapshot.item;
        if item.trashed != trash || kind.is_some_and(|kind| item.kind != kind) {
            continue;
        }
        if !query_text.is_empty() && !item_matches(item, &query_text) {
            continue;
        }
        summaries.push(summary(&snapshot));
    }
    summaries.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    summaries.truncate(MAX_SUMMARY_ITEMS);
    Json(serde_json::json!({ "items": summaries })).into_response()
}

async fn create_item(State(state): State<AppState>, request: Request) -> Response {
    let request: CreateRequest = match parse_json(request).await {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let now = Utc::now();
    let item = NotebookItem {
        id: Uuid::new_v4().to_string(),
        kind: request.kind,
        title: request.title,
        markdown: request.markdown,
        tasks: request.tasks,
        pinned: request.pinned,
        trashed: false,
        revision: 1,
        created_at: now,
        updated_at: now,
        version_id: None,
        conflict_count: 0,
    };
    if let Err(error) = validate_item(&item) {
        return error.into_response();
    }
    let _bytes = match serde_json::to_vec(&item) {
        Ok(bytes) if bytes.len() <= MAX_RECORD_BYTES => bytes,
        Ok(_) => {
            return ApiError::PayloadTooLarge("record_too_large", "笔记内容超过单条记录限制")
                .into_response()
        }
        Err(_) => return storage_failure(),
    };
    match state
        .store
        .create_notebook_local(&item, MAX_NOTEBOOK_ITEMS, MAX_NOTEBOOK_TOTAL_BYTES)
    {
        Ok(NotebookWriteOutcome::Stored) => match state.store.get_notebook_snapshot(&item.id) {
            Ok(Some(snapshot)) => snapshot_response(snapshot, StatusCode::CREATED),
            _ => storage_failure(),
        },
        Ok(NotebookWriteOutcome::Conflict { current_revision }) => ApiError::Conflict {
            error: "revision_conflict",
            message: "笔记版本冲突，请重新加载",
            current_revision: Some(current_revision),
        }
        .into_response(),
        Ok(NotebookWriteOutcome::SyncConflict {
            current_revision,
            conflict_count,
        }) => sync_conflict(current_revision, conflict_count),
        Ok(NotebookWriteOutcome::QuotaExceeded) => ApiError::QuotaExceeded.into_response(),
        Err(Error::TooLarge { .. }) => ApiError::QuotaExceeded.into_response(),
        Ok(NotebookWriteOutcome::NotFound) | Err(_) => storage_failure(),
    }
}

async fn get_item(
    State(state): State<AppState>,
    path: std::result::Result<AxumPath<String>, PathRejection>,
) -> Response {
    let id = match parse_path_id(path) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    match state.store.get_notebook_snapshot(&id) {
        Ok(Some(snapshot)) => snapshot_response(snapshot, StatusCode::OK),
        Ok(None) => ApiError::NotFound.into_response(),
        Err(_) => storage_failure(),
    }
}

async fn update_item(
    State(state): State<AppState>,
    path: std::result::Result<AxumPath<String>, PathRejection>,
    request: Request,
) -> Response {
    let id = match parse_path_id(path) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let request: UpdateRequest = match parse_json(request).await {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let snapshot = match state.store.get_notebook_snapshot(&id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(_) => return storage_failure(),
    };
    if snapshot.conflict_count > 0 {
        return sync_conflict(snapshot.item.revision, snapshot.conflict_count);
    }
    let mut item = snapshot.item;
    if item.revision != request.expected_revision {
        return revision_conflict(item.revision);
    }
    if item.trashed {
        return ApiError::Conflict {
            error: "item_trashed",
            message: "回收站中的笔记不能编辑",
            current_revision: Some(item.revision),
        }
        .into_response();
    }
    if item.kind != request.kind {
        return ApiError::Invalid("kind_immutable", "笔记类型创建后不能更改").into_response();
    }
    let Some(next_revision) = item.revision.checked_add(1) else {
        return ApiError::Conflict {
            error: "revision_exhausted",
            message: "笔记版本号已达到上限",
            current_revision: Some(item.revision),
        }
        .into_response();
    };
    item.title = request.title;
    item.markdown = request.markdown;
    item.tasks = request.tasks;
    item.pinned = request.pinned;
    item.revision = next_revision;
    item.updated_at = Utc::now();
    if let Err(error) = validate_item(&item) {
        return error.into_response();
    }
    save_updated_item(&state, &item, request.expected_revision)
}

async fn trash_item(
    State(state): State<AppState>,
    path: std::result::Result<AxumPath<String>, PathRejection>,
    request: Request,
) -> Response {
    let id = match parse_path_id(path) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let request: RevisionRequest = match parse_json(request).await {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let snapshot = match state.store.get_notebook_snapshot(&id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(_) => return storage_failure(),
    };
    if snapshot.conflict_count > 0 {
        return sync_conflict(snapshot.item.revision, snapshot.conflict_count);
    }
    let mut item = snapshot.item;
    if item.revision != request.expected_revision {
        return revision_conflict(item.revision);
    }
    if item.trashed {
        return ApiError::Conflict {
            error: "already_trashed",
            message: "笔记已在回收站中",
            current_revision: Some(item.revision),
        }
        .into_response();
    }
    let Some(next_revision) = item.revision.checked_add(1) else {
        return ApiError::Conflict {
            error: "revision_exhausted",
            message: "笔记版本号已达到上限",
            current_revision: Some(item.revision),
        }
        .into_response();
    };
    item.trashed = true;
    item.revision = next_revision;
    item.updated_at = Utc::now();
    save_updated_item(&state, &item, request.expected_revision)
}

async fn restore_item(
    State(state): State<AppState>,
    path: std::result::Result<AxumPath<String>, PathRejection>,
    request: Request,
) -> Response {
    let id = match parse_path_id(path) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let request: RevisionRequest = match parse_json(request).await {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let snapshot = match state.store.get_notebook_snapshot(&id) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return ApiError::NotFound.into_response(),
        Err(_) => return storage_failure(),
    };
    if snapshot.conflict_count > 0 {
        return sync_conflict(snapshot.item.revision, snapshot.conflict_count);
    }
    let mut item = snapshot.item;
    if item.revision != request.expected_revision {
        return revision_conflict(item.revision);
    }
    if !item.trashed {
        return ApiError::Conflict {
            error: "not_trashed",
            message: "笔记不在回收站中",
            current_revision: Some(item.revision),
        }
        .into_response();
    }
    let Some(next_revision) = item.revision.checked_add(1) else {
        return ApiError::Conflict {
            error: "revision_exhausted",
            message: "笔记版本号已达到上限",
            current_revision: Some(item.revision),
        }
        .into_response();
    };
    item.trashed = false;
    item.revision = next_revision;
    item.updated_at = Utc::now();
    save_updated_item(&state, &item, request.expected_revision)
}

async fn publish_item(
    State(state): State<AppState>,
    path: std::result::Result<AxumPath<String>, PathRejection>,
    request: Request,
) -> Response {
    let id = match parse_path_id(path) {
        Ok(id) => id,
        Err(error) => return error.into_response(),
    };
    let request: RevisionRequest = match parse_json(request).await {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let origin = state.origin.clone();
    let max_transfer_bytes = state.config.limits.max_transfer_bytes;
    match state
        .store
        .publish_notebook_entry(&id, request.expected_revision, |bytes| {
            let item: NotebookItem = serde_json::from_slice(bytes)
                .map_err(|_| Error::Storage("invalid notebook record".into()))?;
            if item.id != id {
                return Err(Error::Storage("invalid notebook key".into()));
            }
            let text = publish_text(&item);
            if text.len() as u64 > max_transfer_bytes {
                return Err(Error::TooLarge {
                    size: text.len() as u64,
                    limit: max_transfer_bytes,
                });
            }
            let mut entry = Entry {
                id: Uuid::new_v4().to_string(),
                kind: EntryKind::Text,
                origin: origin.clone(),
                created_at: Utc::now(),
                text: Some(text),
                blob: None,
                thumb: None,
                files: None,
                preview: String::new(),
                pinned: false,
                size: 0,
                holder: None,
            };
            entry.size = entry.text.as_ref().map_or(0, |text| text.len() as u64);
            entry.preview = entry.build_preview();
            Ok(entry)
        }) {
        Ok(NotebookPublishOutcome::Published { entry_id }) => {
            // The normal clipboard-history cap still applies. Notebook rows
            // live in a different table and are not affected by this prune.
            if state
                .store
                .prune(state.config.storage.history_limit)
                .is_err()
            {
                tracing::warn!("notebook publish history prune failed");
            }
            Json(serde_json::json!({ "entry_id": entry_id })).into_response()
        }
        Ok(NotebookPublishOutcome::NotFound) => ApiError::NotFound.into_response(),
        Ok(NotebookPublishOutcome::Trashed) => ApiError::Conflict {
            error: "item_trashed",
            message: "回收站中的笔记不能发布",
            current_revision: Some(request.expected_revision),
        }
        .into_response(),
        Ok(NotebookPublishOutcome::Conflict { current_revision }) => {
            revision_conflict(current_revision)
        }
        Err(Error::TooLarge { .. }) => {
            ApiError::PayloadTooLarge("entry_too_large", "发布内容超过本机传输限制").into_response()
        }
        Err(_) => storage_failure(),
    }
}

fn save_updated_item(state: &AppState, item: &NotebookItem, expected_revision: u64) -> Response {
    let _bytes = match serde_json::to_vec(item) {
        Ok(bytes) if bytes.len() <= MAX_RECORD_BYTES => bytes,
        Ok(_) => {
            return ApiError::PayloadTooLarge("record_too_large", "笔记内容超过单条记录限制")
                .into_response()
        }
        Err(_) => return storage_failure(),
    };
    match state.store.update_notebook_local(
        item,
        expected_revision,
        MAX_NOTEBOOK_ITEMS,
        MAX_NOTEBOOK_TOTAL_BYTES,
    ) {
        Ok(NotebookWriteOutcome::Stored) => match state.store.get_notebook_snapshot(&item.id) {
            Ok(Some(snapshot)) => snapshot_response(snapshot, StatusCode::OK),
            _ => storage_failure(),
        },
        Ok(NotebookWriteOutcome::NotFound) => ApiError::NotFound.into_response(),
        Ok(NotebookWriteOutcome::Conflict { current_revision }) => {
            revision_conflict(current_revision)
        }
        Ok(NotebookWriteOutcome::SyncConflict {
            current_revision,
            conflict_count,
        }) => sync_conflict(current_revision, conflict_count),
        Ok(NotebookWriteOutcome::QuotaExceeded) => ApiError::QuotaExceeded.into_response(),
        Err(Error::TooLarge { .. }) => ApiError::QuotaExceeded.into_response(),
        Err(_) => storage_failure(),
    }
}

pub(crate) fn parse_path_id(
    path: std::result::Result<AxumPath<String>, PathRejection>,
) -> std::result::Result<String, ApiError> {
    let AxumPath(id) = path.map_err(|_| ApiError::Invalid("invalid_id", "笔记编号无效"))?;
    Uuid::parse_str(&id)
        .map(|uuid| uuid.hyphenated().to_string())
        .map_err(|_| ApiError::Invalid("invalid_id", "笔记编号无效"))
}

pub(crate) async fn parse_json<T: DeserializeOwned>(
    request: Request,
) -> std::result::Result<T, ApiError> {
    let bytes = to_bytes(request.into_body(), MAX_REQUEST_BYTES)
        .await
        .map_err(|error| {
            if error.to_string().contains("length limit exceeded") {
                ApiError::PayloadTooLarge("request_too_large", "请求超过大小限制")
            } else {
                ApiError::Invalid("invalid_request", "请求格式无效")
            }
        })?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(ApiError::PayloadTooLarge(
            "request_too_large",
            "请求超过大小限制",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| ApiError::Invalid("invalid_request", "请求格式无效"))
}

fn validate_item(item: &NotebookItem) -> std::result::Result<(), ApiError> {
    if item.title.chars().count() > MAX_TITLE_CHARS {
        return Err(ApiError::Invalid("title_too_long", "标题最多 200 个字符"));
    }
    match item.kind {
        NotebookKind::Markdown if !item.tasks.is_empty() => {
            return Err(ApiError::Invalid(
                "invalid_kind_fields",
                "Markdown 笔记不能包含 Todo 项",
            ));
        }
        NotebookKind::Todo if !item.markdown.is_empty() => {
            return Err(ApiError::Invalid(
                "invalid_kind_fields",
                "Todo 清单不能包含 Markdown 正文",
            ));
        }
        _ => {}
    }
    if item.tasks.len() > MAX_TASKS {
        return Err(ApiError::Invalid("too_many_tasks", "Todo 项最多 500 条"));
    }
    let mut ids = HashSet::with_capacity(item.tasks.len());
    for task in &item.tasks {
        if task.text.len() > MAX_TASK_TEXT_BYTES {
            return Err(ApiError::Invalid(
                "task_text_too_long",
                "单项任务文本最多 4096 字节",
            ));
        }
        let id = Uuid::parse_str(&task.id)
            .map_err(|_| ApiError::Invalid("invalid_task_id", "任务编号必须是 UUID"))?;
        if !ids.insert(id) {
            return Err(ApiError::Invalid("duplicate_task_id", "任务编号不能重复"));
        }
    }
    Ok(())
}

fn item_matches(item: &NotebookItem, query: &str) -> bool {
    item.title.to_lowercase().contains(query)
        || item.markdown.to_lowercase().contains(query)
        || item
            .tasks
            .iter()
            .any(|task| task.text.to_lowercase().contains(query))
}

fn summary(snapshot: &NotebookSnapshot) -> NotebookSummary {
    let item = &snapshot.item;
    let preview_source = match item.kind {
        NotebookKind::Markdown => item.markdown.as_str(),
        NotebookKind::Todo => "",
    };
    let preview_source = if preview_source.is_empty() {
        item.tasks
            .iter()
            .map(|task| task.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        preview_source.to_owned()
    };
    let flat = preview_source
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    NotebookSummary {
        id: item.id.clone(),
        kind: item.kind,
        title: item.title.clone(),
        preview: flat.chars().take(MAX_PREVIEW_CHARS).collect(),
        pinned: item.pinned,
        trashed: item.trashed,
        revision: item.revision,
        created_at: item.created_at,
        updated_at: item.updated_at,
        task_count: item.tasks.len(),
        completed_count: item.tasks.iter().filter(|task| task.done).count(),
        version_id: snapshot.version_id.clone(),
        conflict_count: snapshot.conflict_count,
    }
}

fn snapshot_response(snapshot: NotebookSnapshot, status: StatusCode) -> Response {
    let body = NotebookItemResponse {
        item: &snapshot.item,
        version_id: &snapshot.version_id,
        conflict_count: snapshot.conflict_count,
    };
    (status, Json(body)).into_response()
}

pub(crate) fn sync_conflict(current_revision: u64, conflict_count: usize) -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "error": "sync_conflict",
            "message": "笔记存在并发版本，请先选择要保留的版本",
            "current_revision": current_revision,
            "conflict_count": conflict_count,
        })),
    )
        .into_response()
}

fn publish_text(item: &NotebookItem) -> String {
    let title = if item.title.is_empty() {
        String::new()
    } else {
        format!("# {}\n\n", item.title)
    };
    match item.kind {
        NotebookKind::Markdown => format!("{title}{}", item.markdown),
        NotebookKind::Todo => {
            let tasks = item
                .tasks
                .iter()
                .map(|task| {
                    let state = if task.done { "x" } else { " " };
                    let text = task.text.replace(['\r', '\n'], " ");
                    format!("- [{state}] {text}")
                })
                .collect::<Vec<_>>()
                .join("\n");
            if tasks.is_empty() {
                title.trim_end().to_owned()
            } else {
                format!("{title}{tasks}")
            }
        }
    }
}

fn revision_conflict(current_revision: u64) -> Response {
    ApiError::Conflict {
        error: "revision_conflict",
        message: "笔记版本冲突，请重新加载",
        current_revision: Some(current_revision),
    }
    .into_response()
}

pub(crate) fn storage_failure() -> Response {
    tracing::error!("notebook storage operation failed");
    ApiError::Storage.into_response()
}
