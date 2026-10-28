//! Redacted settings for the desktop process's local user.
//!
//! The peer-facing configuration API is deliberately separate. These routes
//! never serialize `Config` into a response and never accept peer credentials
//! as a substitute for the local-origin gate.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::body::to_bytes;
use axum::extract::{Extension, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::config::Config;
use crate::server::{self, AppState};

const SETTINGS_PATH: &str = "/v1/desktop/settings";
const RESTART_PATH: &str = "/v1/desktop/settings/restart";
const MAX_BODY: usize = 256 * 1024;
const LOCK_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone)]
struct SettingsContext {
    path: PathBuf,
    startup: Arc<Config>,
    salt: [u8; 32],
    lock: Arc<Mutex<()>>,
    restart: Restart,
    #[cfg(test)]
    default_download_dir: Option<PathBuf>,
}

type Restart = fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>>;

struct ProcessSettings {
    salt: [u8; 32],
    instance_id: uuid::Uuid,
    lock: Arc<Mutex<()>>,
}

fn process_settings() -> &'static ProcessSettings {
    static SETTINGS: OnceLock<ProcessSettings> = OnceLock::new();
    SETTINGS.get_or_init(|| {
        let mut salt = [0; 32];
        salt[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        salt[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        ProcessSettings {
            salt,
            instance_id: uuid::Uuid::new_v4(),
            lock: Arc::new(Mutex::new(())),
        }
    })
}

fn restart_service() -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>> {
    Box::pin(server::action_restart())
}

impl SettingsContext {
    fn new(path: PathBuf, startup: Arc<Config>, restart: Restart) -> Self {
        let process = process_settings();
        Self {
            path,
            startup,
            salt: process.salt,
            lock: process.lock.clone(),
            restart,
            #[cfg(test)]
            default_download_dir: None,
        }
    }

    fn revision(&self, exists: bool, bytes: &[u8]) -> String {
        let mut hash = Sha256::new();
        hash.update(self.salt);
        hash.update([u8::from(exists)]);
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
        hash.update(self.salt);
        hex::encode(hash.finalize())
    }

    fn default_download_dir(&self) -> PathBuf {
        #[cfg(test)]
        if let Some(directory) = &self.default_download_dir {
            return directory.clone();
        }
        crate::paths::download_dir()
    }

    fn read(&self) -> Result<Snapshot, ApiError> {
        let bytes = match fs::symlink_metadata(&self.path) {
            Ok(metadata) if metadata.file_type().is_file() => {
                Some(fs::read(&self.path).map_err(|_| ApiError::storage())?)
            }
            Ok(_) => return Err(ApiError::storage()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(_) => return Err(ApiError::storage()),
        };
        let exists = bytes.is_some();
        let bytes = match bytes {
            Some(bytes) => bytes,
            None => self
                .startup
                .to_toml()
                .map_err(|_| ApiError::storage())?
                .into_bytes(),
        };
        let raw = std::str::from_utf8(&bytes).map_err(|_| ApiError::storage())?;
        let document: toml::Value = toml::from_str(raw).map_err(|_| ApiError::storage())?;
        if !document.is_table() {
            return Err(ApiError::storage());
        }
        let config: Config = toml::from_str(raw).map_err(|_| ApiError::storage())?;
        config.mobile.validate().map_err(|_| ApiError::storage())?;
        Ok(Snapshot {
            revision: self.revision(exists, &bytes),
            exists,
            bytes,
            document,
            config,
        })
    }
}

struct Snapshot {
    revision: String,
    exists: bool,
    bytes: Vec<u8>,
    document: toml::Value,
    config: Config,
}

#[derive(Clone, Copy, Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl ApiError {
    fn invalid() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "settings_invalid",
            message: "Invalid settings request.",
        }
    }
    fn forbidden() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "settings_forbidden",
            message: "Settings require a local same-origin request.",
        }
    }
    fn conflict() -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "settings_conflict",
            message: "Settings changed. Reload before saving.",
        }
    }
    fn too_large() -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "settings_too_large",
            message: "Settings request is too large.",
        }
    }
    fn storage() -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "settings_unavailable",
            message: "Settings are unavailable.",
        }
    }
    fn restart_pending() -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "restart_pending",
            message: "Restart already requested.",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({"error": self.code, "message": self.message})),
        )
            .into_response()
    }
}

fn is_settings_path(path: &str) -> bool {
    matches!(path, SETTINGS_PATH | RESTART_PATH)
}

fn trusted(request: &Request) -> bool {
    if !server::trusted_desktop_request(request) {
        return false;
    }
    let headers = request.headers();
    if headers.get_all(header::HOST).iter().count() != 1
        || headers.get_all(header::ORIGIN).iter().count() > 1
        || headers.get_all("sec-fetch-site").iter().count() > 1
    {
        return false;
    }
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    if host.contains('@') {
        return false;
    }
    if let Some(authority) = request.uri().authority() {
        if authority.as_str() != host {
            return false;
        }
    }
    request
        .uri()
        .scheme_str()
        .is_none_or(|scheme| scheme == "http")
}

