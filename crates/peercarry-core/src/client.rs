//! HTTP client used to talk to other nodes.

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use futures_util::stream::{self, StreamExt};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::model::Entry;
use crate::protocol;

pub struct PeerClient {
    http: reqwest::Client,
    transfer_http: reqwest::Client,
    token: Option<String>,
    port: u16,
    concurrency: usize,
    max_transfer_bytes: u64,
    transfer_idle_timeout: Duration,
    transfer_prepare_timeout_secs: u64,
}

static DOWNLOAD_GUARD: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

struct DownloadGuard {
    key: String,
    set: &'static Mutex<HashSet<String>>,
}

impl Drop for DownloadGuard {
    fn drop(&mut self) {
        if let Ok(mut active) = self.set.lock() {
            active.remove(&self.key);
        }
    }
}

impl PeerClient {
    pub fn new(config: &Config) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(config.network.connect_timeout_secs))
            .timeout(Duration::from_secs(config.network.request_timeout_secs))
            // Tailnet traffic is already encrypted end to end and proxied
            // connections cannot reach 100.x addresses anyway. Honouring
            // HTTP_PROXY here would break pulls on machines with a system
            // proxy, so dial peers directly.
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| Error::Network(format!("cannot build client: {e}")))?;
        let transfer_http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(config.network.connect_timeout_secs))
            .no_proxy()
            .build()
            .map_err(|e| Error::Network(format!("cannot build transfer client: {e}")))?;

        Ok(Self {
            http,
            transfer_http,
            token: config.network.auth_token.clone(),
            port: config.network.port,
            concurrency: config.network.discovery_concurrency.max(1),
            max_transfer_bytes: config.limits.max_transfer_bytes,
            transfer_idle_timeout: Duration::from_secs(
                config.network.transfer_idle_timeout_secs.max(1),
            ),
            transfer_prepare_timeout_secs: config.network.transfer_prepare_timeout_secs.max(1),
        })
    }

    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut req = self.http.request(method, url);
        if let Some(token) = &self.token {
            if !token.is_empty() {
                req = req.header(protocol::TOKEN_HEADER, token);
            }
        }
        req
    }

    pub async fn hello(&self, addr: &str) -> Result<protocol::HelloResponse> {
        let url = format!("{}/v1/hello", addr.trim_end_matches('/'));
        let resp = self.request(reqwest::Method::GET, &url).send().await?;
        if !resp.status().is_success() {
            return Err(Error::PeerUnreachable {
                peer: addr.to_string(),
            });
        }
        Ok(resp.json().await?)
    }

    /// Explicitly request a fresh capture; never substitutes an old history item.
    pub async fn capture_remote(&self, addr: &str) -> Result<Entry> {
        let url = format!("{}/v1/actions/capture", addr.trim_end_matches('/'));
        let response = self.request(reqwest::Method::POST, &url).send().await?;
        if !response.status().is_success() {
            let reason = match response.status() {
                StatusCode::FORBIDDEN => "enable node.allow_remote_capture on the selected device",
                StatusCode::UNAUTHORIZED => "device authentication failed",
                StatusCode::NOT_FOUND => "update peercarry on the selected device",
                StatusCode::UNPROCESSABLE_ENTITY => "remote clipboard is empty or unavailable",
                _ => "remote clipboard capture failed",
            };
            return Err(Error::Network(format!("{}: {reason}", response.status())));
        }
        Ok(response.json().await?)
    }

    /// Probe a peer without parsing a body - cheaper than `hello`.
    pub async fn is_alive(&self, addr: &str) -> bool {
        match self.hello(addr).await {
            Ok(_) => true,
            Err(e) => {
                tracing::debug!("peer {addr} not responding: {e}");
                false
            }
        }
    }

    /// Names of the apps a peer offers for launching.
    pub async fn apps(&self, addr: &str) -> Result<Vec<String>> {
        let url = format!("{}/v1/apps", addr.trim_end_matches('/'));
        let resp = self.request(reqwest::Method::GET, &url).send().await?;
        if !resp.status().is_success() {
            return Err(Error::Network(format!(
                "app list failed: {}",
                resp.status()
            )));
        }
        Ok(resp.json().await?)
    }

    /// Ask a peer to launch one of its registered apps.
    pub async fn launch(&self, addr: &str, name: &str) -> Result<String> {
        let url = format!("{}/v1/actions/launch", addr.trim_end_matches('/'));
        let resp = self
            .request(reqwest::Method::POST, &url)
            .json(&protocol::LaunchRequest {
                name: name.to_string(),
            })
            .send()
            .await?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            return Err(Error::Unauthorized);
        }
        if !resp.status().is_success() {
            return Err(Error::Network(format!(
                "launch rejected by {addr}: {} (is the app registered and \
                 node.allow_remote_launch enabled there?)",
                resp.status()
            )));
        }
        Ok(resp.json::<protocol::ActionResult>().await?.detail)
    }

    /// Launch a registered app on the online peer whose host or DNS name
    /// contains `host`.
    pub async fn launch_on(&self, host: &str, name: &str) -> Result<String> {
        let needle = host.to_lowercase();
        let peers = crate::tailscale::peers(self.port).await?;
        let peer = peers
            .into_iter()
            .find(|p| {
                p.online
                    && (p.host.to_lowercase().contains(&needle)
                        || p.dns_name.to_lowercase().contains(&needle))
            })
            .ok_or_else(|| Error::PeerUnreachable {
                peer: host.to_string(),
            })?;
        let addr = peer
            .addr_with_port(self.port)
            .ok_or_else(|| Error::PeerUnreachable {
                peer: peer.host.clone(),
            })?;
        self.launch(&addr, name).await
    }

    /// Launchable apps advertised by every online peer that registered at
    /// least one, sorted by host. Offline or unreachable peers are skipped.
    pub async fn remote_apps(&self) -> Vec<(String, Vec<String>)> {
        let peers = crate::tailscale::peers(self.port).await.unwrap_or_default();

        let futs = peers.into_iter().filter(|p| p.online).map(|peer| {
            let client = &self;
            async move {
                let addr = peer.addr_with_port(client.port)?;
                let apps = client.apps(&addr).await.ok()?;
                (!apps.is_empty()).then(|| (peer.host.clone(), apps))
            }
        });

        let mut buffered = stream::iter(futs).buffer_unordered(self.concurrency);
        let mut out = Vec::new();
        while let Some(item) = buffered.next().await {
            if let Some(item) = item {
                out.push(item);
            }
        }
        out.sort();
        out
    }

    pub async fn list(&self, addr: &str, limit: usize) -> Result<Vec<Entry>> {
        let url = protocol::list_url(addr, Some(limit));
        let resp = self.request(reqwest::Method::GET, &url).send().await?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            return Err(Error::Unauthorized);
        }
        if !resp.status().is_success() {
            return Err(Error::Network(format!(
                "peer {} returned {}",
                addr,
                resp.status()
            )));
        }
        Ok(resp.json().await?)
    }

    /// Push an entry into a peer's history.
    pub async fn push(&self, addr: &str, entry: &Entry) -> Result<()> {
        let url = format!("{}/v1/entries", addr.trim_end_matches('/'));
        let resp = self
            .request(reqwest::Method::POST, &url)
            .json(entry)
            .send()
            .await?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            return Err(Error::Unauthorized);
        }
        if !resp.status().is_success() {
            return Err(Error::Network(format!(
                "peer {} rejected push: {}",
                addr,
                resp.status()
            )));
        }
        Ok(())
    }

    /// Fetch an image blob.
    /// Image preview for one entry, served by whichever node knows it.
    pub async fn fetch_thumb(&self, addr: &str, id: &str) -> Result<Vec<u8>> {
        let url = format!("{}/v1/entries/{}/thumb", addr.trim_end_matches('/'), id);
        let resp = self.request(reqwest::Method::GET, &url).send().await?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Err(Error::NotFound(format!("preview for {id} on {addr}")));
        }
        if !resp.status().is_success() {
            return Err(Error::Network(format!(
                "preview fetch failed: {}",
                resp.status()
            )));
        }
        Ok(resp.bytes().await?.to_vec())
    }

    pub async fn fetch_blob(&self, addr: &str, hash: &str) -> Result<Vec<u8>> {
        let url = format!("{}/v1/blobs/{}", addr.trim_end_matches('/'), hash);
        let resp = self.request(reqwest::Method::GET, &url).send().await?;
        if resp.status() == StatusCode::NOT_FOUND {
            return Err(Error::NotFound(format!("blob {hash} on {addr}")));
        }
        if !resp.status().is_success() {
            return Err(Error::Network(format!(
                "blob fetch failed: {}",
                resp.status()
            )));
        }
        Ok(resp.bytes().await?.to_vec())
    }

    /// Stream one captured file (or directory archive) into `dest_dir`.
    ///
    /// Returns the final path on disk. Names are de-duplicated so pulling the
    /// same entry twice never clobbers the previous copy. Directory payloads
    /// arrive as a tar stream and are unpacked here.
    pub async fn download_file(
        &self,
        addr: &str,
        entry_id: &str,
        index: usize,
        dest_dir: &Path,
    ) -> Result<PathBuf> {
        let url = protocol::entry_file_url(addr, entry_id, index);
        tokio::fs::create_dir_all(dest_dir).await?;
        let dest_dir = tokio::fs::canonicalize(dest_dir).await?;
        let guard_key = format!("{}\n{}", url, dest_dir.display());
        let guard = DOWNLOAD_GUARD.get_or_init(|| Mutex::new(HashSet::new()));
        {
            let mut active = guard
                .lock()
                .map_err(|_| Error::Network("download lock poisoned".into()))?;
            if !active.insert(guard_key.clone()) {
                return Err(Error::Network(
                    "same download is already in progress".into(),
                ));
            }
        }
        let _guard = DownloadGuard {
            key: guard_key,
            set: guard,
        };
        self.download_file_inner(&url, entry_id, index, &dest_dir)
            .await
    }

    async fn download_file_inner(
        &self,
        url: &str,
        entry_id: &str,
        index: usize,
        dest_dir: &Path,
    ) -> Result<PathBuf> {
        let mut restarted = false;
        let hash = hex::encode(Sha256::digest(url.as_bytes()));
        let lock_path = dest_dir.join(format!(".peercarry-{hash}.lock"));
        reject_symlink(&lock_path).await?;
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        lock_file
            .try_lock()
            .map_err(|e| Error::Network(format!("download lock unavailable: {e}")))?;
        loop {
            let part = dest_dir.join(format!(".peercarry-{hash}.part"));
            let meta_path = dest_dir.join(format!(".peercarry-{hash}.meta"));
            reject_symlink(&part).await?;
            reject_symlink(&meta_path).await?;
            let existing_len = tokio::fs::metadata(&part)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            let meta = read_download_meta(&meta_path).await?;
            let mut req = self.request_with_transfer(reqwest::Method::GET, url);
            let requested_range = existing_len > 0
                && meta
                    .as_ref()
                    .and_then(|m| m.etag.as_deref())
                    .is_some_and(is_strong_etag);
            if requested_range {
                req = req.header(reqwest::header::RANGE, format!("bytes={existing_len}-"));
                req = req.header(
                    reqwest::header::IF_RANGE,
                    meta.as_ref().unwrap().etag.as_ref().unwrap(),
                );
            }
            let resp = tokio::time::timeout(
                Duration::from_secs(self.transfer_prepare_timeout_secs.max(1)),
                req.send(),
            )
            .await
            .map_err(|_| Error::Network("transfer response preparation timeout".into()))??;
            if resp.status() == StatusCode::NOT_FOUND {
                return Err(Error::NotFound(format!(
                    "file #{index} of entry {entry_id}"
                )));
            }
            if matches!(
                resp.status(),
                StatusCode::PRECONDITION_FAILED | StatusCode::RANGE_NOT_SATISFIABLE
            ) {
                if !restarted {
                    restarted = true;
                    remove_partial(&part, &meta_path).await;
                    continue;
                }
                return Err(Error::Network(format!(
                    "file download failed: {}",
                    resp.status()
                )));
            }
            if resp.status() != StatusCode::OK && resp.status() != StatusCode::PARTIAL_CONTENT {
                return Err(Error::Network(format!(
                    "file download failed: {}",
                    resp.status()
                )));
            }

            let fallback = format!("download-{entry_id}-{index}.bin");
            let mut name = resp
                .headers()
                .get(protocol::FILENAME_HEADER)
                .map(|v| String::from_utf8_lossy(v.as_bytes()).to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or(fallback);
            let is_dir = resp
                .headers()
                .get(protocol::IS_DIR_HEADER)
                .and_then(|v| v.to_str().ok())
                == Some("1");
            if is_dir {
                if let Some(stem) = name.strip_suffix(".tar") {
                    name = stem.to_string();
                }
            }
            validate_download_name(&name)?;
            let etag = resp
                .headers()
                .get(reqwest::header::ETAG)
                .map(|v| v.to_str().unwrap_or_default().to_string())
                .filter(|s| !s.is_empty());
            let total = response_total(&resp)?;
            if total > self.max_transfer_bytes {
                return Err(Error::TooLarge {
                    size: total,
                    limit: self.max_transfer_bytes,
                });
            }
            if resp.status() == StatusCode::PARTIAL_CONTENT && !requested_range {
                return Err(Error::Network(
                    "unexpected 206 response without a requested range".into(),
                ));
            }
            let resumed = resp.status() == StatusCode::PARTIAL_CONTENT;
            if resumed {
                validate_partial_response(
                    &resp,
                    existing_len,
                    total,
                    etag.as_deref(),
                    meta.as_ref(),
                )?;
            } else if resp.status() == StatusCode::OK && existing_len > 0 {
                // A 200 means the peer ignored the range or the source changed.
                if existing_len > 0 {
                    tokio::fs::File::create(&part).await?;
                }
            }
            let mode_append = resumed;
            let new_meta = DownloadMeta {
                etag: etag.clone(),
                total,
                name: name.clone(),
            };
            write_download_meta(&meta_path, &new_meta).await?;
            write_response_to_part(
                resp,
                &part,
                mode_append,
                total,
                self.max_transfer_bytes,
                self.transfer_idle_timeout,
            )
            .await?;
            if let Some(etag) = etag.as_deref() {
                if is_strong_etag(etag) && !verify_part_hash(&part, etag).await? {
                    if !restarted {
                        restarted = true;
                        remove_partial(&part, &meta_path).await;
                        continue;
                    }
                    return Err(Error::Network("download ETag hash mismatch".into()));
                }
            }
            if is_dir {
                let stage = dest_dir.join(format!(
                    ".peercarry-{hash}.extract-{}",
                    uuid::Uuid::new_v4()
                ));
                let part_clone = part.clone();
                let stage_clone = stage.clone();
                let limit = self.max_transfer_bytes;
                let unpack = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
                    std::fs::create_dir_all(&stage_clone)?;
                    let file = std::fs::File::open(&part_clone)?;
                    unpack_bounded(file, &stage_clone, limit)
                })
                .await
                .map_err(|e| Error::Network(format!("unpack task failed: {e}")))?;
                if let Err(e) = unpack {
                    let _ = tokio::fs::remove_dir_all(&stage).await;
                    return Err(Error::Network(format!("cannot unpack tar: {e}")));
                }
                let dest = unique_path(dest_dir, &name);
                tokio::fs::rename(&stage, &dest).await?;
                remove_partial(&part, &meta_path).await;
                return Ok(dest);
            }
            let dest = unique_path(dest_dir, &name);
            match tokio::fs::hard_link(&part, &dest).await {
                Ok(()) => {
                    remove_partial(&part, &meta_path).await;
                    return Ok(dest);
                }
                Err(e) => {
                    return Err(Error::Network(format!(
                        "cannot atomically publish file (hard link required): {e}"
                    )))
                }
            }
        }
    }

    fn request_with_transfer(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut req = self.transfer_http.request(method, url);
        if let Some(token) = &self.token {
            if !token.is_empty() {
                req = req.header(protocol::TOKEN_HEADER, token);
            }
        }
        req
    }

    /// Query every online peer in parallel and merge their histories.
    ///
    /// Offline or unreachable peers are skipped rather than failing the call -
    /// a laptop that is asleep should not break the list on every other node.
    pub async fn aggregate(&self, local: Vec<Entry>, limit_per_peer: usize) -> Result<Vec<Entry>> {
        let peers = crate::tailscale::peers(self.port).await.unwrap_or_default();
        let online: Vec<_> = peers.into_iter().filter(|p| p.online).collect();

        if online.is_empty() {
            return Ok(local);
        }

        let mut merged = local;
        let limit = limit_per_peer;
        let futs = online.into_iter().map(|peer| {
            let client = &self;
            async move {
                let Some(addr) = peer.addr_with_port(client.port) else {
                    return (
                        peer.host.clone(),
                        Err(Error::PeerUnreachable {
                            peer: peer.host.clone(),
                        }),
                    );
                };
                let result = client.list(&addr, limit).await;
                (peer.host.clone(), result)
            }
        });

        let mut buffered = stream::iter(futs).buffer_unordered(self.concurrency);
        while let Some((host, result)) = buffered.next().await {
            match result {
                Ok(entries) => merged.extend(entries.into_iter().map(|mut e| {
                    e.holder = Some(host.clone());
                    e
                })),
                Err(e) => tracing::debug!("skipping peer {host}: {e}"),
            }
        }

        merged.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        merged.dedup_by(|a, b| a.id == b.id);
        Ok(merged)
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct DownloadMeta {
    etag: Option<String>,
    total: u64,
    name: String,
}

async fn read_download_meta(path: &Path) -> Result<Option<DownloadMeta>> {
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).ok()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

async fn write_download_meta(path: &Path, meta: &DownloadMeta) -> Result<()> {
    let bytes = serde_json::to_vec(meta)?;
    let tmp = path.with_extension("meta.tmp");
    reject_symlink(&tmp).await?;
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(tmp, path).await?;
    Ok(())
}

async fn remove_partial(part: &Path, meta: &Path) {
    let _ = tokio::fs::remove_file(part).await;
    let _ = tokio::fs::remove_file(meta).await;
}

async fn reject_symlink(path: &Path) -> Result<()> {
    if let Ok(meta) = tokio::fs::symlink_metadata(path).await {
        if meta.file_type().is_symlink() {
            return Err(Error::Network(format!(
                "refusing symlink partial path: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn validate_download_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\', ':']) {
        return Err(Error::Network("peer supplied an unsafe file name".into()));
    }
    Ok(())
}

fn unpack_bounded(file: std::fs::File, stage: &Path, limit: u64) -> std::io::Result<()> {
    let mut total = 0u64;
    let mut archive = tar::Archive::new(file);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err(std::io::Error::other(
                "archive contains unsupported links or special files",
            ));
        }
        let path = entry.path()?;
        if path.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        }) {
            return Err(std::io::Error::other("unsafe archive path"));
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| std::io::Error::other("archive size overflow"))?;
        if total > limit {
            return Err(std::io::Error::other(
                "extracted directory exceeds transfer limit",
            ));
        }
        if !entry.unpack_in(stage)? {
            return Err(std::io::Error::other("archive entry escaped destination"));
        }
    }
    Ok(())
}

fn is_strong_etag(etag: &str) -> bool {
    let Some(value) = etag.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
        return false;
    };
    let Some(hash) = value.strip_prefix("sha256-") else {
        return false;
    };
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

async fn verify_part_hash(path: &Path, etag: &str) -> Result<bool> {
    let expected = etag
        .trim()
        .trim_matches('"')
        .strip_prefix("sha256-")
        .unwrap_or("")
        .to_ascii_lowercase();
    let path = path.to_path_buf();
    let actual = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
        use std::io::Read;
        let mut file = std::fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hex::encode(hasher.finalize()))
    })
    .await
    .map_err(|e| Error::Network(format!("hash task failed: {e}")))??;
    Ok(actual == expected)
}

