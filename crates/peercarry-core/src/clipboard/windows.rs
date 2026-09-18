//! Windows clipboard file-reference access via `CF_HDROP`.
//!
//! A `CF_HDROP` payload is a `DROPFILES` header followed by a double-null
//! terminated list of UTF-16 paths. Only the paths are stored, never the bytes.

use std::mem::size_of;
use std::path::PathBuf;

use windows::core::BOOL;
use windows::Win32::Foundation::{HANDLE, HGLOBAL, POINT};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::CF_HDROP;
use windows::Win32::UI::Shell::{DROPFILES, HDROP};

use crate::error::{Error, Result};

/// Encode a UTF-16 string as raw little-endian bytes, NUL terminated.
fn push_wide(bytes: &mut Vec<u8>, text: &str) {
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes.extend_from_slice(&0u16.to_le_bytes());
}

pub(super) fn read_file_paths() -> Result<Vec<PathBuf>> {
    unsafe {
        OpenClipboard(None).map_err(|e| Error::Clipboard(format!("OpenClipboard: {e}")))?;

        let handle = match GetClipboardData(CF_HDROP.0 as u32) {
            Ok(h) => h,
            Err(_) => {
                let _ = CloseClipboard();
                return Ok(Vec::new());
            }
        };

        if handle.is_invalid() {
            let _ = CloseClipboard();
            return Ok(Vec::new());
        }

        let hglobal = HGLOBAL(handle.0);
        let _lock = GlobalLock(hglobal);
        let hdrop = HDROP(handle.0);
        let count = windows::Win32::UI::Shell::DragQueryFileW(hdrop, 0xFFFF_FFFF, None);

        let mut out = Vec::with_capacity(count.min(64) as usize);
        for i in 0..count {
            // First call with a null buffer returns the required length.
            let len = windows::Win32::UI::Shell::DragQueryFileW(hdrop, i, None) as usize;
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len + 1];
            let copied =
                windows::Win32::UI::Shell::DragQueryFileW(hdrop, i, Some(buf.as_mut_slice()));
            buf.truncate(copied as usize);
            out.push(PathBuf::from(String::from_utf16_lossy(&buf)));
        }

        let _ = GlobalUnlock(hglobal);
        let _ = CloseClipboard();
        Ok(out)
    }
}

pub(super) fn write_file_paths(paths: &[PathBuf]) -> Result<()> {
    unsafe {
        let header = DROPFILES {
            pFiles: size_of::<DROPFILES>() as u32,
            pt: POINT { x: 0, y: 0 },
            fNC: BOOL(0),
            fWide: BOOL(1),
        };

        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend_from_slice(std::slice::from_raw_parts(
            &header as *const DROPFILES as *const u8,
            size_of::<DROPFILES>(),
        ));
        for path in paths {
            push_wide(&mut bytes, &path.to_string_lossy());
        }
        // Terminating NUL: the header promises a double-null terminated list.
        bytes.extend_from_slice(&0u16.to_le_bytes());

        OpenClipboard(None).map_err(|e| Error::Clipboard(format!("OpenClipboard: {e}")))?;
        let result = (|| -> Result<()> {
            EmptyClipboard().map_err(|e| Error::Clipboard(format!("EmptyClipboard: {e}")))?;

            let mem = GlobalAlloc(GMEM_MOVEABLE, bytes.len())
                .map_err(|e| Error::Clipboard(format!("GlobalAlloc: {e}")))?;
            let dst = GlobalLock(mem);
            if dst.is_null() {
                return Err(Error::Clipboard("GlobalLock failed".to_string()));
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst as *mut u8, bytes.len());
            let _ = GlobalUnlock(mem);

            // Ownership transfers to the clipboard on success; do not free.
            if SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(mem.0))).is_err() {
                return Err(Error::Clipboard("SetClipboardData failed".to_string()));
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}
