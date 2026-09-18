//! Local-only mobile integration server with isolated synthetic credentials.
use peercarry_core::{
    config::{Config, MobileDevice},
    engine::Engine,
    model::Origin,
    server::{self, AppState},
    store::Store,
};
use std::{path::PathBuf, sync::Arc};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    anyhow::ensure!(args.len() == 2, "mobile_server_probe NEW_TEST_DIR");
    let root = PathBuf::from(&args[1]);
    std::fs::create_dir(&root)?;
    let root = std::fs::canonicalize(root)?;
    std::env::set_var("PEERCARRY_DATA_DIR", &root);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let mut config = Config::default();
    config.network.port = addr.port();
    config.network.bind = Some(addr.to_string());
    config.mobile.enabled = true;
    let token = uuid::Uuid::new_v4().simple().to_string();
    config.mobile.devices.push(MobileDevice {
        id: "test-phone".into(),
        token: token.clone(),
    });
    config.limits.max_transfer_bytes = 32 * 1024 * 1024;
    config.mobile.max_storage_bytes = 64 * 1024 * 1024;
    let config = Arc::new(config);
    let store = Arc::new(Store::open(&root.join("state.redb"))?);
    let origin = Origin {
        host: "mobile-test-server".into(),
        node_id: "mobile-test-server".into(),
        addr: format!("http://{addr}"),
        os: std::env::consts::OS.into(),
    };
    let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone())?);
    std::fs::write(
        root.join("connection.json"),
        serde_json::to_vec(&serde_json::json!({
            "url": origin.addr, "device_id":"test-phone", "device_token": token,
        }))?,
    )?;
    println!("Ready on {addr}; synthetic connection details saved in test directory");
    axum::serve(
        listener,
        server::router(AppState {
            config,
            store,
            origin,
            engine,
        })
        .into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}
