//! Isolated Dart/Rust acceptance fixture. Synthetic tickets are written only to
//! the explicitly supplied test directory, never to normal application data.
use peercarry_core::{
    config::Config, mobile_connect::Gateway, model::Origin, server::AppState, store::Store, Engine,
};
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    task::{JoinHandle, JoinSet},
};

fn proxy(listener: TcpListener, target: SocketAddr) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let Ok((mut incoming, _)) = accepted else { break; };
                    connections.spawn(async move {
                        if let Ok(mut outgoing) = TcpStream::connect(target).await {
                            let _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await;
                        }
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    })
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("isolated output directory required"),
    );
    anyhow::ensure!(
        !root.join("ticket.json").exists(),
        "use a fresh test directory"
    );
    std::fs::create_dir_all(&root)?;
    std::env::set_var("PEERCARRY_DATA_DIR", &root);
    let config = Arc::new(Config::default());
    let store = Arc::new(Store::open(&root.join("state.redb"))?);
    let origin = Origin {
        host: "synthetic TLS computer".into(),
        node_id: "probe".into(),
        addr: "http://127.0.0.1:1".into(),
        os: "test".into(),
    };
    let app = AppState {
        engine: Arc::new(Engine::new(config.clone(), store.clone(), origin.clone())?),
        config,
        store,
        origin,
    };
    let lan = TcpListener::bind("127.0.0.1:0").await?;
    let relay = TcpListener::bind("127.0.0.1:0").await?;
    let relay_url = format!("https://{}", relay.local_addr()?);
    let gateway = Gateway::start(
        app,
        "127.0.0.1:0".parse()?,
        vec!["127.0.0.1".parse()?],
        Some(relay_url),
    )
    .await?;
    let mut ticket = gateway.ticket().await;
    ticket.lan = vec![format!("https://{}", lan.local_addr()?)];
    let lan_task = proxy(lan, gateway.address);
    let relay_task = proxy(relay, gateway.address);
    std::fs::write(root.join("ticket.json"), serde_json::to_vec(&ticket)?)?;
    let unrelated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()])?;
    std::fs::write(root.join("unrelated-certificate.der"), unrelated.cert.der())?;
    // Only readiness is printed: never print tokens, certificates or payloads.
    println!("synthetic TLS fixture ready");
    let started = std::time::Instant::now();
    loop {
        if root.join("disable-lan").exists() {
            lan_task.abort();
            std::fs::write(root.join("lan-disabled"), b"ready")?;
        }
        if root.join("stop").exists() || started.elapsed() > Duration::from_secs(180) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    gateway.stop().await;
    lan_task.abort();
    relay_task.abort();
    Ok(())
}
