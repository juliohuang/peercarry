//! `peercarry` - command line interface for peercarry.
//!
//! Works two ways:
//!
//! * against a running daemon (`peercarry serve`) - the normal case, and the only way
//!   to use the tray app at the same time, since redb locks the store
//!   exclusively;
//! * directly against the local store when no daemon is running.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
mod ai;
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use peercarry_core::client::PeerClient;
use peercarry_core::config::Config;
use peercarry_core::engine::Engine;
use peercarry_core::model::{Entry, Peer};
use peercarry_core::protocol::{ActionResult, ApplyRequest, PinRequest, SendRequest, SendResult};
use peercarry_core::store::Store;

#[derive(Parser)]
#[command(
    name = "peercarry",
    version,
    about = "Manual, peer-to-peer clipboard sharing across a Tailscale tailnet",
    long_about = "Nothing is synced automatically. Capture with `peercarry push`, then pull with `peercarry pull` on another machine."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Machine readable output.
    #[arg(long, global = true)]
    json: bool,

    /// Repeat for more detail (-v info, -vv debug).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(Subcommand)]
enum Commands {
    /// Discover and bind AI tool notification hooks.
    Ai {
        #[command(subcommand)]
        command: ai::Args,
    },
    /// Run the daemon that serves this machine's clipboard history.
    Serve,

    /// Capture the current system clipboard into the local history.
    #[command(alias = "capture")]
    Push,

    /// List entries from this machine and every online peer.
    List {
        /// Max entries per machine.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Only show this machine's history.
        #[arg(long)]
        local: bool,
        /// Filter by kind: text, image or files.
        #[arg(long)]
        kind: Option<String>,
    },

    /// Search the aggregated timeline across every machine.
    ///
    /// Words are ANDed, matching case-insensitively against previews,
    /// inline text, file names and device names.
    ///
    /// The same view lives in the browser: `http://127.0.0.1:<port>/`.
    Search {
        /// Words to look for.
        #[arg(value_name = "WORD")]
        words: Vec<String>,
        /// Max entries per machine.
        #[arg(long, default_value_t = 200)]
        limit: usize,
    },

    /// Show one entry in full.
    Show { selector: String },

    /// Save an image entry's preview and print where it landed.
    ///
    /// Handy for a quick look: `open "$(peercarry thumb 3)"`.
    Thumb {
        selector: String,
        /// Write here instead of the download directory.
        #[arg(long, short)]
        out: Option<PathBuf>,
    },

    /// Copy an entry onto this machine's clipboard.
    ///
    /// SELECTOR is a list index (1-based), an id prefix, or omitted for the
    /// newest entry in the aggregated list.
    Pull { selector: Option<String> },

    /// Capture (or resend) and push to peers.
    ///
    /// With no HOST the entry goes to every online peer. Peers only record the
    /// entry; they fetch the bytes when someone actually pulls it.
    Send {
        /// Host name or DNS name fragment. Omit to broadcast.
        host: Option<String>,
        /// Resend an existing entry instead of capturing the clipboard.
        #[arg(long)]
        id: Option<String>,
        /// Send these files/directories instead of the clipboard contents.
        /// Repeatable: --file a.pdf --file notes.txt
        #[arg(long = "file", value_name = "PATH")]
        files: Vec<PathBuf>,
    },

    /// List desktop machines in the tailnet and whether they answer.
    Peers,

    /// Show daemon, tailnet and history status.
    Status,

    /// Forget one entry from the local history.
    Delete { selector: String },

    /// Pin an entry so `clear` and history pruning skip it.
    ///
    /// Remote entries are copied into the local history first. Undo with
    /// `peercarry pin --off`.
    Pin {
        selector: String,
        /// Unpin instead of pin.
        #[arg(long)]
        off: bool,
    },

    /// Drop the entire local history (pinned entries survive).
    Clear {
        #[arg(long)]
        yes: bool,
    },

    /// Drop older copies of the same content, keeping the newest of each.
    /// Pinned entries are never removed.
    Dedupe,

    /// Print the active configuration.
    Config {
        /// Write a default config file if none exists.
        #[arg(long)]
        init: bool,
    },

    /// Rename this node as it appears to you and to other machines.
    ///
    /// Defaults to the hostname. Entries already in the history keep the
    /// name they were captured under; they stay recognised as local
    /// through the Tailscale node id.
    Rename {
        /// New name for this node.
        name: Option<String>,
        /// Drop the override and use the hostname again.
        #[arg(long)]
        clear: bool,
    },

    /// Print or change the folder that receives pulled files.
    ///
    /// `peercarry download-dir` prints the current folder;
    /// `peercarry download-dir D:\recv` sets (and creates) it;
    /// `peercarry download-dir --reset` returns to ~/Downloads/peercarry.
    DownloadDir {
        /// New download folder. Omit to print the current one.
        path: Option<PathBuf>,
        /// Back to the platform default (~/Downloads/peercarry).
        #[arg(long)]
        reset: bool,
    },

    /// Launch a registered app.
    ///
    /// `peercarry launch chrome` starts it on this machine;
    /// `peercarry launch chrome --on legion` asks the peer to start its copy.
    /// Peers can only launch apps registered in the target's `[[apps]]`
    /// config, and only when that machine enables allow_remote_launch.
    Launch {
        /// App name as registered on the target machine.
        name: String,
        /// Launch on this peer (host name fragment) instead of locally.
        #[arg(long)]
        on: Option<String>,
    },

    /// Show the apps this machine offers and what online peers offer.
    ///
    /// Manage the local list with `peercarry apps add` / `peercarry apps remove`.
    Apps {
        #[command(subcommand)]
        command: Option<AppsCommands>,
    },

    /// Install (or with --remove, uninstall) the auto-start service.
    ///
    /// macOS: a LaunchAgent for the tray app (falls back to `peercarry serve`).
    /// Linux: a systemd user unit running `peercarry serve`.
    /// Windows: an HKCU Run entry for the tray app.
    InstallService {
        /// Remove the service instead of installing it.
        #[arg(long)]
        remove: bool,
    },
}

