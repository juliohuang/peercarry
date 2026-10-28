//! Isolated browser-review fixture: no OS clipboard or real peer access.
use std::{future::IntoFuture, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use peercarry_core::{config::Config, engine::Engine, model::Origin, server, store::Store};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "status") {
        println!(
            "{}",
            serde_json::json!({
                "Self": {"ID":"review-self", "HostName":"Notebook review", "TailscaleIPs":["127.0.0.1"]},
                "Peer": {"offline": {"ID":"review-offline", "HostName":"Offline review computer", "OS":"windows", "Online":false, "TailscaleIPs":["100.64.255.254"]}}
            })
        );
        return Ok(());
    }
    let root = PathBuf::from(
        args.get(1)
            .ok_or_else(|| anyhow::anyhow!("usage: notebook_review_probe ISOLATED_DATA_DIR"))?,
    );
    std::fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    std::env::set_var("PEERCARRY_DATA_DIR", &root);
    std::env::set_var("PEERCARRY_TAILSCALE", std::env::current_exe()?);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let mut config = Config::default();
    config.network.port = address.port();
    config.network.auth_token = Some("notebook-review-synthetic-token-at-least-32".into());
    config.storage.data_dir = Some(root.clone());
    config.storage.download_dir = Some(root.join("downloads"));
    let config = Arc::new(config);
    let origin = Origin {
        host: "Notebook review".into(),
        node_id: "review-self".into(),
        addr: format!("http://{address}"),
        os: "test".into(),
    };
    let store = Arc::new(Store::open(&root.join("state.redb"))?);
    let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone())?);
    let app = server::router(server::AppState {
        config,
        store,
        origin,
        engine,
    });
    println!("http://{address}");
    // A bounded fixture lifetime also prevents abandoned review processes.
    tokio::time::timeout(
        Duration::from_secs(1_800),
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .into_future(),
    )
    .await??;
    Ok(())
}
