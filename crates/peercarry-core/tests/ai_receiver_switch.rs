use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    body::Body,
    extract::ConnectInfo,
    middleware::{self, Next},
    response::Response,
};
use peercarry_core::{config::Config, engine::Engine, model::Origin, server, store::Store};
use reqwest::{Client, StatusCode};
use uuid::Uuid;

async fn test_remote(mut request: axum::http::Request<Body>, next: Next) -> Response {
    if let Some(value) = request.headers().get("x-test-remote") {
        if let Ok(addr) = value.to_str().unwrap_or_default().parse::<SocketAddr>() {
            request.extensions_mut().insert(ConnectInfo(addr));
        }
    }
    next.run(request).await
}

#[tokio::test]
async fn receiver_declaration_requires_auth_and_is_read_only_peer_visible() {
    let root: PathBuf =
        std::env::temp_dir().join(format!("peercarry-ai-selection-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var("PEERCARRY_DATA_DIR", &root);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_addr = listener.local_addr().unwrap();
    let url = format!("http://{}", local_addr);
    let config = Config {
        ai_token: Some("a-test-ai-token-that-is-at-least-thirty-two-bytes".into()),
        ..Default::default()
    };
    let config = Arc::new(config);
    let store = Arc::new(Store::open(&root.join("state.redb")).unwrap());
    let origin = Origin {
        host: "switch-test".into(),
        node_id: "switch-test".into(),
        addr: url.clone(),
        os: "test".into(),
    };
    let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone()).unwrap());
    let app = server::router(server::AppState {
        config,
        store,
        origin,
        engine,
    })
    .layer(middleware::from_fn(test_remote));
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let client = Client::new();
    let remote = "100.64.0.5:1";
    let url_sel = format!("{url}/v1/ai/receiver-selection");
    let denied = client
        .get(&url_sel)
        .header("x-test-remote", remote)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
    let absent = client
        .get(&url_sel)
        .header("x-test-remote", remote)
        .header(
            "x-peercarry-ai-token",
            "a-test-ai-token-that-is-at-least-thirty-two-bytes",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(absent.status(), StatusCode::OK);
    assert_eq!(
        absent.json::<serde_json::Value>().await.unwrap(),
        serde_json::Value::Null
    );
    let switched = client
        .post(format!("{url}/v1/ai/receiver"))
        .header("host", format!("localhost:{}", local_addr.port()))
        .json(&serde_json::json!({"enabled":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(switched.status(), StatusCode::NO_CONTENT);
    let published = client
        .get(&url_sel)
        .header("x-test-remote", remote)
        .header(
            "x-peercarry-ai-token",
            "a-test-ai-token-that-is-at-least-thirty-two-bytes",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::OK);
    let record: serde_json::Value = published.json().await.unwrap();
    assert!(record["enabled"].as_bool().unwrap());
    assert!(record["device_id"].as_str().is_some());
    assert!(record.get("token").is_none());
    assert!(record.get("url").is_none());
    task.abort();
    let _ = std::fs::remove_dir_all(root);
}
