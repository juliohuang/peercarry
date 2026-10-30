//! Immutable encrypted checkpoints. Callers retain the chosen revision instead
//! of selecting snapshots by untrusted filesystem timestamps.
use crate::{Error, Result, Vault};
use ring::digest;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
};

pub fn save(directory: &Path, vault: &Vault) -> Result<String> {
    fs::create_dir_all(directory).map_err(|_| Error::Storage)?;
    let data = vault.encode()?;
    let id = hex::encode(digest::digest(&digest::SHA256, &data).as_ref());
    let destination = directory.join(format!("{id}.pcvault"));
    if destination.exists() {
        let existing = fs::read(&destination).map_err(|_| Error::Storage)?;
        return if existing == data {
            Ok(id)
        } else {
            Err(Error::Storage)
        };
    }
    let temporary = directory.join(format!("{}.partial", hex::encode(crate::random::<16>()?)));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(|_| Error::Storage)?;
        file.write_all(&data).map_err(|_| Error::Storage)?;
        file.sync_all().map_err(|_| Error::Storage)?;
        drop(file);
        fs::rename(&temporary, &destination).map_err(|_| Error::Storage)?;
        Ok(id)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn load(directory: &Path, revision: &str, password: &[u8]) -> Result<Vault> {
    if revision.len() != 64 || !revision.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Invalid);
    }
    let file = fs::File::open(directory.join(format!("{revision}.pcvault")))
        .map_err(|_| Error::Storage)?;
    let mut bytes = Vec::new();
    file.take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Storage)?;
    if hex::encode(digest::digest(&digest::SHA256, &bytes).as_ref()) != revision {
        return Err(Error::Authentication);
    }
    Vault::decode(&bytes, password)
}
