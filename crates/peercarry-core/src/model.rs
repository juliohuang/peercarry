//! Core data model shared by the daemon, the CLI and the tray app.
//!
//! Every struct here is `serde` serialisable because it travels over HTTP
//! between peers. All optional fields are `#[serde(default)]` so that a newer
//! daemon can still read payloads produced by an older one.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Lowercase hex SHA-256 of some bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Stable identifier of a clipboard entry (UUIDv4 string).
pub type EntryId = String;

/// SHA-256 of a blob, hex encoded. Doubles as the on-disk file name.
pub type BlobHash = String;

/// What kind of payload a clipboard entry carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    /// Plain text (also used for HTML stripped down to text).
    Text,
    /// Raster image, stored as PNG.
    Image,
    /// One or more files/directories copied from a file manager.
    /// Only the references travel; bytes are streamed on demand.
    Files,
}

impl EntryKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EntryKind::Text => "text",
            EntryKind::Image => "image",
            EntryKind::Files => "files",
        }
    }
}

/// A single file inside a [`EntryKind::Files`] payload.
///
/// Note that `path` is only meaningful **on the machine that owns the entry**.
/// When another peer pulls it, the bytes come over HTTP and land in that peer's
/// download directory instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRef {
    /// Absolute path on the origin machine.
    pub path: String,
    /// File (or directory) name, used as the destination name on the receiver.
    pub name: String,
    /// Byte size of the file, or the recursive size for a directory.
    pub size: u64,
    #[serde(default)]
    pub is_dir: bool,
    /// SHA-256 once the bytes have been transferred and stored locally.
    #[serde(default)]
    pub hash: Option<String>,
}

/// Reference to a binary payload (currently only images).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlobRef {
    pub hash: BlobHash,
    pub size: u64,
    #[serde(default)]
    pub mime: Option<String>,
    /// Raw bytes for small payloads so a pull needs no extra round trip.
    /// Larger payloads set this to `None` and are fetched lazily.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline: Option<Vec<u8>>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

impl BlobRef {
    /// `true` when the bytes must be fetched from the origin machine.
    pub fn is_remote(&self) -> bool {
        self.inline.is_none()
    }
}

/// Identity of the machine an entry was captured on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Origin {
    /// Human readable host name.
    pub host: String,
    /// Tailscale node ID - stable across IP changes.
    pub node_id: String,
    /// Base URL used to fetch remote payloads, e.g. `http://100.64.0.1:5199`.
    pub addr: String,
    #[serde(default)]
    pub os: String,
}

/// One clipboard record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: EntryId,
    pub kind: EntryKind,
    pub origin: Origin,
    pub created_at: DateTime<Utc>,
    /// Set for [`EntryKind::Text`].
    #[serde(default)]
    pub text: Option<String>,
    /// Set for [`EntryKind::Image`].
    #[serde(default)]
    pub blob: Option<BlobRef>,
    /// Small preview of an image entry, stored as its own blob so peers can
    /// fetch it (by hash, through the usual blob route) without pulling the
    /// full image.
    #[serde(default)]
    pub thumb: Option<BlobRef>,
    /// Set for [`EntryKind::Files`].
    #[serde(default)]
    pub files: Option<Vec<FileRef>>,
    /// Single line summary used by the tray menu and `sc list`.
    #[serde(default)]
    pub preview: String,
    #[serde(default)]
    pub pinned: bool,
    /// Total payload size in bytes (text length, image size, or summed files).
    #[serde(default)]
    pub size: u64,
    /// Which node holds this entry, filled in by the merged tailnet view.
    ///
    /// Purely local bookkeeping: it is never serialized, so peers exchanging
    /// entries cannot spoof or clobber it. `None` means "this machine".
    #[serde(default, skip_serializing)]
    pub holder: Option<String>,
}

