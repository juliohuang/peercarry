//! Platform specific directories.
//!
//! Layout (identical on all three desktops):
//!
//! ```text
//! <data_dir>/config.toml      user editable configuration
//! <data_dir>/state.redb       entry metadata
//! <data_dir>/blobs/<sha256>   payload bytes
//! <download_dir>/             where pulled files land
//! ```

use std::path::PathBuf;

/// Root directory for configuration and state.
///
/// * macOS   - `~/Library/Application Support/peercarry`
/// * Linux   - `$XDG_DATA_HOME/peercarry` (fallback `~/.local/share/peercarry`)
/// * Windows - `%APPDATA%\peercarry`
pub fn data_dir() -> PathBuf {
    if let Ok(dir) =
        std::env::var("PEERCARRY_DATA_DIR").or_else(|_| std::env::var("SYNC_CLIP_DATA_DIR"))
    {
        return PathBuf::from(dir);
    }
    compatible_dir(dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")))
}

/// Where files pulled from a remote peer are written.
pub fn download_dir() -> PathBuf {
    compatible_dir(
        dirs::download_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from(".")),
    )
}

// Reuse existing installations without moving live databases or breaking old hooks.
fn compatible_dir(base: PathBuf) -> PathBuf {
    let legacy = base.join("sync-clip");
    if legacy.is_dir() {
        legacy
    } else {
        base.join("peercarry")
    }
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.toml")
}

pub fn db_path() -> PathBuf {
    data_dir().join("state.redb")
}

pub fn blobs_dir() -> PathBuf {
    data_dir().join("blobs")
}

/// On-disk location of a blob given its SHA-256.
pub fn blob_path(hash: &str) -> PathBuf {
    // Two level fan-out keeps directories small when history grows.
    let (prefix, rest) = hash.split_at(2.min(hash.len()));
    blobs_dir().join(prefix).join(rest)
}

/// Ensure every directory we write to exists.
pub fn ensure_layout() -> anyhow::Result<()> {
    let dirs = [data_dir(), blobs_dir(), download_dir()];
    for dir in dirs {
        std::fs::create_dir_all(&dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn legacy_directory_is_reused_without_moving_data() {
        let root = std::env::temp_dir().join(format!("peercarry-paths-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(compatible_dir(root.clone()), root.join("peercarry"));
        std::fs::create_dir(root.join("sync-clip")).unwrap();
        assert_eq!(compatible_dir(root.clone()), root.join("sync-clip"));
        std::fs::create_dir(root.join("peercarry")).unwrap();
        assert_eq!(compatible_dir(root.clone()), root.join("sync-clip"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fan_out_is_two_levels() {
        let p = blob_path("abcdef123");
        // Component comparison, not string matching: the separator differs
        // between Windows and the Unixes.
        assert!(p.ends_with(Path::new("blobs/ab/cdef123")));
    }
}
