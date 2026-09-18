//! High level operations shared by the CLI and the tray app.
//!
//! Nothing in this module touches the clipboard unless you call it: peercarry
//! is strictly pull/push on demand.

use std::collections::HashSet;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use chrono::Utc;
use futures_util::stream::{self, StreamExt};
use image::ImageEncoder;
use sha2::{Digest, Sha256};

use crate::client::PeerClient;
use crate::clipboard;
use crate::config::{AppEntry, Config};
use crate::error::{Error, Result};
use crate::model::{BlobRef, ClipboardPayload, Entry, EntryKind, FileRef, Origin, Peer};
use crate::store::Store;

pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Outcome of applying an entry to the local clipboard.
#[derive(Debug, Clone)]
pub struct ApplyResult {
    pub kind: EntryKind,
    /// Human readable summary, e.g. `3 files -> ~/Downloads/peercarry`.
    pub detail: String,
    /// Local paths involved (downloaded files).
    pub paths: Vec<PathBuf>,
}

/// Content resolved and ready to be written to the clipboard.
///
/// Splitting this from the write itself lets a UI do all the async work on a
/// background task and apply the result on the main thread, which is what
/// macOS wants for pasteboard access.
#[derive(Debug, Clone)]
pub enum ClipboardWrite {
    Text(String),
    Image {
        width: usize,
        height: usize,
        rgba: Vec<u8>,
    },
    Files(Vec<PathBuf>),
}

/// What a capture produced.
#[derive(Debug, Clone)]
pub struct CaptureOutcome {
    /// The entry in the history: newly created, or the one already holding
    /// this content.
    pub entry: Entry,
    /// `true` when the content was already held by a pinned entry, so the
    /// existing record was kept instead of writing a fresh one.
    pub deduped: bool,
}

pub struct Engine {
    pub config: Arc<Config>,
    pub store: Arc<Store>,
    pub client: PeerClient,
    /// Identity stamped on entries captured here. Filled by the daemon; the
    /// CLI falls back to a locally built one.
    pub origin: Origin,
    /// Runtime override of `storage.download_dir`, so a folder picked in the
    /// tray takes effect on the next pull instead of after a restart.
    download_dir: RwLock<Option<PathBuf>>,
}

impl Engine {
    pub fn new(config: Arc<Config>, store: Arc<Store>, origin: Origin) -> Result<Self> {
        let client = PeerClient::new(&config)?;
        Ok(Self {
            config,
            store,
            client,
            origin,
            download_dir: RwLock::new(None),
        })
    }

    /// Override where pulled files land; see [`Engine::download_dir`].
    pub fn set_download_dir(&self, dir: PathBuf) {
        if let Some(parent) = dir.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::create_dir_all(&dir);
        *self.download_dir.write().expect("download_dir lock") = Some(dir);
    }

    /// Drop the runtime override so `storage.download_dir` from the config
    /// applies again (the settings page clears it when the field is empty).
    pub fn clear_download_dir(&self) {
        *self.download_dir.write().expect("download_dir lock") = None;
    }

    /// Directory receiving pulled files: the runtime override if one was set,
    /// otherwise the config value, otherwise the platform default.
    pub fn download_dir(&self) -> PathBuf {
        self.download_dir
            .read()
            .expect("download_dir lock")
            .clone()
            .unwrap_or_else(|| self.config.download_dir())
    }

    /// Origin for CLI invocations that run without a daemon.
    ///
    /// The address must be the Tailscale IP, not loopback: it is what peers
    /// later use to fetch blobs for entries captured here.
    pub async fn local_origin(config: &Config) -> Origin {
        let ip = crate::tailscale::self_ipv4().await.ok().flatten();
        let addr = match ip {
            Some(ip) => format!("http://{ip}:{}", config.network.port),
            None => {
                tracing::warn!(
                    "no Tailscale IPv4 found; entries captured now may not be pullable by peers"
                );
                format!("http://127.0.0.1:{}", config.network.port)
            }
        };
        let node = crate::tailscale::self_node().await.ok().flatten();

        Origin {
            host: config.display_name(),
            node_id: node.and_then(|n| n.id).unwrap_or_default(),
            addr,
            os: crate::server::os_label().to_string(),
        }
    }

