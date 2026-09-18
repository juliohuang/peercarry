//! Linux clipboard file-reference access via the `text/uri-list` target.
//!
//! `arboard` has no API for custom MIME types, so we shell out to `wl-copy` /
//! `wl-paste` on Wayland and to `xclip` on X11. This is the same approach
//! taken by most CLI clipboard tools on Linux.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::clipboard::is_wayland;
use crate::error::{Error, Result};

/// Percent-encode everything that is not unreserved or a path separator.
fn encode_path(path: &Path) -> String {
    let mut out = String::with_capacity(path.as_os_str().len() + 32);
    for byte in path.to_string_lossy().as_bytes() {
        let keep = byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~');
        if keep {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Inverse of [`encode_path`].
fn decode_path(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Run a command, optionally feeding `input` on stdin, and capture stdout.
fn run(program: &str, args: &[&str], input: Option<&str>) -> Result<String> {
    use std::io::Write;

    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = cmd
        .spawn()
        .map_err(|e| Error::Clipboard(format!("cannot run {program}: {e}")))?;

    if let Some(text) = input {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
    }

    let out = child
        .wait_with_output()
        .map_err(|e| Error::Clipboard(format!("{program} failed: {e}")))?;

    if !out.status.success() {
        return Err(Error::Clipboard(format!(
            "{program} exited with {}",
            out.status
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub(super) fn read_file_paths() -> Result<Vec<PathBuf>> {
    let raw = if is_wayland() {
        run("wl-paste", &["--type", "text/uri-list"], None)
    } else {
        run(
            "xclip",
            &["-selection", "clipboard", "-t", "text/uri-list", "-o"],
            None,
        )
    }?;

    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let path = match line.strip_prefix("file://") {
            Some(rest) => decode_path(rest),
            None => decode_path(line),
        };
        if !path.is_empty() {
            out.push(PathBuf::from(path));
        }
    }
    Ok(out)
}

pub(super) fn write_file_paths(paths: &[PathBuf]) -> Result<()> {
    let mut list = String::new();
    for path in paths {
        list.push_str(&format!("file://{}\r\n", encode_path(path)));
    }

    if is_wayland() {
        run("wl-copy", &["--type", "text/uri-list"], Some(&list))?;
    } else {
        run(
            "xclip",
            &["-selection", "clipboard", "-t", "text/uri-list", "-i"],
            Some(&list),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_spaces_and_unicode() {
        let original = "/tmp/sync clip 测试/文件 (1).txt";
        let encoded = encode_path(Path::new(original));
        assert!(!encoded.contains(' '));
        assert_eq!(decode_path(&encoded), original);
    }
}
