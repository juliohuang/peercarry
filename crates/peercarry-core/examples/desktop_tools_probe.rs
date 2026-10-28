//! Isolated UI fixture. Never reads the OS clipboard or contacts real peers.
use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use peercarry_core::{
    config::Config,
    engine::Engine,
    model::{Entry, EntryKind, Origin},
    server,
    store::Store,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|s| s == "status") {
        println!(
            "{}",
            serde_json::json!({"Self":{"ID":"probe-self","HostName":"UI fixture","TailscaleIPs":["127.0.0.1"]},"Peer":{"offline":{"ID":"probe-offline","HostName":"Offline test computer","OS":"windows","Online":false,"TailscaleIPs":["100.64.255.254"]}}})
        );
        return Ok(());
    }
    let root =
        PathBuf::from(args.get(1).ok_or_else(|| {
            anyhow::anyhow!("usage: desktop_tools_probe ISOLATED_DATA_DIR [PORT]")
        })?);
    std::fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    let port: u16 = args.get(2).map(String::as_str).unwrap_or("51991").parse()?;
    std::env::set_var("PEERCARRY_DATA_DIR", &root);
    std::env::set_var("PEERCARRY_TAILSCALE", std::env::current_exe()?);
    let mut config = Config::default();
    config.network.port = port;
    config.network.auth_token = Some("isolated-ui-synthetic-secret".into());
    config.ai_token = Some("isolated-ui-ai-token-with-at-least-thirty-two-bytes".into());
    config.storage.data_dir = Some(root.clone());
    config.storage.download_dir = Some(root.join("downloads"));
    let config = Arc::new(config);
    let origin = Origin {
        host: "UI fixture".into(),
        node_id: "probe-self".into(),
        addr: format!("http://127.0.0.1:{port}"),
        os: "test".into(),
    };
    let store = Arc::new(Store::open(&root.join("state.redb"))?);
    store.insert(&Entry {
        id: "probe-text".into(),
        kind: EntryKind::Text,
        origin: origin.clone(),
        created_at: chrono::Utc::now(),
        text: Some("Synthetic multi-computer test".into()),
        blob: None,
        thumb: None,
        files: None,
        preview: "Synthetic multi-computer test".into(),
        pinned: true,
        size: 29,
        holder: None,
    })?;
    let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone())?);
    let app = server::router(server::AppState {
        config: config.clone(),
        store,
        origin,
        engine,
    });
    let monitor = tokio::spawn(peercarry_core::ai_monitor::run(config));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("Isolated UI fixture: http://127.0.0.1:{port}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    monitor.abort();
    Ok(())
}
