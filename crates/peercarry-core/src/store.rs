//! Local entry store.
//!
//! Metadata lives in redb (atomic, no C dependency); payload bytes live in
//! `<data_dir>/blobs/<2-char prefix>/<rest>` so that large images or pulled
//! files never bloat the database.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::Utc;
use redb::{Database, ReadableTable, TableDefinition};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::error::{Error, Result};
use crate::model::{BlobHash, Entry, EntryId};
use crate::notebook::{NotebookItem, MAX_NOTEBOOK_ITEMS, MAX_NOTEBOOK_TOTAL_BYTES};
use crate::notebook_sync::{
    self, NoteSyncMetadata, NotebookSnapshot, PeerProgress, PeerSyncState, PersistedSyncState,
    SyncSettings, SyncVersion, MAX_SYNC_CONTENT_BYTES, MAX_SYNC_PERSISTED_BYTES,
};
use crate::paths;

/// `id -> JSON(Entry)`
const ENTRIES: TableDefinition<&str, &[u8]> = TableDefinition::new("entries");
/// `id -> JSON(NotebookItem)`; kept independent from clipboard history.
const NOTEBOOKS: TableDefinition<&str, &[u8]> = TableDefinition::new("notebook_items");
/// Global sync settings and per-note causal heads; independent from clipboard history.
const NOTEBOOK_SYNC: TableDefinition<&str, &[u8]> = TableDefinition::new("notebook_sync_meta");
const SYNC_GLOBAL_KEY: &str = "global";
const SYNC_NOTE_PREFIX: &str = "note:";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotebookWriteOutcome {
    Stored,
    NotFound,
    Conflict {
        current_revision: u64,
    },
    SyncConflict {
        current_revision: u64,
        conflict_count: usize,
    },
    QuotaExceeded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotebookResolveOutcome {
    Resolved,
    NotFound,
    Conflict { current_revision: u64 },
    VersionNotFound,
    NotConflicted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncApplyOutcome {
    Applied,
    SettingsRevoked,
    CursorChanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NotebookPublishOutcome {
    Published { entry_id: String },
    NotFound,
    Trashed,
    Conflict { current_revision: u64 },
}

pub struct Store {
    /// redb allows a single write transaction at a time; the mutex serialises
    /// callers instead of failing them.
    db: Mutex<Database>,
    blobs_dir: PathBuf,
    sync_notify: Arc<Notify>,
}

impl Store {
    pub fn open(db_path: &Path) -> Result<Self> {
        Self::open_with_blob_dir(db_path, paths::blobs_dir())
    }

    #[cfg(test)]
    pub(crate) fn open_isolated_for_tests(db_path: &Path) -> Result<Self> {
        let blobs_dir = db_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("blobs");
        Self::open_with_blob_dir(db_path, blobs_dir)
    }

    fn open_with_blob_dir(db_path: &Path, blobs_dir: PathBuf) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(&blobs_dir)?;

        let db = Database::create(db_path)?;
        {
            // Touch the table once so it exists for later readers.
            let txn = db.begin_write()?;
            txn.open_table(ENTRIES)?;
            txn.open_table(NOTEBOOKS)?;
            txn.open_table(NOTEBOOK_SYNC)?;
            txn.commit()?;
        }

        let store = Self {
            db: Mutex::new(db),
            blobs_dir,
            sync_notify: Arc::new(Notify::new()),
        };
        store.initialize_notebook_sync()?;
        Ok(store)
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

    // ----------------------------------------------------------- notebooks

    #[cfg(test)]
    pub(crate) fn get_notebook_raw(&self, id: &str) -> Result<Option<Vec<u8>>> {
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_read()?;
        let table = txn.open_table(NOTEBOOKS)?;
        Ok(table.get(id)?.map(|guard| guard.value().to_vec()))
    }

    /// Store one notebook record with revision CAS and quota checks in the
    /// same redb write transaction. `expected_revision=None` means create.
    #[cfg(test)]
    pub(crate) fn save_notebook_raw(
        &self,
        id: &str,
        value: &[u8],
        expected_revision: Option<u64>,
        max_items: usize,
        max_total_bytes: usize,
    ) -> Result<NotebookWriteOutcome> {
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_write()?;
        let outcome = {
            let mut table = txn.open_table(NOTEBOOKS)?;
            let existing = table.get(id)?.map(|guard| guard.value().to_vec());
            match (&existing, expected_revision) {
                (Some(bytes), None) => {
                    return Ok(NotebookWriteOutcome::Conflict {
                        current_revision: notebook_revision(bytes)?,
                    });
                }
                (None, Some(_)) => return Ok(NotebookWriteOutcome::NotFound),
                (Some(bytes), Some(expected)) => {
                    let current_revision = notebook_revision(bytes)?;
                    if current_revision != expected {
                        return Ok(NotebookWriteOutcome::Conflict { current_revision });
                    }
                }
                (None, None) => {}
            }

            let mut count = 0usize;
            let mut total_bytes = 0usize;
            for item in table.iter()? {
                let (_, value) = item?;
                count = count.saturating_add(1);
                total_bytes = total_bytes.saturating_add(value.value().len());
            }
            let new_count = count.saturating_add(usize::from(existing.is_none()));
            let replaced_bytes = existing.as_ref().map_or(0, Vec::len);
            let new_total_bytes = total_bytes
                .saturating_sub(replaced_bytes)
                .saturating_add(value.len());
            if new_count > max_items || new_total_bytes > max_total_bytes {
                return Ok(NotebookWriteOutcome::QuotaExceeded);
            }

            table.insert(id, value)?;
            NotebookWriteOutcome::Stored
        };
        if outcome == NotebookWriteOutcome::Stored {
            txn.commit()?;
        }
        Ok(outcome)
    }

    /// CAS-check a notebook snapshot and insert its explicitly published text
    /// entry atomically. The caller builds the entry from the stored snapshot.
    pub(crate) fn publish_notebook_entry<F>(
        &self,
        id: &str,
        expected_revision: u64,
        build_entry: F,
    ) -> Result<NotebookPublishOutcome>
    where
        F: FnOnce(&[u8]) -> Result<Entry>,
    {
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_write()?;
        let notebook = {
            let table = txn.open_table(NOTEBOOKS)?;
            let Some(guard) = table.get(id)? else {
                return Ok(NotebookPublishOutcome::NotFound);
            };
            guard.value().to_vec()
        };
        let current_revision = notebook_revision(&notebook)?;
        if current_revision != expected_revision {
            return Ok(NotebookPublishOutcome::Conflict { current_revision });
        }
        if notebook_trashed(&notebook)? {
            return Ok(NotebookPublishOutcome::Trashed);
        }

        let entry = build_entry(&notebook)?;
        let entry_id = entry.id.clone();
        let entry_bytes = serde_json::to_vec(&entry)?;
        {
            let mut entries = txn.open_table(ENTRIES)?;
            entries.insert(entry.id.as_str(), entry_bytes.as_slice())?;
        }
        txn.commit()?;
        Ok(NotebookPublishOutcome::Published { entry_id })
    }

    /// Upgrade existing notebook rows with their first local causal version.
    /// The note bytes, id, timestamps, and record-level revision are untouched.
    fn initialize_notebook_sync(&self) -> Result<()> {
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_write()?;
        let note_rows = {
            let notes = txn.open_table(NOTEBOOKS)?;
            let mut rows = Vec::new();
            for entry in notes.iter()? {
                let (key, value) = entry?;
                rows.push((key.value().to_owned(), value.value().to_vec()));
            }
            rows
        };
        let mut sync = txn.open_table(NOTEBOOK_SYNC)?;
        let global = match sync.get(SYNC_GLOBAL_KEY)? {
            Some(value) => serde_json::from_slice::<PersistedSyncState>(value.value())?,
            None => PersistedSyncState {
                replica_id: Uuid::new_v4().hyphenated().to_string(),
                settings_generation: 0,
                settings: SyncSettings::default(),
                peers: Default::default(),
            },
        };
        if Uuid::parse_str(&global.replica_id)
            .ok()
            .map(|uuid| uuid.hyphenated().to_string())
            .as_deref()
            != Some(global.replica_id.as_str())
        {
            return Err(Error::Storage("invalid notebook replica id".into()));
        }
        for (id, bytes) in note_rows {
            let key = sync_note_key(&id);
            if sync.get(key.as_str())?.is_some() {
                continue;
            }
            let item: NotebookItem = serde_json::from_slice(&bytes)?;
            if item.id != id {
                return Err(Error::Storage("invalid notebook key".into()));
            }
            let version = notebook_sync::initial_version(&item, &global.replica_id)?;
            let metadata = notebook_sync::metadata_from_heads(&[version])?;
            let value = serde_json::to_vec(&metadata)?;
            sync.insert(key.as_str(), value.as_slice())?;
        }
        let global_bytes = serde_json::to_vec(&global)?;
        sync.insert(SYNC_GLOBAL_KEY, global_bytes.as_slice())?;
        drop(sync);
        check_notebook_sync_quotas(&txn, MAX_NOTEBOOK_ITEMS, MAX_NOTEBOOK_TOTAL_BYTES)?;
        txn.commit()?;
        Ok(())
    }

    pub(crate) fn sync_notifier(&self) -> Arc<Notify> {
        self.sync_notify.clone()
    }

    pub(crate) fn sync_state(&self) -> Result<PersistedSyncState> {
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_read()?;
        let sync = txn.open_table(NOTEBOOK_SYNC)?;
        read_sync_global(&sync)
    }

    pub(crate) fn sync_settings(&self) -> Result<SyncSettings> {
        Ok(self.sync_state()?.settings)
    }

    pub(crate) fn update_sync_settings(&self, settings: SyncSettings) -> Result<()> {
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_write()?;
        let mut sync = txn.open_table(NOTEBOOK_SYNC)?;
        let mut global = read_sync_global(&sync)?;
        if global.settings != settings {
            global.settings_generation =
                global.settings_generation.checked_add(1).ok_or_else(|| {
                    Error::Storage("notebook sync settings generation exhausted".into())
                })?;
        }
        let old_peers = std::mem::take(&mut global.peers);
        let mut peers = BTreeMap::new();
        for node_id in &settings.peer_ids {
            let mut progress = old_peers.get(node_id).cloned().unwrap_or_default();
            if !settings.enabled {
                progress.state = PeerSyncState::Disabled;
                progress.error = None;
            }
            peers.insert(node_id.clone(), progress);
        }
        global.settings = settings;
        global.peers = peers;
        let bytes = serde_json::to_vec(&global)?;
        sync.insert(SYNC_GLOBAL_KEY, bytes.as_slice())?;
        drop(sync);
        check_notebook_sync_quotas(&txn, MAX_NOTEBOOK_ITEMS, MAX_NOTEBOOK_TOTAL_BYTES)?;
        txn.commit()?;
        self.sync_notify.notify_one();
        Ok(())
    }

    pub(crate) fn get_notebook_snapshot(&self, id: &str) -> Result<Option<NotebookSnapshot>> {
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_read()?;
        let notes = txn.open_table(NOTEBOOKS)?;
        let Some(bytes) = notes.get(id)? else {
            return Ok(None);
        };
        let item: NotebookItem = serde_json::from_slice(bytes.value())?;
        if item.id != id {
            return Err(Error::Storage("invalid notebook key".into()));
        }
        let sync = txn.open_table(NOTEBOOK_SYNC)?;
        let metadata = read_note_sync_metadata(&sync, id)?
            .ok_or_else(|| Error::Storage("missing notebook sync metadata".into()))?;
        let heads = notebook_sync::materialize_heads(&item, &metadata)?;
        Ok(Some(snapshot_from_heads(item, heads)?))
    }

    pub(crate) fn list_notebook_snapshots(&self) -> Result<Vec<NotebookSnapshot>> {
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_read()?;
        let rows = {
            let notes = txn.open_table(NOTEBOOKS)?;
            let mut rows = Vec::new();
            for entry in notes.iter()? {
                let (key, value) = entry?;
                rows.push((key.value().to_owned(), value.value().to_vec()));
            }
            rows
        };
        let sync = txn.open_table(NOTEBOOK_SYNC)?;
        let mut snapshots = Vec::with_capacity(rows.len());
        for (id, bytes) in rows {
            let item: NotebookItem = serde_json::from_slice(&bytes)?;
            if item.id != id {
                return Err(Error::Storage("invalid notebook key".into()));
            }
            let metadata = read_note_sync_metadata(&sync, &id)?
                .ok_or_else(|| Error::Storage("missing notebook sync metadata".into()))?;
            let heads = notebook_sync::materialize_heads(&item, &metadata)?;
            snapshots.push(snapshot_from_heads(item, heads)?);
        }
        Ok(snapshots)
    }

    pub(crate) fn create_notebook_local(
        &self,
        item: &NotebookItem,
        max_items: usize,
        max_total_bytes: usize,
    ) -> Result<NotebookWriteOutcome> {
        self.save_notebook_local(item, None, max_items, max_total_bytes)
    }

    pub(crate) fn update_notebook_local(
        &self,
        item: &NotebookItem,
        expected_revision: u64,
        max_items: usize,
        max_total_bytes: usize,
    ) -> Result<NotebookWriteOutcome> {
        self.save_notebook_local(item, Some(expected_revision), max_items, max_total_bytes)
    }

    fn save_notebook_local(
        &self,
        item: &NotebookItem,
        expected_revision: Option<u64>,
        max_items: usize,
        max_total_bytes: usize,
    ) -> Result<NotebookWriteOutcome> {
        let note_bytes = serde_json::to_vec(item)?;
        if note_bytes.len() > 256 * 1024 {
            return Ok(NotebookWriteOutcome::QuotaExceeded);
        }
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_write()?;
        let mut global;
        let current_bytes = {
            let sync = txn.open_table(NOTEBOOK_SYNC)?;
            global = read_sync_global(&sync)?;
            let notes = txn.open_table(NOTEBOOKS)?;
            let current = notes
                .get(item.id.as_str())?
                .map(|value| value.value().to_vec());
            current
        };
        let current = current_bytes
            .as_deref()
            .map(serde_json::from_slice::<NotebookItem>)
            .transpose()?;
        match (current.as_ref(), expected_revision) {
            (Some(existing), None) => {
                return Ok(NotebookWriteOutcome::Conflict {
                    current_revision: existing.revision,
                });
            }
            (None, Some(_)) => return Ok(NotebookWriteOutcome::NotFound),
            (Some(existing), Some(_)) => {
                let sync = txn.open_table(NOTEBOOK_SYNC)?;
                let metadata = read_note_sync_metadata(&sync, &item.id)?
                    .ok_or_else(|| Error::Storage("missing notebook sync metadata".into()))?;
                if metadata.heads.len() > 1 {
                    return Ok(NotebookWriteOutcome::SyncConflict {
                        current_revision: existing.revision,
                        conflict_count: metadata.heads.len() - 1,
                    });
                }
                if Some(existing.revision) != expected_revision {
                    return Ok(NotebookWriteOutcome::Conflict {
                        current_revision: existing.revision,
                    });
                }
                if item.kind != existing.kind || item.created_at != existing.created_at {
                    return Err(Error::Storage("invalid local notebook mutation".into()));
                }
            }
            (None, None) => {}
        }
        let next_version = if let Some(existing) = current.as_ref() {
            let sync = txn.open_table(NOTEBOOK_SYNC)?;
            let metadata = read_note_sync_metadata(&sync, &item.id)?
                .ok_or_else(|| Error::Storage("missing notebook sync metadata".into()))?;
            let existing_heads = notebook_sync::materialize_heads(existing, &metadata)?;
            let Some(head) = existing_heads.first() else {
                return Err(Error::Storage("missing notebook sync head".into()));
            };
            notebook_sync::next_local_version(item, &head.clock, &global.replica_id)?
        } else {
            notebook_sync::initial_version(item, &global.replica_id)?
        };
        let metadata = notebook_sync::metadata_from_heads(&[next_version])?;
        let metadata_bytes = serde_json::to_vec(&metadata)?;
        {
            let mut notes = txn.open_table(NOTEBOOKS)?;
            notes.insert(item.id.as_str(), note_bytes.as_slice())?;
        }
        {
            let mut sync = txn.open_table(NOTEBOOK_SYNC)?;
            let key = sync_note_key(&item.id);
            sync.insert(key.as_str(), metadata_bytes.as_slice())?;
            let selected_peer_ids = global.settings.peer_ids.clone();
            global
                .peers
                .retain(|node_id, _| selected_peer_ids.contains(node_id));
            let global_bytes = serde_json::to_vec(&global)?;
            sync.insert(SYNC_GLOBAL_KEY, global_bytes.as_slice())?;
        }
        check_notebook_sync_quotas(&txn, max_items, max_total_bytes)?;
        txn.commit()?;
        self.sync_notify.notify_one();
        Ok(NotebookWriteOutcome::Stored)
    }

    pub(crate) fn resolve_notebook_conflict(
        &self,
        id: &str,
        expected_revision: u64,
        version_id: &str,
    ) -> Result<NotebookResolveOutcome> {
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_write()?;
        let (mut item, global, metadata) = {
            let notes = txn.open_table(NOTEBOOKS)?;
            let Some(bytes) = notes.get(id)? else {
                return Ok(NotebookResolveOutcome::NotFound);
            };
            let item: NotebookItem = serde_json::from_slice(bytes.value())?;
            let sync = txn.open_table(NOTEBOOK_SYNC)?;
            let global = read_sync_global(&sync)?;
            let metadata = read_note_sync_metadata(&sync, id)?
                .ok_or_else(|| Error::Storage("missing notebook sync metadata".into()))?;
            (item, global, metadata)
        };
        if metadata.heads.len() < 2 {
            return Ok(NotebookResolveOutcome::NotConflicted);
        }
        if item.revision != expected_revision {
            return Ok(NotebookResolveOutcome::Conflict {
                current_revision: item.revision,
            });
        }
        let heads = notebook_sync::materialize_heads(&item, &metadata)?;
        let Some(next_revision) = item.revision.checked_add(1) else {
            return Ok(NotebookResolveOutcome::Conflict {
                current_revision: item.revision,
            });
        };
        let (resolved, version) = match notebook_sync::resolve_version(
            id,
            &heads,
            version_id,
            &global.replica_id,
            next_revision,
        ) {
            Ok(value) => value,
            Err(Error::Storage(_)) if !metadata.heads.contains_key(version_id) => {
                return Ok(NotebookResolveOutcome::VersionNotFound);
            }
            Err(error) => return Err(error),
        };
        item = resolved;
        let note_bytes = serde_json::to_vec(&item)?;
        let metadata_bytes = serde_json::to_vec(&notebook_sync::metadata_from_heads(&[version])?)?;
        {
            let mut notes = txn.open_table(NOTEBOOKS)?;
            notes.insert(id, note_bytes.as_slice())?;
        }
        {
            let mut sync = txn.open_table(NOTEBOOK_SYNC)?;
            let key = sync_note_key(id);
            sync.insert(key.as_str(), metadata_bytes.as_slice())?;
        }
        check_notebook_sync_quotas(&txn, MAX_NOTEBOOK_ITEMS, MAX_NOTEBOOK_TOTAL_BYTES)?;
        txn.commit()?;
        self.sync_notify.notify_one();
        Ok(NotebookResolveOutcome::Resolved)
    }

    pub(crate) fn notebook_conflict_versions(&self, id: &str) -> Result<Option<Vec<SyncVersion>>> {
        let Some(snapshot) = self.get_notebook_snapshot(id)? else {
            return Ok(None);
        };
        if snapshot.conflict_count == 0 {
            return Ok(Some(Vec::new()));
        }
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_read()?;
        let notes = txn.open_table(NOTEBOOKS)?;
        let Some(bytes) = notes.get(id)? else {
            return Ok(None);
        };
        let item: NotebookItem = serde_json::from_slice(bytes.value())?;
        let sync = txn.open_table(NOTEBOOK_SYNC)?;
        let metadata = read_note_sync_metadata(&sync, id)?
            .ok_or_else(|| Error::Storage("missing notebook sync metadata".into()))?;
        Ok(Some(notebook_sync::materialize_heads(&item, &metadata)?))
    }

    pub(crate) fn all_notebook_sync_versions(&self) -> Result<Vec<(String, SyncVersion)>> {
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_read()?;
        let rows = {
            let notes = txn.open_table(NOTEBOOKS)?;
            let mut rows = Vec::new();
            for entry in notes.iter()? {
                let (key, value) = entry?;
                rows.push((key.value().to_owned(), value.value().to_vec()));
            }
            rows
        };
        let sync = txn.open_table(NOTEBOOK_SYNC)?;
        let mut versions = Vec::new();
        for (id, bytes) in rows {
            let item: NotebookItem = serde_json::from_slice(&bytes)?;
            if item.id != id {
                return Err(Error::Storage("invalid notebook key".into()));
            }
            let metadata = read_note_sync_metadata(&sync, &id)?
                .ok_or_else(|| Error::Storage("missing notebook sync metadata".into()))?;
            versions.extend(
                notebook_sync::materialize_heads(&item, &metadata)?
                    .into_iter()
                    .map(|version| (id.clone(), version)),
            );
        }
        versions.sort_by(|left, right| {
            (&left.0, &left.1.version_id).cmp(&(&right.0, &right.1.version_id))
        });
        Ok(versions)
    }

    pub(crate) fn notebook_conflict_count(&self) -> Result<usize> {
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_read()?;
        let sync = txn.open_table(NOTEBOOK_SYNC)?;
        let mut count = 0usize;
        for entry in sync.iter()? {
            let (key, value) = entry?;
            if key.value().starts_with(SYNC_NOTE_PREFIX) {
                let metadata: NoteSyncMetadata = serde_json::from_slice(value.value())?;
                count += usize::from(metadata.heads.len() > 1);
            }
        }
        Ok(count)
    }

    pub(crate) fn update_peer_progress<F>(
        &self,
        node_id: &str,
        expected_settings_generation: u64,
        update: F,
    ) -> Result<bool>
    where
        F: FnOnce(&mut PeerProgress),
    {
        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_write()?;
        let mut sync = txn.open_table(NOTEBOOK_SYNC)?;
        let mut global = read_sync_global(&sync)?;
        if global.settings_generation != expected_settings_generation
            || !global.settings.enabled
            || !global.settings.peer_ids.iter().any(|id| id == node_id)
        {
            return Ok(false);
        }
        let progress = global.peers.entry(node_id.to_owned()).or_default();
        update(progress);
        let bytes = serde_json::to_vec(&global)?;
        sync.insert(SYNC_GLOBAL_KEY, bytes.as_slice())?;
        drop(sync);
        check_notebook_sync_quotas(&txn, MAX_NOTEBOOK_ITEMS, MAX_NOTEBOOK_TOTAL_BYTES)?;
        txn.commit()?;
        Ok(true)
    }

    pub(crate) fn peer_is_selected(&self, node_id: &str) -> Result<bool> {
        let state = self.sync_state()?;
        Ok(state.settings.enabled && state.settings.peer_ids.iter().any(|id| id == node_id))
    }

    /// Merge one already-validated page and advance its cursor in the same
    /// transaction. A concurrent settings change or cursor update aborts the
    /// entire page without acknowledging any of its versions.
    pub(crate) fn apply_sync_page(
        &self,
        node_id: &str,
        expected_settings_generation: u64,
        expected_cursor: Option<&str>,
        next_cursor: Option<String>,
        completed: bool,
        versions: Vec<(String, SyncVersion)>,
    ) -> Result<SyncApplyOutcome> {
        use std::collections::{BTreeMap as OrderedMap, HashSet};

        for (note_id, version) in &versions {
            notebook_sync::validate_version(note_id, version)?;
        }
        let mut seen = HashSet::with_capacity(versions.len());
        for (note_id, version) in &versions {
            let key = notebook_sync::cursor_for(note_id, &version.version_id);
            if !seen.insert(key) {
                return Err(Error::Storage("duplicate notebook sync version".into()));
            }
        }
        if completed != next_cursor.is_none() {
            return Err(Error::Storage("invalid notebook sync cursor".into()));
        }
        if let Some(next) = &next_cursor {
            let Some((next_note, next_version)) = notebook_sync::parse_cursor(next) else {
                return Err(Error::Storage("invalid notebook sync cursor".into()));
            };
            if let Some(previous) = expected_cursor {
                let Some((previous_note, previous_version)) = notebook_sync::parse_cursor(previous)
                else {
                    return Err(Error::Storage("invalid notebook sync cursor".into()));
                };
                if (next_note.as_str(), next_version.as_str())
                    <= (previous_note.as_str(), previous_version.as_str())
                {
                    return Err(Error::Storage(
                        "notebook sync cursor did not advance".into(),
                    ));
                }
            }
        }

        struct StagedNote {
            current: Option<NotebookItem>,
            heads: Vec<SyncVersion>,
            changed: bool,
        }

        let db = self.db.lock().map_err(|e| Error::Storage(e.to_string()))?;
        let txn = db.begin_write()?;
        let mut global = {
            let sync = txn.open_table(NOTEBOOK_SYNC)?;
            read_sync_global(&sync)?
        };
        if global.settings_generation != expected_settings_generation
            || !global.settings.enabled
            || !global.settings.peer_ids.iter().any(|id| id == node_id)
        {
            return Ok(SyncApplyOutcome::SettingsRevoked);
        }
        let progress = global.peers.entry(node_id.to_owned()).or_default();
        if progress.cursor.as_deref() != expected_cursor {
            return Ok(SyncApplyOutcome::CursorChanged);
        }

        let mut staged: OrderedMap<String, StagedNote> = OrderedMap::new();
        for (note_id, incoming) in versions {
            if !staged.contains_key(&note_id) {
                let current_bytes = {
                    let notes = txn.open_table(NOTEBOOKS)?;
                    let current = notes
                        .get(note_id.as_str())?
                        .map(|value| value.value().to_vec());
                    current
                };
                let current = current_bytes
                    .as_deref()
                    .map(serde_json::from_slice::<NotebookItem>)
                    .transpose()?;
                let heads = if let Some(item) = current.as_ref() {
                    let sync = txn.open_table(NOTEBOOK_SYNC)?;
                    let metadata = read_note_sync_metadata(&sync, &note_id)?
                        .ok_or_else(|| Error::Storage("missing notebook sync metadata".into()))?;
                    notebook_sync::materialize_heads(item, &metadata)?
                } else {
                    let sync = txn.open_table(NOTEBOOK_SYNC)?;
                    if read_note_sync_metadata(&sync, &note_id)?.is_some() {
                        return Err(Error::Storage("orphan notebook sync metadata".into()));
                    }
                    Vec::new()
                };
                staged.insert(
                    note_id.clone(),
                    StagedNote {
                        current,
                        heads,
                        changed: false,
                    },
                );
            }
            let note = staged.get_mut(&note_id).expect("staged note inserted");
            if note
                .current
                .as_ref()
                .is_some_and(|item| item.kind != incoming.content.kind)
                || note
                    .heads
                    .iter()
                    .any(|head| head.content.kind != incoming.content.kind)
            {
                return Err(Error::Storage("notebook sync kind changed".into()));
            }
            note.changed |= notebook_sync::merge_version(&note_id, &mut note.heads, incoming)?;
        }

        for (note_id, note) in staged {
            if !note.changed {
                continue;
            }
            let primary = notebook_sync::choose_primary(&note.heads)
                .ok_or_else(|| Error::Storage("missing notebook sync head".into()))?;
            let revision = match note.current.as_ref() {
                Some(item) => item
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| Error::Storage("notebook revision exhausted".into()))?,
                None => 1,
            };
            let item = notebook_sync::item_from_content(&primary.content, revision);
            let item_bytes = serde_json::to_vec(&item)?;
            let metadata = notebook_sync::metadata_from_heads(&note.heads)?;
            let metadata_bytes = serde_json::to_vec(&metadata)?;
            {
                let mut notes = txn.open_table(NOTEBOOKS)?;
                notes.insert(note_id.as_str(), item_bytes.as_slice())?;
            }
            {
                let mut sync = txn.open_table(NOTEBOOK_SYNC)?;
                let key = sync_note_key(&note_id);
                sync.insert(key.as_str(), metadata_bytes.as_slice())?;
            }
        }

        let progress = global.peers.entry(node_id.to_owned()).or_default();
        progress.cursor = next_cursor;
        progress.state = if completed {
            PeerSyncState::Checked
        } else {
            PeerSyncState::Pending
        };
        progress.last_success_at = Some(Utc::now());
        progress.error = None;
        progress.failures = 0;
        progress.retry_after = None;
        let global_bytes = serde_json::to_vec(&global)?;
        {
            let mut sync = txn.open_table(NOTEBOOK_SYNC)?;
            sync.insert(SYNC_GLOBAL_KEY, global_bytes.as_slice())?;
        }
        check_notebook_sync_quotas(&txn, MAX_NOTEBOOK_ITEMS, MAX_NOTEBOOK_TOTAL_BYTES)?;
        txn.commit()?;
        Ok(SyncApplyOutcome::Applied)
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

fn notebook_revision(bytes: &[u8]) -> Result<u64> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    value
        .get("revision")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| Error::Storage("invalid notebook revision".into()))
}

fn notebook_trashed(bytes: &[u8]) -> Result<bool> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    value
        .get("trashed")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| Error::Storage("invalid notebook trash state".into()))
}

