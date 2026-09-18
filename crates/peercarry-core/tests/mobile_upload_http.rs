//! Exercise the complete router, including the existing global authentication gate.
use peercarry_core::{
    config::{Config, MobileDevice},
    engine::Engine,
    model::Origin,
    server::{self, AppState},
    store::Store,
};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

#[tokio::test]
async fn authenticated_mobile_upload_reaches_existing_download_protocol() {
    let root = std::env::temp_dir().join(format!("peercarry-mobile-http-{}", uuid::Uuid::new_v4()));
    std::env::set_var("PEERCARRY_DATA_DIR", &root);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let mut config = Config::default();
    config.mobile.enabled = true;
    config.network.auth_token = Some("synthetic-global-test-token".into());
    config.mobile.devices = vec![
        MobileDevice {
            id: "phone-a".into(),
            token: "a".repeat(32),
        },
        MobileDevice {
            id: "phone-b".into(),
            token: "b".repeat(32),
        },
    ];
    let config = Arc::new(config);
    let store = Arc::new(Store::open(&root.join("state.redb")).unwrap());
    let origin = Origin {
        host: "test".into(),
        node_id: "test".into(),
        addr: url.clone(),
        os: "test".into(),
    };
    let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone()).unwrap());
    let app = server::router(AppState {
        config,
        store: store.clone(),
        origin,
        engine,
    });
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let client = Client::builder().no_proxy().build().unwrap();
    let headers = |id: &str, token: &str| {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "x-syncclip-token",
            "synthetic-global-test-token".parse().unwrap(),
        );
        headers.insert("x-syncclip-device-id", id.parse().unwrap());
        headers.insert("x-syncclip-device-token", token.parse().unwrap());
        headers
    };
    let owner = headers("phone-a", &"a".repeat(32));
    let other = headers("phone-b", &"b".repeat(32));
    let payload = b"mobile-file-roundtrip-0123456789";
    let metadata = json!({"request_id":uuid::Uuid::new_v4().to_string(),"filename":"phone.txt",
        "size":payload.len(),"sha256":hex::encode(Sha256::digest(payload))});
    let endpoint = format!("{url}/v1/uploads");
    assert_eq!(
        client
            .post(&endpoint)
            .json(&metadata)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let created = client
        .post(&endpoint)
        .headers(owner.clone())
        .json(&metadata)
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = created.json().await.unwrap();
    let id = created["id"].as_str().unwrap();
    let session = format!("{endpoint}/{id}");
    assert_eq!(
        client
            .get(&session)
            .headers(other.clone())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        client
            .delete(&session)
            .headers(other.clone())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        client
            .patch(&session)
            .headers(other)
            .header("upload-offset", "0")
            .body("x")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let first = client
        .patch(&session)
        .headers(owner.clone())
        .header("upload-offset", "0")
        .body(payload[..7].to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    assert!(store.get(id).unwrap().is_none());
    assert_eq!(
        client
            .patch(&session)
            .headers(owner.clone())
            .header("upload-offset", "0")
            .body("stale")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let resumed = client
        .patch(&session)
        .headers(owner.clone())
        .header("upload-offset", "7")
        .body(payload[7..].to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resumed.status(), StatusCode::OK);
    for _ in 0..2 {
        let done = client
            .post(format!("{session}/complete"))
            .headers(owner.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(done.status(), StatusCode::OK);
        let done: Value = done.json().await.unwrap();
        assert_eq!(done["state"], "completed");
        assert_eq!(done["entry_id"], id);
    }
    assert_eq!(store.list(100, None).unwrap().len(), 1);
    let downloaded = client
        .get(format!("{url}/v1/entries/{id}/files/0"))
        .headers(owner.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(downloaded.status(), StatusCode::OK);
    assert_eq!(&downloaded.bytes().await.unwrap()[..], payload);
    assert_eq!(
        client
            .delete(&session)
            .headers(owner)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    task.abort();
    let _ = task.await;
    drop(store);
    std::fs::remove_dir_all(root).unwrap();
}
