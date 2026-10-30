//! Dedicated TLS service. It never imports clipboard storage or exposes AI tools.
use crate::{Coordinator, Operation, Phase, Rect, Scope, Target};
use axum::{
    extract::{rejection::JsonRejection, DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::Cursor,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot, Mutex},
};
use tokio_rustls::{rustls, TlsAcceptor};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub id: String,
    pub kind: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub target: Target,
    pub view: Rect,
    pub viewport: Rect,
    pub fields: Vec<Field>,
    pub revision: String,
}
impl Snapshot {
    pub fn validate(&self) -> bool {
        self.target.frame == 0
            && !self.target.document.is_empty()
            && (self.target.origin.starts_with("https://")
                || self.target.origin == "http://127.0.0.1:5188")
            && self.view.valid()
            && self.viewport.valid()
            && self.viewport.x == 0
            && self.viewport.y == 0
            && self.viewport.width <= 8192
            && self.viewport.height <= 8192
            && u64::from(self.viewport.width) * u64::from(self.viewport.height) <= 16 * 1024 * 1024
            && self.view.x + self.view.width <= self.viewport.width
            && self.view.y + self.view.height <= self.viewport.height
            && !self.revision.is_empty()
            && self.revision.len() <= 128
            && self.fields.len() <= 64
            && self.fields.iter().all(|f| {
                !f.id.is_empty()
                    && f.id.len() <= 128
                    && matches!(f.kind.as_str(), "account" | "password" | "otp")
            })
            && self
                .fields
                .iter()
                .map(|f| &f.id)
                .collect::<HashSet<_>>()
                .len()
                == self.fields.len()
    }
}

pub struct BrowserCall {
    pub payload: Value,
    pub reply: oneshot::Sender<Result<Value, ()>>,
}
#[derive(Clone)]
pub struct Browser(pub mpsc::Sender<BrowserCall>);
impl Browser {
    pub async fn call(&self, payload: Value) -> Result<Value, ApiError> {
        let (tx, rx) = oneshot::channel();
        self.0
            .try_send(BrowserCall { payload, reply: tx })
            .map_err(|_| ApiError("BROWSER_UNAVAILABLE"))?;
        tokio::time::timeout(Duration::from_secs(8), rx)
            .await
            .map_err(|_| ApiError("BROWSER_TIMEOUT"))?
            .map_err(|_| ApiError("BROWSER_UNAVAILABLE"))?
            .map_err(|_| ApiError("TARGET_CHANGED"))
    }
}

pub struct ApiError(pub &'static str);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = if self.0 == "UNAUTHORIZED" {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::CONFLICT
        };
        (
            status,
            [("cache-control", "no-store")],
            Json(json!({"error":self.0})),
        )
            .into_response()
    }
}
impl From<crate::Error> for ApiError {
    fn from(e: crate::Error) -> Self {
        Self(match e {
            crate::Error::Unauthorized => "UNAUTHORIZED",
            crate::Error::Expired => "EXPIRED",
            crate::Error::Stale => "TARGET_CHANGED",
            crate::Error::Replay => "REPLAY",
            crate::Error::Scope => "OUT_OF_SCOPE",
            _ => "INVALID_STATE",
        })
    }
}
fn secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
fn same_secret(a: &str, b: &str) -> bool {
    // Compare fixed-size digests, never attacker-controlled length prefixes.
    Sha256::digest(a.as_bytes()) == Sha256::digest(b.as_bytes())
}