fn sync_note_key(id: &str) -> String {
    format!("{SYNC_NOTE_PREFIX}{id}")
}

fn read_sync_global<T>(table: &T) -> Result<PersistedSyncState>
where
    T: ReadableTable<&'static str, &'static [u8]>,
{
    let Some(value) = table.get(SYNC_GLOBAL_KEY)? else {
        return Err(Error::Storage("missing notebook sync state".into()));
    };
    let state: PersistedSyncState = serde_json::from_slice(value.value())?;
    let replica_id = Uuid::parse_str(&state.replica_id)
        .ok()
        .map(|uuid| uuid.hyphenated().to_string());
    let unique_peers: std::collections::HashSet<_> = state.settings.peer_ids.iter().collect();
    if replica_id.as_deref() != Some(state.replica_id.as_str())
        || state.settings.peer_ids.len() > notebook_sync::MAX_SYNC_PEERS
        || unique_peers.len() != state.settings.peer_ids.len()
        || state
            .settings
            .peer_ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
        || state
            .peers
            .keys()
            .any(|node_id| !state.settings.peer_ids.contains(node_id))
    {
        return Err(Error::Storage("invalid notebook sync state".into()));
    }
    Ok(state)
}

fn read_note_sync_metadata<T>(table: &T, note_id: &str) -> Result<Option<NoteSyncMetadata>>
where
    T: ReadableTable<&'static str, &'static [u8]>,
{
    let key = sync_note_key(note_id);
    table
        .get(key.as_str())?
        .map(|value| serde_json::from_slice(value.value()).map_err(Error::from))
        .transpose()
}

