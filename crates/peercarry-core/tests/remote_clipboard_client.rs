use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use peercarry_core::{
    client::PeerClient,
    config::Config,
    model::{Entry, EntryKind, Origin},
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tokio::net::TcpListener;

#[derive(Clone)]
enum CaptureMode {
    Entry(Entry),
    Status(StatusCode),
    Redirect,
}

#[derive(Clone)]
struct MockState {
    expected_token: Option<String>,
    mode: CaptureMode,
    capture_hits: Arc<AtomicUsize>,
    redirect_hits: Arc<AtomicUsize>,
}

async fn capture(State(state): State<MockState>, headers: HeaderMap) -> Response {
    state.capture_hits.fetch_add(1, Ordering::SeqCst);
    if let Some(expected) = &state.expected_token {
        let actual = headers
            .get("x-syncclip-token")
            .and_then(|value| value.to_str().ok());
        if actual != Some(expected.as_str()) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
    }

    match state.mode {
        CaptureMode::Entry(ref entry) => Json(entry).into_response(),
        CaptureMode::Status(status) => status.into_response(),
        CaptureMode::Redirect => Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, "/redirect-target")
            .body(axum::body::Body::empty())
            .unwrap(),
    }
}

async fn redirect_target(State(state): State<MockState>) -> Response {
    state.redirect_hits.fetch_add(1, Ordering::SeqCst);
    StatusCode::OK.into_response()
}

async fn spawn_mock(state: MockState) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/v1/actions/capture", post(capture))
        .route("/redirect-target", get(redirect_target))
        .with_state(state);
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, task)
}

fn synthetic_entry() -> Entry {
    Entry {
        id: "synthetic-entry".into(),
        kind: EntryKind::Text,
        origin: Origin {
            host: "mock-peer".into(),
            node_id: "mock-node".into(),
            addr: "http://127.0.0.1:1".into(),
            os: "test".into(),
        },
        created_at: Utc::now(),
        text: Some("captured remotely".into()),
        blob: None,
        thumb: None,
        files: None,
        preview: "captured remotely".into(),
        pinned: false,
        size: 17,
        holder: None,
    }
}

fn client_with_token(token: Option<&str>) -> PeerClient {
    let mut config = Config::default();
    config.network.auth_token = token.map(str::to_owned);
    PeerClient::new(&config).unwrap()
}

#[tokio::test]
async fn capture_remote_sends_shared_token_and_returns_entry() {
    let token = "synthetic-shared-token";
    let state = MockState {
        expected_token: Some(token.into()),
        mode: CaptureMode::Entry(synthetic_entry()),
        capture_hits: Arc::new(AtomicUsize::new(0)),
        redirect_hits: Arc::new(AtomicUsize::new(0)),
    };
    let hits = state.capture_hits.clone();
    let (addr, task) = spawn_mock(state).await;

    let entry = client_with_token(Some(token))
        .capture_remote(&addr)
        .await
        .unwrap();
    assert_eq!(entry.id, "synthetic-entry");
    assert_eq!(entry.text.as_deref(), Some("captured remotely"));
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    task.abort();
}

#[tokio::test]
async fn capture_remote_explains_remote_capture_permission_denial() {
    let state = MockState {
        expected_token: None,
        mode: CaptureMode::Status(StatusCode::FORBIDDEN),
        capture_hits: Arc::new(AtomicUsize::new(0)),
        redirect_hits: Arc::new(AtomicUsize::new(0)),
    };
    let (addr, task) = spawn_mock(state).await;

    let error = client_with_token(None)
        .capture_remote(&addr)
        .await
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("403 Forbidden"));
    assert!(message.contains("node.allow_remote_capture"));

    task.abort();
}

#[tokio::test]
async fn capture_remote_reports_unauthorized_response() {
    let state = MockState {
        expected_token: None,
        mode: CaptureMode::Status(StatusCode::UNAUTHORIZED),
        capture_hits: Arc::new(AtomicUsize::new(0)),
        redirect_hits: Arc::new(AtomicUsize::new(0)),
    };
    let (addr, task) = spawn_mock(state).await;

    let error = client_with_token(Some("wrong-token"))
        .capture_remote(&addr)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("device authentication failed"));

    task.abort();
}

#[tokio::test]
async fn capture_remote_does_not_fallback_when_clipboard_is_empty() {
    let state = MockState {
        expected_token: None,
        mode: CaptureMode::Status(StatusCode::UNPROCESSABLE_ENTITY),
        capture_hits: Arc::new(AtomicUsize::new(0)),
        redirect_hits: Arc::new(AtomicUsize::new(0)),
    };
    let (addr, task) = spawn_mock(state).await;

    let error = client_with_token(None)
        .capture_remote(&addr)
        .await
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("remote clipboard is empty or unavailable"));

    task.abort();
}

#[tokio::test]
async fn capture_remote_does_not_follow_redirects() {
    let state = MockState {
        expected_token: None,
        mode: CaptureMode::Redirect,
        capture_hits: Arc::new(AtomicUsize::new(0)),
        redirect_hits: Arc::new(AtomicUsize::new(0)),
    };
    let redirect_hits = state.redirect_hits.clone();
    let (addr, task) = spawn_mock(state).await;

    let error = client_with_token(None)
        .capture_remote(&addr)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("302 Found"));
    assert_eq!(redirect_hits.load(Ordering::SeqCst), 0);

    task.abort();
}
