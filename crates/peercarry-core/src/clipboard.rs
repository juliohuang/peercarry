//! Clipboard access.
//!
//! Three payload classes are supported:
//!
//! | kind  | read                              | write                          |
//! |-------|-----------------------------------|--------------------------------|
//! | Text  | `arboard`                         | `arboard`                      |
//! | Image | `arboard` (RGBA)                  | `arboard` (RGBA)               |
//! | Files | platform native (paths only)      | platform native (paths only)   |
//!
//! Files never have their bytes touched while sitting in the clipboard - only
//! the path list is captured, so copying a 20 GB folder is instant.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::model::{ClipboardPayload, FileRef};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use self::windows as platform;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use self::linux as platform;

/// Recursive size of a path, following symlinks one level.
fn path_size(path: &Path) -> u64 {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(t) if t.is_dir() => stack.push(entry.path()),
                Ok(_) => total += entry.metadata().map(|m| m.len()).unwrap_or(0),
                Err(_) => {}
            }
        }
    }
    total
}

/// Turn a list of absolute paths into [`FileRef`]s with sizes filled in.
pub fn describe_paths(paths: &[PathBuf]) -> Vec<FileRef> {
    paths
        .iter()
        .map(|p| FileRef {
            path: p.to_string_lossy().to_string(),
            name: p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| p.to_string_lossy().to_string()),
            size: path_size(p),
            is_dir: p.is_dir(),
            hash: None,
        })
        .collect()
}

/// Read the system clipboard.
///
/// Files are probed first because a file copy also exposes a plain text
/// fallback that would otherwise shadow the more specific payload.
pub fn read() -> Result<ClipboardPayload> {
    match platform::read_file_paths() {
        Ok(paths) if !paths.is_empty() => {
            return Ok(ClipboardPayload::Files(describe_paths(&paths)))
        }
        Ok(_) => {}
        Err(e) => tracing::debug!("file clipboard probe failed: {e}"),
    }

    let mut board = arboard::Clipboard::new().map_err(|e| Error::Clipboard(e.to_string()))?;

    if let Ok(img) = board.get_image() {
        return Ok(ClipboardPayload::Image {
            width: img.width,
            height: img.height,
            rgba: img.bytes.into_owned(),
        });
    }

    if let Ok(text) = board.get_text() {
        if !text.trim().is_empty() {
            return Ok(ClipboardPayload::Text(text));
        }
    }

    Ok(ClipboardPayload::Empty)
}

/// Write plain text to the system clipboard.
pub fn write_text(text: &str) -> Result<()> {
    let mut board = arboard::Clipboard::new().map_err(|e| Error::Clipboard(e.to_string()))?;
    board
        .set_text(text.to_string())
        .map_err(|e| Error::Clipboard(e.to_string()))
}

/// Write an RGBA image to the system clipboard.
pub fn write_image(width: usize, height: usize, rgba: &[u8]) -> Result<()> {
    let mut board = arboard::Clipboard::new().map_err(|e| Error::Clipboard(e.to_string()))?;
    let data = arboard::ImageData {
        width,
        height,
        bytes: std::borrow::Cow::Borrowed(rgba),
    };
    board
        .set_image(data)
        .map_err(|e| Error::Clipboard(e.to_string()))
}

/// Write a file/directory reference list to the system clipboard.
///
/// Only the paths are placed on the clipboard; the bytes stay where they are
/// until the receiving peer actually pulls them.
pub fn write_files(paths: &[PathBuf]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    platform::write_file_paths(paths)
}

/// True when the current session is Wayland rather than X11.
#[cfg(target_os = "linux")]
pub(crate) fn is_wayland() -> bool {
    std::env::var("WAYLAND_DISPLAY")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}