    // ------------------------------------------------------------- capturing

    /// Read the system clipboard and record it in the local history.
    pub async fn capture(&self) -> Result<CaptureOutcome> {
        let payload = clipboard::read()?;

        let entry = match payload {
            ClipboardPayload::Empty => return Err(Error::Clipboard("clipboard is empty".into())),
            ClipboardPayload::Text(text) => self.entry_from_text(text)?,
            ClipboardPayload::Image {
                width,
                height,
                rgba,
            } => self.entry_from_image(width, height, rgba)?,
            ClipboardPayload::Files(files) => self.entry_from_files(files)?,
        };

        // Copying the same thing twice should not fill the history with
        // copies: when the content is already here the stale record is
        // dropped, so the fresh capture takes its place at the top. A pinned
        // record is a promise to keep it, so re-capturing its content points
        // at it instead of replacing it.
        if self.config.storage.dedupe {
            if let Some(existing) = self.find_duplicate(&entry)? {
                if existing.pinned {
                    tracing::info!(
                        "already pinned in history as {} (captured {})",
                        existing.id,
                        existing.created_at
                    );
                    return Ok(CaptureOutcome {
                        entry: existing,
                        deduped: true,
                    });
                }
                tracing::info!("replacing stale copy {} with fresh capture", existing.id);
                self.store.delete(&existing.id)?;
            }
        }

        self.store.insert(&entry)?;
        self.store.prune(self.config.storage.history_limit)?;
        tracing::info!(
            "captured {} entry {} ({} bytes)",
            entry.kind.as_str(),
            entry.id,
            entry.size
        );
        Ok(CaptureOutcome {
            entry,
            deduped: false,
        })
    }

    /// Record the given files/directories in the local history without going
    /// through the clipboard - "send these files" instead of "send what I
    /// copied". Only the paths and sizes are stored, exactly like a clipboard
    /// file capture; peers stream the bytes when they pull.
    pub fn capture_files(&self, paths: &[PathBuf]) -> Result<CaptureOutcome> {
        if paths.is_empty() {
            return Err(Error::Clipboard("no files given".into()));
        }
        let missing: Vec<String> = paths
            .iter()
            .filter(|p| !p.exists())
            .map(|p| p.display().to_string())
            .collect();
        if !missing.is_empty() {
            return Err(Error::Clipboard(format!(
                "no such file: {}",
                missing.join(", ")
            )));
        }

        let entry = self.entry_from_files(clipboard::describe_paths(paths))?;

        // Same dedupe contract as clipboard capture: content already in the
        // history is pointed at, not copied.
        if self.config.storage.dedupe {
            if let Some(existing) = self.find_duplicate(&entry)? {
                tracing::info!(
                    "already in history as {} (captured {})",
                    existing.id,
                    existing.created_at
                );
                return Ok(CaptureOutcome {
                    entry: existing,
                    deduped: true,
                });
            }
        }

        self.store.insert(&entry)?;
        self.store.prune(self.config.storage.history_limit)?;
        tracing::info!(
            "captured {} entry {} ({} bytes) from picked paths",
            entry.kind.as_str(),
            entry.id,
            entry.size
        );
        Ok(CaptureOutcome {
            entry,
            deduped: false,
        })
    }

    /// The entry already holding this content, newest first.
    pub fn find_duplicate(&self, entry: &Entry) -> Result<Option<Entry>> {
        let wanted = entry.fingerprint();
        Ok(self
            .store
            .list(self.config.storage.history_limit.max(1), None)?
            .into_iter()
            .find(|existing| existing.id != entry.id && existing.fingerprint() == wanted))
    }