impl Entry {
    /// Case-insensitive substring match over everything a user could search
    /// for: the preview line, inline text, file names and paths, and the
    /// originating device. Empty query matches everything.
    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }
        let mut hay = String::with_capacity(256);
        hay.push_str(&self.preview);
        hay.push(' ');
        hay.push_str(&self.origin.host);
        if let Some(text) = &self.text {
            hay.push(' ');
            hay.push_str(text);
        }
        if let Some(files) = &self.files {
            for file in files {
                hay.push(' ');
                hay.push_str(&file.name);
                hay.push(' ');
                hay.push_str(&file.path);
            }
        }
        hay.to_lowercase().contains(&query)
    }

    /// Fingerprint of the content, used to spot repeats.
    ///
    /// Two entries with the same fingerprint hold the same bytes: identical
    /// text, the same image (blobs are content addressed, so the hash is
    /// the bytes), or the same set of files at the same sizes. When it was
    /// copied, and by whom, deliberately plays no part.
    pub fn fingerprint(&self) -> String {
        match self.kind {
            EntryKind::Text => match (&self.text, &self.blob) {
                (Some(text), _) => format!("t:{}", sha256_hex(text.as_bytes())),
                (None, Some(blob)) => format!("t:{}", blob.hash),
                // Old or unusual entries: fall back to the preview so they
                // can still be matched against each other.
                (None, None) => format!("t:{}", sha256_hex(self.preview.as_bytes())),
            },
            EntryKind::Image => match &self.blob {
                Some(blob) => format!("i:{}", blob.hash),
                None => format!("i:{}", sha256_hex(self.preview.as_bytes())),
            },
            EntryKind::Files => {
                let mut parts: Vec<String> = self
                    .files
                    .as_ref()
                    .map(|files| {
                        files
                            .iter()
                            .map(|f| format!("{}|{}", f.path, f.size))
                            .collect()
                    })
                    .unwrap_or_default();
                parts.sort();
                format!("f:{}", sha256_hex(parts.join("\n").as_bytes()))
            }
        }
    }

    /// Build the one-line preview shown in lists and tray menus.
    pub fn build_preview(&self) -> String {
        match self.kind {
            EntryKind::Text => {
                let raw = self.text.as_deref().unwrap_or_default();
                let flat: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
                const MAX: usize = 80;
                if flat.chars().count() > MAX {
                    format!("{}…", flat.chars().take(MAX).collect::<String>())
                } else {
                    flat
                }
            }
            EntryKind::Image => {
                let (w, h) = self
                    .blob
                    .as_ref()
                    .map(|b| (b.width, b.height))
                    .unwrap_or((None, None));
                match (w, h) {
                    (Some(w), Some(h)) => format!("<image {}x{}>", w, h),
                    _ => "<image>".to_string(),
                }
            }
            EntryKind::Files => {
                let files = self.files.as_deref().unwrap_or_default();
                match files.len() {
                    0 => "<files>".to_string(),
                    1 => format!("<file> {}", files[0].name),
                    n => format!("<{} files> {}", n, files[0].name),
                }
            }
        }
    }

    /// A short marker describing where the entry came from.
    pub fn source_label(&self) -> String {
        self.origin.host.clone()
    }
}

/// A node discovered through `tailscale status --json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Peer {
    pub node_id: String,
    pub host: String,
    pub dns_name: String,
    pub os: String,
    pub addresses: Vec<String>,
    #[serde(default)]
    pub online: bool,
    /// Resolved peercarry service URL, e.g. `http://100.64.0.1:5199`.
    /// Filled in by discovery; `None` when the node has no usable address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
}

impl Peer {
    /// `true` for desktop platforms that can run peercarry.
    pub fn is_desktop(&self) -> bool {
        matches!(self.os.as_str(), "macOS" | "windows" | "linux")
    }
}

/// Raw clipboard content as read from the platform clipboard.
#[derive(Debug, Clone)]
pub enum ClipboardPayload {
    Empty,
    Text(String),
    Image {
        width: usize,
        height: usize,
        /// RGBA8 pixel buffer, length = width * height * 4.
        rgba: Vec<u8>,
    },
    Files(Vec<FileRef>),
}

impl ClipboardPayload {
    pub fn kind(&self) -> Option<EntryKind> {
        match self {
            ClipboardPayload::Empty => None,
            ClipboardPayload::Text(_) => Some(EntryKind::Text),
            ClipboardPayload::Image { .. } => Some(EntryKind::Image),
            ClipboardPayload::Files(_) => Some(EntryKind::Files),
        }
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, ClipboardPayload::Empty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_entry(text: &str) -> Entry {
        Entry {
            id: "id".to_string(),
            kind: EntryKind::Text,
            origin: Origin {
                host: "x".into(),
                node_id: String::new(),
                addr: String::new(),
                os: "macos".into(),
            },
            created_at: Utc::now(),
            text: Some(text.to_string()),
            blob: None,
            thumb: None,
            files: None,
            preview: text.chars().take(80).collect(),
            pinned: false,
            size: text.len() as u64,
            holder: None,
        }
    }

    #[test]
    fn same_text_fingerprints_equal() {
        assert_eq!(
            text_entry("hello world").fingerprint(),
            text_entry("hello world").fingerprint()
        );
    }

    #[test]
    fn different_text_fingerprints_differ() {
        assert_ne!(
            text_entry("hello world").fingerprint(),
            text_entry("goodbye").fingerprint()
        );
    }

    /// A huge text, which lives in a blob, must still match a small text of
    /// the same content: both are "t:<sha256>".
    #[test]
    fn text_with_and_without_blob_share_fingerprint() {
        let big = "a".repeat(1024);
        let big_fp = text_entry(&big).fingerprint();
        // Pretend the same text has been moved to a blob.
        let mut via_blob = text_entry("");
        via_blob.text = None;
        via_blob.blob = Some(BlobRef {
            hash: crate::model::sha256_hex(big.as_bytes()),
            size: big.len() as u64,
            mime: Some("text/plain".into()),
            inline: None,
            width: None,
            height: None,
        });
        assert_eq!(big_fp, via_blob.fingerprint());
    }
}