#[derive(Subcommand)]
enum AppsCommands {
    /// Register an app this machine offers for launching.
    ///
    /// Example: `peercarry apps add chrome "C:\\Program Files\\...\\chrome.exe"`
    /// The path is stored as given; peers launch it by name only.
    Add {
        /// Short name peers will refer to it by (no colons/whitespace).
        name: String,
        /// Executable path on this machine.
        path: PathBuf,
        /// Fixed arguments, stored with the registration.
        args: Vec<String>,
    },

    /// Remove a registered app.
    Remove { name: String },
}

// ---------------------------------------------------------------- plumbing

/// A daemon reachable on loopback.
struct LocalDaemon {
    base: String,
    http: reqwest::Client,
    token: Option<String>,
}

impl LocalDaemon {
    async fn discover(port: u16, token: Option<String>) -> Option<Self> {
        let base = format!("http://127.0.0.1:{port}");

        // Discovery has to be quick: when no daemon is running we fall back to
        // opening the store directly, and a slow probe would delay every
        // command. Loopback: never route through a system proxy.
        let probe = reqwest::Client::builder()
            .timeout(Duration::from_millis(700))
            .no_proxy()
            .build()
            .ok()?;
        let resp = probe.get(format!("{base}/v1/hello")).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }

        // Real requests get a generous timeout: actions such as `pin` or
        // `apply` may have to fan out to peers across the tailnet first.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .no_proxy()
            .build()
            .ok()?;
        Some(Self { base, http, token })
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut req = self.http.request(method, format!("{}{}", self.base, path));
        if let Some(token) = &self.token {
            if !token.is_empty() {
                req = req.header(peercarry_core::protocol::TOKEN_HEADER, token);
            }
        }
        req
    }

    async fn capture(&self) -> Result<CaptureOutcome> {
        let resp = self
            .request(reqwest::Method::POST, "/v1/actions/capture")
            .send()
            .await
            .context("cannot reach the local daemon")?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("capture failed: {body}"));
        }
        // The dedup flag is in a header so the body shape stays the same for
        // older clients that do not know about it.
        let deduped = resp
            .headers()
            .get(peercarry_core::protocol::DEDUPED_HEADER)
            .is_some();
        let entry: Entry = resp.json().await?;
        Ok(CaptureOutcome { entry, deduped })
    }

    /// Capture explicit files (not the clipboard) into the daemon's history.
    async fn capture_files(&self, paths: &[PathBuf]) -> Result<CaptureOutcome> {
        let resp = self
            .request(reqwest::Method::POST, "/v1/actions/capture-files")
            .json(&peercarry_core::protocol::CaptureFilesRequest {
                paths: paths
                    .iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect(),
            })
            .send()
            .await
            .context("cannot reach the local daemon")?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("capture-files failed: {body}"));
        }
        let deduped = resp
            .headers()
            .get(peercarry_core::protocol::DEDUPED_HEADER)
            .is_some();
        let entry: Entry = resp.json().await?;
        Ok(CaptureOutcome { entry, deduped })
    }

    async fn apply(&self, request: &ApplyRequest) -> Result<ActionResult> {
        let resp = self
            .request(reqwest::Method::POST, "/v1/actions/apply")
            .json(request)
            .send()
            .await
            .context("cannot reach the local daemon")?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("apply failed: {body}"));
        }
        Ok(resp.json().await?)
    }

    async fn send(&self, request: &SendRequest) -> Result<SendResult> {
        let resp = self
            .request(reqwest::Method::POST, "/v1/actions/send")
            .json(request)
            .send()
            .await
            .context("cannot reach the local daemon")?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("send failed: {body}"));
        }
        Ok(resp.json().await?)
    }

    async fn list(&self, limit: usize) -> Result<Vec<Entry>> {
        let resp = self
            .request(reqwest::Method::GET, &format!("/v1/entries?limit={limit}"))
            .send()
            .await
            .context("cannot reach the local daemon")?;
        if !resp.status().is_success() {
            return Err(anyhow!("list failed: {}", resp.status()));
        }
        Ok(resp.json().await?)
    }

    async fn delete(&self, id: &str) -> Result<bool> {
        let resp = self
            .request(reqwest::Method::DELETE, &format!("/v1/entries/{id}"))
            .send()
            .await
            .context("cannot reach the local daemon")?;
        Ok(resp.status().is_success())
    }

    /// Image preview bytes, or an error when the entry has none.
    async fn thumb(&self, id: &str) -> Result<Vec<u8>> {
        let resp = self
            .request(reqwest::Method::GET, &format!("/v1/entries/{id}/thumb"))
            .send()
            .await
            .context("cannot reach the local daemon")?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "no preview for {}: {}",
                &id[..8.min(id.len())],
                resp.status()
            ));
        }
        Ok(resp.bytes().await?.to_vec())
    }

    async fn clear(&self) -> Result<usize> {
        let entries = self.list(usize::MAX).await?;
        let mut removed = 0;
        for entry in entries.into_iter().filter(|e| !e.pinned) {
            if self.delete(&entry.id).await.unwrap_or(false) {
                removed += 1;
            }
        }
        Ok(removed)
    }

    async fn dedupe(&self) -> Result<usize> {
        let resp = self
            .request(reqwest::Method::POST, "/v1/actions/dedupe")
            .send()
            .await
            .context("cannot reach the local daemon")?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("dedupe failed: {body}"));
        }
        let value: serde_json::Value = resp.json().await?;
        Ok(value.get("removed").and_then(|v| v.as_u64()).unwrap_or(0) as usize)
    }

    /// Launch a locally registered app through the daemon, so the process
    /// lands in the daemon's desktop session rather than this terminal's.
    async fn launch(&self, name: &str) -> Result<String> {
        let resp = self
            .request(reqwest::Method::POST, "/v1/actions/launch")
            .json(&peercarry_core::protocol::LaunchRequest {
                name: name.to_string(),
            })
            .send()
            .await
            .context("cannot reach the local daemon")?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("launch failed: {body}"));
        }
        Ok(resp.json::<ActionResult>().await?.detail)
    }

    async fn pin(&self, id: &str, pinned: bool) -> Result<Entry> {
        let resp = self
            .request(reqwest::Method::POST, "/v1/actions/pin")
            .json(&PinRequest {
                id: id.to_string(),
                pinned,
            })
            .send()
            .await
            .context("cannot reach the local daemon")?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("pin failed: {body}"));
        }
        Ok(resp.json().await?)
    }
}