struct Inner {
    policy: Coordinator,
    id: u64,
    snapshot: Snapshot,
    pair: Option<String>,
    bearer: Option<String>,
    last_seen: u64,
}
#[derive(Clone)]
pub struct Service {
    inner: Arc<Mutex<Inner>>,
    serial: Arc<Mutex<()>>,
    browser: Browser,
    start: Instant,
}
impl Service {
    pub fn new(snapshot: Snapshot, browser: Browser) -> Result<(Self, String), ApiError> {
        if !snapshot.validate() {
            return Err(ApiError("INVALID_TARGET"));
        }
        let mut policy = Coordinator::default();
        let id =
            policy.request_manual(snapshot.target.clone(), "paired-phone".into(), 0, 300_000)?;
        let pair = secret();
        Ok((
            Self {
                inner: Arc::new(Mutex::new(Inner {
                    policy,
                    id,
                    snapshot,
                    pair: Some(pair.clone()),
                    bearer: None,
                    last_seen: 0,
                })),
                serial: Arc::new(Mutex::new(())),
                browser,
                start: Instant::now(),
            },
            pair,
        ))
    }
    fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
    pub async fn invalidate(&self) {
        let mut s = self.inner.lock().await;
        let id = s.id;
        let _ = s.policy.invalidate(id);
    }
    pub async fn revoke(&self) {
        let mut s = self.inner.lock().await;
        s.bearer = None;
        s.pair = None;
        let id = s.id;
        let _ = s.policy.cancel(id, "manual");
    }
    pub async fn watchdog(&self) {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let mut s = self.inner.lock().await;
            let now = self.now();
            let id = s.id;
            if now.saturating_sub(s.last_seen) > 10_000 && s.bearer.is_some() {
                // A lost foreground connection permanently revokes this pairing.
                s.bearer = None;
                let _ = s.policy.invalidate(id);
            }
            let _ = s.policy.status(id, "manual", now);
        }
    }
    pub fn router(&self) -> Router {
        Router::new()
            .route("/v1/pair", post(pair))
            .route("/v1/command", post(command))
            .layer(DefaultBodyLimit::max(16 * 1024))
            .with_state(self.clone())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairRequest {
    secret: String,
}
async fn pair(
    State(host): State<Service>,
    headers: HeaderMap,
    body: Result<Json<PairRequest>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let Json(req) = body.map_err(|_| ApiError("INVALID_REQUEST"))?;
    if headers.contains_key("origin") {
        return Err(ApiError("UNAUTHORIZED"));
    }
    let mut s = host.inner.lock().await;
    if host.now() >= 60_000 || !s.pair.as_ref().is_some_and(|p| same_secret(p, &req.secret)) {
        return Err(ApiError("UNAUTHORIZED"));
    }
    s.pair = None;
    let token = secret();
    s.bearer = Some(token.clone());
    s.last_seen = host.now();
    Ok((
        [("cache-control", "no-store")],
        Json(json!({"token": token})),
    ))
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    State,
    Approve {
        epoch: u64,
    },
    Fill {
        epoch: u64,
        command: u64,
        field: String,
        value: String,
    },
    Click {
        epoch: u64,
        command: u64,
        x: u32,
        y: u32,
        revision: String,
    },
    Frame {
        epoch: u64,
    },
    Expand {
        epoch: u64,
    },
    Refresh,
    HandBack {
        epoch: u64,
    },
    Cancel,
}

async fn command(
    State(host): State<Service>,
    headers: HeaderMap,
    body: Result<Json<Command>, JsonRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let Json(req) = body.map_err(|_| ApiError("INVALID_REQUEST"))?;
    let _serial = host.serial.try_lock().map_err(|_| ApiError("BUSY"))?;
    let mut s = host.inner.lock().await;
    let bearer = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .unwrap_or("");
    if headers.contains_key("origin") || !s.bearer.as_ref().is_some_and(|b| same_secret(b, bearer))
    {
        return Err(ApiError("UNAUTHORIZED"));
    }
    let now = host.now();
    if now.saturating_sub(s.last_seen) > 10_000 {
        s.bearer = None;
        let id = s.id;
        let _ = s.policy.invalidate(id);
        return Err(ApiError("UNAUTHORIZED"));
    }
    s.last_seen = now;
    let id = s.id;
    let snapshot = s.snapshot.clone();
    let current = s.policy.status(id, "manual", now)?;
    let check_epoch = |epoch| {
        if epoch == current.epoch {
            Ok(())
        } else {
            Err(ApiError("TARGET_CHANGED"))
        }
    };
    let mut extra = json!({});
    match req {
        Command::State => {}
        Command::Approve { epoch } => {
            check_epoch(epoch)?;
            s.policy.approve(
                id,
                "paired-phone",
                &snapshot.target,
                Scope {
                    view: snapshot.view,
                    clicks: Some(snapshot.view),
                    fields: snapshot.fields.iter().map(|f| f.id.clone()).collect(),
                },
                now,
            )?;
        }
        Command::Expand { epoch } => {
            s.policy
                .approve_view(id, "paired-phone", epoch, snapshot.viewport, now)?;
            s.snapshot.view = snapshot.viewport;
        }
        Command::Refresh => {
            if current.phase != Phase::Suspended {
                return Err(ApiError("INVALID_STATE"));
            }
            drop(s);
            let fresh = host
                .browser
                .call(json!({"op":"inspect", "target":snapshot.target}))
                .await?;
            let fresh: Snapshot =
                serde_json::from_value(fresh).map_err(|_| ApiError("INVALID_TARGET"))?;
            if !fresh.validate() {
                return Err(ApiError("INVALID_TARGET"));
            }
            s = host.inner.lock().await;
            s.policy
                .rebind_manual(id, fresh.target.clone(), host.now())?;
            s.snapshot = fresh;
        }
        Command::Fill {
            epoch,
            command,
            field,
            value,
        } => {
            let value = zeroize::Zeroizing::new(value);
            if value.is_empty() || value.len() > 4096 {
                return Err(ApiError("INVALID_VALUE"));
            }
            s.policy.authorize(
                id,
                "paired-phone",
                &snapshot.target,
                epoch,
                command,
                Operation::Fill(&field),
                now,
            )?;
            drop(s);
            let outcome = host.browser.call(json!({"op":"fill", "target": snapshot.target, "revision": snapshot.revision, "field": field, "value": *value})).await;
            s = host.inner.lock().await;
            if outcome.is_err() {
                let _ = s.policy.invalidate(id);
                return Err(ApiError("FILL_UNCONFIRMED"));
            }
        }
        Command::Click {
            epoch,
            command,
            x,
            y,
            revision,
        } => {
            if revision != snapshot.revision {
                return Err(ApiError("TARGET_CHANGED"));
            }
            s.policy.authorize(
                id,
                "paired-phone",
                &snapshot.target,
                epoch,
                command,
                Operation::Click { x, y },
                now,
            )?;
            drop(s);
            let outcome = host.browser.call(json!({"op":"click", "target":snapshot.target, "revision":revision, "x":x, "y":y})).await;
            s = host.inner.lock().await;
            // Clicks can change layout synchronously. Always require a fresh binding.
            let _ = s.policy.invalidate(id);
            if outcome.is_err() {
                return Err(ApiError("CLICK_UNCONFIRMED"));
            }
        }
        Command::Frame { epoch } => {
            check_epoch(epoch)?;
            if current.phase != Phase::Controlling {
                return Err(ApiError("INVALID_STATE"));
            }
            drop(s);
            let capture = host
                .browser
                .call(
                    json!({"op":"capture", "target":snapshot.target, "revision":snapshot.revision}),
                )
                .await;
            s = host.inner.lock().await;
            let capture = match capture {
                Ok(c) => c,
                Err(_) => {
                    let _ = s.policy.invalidate(id);
                    return Err(ApiError("TARGET_CHANGED"));
                }
            };
            let png = capture
                .get("png")
                .and_then(Value::as_str)
                .ok_or(ApiError("INVALID_CAPTURE"))?;
            let bytes = STANDARD
                .decode(png)
                .map_err(|_| ApiError("INVALID_CAPTURE"))?;
            if bytes.len() > 12 * 1024 * 1024 {
                return Err(ApiError("INVALID_CAPTURE"));
            }
            let mut reader =
                image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(8192);
            limits.max_image_height = Some(8192);
            limits.max_alloc = Some(64 * 1024 * 1024);
            reader.limits(limits);
            let decoded = reader.decode().map_err(|_| ApiError("INVALID_CAPTURE"))?;
            // Never stretch mismatched capture geometry into an approved rectangle.
            // Allow at most one physical pixel of device-scale rounding.
            let lhs = u64::from(decoded.width()) * u64::from(snapshot.viewport.height);
            let rhs = u64::from(decoded.height()) * u64::from(snapshot.viewport.width);
            if lhs.abs_diff(rhs) > u64::from(snapshot.viewport.width.max(snapshot.viewport.height))
            {
                let _ = s.policy.invalidate(id);
                return Err(ApiError("TARGET_CHANGED"));
            }
            let pixels = decoded
                .resize_exact(
                    snapshot.viewport.width,
                    snapshot.viewport.height,
                    image::imageops::FilterType::Nearest,
                )
                .to_rgba8();
            let cropped = s.policy.crop_rgba(
                id,
                "paired-phone",
                &snapshot.target,
                epoch,
                pixels.width(),
                pixels.height(),
                pixels.as_raw(),
                host.now(),
            )?;
            let view = s.snapshot.view;
            let image = image::RgbaImage::from_raw(view.width, view.height, cropped)
                .ok_or(ApiError("INVALID_CAPTURE"))?;
            let mut encoded = Cursor::new(Vec::new());
            image
                .write_to(&mut encoded, image::ImageFormat::Png)
                .map_err(|_| ApiError("INVALID_CAPTURE"))?;
            extra = json!({"png":STANDARD.encode(encoded.into_inner()), "view":view, "revision":snapshot.revision});
        }
        Command::HandBack { epoch } => {
            let next = s.policy.hand_back(id, "paired-phone", epoch, now)?;
            drop(s);
            let closed = host
                .browser
                .call(json!({"op":"revoke", "target":snapshot.target}))
                .await;
            s = host.inner.lock().await;
            if closed.is_err() {
                let _ = s.policy.invalidate(id);
                return Err(ApiError("REVOCATION_UNCONFIRMED"));
            }
            s.policy.revoked_ack(id, next, host.now())?;
            s.policy.manual_complete(id, next, host.now())?;
        }
        Command::Cancel => {
            s.policy.cancel(id, "manual")?;
            s.bearer = None;
            drop(s);
            // Authorization is already revoked even if browser cleanup cannot ack.
            let closed = host
                .browser
                .call(json!({"op":"revoke", "target":snapshot.target}))
                .await;
            s = host.inner.lock().await;
            if closed.is_err() {
                return Err(ApiError("REVOCATION_UNCONFIRMED"));
            }
        }
    }
    let status = s.policy.status(id, "manual", host.now())?;
    let response = json!({"status":status, "target":s.snapshot.target, "fields":s.snapshot.fields,
        "view":s.snapshot.view, "viewport":s.snapshot.viewport, "revision":s.snapshot.revision,
        "remaining_ms":300_000u64.saturating_sub(host.now()), "data":extra});
    Ok(([("cache-control", "no-store")], Json(response)))
}

pub struct TlsListener {
    tcp: TcpListener,
    acceptor: TlsAcceptor,
}
impl axum::serve::Listener for TlsListener {
    type Io = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            if let Ok((stream, addr)) = self.tcp.accept().await {
                if let Ok(Ok(tls)) =
                    tokio::time::timeout(Duration::from_secs(3), self.acceptor.accept(stream)).await
                {
                    return (tls, addr);
                }
            } else {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}
pub async fn listen(
    bind: SocketAddr,
) -> Result<(TlsListener, String, SocketAddr), Box<dyn std::error::Error + Send + Sync>> {
    let ip = bind.ip();
    let private = match ip {
        std::net::IpAddr::V4(ip) => {
            ip.is_private()
                || ip.is_loopback()
                || (ip.octets()[0] == 100 && (64..=127).contains(&ip.octets()[1]))
        }
        std::net::IpAddr::V6(ip) => ip.is_loopback() || (ip.segments()[0] & 0xfe00 == 0xfc00),
    };
    if !private {
        return Err("Bind an explicit private or Tailscale address".into());
    }
    let cert = rcgen::generate_simple_self_signed(vec![ip.to_string()])?;
    let der = cert.cert.der().clone();
    let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(vec![der.clone()], key.into())?;
    let tcp = TcpListener::bind(bind).await?;
    let addr = tcp.local_addr()?;
    Ok((
        TlsListener {
            tcp,
            acceptor: TlsAcceptor::from(Arc::new(config)),
        },
        STANDARD.encode(der),
        addr,
    ))
}