/// Run before peer authentication, so rejected local-only requests get the
/// same 403/no-store response whether or not they supplied a peer token.
pub(crate) async fn outer_gate(request: Request, next: Next) -> Response {
    if !is_settings_path(request.uri().path()) {
        return next.run(request).await;
    }
    let mut response = if trusted(&request) {
        next.run(request).await
    } else {
        ApiError::forbidden().into_response()
    };
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

async fn inner_gate(request: Request, next: Next) -> Response {
    if trusted(&request) {
        next.run(request).await
    } else {
        ApiError::forbidden().into_response()
    }
}

pub(crate) fn router(state: &AppState) -> Router<AppState> {
    routes(SettingsContext::new(
        crate::paths::config_path(),
        state.config.clone(),
        restart_service,
    ))
}

fn routes(context: SettingsContext) -> Router<AppState> {
    Router::new()
        .route(SETTINGS_PATH, get(get_settings).post(save_settings))
        .route(RESTART_PATH, post(restart_settings))
        .layer(Extension(context))
        .layer(middleware::from_fn(inner_gate))
}

fn envelope(config: &Config, revision: String, restart: Option<bool>) -> JsonValue {
    let mut result = json!({
        "schema": 1,
        "revision": revision,
        // Public liveness context, independent of the private revision salt.
        // This identifies a process; it is never an access credential.
        "instance_id": process_settings().instance_id.to_string(),
        "desktop_url": desktop_url(config),
        "values": {
            "node.name": config.node.name,
            "node.discoverable": config.node.discoverable,
            "node.allow_remote_apply": config.node.allow_remote_apply,
            "node.allow_remote_capture": config.node.allow_remote_capture,
            "node.allow_remote_launch": config.node.allow_remote_launch,
            "network.port": config.network.port,
            "network.bind": config.network.bind,
            "network.request_timeout_secs": config.network.request_timeout_secs,
            "network.connect_timeout_secs": config.network.connect_timeout_secs,
            "network.discovery_concurrency": config.network.discovery_concurrency,
            "limits.inline_image_bytes": config.limits.inline_image_bytes,
            "limits.inline_text_bytes": config.limits.inline_text_bytes,
            "limits.max_transfer_bytes": config.limits.max_transfer_bytes,
            "storage.history_limit": config.storage.history_limit,
            "storage.thumbnail_px": config.storage.thumbnail_px,
            "storage.dedupe": config.storage.dedupe,
            "storage.download_dir": config.storage.download_dir,
            "storage.data_dir": config.storage.data_dir,
            "updates.auto_check": config.updates.auto_check,
            "updates.check_interval_hours": config.updates.check_interval_hours,
            "updates.source": config.updates.source,
            "updates.domestic_url": config.updates.domestic_url,
            "updates.github_url": config.updates.github_url,
            "updates.custom_url": config.updates.custom_url,
            "updates.public_key": config.updates.public_key,
            "apps": config.apps,
        },
        "secrets": {
            "network.auth_token": config.network.auth_token.as_deref().is_some_and(|v| !v.is_empty()),
            "ai_token": config.ai_token.as_deref().is_some_and(|v| !v.is_empty()),
        }
    });
    // Optional empty text is presented consistently as null.
    for field in [
        "node.name",
        "network.bind",
        "storage.download_dir",
        "storage.data_dir",
    ] {
        if result["values"][field].as_str().is_some_and(|value| {
            value.is_empty()
                || (matches!(field, "node.name" | "network.bind") && value.trim().is_empty())
        }) {
            result["values"][field] = JsonValue::Null;
        }
    }
    if let Some(restart) = restart {
        result["restart_recommended"] = json!(restart);
    }
    result
}

fn desktop_url(config: &Config) -> String {
    use std::net::{IpAddr, SocketAddr};
    // Match server::serve: a loopback main listener uses its actual socket
    // address; other main listeners also attempt 127.0.0.1:network.port.
    // This is a reconnect candidate, not evidence that the listener is up.
    let address = config.network.bind.as_deref().and_then(|bind| {
        bind.parse::<SocketAddr>().ok().or_else(|| {
            bind.parse::<IpAddr>()
                .ok()
                .map(|ip| SocketAddr::new(ip, config.network.port))
        })
    });
    let local = address
        .filter(|address| address.ip().is_loopback())
        .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], config.network.port)));
    format!("http://{local}/")
}

async fn get_settings(Extension(context): Extension<SettingsContext>) -> Response {
    let Ok(_guard) = tokio::time::timeout(LOCK_WAIT, context.lock.lock()).await else {
        return ApiError::storage().into_response();
    };
    match context.read() {
        Ok(snapshot) => match pending_restart(&snapshot.config, &context.startup) {
            Ok(pending) => {
                Json(envelope(&snapshot.config, snapshot.revision, Some(pending))).into_response()
            }
            Err(error) => error.into_response(),
        },
        Err(error) => error.into_response(),
    }
}