    /// Drop older copies of the same content, keeping the newest of each.
    ///
    /// Pinned entries are left alone: pinning is a promise to keep that
    /// record, and a duplicate is still a copy of it.
    pub fn dedupe(&self) -> Result<usize> {
        let mut entries = self
            .store
            .list(self.config.storage.history_limit.max(1), None)?;
        // Newest first, so the first one seen for a fingerprint survives.
        entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        let mut seen: HashSet<String> = HashSet::new();
        let mut removed = 0;
        for entry in entries {
            if entry.pinned {
                continue;
            }
            let fingerprint = entry.fingerprint();
            if seen.contains(&fingerprint) {
                if self.store.delete(&entry.id)? {
                    removed += 1;
                }
            } else {
                seen.insert(fingerprint);
            }
        }
        if removed > 0 {
            tracing::info!("removed {removed} duplicate entries");
        }
        Ok(removed)
    }

    fn new_entry(&self, kind: EntryKind) -> Entry {
        Entry {
            id: uuid::Uuid::new_v4().to_string(),
            kind,
            origin: self.origin.clone(),
            created_at: Utc::now(),
            text: None,
            blob: None,
            thumb: None,
            files: None,
            preview: String::new(),
            pinned: false,
            size: 0,
            holder: None,
        }
    }

    fn entry_from_text(&self, text: String) -> Result<Entry> {
        let mut entry = self.new_entry(EntryKind::Text);
        let size = text.len() as u64;

        if size <= self.config.limits.inline_text_bytes {
            entry.text = Some(text);
        } else {
            // Huge text (a whole log file, say) is stored as a blob so the
            // history stays light and peers only pay for what they pull.
            let hash = hash_bytes(text.as_bytes());
            self.store.write_blob(&hash, text.as_bytes())?;
            entry.blob = Some(BlobRef {
                hash,
                size,
                mime: Some("text/plain".to_string()),
                inline: None,
                width: None,
                height: None,
            });
        }

        entry.size = size;
        entry.preview = entry.build_preview();
        Ok(entry)
    }

    fn entry_from_image(&self, width: usize, height: usize, rgba: Vec<u8>) -> Result<Entry> {
        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new(&mut buf).write_image(
            &rgba,
            width as u32,
            height as u32,
            image::ExtendedColorType::Rgba8,
        )?;

        let mut entry = self.new_entry(EntryKind::Image);
        let size = buf.len() as u64;
        let hash = hash_bytes(&buf);

        let inline = if size <= self.config.limits.inline_image_bytes {
            Some(buf.clone())
        } else {
            None
        };
        if inline.is_none() {
            // Keep the bytes locally; peers fetch them on demand.
            self.store.write_blob(&hash, &buf)?;
        }

        entry.blob = Some(BlobRef {
            hash,
            size,
            mime: Some("image/png".to_string()),
            inline,
            width: Some(width as u32),
            height: Some(height as u32),
        });
        entry.size = size;
        entry.thumb = self.thumbnail(width as u32, height as u32, &rgba)?;
        entry.preview = entry.build_preview();
        Ok(entry)
    }

    /// Downscale an image to a small PNG and store it as a blob.
    ///
    /// Peers pull this by hash instead of the full image, so a list of
    /// thumbnails costs kilobytes rather than megabytes. A failure here only
    /// costs the preview, never the entry, so errors are logged and swallowed.
    fn thumbnail(&self, width: u32, height: u32, rgba: &[u8]) -> Result<Option<BlobRef>> {
        let max = self.config.storage.thumbnail_px.max(16);
        let Some(image) = image::RgbaImage::from_raw(width, height, rgba.to_vec()) else {
            tracing::debug!("cannot build thumbnail: bad image buffer");
            return Ok(None);
        };
        if width == 0 || height == 0 {
            return Ok(None);
        }

        // Keep the aspect ratio and never upscale.
        let scale = (max as f64 / width.max(height) as f64).min(1.0);
        let tw = ((width as f64 * scale).round() as u32).max(1);
        let th = ((height as f64 * scale).round() as u32).max(1);

        let small = image::imageops::thumbnail(&image, tw, th);
        let mut buf = Vec::new();
        image::codecs::png::PngEncoder::new(&mut buf).write_image(
            &small,
            tw,
            th,
            image::ExtendedColorType::Rgba8,
        )?;

        let hash = hash_bytes(&buf);
        self.store.write_blob(&hash, &buf)?;
        tracing::debug!("thumbnail {tw}x{th} ({} bytes)", buf.len());

        Ok(Some(BlobRef {
            hash,
            size: buf.len() as u64,
            mime: Some("image/png".to_string()),
            inline: None,
            width: Some(tw),
            height: Some(th),
        }))
    }

