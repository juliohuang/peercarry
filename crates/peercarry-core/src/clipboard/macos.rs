//! macOS pasteboard file-reference access.
//!
//! Only paths are exchanged. `NSFilenamesPboardType` carries an array of POSIX
//! paths and is understood by Finder and virtually every macOS app.

use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardType};
use objc2_foundation::{NSArray, NSString};

use crate::error::{Error, Result};

extern "C" {
    static NSFilenamesPboardType: Option<&'static NSPasteboardType>;
}

pub(super) fn read_file_paths() -> Result<Vec<PathBuf>> {
    unsafe {
        let Some(file_type) = NSFilenamesPboardType else {
            return Ok(Vec::new());
        };
        let pb = NSPasteboard::generalPasteboard();
        let Some(plist) = pb.propertyListForType(file_type) else {
            return Ok(Vec::new());
        };
        // `DowncastTarget` is only implemented for `NSArray<AnyObject>`, so the
        // elements are narrowed to `NSString` individually below.
        let Some(array) = (&*plist).downcast_ref::<NSArray<AnyObject>>() else {
            return Ok(Vec::new());
        };
        let count = array.count();
        let mut out = Vec::with_capacity(count.min(64));
        for i in 0..count {
            let item = array.objectAtIndex(i);
            if let Some(text) = (&*item).downcast_ref::<NSString>() {
                out.push(PathBuf::from(text.to_string()));
            }
        }
        Ok(out)
    }
}

pub(super) fn write_file_paths(paths: &[PathBuf]) -> Result<()> {
    unsafe {
        let Some(file_type) = NSFilenamesPboardType else {
            return Err(Error::Clipboard(
                "NSFilenamesPboardType unavailable".to_string(),
            ));
        };
        let pb = NSPasteboard::generalPasteboard();
        pb.clearContents();

        let strings: Vec<Retained<NSString>> = paths
            .iter()
            .map(|p| NSString::from_str(&p.to_string_lossy()))
            .collect();
        let array = NSArray::from_retained_slice(&strings);

        let ok: bool = objc2::msg_send![&pb, setPropertyList: &*array, forType: file_type];
        if !ok {
            return Err(Error::Clipboard(
                "failed to write file references to the pasteboard".to_string(),
            ));
        }
        Ok(())
    }
}
