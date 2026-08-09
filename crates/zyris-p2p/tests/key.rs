use zyris_p2p::key::{load_or_create, KeyError};

#[tokio::test]
async fn creates_the_key_at_0600_on_first_use() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peer.key");
    let key = load_or_create(&path).await.unwrap();

    assert!(path.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "actual: {mode:o}");
    }
    // A second call must return the same key — regenerating on every call would make the peer's
    // TOFU pinning reject us every time, since the peer's identity check is keyed off our
    // public key.
    let again = load_or_create(&path).await.unwrap();
    assert_eq!(key.public(), again.public());
}

#[tokio::test]
async fn rejects_a_key_file_others_can_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peer.key");
    load_or_create(&path).await.unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let result = load_or_create(&path).await;
        assert!(matches!(result, Err(KeyError::Permissions(_))), "actual: {result:?}");
    }
}

/// The bytes are on disk by the time `load_or_create` returns.
///
/// `tokio::fs::File::write_all` hands the real write to a blocking pool and returns without
/// waiting, so a plain `write_all` leaves a window where the file exists but is empty. A node
/// killed in that window comes back to a zero-length key and cannot start.
///
/// The loop is the test. A single round passed more often than not even while the bug was
/// present; at fifty rounds the pre-fix code failed essentially every time (it was measured at
/// 474 empty files out of 500 single attempts).
#[tokio::test]
async fn the_key_is_on_disk_before_the_call_returns() {
    let dir = tempfile::tempdir().unwrap();
    for round in 0..50 {
        let path = dir.path().join(format!("peer-{round}.key"));
        let key = load_or_create(&path).await.unwrap();
        let on_disk = std::fs::read(&path).unwrap();
        assert_eq!(on_disk.len(), 32, "round {round}: key file is {} bytes", on_disk.len());
        assert_eq!(on_disk, key.to_bytes(), "round {round}: file does not hold the key we returned");
    }
}