fn response_total(resp: &reqwest::Response) -> Result<u64> {
    if resp.status() == StatusCode::PARTIAL_CONTENT {
        let value = resp
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::Network("206 response missing Content-Range".into()))?;
        let total = value
            .rsplit('/')
            .next()
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| Error::Network("invalid Content-Range".into()))?;
        Ok(total)
    } else {
        Ok(resp.content_length().unwrap_or(0))
    }
}

fn validate_partial_response(
    resp: &reqwest::Response,
    start: u64,
    total: u64,
    etag: Option<&str>,
    meta: Option<&DownloadMeta>,
) -> Result<()> {
    let cr = resp
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let Some((spec, total_text)) = cr.strip_prefix("bytes ").and_then(|s| s.split_once('/')) else {
        return Err(Error::Network("206 Content-Range is invalid".into()));
    };
    if total_text.parse::<u64>().ok() != Some(total) {
        return Err(Error::Network("206 Content-Range total is invalid".into()));
    }
    let mut bounds = spec.split('-');
    let range_start = bounds.next().and_then(|v| v.parse().ok());
    let range_end = bounds.next().and_then(|v| v.parse().ok());
    if bounds.next().is_some()
        || range_start != Some(start)
        || range_end != Some(total.saturating_sub(1))
    {
        return Err(Error::Network(
            "206 Content-Range does not match partial length".into(),
        ));
    }
    if let Some(old_total) = meta.map(|m| m.total).filter(|v| *v > 0) {
        if old_total != total {
            return Err(Error::Network("206 total changed".into()));
        }
    }
    if let Some(old) = meta.and_then(|m| m.etag.as_deref()) {
        if etag != Some(old) {
            return Err(Error::Network("206 ETag changed".into()));
        }
    }
    if let Some(len) = resp.content_length() {
        if len != total.saturating_sub(start) {
            return Err(Error::Network("206 Content-Length mismatch".into()));
        }
    }
    Ok(())
}

