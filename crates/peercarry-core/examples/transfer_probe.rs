//! Isolated, synthetic transfer acceptance tool. Never accesses the clipboard.
use peercarry_core::{
    client::PeerClient,
    config::Config,
    engine::Engine,
    model::Origin,
    server::{self, AppState},
    store::Store,
};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

fn digest(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0; 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("serve") => {
            anyhow::ensure!(args.len() == 5, "serve DATA_DIR BIND_ADDR MIB");
            let root = PathBuf::from(&args[2]);
            std::fs::create_dir_all(&root)?;
            std::env::set_var("PEERCARRY_DATA_DIR", &root);
            let mib: u64 = args[4].parse()?;
            anyhow::ensure!((1..=10240).contains(&mib), "sample must be 1..10240 MiB");
            let source = root.join("sample.bin");
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&source)?;
            let block: Vec<u8> = (0..1024 * 1024)
                .map(|n| ((n * 31 + n / 257) % 251) as u8)
                .collect();
            for _ in 0..mib {
                file.write_all(&block)?;
            }
            file.sync_all()?;
            drop(file);
            let listener = tokio::net::TcpListener::bind(&args[3]).await?;
            let address = listener.local_addr()?;
            let mut config = Config::default();
            config.network.bind = Some(address.to_string());
            config.network.port = address.port();
            config.limits.max_transfer_bytes = (mib + 1) * 1024 * 1024;
            let config = Arc::new(config);
            let store = Arc::new(Store::open(&root.join("test.redb"))?);
            let origin = Origin {
                host: "transfer-test".into(),
                node_id: "transfer-test".into(),
                addr: format!("http://{address}"),
                os: std::env::consts::OS.into(),
            };
            let engine = Arc::new(Engine::new(config.clone(), store.clone(), origin.clone())?);
            let entry = engine.capture_files(std::slice::from_ref(&source))?.entry;
            let manifest = serde_json::json!({"id":entry.id,"url":origin.addr,"bytes":mib*1024*1024,"sha256":digest(&source)?});
            std::fs::write(
                root.join("manifest.json"),
                serde_json::to_vec_pretty(&manifest)?,
            )?;
            println!("{manifest}");
            std::io::stdout().flush()?;
            axum::serve(
                listener,
                server::router(AppState {
                    store,
                    config,
                    origin,
                    engine,
                })
                .layer(axum::middleware::from_fn(
                    |req: axum::extract::Request, next: axum::middleware::Next| async move {
                        let range = req
                            .headers()
                            .get("range")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("none")
                            .to_owned();
                        let response = next.run(req).await;
                        println!(
                            "transfer request range={range} status={}",
                            response.status()
                        );
                        response
                    },
                ))
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await?;
        }
        Some("fetch") | Some("interrupt") => {
            anyhow::ensure!(args.len() >= 6, "fetch URL ID DEST SHA256 [interrupt_ms]");
            let mut config = Config::default();
            config.limits.max_transfer_bytes = 11 * 1024 * 1024 * 1024;
            let client = PeerClient::new(&config)?;
            let dest = Path::new(&args[4]);
            let start = Instant::now();
            let future = client.download_file(&args[2], &args[3], 0, dest);
            if args[1] == "interrupt" {
                let ms: u64 = args
                    .get(6)
                    .ok_or_else(|| anyhow::anyhow!("interrupt_ms required"))?
                    .parse()?;
                match tokio::time::timeout(std::time::Duration::from_millis(ms), future).await {
                    Err(_) => {
                        println!(
                            "INTERRUPTED after {ms} ms; retry with fetch using same destination"
                        );
                        return Ok(());
                    }
                    Ok(result) => {
                        result?;
                        anyhow::bail!("download finished before interrupt; use larger sample or shorter timeout");
                    }
                }
            }
            let path = future.await?;
            let transfer_secs = start.elapsed().as_secs_f64();
            anyhow::ensure!(digest(&path)? == args[5], "SHA256 mismatch");
            let bytes = std::fs::metadata(&path)?.len();
            println!(
                "{}",
                serde_json::json!({"result":"PASS","bytes":bytes,"seconds":transfer_secs,"MiB_per_second":bytes as f64/1048576.0/transfer_secs,"sha256_verified":true,"path":path})
            );
        }
        _ => anyhow::bail!("use serve, fetch, or interrupt"),
    }
    Ok(())
}
