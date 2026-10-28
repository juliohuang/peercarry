use std::{net::SocketAddr, sync::Arc};

use axum::{
    body::Body,
    extract::ConnectInfo,
    middleware::{self, Next},
    response::Response,
};
use peercarry_core::{config::Config, engine::Engine, model::Origin, server, store::Store};
use reqwest::{Client, StatusCode};

async fn simulate_peer(mut request: axum::http::Request<Body>, next: Next) -> Response {
    if request.headers().contains_key("x-test-peer") {
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from(([100, 64, 0, 23], 1000))));
    }
    next.run(request).await
}

#[tokio::test]
async fn desktop_ui_works_with_peer_auth_but_rejects_remote_and_cross_origin_control() {
    let root =
        std::env::temp_dir().join(format!("peercarry-desktop-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::env::set_var("PEERCARRY_DATA_DIR", &root);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    let mut config = Config::default();
    config.network.auth_token = Some("synthetic-peer-secret".into());
    config.node.allow_remote_apply = true;
    config.storage.data_dir = Some(root.clone());
    let config = Arc::new(config);
    let store = Arc::new(Store::open(&root.join("state.redb")).unwrap());
    let origin = Origin {
        host: "test".into(),
        node_id: "test-node".into(),
        addr: base.clone(),
        os: "test".into(),
    };
    let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone()).unwrap());
    let app = server::router(server::AppState {
        store,
        config,
        origin,
        engine,
    })
    .layer(middleware::from_fn(simulate_peer));
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let client = Client::builder().no_proxy().build().unwrap();
    for path in [
        "/",
        "/quick",
        "/assets/ui-locale.js",
        "/assets/transfer-center.js",
        "/v1/transfers",
        "/v1/ai/overview",
    ] {
        let response = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "local UI route {path}");
        assert!(!response
            .text()
            .await
            .unwrap()
            .contains("synthetic-peer-secret"));
    }
    for path in [
        "/quick",
        "/assets/ui-locale.js",
        "/assets/transfer-center.js",
        "/v1/transfers",
    ] {
        let response = client
            .get(format!("{base}{path}"))
            .header("x-test-peer", "1")
            .header("x-syncclip-token", "synthetic-peer-secret")
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "remote local-only route {path}"
        );
    }
    let invalid = serde_json::json!({"operation":"send","node_id":"synthetic","expires_in_secs":1});
    let local = client
        .post(format!("{base}/v1/transfers"))
        .header("origin", &base)
        .header("sec-fetch-site", "same-origin")
        .json(&invalid)
        .send()
        .await
        .unwrap();
    assert_eq!(local.status(), StatusCode::BAD_REQUEST); // reached validation; never captures
    let foreign = client
        .post(format!("{base}/v1/transfers"))
        .header("origin", "https://untrusted.example")
        .header("sec-fetch-site", "cross-site")
        .json(&invalid)
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::UNAUTHORIZED);
    let rebound = client
        .get(format!("{base}/v1/transfers"))
        .header("host", "untrusted.example")
        .send()
        .await
        .unwrap();
    assert_eq!(rebound.status(), StatusCode::UNAUTHORIZED);
    let shared = client
        .get(format!("{base}/v1/entries"))
        .header("x-test-peer", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(shared.status(), StatusCode::UNAUTHORIZED);
    let jobs: serde_json::Value = client
        .get(format!("{base}/v1/transfers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(jobs, serde_json::json!([]));
    task.abort();
}
