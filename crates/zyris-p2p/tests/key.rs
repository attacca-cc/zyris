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