async fn write_response_to_part(
    resp: reqwest::Response,
    part: &Path,
    append: bool,
    total: u64,
    limit: u64,
    idle: Duration,
) -> Result<()> {
    let mut stream = resp.bytes_stream();
    let mut file = if append {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(part)
            .await?
    } else {
        tokio::fs::File::create(part).await?
    };
    let mut written = if append {
        tokio::fs::metadata(part).await?.len()
    } else {
        0
    };
    loop {
        let next = match tokio::time::timeout(idle, stream.next()).await {
            Ok(next) => next,
            Err(_) => {
                let _ = file.flush().await;
                return Err(Error::Network("transfer idle timeout".into()));
            }
        };
        let Some(chunk) = next else {
            break;
        };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(e) => {
                let _ = file.flush().await;
                return Err(e.into());
            }
        };
        written = written.saturating_add(chunk.len() as u64);
        if written > limit {
            let _ = file.flush().await;
            return Err(Error::TooLarge {
                size: written,
                limit,
            });
        }
        if let Err(e) = file.write_all(&chunk).await {
            let _ = file.flush().await;
            return Err(e.into());
        }
    }
    file.flush().await?;
    file.sync_all().await?;
    if total > 0 && written != total {
        return Err(Error::Network(format!(
            "transfer length mismatch: got {written}, expected {total}"
        )));
    }
    Ok(())
}

