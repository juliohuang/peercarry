use peercarry_login_assist::{
    host::{self, Browser, BrowserCall, Service, Snapshot},
    native,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, fs::OpenOptions, net::SocketAddr, path::PathBuf};
use tokio::sync::{mpsc, oneshot};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    allowed_origin: String,
}
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Input {
    Start {
        bind: SocketAddr,
        snapshot: Snapshot,
    },
    Reply {
        reply_id: u64,
        ok: bool,
        data: Option<Value>,
    },
    Invalidate,
    Stop,
}
struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn send(value: Value) -> std::io::Result<()> {
    native::write_message(std::io::stdout().lock(), &value)
}

#[tokio::main]
async fn main() {
    if std::env::args().nth(1).as_deref() == Some("--recover-lock") {
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let path = std::env::current_exe()?.with_extension("lock");
            let file = OpenOptions::new().read(true).write(true).open(&path)?;
            file.try_lock()?;
            std::fs::remove_file(path)?;
            Ok(())
        })();
        if result.is_ok() {
            println!("Stale login-assist lock removed. Start a new pairing explicitly.");
        } else {
            eprintln!("Recovery refused: host is active, no lock exists, or access was denied.");
            std::process::exit(1);
        }
        return;
    }
    // Never print payloads, parse errors, credentials or browser data to stderr.
    if run().await.is_err() {
        let _ = send(json!({"event":"error", "code":"HOST_UNAVAILABLE"}));
    }
}
async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let exe = std::env::current_exe()?;
    let config: Config = serde_json::from_slice(&std::fs::read(exe.with_extension("json"))?)?;
    let origin = std::env::args().nth(1).ok_or("Missing extension origin")?;
    if origin != config.allowed_origin || !origin.starts_with("chrome-extension://") {
        return Err("Extension not allowed".into());
    }
    let lock_path = exe.with_extension("lock");
    let _lock_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)?;
    _lock_file.try_lock()?;
    let _lock = Lock(lock_path);
    let (input_tx, mut input_rx) = mpsc::channel(4);
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        while let Ok(value) = native::read_message(&mut stdin) {
            let Ok(message) = serde_json::from_value::<Input>(value) else {
                break;
            };
            if input_tx.blocking_send(message).is_err() {
                break;
            }
        }
    });
    let (browser_tx, mut browser_rx) = mpsc::channel::<BrowserCall>(1);
    let mut pending: HashMap<u64, oneshot::Sender<Result<Value, ()>>> = HashMap::new();
    let mut next = 0u64;
    let mut service: Option<Service> = None;
    let mut tasks = Vec::new();
    loop {
        tokio::select! {
            message = input_rx.recv() => match message {
                Some(Input::Start { bind, snapshot }) if service.is_none() => {
                    let (listener, certificate, addr) = host::listen(bind).await?;
                    let (host, pair_secret) = Service::new(snapshot, Browser(browser_tx.clone())).map_err(|_| "Invalid target")?;
                    let watchdog = host.clone();
                    tasks.push(tokio::spawn(async move { watchdog.watchdog().await; }));
                    let router = host.router();
                    tasks.push(tokio::spawn(async move { let _ = axum::serve(listener, router).await; }));
                    service = Some(host);
                    send(json!({"event":"started", "ticket":{"version":1, "url":format!("https://{addr}"), "certificate":certificate, "secret":pair_secret}}))?;
                }
                Some(Input::Reply { reply_id, ok, data }) => {
                    if let Some(reply) = pending.remove(&reply_id) {
                        let _ = reply.send(if ok { Ok(data.unwrap_or(Value::Null)) } else { Err(()) });
                    }
                }
                Some(Input::Invalidate) => if let Some(host) = &service { host.invalidate().await; },
                Some(Input::Stop) | None => break,
                _ => { send(json!({"event":"error", "code":"INVALID_STATE"}))?; }
            },
            Some(call) = browser_rx.recv() => {
                pending.retain(|_, tx| !tx.is_closed());
                next = next.checked_add(1).ok_or("Sequence exhausted")?;
                send(json!({"event":"browser", "request_id":next, "payload":call.payload}))?;
                pending.insert(next, call.reply);
            }
        }
    }
    if let Some(host) = service {
        host.revoke().await;
    }
    for task in tasks {
        task.abort();
        let _ = task.await;
    }
    Ok(())
}
