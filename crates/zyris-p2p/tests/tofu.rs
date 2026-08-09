use zyris_p2p::tofu::{TofuError, TofuStore};

#[tokio::test]
async fn an_unknown_peer_passes() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    assert!(store.check("kitchen-pi", "key-1").await.is_ok());
}

#[tokio::test]
async fn the_same_key_passes_after_pinning() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    store.pin("kitchen-pi", "key-1").await.unwrap();
    assert!(store.check("kitchen-pi", "key-1").await.is_ok());
}

#[tokio::test]
async fn a_changed_key_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    store.pin("kitchen-pi", "key-1").await.unwrap();

    match store.check("kitchen-pi", "key-2").await {
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
    TofuStore::new(&path).pin("kitchen-pi", "key-1").await.unwrap();

    // A fresh instance: this has to come off disk.
    let result = TofuStore::new(&path).check("kitchen-pi", "key-2").await;
    assert!(matches!(result, Err(TofuError::Changed { .. })));
}

#[tokio::test]
async fn pinning_twice_keeps_the_first_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    store.pin("kitchen-pi", "key-1").await.unwrap();

    // `pin` keeps the first value. If a later call could overwrite it, pinning would
    // mean nothing — the substitution we are trying to catch would pin itself. The second
    // call must also *report* the mismatch, not just refuse to act on it — a caller that
    // pins without calling `check` first must still learn a substitution was attempted.
    let result = store.pin("kitchen-pi", "key-2").await;
    match result {
        Err(TofuError::Changed { pinned, offered }) => {
            assert_eq!(pinned, "key-1");
            assert_eq!(offered, "key-2");
        }
        other => panic!("a re-pin with a different key must be reported, got {other:?}"),
    }

    assert!(matches!(store.check("kitchen-pi", "key-2").await, Err(TofuError::Changed { .. })));
}

#[tokio::test]
async fn re_pinning_the_identical_key_is_a_silent_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    store.pin("kitchen-pi", "key-1").await.unwrap();

    // Only a re-pin of the *same* key is a true no-op — this is not a substitution attempt,
    // so it must not be reported as one.
    assert!(store.pin("kitchen-pi", "key-1").await.is_ok());
    assert!(store.check("kitchen-pi", "key-1").await.is_ok());
}