enum Backend {
    Daemon(LocalDaemon),
    Direct(Arc<Engine>),
}

use peercarry_core::engine::CaptureOutcome;

impl Backend {
    async fn connect(config: &Config) -> Result<Self> {
        if let Some(daemon) =
            LocalDaemon::discover(config.network.port, config.network.auth_token.clone()).await
        {
            return Ok(Backend::Daemon(daemon));
        }

        // No daemon: take the store ourselves. This fails while a daemon holds
        // the lock, which is exactly the case handled above.
        match Store::open_default() {
            Ok(store) => {
                let origin = Engine::local_origin(config).await;
                let engine = Engine::new(Arc::new(config.clone()), Arc::new(store), origin)?;
                Ok(Backend::Direct(Arc::new(engine)))
            }
            Err(e) => Err(anyhow!(
                "no local daemon on port {} and the store could not be opened ({e}). \
                 Start `peercarry serve` in another terminal.",
                config.network.port
            )),
        }
    }

    /// Local history merged with every online peer's history.
    async fn list(&self, config: &Config, limit: usize) -> Result<Vec<Entry>> {
        let local = match self {
            Backend::Daemon(daemon) => daemon.list(limit).await?,
            Backend::Direct(engine) => engine.store.list(limit, None)?,
        };
        let client = PeerClient::new(config)?;
        Ok(client.aggregate(local, limit).await?)
    }

    async fn dedupe(&self) -> Result<usize> {
        match self {
            Backend::Daemon(daemon) => daemon.dedupe().await,
            Backend::Direct(engine) => engine.dedupe().map_err(Into::into),
        }
    }
}

// ------------------------------------------------------------- formatting

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    if bytes < 1024 {
        return format!("{bytes}B");
    }
    let mut size = bytes as f64;
    let mut unit = 0usize;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{:.1}{}", size, UNITS[unit])
}

fn human_age(created: DateTime<Utc>) -> String {
    let secs = (Utc::now() - created).num_seconds().max(0);
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

fn truncate(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!(
            "{}…",
            flat.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

/// Render the entry table.
///
/// `local_ids` is supplied for the aggregate view: an entry missing from it
/// lives on another machine only, so commands that touch local history
/// (`delete`, `clear`) cannot act on it. Such rows are marked to keep the
/// numbering from looking like something you can address.
fn print_table(entries: &[Entry], local_ids: Option<&HashSet<String>>) {
    println!(
        "{:<4} {:<8} {:>8}  {:<7} {:<16} PREVIEW",
        "#", "KIND", "SIZE", "AGE", "SOURCE"
    );
    for (index, entry) in entries.iter().enumerate() {
        let on_peer_only = local_ids.is_some_and(|ids| !ids.contains(&entry.id));
        // Two independent flags, so a pinned entry that lives on another
        // machine still shows both facts.
        let pinned = if entry.pinned { "*" } else { " " };
        let at = if on_peer_only { "^" } else { " " };
        println!(
            "{:<4} {}{}{:<6} {:>8}  {:<7} {:<16} {}",
            index + 1,
            pinned,
            at,
            entry.kind.as_str(),
            human_size(entry.size),
            human_age(entry.created_at),
            truncate(&entry.origin.host, 16),
            truncate(&entry.preview, 58)
        );
    }
    if entries.is_empty() {
        println!("(empty - try `peercarry push` on this or another machine)");
    }
    if local_ids.is_some() {
        println!("(* pinned  ^ only on a peer - `delete`/`clear` act on this machine only)");
    }
}

/// Resolve `1`, an id prefix, or nothing (newest).
fn select<'a>(entries: &'a [Entry], selector: Option<&str>) -> Result<&'a Entry> {
    let Some(selector) = selector else {
        return entries.first().ok_or_else(|| anyhow!("nothing to pull"));
    };
    if let Ok(index) = selector.parse::<usize>() {
        return entries
            .get(index.saturating_sub(1))
            .ok_or_else(|| anyhow!("no entry #{index}"));
    }
    entries
        .iter()
        .find(|e| e.id.starts_with(selector))
        .ok_or_else(|| anyhow!("no entry matching {selector:?}"))
}

// ----------------------------------------------------------------- commands

async fn cmd_serve(config: Config) -> Result<()> {
    // Self-restart handoff: wait for the old process to release the port.
    if std::env::var_os("PEERCARRY_RESTART").is_some() {
        peercarry_core::server::wait_for_local_port(
            config.network.port,
            std::time::Duration::from_secs(10),
        );
    }
    let bind = peercarry_core::server::bind_addr(&config).await;
    println!(
        "peercarry {} starting on {bind} (node {:?})",
        env!("CARGO_PKG_VERSION"),
        config.display_name()
    );

    let store = Arc::new(Store::open_default()?);
    peercarry_core::server::serve(Arc::new(config), store).await?;
    Ok(())
}

async fn cmd_push(backend: &Backend, json: bool) -> Result<()> {
    let outcome = match backend {
        Backend::Daemon(daemon) => daemon.capture().await?,
        Backend::Direct(engine) => engine.capture().await?,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&outcome.entry)?);
    } else if outcome.deduped {
        println!(
            "already in history: {}  [{}]",
            &outcome.entry.id[..8.min(outcome.entry.id.len())],
            outcome.entry.preview
        );
    } else {
        println!(
            "captured {} entry {}  {}  [{}]",
            outcome.entry.kind.as_str(),
            &outcome.entry.id[..8.min(outcome.entry.id.len())],
            human_size(outcome.entry.size),
            outcome.entry.preview
        );
    }
    Ok(())
}

