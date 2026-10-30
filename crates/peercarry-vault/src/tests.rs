use super::*;
const PASSWORD: &[u8] = b"synthetic-test-master-phrase";
fn account(value: &str) -> Account {
    Account {
        category: Category::Redis,
        name: "synthetic-redis".into(),
        environment: "test".into(),
        address: "redis.example.invalid".into(),
        port: Some(6379),
        username: "synthetic-user".into(),
        password: value.into(),
        database: "0".into(),
        tls: true,
        notes: "synthetic-note".into(),
        tags: vec!["test".into()],
        updated_at: None,
        expires_at: None,
    }
}

#[test]
fn encryption_roundtrip_and_no_plaintext_metadata() {
    let mut a = Vault::create(PASSWORD).unwrap();
    let id = a.add(account("synthetic-secret")).unwrap();
    let encoded = a.encode().unwrap();
    let text = String::from_utf8(encoded.clone()).unwrap();
    for hidden in [
        "synthetic-secret",
        "synthetic-redis",
        "synthetic-user",
        "redis.example.invalid",
        "synthetic-note",
    ] {
        assert!(!text.contains(hidden));
    }
    let b = Vault::decode(&encoded, PASSWORD).unwrap();
    assert_eq!(
        b.read_revision(b.heads(&id).first().unwrap())
            .unwrap()
            .unwrap()
            .password,
        "synthetic-secret"
    );
    assert!(matches!(
        Vault::decode(&encoded, b"wrong-password"),
        Err(Error::Authentication)
    ));
}
#[test]
fn tampering_headers_ciphertext_and_cross_vault_are_rejected() {
    let mut a = Vault::create(PASSWORD).unwrap();
    a.add(account("synthetic-secret")).unwrap();
    let mut snapshot = a.snapshot();
    snapshot.operations[0].ciphertext[0] ^= 1;
    assert!(matches!(a.merge(snapshot), Err(Error::Authentication)));
    let mut snapshot = a.snapshot();
    snapshot.header.rounds = 1;
    assert!(matches!(
        Vault::decode(&json(&snapshot).unwrap(), PASSWORD),
        Err(Error::Invalid)
    ));
    let mut b = Vault::create(PASSWORD).unwrap();
    assert!(matches!(b.merge(a.snapshot()), Err(Error::Authentication)));
}
#[test]
fn two_devices_keep_concurrent_edits_and_explicit_resolution() {
    let mut a = Vault::create(PASSWORD).unwrap();
    let id = a.add(account("initial")).unwrap();
    let mut b = Vault::decode(&a.encode().unwrap(), PASSWORD).unwrap();
    a.write(&id, a.heads(&id), Some(account("device-a")))
        .unwrap();
    b.write(&id, b.heads(&id), Some(account("device-b")))
        .unwrap();
    a.merge(b.snapshot()).unwrap();
    assert_eq!(a.heads(&id).len(), 2);
    assert!(matches!(
        a.write(&id, BTreeSet::new(), Some(account("stale"))),
        Err(Error::Conflict)
    ));
    a.write(&id, a.heads(&id), Some(account("resolved")))
        .unwrap();
    b.merge(a.snapshot()).unwrap();
    assert_eq!(b.heads(&id).len(), 1);
    assert_eq!(
        b.read_revision(b.heads(&id).first().unwrap())
            .unwrap()
            .unwrap()
            .password,
        "resolved"
    );
    assert_eq!(b.merge(a.snapshot()).unwrap(), 0);
}
#[test]
fn tombstone_survives_stale_peer_and_conflicts_with_concurrent_edit() {
    let mut a = Vault::create(PASSWORD).unwrap();
    let id = a.add(account("initial")).unwrap();
    let stale = a.snapshot();
    let mut b = Vault::decode(&a.encode().unwrap(), PASSWORD).unwrap();
    a.write(&id, a.heads(&id), None).unwrap();
    a.merge(stale).unwrap();
    assert!(a
        .read_revision(a.heads(&id).first().unwrap())
        .unwrap()
        .is_none());
    b.write(&id, b.heads(&id), Some(account("offline-edit")))
        .unwrap();
    a.merge(b.snapshot()).unwrap();
    assert_eq!(a.heads(&id).len(), 2);
}
#[test]
fn locked_state_blocks_plaintext_access_and_mutations() {
    let mut a = Vault::create(PASSWORD).unwrap();
    let id = a.add(account("synthetic-secret")).unwrap();
    let rev = a.heads(&id).first().unwrap().clone();
    a.lock();
    assert!(matches!(a.read_revision(&rev), Err(Error::Locked)));
    assert!(matches!(a.add(account("other")), Err(Error::Locked)));
    assert!(a.unlock(b"incorrect-password").is_err());
    assert!(a.is_locked());
    a.unlock(PASSWORD).unwrap();
    assert!(!a.is_locked());
}
#[test]
fn immutable_storage_roundtrip_rejects_truncation_and_path_escape() {
    let root = std::env::temp_dir().join(format!(
        "peercarry-vault-test-{}",
        hex::encode(random::<16>().unwrap())
    ));
    let mut a = Vault::create(PASSWORD).unwrap();
    a.add(account("synthetic-secret")).unwrap();
    let revision = storage::save(&root, &a).unwrap();
    assert_eq!(storage::save(&root, &a).unwrap(), revision);
    assert!(storage::load(&root, &revision, PASSWORD).is_ok());
    assert!(matches!(
        storage::load(&root, "../outside", PASSWORD),
        Err(Error::Invalid)
    ));
    std::fs::write(root.join(format!("{revision}.pcvault")), b"truncated").unwrap();
    assert!(matches!(
        storage::load(&root, &revision, PASSWORD),
        Err(Error::Authentication)
    ));
    // Only remove files explicitly created by this test, no recursive deletion.
    std::fs::remove_file(root.join(format!("{revision}.pcvault"))).unwrap();
    std::fs::remove_dir(root).unwrap();
}
#[test]
fn repeated_content_has_distinct_nonces_and_ciphertext() {
    let mut a = Vault::create(PASSWORD).unwrap();
    let id = a.add(account("same")).unwrap();
    let first = a.operations.values().next().unwrap().nonce;
    let revision = a.write(&id, a.heads(&id), Some(account("same"))).unwrap();
    assert_ne!(first, a.operations[&revision].nonce);
}
