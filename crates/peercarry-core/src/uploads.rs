//! Device-authorized, bounded and persistent mobile uploads.
use crate::{
    model::{Entry, EntryKind, FileRef},
    paths,
    server::AppState,
};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Extension, Path as AxumPath, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const CHUNK_LIMIT: usize = 1024 * 1024;
#[derive(Clone)]
struct UploadState {
    app: AppState,
    root: PathBuf,
    inner: Arc<Mutex<Inner>>,
    unavailable: bool,
}
#[derive(Default)]
struct Inner {
    sessions: HashMap<String, Session>,
    reserved: u64,
}
#[derive(Clone)]
struct Owner(String);
#[derive(Clone, Serialize, Deserialize)]
struct Session {
    request_id: String,
    filename: String,
    size: u64,
    sha256: String,
    device_id: String,
    completed: bool,
    created_at: u64,
    updated_at: u64,
}
#[derive(Deserialize)]
struct CreateRequest {
    request_id: String,
    filename: String,
    size: u64,
    sha256: String,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn error(code: StatusCode, message: &str) -> Response {
    (code, Json(serde_json::json!({"error":message}))).into_response()
}
fn io_error() -> Response {
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        "upload storage unavailable",
    )
}
fn missing() -> Response {
    error(StatusCode::NOT_FOUND, "upload not found")
}
fn reject_link(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => {
            Err(std::io::Error::other("symlink in upload storage"))
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}
fn directory(s: &UploadState, id: &str) -> std::io::Result<PathBuf> {
    Uuid::parse_str(id).map_err(|_| std::io::Error::other("invalid upload ID"))?;
    reject_link(&s.root)?;
    let dir = s.root.join(id);
    reject_link(&dir)?;
    for name in [
        "payload.part",
        "payload.bin",
        "metadata.json",
        "metadata.tmp",
    ] {
        reject_link(&dir.join(name))?;
    }
    Ok(dir)
}
fn manifest(dir: &Path, session: &Session) -> std::io::Result<()> {
    reject_link(&dir.join("metadata.tmp"))?;
    reject_link(&dir.join("metadata.json"))?;
    let mut f = File::create(dir.join("metadata.tmp"))?;
    f.write_all(&serde_json::to_vec(session).map_err(std::io::Error::other)?)?;
    f.sync_all()?;
    drop(f);
    fs::rename(dir.join("metadata.tmp"), dir.join("metadata.json"))
}
fn filename_valid(name: &str) -> bool {
    if name.is_empty()
        || name.len() > 255
        || name == "."
        || name == ".."
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|c| c.is_control() || "/\\:<>\"|?*".contains(c))
    {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    if ["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str()) {
        return false;
    }
    !((stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.len() == 4
        && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}
fn hash_valid(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}
fn offset(dir: &Path, session: &Session) -> std::io::Result<u64> {
    let published = dir.join("payload.bin");
    let path = if session.completed || published.exists() {
        published
    } else {
        dir.join("payload.part")
    };
    let m = fs::metadata(path)?;
    if !m.is_file() || m.len() > session.size {
        return Err(std::io::Error::other("invalid upload payload"));
    }
    Ok(m.len())
}
fn status(id: &str, session: &Session, off: u64) -> Response {
    Json(serde_json::json!({"id":id,"offset":off,"size":session.size,
        "state":if session.completed {"completed"} else {"uploading"},
        "entry_id":if session.completed {Some(id)} else {None}}))
    .into_response()
}
fn cleanup(s: &UploadState, inner: &mut Inner) -> std::io::Result<()> {
    let ids: Vec<_> = inner
        .sessions
        .iter()
        .filter(|(_, v)| {
            !v.completed
                && now().saturating_sub(v.updated_at) >= s.app.config.mobile.session_ttl_secs
        })
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        let dir = directory(s, &id)?;
        // A published file may be awaiting redb/manifest recovery. Never expire it.
        if dir.join("payload.bin").exists() {
            continue;
        }
        fs::remove_dir_all(&dir)?;
        if let Some(session) = inner.sessions.remove(&id) {
            inner.reserved -= session.size;
        }
    }
    Ok(())
}
fn load(s: &UploadState) -> std::io::Result<Inner> {
    reject_link(&s.root)?;
    fs::create_dir_all(&s.root)?;
    let mut inner = Inner::default();
    for item in fs::read_dir(&s.root)? {
        let item = item?;
        let id = item.file_name().to_string_lossy().to_string();
        let dir = directory(s, &id)?;
        let session: Session = serde_json::from_slice(&fs::read(dir.join("metadata.json"))?)
            .map_err(std::io::Error::other)?;
        if !filename_valid(&session.filename)
            || !hash_valid(&session.sha256)
            || Uuid::parse_str(&session.request_id).is_err()
        {
            return Err(std::io::Error::other("invalid upload manifest"));
        }
        offset(&dir, &session)?;
        inner.reserved = inner
            .reserved
            .checked_add(session.size)
            .ok_or_else(|| std::io::Error::other("quota overflow"))?;
        inner.sessions.insert(id, session);
    }
    cleanup(s, &mut inner)?;
    Ok(inner)
}

pub fn router(app: AppState) -> Router<AppState> {
    let mut s = UploadState {
        app,
        root: paths::data_dir().join("mobile-uploads"),
        inner: Arc::new(Mutex::new(Inner::default())),
        unavailable: false,
    };
    if s.app.config.mobile.enabled {
        match load(&s) {
            Ok(inner) => s.inner = Arc::new(Mutex::new(inner)),
            Err(_) => {
                s.unavailable = true;
                tracing::error!(
                    "mobile upload storage could not be recovered; uploads disabled until repaired"
                );
            }
        }
    }
    Router::new()
        .route("/v1/uploads", post(create))
        .route(
            "/v1/uploads/{id}",
            get(get_upload).patch(patch_upload).delete(delete_upload),
        )
        .route("/v1/uploads/{id}/complete", post(complete))
        .layer(DefaultBodyLimit::max(CHUNK_LIMIT))
        .layer(middleware::from_fn_with_state(s.clone(), authorize))
        .with_state(s)
}
async fn authorize(State(s): State<UploadState>, mut req: Request, next: Next) -> Response {
    if !s.app.config.mobile.enabled {
        return missing();
    }
    let id = req
        .headers()
        .get("x-syncclip-device-id")
        .and_then(|v| v.to_str().ok());
    let token = req
        .headers()
        .get("x-syncclip-device-token")
        .and_then(|v| v.to_str().ok());
    let owner = s.app.config.mobile.devices.iter().find(|d| {
        Some(d.id.as_str()) == id && !d.token.is_empty() && Some(d.token.as_str()) == token
    });
    let Some(owner) = owner else {
        return error(StatusCode::UNAUTHORIZED, "device authorization required");
    };
    if s.unavailable || s.app.config.mobile.validate().is_err() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "mobile uploads unavailable",
        );
    }
    req.extensions_mut().insert(Owner(owner.id.clone()));
    next.run(req).await
}
// All disk IO, hashing and mutex ownership stay within a blocking task. If the
// HTTP request is cancelled, the in-flight atomic operation still finishes.
async fn run(
    s: UploadState,
    f: impl FnOnce(&UploadState, &mut Inner) -> Response + Send + 'static,
) -> Response {
    let Some(activity) = crate::maintenance::enter() else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "update in progress");
    };
    tokio::task::spawn_blocking(move || {
        let _activity = activity;
        let Ok(mut inner) = s.inner.lock() else {
            return io_error();
        };
        f(&s, &mut inner)
    })
    .await
    .unwrap_or_else(|_| io_error())
}
fn owned<'a>(inner: &'a Inner, id: &str, owner: &str) -> Option<&'a Session> {
    inner
        .sessions
        .get(id)
        .filter(|session| session.device_id == owner)
}
async fn create(
    State(s): State<UploadState>,
    Extension(owner): Extension<Owner>,
    Json(req): Json<CreateRequest>,
) -> Response {
    if Uuid::parse_str(&req.request_id).is_err()
        || !filename_valid(&req.filename)
        || !hash_valid(&req.sha256)
    {
        return error(StatusCode::BAD_REQUEST, "invalid upload metadata");
    }
    if req.size > s.app.config.limits.max_transfer_bytes {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "file exceeds transfer limit");
    }
    run(s, move |s, inner| {
        if cleanup(s, inner).is_err() {
            return io_error();
        }
        if let Some((id, old)) = inner
            .sessions
            .iter()
            .find(|(_, v)| v.device_id == owner.0 && v.request_id == req.request_id)
        {
            if old.filename != req.filename
                || old.size != req.size
                || !old.sha256.eq_ignore_ascii_case(&req.sha256)
            {
                return error(StatusCode::CONFLICT, "request_id metadata differs");
            }
            return directory(s, id)
                .and_then(|d| offset(&d, old))
                .map(|off| status(id, old, off))
                .unwrap_or_else(|_| io_error());
        }
        if inner.sessions.len() >= s.app.config.mobile.max_sessions
            || inner
                .reserved
                .checked_add(req.size)
                .is_none_or(|n| n > s.app.config.mobile.max_storage_bytes)
        {
            return error(StatusCode::CONFLICT, "upload quota exceeded");
        }
        let id = Uuid::new_v4().to_string();
        let Ok(dir) = directory(s, &id) else {
            return io_error();
        };
        let session = Session {
            request_id: req.request_id,
            filename: req.filename,
            size: req.size,
            sha256: req.sha256.to_ascii_lowercase(),
            device_id: owner.0,
            completed: false,
            created_at: now(),
            updated_at: now(),
        };
        if fs::create_dir(&dir)
            .and_then(|_| File::create(dir.join("payload.part")))
            .and_then(|f| f.sync_all())
            .and_then(|_| manifest(&dir, &session))
            .is_err()
        {
            let _ = fs::remove_dir_all(&dir);
            return io_error();
        }
        inner.reserved += session.size;
        inner.sessions.insert(id.clone(), session.clone());
        let mut response = status(&id, &session, 0);
        *response.status_mut() = StatusCode::CREATED;
        response
    })
    .await
}
async fn get_upload(
    State(s): State<UploadState>,
    Extension(owner): Extension<Owner>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    run(s, move |s, inner| {
        let Some(session) = owned(inner, &id, &owner.0) else {
            return missing();
        };
        directory(s, &id)
            .and_then(|d| offset(&d, session))
            .map(|off| status(&id, session, off))
            .unwrap_or_else(|_| io_error())
    })
    .await
}
async fn patch_upload(
    State(s): State<UploadState>,
    Extension(owner): Extension<Owner>,
    AxumPath(id): AxumPath<String>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Response {
    let Some(expected) = headers
        .get("upload-offset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
    else {
        return error(StatusCode::BAD_REQUEST, "Upload-Offset required");
    };
    run(s, move |s, inner| {
        let Some(old) = owned(inner, &id, &owner.0).cloned() else {
            return missing();
        };
        let Ok(dir) = directory(s, &id) else {
            return io_error();
        };
        if old.completed || dir.join("payload.bin").exists() {
            return error(
                StatusCode::CONFLICT,
                "upload already published; complete to recover",
            );
        }
        let Ok(off) = offset(&dir, &old) else {
            return io_error();
        };
        if off != expected {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error":"upload offset conflict","offset":off})),
            )
                .into_response();
        }
        if off
            .checked_add(bytes.len() as u64)
            .is_none_or(|n| n > old.size)
        {
            return error(StatusCode::BAD_REQUEST, "chunk exceeds declared size");
        }
        if OpenOptions::new()
            .append(true)
            .open(dir.join("payload.part"))
            .and_then(|mut f| {
                f.write_all(&bytes)?;
                f.sync_all()
            })
            .is_err()
        {
            return io_error();
        }
        let mut updated = old;
        updated.updated_at = now();
        if manifest(&dir, &updated).is_err() {
            return io_error();
        }
        inner.sessions.insert(id.clone(), updated.clone());
        status(&id, &updated, off + bytes.len() as u64)
    })
    .await
}
async fn complete(
    State(s): State<UploadState>,
    Extension(owner): Extension<Owner>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    run(s, move |s, inner| {
        let Some(mut session) = owned(inner, &id, &owner.0).cloned() else {
            return missing();
        };
        let Ok(dir) = directory(s, &id) else {
            return io_error();
        };
        if session.completed {
            return status(&id, &session, session.size);
        }
        let published = dir.join("payload.bin");
        let part = dir.join("payload.part");
        let source = if published.exists() {
            &published
        } else {
            &part
        };
        let check = (|| -> std::io::Result<bool> {
            let mut f = File::open(source)?;
            if f.metadata()?.len() != session.size {
                return Ok(false);
            }
            let mut hash = Sha256::new();
            let mut buffer = vec![0u8; CHUNK_LIMIT];
            loop {
                let n = f.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hash.update(&buffer[..n]);
            }
            Ok(hex::encode(hash.finalize()) == session.sha256)
        })();
        match check {
            Ok(true) => {}
            Ok(false) => {
                return error(StatusCode::UNPROCESSABLE_ENTITY, "size or SHA-256 mismatch")
            }
            Err(_) => return io_error(),
        }
        if !published.exists() && fs::hard_link(&part, &published).is_err() {
            return io_error();
        }
        let absolute = match fs::canonicalize(&published) {
            Ok(path) => path,
            Err(_) => return io_error(),
        };
        let mut entry = Entry {
            id: id.clone(),
            kind: EntryKind::Files,
            origin: s.app.origin.clone(),
            created_at: chrono::DateTime::from_timestamp(session.created_at as i64, 0)
                .unwrap_or_else(chrono::Utc::now),
            text: None,
            blob: None,
            thumb: None,
            files: Some(vec![FileRef {
                path: absolute.to_string_lossy().into_owned(),
                name: session.filename.clone(),
                size: session.size,
                is_dir: false,
                hash: Some(session.sha256.clone()),
            }]),
            preview: String::new(),
            pinned: false,
            size: session.size,
            holder: None,
        };
        entry.preview = entry.build_preview();
        // Stable ID makes replay after a crash between redb and manifest idempotent.
        match s.app.store.get(&id) {
            Ok(Some(_)) => {}
            Ok(None) => {
                if s.app.store.insert(&entry).is_err() {
                    return io_error();
                }
            }
            Err(_) => return io_error(),
        }
        session.completed = true;
        session.updated_at = now();
        if manifest(&dir, &session).is_err() {
            return io_error();
        }
        inner.sessions.insert(id.clone(), session.clone());
        let _ = fs::remove_file(part);
        status(&id, &session, session.size)
    })
    .await
}
async fn delete_upload(
    State(s): State<UploadState>,
    Extension(owner): Extension<Owner>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    run(s, move |s, inner| {
        let Some(session) = owned(inner, &id, &owner.0).cloned() else {
            return missing();
        };
        let Ok(dir) = directory(s, &id) else {
            return io_error();
        };
        if session.completed || dir.join("payload.bin").exists() {
            return error(
                StatusCode::CONFLICT,
                "published uploads cannot be cancelled",
            );
        }
        if fs::remove_dir_all(dir).is_err() {
            return io_error();
        }
        inner.sessions.remove(&id);
        inner.reserved -= session.size;
        StatusCode::NO_CONTENT.into_response()
    })
    .await
}
