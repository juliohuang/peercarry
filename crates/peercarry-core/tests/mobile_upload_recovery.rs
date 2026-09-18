use peercarry_core::{
    config::{Config, MobileDevice},
    engine::Engine,
    model::{sha256_hex, Origin},
    server::{self, AppState},
    store::Store,
};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use std::sync::Arc;

async fn start(state: AppState) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let router = server::router(state);
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    (url, task)
}

#[tokio::test]
async fn restart_recovers_offset_and_corruption_fails_closed() {
    let root = std::env::temp_dir().join(format!(
        "peercarry-mobile-recovery-{}",
        uuid::Uuid::new_v4()
    ));
    std::env::set_var("PEERCARRY_DATA_DIR", &root);
    let mut cfg = Config::default();
    cfg.mobile.enabled = true;
    cfg.mobile.devices.push(MobileDevice {
        id: "phone".into(),
        token: "r".repeat(32),
    });
    let config = Arc::new(cfg);
    let store = Arc::new(Store::open(&root.join("state.redb")).unwrap());
    let origin = Origin {
        host: "test".into(),
        node_id: "test".into(),
        addr: "http://127.0.0.1".into(),
        os: "test".into(),
    };
    let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone()).unwrap());
    let state = AppState {
        config,
        store: store.clone(),
        origin,
        engine,
    };
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-syncclip-device-id", "phone".parse().unwrap());
    headers.insert("x-syncclip-device-token", "r".repeat(32).parse().unwrap());
    let client = Client::builder()
        .no_proxy()
        .default_headers(headers)
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();
    let (url, task) = start(state.clone()).await;
    let body = json!({"request_id":uuid::Uuid::new_v4().to_string(),"filename":"resume.bin","size":4,"sha256":sha256_hex(b"abcd")});
    let created: Value = client
        .post(format!("{url}/v1/uploads"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(
        client
            .patch(format!("{url}/v1/uploads/{id}"))
            .header("Upload-Offset", "0")
            .body("ab")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    task.abort();
    let _ = task.await;
    let (url, task) = start(state.clone()).await;
    let recovered: Value = client
        .get(format!("{url}/v1/uploads/{id}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(recovered["offset"], 2);
    let duplicate: Value = client
        .post(format!("{url}/v1/uploads"))
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(duplicate["id"], id);
    let session = format!("{url}/v1/uploads/{id}");
    let first = client
        .patch(&session)
        .header("Upload-Offset", "2")
        .body("cd")
        .send();
    let second = client
        .patch(&session)
        .header("Upload-Offset", "2")
        .body("cd")
        .send();
    let (first, second) = tokio::join!(first, second);
    let mut statuses = [
        first.unwrap().status().as_u16(),
        second.unwrap().status().as_u16(),
    ];
    statuses.sort();
    assert_eq!(statuses, [200, 409]);
    assert_eq!(
        client
            .post(format!("{session}/complete"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(store.list(10, None).unwrap().len(), 1);
    task.abort();
    let _ = task.await;
    let (url, task) = start(state.clone()).await;
    let complete: Value = client
        .post(format!("{url}/v1/uploads/{id}/complete"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(complete["state"], "completed");
    assert_eq!(complete["entry_id"], id);
    task.abort();
    let _ = task.await;
    std::fs::write(
        root.join("mobile-uploads").join(&id).join("metadata.json"),
        b"invalid manifest",
    )
    .unwrap();
    let (url, task) = start(state.clone()).await;
    assert_eq!(
        client
            .post(format!("{url}/v1/uploads"))
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    task.abort();
    let _ = task.await;
    drop(client);
    drop(state);
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