/// Append ` (n)` before the extension until the path is free.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| name.to_string());
    let ext = Path::new(name)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();

    for i in 1..1000 {
        let candidate = dir.join(format!("{stem} ({i}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem}-{}{ext}", chrono::Utc::now().timestamp()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, Bytes},
        http::{Request, StatusCode},
        response::Response,
        routing::any,
        Router,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn transfer_mock_interruption_leaves_part_then_resumes_with_range() {
        let payload = b"0123456789abcdef".to_vec();
        let etag = format!(r#""sha256-{}""#, hex::encode(Sha256::digest(&payload)));
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_handler = calls.clone();
        let payload_for_handler = payload.clone();
        let etag_for_handler = etag.clone();
        let app = Router::new().fallback(any(move |req: Request<Body>| {
            let calls = calls_for_handler.clone();
            let payload = payload_for_handler.clone();
            let etag = etag_for_handler.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                let range = req
                    .headers()
                    .get(reqwest::header::RANGE)
                    .and_then(|v| v.to_str().ok());
                let mut response = Response::builder()
                    .header(protocol::FILENAME_HEADER, "resume.bin")
                    .header(reqwest::header::ETAG, etag);
                assert_eq!(range, Some("bytes=5-"));
                response = response
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(
                        reqwest::header::CONTENT_RANGE,
                        format!("bytes 5-15/{}", payload.len()),
                    )
                    .header(reqwest::header::CONTENT_LENGTH, payload.len() - 5);
                response
                    .body(Body::from(Bytes::copy_from_slice(&payload[5..])))
                    .unwrap()
            }
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let mut cfg = Config::default();
        cfg.limits.max_transfer_bytes = 1024;
        let client = PeerClient::new(&cfg).unwrap();
        let dir = std::env::temp_dir().join(format!("peercarry-test-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let url = protocol::entry_file_url(&format!("http://{addr}"), "e", 0);
        let hash = hex::encode(Sha256::digest(url.as_bytes()));
        tokio::fs::write(dir.join(format!(".peercarry-{hash}.part")), &payload[..5])
            .await
            .unwrap();
        write_download_meta(
            &dir.join(format!(".peercarry-{hash}.meta")),
            &DownloadMeta {
                etag: Some(etag),
                total: payload.len() as u64,
                name: "resume.bin".into(),
            },
        )
        .await
        .unwrap();
        assert!(!dir.join("resume.bin").exists());
        let final_path = client
            .download_file(&format!("http://{addr}"), "e", 0, &dir)
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&final_path).await.unwrap(), payload);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let _ = tokio::fs::remove_dir_all(&dir).await;
        server.abort();
    }

    #[test]
    fn transfer_rejects_filename_traversal_and_weak_etag_resume() {
        assert!(validate_download_name("../evil").is_err());
        assert!(validate_download_name("a:b").is_err());
        assert!(!is_strong_etag(
            r#"W/"sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa""#
        ));
    }

    #[tokio::test]
    async fn transfer_cancel_after_first_chunk_preserves_part_then_resumes() {
        let payload = b"0123456789abcdef".to_vec();
        let etag = format!(r#""sha256-{}""#, hex::encode(Sha256::digest(&payload)));
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_handler = calls.clone();
        let payload_for_handler = payload.clone();
        let etag_for_handler = etag.clone();
        let app = Router::new().fallback(any(move |req: Request<Body>| {
            let calls = calls_for_handler.clone();
            let payload = payload_for_handler.clone();
            let etag = etag_for_handler.clone();
            async move {
                let call = calls.fetch_add(1, Ordering::SeqCst);
                let range = req
                    .headers()
                    .get(reqwest::header::RANGE)
                    .and_then(|v| v.to_str().ok());
                let mut response = Response::builder()
                    .header(protocol::FILENAME_HEADER, "cancel.bin")
                    .header(reqwest::header::ETAG, etag);
                if call == 0 {
                    let first = Bytes::copy_from_slice(&payload[..8]);
                    let body = Body::from_stream(
                        futures_util::stream::once(
                            async move { Ok::<Bytes, std::io::Error>(first) },
                        )
                        .chain(futures_util::stream::once(async {
                            tokio::time::sleep(Duration::from_secs(5)).await;
                            Ok::<Bytes, std::io::Error>(Bytes::new())
                        })),
                    );
                    return response
                        .status(StatusCode::OK)
                        .header(reqwest::header::CONTENT_LENGTH, payload.len())
                        .body(body)
                        .unwrap();
                }
                assert_eq!(range, Some("bytes=8-"));
                response = response
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(
                        reqwest::header::CONTENT_RANGE,
                        format!("bytes 8-15/{}", payload.len()),
                    )
                    .header(reqwest::header::CONTENT_LENGTH, payload.len() - 8);
                response
                    .body(Body::from(Bytes::copy_from_slice(&payload[8..])))
                    .unwrap()
            }
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut cfg = Config::default();
        cfg.limits.max_transfer_bytes = 1024;
        let client = PeerClient::new(&cfg).unwrap();
        let dir = std::env::temp_dir().join(format!("peercarry-cancel-{}", uuid::Uuid::new_v4()));
        let base = format!("http://{addr}");
        let cancelled = tokio::time::timeout(
            Duration::from_millis(250),
            client.download_file(&base, "cancel", 0, &dir),
        )
        .await;
        assert!(cancelled.is_err(), "first transfer should be cancelled");
        assert!(!dir.join("cancel.bin").exists());
        let url = protocol::entry_file_url(&base, "cancel", 0);
        let hash = hex::encode(Sha256::digest(url.as_bytes()));
        let part = dir.join(format!(".peercarry-{hash}.part"));
        assert!(tokio::fs::metadata(&part).await.unwrap().len() >= 8);
        let final_path = client
            .download_file(&base, "cancel", 0, &dir)
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(final_path).await.unwrap(), payload);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let _ = tokio::fs::remove_dir_all(&dir).await;
        server.abort();
    }

    #[tokio::test]
    async fn transfer_200_replaces_smaller_stale_partial_without_tail() {
        let old = b"old-partial-data".to_vec();
        let payload = b"new".to_vec();
        let etag = format!(r#""sha256-{}""#, hex::encode(Sha256::digest(&payload)));
        let app = Router::new().fallback(any(move |_req: Request<Body>| {
            let payload = payload.clone();
            let etag = etag.clone();
            async move {
                Response::builder()
                    .status(StatusCode::OK)
                    .header(protocol::FILENAME_HEADER, "replace.bin")
                    .header(reqwest::header::ETAG, etag)
                    .header(reqwest::header::CONTENT_LENGTH, payload.len())
                    .body(Body::from(payload))
                    .unwrap()
            }
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = PeerClient::new(&Config::default()).unwrap();
        let dir = std::env::temp_dir().join(format!("peercarry-replace-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let base = format!("http://{addr}");
        let url = protocol::entry_file_url(&base, "replace", 0);
        let hash = hex::encode(Sha256::digest(url.as_bytes()));
        tokio::fs::write(dir.join(format!(".peercarry-{hash}.part")), &old)
            .await
            .unwrap();
        write_download_meta(
            &dir.join(format!(".peercarry-{hash}.meta")),
            &DownloadMeta {
                etag: Some(
                    r#""sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa""#
                        .into(),
                ),
                total: old.len() as u64,
                name: "replace.bin".into(),
            },
        )
        .await
        .unwrap();
        let final_path = client
            .download_file(&base, "replace", 0, &dir)
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(final_path).await.unwrap(), b"new");
        let _ = tokio::fs::remove_dir_all(&dir).await;
        server.abort();
    }

    #[tokio::test]
    async fn transfer_wrong_strong_etag_retries_once_and_never_publishes() {
        let payload = b"wrong-hash".to_vec();
        let requests = Arc::new(AtomicUsize::new(0));
        let requests_for_handler = requests.clone();
        let app = Router::new().fallback(any(move |_req: Request<Body>| {
            let requests = requests_for_handler.clone();
            let payload = payload.clone();
            async move {
                requests.fetch_add(1, Ordering::SeqCst);
                Response::builder()
                    .status(StatusCode::OK)
                    .header(protocol::FILENAME_HEADER, "bad.bin")
                    .header(reqwest::header::ETAG, r#""sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa""#)
                    .header(reqwest::header::CONTENT_LENGTH, payload.len())
                    .body(Body::from(payload))
                    .unwrap()
            }
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = PeerClient::new(&Config::default()).unwrap();
        let dir = std::env::temp_dir().join(format!("peercarry-hash-{}", uuid::Uuid::new_v4()));
        let result = client
            .download_file(&format!("http://{addr}"), "bad", 0, &dir)
            .await;
        assert!(result.is_err());
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        assert!(!dir.join("bad.bin").exists());
        let _ = tokio::fs::remove_dir_all(&dir).await;
        server.abort();
    }

    #[tokio::test]
    async fn transfer_rejects_content_length_over_limit_without_final() {
        let payload = vec![b'x'; 16];
        let app = Router::new().fallback(any(move |_req: Request<Body>| {
            let payload = payload.clone();
            async move {
                Response::builder()
                    .status(StatusCode::OK)
                    .header(protocol::FILENAME_HEADER, "large.bin")
                    .header(reqwest::header::CONTENT_LENGTH, payload.len())
                    .body(Body::from(payload))
                    .unwrap()
            }
        }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut cfg = Config::default();
        cfg.limits.max_transfer_bytes = 4;
        let client = PeerClient::new(&cfg).unwrap();
        let dir = std::env::temp_dir().join(format!("peercarry-limit-{}", uuid::Uuid::new_v4()));
        let result = client
            .download_file(&format!("http://{addr}"), "large", 0, &dir)
            .await;
        assert!(matches!(result, Err(Error::TooLarge { .. })));
        assert!(!dir.join("large.bin").exists());
        let _ = tokio::fs::remove_dir_all(&dir).await;
        server.abort();
    }
    #[test]
    fn archive_limit_and_links_are_rejected_before_extraction() {
        let root =
            std::env::temp_dir().join(format!("peercarry-tar-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let archive = root.join("test.tar");
        let mut builder = tar::Builder::new(std::fs::File::create(&archive).unwrap());
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o600);
        header.set_cksum();
        builder
            .append_data(&mut header, "sample.txt", &b"data"[..])
            .unwrap();
        builder.finish().unwrap();
        drop(builder);
        let stage = root.join("stage");
        std::fs::create_dir(&stage).unwrap();
        assert!(unpack_bounded(std::fs::File::open(&archive).unwrap(), &stage, 3).is_err());
        assert!(!stage.join("sample.txt").exists());
        unpack_bounded(std::fs::File::open(&archive).unwrap(), &stage, 4).unwrap();
        assert_eq!(std::fs::read(stage.join("sample.txt")).unwrap(), b"data");
        let mut builder = tar::Builder::new(std::fs::File::create(&archive).unwrap());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        builder
            .append_link(&mut header, "escape", "../outside")
            .unwrap();
        builder.finish().unwrap();
        drop(builder);
        assert!(unpack_bounded(std::fs::File::open(&archive).unwrap(), &stage, 100).is_err());
        assert!(!stage.join("escape").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
