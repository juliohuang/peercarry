//! Preview the actual desktop pairing page against an isolated data directory.
//! Block unrelated APIs so this fixture never queries real Tailscale peers,
//! reads the system clipboard, or exposes local actions.
use axum::{
    extract::Request,
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Json,
};
use peercarry_core::{
    config::Config,
    model::Origin,
    server::{self, AppState},
    store::Store,
    Engine,
};
use serde_json::json;
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

async fn isolate(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if path == "/" || path.starts_with("/v1/mobile/") {
        return next.run(req).await;
    }
    if path == "/v1/timeline" || path == "/v1/peers" {
        return Json(json!([])).into_response();
    }
    StatusCode::FORBIDDEN.into_response()
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("isolated output directory required"),
    );
    anyhow::ensure!(!root.exists(), "use a fresh test directory");
    std::fs::create_dir_all(&root)?;
    std::env::set_var("PEERCARRY_DATA_DIR", &root);
    let config = Arc::new(Config::default());
    let store = Arc::new(Store::open(&root.join("state.redb"))?);
    let origin = Origin {
        host: "PeerCarry 演示电脑".into(),
        node_id: "synthetic".into(),
        addr: String::new(),
        os: "test".into(),
    };
    let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone())?);
    let app = server::router(AppState {
        config,
        store,
        origin,
        engine,
    })
    .layer(middleware::from_fn(isolate));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    std::fs::write(root.join("url.txt"), &url)?;
    println!("{url}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        tokio::time::sleep(Duration::from_secs(300)).await;
    })
    .await?;
    Ok(())
}
