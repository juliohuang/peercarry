//! Encrypted account records and conflict-preserving replica primitives.
//! No network, clipboard, AI tools or implicit device authorization.
//! The transport must authenticate and authorize a replica before import.
use ring::{
    aead, digest, pbkdf2,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU32,
};
use zeroize::{Zeroize, Zeroizing};
pub mod storage;

const VERSION: u32 = 1;
const ROUNDS: u32 = 600_000;
const MAX_RECORD: usize = 64 * 1024;
const MAX_OPS: usize = 10_000;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("invalid_vault_data")]
    Invalid,
    #[error("authentication_failed")]
    Authentication,
    #[error("vault_locked")]
    Locked,
    #[error("conflict_requires_resolution")]
    Conflict,
    #[error("missing_parent_revision")]
    MissingParent,
    #[error("vault_limit_reached")]
    Limit,
    #[error("storage_error")]
    Storage,
}
type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    System,
    Database,
    Redis,
    Website,
}

/// No Debug implementation: callers must not log credential fields.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Account {
    pub category: Category,
    pub name: String,
    pub environment: String,
    pub address: String,
    pub port: Option<u16>,
    pub username: String,
    pub password: String,
    pub database: String,
    pub tls: bool,
    pub notes: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub expires_at: Option<i64>,
}
impl Drop for Account {
    fn drop(&mut self) {
        for value in [
            &mut self.name,
            &mut self.environment,
            &mut self.address,
            &mut self.username,
            &mut self.password,
            &mut self.database,
            &mut self.notes,
        ] {
            value.zeroize();
        }
        for tag in &mut self.tags {
            tag.zeroize();
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    version: u32,
    id: String,
    rounds: u32,
    salt: [u8; 16],
    nonce: [u8; 12],
    wrapped_key: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    vault: String,
    entry: String,
    parents: BTreeSet<String>,
    nonce: [u8; 12],
    ciphertext: Vec<u8>,
}
impl Operation {
    pub fn revision(&self) -> String {
        // Struct field order and sorted parent sets make serialization stable.
        hex::encode(
            digest::digest(
                &digest::SHA256,
                &serde_json::to_vec(self).expect("fixed schema"),
            )
            .as_ref(),
        )
    }
    pub fn entry_id(&self) -> &str {
        &self.entry
    }
    fn aad(&self) -> Result<Vec<u8>> {
        json(&(
            "peercarry-vault-record-v1",
            &self.vault,
            &self.entry,
            &self.parents,
        ))
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    header: Header,
    operations: Vec<Operation>,
}

pub struct Vault {
    header: Header,
    key: Option<Zeroizing<[u8; 32]>>,
    operations: BTreeMap<String, Operation>,
}

fn json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(value).map_err(|_| Error::Invalid)
}
fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Error::Invalid)?;
    Ok(bytes)
}
fn key(password: &[u8], h: &Header) -> Result<Zeroizing<[u8; 32]>> {
    if h.version != VERSION
        || h.rounds != ROUNDS
        || h.wrapped_key.len() != 48
        || h.id.len() != 32
        || !h.id.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(Error::Invalid);
    }
    if password.is_empty() || password.len() > 1024 {
        return Err(Error::Authentication);
    }
    let mut out = Zeroizing::new([0; 32]);
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(ROUNDS).unwrap(),
        &h.salt,
        password,
        out.as_mut(),
    );
    Ok(out)
}
fn header_aad(h: &Header) -> Result<Vec<u8>> {
    json(&("peercarry-vault-key", h.version, &h.id, h.rounds, h.salt))
}
fn cipher(k: &[u8]) -> Result<aead::LessSafeKey> {
    Ok(aead::LessSafeKey::new(
        aead::UnboundKey::new(&aead::AES_256_GCM, k).map_err(|_| Error::Invalid)?,
    ))
}
fn seal(k: &[u8], nonce: [u8; 12], aad: &[u8], plain: &[u8]) -> Result<Vec<u8>> {
    let mut bytes = Zeroizing::new(plain.to_vec());
    cipher(k)?
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(aad),
            &mut *bytes,
        )
        .map_err(|_| Error::Authentication)?;
    Ok(bytes.to_vec())
}
fn open(k: &[u8], nonce: [u8; 12], aad: &[u8], encrypted: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(encrypted.to_vec());
    let len = cipher(k)?
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(aad),
            &mut bytes,
        )
        .map_err(|_| Error::Authentication)?
        .len();
    bytes.truncate(len);
    Ok(bytes)
}