    /// Files are recorded as references only - the bytes are never copied here.
    fn entry_from_files(&self, files: Vec<FileRef>) -> Result<Entry> {
        let mut entry = self.new_entry(EntryKind::Files);
        entry.size = files.iter().map(|f| f.size).sum();
        entry.files = Some(files);
        entry.preview = entry.build_preview();
        Ok(entry)
    }

    // -------------------------------------------------------------- applying

    /// Put an entry onto the local clipboard, fetching bytes if needed.
    pub async fn apply(&self, entry: &Entry) -> Result<ApplyResult> {
        let (payload, result) = self.prepare(entry).await?;
        Self::write(payload)?;
        Ok(result)
    }

    /// Resolve everything needed to write `entry` to the clipboard.
    ///
    /// Every network and disk operation happens here, so the caller can hand
    /// the result to [`Engine::write`] on a different thread.
    pub async fn prepare(&self, entry: &Entry) -> Result<(ClipboardWrite, ApplyResult)> {
        match entry.kind {
            EntryKind::Text => {
                let text = self.resolve_text(entry).await?;
                let detail = format!("{} chars", text.chars().count());
                Ok((
                    ClipboardWrite::Text(text),
                    ApplyResult {
                        kind: EntryKind::Text,
                        detail,
                        paths: Vec::new(),
                    },
                ))
            }
            EntryKind::Image => {
                let png = self.resolve_blob(entry).await?;
                let (width, height, rgba) = decode_png(&png)?;
                let detail = format!("{width}x{height} image");
                Ok((
                    ClipboardWrite::Image {
                        width,
                        height,
                        rgba,
                    },
                    ApplyResult {
                        kind: EntryKind::Image,
                        detail,
                        paths: Vec::new(),
                    },
                ))
            }
            EntryKind::Files => {
                let (paths, result) = self.prepare_files(entry).await?;
                Ok((ClipboardWrite::Files(paths), result))
            }
        }
    }

    /// Write already-resolved content to the clipboard.
    ///
    /// Synchronous and free of I/O beyond the clipboard itself, so it is safe
    /// to call from the main thread of a GUI app.
    pub fn write(payload: ClipboardWrite) -> Result<()> {
        match payload {
            ClipboardWrite::Text(text) => clipboard::write_text(&text),
            ClipboardWrite::Image {
                width,
                height,
                rgba,
            } => clipboard::write_image(width, height, &rgba),
            ClipboardWrite::Files(paths) => clipboard::write_files(&paths),
        }
    }

    async fn resolve_text(&self, entry: &Entry) -> Result<String> {
        if let Some(text) = &entry.text {
            return Ok(text.clone());
        }
        if entry.blob.is_none() {
            return Err(Error::NotFound(entry.id.clone()));
        }
        let bytes = self.resolve_blob(entry).await?;
        String::from_utf8(bytes).map_err(|e| Error::Storage(format!("text is not utf-8: {e}")))
    }

    /// Bytes for an entry's blob, from inline data, the local cache, or the
    /// origin machine - in that order.
    async fn resolve_blob(&self, entry: &Entry) -> Result<Vec<u8>> {
        let Some(blob) = &entry.blob else {
            return Err(Error::NotFound(entry.id.clone()));
        };
        self.resolve_blob_ref(entry, blob).await
    }

