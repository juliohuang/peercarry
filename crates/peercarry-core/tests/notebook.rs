//! Local notebook API tests. Every record and temporary directory is under target/.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use axum::{
    extract::{ConnectInfo, Request},
    http::StatusCode,
    middleware::{self, Next},
    response::Response,
};
use chrono::Utc;
use peercarry_core::{
    config::Config,
    engine::Engine,
    model::{Entry, EntryKind, Origin},
    server::{self, AppState},
    store::Store,
};
use reqwest::Client;
use serde_json::{json, Value};
use tokio::net::TcpListener;
use uuid::Uuid;

const PEER_TOKEN: &str = "notebook-test-peer-token";

fn test_data_root() -> PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/session-notebook");
        let data = root.join("data");
        let temp = root.join("tmp");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&temp).unwrap();
        std::env::set_var("PEERCARRY_DATA_DIR", &data);
        std::env::set_var("TEMP", &temp);
        std::env::set_var("TMP", &temp);
        root
    })
    .clone()
}

fn isolated_db_path(name: &str) -> PathBuf {
    let root = test_data_root().join(name).join(Uuid::new_v4().to_string());
    std::fs::create_dir_all(&root).unwrap();
    root.join("state.redb")
}

fn app_state(store: Arc<Store>) -> AppState {
    let mut config = Config::default();
    config.network.auth_token = Some(PEER_TOKEN.into());
    config.node.allow_remote_apply = true;
    let config = Arc::new(config);
    let origin = Origin {
        host: "notebook-test-computer".into(),
        node_id: "synthetic-node".into(),
        addr: "http://127.0.0.1:5199".into(),
        os: "test".into(),
    };
    let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone()).unwrap());
    AppState {
        store,
        config,
        origin,
        engine,
    }
}

async fn test_connect_info(mut request: Request, next: Next) -> Response {
    if request.headers().contains_key("x-test-remote-peer") {
        request.extensions_mut().insert(ConnectInfo(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 40)),
            48_123,
        )));
    }
    next.run(request).await
}

struct TestServer {
    base: String,
    client: Client,
    task: tokio::task::JoinHandle<()>,
    shutdown: tokio::sync::oneshot::Sender<()>,
}

impl TestServer {
    async fn start(state: AppState) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = server::router(state).layer(middleware::from_fn(test_connect_info));
        let (shutdown, shutdown_signal) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                let _ = shutdown_signal.await;
            })
            .await
            .unwrap();
        });
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONNECTION,
            reqwest::header::HeaderValue::from_static("close"),
        );
        let client = Client::builder()
            .default_headers(headers)
            .no_proxy()
            .build()
            .unwrap();
        Self {
            base: format!("http://{address}"),
            client,
            task,
            shutdown,
        }
    }

    async fn stop(self) {
        let Self {
            client,
            task,
            shutdown,
            ..
        } = self;
        drop(client);
        let _ = shutdown.send(());
        let _ = task.await;
    }

    fn notebook_url(&self) -> String {
        format!("{}/v1/notebook", self.base)
    }
}

async fn create_item(server: &TestServer, body: Value) -> (StatusCode, Value) {
    let response = server
        .client
        .post(server.notebook_url())
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap();
    (status, body)
}

fn todo_task(text: &str, done: bool) -> Value {
    json!({"id": Uuid::new_v4().to_string(), "text": text, "done": done})
}

