//! Opt-in, certificate-pinned mobile gateway. A TCP relay forwards the TLS
//! stream unchanged; only the phone and this process can read its contents.
//! This router deliberately does not expose clipboard actions, settings, AI,
//! arbitrary file paths, or the desktop's shared authentication token.
use crate::{model::EntryKind, server::AppState, uploads::Owner};
use axum::{
    extract::{DefaultBodyLimit, Extension, Path, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::{future::BoxFuture, stream::FuturesUnordered, StreamExt};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{net::TcpListener, sync::Mutex, task::JoinHandle};
use tokio_rustls::{rustls, TlsAcceptor};
use uuid::Uuid;

pub const DEFAULT_PORT: u16 = 5197;
const PAIR_TTL: Duration = Duration::from_secs(300);
const SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_DEVICES: usize = 8;

// No Debug derives on tickets or credentials: these must never enter logs.
#[derive(Clone, Serialize, Deserialize)]
pub struct ConnectionTicket {
    pub kind: String,
    pub version: u8,
    pub host: String,
    pub lan: Vec<String>,
    pub relay: Option<String>,
    pub certificate: String,
    pub secret: String,
    pub expires_at: i64,
}

#[derive(Clone, Serialize)]
struct Credentials {
    device_id: String,
    token: String,
    expires_at: i64,
}
struct PendingPair {
    secret_hash: [u8; 32],
    deadline: Instant,
    used: Option<(String, Credentials)>,
}
struct Device {
    name: String,
    token_hash: [u8; 32],
    deadline: Instant,
    expires_at: i64,
}
#[derive(Default)]
struct Auth {
    pair: Option<PendingPair>,
    devices: HashMap<String, Device>,
}
type SharedAuth = Arc<Mutex<Auth>>;

fn secret() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}
fn hash(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}
fn valid_secret(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn failure(status: StatusCode, code: &'static str) -> Response {
    (status, Json(json!({"error": code}))).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PairRequest {
    secret: String,
    request_id: String,
    name: String,
}
async fn pair(State(auth): State<SharedAuth>, Json(input): Json<PairRequest>) -> Response {
    if !valid_secret(&input.secret)
        || !valid_secret(&input.request_id)
        || input.name.trim().is_empty()
        || input.name.len() > 80
        || input.name.chars().any(char::is_control)
    {
        return failure(StatusCode::BAD_REQUEST, "INVALID_PAIRING");
    }
    let mut auth = auth.lock().await;
    let Some(pending) = auth.pair.as_ref() else {
        return failure(StatusCode::UNAUTHORIZED, "PAIRING_EXPIRED");
    };
    if Instant::now() >= pending.deadline || hash(&input.secret) != pending.secret_hash {
        return failure(StatusCode::UNAUTHORIZED, "PAIRING_EXPIRED");
    }
    if let Some((request_id, credentials)) = &pending.used {
        // A lost response may be retried over the other route. The same QR
        // cannot authorize a different phone, or resurrect a revoked device.
        return if request_id == &input.request_id
            && auth.devices.contains_key(&credentials.device_id)
        {
            Json(credentials.clone()).into_response()
        } else {
            failure(StatusCode::CONFLICT, "PAIRING_USED")
        };
    }
    auth.devices.retain(|_, d| Instant::now() < d.deadline);
    if auth.devices.len() >= MAX_DEVICES {
        return failure(StatusCode::CONFLICT, "DEVICE_LIMIT");
    }
    let credentials = Credentials {
        device_id: Uuid::new_v4().to_string(),
        token: secret(),
        expires_at: chrono::Utc::now().timestamp() + SESSION_TTL.as_secs() as i64,
    };
    auth.devices.insert(
        credentials.device_id.clone(),
        Device {
            name: input.name.trim().to_owned(),
            token_hash: hash(&credentials.token),
            deadline: Instant::now() + SESSION_TTL,
            expires_at: credentials.expires_at,
        },
    );
    auth.pair.as_mut().unwrap().used = Some((input.request_id, credentials.clone()));
    Json(credentials).into_response()
}

async fn authorize(State(auth): State<SharedAuth>, mut req: Request, next: Next) -> Response {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| valid_secret(v));
    let Some(token) = token else {
        return failure(StatusCode::UNAUTHORIZED, "PAIRING_REQUIRED");
    };
    let digest = hash(token);
    let owner = auth.lock().await.devices.iter().find_map(|(id, d)| {
        (Instant::now() < d.deadline && d.token_hash == digest).then(|| Owner(id.clone()))
    });
    let Some(owner) = owner else {
        return failure(StatusCode::UNAUTHORIZED, "PAIRING_REQUIRED");
    };
    req.extensions_mut().insert(owner);
    next.run(req).await
}

async fn no_browser(req: Request, next: Next) -> Response {
    // Only native paired clients use this surface. In particular, a website
    // must not use the loopback FRP leg to acquire a mobile session.
    if req.headers().contains_key(header::ORIGIN) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let mut response = next.run(req).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

async fn entries(State(app): State<AppState>) -> Response {
    let Ok(entries) = app.store.list(200, None) else {
        return failure(StatusCode::INTERNAL_SERVER_ERROR, "HISTORY_UNAVAILABLE");
    };
    let mut bounded = Vec::new();
    let mut size = 0;
    for mut entry in entries {
        // History remains metadata-only for binaries; never send local paths
        // or inline image bytes speculatively to the phone.
        for blob in [&mut entry.blob, &mut entry.thumb].into_iter().flatten() {
            blob.inline = None;
        }
        for file in entry.files.iter_mut().flatten() {
            file.path.clear();
        }
        entry.origin.addr.clear();
        size += serde_json::to_vec(&entry).map_or(8 * 1024 * 1024 + 1, |v| v.len() + 1);
        if size > 8 * 1024 * 1024 {
            break;
        }
        bounded.push(entry);
    }
    Json(bounded).into_response()
}

async fn text_entry(
    State(app): State<AppState>,
    Extension(owner): Extension<Owner>,
    Extension(serial): Extension<Arc<Mutex<()>>>,
    Json(mut entry): Json<crate::model::Entry>,
) -> Response {
    if entry.kind != EntryKind::Text
        || Uuid::parse_str(&entry.id).is_err()
        || entry
            .text
            .as_ref()
            .is_none_or(|s| s.trim().is_empty() || s.len() > 65536)
        || entry.files.is_some()
        || entry.blob.is_some()
        || entry.thumb.is_some()
    {
        return failure(StatusCode::BAD_REQUEST, "INVALID_TEXT");
    }
    // Route retry must not overwrite an existing entry belonging to a
    // different sender, nor replace the payload of an earlier request.
    let _serial = serial.lock().await;
    match app.store.get(&entry.id) {
        Ok(Some(old)) if old.origin.node_id == owner.0 && old.text == entry.text => {
            return Json(json!({"id":entry.id,"accepted":0})).into_response();
        }
        Ok(Some(_)) => return failure(StatusCode::CONFLICT, "ENTRY_EXISTS"),
        Err(_) => return failure(StatusCode::INTERNAL_SERVER_ERROR, "HISTORY_UNAVAILABLE"),
        Ok(None) => {}
    }
    entry.origin = crate::model::Origin {
        host: "paired-phone".into(),
        node_id: owner.0,
        addr: String::new(),
        os: "mobile".into(),
    };
    entry.created_at = chrono::Utc::now();
    entry.size = entry.text.as_ref().unwrap().len() as u64;
    entry.pinned = false;
    crate::server::push_entry(State(app), Json(entry))
        .await
        .into_response()
}

type TlsStream = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;
struct TlsListener {
    tcp: TcpListener,
    acceptor: TlsAcceptor,
    pending: FuturesUnordered<BoxFuture<'static, Option<(TlsStream, SocketAddr)>>>,
}
impl axum::serve::Listener for TlsListener {
    type Io = TlsStream;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            tokio::select! {
                connection = self.tcp.accept(), if self.pending.len() < 32 => {
                    if let Ok((tcp, addr)) = connection {
                        let acceptor = self.acceptor.clone();
                        self.pending.push(Box::pin(async move {
                            tokio::time::timeout(Duration::from_secs(5), acceptor.accept(tcp))
                                .await.ok()?.ok().map(|tls| (tls, addr))
                        }));
                    } else {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
                ready = self.pending.next(), if !self.pending.is_empty() => {
                    if let Some(Some(connection)) = ready { return connection; }
                }
            }
        }
    }
    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}

pub struct Gateway {
    auth: SharedAuth,
    task: JoinHandle<()>,
    pub address: SocketAddr,
    host: String,
    certificate: String,
    lan: Vec<String>,
    relay: Option<String>,
}
impl Drop for Gateway {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Gateway {
    /// Private transfer service, also used by isolated protocol acceptance tests.
    pub async fn start(
        mut app: AppState,
        bind: SocketAddr,
        lan_ips: Vec<IpAddr>,
        relay: Option<String>,
    ) -> anyhow::Result<Self> {
        let relay = relay.map(|v| relay_url(&v)).transpose()?;
        let mut names: Vec<String> = lan_ips.iter().map(ToString::to_string).collect();
        names.push("127.0.0.1".into());
        if let Some(url) = &relay {
            names.push(
                reqwest::Url::parse(url)?
                    .host_str()
                    .unwrap()
                    .trim_matches(['[', ']'])
                    .into(),
            );
        }
        let cert = rcgen::generate_simple_self_signed(names)?;
        let der = cert.cert.der().clone();
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
        let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(vec![der.clone()], key.into())?;
        let tcp = TcpListener::bind(bind).await?;
        let address = tcp.local_addr()?;
        let lan = lan_ips
            .iter()
            .map(|ip| format!("https://{}", SocketAddr::new(*ip, address.port())))
            .collect();
        let listener = TlsListener {
            tcp,
            acceptor: TlsAcceptor::from(Arc::new(tls)),
            pending: FuturesUnordered::new(),
        };
        let auth = Arc::new(Mutex::new(Auth::default()));
        let host = app.origin.host.clone();
        let mut config = (*app.config).clone();
        // QR confirmation grants this separate gateway's upload capability;
        // it never enables the legacy plaintext upload endpoint.
        config.mobile.enabled = true;
        config.mobile.devices.clear();
        config.mobile.validate().map_err(anyhow::Error::msg)?;
        app.config = Arc::new(config);
        let api = Router::new()
            .route("/v1/hello", get(crate::server::hello))
            .route("/v1/entries", get(entries).post(text_entry))
            .layer(Extension(Arc::new(Mutex::new(()))))
            .layer(DefaultBodyLimit::max(128 * 1024))
            .merge(crate::uploads::paired_router(app.clone()))
            .with_state(app)
            .layer(middleware::from_fn_with_state(auth.clone(), authorize))
            .layer(middleware::from_fn(crate::server::track_activity));
        let routes = Router::new()
            .route("/v1/pair", post(pair))
            .layer(DefaultBodyLimit::max(4096))
            .with_state(auth.clone())
            .merge(api)
            .layer(middleware::from_fn(no_browser));
        let task = tokio::spawn(async move {
            if axum::serve(listener, routes).await.is_err() {
                tracing::error!("mobile TLS listener stopped");
            }
        });
        Ok(Self {
            auth,
            task,
            address,
            host,
            certificate: STANDARD.encode(der),
            lan,
            relay,
        })
    }

    pub async fn ticket(&self) -> ConnectionTicket {
        let secret = secret();
        self.auth.lock().await.pair = Some(PendingPair {
            secret_hash: hash(&secret),
            deadline: Instant::now() + PAIR_TTL,
            used: None,
        });
        ConnectionTicket {
            kind: "peercarry-connect".into(),
            version: 1,
            host: self.host.clone(),
            lan: self.lan.clone(),
            relay: self.relay.clone(),
            certificate: self.certificate.clone(),
            secret,
            expires_at: chrono::Utc::now().timestamp() + PAIR_TTL.as_secs() as i64,
        }
    }
    pub async fn revoke(&self, id: &str) {
        self.auth.lock().await.devices.remove(id);
    }
    pub async fn stop(&self) {
        *self.auth.lock().await = Auth::default();
        self.task.abort();
        while !self.task.is_finished() {
            tokio::task::yield_now().await;
        }
    }
    async fn status(&self) -> Value {
        let mut auth = self.auth.lock().await;
        auth.devices.retain(|_, d| Instant::now() < d.deadline);
        let devices: Vec<_> = auth
            .devices
            .iter()
            .map(|(id, d)| json!({"id":id,"name":d.name,"expires_at":d.expires_at}))
            .collect();
        let pairing = match auth.pair.as_ref() {
            Some(p) if Instant::now() >= p.deadline => "expired",
            Some(p) if p.used.is_some() => "used",
            Some(_) => "awaiting",
            None => "none",
        };
        json!({"running":!self.task.is_finished(),"port":self.address.port(),"lan":self.lan,"relay":self.relay,"devices":devices,"pairing":pairing})
    }
}

fn relay_url(value: &str) -> anyhow::Result<String> {
    let url = reqwest::Url::parse(value)?;
    anyhow::ensure!(
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.path() == "/"
            && value.len() < 256,
        "Use an HTTPS relay origin without credentials, paths or query parameters"
    );
    Ok(url.as_str().trim_end_matches('/').to_owned())
}
fn lan_ips() -> Vec<IpAddr> {
    let mut ips: Vec<_> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .map(|a| a.ip())
        .filter(|ip| matches!(ip, IpAddr::V4(ip) if ip.is_private()))
        .collect();
    ips.sort();
    ips.dedup();
    ips.truncate(8);
    ips
}
#[derive(Clone)]
struct Manager {
    app: AppState,
    gateway: Arc<Mutex<Option<Gateway>>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartRequest {
    #[serde(default)]
    relay: Option<String>,
}

async fn create_ticket(State(m): State<Manager>, Json(input): Json<StartRequest>) -> Response {
    let relay = match input
        .relay
        .filter(|s| !s.trim().is_empty())
        .map(|v| relay_url(v.trim()))
        .transpose()
    {
        Ok(url) => url,
        Err(_) => return failure(StatusCode::BAD_REQUEST, "INVALID_RELAY_URL"),
    };
    let mut gateway = m.gateway.lock().await;
    if let Some(g) = gateway.as_ref() {
        if g.relay != relay {
            return failure(StatusCode::CONFLICT, "STOP_BEFORE_CHANGING_RELAY");
        }
    } else {
        let ips = lan_ips();
        if ips.is_empty() && relay.is_none() {
            return failure(StatusCode::CONFLICT, "NO_LAN_ADDRESS");
        }
        match Gateway::start(
            m.app.clone(),
            SocketAddr::from(([0, 0, 0, 0], DEFAULT_PORT)),
            ips,
            relay,
        )
        .await
        {
            Ok(g) => *gateway = Some(g),
            Err(_) => {
                return failure(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "MOBILE_LISTENER_UNAVAILABLE",
                )
            }
        }
    }
    let ticket = gateway.as_ref().unwrap().ticket().await;
    let payload = serde_json::to_string(&ticket).unwrap();
    let Ok(code) = qrcode::QrCode::new(payload.as_bytes()) else {
        return failure(StatusCode::INTERNAL_SERVER_ERROR, "QR_TOO_LARGE");
    };
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(360, 360)
        .build();
    Json(json!({"ticket":ticket,"qr_svg":svg})).into_response()
}
async fn status(State(m): State<Manager>) -> Json<Value> {
    let gateway = m.gateway.lock().await;
    Json(match gateway.as_ref() {
        Some(g) => g.status().await,
        None => json!({"running":false,"devices":[],"lan":[],"relay":null}),
    })
}
async fn revoke(State(m): State<Manager>, Path(id): Path<String>) -> StatusCode {
    if let Some(g) = m.gateway.lock().await.as_ref() {
        g.revoke(&id).await;
    }
    StatusCode::NO_CONTENT
}
async fn stop(State(m): State<Manager>) -> StatusCode {
    if let Some(g) = m.gateway.lock().await.take() {
        g.stop().await;
    }
    StatusCode::NO_CONTENT
}
async fn local_headers(req: Request, next: Next) -> Response {
    let mut result = next.run(req).await;
    result
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    result
}
pub(crate) fn local_router(app: AppState) -> Router<AppState> {
    Router::new()
        .route("/v1/mobile/status", get(status))
        .route("/v1/mobile/pairing", post(create_ticket))
        .route("/v1/mobile/devices/{id}", delete(revoke))
        .route("/v1/mobile/stop", post(stop))
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn(crate::ai_monitor::local_gate))
        .layer(middleware::from_fn(local_headers))
        .with_state(Manager {
            app,
            gateway: Arc::new(Mutex::new(None)),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn expired_and_replaced_codes_cannot_pair() {
        let auth = Arc::new(Mutex::new(Auth::default()));
        auth.lock().await.pair = Some(PendingPair {
            secret_hash: hash(&"a".repeat(64)),
            deadline: Instant::now() - Duration::from_secs(1),
            used: None,
        });
        let input = || PairRequest {
            secret: "a".repeat(64),
            request_id: "c".repeat(64),
            name: "synthetic".into(),
        };
        assert_eq!(
            pair(State(auth.clone()), Json(input())).await.status(),
            StatusCode::UNAUTHORIZED
        );
        auth.lock().await.pair = Some(PendingPair {
            secret_hash: hash(&"b".repeat(64)),
            deadline: Instant::now() + PAIR_TTL,
            used: None,
        });
        assert_eq!(
            pair(State(auth), Json(input())).await.status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn used_pair_response_cannot_restore_revoked_authorization() {
        let auth = Arc::new(Mutex::new(Auth::default()));
        auth.lock().await.pair = Some(PendingPair {
            secret_hash: hash(&"a".repeat(64)),
            deadline: Instant::now() + PAIR_TTL,
            used: None,
        });
        let input = || PairRequest {
            secret: "a".repeat(64),
            request_id: "b".repeat(64),
            name: "synthetic".into(),
        };
        assert_eq!(
            pair(State(auth.clone()), Json(input())).await.status(),
            StatusCode::OK
        );
        auth.lock().await.devices.clear();
        assert_eq!(
            pair(State(auth), Json(input())).await.status(),
            StatusCode::CONFLICT
        );
    }
    #[test]
    fn relay_requires_bare_https_origin() {
        assert_eq!(
            relay_url("https://relay.example.com:65197/").unwrap(),
            "https://relay.example.com:65197"
        );
        for bad in [
            "http://relay.example.com",
            "https://u:p@relay.example.com",
            "https://relay.example.com/path",
            "https://relay.example.com/?token=x",
            "file:///tmp/x",
        ] {
            assert!(relay_url(bad).is_err());
        }
    }
}