    /// Thumbnail bytes for an image entry, fetched from the origin when this
    /// machine does not hold it yet. `None` when the entry has no thumbnail
    /// (text, files, or a preview that could not be generated).
    pub async fn thumbnail_bytes(&self, entry: &Entry) -> Result<Option<Vec<u8>>> {
        match &entry.thumb {
            Some(thumb) => self.resolve_blob_ref(entry, thumb).await.map(Some),
            None => Ok(None),
        }
    }

    /// Thumbnail for an entry found by id, looked up across peers like any
    /// other entry. Errors when the entry has no preview image.
    pub async fn thumbnail_for(&self, id: &str) -> Result<Vec<u8>> {
        let entry = self
            .find(id)
            .await?
            .ok_or_else(|| Error::NotFound(id.to_string()))?;
        self.thumbnail_bytes(&entry)
            .await?
            .ok_or_else(|| Error::NotFound(format!("no preview for {id}")))
    }

    /// Resolve any blob belonging to `entry`: inline data, the local blob
    /// cache, then the machine that captured it.
    async fn resolve_blob_ref(&self, entry: &Entry, blob: &BlobRef) -> Result<Vec<u8>> {
        if let Some(inline) = &blob.inline {
            return Ok(inline.clone());
        }
        if let Some(bytes) = self.store.read_blob(&blob.hash)? {
            return Ok(bytes);
        }

        let addr = self.origin_addr(entry);
        tracing::info!("fetching blob {} from {}", blob.hash, addr);
        let bytes = match self.client.fetch_blob(&addr, &blob.hash).await {
            Ok(bytes) => bytes,
            Err(first) => {
                // The recorded origin address can be stale; a peer holding
                // the entry may have the blob cached and be able to serve it.
                tracing::warn!("blob fetch from origin {addr} failed: {first}");
                match self.peer_holding(&entry.id).await {
                    Some(alt) if alt != addr => self
                        .client
                        .fetch_blob(&alt, &blob.hash)
                        .await
                        .map_err(|second| {
                            tracing::warn!("blob fetch from peer {alt} failed: {second}");
                            first
                        })?,
                    _ => return Err(first),
                }
            }
        };
        // Cache so a second pull of the same image does not hit the network.
        self.store.write_blob(&blob.hash, &bytes)?;
        Ok(bytes)
    }

    /// Download (or locate) every file in a Files entry.
    async fn prepare_files(&self, entry: &Entry) -> Result<(Vec<PathBuf>, ApplyResult)> {
        let files = entry.files.clone().unwrap_or_default();
        if files.is_empty() {
            return Err(Error::NotFound(entry.id.clone()));
        }

        let dest_dir = self.download_dir();
        // Resolved lazily on the first failed download and reused for the
        // rest of the batch: locating the holding peer costs one list call
        // per online node.
        let mut fallback_addr: Option<Option<String>> = None;
        let mut paths = Vec::with_capacity(files.len());

        for (index, file) in files.iter().enumerate() {
            // Same-machine case: the original path is still valid, use it.
            let original = PathBuf::from(&file.path);
            if original.exists() && self.is_local(entry) {
                paths.push(original);
                continue;
            }
            let downloaded = self
                .download_file_with_fallback(entry, file, index, &dest_dir, &mut fallback_addr)
                .await?;
            paths.push(downloaded);
        }

        let detail = format!("{} item(s) -> {}", paths.len(), dest_dir.display());
        Ok((
            paths.clone(),
            ApplyResult {
                kind: EntryKind::Files,
                detail,
                paths,
            },
        ))
    }

