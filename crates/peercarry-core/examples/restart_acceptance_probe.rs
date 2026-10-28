//! Real-process restart acceptance with an explicit, isolated data directory.
//! Uses the production server, synthetic Tailscale identity, and no clipboard actions.
use std::{path::PathBuf, sync::Arc, time::Duration};

use peercarry_core::{config::Config, paths, server, store::Store};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|argument| argument == "status") {
        println!(
            "{}",
            serde_json::json!({
                "Self": {"ID":"restart-review-self", "HostName":"Restart review", "TailscaleIPs":["127.0.0.1"]},
                "Peer": {}
            })
        );
        return Ok(());
    }
    if args.len() != 3 || args[1] != "serve" {
        anyhow::bail!("usage: restart_acceptance_probe serve ISOLATED_DATA_DIR");
    }
    let root = PathBuf::from(&args[2]).canonicalize()?;
    std::env::set_var("PEERCARRY_DATA_DIR", &root);
    std::env::set_var("PEERCARRY_TAILSCALE", std::env::current_exe()?);
    // The caller must create this synthetic config; never fall back to user defaults.
    anyhow::ensure!(
        root.join("config.toml").is_file(),
        "isolated config required"
    );
    let config = Config::load()?;
    anyhow::ensure!(
        server::bind_addr(&config).await.ip().is_loopback(),
        "loopback config required"
    );
    anyhow::ensure!(
        config.ai_token.is_none() && !config.mobile.enabled,
        "fixture features must remain disabled"
    );
    if std::env::var_os("PEERCARRY_RESTART").is_some() {
        server::wait_for_restart_handoff(config.network.port, Duration::from_secs(10))?;
    }
    // Avoid Store::open_default's platform Downloads bootstrap in this fixture.
    let store = Arc::new(Store::open(&paths::db_path())?);
    println!(
        "restart fixture pid={} port={}",
        std::process::id(),
        config.network.port
    );
    tokio::time::timeout(
        Duration::from_secs(90),
        server::serve(Arc::new(config), store),
    )
    .await??;
    Ok(())
}