/// Drop older copies of the same content, keeping the newest of each.
async fn cmd_dedupe(backend: &Backend) -> Result<()> {
    let removed = backend.dedupe().await?;
    if removed == 0 {
        println!("no duplicates");
    } else {
        println!("removed {removed} duplicate entries (pinned entries are kept)");
    }
    Ok(())
}

async fn cmd_list(
    backend: &Backend,
    config: &Config,
    limit: usize,
    local_only: bool,
    kind: Option<&str>,
    json: bool,
) -> Result<()> {
    let (mut entries, local_ids) = if local_only {
        let entries = match backend {
            Backend::Daemon(daemon) => daemon.list(limit).await?,
            Backend::Direct(engine) => engine.store.list(limit, None)?,
        };
        (entries, None)
    } else {
        // The aggregate view merges peer histories, so keep the local ids
        // around to tell the two apart when printing.
        let local = match backend {
            Backend::Daemon(daemon) => daemon.list(limit).await?,
            Backend::Direct(engine) => engine.store.list(limit, None)?,
        };
        let ids: HashSet<String> = local.iter().map(|e| e.id.clone()).collect();
        (backend.list(config, limit).await?, Some(ids))
    };

    entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    if let Some(kind) = kind {
        let wanted = kind.to_lowercase();
        entries.retain(|e| e.kind.as_str() == wanted);
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        print_table(&entries, local_ids.as_ref());
    }
    Ok(())
}

async fn cmd_search(
    backend: &Backend,
    config: &Config,
    words: &[String],
    limit: usize,
    json: bool,
) -> Result<()> {
    let mut entries = backend.list(config, limit).await?;
    entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    // Every word must match somewhere; the words themselves can land in
    // different fields.
    entries.retain(|e| words.iter().all(|w| e.matches(w)));

    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else if entries.is_empty() {
        println!("(no entry matches {})", words.join(" "));
    } else {
        println!("{} match(es) for '{}':", entries.len(), words.join(" "));
        print_table(&entries, None);
    }
    Ok(())
}

async fn cmd_pull(backend: &Backend, config: &Config, selector: Option<String>) -> Result<()> {
    let entries = backend.list(config, 50).await?;
    let entry = select(&entries, selector.as_deref())?.clone();

    let result = match backend {
        Backend::Daemon(daemon) => {
            daemon
                .apply(&ApplyRequest {
                    id: Some(entry.id.clone()),
                    entry: Some(entry.clone()),
                })
                .await?
        }
        Backend::Direct(engine) => {
            let applied = engine.apply(&entry).await?;
            ActionResult {
                ok: true,
                kind: applied.kind.as_str().to_string(),
                detail: applied.detail,
            }
        }
    };

    println!(
        "pulled {} from {}: {}",
        result.kind, entry.origin.host, result.detail
    );
    Ok(())
}

async fn cmd_send(
    backend: &Backend,
    config: &Config,
    host: Option<String>,
    id: Option<String>,
    files: &[PathBuf],
    json: bool,
) -> Result<()> {
    if !files.is_empty() && id.is_some() {
        return Err(anyhow!("--id and --file are mutually exclusive"));
    }
    let mut request = SendRequest { id, host };

    let result = match backend {
        Backend::Daemon(daemon) => {
            // The entry has to live in the daemon's store: it is the one
            // serving the file bytes to peers later.
            if !files.is_empty() {
                let outcome = daemon.capture_files(files).await?;
                request.id = Some(outcome.entry.id);
            }
            daemon.send(&request).await?
        }
        Backend::Direct(engine) => {
            let entry = if !files.is_empty() {
                engine.capture_files(files)?.entry
            } else {
                match request.id.as_deref() {
                    Some(id) => engine
                        .find(id)
                        .await?
                        .ok_or_else(|| anyhow!("no entry {id}"))?,
                    None => engine.capture().await?.entry,
                }
            };

            let outcomes = match request.host.as_deref() {
                Some(host) => {
                    let needle = host.to_lowercase();
                    let peers = engine.peers().await;
                    let peer = peers
                        .into_iter()
                        .find(|p| {
                            p.host.to_lowercase().contains(&needle)
                                || p.dns_name.to_lowercase().contains(&needle)
                        })
                        .ok_or_else(|| anyhow!("no peer matching {host}"))?;
                    let outcome = engine.send_to(&peer, &entry).await;
                    vec![(peer.host.clone(), outcome)]
                }
                None => engine.broadcast(&entry).await,
            };

            SendResult {
                id: entry.id.clone(),
                targets: outcomes
                    .into_iter()
                    .map(|(host, outcome)| {
                        if let Err(e) = &outcome {
                            tracing::warn!("send to {host} failed: {e}");
                        }
                        (host, outcome.is_ok())
                    })
                    .collect(),
            }
        }
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        let ok = result.targets.iter().filter(|(_, ok)| *ok).count();
        println!(
            "pushed {} to {}/{} peers",
            &result.id[..8.min(result.id.len())],
            ok,
            result.targets.len()
        );
        for (host, accepted) in &result.targets {
            println!("  {} {}", if *accepted { "ok  " } else { "FAIL" }, host);
        }
    }
    let _ = config;
    Ok(())
}

async fn cmd_peers(config: &Config, json: bool) -> Result<()> {
    let peers: Vec<Peer> = peercarry_core::tailscale::peers(config.network.port).await?;
    let client = PeerClient::new(config)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&peers)?);
        return Ok(());
    }

    println!("{:<24} {:<8} {:<10} {}", "HOST", "OS", "STATE", "ADDRESS");
    for peer in &peers {
        let addr = peer.addr_with_port(config.network.port).unwrap_or_default();
        let state = if !peer.online {
            "offline".to_string()
        } else if client.is_alive(&addr).await {
            "online".to_string()
        } else {
            "no daemon".to_string()
        };
        println!(
            "{:<24} {:<8} {:<10} {}",
            truncate(&peer.host, 24),
            peer.os,
            state,
            addr
        );
    }
    if peers.is_empty() {
        println!("(no desktop peers - is Tailscale running?)");
    }
    Ok(())
}