impl Vault {
    pub fn create(password: &[u8]) -> Result<Self> {
        if password.len() < 12 || password.len() > 1024 {
            return Err(Error::Invalid);
        }
        let secret = Zeroizing::new(random::<32>()?);
        let mut header = Header {
            version: VERSION,
            id: hex::encode(random::<16>()?),
            rounds: ROUNDS,
            salt: random()?,
            nonce: random()?,
            wrapped_key: vec![0; 48],
        };
        let kek = key(password, &header)?;
        header.wrapped_key = seal(
            kek.as_ref(),
            header.nonce,
            &header_aad(&header)?,
            secret.as_ref(),
        )?;
        Ok(Self {
            header,
            key: Some(secret),
            operations: BTreeMap::new(),
        })
    }
    pub fn lock(&mut self) {
        self.key = None;
    }
    pub fn is_locked(&self) -> bool {
        self.key.is_none()
    }
    pub fn unlock(&mut self, password: &[u8]) -> Result<()> {
        self.lock();
        let kek = key(password, &self.header)?;
        let raw = open(
            kek.as_ref(),
            self.header.nonce,
            &header_aad(&self.header)?,
            &self.header.wrapped_key,
        )?;
        let secret: [u8; 32] = raw
            .as_slice()
            .try_into()
            .map_err(|_| Error::Authentication)?;
        self.key = Some(Zeroizing::new(secret));
        Ok(())
    }
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            header: self.header.clone(),
            operations: self.operations.values().cloned().collect(),
        }
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        let bytes = json(&self.snapshot())?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err(Error::Limit);
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8], password: &[u8]) -> Result<Self> {
        if bytes.len() > 32 * 1024 * 1024 {
            return Err(Error::Limit);
        }
        let snapshot: Snapshot = serde_json::from_slice(bytes).map_err(|_| Error::Invalid)?;
        let mut vault = Self {
            header: snapshot.header.clone(),
            key: None,
            operations: BTreeMap::new(),
        };
        vault.unlock(password)?;
        vault.merge(snapshot)?;
        Ok(vault)
    }
    /// Caller explicitly approves the peer before importing. This is not device authentication.
    /// Import is additive and transactional: it cannot erase already-seen revisions.
    pub fn merge(&mut self, snapshot: Snapshot) -> Result<usize> {
        let secret = self.key.as_ref().ok_or(Error::Locked)?;
        if json(&self.header)? != json(&snapshot.header)? {
            return Err(Error::Authentication);
        }
        if snapshot.operations.len() > MAX_OPS {
            return Err(Error::Limit);
        }
        let mut candidate = self.operations.clone();
        for op in snapshot.operations {
            if op.vault != self.header.id
                || op.entry.len() != 32
                || !op.entry.bytes().all(|b| b.is_ascii_hexdigit())
                || op.parents.len() > 32
                || op.parents.iter().any(|parent| {
                    parent.len() != 64 || !parent.bytes().all(|b| b.is_ascii_hexdigit())
                })
                || op.ciphertext.len() > MAX_RECORD + 16
            {
                return Err(Error::Invalid);
            }
            let raw = open(secret.as_ref(), op.nonce, &op.aad()?, &op.ciphertext)?;
            let _: Option<Account> = serde_json::from_slice(&raw).map_err(|_| Error::Invalid)?;
            candidate.insert(op.revision(), op);
            if candidate.len() > MAX_OPS {
                return Err(Error::Limit);
            }
        }
        for op in candidate.values() {
            for parent in &op.parents {
                let p = candidate.get(parent).ok_or(Error::MissingParent)?;
                if p.entry != op.entry {
                    return Err(Error::Invalid);
                }
            }
        }
        let count = candidate.len() - self.operations.len();
        self.operations = candidate;
        Ok(count)
    }
    pub fn entry_ids(&self) -> Result<BTreeSet<String>> {
        self.key.as_ref().ok_or(Error::Locked)?;
        Ok(self
            .operations
            .values()
            .map(|op| op.entry.clone())
            .collect())
    }
    pub fn heads(&self, entry: &str) -> BTreeSet<String> {
        let mut heads: BTreeSet<_> = self
            .operations
            .iter()
            .filter(|(_, op)| op.entry == entry)
            .map(|(id, _)| id.clone())
            .collect();
        for op in self.operations.values().filter(|op| op.entry == entry) {
            for p in &op.parents {
                heads.remove(p);
            }
        }
        heads
    }
    /// Tombstones decrypt to None; competing tombstone/edit branches remain a conflict.
    pub fn read_revision(&self, revision: &str) -> Result<Option<Account>> {
        let secret = self.key.as_ref().ok_or(Error::Locked)?;
        let op = self.operations.get(revision).ok_or(Error::Invalid)?;
        let raw = open(secret.as_ref(), op.nonce, &op.aad()?, &op.ciphertext)?;
        serde_json::from_slice(&raw).map_err(|_| Error::Invalid)
    }
    pub fn add(&mut self, value: Account) -> Result<String> {
        let entry = hex::encode(random::<16>()?);
        self.write(&entry, BTreeSet::new(), Some(value))?;
        Ok(entry)
    }
    /// Resolve/edit/delete only against all observed heads (optimistic concurrency).
    pub fn write(
        &mut self,
        entry: &str,
        parents: BTreeSet<String>,
        value: Option<Account>,
    ) -> Result<String> {
        let secret = self.key.as_ref().ok_or(Error::Locked)?;
        if self.operations.len() >= MAX_OPS {
            return Err(Error::Limit);
        }
        if entry.len() != 32 || !entry.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Invalid);
        }
        if parents != self.heads(entry) || parents.len() > 32 {
            return Err(Error::Conflict);
        }
        let plain = Zeroizing::new(json(&value)?);
        if plain.len() > MAX_RECORD {
            return Err(Error::Limit);
        }
        let mut op = Operation {
            vault: self.header.id.clone(),
            entry: entry.into(),
            parents,
            nonce: random()?,
            ciphertext: Vec::new(),
        };
        op.ciphertext = seal(secret.as_ref(), op.nonce, &op.aad()?, &plain)?;
        let revision = op.revision();
        self.operations.insert(revision.clone(), op);
        Ok(revision)
    }
}

#[cfg(test)]
mod tests;
