# Encrypted credential vault core

Development implementation, not yet connected to the tray, mobile backend or a
network synchronization service. Do not store production credentials here yet.

## Implemented

- Typed system/database/Redis/website account records; names, addresses, users,
  passwords and notes are encrypted together.
- Random 256-bit vault key, wrapped with a master-password-derived key using
  PBKDF2-HMAC-SHA256 (600,000 iterations, random 128-bit salt) and AES-256-GCM.
  Each record uses an OS-random 96-bit nonce; format, vault ID and parent revisions
  are authenticated. Password strength and mobile KDF latency still need UX work.
- No plaintext `Debug`, clipboard, database logging, AI endpoints or implicit
  device trust. Controlled plaintext buffers are zeroized when dropped, without
  claiming erasure of all allocator/OS/cipher implementation copies.
- Add/edit/delete and explicit conflict resolution. A tombstone is encrypted
  `null`. Import only adds revisions, preserves offline edit/delete conflicts and
  cannot replace the local history with an older snapshot.
- Immutable encrypted checkpoints, file flush then rename, content-hash lookup,
  bounded reads and rejection of malformed revision paths.

`Vault::decode` is for opening an explicitly chosen checkpoint or initializing a
replica. It does not determine whether that checkpoint is the latest. Existing
replicas must call `merge` to preserve their already-seen history. The storage
directory must be private to the current OS user; no shared or untrusted paths.

## Not implemented

Approved-member identities, signed device operations, revocation/key rotation,
authenticated network transport, persisted anti-rollback anchors, automatic OS
lock integration, safe master-password input UI, backup/recovery UX and mobile
native bridge. `merge` is a library primitive requiring an already-approved peer,
not a device-authentication endpoint. A holder of the vault key can author data;
no author/device attribution is claimed by this format.

Never attach this library to the ordinary clipboard HTTP service. The Flutter
view is a separate controller-driven component until its local secure bridge and
device protocol are implemented and tested.

## Verification

```sh
cargo test -p peercarry-vault
```

Tests use synthetic records and cover decryption, wrong passwords, tampering,
cross-vault rejection, concurrent edits, deletion versus stale imports, locking,
nonce variation and immutable-storage integrity. Replica tests run in one
process; they are not cross-device acceptance.