async fn cmd_status(config: &Config, backend: &Backend) -> Result<()> {
    let self_ip = peercarry_core::tailscale::self_ipv4().await.ok().flatten();
    let daemon = LocalDaemon::discover(config.network.port, config.network.auth_token.clone())
        .await
        .is_some();

    let local_count = match backend {
        Backend::Daemon(d) => d.list(usize::MAX).await.map(|e| e.len()).unwrap_or(0),
        Backend::Direct(e) => e.store.list(usize::MAX, None).map(|e| e.len()).unwrap_or(0),
    };

    let peers = peercarry_core::tailscale::peers(config.network.port)
        .await
        .unwrap_or_default();

    println!("node          {}", config.display_name());
    println!("tailscale ip  {}", self_ip.unwrap_or_else(|| "-".into()));
    println!("port          {}", config.network.port);
    println!(
        "daemon        {}",
        if daemon { "running" } else { "stopped" }
    );
    println!("entries       {local_count}");
    println!(
        "peers         {} ({} online)",
        peers.len(),
        peers.iter().filter(|p| p.online).count()
    );
    println!(
        "data dir      {}",
        peercarry_core::paths::data_dir().display()
    );
    println!("downloads     {}", config.download_dir().display());
    Ok(())
}

async fn cmd_delete(backend: &Backend, config: &Config, selector: &str) -> Result<()> {
    let entries = match backend {
        Backend::Daemon(d) => d.list(usize::MAX).await?,
        Backend::Direct(e) => e.store.list(usize::MAX, None)?,
    };
    let entry = match select(&entries, Some(selector)) {
        Ok(entry) => entry.clone(),
        Err(_) => {
            // The default list is the merged tailnet view, so a number can
            // point at a peer's copy. Say so instead of a bare "no entry".
            let merged = backend.list(config, 200).await.unwrap_or_default();
            if let Ok(remote) = select(&merged, Some(selector)) {
                let holder = remote
                    .holder
                    .clone()
                    .unwrap_or_else(|| "another machine".to_string());
                return Err(anyhow!(
                    "entry #{selector} is stored on {holder}, not here - delete affects this \
                     machine's history; use `peercarry pin {selector}` to keep a local copy, or \
                     `peercarry list --local` to see only local entries"
                ));
            }
            return Err(anyhow!("no entry #{selector} in the local history"));
        }
    };

    let removed = match backend {
        Backend::Daemon(d) => d.delete(&entry.id).await?,
        Backend::Direct(e) => e.store.delete(&entry.id)?,
    };
    if removed {
        println!("deleted {}", entry.id);
    } else {
        println!("entry {} was not in the local history", entry.id);
    }
    Ok(())
}

async fn cmd_thumb(
    backend: &Backend,
    config: &Config,
    selector: &str,
    out: Option<&Path>,
) -> Result<()> {
    let entries = backend.list(config, 200).await?;
    let entry = select(&entries, Some(selector))?.clone();
    if entry.kind != peercarry_core::model::EntryKind::Image {
        return Err(anyhow!(
            "entry {} is {}, not an image - only images have a preview",
            &entry.id[..8.min(entry.id.len())],
            entry.kind.as_str()
        ));
    }

    let bytes = match backend {
        Backend::Daemon(d) => d.thumb(&entry.id).await?,
        Backend::Direct(e) => e.thumbnail_for(&entry.id).await?,
    };

    let path = match out {
        Some(path) => path.to_path_buf(),
        None => config
            .download_dir()
            .join("thumbs")
            .join(format!("{}.png", entry.id)),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, &bytes)?;
    println!("{}", path.display());
    Ok(())
}

/// Set (or clear) this node's display name.
async fn cmd_rename(config: &Config, name: Option<&str>, clear: bool) -> Result<()> {
    let before = config.display_name();

    let chosen = match (name, clear) {
        (Some(_), true) => return Err(anyhow!("pass a name or --clear, not both")),
        (Some(raw), false) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Err(anyhow!("name cannot be empty"));
            }
            Some(trimmed.to_string())
        }
        (None, true) => None,
        (None, false) => return Err(anyhow!("give a name, or --clear to use the hostname again")),
    };

    let mut updated = config.clone();
    updated.node.name = chosen;
    updated.save()?;

    println!("{}  ->  {}", before, updated.display_name());
    println!(
        "written to {}",
        peercarry_core::paths::config_path().display()
    );
    // The name is part of the origin advertised at startup, so a running
    // daemon keeps the old one until it is restarted.
    println!("restart the service to apply it:");
    if cfg!(target_os = "macos") {
        println!("  launchctl kickstart gui/$(id -u)/cn.peercarry");
    } else if cfg!(target_os = "linux") {
        println!("  systemctl --user restart peercarry");
    } else {
        println!("  quit and reopen peercarry-tray");
    }
    Ok(())
}

/// Print or change the folder that receives pulled files.
async fn cmd_download_dir(config: &Config, path: Option<&Path>, reset: bool) -> Result<()> {
    if path.is_none() && !reset {
        println!("{}", config.download_dir().display());
        return Ok(());
    }
    if path.is_some() && reset {
        return Err(anyhow!("pass a path or --reset, not both"));
    }

    let before = config.download_dir();
    let mut updated = config.clone();
    updated.storage.download_dir = match path {
        Some(raw) => {
            // Store an absolute path so a daemon started from another
            // directory resolves the same folder.
            let absolute = if raw.is_absolute() {
                raw.to_path_buf()
            } else {
                std::env::current_dir()?.join(raw)
            };
            std::fs::create_dir_all(&absolute)
                .with_context(|| format!("cannot create {}", absolute.display()))?;
            Some(absolute)
        }
        None => None,
    };
    updated.save()?;

    println!(
        "{}  ->  {}",
        before.display(),
        updated.download_dir().display()
    );
    println!(
        "written to {}",
        peercarry_core::paths::config_path().display()
    );
    // A running daemon keeps its copy of the config in memory.
    println!("restart the service to apply it to tray/daemon pulls:");
    if cfg!(target_os = "macos") {
        println!("  launchctl kickstart gui/$(id -u)/cn.peercarry");
    } else if cfg!(target_os = "linux") {
        println!("  systemctl --user restart peercarry");
    } else {
        println!("  quit and reopen peercarry-tray");
    }
    Ok(())
}

