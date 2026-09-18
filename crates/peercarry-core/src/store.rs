//! Local entry store.
//!
//! Metadata lives in redb (atomic, no C dependency); payload bytes live in
//! `<data_dir>/blobs/<2-char prefix>/<rest>` so that large images or pulled
//! files never bloat the database.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use redb::{Database, ReadableTable, TableDefinition};

use crate::error::{Error, Result};
use crate::model::{BlobHash, Entry, EntryId};
use crate::paths;

/// `id -> JSON(Entry)`
const ENTRIES: TableDefinition<&str, &[u8]> = TableDefinition::new("entries");

pub struct Store {
    /// redb allows a single write transaction at a time; the mutex serialises
    /// callers instead of failing them.
    db: Mutex<Database>,
    blobs_dir: PathBuf,
}

impl Store {
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(paths::blobs_dir())?;

        let db = Database::create(db_path)?;
        {
            // Touch the table once so it exists for later readers.
            let txn = db.begin_write()?;
            txn.open_table(ENTRIES)?;
            txn.commit()?;
        }

        Ok(Self {
            db: Mutex::new(db),
            blobs_dir: paths::blobs_dir(),
        })
    }

    /// Open the store at the default platform location.
    pub fn open_default() -> Result<Self> {
        paths::ensure_layout()?;
        Self::open(&paths::db_path())
    }

    // ---------------------------------------------------------------- entries

    pub fn insert(&self, entry: &Entry) -> Result<()> {
        let bytes = serde_json::to_vec(entry)?;
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(ENTRIES)?;
            table.insert(entry.id.as_str(), bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<Entry>> {
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_read()?;
        let table = txn.open_table(ENTRIES)?;
        match table.get(id)? {
            Some(guard) => Ok(Some(serde_json::from_slice(guard.value())?)),
            None => Ok(None),
        }
    }

    /// Newest first, optionally filtered by kind.
    pub fn list(&self, limit: usize, kind: Option<crate::model::EntryKind>) -> Result<Vec<Entry>> {
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_read()?;
        let table = txn.open_table(ENTRIES)?;

        let mut entries: Vec<Entry> = Vec::new();
        for item in table.iter()? {
            let (_key, value) = item?;
            match serde_json::from_slice::<Entry>(value.value()) {
                Ok(entry) => entries.push(entry),
                Err(e) => tracing::warn!("skipping corrupt entry: {e}"),
            }
        }
        drop(table);
        drop(txn);
        drop(db);

        if let Some(kind) = kind {
            entries.retain(|e| e.kind == kind);
        }
        entries.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        entries.truncate(limit);
        Ok(entries)
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_write()?;
        let removed = {
            let mut table = txn.open_table(ENTRIES)?;
            // Bind to a local first: the temporary guard from `remove` would
            // otherwise outlive `table` and fail borrowck.
            let existed = table.remove(id)?.is_some();
            existed
        };
        txn.commit()?;
        Ok(removed)
    }

    /// Flip the pinned flag of an entry. Returns the updated entry, or `None`
    /// when the id is unknown locally.
    pub fn set_pinned(&self, id: &str, pinned: bool) -> Result<Option<Entry>> {
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_write()?;
        let updated = {
            let mut table = txn.open_table(ENTRIES)?;
            // Copy the entry out before mutating: the guard returned by `get`
            // borrows the table and would conflict with the write below.
            let existing: Option<Entry> = match table.get(id)? {
                Some(guard) => Some(serde_json::from_slice(guard.value())?),
                None => None,
            };
            match existing {
                Some(mut entry) => {
                    entry.pinned = pinned;
                    let bytes = serde_json::to_vec(&entry)?;
                    table.insert(id, bytes.as_slice())?;
                    Some(entry)
                }
                None => None,
            }
        };
        txn.commit()?;
        Ok(updated)
    }

    pub fn clear(&self) -> Result<usize> {
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_write()?;
        let mut removed = 0usize;
        {
            let mut table = txn.open_table(ENTRIES)?;
            let ids: Vec<String> = table
                .iter()?
                .flatten()
                .filter_map(|(_, v)| serde_json::from_slice::<Entry>(v.value()).ok())
                .filter(|e| !e.pinned)
                .map(|e| e.id)
                .collect();
            for id in ids {
                if table.remove(id.as_str())?.is_some() {
                    removed += 1;
                }
            }
        }
        txn.commit()?;
        Ok(removed)
    }

    /// Drop the oldest unpinned entries until at most `keep` remain.
    pub fn prune(&self, keep: usize) -> Result<usize> {
        let all = self.list(usize::MAX, None)?;
        let mut removable: Vec<EntryId> = all
            .iter()
            .filter(|e| !e.pinned)
            .skip(keep)
            .map(|e| e.id.clone())
            .collect();
        if removable.is_empty() {
            return Ok(0);
        }

        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_write()?;
        let mut removed = 0usize;
        {
            let mut table = txn.open_table(ENTRIES)?;
            for id in removable.drain(..) {
                if table.remove(id.as_str())?.is_some() {
                    removed += 1;
                }
            }
        }
        txn.commit()?;
        Ok(removed)
    }

    // ------------------------------------------------------------------ blobs

    fn blob_path(&self, hash: &BlobHash) -> PathBuf {
        let (prefix, rest) = hash.split_at(2.min(hash.len()));
        self.blobs_dir.join(prefix).join(rest)
    }

    /// Persist blob bytes. Idempotent: returns early if the blob already exists.
    pub fn write_blob(&self, hash: &BlobHash, bytes: &[u8]) -> Result<PathBuf> {
        let path = self.blob_path(hash);
        if path.exists() {
            return Ok(path);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Write to a temp file then rename, so a crash never leaves a partial
        // blob that later validates as complete.
        let tmp = path.with_extension("tmp");
        {
            let mut file = File::create(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    pub fn has_blob(&self, hash: &BlobHash) -> bool {
        self.blob_path(hash).exists()
    }

    pub fn read_blob(&self, hash: &BlobHash) -> Result<Option<Vec<u8>>> {
        let path = self.blob_path(hash);
        if !path.exists() {
            return Ok(None);
        }
        let mut file = File::open(&path)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        Ok(Some(buf))
    }

    /// Open a blob for streaming, avoiding a full read into memory.
    pub fn open_blob(&self, hash: &BlobHash) -> Result<Option<File>> {
        let path = self.blob_path(hash);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(File::open(&path)?))
    }
}
