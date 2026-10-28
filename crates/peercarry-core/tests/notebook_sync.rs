//! Local notebook sync control-plane HTTP tests. All databases live under target/.

use std::sync::{Arc, OnceLock};

use axum::http::StatusCode;
use peercarry_core::{
    config::Config,
    engine::Engine,
    model::Origin,
    protocol,
    server::{self, AppState},
    store::Store,
};
use reqwest::Client;
use serde_json::{json, Value};
use tokio::net::TcpListener;

const TOKEN: &str = "short-sync-test-token";

fn isolated_db_path() -> std::path::PathBuf {
    static ROOT: OnceLock<std::path::PathBuf> = OnceLock::new();
    let root = ROOT
        .get_or_init(|| {
            let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/session-notebook-sync-http");
            let data = path.join("data");
            let temp = path.join("tmp");
            std::fs::create_dir_all(&data).unwrap();
            std::fs::create_dir_all(&temp).unwrap();
            std::env::set_var("PEERCARRY_DATA_DIR", &data);
            std::env::set_var("TEMP", &temp);
            std::env::set_var("TMP", &temp);
            path
        })
        .clone();
    let directory = root.join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&directory).unwrap();
    directory.join("state.redb")
}

struct TestServer {
    base: String,
    client: Client,
    shutdown: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
}

impl TestServer {
    async fn start() -> Self {
        let store = Arc::new(Store::open(&isolated_db_path()).unwrap());
        let mut config = Config::default();
        config.network.auth_token = Some(TOKEN.to_string());
        let config = Arc::new(config);
        let origin = Origin {
            host: "sync-test".to_string(),
            node_id: "local-test-node".to_string(),
            addr: "http://127.0.0.1".to_string(),
            os: "test".to_string(),
        };
        let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone()).unwrap());
        let state = AppState {
            store,
            config,
            origin,
            engine,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = server::router(state);
        let (shutdown, shutdown_signal) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                let _ = shutdown_signal.await;
            })
            .await
            .unwrap();
        });
        let client = Client::builder().no_proxy().build().unwrap();
        Self {
            base: format!("http://{address}"),
            client,
            shutdown,
            task,
        }
    }

    async fn stop(self) {
        let Self {
            client,
            shutdown,
            task,
            ..
        } = self;
        drop(client);
        let _ = shutdown.send(());
        task.await.unwrap();
    }
}

#[tokio::test]
async fn local_settings_run_and_auth_precondition_are_exposed_without_token() {
    let server = TestServer::start().await;
    let settings_url = format!("{}/v1/notebook/sync/settings", server.base);
    let response = server
        .client
        .get(&settings_url)
        .header(protocol::TOKEN_HEADER, TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let settings: Value = response.json().await.unwrap();
    assert_eq!(settings, json!({ "enabled": false, "peer_ids": [] }));
    assert!(!settings.to_string().contains(TOKEN));

    let disable = server
        .client
        .post(&settings_url)
        .header(protocol::TOKEN_HEADER, TOKEN)
        .json(&json!({ "enabled": false, "peer_ids": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(disable.status(), StatusCode::OK);

    let enable = server
        .client
        .post(&settings_url)
        .header(protocol::TOKEN_HEADER, TOKEN)
        .json(&json!({ "enabled": true, "peer_ids": ["synthetic-peer"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(enable.status(), StatusCode::CONFLICT);
    assert_eq!(
        enable.json::<Value>().await.unwrap()["error"],
        "authentication_required"
    );

    let unknown_field = server
        .client
        .post(&settings_url)
        .header(protocol::TOKEN_HEADER, TOKEN)
        .json(&json!({
            "enabled": false,
            "peer_ids": [],
            "token": "must never be accepted"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(unknown_field.status(), StatusCode::BAD_REQUEST);

    let created = server
        .client
        .post(format!("{}/v1/notebook", server.base))
        .header(protocol::TOKEN_HEADER, TOKEN)
        .json(&json!({ "kind": "markdown", "title": "sync test", "markdown": "local only" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = created.json().await.unwrap();
    assert!(created["version_id"].as_str().is_some());
    assert_eq!(created["conflict_count"], 0);
    let note_id = created["id"].as_str().unwrap();
    let conflicts = server
        .client
        .get(format!("{}/v1/notebook/{note_id}/conflicts", server.base))
        .header(protocol::TOKEN_HEADER, TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(conflicts.status(), StatusCode::OK);
    assert_eq!(
        conflicts.json::<Value>().await.unwrap()["versions"],
        json!([])
    );
    let resolve = server
        .client
        .post(format!("{}/v1/notebook/{note_id}/resolve", server.base))
        .header(protocol::TOKEN_HEADER, TOKEN)
        .json(&json!({ "expected_revision": 1, "version_id": created["version_id"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(resolve.status(), StatusCode::CONFLICT);
    assert_eq!(
        resolve.json::<Value>().await.unwrap()["error"],
        "not_conflicted"
    );

    let run = server
        .client
        .post(format!("{}/v1/notebook/sync/run", server.base))
        .header(protocol::TOKEN_HEADER, TOKEN)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(run.status(), StatusCode::ACCEPTED);
    server.stop().await;
}