/// Launch a registered app here, or ask a peer to launch its copy.
async fn cmd_launch(
    backend: &Backend,
    config: &Config,
    name: &str,
    on: Option<&str>,
) -> Result<()> {
    let detail = match on {
        // Straight to the peer: the local daemon has no role in a remote
        // launch, and this works even while no daemon runs here.
        Some(host) => {
            let client = PeerClient::new(config)?;
            client.launch_on(host, name).await?
        }
        None => match backend {
            Backend::Daemon(daemon) => daemon.launch(name).await?,
            Backend::Direct(engine) => engine.launch_app(name)?,
        },
    };
    println!("{detail}");
    Ok(())
}

/// Print the local app registry and what online peers advertise.
async fn cmd_apps(config: &Config, command: Option<AppsCommands>) -> Result<()> {
    match command {
        Some(AppsCommands::Add { name, path, args }) => {
            return cmd_apps_add(config, name, path, args).await;
        }
        Some(AppsCommands::Remove { name }) => {
            return cmd_apps_remove(config, &name).await;
        }
        None => {}
    }

    println!("local:");
    if config.apps.is_empty() {
        println!("  (none - `peercarry apps add <name> <path>` registers one)");
    }
    for app in &config.apps {
        let args = app.args.join(" ");
        println!(
            "  {} -> {}{}",
            app.name,
            app.path.display(),
            if args.is_empty() {
                String::new()
            } else {
                format!(" {args}")
            }
        );
    }

    let client = PeerClient::new(config)?;
    let remote = client.remote_apps().await;
    if !remote.is_empty() {
        println!("peers (launch with `peercarry launch <name> --on <host>`):");
        for (host, apps) in remote {
            println!("  {}: {}", host, apps.join(", "));
        }
    } else {
        println!("peers: none advertise launchable apps");
    }
    Ok(())
}

/// Validate and store one `[[apps]]` entry.
async fn cmd_apps_add(
    config: &Config,
    name: String,
    path: PathBuf,
    args: Vec<String>,
) -> Result<()> {
    if name.is_empty() || name.contains(':') || name.split_whitespace().count() != 1 {
        return Err(anyhow!(
            "app name must be non-empty and free of colons and whitespace"
        ));
    }
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    if !absolute.exists() {
        return Err(anyhow!("{} does not exist", absolute.display()));
    }

    let mut updated = config.clone();
    let entry = peercarry_core::config::AppEntry {
        name: name.clone(),
        path: absolute,
        args,
    };
    // Re-adding a name replaces the old registration.
    updated.apps.retain(|a| !a.name.eq_ignore_ascii_case(&name));
    updated.apps.push(entry);
    updated.save()?;

    println!("registered {name} (peers can launch it once allow_remote_launch is on)");
    Ok(())
}

async fn cmd_apps_remove(config: &Config, name: &str) -> Result<()> {
    let mut updated = config.clone();
    let before = updated.apps.len();
    updated.apps.retain(|a| !a.name.eq_ignore_ascii_case(name));
    if updated.apps.len() == before {
        return Err(anyhow!("no app named {name:?} is registered here"));
    }
    updated.save()?;
    println!("removed {name}");
    Ok(())
}

async fn cmd_clear(backend: &Backend, yes: bool) -> Result<()> {
    if !yes {
        return Err(anyhow!("this drops the local history; re-run with --yes"));
    }
    let removed = match backend {
        Backend::Daemon(d) => d.clear().await?,
        Backend::Direct(e) => e.store.clear()?,
    };
    let kept = match backend {
        Backend::Daemon(d) => d.list(usize::MAX).await.map(|e| e.len()).unwrap_or(0),
        Backend::Direct(e) => e.store.list(usize::MAX, None).map(|e| e.len()).unwrap_or(0),
    };
    if kept > 0 {
        println!("removed {removed} entries, kept {kept} pinned (local history only)");
    } else {
        println!("removed {removed} entries (local history only)");
    }
    Ok(())
}

// ------------------------------------------------------------ service setup

/// Absolute path of this binary.
fn self_path() -> PathBuf {
    std::env::current_exe().expect("current_exe is always available")
}

/// The tray app next to this binary, if present. It bundles the HTTP service,
/// so on GUI platforms it is the preferred thing to auto-start.
///
/// Only the launchd and registry installers need it: the systemd unit runs
/// `peercarry serve` directly, so the helper would be dead code on Linux.
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn tray_path() -> Option<PathBuf> {
    let exe = self_path();
    let name = if cfg!(windows) {
        "peercarry-tray.exe"
    } else {
        "peercarry-tray"
    };
    let sibling = exe.with_file_name(name);
    if sibling.exists() {
        return Some(sibling);
    }
    // Common install locations outside of the release directory.
    let candidates = [
        PathBuf::from("/usr/local/bin").join(name),
        dirs::home_dir()?.join(".local/bin").join(name),
    ];
    candidates.into_iter().find(|p| p.exists())
}

async fn run_quiet(program: &str, args: &[&str]) -> Result<std::process::Output> {
    tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .with_context(|| format!("cannot execute {program}"))
}

/// Resolve `~`-relative paths for display.
#[cfg(not(windows))]
fn display_path(path: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(rest) = path.strip_prefix(&home) {
            return format!("~/{}", rest.display());
        }
    }
    path.display().to_string()
}