fn snapshot_from_heads(
    mut item: NotebookItem,
    heads: Vec<SyncVersion>,
) -> Result<NotebookSnapshot> {
    let primary = notebook_sync::choose_primary(&heads)
        .ok_or_else(|| Error::Storage("missing notebook sync head".into()))?;
    let version_id = primary.version_id.clone();
    let conflict_count = heads.len().saturating_sub(1);
    item.version_id = Some(version_id.clone());
    item.conflict_count = conflict_count;
    Ok(NotebookSnapshot {
        item,
        version_id,
        conflict_count,
    })
}

fn check_notebook_sync_quotas(
    txn: &redb::WriteTransaction,
    max_items: usize,
    max_note_bytes: usize,
) -> Result<()> {
    let note_rows: HashMap<String, Vec<u8>> = {
        let notes = txn.open_table(NOTEBOOKS)?;
        let mut rows = HashMap::new();
        for entry in notes.iter()? {
            let (key, value) = entry?;
            rows.insert(key.value().to_owned(), value.value().to_vec());
        }
        rows
    };
    let note_count = note_rows.len();
    let note_payload_bytes = note_rows
        .iter()
        .fold(0usize, |sum, (_, bytes)| sum.saturating_add(bytes.len()));
    let persisted_note_bytes = note_rows.iter().fold(0usize, |sum, (id, bytes)| {
        sum.saturating_add(bytes.len()).saturating_add(id.len())
    });
    let sync = txn.open_table(NOTEBOOK_SYNC)?;
    let mut persisted_meta_bytes = 0usize;
    let mut sync_content_bytes = 0usize;
    let mut metadata_count = 0usize;
    let mut saw_global = false;
    for entry in sync.iter()? {
        let (key, value) = entry?;
        let key = key.value();
        let bytes = value.value();
        persisted_meta_bytes = persisted_meta_bytes
            .saturating_add(key.len())
            .saturating_add(bytes.len());
        if key == SYNC_GLOBAL_KEY {
            saw_global = true;
            continue;
        }
        let Some(note_id) = key.strip_prefix(SYNC_NOTE_PREFIX) else {
            return Err(Error::Storage("invalid notebook sync key".into()));
        };
        metadata_count = metadata_count.saturating_add(1);
        let Some(note_bytes) = note_rows.get(note_id) else {
            return Err(Error::Storage("orphan notebook sync metadata".into()));
        };
        let item: NotebookItem = serde_json::from_slice(note_bytes)?;
        if item.id != note_id {
            return Err(Error::Storage("invalid notebook key".into()));
        }
        let metadata: NoteSyncMetadata = serde_json::from_slice(bytes)?;
        let heads = notebook_sync::materialize_heads(&item, &metadata)?;
        for head in heads {
            let content_bytes = serde_json::to_vec(&head.content)?.len();
            sync_content_bytes = sync_content_bytes.saturating_add(content_bytes);
        }
    }
    if note_count > max_items
        || note_payload_bytes > max_note_bytes
        || metadata_count != note_count
        || !saw_global
    {
        return Err(Error::TooLarge {
            size: note_count.max(note_payload_bytes) as u64,
            limit: max_items.max(max_note_bytes) as u64,
        });
    }
    if sync_content_bytes > MAX_SYNC_CONTENT_BYTES {
        return Err(Error::TooLarge {
            size: sync_content_bytes as u64,
            limit: MAX_SYNC_CONTENT_BYTES as u64,
        });
    }
    let persisted_total = persisted_note_bytes.saturating_add(persisted_meta_bytes);
    if persisted_total > MAX_SYNC_PERSISTED_BYTES {
        return Err(Error::TooLarge {
            size: persisted_total as u64,
            limit: MAX_SYNC_PERSISTED_BYTES as u64,
        });
    }
    Ok(())
}