    /// Fetch one file of `entry`, first from the recorded origin, then from
    /// whichever peer actually holds the entry - the origin address can go
    /// stale (host renamed, IP changed, entry dropped from the origin's own
    /// history). Only the origin can serve the bytes: a peer's copy is
    /// metadata, it opens the recorded path on its own disk.
    async fn download_file_with_fallback(
        &self,
        entry: &Entry,
        _file: &FileRef,
        index: usize,
        dest_dir: &std::path::Path,
        fallback_addr: &mut Option<Option<String>>,
    ) -> Result<PathBuf> {
        let primary = self.origin_addr(entry);
        let first_error = match self
            .client
            .download_file(&primary, &entry.id, index, dest_dir)
            .await
        {
            Ok(path) => return Ok(path),
            Err(first) => {
                tracing::warn!("file download from origin {primary} failed: {first}");
                first
            }
        };
        if fallback_addr.is_none() {
            *fallback_addr = Some(self.peer_holding(&entry.id).await);
        }
        if let Some(alt) = fallback_addr.clone().flatten() {
            if alt != primary {
                match self
                    .client
                    .download_file(&alt, &entry.id, index, dest_dir)
                    .await
                {
                    Ok(path) => {
                        tracing::info!("file served by peer {alt} instead of origin {primary}");
                        return Ok(path);
                    }
                    Err(second) => {
                        tracing::warn!("file download from peer {alt} failed: {second}");
                    }
                }
            }
        }

        // Preserve timeout, integrity, disk and authorization failures. They
        // are not evidence that the source file has been deleted.
        Err(first_error)
    }

    /// Address to fetch payloads from.
    fn origin_addr(&self, entry: &Entry) -> String {
        if entry.origin.addr.is_empty() {
            self.origin.addr.clone()
        } else {
            entry.origin.addr.clone()
        }
    }

    /// True when the entry was captured by this machine.
    fn is_local(&self, entry: &Entry) -> bool {
        entry.origin.host == self.origin.host
            || (!self.origin.node_id.is_empty() && entry.origin.node_id == self.origin.node_id)
    }

    // ---------------------------------------------------------------- lookup

    /// Find an entry locally first, then across online peers.
    pub async fn find(&self, id: &str) -> Result<Option<Entry>> {
        if let Some(entry) = self.store.get(id)? {
            return Ok(Some(entry));
        }
        Ok(self.peer_with_entry(id).await.map(|(_, entry)| entry))
    }

    /// Ask every online peer whether its store holds `id`; the first match
    /// wins. Returns the peer's address together with the entry.
    async fn peer_with_entry(&self, id: &str) -> Option<(String, Entry)> {
        let peers = crate::tailscale::peers(self.config.network.port)
            .await
            .unwrap_or_default();

        // Ask every online peer at once: done serially, one slow or offline
        // node would stall the whole lookup behind its connect timeout.
        let port = self.config.network.port;
        let id = id.to_string();
        let futs = peers.into_iter().filter(|p| p.online).map(|peer| {
            let client = &self.client;
            let id = id.clone();
            async move {
                let addr = peer.addr_with_port(port)?;
                let entries = client.list(&addr, 200).await.ok()?;
                let entry = entries.into_iter().find(|e| e.id == id)?;
                Some((addr, entry))
            }
        });

        let mut buffered = stream::iter(futs).buffer_unordered(8);
        while let Some(found) = buffered.next().await {
            if found.is_some() {
                return found;
            }
        }
        None
    }

    /// Address of an online peer whose store holds `id`. Download fallback
    /// for payloads whose recorded origin address no longer serves them.
    async fn peer_holding(&self, id: &str) -> Option<String> {
        self.peer_with_entry(id).await.map(|(addr, _)| addr)
    }

    /// Local history merged with every online peer's history.
    pub async fn list_all(&self, limit_per_peer: usize) -> Result<Vec<Entry>> {
        let local = self.store.list(limit_per_peer, None)?;
        self.client.aggregate(local, limit_per_peer).await
    }

    pub async fn peers(&self) -> Vec<Peer> {
        crate::tailscale::peers(self.config.network.port)
            .await
            .unwrap_or_default()
    }