#[cfg(target_os = "macos")]
mod service {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const LABEL: &str = "cn.peercarry";

    extern "C" {
        fn getuid() -> u32;
    }

    fn uid() -> u32 {
        unsafe { getuid() }
    }

    fn plist_path() -> PathBuf {
        dirs::home_dir()
            .expect("home dir exists")
            .join("Library/LaunchAgents/cn.peercarry.plist")
    }

    fn plist_contents(program: &PathBuf, log: &PathBuf) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key><false/>
  </dict>
  <key>ProcessType</key><string>Interactive</string>
  <key>LimitLoadToSessionType</key><string>Aqua</string>
  <key>StandardOutPath</key><string>{}</string>
  <key>StandardErrorPath</key><string>{}</string>
</dict>
</plist>
"#,
            program.display(),
            log.display(),
            log.display()
        )
    }

    /// LaunchAgents is not always on disk, and launchd wants a file it can
    /// read without any ACL surprises.
    fn prepare_dir(path: &PathBuf) -> Result<()> {
        let dir = path.parent().expect("plist has a parent");
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    pub async fn install() -> Result<()> {
        let program = tray_path().unwrap_or_else(self_path);
        let log = peercarry_core::paths::data_dir().join("service.log");
        let path = plist_path();
        prepare_dir(&path)?;
        std::fs::write(&path, plist_contents(&program, &log))?;

        // A stale registration would make bootstrap fail with EIO.
        let uid = uid();
        let _ = run_quiet("launchctl", &["bootout", &format!("gui/{uid}/{LABEL}")]).await;

        match run_quiet(
            "launchctl",
            &[
                "bootstrap",
                &format!("gui/{uid}"),
                &path.display().to_string(),
            ],
        )
        .await
        {
            Ok(out) if out.status.success() => {
                println!("installed {} ({})", display_path(&path), program.display());
                println!("service started - it will also run at every login");
            }
            _ => {
                println!("installed {} ({})", display_path(&path), program.display());
                println!();
                println!("launchd did not accept the registration from this session.");
                println!("Run this once in your own terminal to activate it:");
                println!();
                println!("  launchctl bootstrap gui/{uid} {}", display_path(&path));
            }
        }
        Ok(())
    }

    pub async fn remove() -> Result<()> {
        let uid = uid();
        let _ = run_quiet("launchctl", &["bootout", &format!("gui/{uid}/{LABEL}")]).await;
        match std::fs::remove_file(plist_path()) {
            Ok(()) => println!("removed {}", display_path(&plist_path())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                println!("nothing to remove");
            }
            Err(e) => return Err(e.into()),
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod service {
    use super::*;

    /// A daemon started by systemd runs outside the graphical session, so
    /// `DISPLAY` / `WAYLAND_DISPLAY` are not in its environment and arboard
    /// could never touch the clipboard. This launcher re-attaches to whichever
    /// display sockets exist at start time.
    fn launcher_contents(program: &PathBuf) -> String {
        format!(
            r#"#!/bin/sh
# peercarry service launcher: attach to the active graphical session if present
u="${{XDG_RUNTIME_DIR:-/run/user/$(id -u)}}"
export XDG_RUNTIME_DIR="$u"
if [ -z "$WAYLAND_DISPLAY" ]; then
    for w in "$u"/wayland-*; do
        [ -S "$w" ] || continue
        WAYLAND_DISPLAY="${{w##*/}}"
        export WAYLAND_DISPLAY
        break
    done
fi
if [ -z "$DISPLAY" ]; then
    for x in /tmp/.X11-unix/X*; do
        [ -S "$x" ] || continue
        DISPLAY=":${{x#/tmp/.X11-unix/X}}"
        export DISPLAY
        break
    done
fi
if [ -z "$XAUTHORITY" ]; then
    for a in "$u"/.mutter-Xwaylandauth-* "$HOME/.Xauthority" "$u/gdm/Xauthority"; do
        [ -f "$a" ] || continue
        XAUTHORITY="$a"
        export XAUTHORITY
        break
    done
fi
exec "{}" serve
"#,
            program.display()
        )
    }

    fn unit_path() -> PathBuf {
        dirs::home_dir()
            .expect("home dir exists")
            .join(".config/systemd/user/peercarry.service")
    }

    fn launcher_path() -> PathBuf {
        dirs::home_dir()
            .expect("home dir exists")
            .join(".local/bin/peercarry-serve.sh")
    }

    fn unit_contents(launcher: &PathBuf) -> String {
        format!(
            r#"[Unit]
Description=peercarry daemon
# Clipboard access only exists inside a graphical session, so the daemon
# follows it: it starts when you log into the desktop and stops on logout.
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart={}
Restart=on-failure

[Install]
WantedBy=graphical-session.target
"#,
            launcher.display()
        )
    }

    pub async fn install() -> Result<()> {
        let program = self_path();
        let launcher = launcher_path();
        std::fs::create_dir_all(launcher.parent().expect("launcher has a parent"))?;
        std::fs::write(&launcher, launcher_contents(&program))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;

        let path = unit_path();
        std::fs::create_dir_all(path.parent().expect("unit has a parent"))?;
        std::fs::write(&path, unit_contents(&launcher))?;

        let _ = run_quiet("systemctl", &["--user", "daemon-reload"]).await;
        let enabled = run_quiet(
            "systemctl",
            &["--user", "enable", "--now", "peercarry.service"],
        )
        .await;

        match enabled {
            Ok(out) if out.status.success() => {
                let state = run_quiet("systemctl", &["--user", "is-active", "peercarry.service"])
                    .await
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default();
                println!("installed {} ({})", display_path(&path), program.display());
                if state == "active" {
                    println!("service started - it also starts with every desktop login");
                } else {
                    println!();
                    println!("no graphical session is running, so the daemon is not started");
                    println!("yet. It will start automatically when you log into the desktop.");
                    println!(
                        "(To run it anyway as a history relay: systemctl --user start peercarry)"
                    );
                }
            }
            _ => {
                println!("installed {}", display_path(&path));
                println!();
                println!("systemctl is not usable from this session. Run this once:");
                println!();
                println!("  systemctl --user daemon-reload");
                println!("  systemctl --user enable --now peercarry.service");
            }
        }
        Ok(())
    }

    pub async fn remove() -> Result<()> {
        let _ = run_quiet(
            "systemctl",
            &["--user", "disable", "--now", "peercarry.service"],
        )
        .await;
        let mut removed = false;
        for path in [unit_path(), launcher_path()] {
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    removed = true;
                    println!("removed {}", display_path(&path));
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        if removed {
            let _ = run_quiet("systemctl", &["--user", "daemon-reload"]).await;
        } else {
            println!("nothing to remove");
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
mod service {
    use super::*;

    const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE_NAME: &str = "peercarry";

    pub async fn install() -> Result<()> {
        let program = tray_path().unwrap_or_else(self_path);
        let out = run_quiet(
            "reg",
            &[
                "add",
                RUN_KEY,
                "/v",
                VALUE_NAME,
                "/t",
                "REG_SZ",
                "/d",
                &format!("\"{}\"", program.display()),
                "/f",
            ],
        )
        .await?;
        if !out.status.success() {
            return Err(anyhow!(
                "reg add failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        println!(
            "installed login entry for {} (HKCU ...\\Run)",
            program.display()
        );
        println!("it starts at your next login");
        Ok(())
    }

    pub async fn remove() -> Result<()> {
        let out = run_quiet("reg", &["delete", RUN_KEY, "/v", VALUE_NAME, "/f"]).await?;
        if out.status.success() {
            println!("removed login entry");
        } else {
            println!("nothing to remove");
        }
        Ok(())
    }
}

async fn cmd_install_service(remove: bool) -> Result<()> {
    if remove {
        service::remove().await
    } else {
        service::install().await
    }
}

async fn cmd_pin(backend: &Backend, config: &Config, selector: &str, off: bool) -> Result<()> {
    let entries = backend.list(config, 200).await?;
    let entry = select(&entries, Some(selector))?.clone();

    let updated = match backend {
        Backend::Daemon(d) => d.pin(&entry.id, !off).await?,
        Backend::Direct(e) => e
            .pin(&entry.id, !off)
            .await?
            .ok_or_else(|| anyhow!("entry not found"))?,
    };

    println!(
        "{} {}  [{}]",
        if off { "unpinned" } else { "pinned" },
        &updated.id[..8.min(updated.id.len())],
        updated.preview
    );
    Ok(())
}

// --------------------------------------------------------------------- main

fn init_logging(verbose: u8) {
    let default = match verbose {
        0 => "peercarry_core=warn",
        1 => "peercarry_core=info",
        2 => "peercarry_core=debug",
        _ => "debug",
    };
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| default.to_string());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    let command = match cli.command {
        Commands::Ai { command } => return ai::run(command).await,
        command => command,
    };

    let config = Config::load()?;

    match command {
        Commands::Ai { command } => ai::run(command).await,
        Commands::Serve => cmd_serve(config).await,

        Commands::Push => {
            let backend = Backend::connect(&config).await?;
            cmd_push(&backend, cli.json).await
        }

        Commands::List { limit, local, kind } => {
            let backend = Backend::connect(&config).await?;
            cmd_list(&backend, &config, limit, local, kind.as_deref(), cli.json).await
        }

        Commands::Thumb { selector, out } => {
            let backend = Backend::connect(&config).await?;
            cmd_thumb(&backend, &config, &selector, out.as_deref()).await
        }

        Commands::Search { words, limit } => {
            let backend = Backend::connect(&config).await?;
            cmd_search(&backend, &config, &words, limit, cli.json).await
        }

        Commands::Show { selector } => {
            let backend = Backend::connect(&config).await?;
            let entries = backend.list(&config, 200).await?;
            let entry = select(&entries, Some(&selector))?;
            println!("{}", serde_json::to_string_pretty(entry)?);
            Ok(())
        }

        Commands::Pull { selector } => {
            let backend = Backend::connect(&config).await?;
            cmd_pull(&backend, &config, selector).await
        }

        Commands::Send { host, id, files } => {
            let backend = Backend::connect(&config).await?;
            cmd_send(&backend, &config, host, id, &files, cli.json).await
        }

        Commands::Peers => cmd_peers(&config, cli.json).await,

        Commands::Status => {
            let backend = Backend::connect(&config).await?;
            cmd_status(&config, &backend).await
        }

        Commands::Delete { selector } => {
            let backend = Backend::connect(&config).await?;
            cmd_delete(&backend, &config, &selector).await
        }

        Commands::Pin { selector, off } => {
            let backend = Backend::connect(&config).await?;
            cmd_pin(&backend, &config, &selector, off).await
        }

        Commands::Clear { yes } => {
            let backend = Backend::connect(&config).await?;
            cmd_clear(&backend, yes).await
        }

        Commands::Dedupe => {
            let backend = Backend::connect(&config).await?;
            cmd_dedupe(&backend).await
        }

        Commands::Config { init } => {
            if init {
                config.save()?;
                println!("wrote {}", peercarry_core::paths::config_path().display());
            } else {
                // The active values, not a template: a template reads like
                // the current state and hides whatever you have changed.
                println!("# {}", peercarry_core::paths::config_path().display());
                print!("{}", config.to_toml()?);
            }
            Ok(())
        }

        Commands::Rename { name, clear } => cmd_rename(&config, name.as_deref(), clear).await,

        Commands::DownloadDir { path, reset } => {
            cmd_download_dir(&config, path.as_deref(), reset).await
        }

        Commands::Launch { name, on } => {
            let backend = Backend::connect(&config).await?;
            cmd_launch(&backend, &config, &name, on.as_deref()).await
        }

        Commands::Apps { command } => cmd_apps(&config, command).await,

        Commands::InstallService { remove } => cmd_install_service(remove).await,
    }
}