#[cfg(test)]
mod notebook_storage_tests {
    use super::*;

    fn isolated_root() -> PathBuf {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/session-notebook-store")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn isolated_store_at(root: &Path) -> Store {
        std::fs::create_dir_all(root).unwrap();
        let db = Database::create(root.join("state.redb")).unwrap();
        let txn = db.begin_write().unwrap();
        txn.open_table(ENTRIES).unwrap();
        txn.open_table(NOTEBOOKS).unwrap();
        txn.open_table(NOTEBOOK_SYNC).unwrap();
        txn.commit().unwrap();
        let store = Store {
            db: Mutex::new(db),
            blobs_dir: root.join("blobs"),
            sync_notify: Arc::new(Notify::new()),
        };
        store.initialize_notebook_sync().unwrap();
        store
    }

    fn isolated_store() -> Store {
        isolated_store_at(&isolated_root())
    }

    #[test]
    fn notebook_cas_and_quotas_are_atomic() {
        let store = isolated_store();
        let first = br#"{"revision":1,"trashed":false}"#;
        assert_eq!(
            store.save_notebook_raw("one", first, None, 1, 128).unwrap(),
            NotebookWriteOutcome::Stored
        );
        assert_eq!(
            store.save_notebook_raw("two", first, None, 1, 128).unwrap(),
            NotebookWriteOutcome::QuotaExceeded
        );
        assert_eq!(
            store
                .save_notebook_raw("one", br#"{"revision":2,"trashed":false}"#, Some(0), 1, 128)
                .unwrap(),
            NotebookWriteOutcome::Conflict {
                current_revision: 1
            }
        );

        let oversized = serde_json::to_vec(&serde_json::json!({
            "revision": 2,
            "trashed": false,
            "body": "x".repeat(256),
        }))
        .unwrap();
        assert_eq!(
            store
                .save_notebook_raw("one", &oversized, Some(1), 1, 128)
                .unwrap(),
            NotebookWriteOutcome::QuotaExceeded
        );
        assert_eq!(
            store.get_notebook_raw("one").unwrap().as_deref(),
            Some(&first[..])
        );

        let next = br#"{"revision":2,"trashed":false}"#;
        assert_eq!(
            store
                .save_notebook_raw("one", next, Some(1), 1, 128)
                .unwrap(),
            NotebookWriteOutcome::Stored
        );
        assert_eq!(
            store.get_notebook_raw("one").unwrap().as_deref(),
            Some(&next[..])
        );
    }

    #[test]
    fn notebook_records_survive_reopening_the_database() {
        use crate::notebook::{NotebookItem, NotebookKind, NotebookTask};
        use chrono::Utc;

        let root = isolated_root();
        let item = NotebookItem {
            id: uuid::Uuid::new_v4().to_string(),
            kind: NotebookKind::Todo,
            title: "持久化检查".into(),
            markdown: String::new(),
            tasks: vec![NotebookTask {
                id: uuid::Uuid::new_v4().to_string(),
                text: "本机保存".into(),
                done: true,
            }],
            pinned: false,
            trashed: false,
            revision: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            version_id: None,
            conflict_count: 0,
        };
        let encoded = serde_json::to_vec(&item).unwrap();
        {
            let store = isolated_store_at(&root);
            assert_eq!(
                store
                    .save_notebook_raw(&item.id, &encoded, None, 1_000, 32 * 1024 * 1024)
                    .unwrap(),
                NotebookWriteOutcome::Stored
            );
        }

        let reopened = isolated_store_at(&root);
        let bytes = reopened.get_notebook_raw(&item.id).unwrap().unwrap();
        let persisted: NotebookItem = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(persisted, item);
    }
}