    /// Capture only the explicitly selected, currently discovered device.
    /// Payloads remain on demand; this never writes the local clipboard.
    pub async fn fetch_clipboard(&self, node_id: &str) -> Result<Entry> {
        let peers = crate::tailscale::peers(self.config.network.port).await?;
        let matches: Vec<_> = peers.iter().filter(|p| p.node_id == node_id).collect();
        if node_id.is_empty() || matches.len() != 1 {
            return Err(Error::NotFound(
                "selected device not found or ambiguous".into(),
            ));
        }
        let peer = matches[0];
        if !peer.online {
            return Err(Error::PeerUnreachable {
                peer: peer.host.clone(),
            });
        }
        let addr = peer
            .addr_with_port(self.config.network.port)
            .ok_or_else(|| Error::PeerUnreachable {
                peer: peer.host.clone(),
            })?;
        let mut entry = self.client.capture_remote(&addr).await?;
        // The response came from this selected peer, not an arbitrary supplied URL.
        entry.origin.addr = addr;
        entry.origin.node_id = peer.node_id.clone();
        entry.origin.host = peer.host.clone();
        entry.holder = None;
        self.store.insert(&entry)?;
        Ok(entry)
    }

    // ------------------------------------------------------------------ apps

    /// Launch an app registered in this machine's `[[apps]]` config.
    ///
    /// Resolved against the config this process holds; the HTTP handler
    /// re-reads the file per request so `peercarry apps add` lands without a
    /// daemon restart. No path or argument ever arrives from the network.
    pub fn launch_app(&self, name: &str) -> Result<String> {
        let app = self.config.find_app(name).ok_or_else(|| {
            Error::NotFound(format!(
                "app {name:?} is not registered here - add it with `peercarry apps add`"
            ))
        })?;
        Self::spawn_app(app)
    }

    /// Start a registered app.
    ///
    /// The child outlives this process: spawned detached from our process
    /// group (Unix), with stdin closed.
    pub fn spawn_app(app: &AppEntry) -> Result<String> {
        let mut cmd = std::process::Command::new(&app.path);
        cmd.args(&app.args);
        cmd.stdin(std::process::Stdio::null());
        #[cfg(unix)]
        cmd.process_group(0);

        let child = cmd.spawn().map_err(|e| {
            Error::Io(std::io::Error::new(
                e.kind(),
                format!("cannot launch {}: {e}", app.path.display()),
            ))
        })?;
        let detail = format!("launched {} (pid {})", app.path.display(), child.id());
        tracing::info!("{detail}");
        Ok(detail)
    }

    // ----------------------------------------------------------------- push

    /// Pin (or unpin) an entry on this node.
    ///
    /// A pinned entry survives `clear` and history pruning. Remote entries
    /// are first copied into the local store - pinning is a promise that this
    /// machine keeps the payload, which only makes sense if the payload lives
    /// here.
    pub async fn pin(&self, id: &str, pinned: bool) -> Result<Option<Entry>> {
        if self.store.get(id)?.is_none() {
            match self.find(id).await? {
                Some(entry) => self.store.insert(&entry)?,
                None => return Ok(None),
            }
        }
        let updated = self.store.set_pinned(id, pinned)?;
        if let Some(entry) = &updated {
            tracing::info!(
                "{} entry {}",
                if pinned { "pinned" } else { "unpinned" },
                entry.id
            );
        }
        Ok(updated)
    }

    /// Push an entry into one peer's history.
    pub async fn send_to(&self, peer: &Peer, entry: &Entry) -> Result<()> {
        let Some(addr) = peer.addr_with_port(self.config.network.port) else {
            return Err(Error::PeerUnreachable {
                peer: peer.host.clone(),
            });
        };
        self.client.push(&addr, entry).await
    }

    /// Push an entry to every online peer. Returns per-peer outcomes.
    pub async fn broadcast(&self, entry: &Entry) -> Vec<(String, Result<()>)> {
        let peers = self.peers().await;
        let mut results = Vec::new();

        for peer in peers.into_iter().filter(|p| p.online) {
            let outcome = self.send_to(&peer, entry).await;
            results.push((peer.host.clone(), outcome));
        }
        results
    }
}

/// Decode a PNG buffer into clipboard-ready RGBA.
pub fn decode_png(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>)> {
    let img = image::load_from_memory(bytes)?.to_rgba8();
    let (width, height) = (img.width() as usize, img.height() as usize);
    Ok((width, height, img.into_raw()))
}