fn pending_restart(config: &Config, startup: &Config) -> Result<bool, ApiError> {
    let startup_values = |config: &Config| -> Result<JsonValue, ApiError> {
        let mut value = serde_json::to_value(config).map_err(|_| ApiError::storage())?;
        // These settings are read from disk or explicitly applied after save.
        value
            .as_object_mut()
            .ok_or_else(ApiError::storage)?
            .remove("updates");
        value["storage"]
            .as_object_mut()
            .ok_or_else(ApiError::storage)?
            .remove("download_dir");
        Ok(value)
    };
    Ok(startup_values(config)? != startup_values(startup)?)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveRequest {
    revision: String,
    #[serde(default, deserialize_with = "unique_map")]
    changes: BTreeMap<String, JsonValue>,
    #[serde(default, deserialize_with = "unique_map")]
    secret_changes: BTreeMap<String, SecretChange>,
}

fn unique_map<'de, D, T>(deserializer: D) -> Result<BTreeMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Unique<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Unique<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("an object with unique keys")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut access: M,
        ) -> Result<Self::Value, M::Error> {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = access.next_entry::<String, T>()? {
                if values.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate key"));
                }
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(Unique(std::marker::PhantomData))
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum SecretChange {
    Replace { value: String },
    Clear {},
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RestartRequest {
    revision: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppChange {
    name: String,
    path: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Clone, Copy)]
enum Field {
    Bool,
    Number,
    Text,
    OptionalText,
    AppList,
}

fn field(key: &str) -> Option<Field> {
    Some(match key {
        "node.discoverable"
        | "node.allow_remote_apply"
        | "node.allow_remote_capture"
        | "node.allow_remote_launch"
        | "storage.dedupe"
        | "updates.auto_check" => Field::Bool,
        "network.port"
        | "network.request_timeout_secs"
        | "network.connect_timeout_secs"
        | "network.discovery_concurrency"
        | "limits.inline_image_bytes"
        | "limits.inline_text_bytes"
        | "limits.max_transfer_bytes"
        | "storage.history_limit"
        | "storage.thumbnail_px"
        | "updates.check_interval_hours" => Field::Number,
        "updates.source"
        | "updates.domestic_url"
        | "updates.github_url"
        | "updates.custom_url"
        | "updates.public_key" => Field::Text,
        "node.name" | "network.bind" | "storage.download_dir" | "storage.data_dir" => {
            Field::OptionalText
        }
        "apps" => Field::AppList,
        _ => return None,
    })
}

fn patch(
    document: &mut toml::Value,
    key: &str,
    value: Option<toml::Value>,
) -> Result<(), ApiError> {
    let root = document.as_table_mut().ok_or_else(ApiError::storage)?;
    let (table, name) = if let Some((section, name)) = key.split_once('.') {
        let section = root
            .entry(section)
            .or_insert_with(|| toml::Value::Table(Default::default()));
        (section.as_table_mut().ok_or_else(ApiError::storage)?, name)
    } else {
        (root, key)
    };
    if let Some(value) = value {
        table.insert(name.to_owned(), value);
    } else {
        table.remove(name);
    }
    Ok(())
}

fn field_value(key: &str, value: JsonValue) -> Result<Option<toml::Value>, ApiError> {
    let result = match field(key).ok_or_else(ApiError::invalid)? {
        Field::Bool => toml::Value::Boolean(value.as_bool().ok_or_else(ApiError::invalid)?),
        Field::Number => {
            let number = value
                .as_u64()
                .and_then(|v| i64::try_from(v).ok())
                .ok_or_else(ApiError::invalid)?;
            toml::Value::Integer(number)
        }
        Field::Text => {
            toml::Value::String(value.as_str().ok_or_else(ApiError::invalid)?.to_owned())
        }
        Field::OptionalText => {
            if value.is_null() {
                return Ok(None);
            }
            let text = value.as_str().ok_or_else(ApiError::invalid)?;
            let text = if matches!(key, "node.name" | "network.bind") {
                text.trim()
            } else {
                text
            };
            if text.is_empty() {
                return Ok(None);
            }
            toml::Value::String(text.to_owned())
        }
        Field::AppList => {
            let mut apps: Vec<AppChange> =
                serde_json::from_value(value).map_err(|_| ApiError::invalid())?;
            for app in &mut apps {
                app.name = app.name.trim().to_owned();
            }
            toml::Value::try_from(apps).map_err(|_| ApiError::invalid())?
        }
    };
    Ok(Some(result))
}

async fn read_body<T: for<'de> Deserialize<'de>>(request: Request) -> Result<T, ApiError> {
    let bytes = to_bytes(request.into_body(), MAX_BODY)
        .await
        .map_err(|_| ApiError::too_large())?;
    serde_json::from_slice(&bytes).map_err(|_| ApiError::invalid())
}

async fn save_settings(
    State(state): State<AppState>,
    Extension(context): Extension<SettingsContext>,
    request: Request,
) -> Response {
    match save(&state, &context, request).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => error.into_response(),
    }
}

async fn save(
    state: &AppState,
    context: &SettingsContext,
    request: Request,
) -> Result<JsonValue, ApiError> {
    let input: SaveRequest = read_body(request).await?;
    let _guard = tokio::time::timeout(LOCK_WAIT, context.lock.lock())
        .await
        .map_err(|_| ApiError::storage())?;
    let original = context.read()?;
    if input.revision != original.revision {
        return Err(ApiError::conflict());
    }
    let mut document = original.document.clone();
    let mut download_changed = false;
    for (key, value) in input.changes {
        let value = field_value(&key, value)?;
        patch(&mut document, &key, value)?;
        if key == "storage.download_dir" {
            download_changed = true;
        }
    }
    let ai_changed = input.secret_changes.contains_key("ai_token");
    for (key, change) in input.secret_changes {
        if !matches!(key.as_str(), "network.auth_token" | "ai_token") {
            return Err(ApiError::invalid());
        }
        let value = match change {
            SecretChange::Clear {} => None,
            SecretChange::Replace { value } => {
                if !(32..=256).contains(&value.len())
                    || !value.bytes().all(|c| c.is_ascii_graphic())
                {
                    return Err(ApiError::invalid());
                }
                Some(toml::Value::String(value))
            }
        };
        patch(&mut document, &key, value)?;
    }
    let raw = toml::to_string_pretty(&document).map_err(|_| ApiError::storage())?;
    let config: Config = toml::from_str(&raw).map_err(|_| ApiError::invalid())?;
    // Legacy secrets are preserved byte-for-byte in the TOML value unless an
    // explicit secret operation is supplied. Only new AI secrets are checked.
    let mut validation = config.clone();
    if !ai_changed {
        validation.ai_token = None;
    }
    server::validate_config(&validation).map_err(|_| ApiError::invalid())?;
    let restart_recommended = pending_restart(&config, &context.startup)?;
    if document != original.document || !original.exists {
        atomic_save(context, &original, raw.as_bytes())?;
    } else {
        // A writer outside this endpoint may have changed the file meanwhile.
        if context.read()?.revision != original.revision {
            return Err(ApiError::conflict());
        }
        if download_changed {
            apply_download_dir(state, context, &config);
        }
        return Ok(envelope(
            &config,
            original.revision,
            Some(restart_recommended),
        ));
    }
    if download_changed {
        apply_download_dir(state, context, &config);
    }
    Ok(envelope(
        &config,
        context.revision(true, raw.as_bytes()),
        Some(restart_recommended),
    ))
}

fn apply_download_dir(state: &AppState, context: &SettingsContext, config: &Config) {
    let directory = config
        .storage
        .download_dir
        .clone()
        .unwrap_or_else(|| context.default_download_dir());
    state.engine.set_download_dir(directory);
}

async fn restart_settings(
    Extension(context): Extension<SettingsContext>,
    request: Request,
) -> Response {
    match restart(&context, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn restart(context: &SettingsContext, request: Request) -> Result<Response, ApiError> {
    let input: RestartRequest = read_body(request).await?;
    let _guard = tokio::time::timeout(LOCK_WAIT, context.lock.lock())
        .await
        .map_err(|_| ApiError::storage())?;
    let current = context.read()?;
    if !current.exists || input.revision != current.revision {
        return Err(ApiError::conflict());
    }
    let response = (context.restart)().await;
    if response.status().is_success() {
        Ok(response)
    } else if response.status() == StatusCode::CONFLICT {
        Err(ApiError::restart_pending())
    } else {
        Err(ApiError::storage())
    }
}

struct TempFile(PathBuf);
impl Drop for TempFile {
    fn drop(&mut self) {
        #[cfg(windows)]
        // Windows-only: clear the read-only attribute on our own temp file
        // before cleanup; Unix permissions are never widened here.
        #[allow(clippy::permissions_set_readonly_false)]
        if let Ok(metadata) = fs::metadata(&self.0) {
            let mut permissions = metadata.permissions();
            if permissions.readonly() {
                permissions.set_readonly(false);
                let _ = fs::set_permissions(&self.0, permissions);
            }
        }
        let _ = fs::remove_file(&self.0);
    }
}

fn atomic_save(
    context: &SettingsContext,
    original: &Snapshot,
    bytes: &[u8],
) -> Result<(), ApiError> {
    let parent = context.path.parent().ok_or_else(ApiError::storage)?;
    fs::create_dir_all(parent).map_err(|_| ApiError::storage())?;
    let temp_path = parent.join(format!(".peercarry-settings-{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    if original.exists {
        use std::os::windows::fs::OpenOptionsExt;
        // Keep one stable handle while copying the original owner and DACL.
        options.access_mode(0x4000_0000 | 0x0004_0000 | 0x0008_0000);
    }
    let opened = options.open(&temp_path).map_err(|_| ApiError::storage())?;
    // Only an exclusively created file belongs to this guard.
    let temp = TempFile(temp_path);
    // The handle must close before the guard attempts Windows cleanup.
    let mut file = opened;
    if original.exists {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            const ENCRYPTED: u32 = 0x0000_4000;
            let source_attributes = fs::metadata(&context.path)
                .map_err(|_| ApiError::storage())?
                .file_attributes();
            let temp_attributes = file
                .metadata()
                .map_err(|_| ApiError::storage())?
                .file_attributes();
            if source_attributes & ENCRYPTED != temp_attributes & ENCRYPTED {
                return Err(ApiError::storage());
            }
        }
        copy_security(&context.path, &file).map_err(|_| ApiError::storage())?;
    }
    file.write_all(bytes).map_err(|_| ApiError::storage())?;
    file.sync_all().map_err(|_| ApiError::storage())?;
    if original.exists {
        let permissions = fs::metadata(&context.path)
            .map_err(|_| ApiError::storage())?
            .permissions();
        fs::set_permissions(&temp.0, permissions).map_err(|_| ApiError::storage())?;
        file.sync_all().map_err(|_| ApiError::storage())?;
    }
    drop(file);
    let current = context.read()?;
    if current.exists != original.exists || current.bytes != original.bytes {
        return Err(ApiError::conflict());
    }
    fs::rename(&temp.0, &context.path).map_err(|_| ApiError::storage())?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn copy_security(original: &Path, replacement: &fs::File) -> io::Result<()> {
    linux_file_security::copy(original, replacement)
}

#[cfg(target_os = "linux")]
mod linux_file_security {
    use std::collections::BTreeMap;
    use std::ffi::{c_char, c_void, CString};
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    extern "C" {
        fn flistxattr(fd: i32, list: *mut c_char, size: usize) -> isize;
        fn fgetxattr(fd: i32, name: *const c_char, value: *mut c_void, size: usize) -> isize;
        fn fsetxattr(
            fd: i32,
            name: *const c_char,
            value: *const c_void,
            size: usize,
            flags: i32,
        ) -> i32;
        fn fremovexattr(fd: i32, name: *const c_char) -> i32;
        fn fchown(fd: i32, owner: u32, group: u32) -> i32;
    }

    fn attributes(file: &File) -> io::Result<BTreeMap<CString, Vec<u8>>> {
        let fd = file.as_raw_fd();
        let length = unsafe { flistxattr(fd, std::ptr::null_mut(), 0) };
        if length < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(95) {
                return Ok(BTreeMap::new());
            } // ENOTSUP
            return Err(error);
        }
        if length > 1024 * 1024 {
            return Err(io::Error::other("extended attributes too large"));
        }
        let mut names = vec![0u8; length as usize];
        if length != 0 {
            let read = unsafe { flistxattr(fd, names.as_mut_ptr().cast(), names.len()) };
            if read < 0 {
                return Err(io::Error::last_os_error());
            }
            names.truncate(read as usize);
        }
        let mut result = BTreeMap::new();
        for name in names
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
        {
            let name =
                CString::new(name).map_err(|_| io::Error::other("invalid attribute name"))?;
            let length = unsafe { fgetxattr(fd, name.as_ptr(), std::ptr::null_mut(), 0) };
            if length < 0 {
                return Err(io::Error::last_os_error());
            }
            if length > 1024 * 1024 {
                return Err(io::Error::other("extended attribute too large"));
            }
            let mut value = vec![0u8; length as usize];
            if length != 0 {
                let read =
                    unsafe { fgetxattr(fd, name.as_ptr(), value.as_mut_ptr().cast(), value.len()) };
                if read < 0 {
                    return Err(io::Error::last_os_error());
                }
                value.truncate(read as usize);
            }
            result.insert(name, value);
        }
        Ok(result)
    }

    pub(super) fn copy(original: &Path, replacement: &File) -> io::Result<()> {
        let source = File::open(original)?;
        let source_meta = source.metadata()?;
        let destination_meta = replacement.metadata()?;
        if (source_meta.uid(), source_meta.gid())
            != (destination_meta.uid(), destination_meta.gid())
            && unsafe {
                fchown(
                    replacement.as_raw_fd(),
                    source_meta.uid(),
                    source_meta.gid(),
                )
            } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let source_attrs = attributes(&source)?;
        let destination_attrs = attributes(replacement)?;
        for name in destination_attrs
            .keys()
            .filter(|name| !source_attrs.contains_key(*name))
        {
            if unsafe { fremovexattr(replacement.as_raw_fd(), name.as_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        for (name, value) in &source_attrs {
            if destination_attrs.get(name) == Some(value) {
                continue;
            }
            if unsafe {
                fsetxattr(
                    replacement.as_raw_fd(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn copy_security(original: &Path, replacement: &fs::File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    extern "C" {
        fn fcopyfile(from: i32, to: i32, state: *mut std::ffi::c_void, flags: u32) -> i32;
    }
    let source = fs::File::open(original)?;
    // COPYFILE_ACL | COPYFILE_STAT | COPYFILE_XATTR; no file data is copied.
    if unsafe {
        fcopyfile(
            source.as_raw_fd(),
            replacement.as_raw_fd(),
            std::ptr::null_mut(),
            7,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let source_metadata = source.metadata()?;
    let copied_metadata = replacement.metadata()?;
    if (
        source_metadata.uid(),
        source_metadata.gid(),
        source_metadata.mode() & 0o7777,
    ) != (
        copied_metadata.uid(),
        copied_metadata.gid(),
        copied_metadata.mode() & 0o7777,
    ) {
        return Err(io::Error::other("file security could not be preserved"));
    }
    Ok(())
}

#[cfg(all(not(windows), not(target_os = "linux"), not(target_os = "macos")))]
fn copy_security(_original: &Path, _replacement: &fs::File) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "file security preservation is unsupported",
    ))
}

// std::fs::Permissions on Windows covers only the read-only attribute. Copy
// the DACL and owner/group before any secrets are written to the new file.
#[cfg(windows)]
fn copy_security(original: &Path, replacement: &fs::File) -> io::Result<()> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    #[link(name = "advapi32")]
    extern "system" {
        fn GetFileSecurityW(
            name: *const u16,
            requested: u32,
            descriptor: *mut c_void,
            length: u32,
            needed: *mut u32,
        ) -> i32;
        fn GetSecurityDescriptorControl(
            descriptor: *const c_void,
            control: *mut u16,
            revision: *mut u32,
        ) -> i32;
        fn GetSecurityDescriptorOwner(
            descriptor: *const c_void,
            owner: *mut *mut c_void,
            defaulted: *mut i32,
        ) -> i32;
        fn GetSecurityDescriptorGroup(
            descriptor: *const c_void,
            group: *mut *mut c_void,
            defaulted: *mut i32,
        ) -> i32;
        fn GetSecurityDescriptorDacl(
            descriptor: *const c_void,
            present: *mut i32,
            dacl: *mut *mut c_void,
            defaulted: *mut i32,
        ) -> i32;
        fn GetSecurityDescriptorSacl(
            descriptor: *const c_void,
            present: *mut i32,
            sacl: *mut *mut c_void,
            defaulted: *mut i32,
        ) -> i32;
        fn SetSecurityInfo(
            handle: *mut c_void,
            object_type: u32,
            requested: u32,
            owner: *mut c_void,
            group: *mut c_void,
            dacl: *mut c_void,
            sacl: *mut c_void,
        ) -> u32;
    }
    const SECURITY: u32 = 0x1 | 0x2 | 0x4 | 0x10; // owner, group, DACL, integrity label
    const DACL_PROTECTED: u16 = 0x1000;
    const PROTECTED_DACL: u32 = 0x8000_0000;
    const UNPROTECTED_DACL: u32 = 0x2000_0000;
    let source: Vec<u16> = original.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut needed = 0u32;
    unsafe {
        GetFileSecurityW(
            source.as_ptr(),
            SECURITY,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
    }
    if needed == 0 || needed > 1024 * 1024 {
        return Err(io::Error::last_os_error());
    }
    let mut descriptor = vec![0u8; needed as usize];
    if unsafe {
        GetFileSecurityW(
            source.as_ptr(),
            SECURITY,
            descriptor.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut control = 0u16;
    let mut revision = 0u32;
    if unsafe {
        GetSecurityDescriptorControl(descriptor.as_ptr().cast(), &mut control, &mut revision)
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let protection = if control & DACL_PROTECTED != 0 {
        PROTECTED_DACL
    } else {
        UNPROTECTED_DACL
    };
    let mut owner = std::ptr::null_mut();
    let mut group = std::ptr::null_mut();
    let mut dacl = std::ptr::null_mut();
    let mut sacl = std::ptr::null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    // The owner, group and DACL pointers borrow from `descriptor`, which stays
    // alive until after SetSecurityInfo has consumed them.
    let descriptor_ptr = descriptor.as_ptr().cast();
    if unsafe { GetSecurityDescriptorOwner(descriptor_ptr, &mut owner, &mut defaulted) } == 0
        || unsafe { GetSecurityDescriptorGroup(descriptor_ptr, &mut group, &mut defaulted) } == 0
        || unsafe {
            GetSecurityDescriptorDacl(descriptor_ptr, &mut present, &mut dacl, &mut defaulted)
        } == 0
        || unsafe {
            GetSecurityDescriptorSacl(descriptor_ptr, &mut present, &mut sacl, &mut defaulted)
        } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let error = unsafe {
        SetSecurityInfo(
            replacement.as_raw_handle().cast(),
            1,
            SECURITY | protection,
            owner,
            group,
            dacl,
            sacl,
        )
    };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error as i32));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn reconnect_address_matches_loopback_listener_and_stays_local() {
        let mut config = crate::config::Config::default();
        config.network.port = 6200;
        for (bind, expected) in [
            (None, "http://127.0.0.1:6200/"),
            (Some("127.0.0.2"), "http://127.0.0.2:6200/"),
            (Some("127.0.0.1:6300"), "http://127.0.0.1:6300/"),
            (Some("::1"), "http://[::1]:6200/"),
            (Some("[::1]:6400"), "http://[::1]:6400/"),
            (Some("100.64.0.7:6500"), "http://127.0.0.1:6200/"),
            (Some("0.0.0.0:6500"), "http://127.0.0.1:6200/"),
            (Some("http://example.com/"), "http://127.0.0.1:6200/"),
        ] {
            config.network.bind = bind.map(str::to_owned);
            assert_eq!(super::desktop_url(&config), expected);
        }
    }

    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static RESTART_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn secret_operations_reject_unknown_members() {
        assert!(
            serde_json::from_str::<SecretChange>(r#"{"op":"clear","value":"unused"}"#).is_err()
        );
        assert!(serde_json::from_str::<SecretChange>(
            r#"{"op":"replace","value":"new","extra":true}"#
        )
        .is_err());
        assert!(serde_json::from_str::<SecretChange>(r#"{"op":"clear"}"#).is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_security_preserves_extended_attributes() {
        use std::ffi::{c_char, c_void};
        use std::os::fd::AsRawFd;
        extern "C" {
            fn fsetxattr(
                fd: i32,
                name: *const c_char,
                value: *const c_void,
                size: usize,
                flags: i32,
            ) -> i32;
            fn fgetxattr(fd: i32, name: *const c_char, value: *mut c_void, size: usize) -> isize;
        }
        let root =
            std::env::temp_dir().join(format!("peercarry-settings-xattr-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source");
        let destination = root.join("destination");
        let source_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&source)
            .unwrap();
        let destination_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&destination)
            .unwrap();
        let keep = b"user.peercarry.keep\0";
        let stale = b"user.peercarry.stale\0";
        let value = b"synthetic";
        let result = unsafe {
            fsetxattr(
                source_file.as_raw_fd(),
                keep.as_ptr().cast(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        };
        if result != 0 && io::Error::last_os_error().raw_os_error() == Some(95) {
            eprintln!("SKIP: test filesystem does not support extended attributes");
            return;
        }
        assert_eq!(result, 0);
        assert_eq!(
            unsafe {
                fsetxattr(
                    destination_file.as_raw_fd(),
                    stale.as_ptr().cast(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                )
            },
            0
        );
        copy_security(&source, &destination_file).unwrap();
        let mut received = [0u8; 9];
        assert_eq!(
            unsafe {
                fgetxattr(
                    destination_file.as_raw_fd(),
                    keep.as_ptr().cast(),
                    received.as_mut_ptr().cast(),
                    received.len(),
                )
            },
            9
        );
        assert_eq!(&received, value);
        assert_eq!(
            unsafe {
                fgetxattr(
                    destination_file.as_raw_fd(),
                    stale.as_ptr().cast(),
                    std::ptr::null_mut(),
                    0,
                )
            },
            -1
        );
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(61)); // ENODATA
    }

    #[cfg(windows)]
    #[test]
    fn copy_windows_permissions_for_synthetic_file() {
        let root =
            std::env::temp_dir().join(format!("peercarry-settings-acl-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source");
        let destination = root.join("destination");
        fs::write(&source, b"synthetic").unwrap();
        use std::os::windows::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .access_mode(0x4000_0000 | 0x0004_0000 | 0x0008_0000)
            .open(&destination)
            .unwrap();
        copy_security(&source, &file).expect("copy synthetic file security");
        file.write_all(b"new synthetic contents")
            .expect("write temporary file");
        file.sync_all().expect("sync temporary file");
        fs::set_permissions(&destination, fs::metadata(&source).unwrap().permissions())
            .expect("copy attributes");
        file.sync_all().expect("sync attributes");
        drop(file);
        fs::rename(&destination, &source).expect("replace synthetic source");
    }

    fn restart_spy() -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>> {
        Box::pin(async {
            RESTART_CALLS.fetch_add(1, Ordering::SeqCst);
            Json(json!({"ok": true})).into_response()
        })
    }

    #[tokio::test]
    async fn restart_advice_tracks_pending_startup_changes_across_saves() {
        use crate::{engine::Engine, model::Origin, store::Store};
        let root = std::env::temp_dir().join(format!(
            "peercarry-settings-advice-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let config = Arc::new(Config::default());
        let context = SettingsContext::new(root.join("config.toml"), config.clone(), restart_spy);
        let store = Arc::new(Store::open(&root.join("state.redb")).unwrap());
        let origin = Origin {
            host: "test".into(),
            node_id: "test".into(),
            addr: "http://127.0.0.1:1".into(),
            os: "test".into(),
        };
        let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone()).unwrap());
        let state = AppState {
            store,
            config,
            origin,
            engine,
        };
        for (changes, expected) in [
            (json!({"node.name":"Pending restart"}), true),
            (json!({"storage.download_dir":root.join("downloads")}), true),
            (json!({}), true),
            (json!({"updates.auto_check":false}), true),
            (json!({"node.name":null}), false),
        ] {
            let revision = context.read().unwrap().revision;
            let request = Request::builder()
                .method("POST")
                .body(axum::body::Body::from(
                    json!({"revision":revision,"changes":changes}).to_string(),
                ))
                .unwrap();
            let response = save(&state, &context, request).await.unwrap();
            assert_eq!(response["restart_recommended"], expected);
        }
    }

    #[tokio::test]
    async fn clearing_download_directory_applies_default_after_save() {
        use crate::{engine::Engine, model::Origin, store::Store};
        let root = std::env::temp_dir().join(format!(
            "peercarry-settings-download-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        let mut config = Config::default();
        config.storage.download_dir = Some(root.join("old-downloads"));
        fs::write(&path, config.to_toml().unwrap()).unwrap();
        let config = Arc::new(config);
        let mut context = SettingsContext::new(path, config.clone(), restart_spy);
        let default_dir = root.join("default-downloads");
        context.default_download_dir = Some(default_dir.clone());
        let revision = context.read().unwrap().revision;
        let store = Arc::new(Store::open(&root.join("state.redb")).unwrap());
        let origin = Origin {
            host: "test".into(),
            node_id: "test".into(),
            addr: "http://127.0.0.1:1".into(),
            os: "test".into(),
        };
        let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone()).unwrap());
        let state = AppState {
            store,
            config,
            origin,
            engine: engine.clone(),
        };
        let body = json!({"revision":revision,"changes":{"storage.download_dir":null}}).to_string();
        let request = Request::builder()
            .method("POST")
            .body(axum::body::Body::from(body))
            .unwrap();
        let response = save(&state, &context, request).await.unwrap();
        assert!(response["values"]["storage.download_dir"].is_null());
        assert_eq!(engine.download_dir(), default_dir);
    }

    #[tokio::test]
    async fn restart_requires_a_current_saved_revision_and_trusted_http_origin() {
        use crate::{engine::Engine, model::Origin, store::Store};
        use std::net::SocketAddr;
        RESTART_CALLS.store(0, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "peercarry-settings-restart-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("config.toml");
        let config = Arc::new(Config::default());
        let context = SettingsContext::new(path.clone(), config.clone(), restart_spy);
        let missing_revision = context.read().unwrap().revision;
        let store = Arc::new(Store::open(&root.join("state.redb")).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let origin = Origin {
            host: "test".into(),
            node_id: "test".into(),
            addr: base.clone(),
            os: "test".into(),
        };
        let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone()).unwrap());
        let state = AppState {
            store,
            config,
            origin,
            engine,
        };
        let app = routes(context.clone())
            .with_state(state)
            .layer(middleware::from_fn(outer_gate));
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let endpoint = format!("{base}{RESTART_PATH}");
        let unsaved = client
            .post(&endpoint)
            .json(&json!({"revision":missing_revision}))
            .send()
            .await
            .unwrap();
        assert_eq!(unsaved.status(), StatusCode::CONFLICT);
        assert_eq!(RESTART_CALLS.load(Ordering::SeqCst), 0);
        fs::write(&path, context.startup.to_toml().unwrap()).unwrap();
        let revision = context.read().unwrap().revision;
        let foreign = client
            .post(&endpoint)
            .header("origin", "https://untrusted.example")
            .json(&json!({"revision":revision}))
            .send()
            .await
            .unwrap();
        assert_eq!(foreign.status(), StatusCode::FORBIDDEN);
        let stale = client
            .post(&endpoint)
            .json(&json!({"revision":missing_revision}))
            .send()
            .await
            .unwrap();
        assert_eq!(stale.status(), StatusCode::CONFLICT);
        let extra = client
            .post(&endpoint)
            .json(&json!({"revision":revision,"extra":true}))
            .send()
            .await
            .unwrap();
        assert_eq!(extra.status(), StatusCode::BAD_REQUEST);
        assert_eq!(RESTART_CALLS.load(Ordering::SeqCst), 0);
        let allowed = client
            .post(&endpoint)
            .header("origin", &base)
            .header("sec-fetch-site", "same-origin")
            .json(&json!({"revision":revision}))
            .send()
            .await
            .unwrap();
        assert_eq!(allowed.status(), StatusCode::OK);
        assert_eq!(allowed.headers().get("cache-control").unwrap(), "no-store");
        assert_eq!(RESTART_CALLS.load(Ordering::SeqCst), 1);
        task.abort();
    }
}