#[tokio::test]
async fn a_corrupt_pin_file_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.json");

    // Each of these is a shape a ledger must never silently read as "no pins ever taken".
    // One input passing this test (the garbage string alone, before) is exactly what let
    // `{}`, `[]`, and a renamed `peers` key all fail open — one test case gives one input's
    // worth of confidence, not the property's.
    let bad_contents: [(&str, &[u8]); 7] = [
        ("empty object", &b"{}"[..]),
        ("json array", &b"[]"[..]),
        ("wrong-shaped object", &br#"{"nope": {}}"#[..]),
        ("null peers", &br#"{"peers": null}"#[..]),
        // `peers` is present and well-formed here, so this shape only fails because of
        // `deny_unknown_fields` — every other case above would already fail on a missing or
        // mistyped `peers` field alone, so none of them actually exercise that attribute.
        ("unexpected extra field", &br#"{"peers":{},"x":1}"#[..]),
        ("zero-length file", &b""[..]),
        ("garbage", &b"{ this is not json"[..]),
    ];

    for (name, content) in bad_contents {
        let store = TofuStore::new(&path);
        store.pin("kitchen-pi", "key-1").await.unwrap();
        tokio::fs::write(&path, content).await.unwrap();

        // Treating an unreadable ledger as "nothing is pinned" would let anyone erase every
        // pin by corrupting one file — which is exactly what you would do right before
        // swapping a key.
        let result = store.check("kitchen-pi", "key-2").await;
        assert!(matches!(result, Err(TofuError::Malformed { .. })), "{name}: got {result:?}");
        // Even the peer that IS pinned correctly must not slip through while we cannot read.
        let same = store.check("kitchen-pi", "key-1").await;
        assert!(matches!(same, Err(TofuError::Malformed { .. })), "{name}: got {same:?}");

        tokio::fs::remove_file(&path).await.unwrap();
    }
}

#[tokio::test]
async fn pinning_a_second_peer_keeps_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    store.pin("garage-cam", "key-a").await.unwrap();
    store.pin("kitchen-pi", "key-b").await.unwrap();

    assert!(matches!(store.check("garage-cam", "other").await, Err(TofuError::Changed { .. })));
    assert!(matches!(store.check("kitchen-pi", "other").await, Err(TofuError::Changed { .. })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_pins_all_survive() {
    // Multiple independent rounds, each wider than the original 8 tasks: measured against
    // the missing-lock-file mutation, one round of 8 caught it 11/12 runs, not reliably
    // enough. More tasks raise the odds any one round hits the race, and more rounds mean a
    // single `cargo test` invocation gets more than one chance to.
    for round in 0..3 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("peers.json");

        let mut tasks = Vec::new();
        for i in 0..16 {
            // A fresh `TofuStore` per task, not a clone. Cloning shares the in-process
            // mutex, which would serialize these writes on its own and hide exactly the loss
            // this test exists to catch — the pin file has to be what keeps two independent
            // stores (as separate processes would be) from stepping on each other, not the
            // mutex.
            let store = TofuStore::new(&path);
            tasks.push(tokio::spawn(async move {
                store.pin(&format!("gadget-{i}"), &format!("key-{i}")).await.unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }

        let store = TofuStore::new(&path);
        // A lost pin is not a lost log line: that peer is "unknown" again, so the next key
        // change for it passes unnoticed.
        for i in 0..16 {
            let result = store.check(&format!("gadget-{i}"), "someone-else").await;
            assert!(matches!(result, Err(TofuError::Changed { .. })), "round {round}: gadget-{i} lost its pin");
        }
    }
}

#[tokio::test]
async fn pinning_into_a_missing_parent_directory_creates_it() {
    let dir = tempfile::tempdir().unwrap();
    // Nothing under `dir` exists yet beyond `dir` itself — this is what a node's very first
    // run looks like, before its config directory has ever been created.
    let path = dir.path().join("nested").join("deeper").join("peers.json");
    let store = TofuStore::new(&path);

    store.pin("kitchen-pi", "key-1").await.unwrap();
    assert!(store.check("kitchen-pi", "key-1").await.is_ok());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path.parent().unwrap()).unwrap();
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o700,
            "the created parent directory must be 0700, not the default umask"
        );
    }
}

/// Mirrors `TofuStore`'s private `lock_path` derivation so the test can find and manipulate
/// the lock file through only the public API's notion of where the ledger lives.
fn lock_path_for(path: &std::path::Path) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    std::path::PathBuf::from(name)
}

#[tokio::test]
async fn a_stale_lock_file_is_broken_and_the_pin_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.json");
    let lock_path = lock_path_for(&path);
    tokio::fs::write(&lock_path, b"stale-nonce").await.unwrap();

    // Backdate instead of sleeping for the real threshold: `set_times` is stable std, no new
    // dependency needed.
    let file = std::fs::OpenOptions::new().write(true).open(&lock_path).unwrap();
    let ancient = std::time::SystemTime::now() - std::time::Duration::from_secs(120);
    file.set_times(std::fs::FileTimes::new().set_modified(ancient)).unwrap();

    let store = TofuStore::new(&path);
    // A lock left behind by a dead writer (killed, crashed, OOM-reaped) must not block every
    // later pin forever — this machine runs earlyoom and systemd-oomd for exactly that
    // reason, so it is not hypothetical here.
    store.pin("kitchen-pi", "key-1").await.unwrap();
    assert!(store.check("kitchen-pi", "key-1").await.is_ok());
}

#[tokio::test]
async fn a_fresh_lock_file_is_not_broken_and_the_pin_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.json");
    let lock_path = lock_path_for(&path);
    // Freshly written: its mtime is "now", nowhere near the staleness threshold.
    tokio::fs::write(&lock_path, b"fresh-nonce").await.unwrap();

    // A short timeout instead of the 5s default: this test only cares that the retry loop
    // gives up and reports an error, not about the production wait time. `with_lock_timeout`
    // is `#[doc(hidden)]` — for tests exactly like this one, not for real callers.
    let store = TofuStore::new(&path).with_lock_timeout(std::time::Duration::from_millis(100));
    // A live writer's lock must never be stolen: this has to fail loudly, not proceed as if
    // no one held it and not silently drop the pin.
    let result = store.pin("kitchen-pi", "key-1").await;
    assert!(matches!(result, Err(TofuError::Io(_))), "got {result:?}");
}
