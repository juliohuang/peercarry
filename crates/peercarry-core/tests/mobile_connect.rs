//! Synthetic HTTPS acceptance; never captures or applies the real clipboard.
use base64::{engine::general_purpose::STANDARD, Engine as _};
use peercarry_core::{
    config::Config,
    mobile_connect::Gateway,
    model::Origin,
    server::{self, AppState},
    store::Store,
    Engine,
};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{net::SocketAddr, sync::Arc};

#[tokio::test]
async fn pinned_pairing_upload_revocation_and_management_isolation() {
    let root = std::env::temp_dir().join(format!("peercarry-pair-test-{}", uuid::Uuid::new_v4()));
    std::env::set_var("PEERCARRY_DATA_DIR", &root);
    let mut config = Config::default();
    config.node.allow_remote_apply = true;
    let config = Arc::new(config);
    let store = Arc::new(Store::open(&root.join("state.redb")).unwrap());
    let origin = Origin {
        host: "synthetic-computer".into(),
        node_id: "test".into(),
        addr: "http://127.0.0.1:1".into(),
        os: "test".into(),
    };
    let app = AppState {
        engine: Arc::new(Engine::new(config.clone(), store.clone(), origin.clone()).unwrap()),
        config,
        store: store.clone(),
        origin,
    };
    let gateway = Gateway::start(
        app.clone(),
        "127.0.0.1:0".parse().unwrap(),
        vec!["127.0.0.1".parse().unwrap()],
        None,
    )
    .await
    .unwrap();
    let ticket = gateway.ticket().await;
    let url = &ticket.lan[0];
    let client = Client::builder()
        .no_proxy()
        .tls_built_in_root_certs(false)
        .add_root_certificate(
            reqwest::Certificate::from_der(&STANDARD.decode(&ticket.certificate).unwrap()).unwrap(),
        )
        .build()
        .unwrap();
    assert!(Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(format!("{url}/v1/hello"))
        .send()
        .await
        .is_err());
    assert_eq!(
        client
            .get(format!("{url}/v1/hello"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let mut request =
        json!({"secret":ticket.secret,"request_id":"a".repeat(64),"name":"synthetic-phone"});
    assert_eq!(
        client
            .post(format!("{url}/v1/pair"))
            .header("origin", "https://evil.example")
            .json(&request)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let paired = client
        .post(format!("{url}/v1/pair"))
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(paired.headers()["cache-control"], "no-store");
    let credentials: Value = paired.json().await.unwrap();
    let token = credentials["token"].as_str().unwrap();
    let repeated: Value = client
        .post(format!("{url}/v1/pair"))
        .json(&request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(credentials, repeated, "lost pairing response is idempotent");
    request["request_id"] = json!("b".repeat(64));
    assert_eq!(
        client
            .post(format!("{url}/v1/pair"))
            .json(&request)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let hello: Value = client
        .get(format!("{url}/v1/hello"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(hello["capabilities"][0], "mobile-upload-v1");
    assert_eq!(
        hello["protocol"],
        peercarry_core::protocol::PROTOCOL_VERSION
    );
    for (method, path) in [
        ("GET", "/v1/config"),
        ("POST", "/v1/actions/apply"),
        ("POST", "/v1/actions/capture"),
        ("POST", "/v1/actions/launch"),
        ("POST", "/v1/mobile/pairing"),
        ("GET", "/v1/ai/overview"),
        ("GET", "/v1/blobs/anything"),
        ("GET", "/v1/entries/anything/files/0"),
    ] {
        assert_eq!(
            client
                .request(method.parse().unwrap(), format!("{url}{path}"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    let entry_id = uuid::Uuid::new_v4().to_string();
    let entry = json!({"id":entry_id,"kind":"text","text":"synthetic encrypted text",
        "created_at":chrono::Utc::now(), "origin":{"host":"spoofed","node_id":"spoofed","addr":"http://evil.example","os":"test"}});
    for _ in 0..2 {
        assert_eq!(
            client
                .post(format!("{url}/v1/entries"))
                .bearer_auth(token)
                .json(&entry)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }
    let saved = store.get(&entry_id).unwrap().unwrap();
    assert_eq!(saved.origin.node_id, credentials["device_id"]);
    assert_eq!(saved.origin.host, "paired-phone");
    assert_eq!(store.list(200, None).unwrap().len(), 1);
    let mut bad = entry.clone();
    bad["files"] = json!([]);
    assert_eq!(
        client
            .post(format!("{url}/v1/entries"))
            .bearer_auth(token)
            .json(&bad)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );

    let bytes = vec![42u8; 2_500_000];
    let metadata = json!({"request_id":uuid::Uuid::new_v4().to_string(), "filename":"synthetic.bin",
        "size":bytes.len(),"sha256":hex::encode(Sha256::digest(&bytes))});
    let upload: Value = client
        .post(format!("{url}/v1/uploads"))
        .bearer_auth(token)
        .json(&metadata)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let upload_url = format!("{url}/v1/uploads/{}", upload["id"].as_str().unwrap());
    for (chunk, body) in bytes.chunks(1024 * 1024).enumerate() {
        let result = client
            .patch(&upload_url)
            .bearer_auth(token)
            .header("upload-offset", chunk * 1024 * 1024)
            .body(body.to_vec())
            .send()
            .await
            .unwrap();
        assert_eq!(result.status(), StatusCode::OK);
    }
    assert_eq!(
        client
            .patch(&upload_url)
            .bearer_auth(token)
            .header("upload-offset", 0)
            .body(vec![42])
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let complete: Value = client
        .post(format!("{upload_url}/complete"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(complete["state"], "completed");
    let file_entry = store
        .get(complete["entry_id"].as_str().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        std::fs::read(&file_entry.files.unwrap()[0].path).unwrap(),
        bytes
    );
    let history: Value = client
        .get(format!("{url}/v1/entries"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let file_history = history
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "files")
        .unwrap();
    assert_eq!(file_history["files"][0]["path"], "");
    assert!(!history.to_string().contains("spoofed"));

    let ticket2 = gateway.ticket().await;
    request["secret"] = json!(ticket2.secret);
    let other: Value = client
        .post(format!("{url}/v1/pair"))
        .json(&request)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        client
            .get(&upload_url)
            .bearer_auth(other["token"].as_str().unwrap())
            .header(
                "x-peercarry-device-id",
                credentials["device_id"].as_str().unwrap()
            )
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    gateway
        .revoke(credentials["device_id"].as_str().unwrap())
        .await;
    assert_eq!(
        client
            .get(format!("{url}/v1/hello"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        client
            .get(format!("{url}/v1/hello"))
            .bearer_auth(other["token"].as_str().unwrap())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    // Local management ignores allow_remote_apply and rejects hostile origins
    // and DNS rebinding hosts even when the socket itself is loopback.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = format!("http://{}", listener.local_addr().unwrap());
    let app = server::router(app);
    let local_task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let raw = Client::builder().no_proxy().build().unwrap();
    assert_eq!(
        raw.get(format!("{local}/v1/mobile/status"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        raw.get(format!("{local}/v1/mobile/status"))
            .header("host", "evil.example")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        raw.post(format!("{local}/v1/mobile/pairing"))
            .header("origin", "https://evil.example")
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    // The old plaintext endpoint stays disabled even after QR authorizes uploads.
    assert_eq!(
        raw.post(format!("{local}/v1/uploads"))
            .json(&metadata)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    gateway.stop().await;
    local_task.abort();
}
