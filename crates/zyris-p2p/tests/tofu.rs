use zyris_p2p::tofu::{TofuError, TofuStore};

#[tokio::test]
async fn an_unknown_peer_passes() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    assert!(store.check("node-b", "key-1").await.is_ok());
}

#[tokio::test]
async fn the_same_key_passes_after_pinning() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    store.pin("node-b", "key-1").await.unwrap();
    assert!(store.check("node-b", "key-1").await.is_ok());
}

#[tokio::test]
async fn a_changed_key_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    store.pin("node-b", "key-1").await.unwrap();

    match store.check("node-b", "key-2").await {
        Err(TofuError::Changed { pinned, offered }) => {
            assert_eq!(pinned, "key-1");
            assert_eq!(offered, "key-2");
        }
        other => panic!("a changed key must be refused, got {other:?}"),
    }
}

#[tokio::test]
async fn the_pin_survives_a_new_store_instance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.json");
    TofuStore::new(&path).pin("node-b", "key-1").await.unwrap();

    // A fresh instance: this has to come off disk.
    let result = TofuStore::new(&path).check("node-b", "key-2").await;
    assert!(matches!(result, Err(TofuError::Changed { .. })));
}

#[tokio::test]
async fn pinning_twice_keeps_the_first_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    store.pin("node-b", "key-1").await.unwrap();
    store.pin("node-b", "key-2").await.unwrap();

    // `pin` keeps the first value. If a later call could overwrite it, pinning would
    // mean nothing — the substitution we are trying to catch would pin itself.
    assert!(matches!(store.check("node-b", "key-2").await, Err(TofuError::Changed { .. })));
}

#[tokio::test]
async fn a_corrupt_pin_file_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.json");

    // Each of these is a shape a ledger must never silently read as "no pins ever taken".
    // One input passing this test (the garbage string alone, before) is exactly what let
    // `{}`, `[]`, and a renamed `peers` key all fail open — one test case gives one input's
    // worth of confidence, not the property's.
    let bad_contents: [(&str, &[u8]); 6] = [
        ("empty object", &b"{}"[..]),
        ("json array", &b"[]"[..]),
        ("wrong-shaped object", &br#"{"nope": {}}"#[..]),
        ("null peers", &br#"{"peers": null}"#[..]),
        ("zero-length file", &b""[..]),
        ("garbage", &b"{ this is not json"[..]),
    ];

    for (name, content) in bad_contents {
        let store = TofuStore::new(&path);
        store.pin("node-b", "key-1").await.unwrap();
        tokio::fs::write(&path, content).await.unwrap();

        // Treating an unreadable ledger as "nothing is pinned" would let anyone erase every
        // pin by corrupting one file — which is exactly what you would do right before
        // swapping a key.
        let result = store.check("node-b", "key-2").await;
        assert!(matches!(result, Err(TofuError::Malformed { .. })), "{name}: got {result:?}");
        // Even the peer that IS pinned correctly must not slip through while we cannot read.
        let same = store.check("node-b", "key-1").await;
        assert!(matches!(same, Err(TofuError::Malformed { .. })), "{name}: got {same:?}");

        tokio::fs::remove_file(&path).await.unwrap();
    }
}

#[tokio::test]
async fn pinning_a_second_peer_keeps_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    store.pin("node-a", "key-a").await.unwrap();
    store.pin("node-b", "key-b").await.unwrap();

    assert!(matches!(store.check("node-a", "other").await, Err(TofuError::Changed { .. })));
    assert!(matches!(store.check("node-b", "other").await, Err(TofuError::Changed { .. })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_pins_all_survive() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.json");

    let mut tasks = Vec::new();
    for i in 0..8 {
        // A fresh `TofuStore` per task, not a clone. Cloning shares the in-process mutex,
        // which would serialize these writes on its own and hide exactly the loss this test
        // exists to catch — the pin file has to be what keeps two independent stores (as
        // separate processes would be) from stepping on each other, not the mutex.
        let store = TofuStore::new(&path);
        tasks.push(tokio::spawn(async move {
            store.pin(&format!("node-{i}"), &format!("key-{i}")).await.unwrap();
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }

    let store = TofuStore::new(&path);
    // A lost pin is not a lost log line: that peer is "unknown" again, so the next key
    // change for it passes unnoticed.
    for i in 0..8 {
        let result = store.check(&format!("node-{i}"), "someone-else").await;
        assert!(matches!(result, Err(TofuError::Changed { .. })), "node-{i} lost its pin");
    }
}