#[tokio::test]
async fn markdown_is_persisted_and_only_explicit_publish_creates_history() {
    let db_path = isolated_db_path("publish-reopen");
    let store = Arc::new(Store::open(&db_path).unwrap());
    let server = TestServer::start(app_state(store.clone())).await;

    let (status, created) = create_item(
        &server,
        json!({
            "kind": "markdown",
            "title": "会议记录",
            "markdown": "正文内容",
            "pinned": true
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["kind"], "markdown");
    assert_eq!(created["revision"], 1);
    assert!(Uuid::parse_str(created["id"].as_str().unwrap()).is_ok());
    let note_id = created["id"].as_str().unwrap().to_owned();
    assert!(store.list(usize::MAX, None).unwrap().is_empty());

    let update = server
        .client
        .put(format!("{}/v1/notebook/{note_id}", server.base))
        .json(&json!({
            "kind": "markdown",
            "title": "会议记录",
            "markdown": "更新后的正文",
            "tasks": [],
            "pinned": true,
            "expected_revision": 1
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(update.status(), StatusCode::OK);
    assert_eq!(update.json::<Value>().await.unwrap()["revision"], 2);
    assert!(store.list(usize::MAX, None).unwrap().is_empty());

    let published = server
        .client
        .post(format!("{}/v1/notebook/{note_id}/publish", server.base))
        .json(&json!({"expected_revision": 2}))
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::OK);
    let publish_response: Value = published.json().await.unwrap();
    let entry_id = publish_response["entry_id"].as_str().unwrap();
    let entry = store.get(entry_id).unwrap().unwrap();
    assert_eq!(entry.kind, EntryKind::Text);
    assert_eq!(entry.text.as_deref(), Some("# 会议记录\n\n更新后的正文"));
    assert_eq!(entry.origin.host, "notebook-test-computer");
    assert!(!entry.pinned);
    assert_eq!(store.list(usize::MAX, None).unwrap().len(), 1);
    let persisted = server
        .client
        .get(format!("{}/v1/notebook/{note_id}", server.base))
        .send()
        .await
        .unwrap();
    assert_eq!(persisted.status(), StatusCode::OK);
    assert_eq!(persisted.json::<Value>().await.unwrap()["revision"], 2);
    server.stop().await;
    drop(store);
}

#[tokio::test]
async fn concurrent_updates_use_cas_and_trash_restore_versions() {
    let store = Arc::new(Store::open(&isolated_db_path("cas-trash")).unwrap());
    let server = TestServer::start(app_state(store)).await;
    let (status, created) = create_item(
        &server,
        json!({
            "kind": "todo",
            "title": "出门准备",
            "tasks": [todo_task("带钥匙", false)]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["id"].as_str().unwrap();
    let update_url = format!("{}/v1/notebook/{id}", server.base);
    let first = json!({
        "kind": "todo", "title": "出门准备 A", "markdown": "",
        "tasks": [todo_task("带钥匙", true), todo_task("带伞", false)],
        "pinned": false, "expected_revision": 1
    });
    let second = json!({
        "kind": "todo", "title": "出门准备 B", "markdown": "",
        "tasks": [todo_task("带钥匙", false)],
        "pinned": true, "expected_revision": 1
    });
    let (first, second) = tokio::join!(
        server.client.put(&update_url).json(&first).send(),
        server.client.put(&update_url).json(&second).send(),
    );
    let first = first.unwrap();
    let second = second.unwrap();
    let statuses = [first.status(), second.status()];
    assert!(statuses.contains(&StatusCode::OK));
    assert!(statuses.contains(&StatusCode::CONFLICT));
    let conflict = if first.status() == StatusCode::CONFLICT {
        first.json::<Value>().await.unwrap()
    } else {
        second.json::<Value>().await.unwrap()
    };
    assert_eq!(conflict["error"], "revision_conflict");
    assert_eq!(conflict["current_revision"], 2);
    let current: Value = server
        .client
        .get(&update_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let expected_task_count = current["tasks"].as_array().unwrap().len();
    let expected_completed_count = current["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|task| task["done"] == true)
        .count();

    let trash = server
        .client
        .delete(&update_url)
        .json(&json!({"expected_revision": 2}))
        .send()
        .await
        .unwrap();
    assert_eq!(trash.status(), StatusCode::OK);
    assert_eq!(trash.json::<Value>().await.unwrap()["trashed"], true);
    let trash_list: Value = server
        .client
        .get(format!("{}?kind=todo&trash=true", server.notebook_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let summary = &trash_list["items"][0];
    assert_eq!(summary["task_count"], expected_task_count);
    assert_eq!(summary["completed_count"], expected_completed_count);
    assert!(summary.get("tasks").is_none());

    let edit_trashed = server
        .client
        .put(&update_url)
        .json(&json!({
            "kind": "todo", "title": "不可编辑", "markdown": "",
            "tasks": [], "pinned": false, "expected_revision": 3
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(edit_trashed.status(), StatusCode::CONFLICT);
    assert_eq!(
        edit_trashed.json::<Value>().await.unwrap()["error"],
        "item_trashed"
    );

    let restored = server
        .client
        .post(format!("{}/v1/notebook/{id}/restore", server.base))
        .json(&json!({"expected_revision": 3}))
        .send()
        .await
        .unwrap();
    assert_eq!(restored.status(), StatusCode::OK);
    let restored: Value = restored.json().await.unwrap();
    assert_eq!(restored["trashed"], false);
    assert_eq!(restored["revision"], 4);

    let publish = server
        .client
        .post(format!("{}/v1/notebook/{id}/publish", server.base))
        .json(&json!({"expected_revision": 4}))
        .send()
        .await
        .unwrap();
    assert_eq!(publish.status(), StatusCode::OK);
    let entry_id = publish.json::<Value>().await.unwrap()["entry_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let entry = server
        .client
        .get(format!("{}/v1/entries/{entry_id}", server.base))
        .bearer_auth(PEER_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(entry.status(), StatusCode::OK);
    let entry: Value = entry.json().await.unwrap();
    assert!(entry["text"].as_str().unwrap().starts_with("# "));
    assert!(entry["text"].as_str().unwrap().contains("- ["));
    server.stop().await;
}

#[tokio::test]
async fn list_search_and_kind_filter_return_summaries_only() {
    let store = Arc::new(Store::open(&isolated_db_path("list-search")).unwrap());
    let server = TestServer::start(app_state(store)).await;
    let (status, _) = create_item(
        &server,
        json!({
            "kind": "markdown", "title": "设计记录",
            "markdown": "needle in the body", "pinned": false
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, todo) = create_item(
        &server,
        json!({"kind": "todo", "title": "采购", "tasks": [todo_task("needle milk", true)]}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let results: Value = server
        .client
        .get(format!("{}?q=needle", server.notebook_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(results["items"].as_array().unwrap().len(), 2);
    assert!(results["items"][0].get("markdown").is_none());
    assert!(results["items"][0].get("tasks").is_none());
    assert!(results["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["id"] == todo["id"]));

    let todo_results: Value = server
        .client
        .get(format!("{}?kind=todo&q=needle", server.notebook_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(todo_results["items"].as_array().unwrap().len(), 1);
    assert_eq!(todo_results["items"][0]["task_count"], 1);
    assert_eq!(todo_results["items"][0]["completed_count"], 1);
    assert!(todo_results["items"][0]["preview"].as_str().unwrap().len() <= 160);
    server.stop().await;
}

#[tokio::test]
async fn invalid_fields_and_oversized_bodies_are_rejected_without_writes() {
    let store = Arc::new(Store::open(&isolated_db_path("limits")).unwrap());
    let server = TestServer::start(app_state(store.clone())).await;

    for (body, error) in [
        (
            json!({"kind":"markdown", "title":"名".repeat(201)}),
            "title_too_long",
        ),
        (
            json!({"kind":"markdown", "tasks":[todo_task("invalid kind", false)]}),
            "invalid_kind_fields",
        ),
        (
            json!({
                "kind":"todo", "tasks":[
                    {"id":"not-a-uuid", "text":"task", "done":false}
                ]
            }),
            "invalid_task_id",
        ),
        (
            json!({
                "kind":"todo", "tasks":[
                    {"id":"00000000-0000-4000-8000-000000000001", "text":"one", "done":false},
                    {"id":"00000000-0000-4000-8000-000000000001", "text":"two", "done":false}
                ]
            }),
            "duplicate_task_id",
        ),
        (
            json!({
                "kind":"todo", "tasks":[
                    {"id":Uuid::new_v4().to_string(), "text":"x".repeat(4097), "done":false}
                ]
            }),
            "task_text_too_long",
        ),
        (
            json!({"kind":"markdown", "unexpected":"value"}),
            "invalid_request",
        ),
    ] {
        let (status, response) = create_item(&server, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(response["error"], error);
    }

    let too_large_record = json!({
        "kind": "markdown",
        "markdown": "x".repeat(256 * 1024)
    });
    let (status, response) = create_item(&server, too_large_record).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(response["error"], "record_too_large");

    let request_body = format!(
        r#"{{"kind":"markdown","markdown":"{}"}}"#,
        "x".repeat(512 * 1024)
    );
    let response = server
        .client
        .post(server.notebook_url())
        .header("content-type", "application/json")
        .body(request_body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"],
        "request_too_large"
    );

    let invalid_id = server
        .client
        .get(format!("{}/v1/notebook/not-a-uuid", server.base))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid_id.status(), StatusCode::BAD_REQUEST);
    let invalid_kind = server
        .client
        .get(format!("{}?kind=other", server.notebook_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid_kind.status(), StatusCode::BAD_REQUEST);
    assert!(store.list(usize::MAX, None).unwrap().is_empty());
    server.stop().await;
}

#[tokio::test]
async fn clearing_or_pruning_clipboard_history_keeps_notes() {
    let store = Arc::new(Store::open(&isolated_db_path("history-isolation")).unwrap());
    let server = TestServer::start(app_state(store.clone())).await;
    let (status, created) = create_item(
        &server,
        json!({"kind":"markdown", "title":"Keep me", "markdown":"local note"}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let note_id = created["id"].as_str().unwrap();

    store.insert(&history_entry("first history row")).unwrap();
    assert_eq!(store.clear().unwrap(), 1);
    assert_eq!(store.list(usize::MAX, None).unwrap().len(), 0);
    assert_eq!(
        server
            .client
            .get(format!("{}/v1/notebook/{note_id}", server.base))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    store.insert(&history_entry("second history row")).unwrap();
    assert_eq!(store.prune(0).unwrap(), 1);
    assert_eq!(store.list(usize::MAX, None).unwrap().len(), 0);
    let persisted = server
        .client
        .get(format!("{}/v1/notebook/{note_id}", server.base))
        .send()
        .await
        .unwrap();
    assert_eq!(persisted.status(), StatusCode::OK);
    assert_eq!(
        persisted.json::<Value>().await.unwrap()["markdown"],
        "local note"
    );
    server.stop().await;
}

fn history_entry(text: &str) -> Entry {
    Entry {
        id: Uuid::new_v4().to_string(),
        kind: EntryKind::Text,
        origin: Origin {
            host: "synthetic-history-source".into(),
            node_id: "synthetic-history-node".into(),
            addr: "http://127.0.0.1:1".into(),
            os: "test".into(),
        },
        created_at: Utc::now(),
        text: Some(text.into()),
        blob: None,
        thumb: None,
        files: None,
        preview: text.into(),
        pinned: false,
        size: text.len() as u64,
        holder: None,
    }
}

#[tokio::test]
async fn notebook_and_assets_require_local_peer_host_origin_and_fetch_site() {
    let store = Arc::new(Store::open(&isolated_db_path("security-gate")).unwrap());
    let server = TestServer::start(app_state(store)).await;
    let url = server.notebook_url();

    // The configured peer token is not needed for a trusted local browser.
    let local = server.client.get(&url).send().await.unwrap();
    assert_eq!(local.status(), StatusCode::OK);
    let same_origin = server
        .client
        .get(&url)
        .header("origin", &server.base)
        .header("sec-fetch-site", "none")
        .send()
        .await
        .unwrap();
    assert_eq!(same_origin.status(), StatusCode::OK);

    let asset = server
        .client
        .get(format!("{}/assets/notebook.js", server.base))
        .send()
        .await
        .unwrap();
    assert_eq!(asset.status(), StatusCode::OK);
    assert_eq!(asset.headers()["cache-control"], "no-store");

    let forged_host = server
        .client
        .get(&url)
        .header("host", "evil.example")
        .bearer_auth(PEER_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(forged_host.status(), StatusCode::FORBIDDEN);

    let forged_origin = server
        .client
        .get(&url)
        .header("origin", "https://evil.example")
        .bearer_auth(PEER_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(forged_origin.status(), StatusCode::FORBIDDEN);

    let cross_site = server
        .client
        .get(&url)
        .header("sec-fetch-site", "cross-site")
        .bearer_auth(PEER_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(cross_site.status(), StatusCode::FORBIDDEN);

    let remote_peer = server
        .client
        .get(&url)
        .header("x-test-remote-peer", "1")
        .bearer_auth(PEER_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(remote_peer.status(), StatusCode::FORBIDDEN);

    let remote_asset = server
        .client
        .get(format!("{}/assets/notebook.css", server.base))
        .header("x-test-remote-peer", "1")
        .bearer_auth(PEER_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(remote_asset.status(), StatusCode::FORBIDDEN);

    let prefix_neighbor = server
        .client
        .get(format!("{}/v1/notebookish", server.base))
        .bearer_auth(PEER_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(prefix_neighbor.status(), StatusCode::NOT_FOUND);
    server.stop().await;
}
