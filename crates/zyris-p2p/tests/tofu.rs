use std::sync::atomic::{AtomicUsize, Ordering};

use zyris_p2p::fingerprint::{fingerprint, PeerConfirmer};
use zyris_p2p::tofu::{TofuError, TofuStore};

/// A fresh, genuinely valid `EndpointId` string. Needed anywhere a test drives `authorize`
/// down the unknown-peer path, since that path calls `fingerprint`, which (correctly) rejects
/// anything that does not parse as a real key — placeholder strings like `"key-1"` are fine for
/// `check`/`pin_preapproved` (which never parse their `endpoint_id`) but not here.
fn fresh_endpoint_id() -> String {
    iroh::SecretKey::generate().public().to_string()
}

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
    store.pin_preapproved("kitchen-pi", "key-1").await.unwrap();
    assert!(store.check("kitchen-pi", "key-1").await.is_ok());
}

#[tokio::test]
async fn a_changed_key_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    store.pin_preapproved("kitchen-pi", "key-1").await.unwrap();

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
    TofuStore::new(&path).pin_preapproved("kitchen-pi", "key-1").await.unwrap();

    // A fresh instance: this has to come off disk.
    let result = TofuStore::new(&path).check("kitchen-pi", "key-2").await;
    assert!(matches!(result, Err(TofuError::Changed { .. })));
}

#[tokio::test]
async fn pinning_twice_keeps_the_first_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    store.pin_preapproved("kitchen-pi", "key-1").await.unwrap();

    // `pin_preapproved` keeps the first value. If a later call could overwrite it, pinning
    // would mean nothing — the substitution we are trying to catch would pin itself. The
    // second call must also *report* the mismatch, not just refuse to act on it — a caller
    // that pins without calling `check` first must still learn a substitution was attempted.
    let result = store.pin_preapproved("kitchen-pi", "key-2").await;
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
    store.pin_preapproved("kitchen-pi", "key-1").await.unwrap();

    // Only a re-pin of the *same* key is a true no-op — this is not a substitution attempt,
    // so it must not be reported as one.
    assert!(store.pin_preapproved("kitchen-pi", "key-1").await.is_ok());
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
        store.pin_preapproved("kitchen-pi", "key-1").await.unwrap();
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
    store.pin_preapproved("garage-cam", "key-a").await.unwrap();
    store.pin_preapproved("kitchen-pi", "key-b").await.unwrap();

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
                store.pin_preapproved(&format!("gadget-{i}"), &format!("key-{i}")).await.unwrap();
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

    store.pin_preapproved("kitchen-pi", "key-1").await.unwrap();
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
    store.pin_preapproved("kitchen-pi", "key-1").await.unwrap();
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
    let result = store.pin_preapproved("kitchen-pi", "key-1").await;
    assert!(matches!(result, Err(TofuError::Io(_))), "got {result:?}");
}

/// A [`PeerConfirmer`] whose answer and call count are set up by the test. Counting calls is
/// what lets a test prove a *known* peer (matching or changed) never reaches a human at all —
/// only an unknown one should.
struct StubConfirmer {
    answer: bool,
    calls: AtomicUsize,
}

impl StubConfirmer {
    fn answering(answer: bool) -> StubConfirmer {
        StubConfirmer { answer, calls: AtomicUsize::new(0) }
    }
}

#[async_trait::async_trait]
impl PeerConfirmer for StubConfirmer {
    async fn confirm(&self, _label: &str, _fingerprint: &str) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.answer
    }
}

#[tokio::test]
async fn authorize_passes_a_known_matching_key_without_asking() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    store.pin_preapproved("kitchen-pi", "key-1").await.unwrap();

    let confirmer = StubConfirmer::answering(true);
    let result = store.authorize(&confirmer, "kitchen-pi", "key-1").await;

    assert!(result.is_ok(), "got {result:?}");
    assert_eq!(
        confirmer.calls.load(Ordering::SeqCst),
        0,
        "a peer already pinned under the same key must not be confirmed again"
    );
}

#[tokio::test]
async fn authorize_refuses_a_known_changed_key_without_asking() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    store.pin_preapproved("kitchen-pi", "key-1").await.unwrap();

    // Answers `true` on purpose: even a confirmer that would say yes to anything must never
    // be given the chance here. A changed key on an already-pinned slug is exactly the
    // substitution TOFU exists to catch — it must be refused outright, not turned into a
    // prompt an attacker could get lucky on.
    let confirmer = StubConfirmer::answering(true);
    let result = store.authorize(&confirmer, "kitchen-pi", "key-2").await;

    assert!(matches!(result, Err(TofuError::Changed { .. })), "got {result:?}");
    assert_eq!(
        confirmer.calls.load(Ordering::SeqCst),
        0,
        "a known peer offering a different key must be refused without ever asking"
    );
    // And still not pinned as key-2 — the refusal must not have side effects.
    assert!(matches!(
        store.check("kitchen-pi", "key-2").await,
        Err(TofuError::Changed { .. })
    ));
}

#[tokio::test]
async fn authorize_asks_once_for_an_unknown_peer_and_does_not_pin_on_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    let offered = fresh_endpoint_id();

    let confirmer = StubConfirmer::answering(false);
    let result = store.authorize(&confirmer, "kitchen-pi", &offered).await;

    assert!(matches!(result, Err(TofuError::Refused { .. })), "got {result:?}");
    assert_eq!(confirmer.calls.load(Ordering::SeqCst), 1, "an unknown peer must be asked");
    // Refused, not pinned: the peer must still read as unknown afterward.
    assert!(
        store.check("kitchen-pi", "anything-at-all").await.is_ok(),
        "a refused peer must not have been pinned"
    );
}

#[tokio::test]
async fn authorize_pins_an_unknown_peer_on_acceptance() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    let offered = fresh_endpoint_id();

    let confirmer = StubConfirmer::answering(true);
    let result = store.authorize(&confirmer, "kitchen-pi", &offered).await;

    assert!(result.is_ok(), "got {result:?}");
    assert_eq!(confirmer.calls.load(Ordering::SeqCst), 1);
    assert!(store.check("kitchen-pi", &offered).await.is_ok());
    assert!(matches!(
        store.check("kitchen-pi", "some-other-key").await,
        Err(TofuError::Changed { .. })
    ));
}

#[tokio::test]
async fn authorize_with_deny_unknown_refuses_every_unknown_peer() {
    // The carrying-item version of the above two: `DenyUnknown` is the fail-closed default a
    // node with no human uses, and it should behave exactly like a `StubConfirmer` that always
    // says no — refused, and nothing pinned.
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    let offered = fresh_endpoint_id();

    let result = store.authorize(&zyris_p2p::fingerprint::DenyUnknown, "kitchen-pi", &offered).await;

    assert!(matches!(result, Err(TofuError::Refused { .. })), "got {result:?}");
    assert!(store.check("kitchen-pi", "anything-at-all").await.is_ok());
}

/// Captures exactly what `authorize` hands to `confirm` — the coverage gap the review round 1
/// found: every other `authorize` test only checks *whether* the confirmer was called, never
/// *what it was shown*. `fingerprint(peer_slug)` in place of `fingerprint(endpoint_id)`, or a
/// hardcoded constant label, would have passed every test above without this one — a person
/// would be shown a fingerprint that matches nothing about the actual offered key, compare it
/// against itself, and approve a pin that vouches for nothing.
struct RecordingConfirmer {
    seen: std::sync::Mutex<Option<(String, String)>>,
    answer: bool,
}

impl RecordingConfirmer {
    fn answering(answer: bool) -> RecordingConfirmer {
        RecordingConfirmer { seen: std::sync::Mutex::new(None), answer }
    }
}

#[async_trait::async_trait]
impl PeerConfirmer for RecordingConfirmer {
    async fn confirm(&self, label: &str, fingerprint: &str) -> bool {
        *self.seen.lock().unwrap() = Some((label.to_string(), fingerprint.to_string()));
        self.answer
    }
}

#[tokio::test]
async fn authorize_shows_the_confirmer_the_offered_keys_fingerprint_and_the_slug_as_label() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    let offered = fresh_endpoint_id();

    let confirmer = RecordingConfirmer::answering(true);
    store.authorize(&confirmer, "kitchen-pi", &offered).await.unwrap();

    let (label, shown_fingerprint) =
        confirmer.seen.lock().unwrap().clone().expect("confirm was never called");
    assert_eq!(
        label, "kitchen-pi",
        "the label shown to a person must be the peer_slug, not anything else"
    );
    // Computed independently of whatever `authorize` did internally -- this is what actually
    // pins down that the *offered key*, not the slug or a constant, is what got hashed.
    assert_eq!(
        shown_fingerprint,
        fingerprint(&offered).unwrap(),
        "the fingerprint shown to a person must be derived from the offered endpoint_id"
    );
}

#[tokio::test]
async fn authorize_fails_closed_on_a_corrupt_ledger_instead_of_prompting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.json");
    let store = TofuStore::new(&path);
    let pinned_key = fresh_endpoint_id();
    let offered_key = fresh_endpoint_id();
    store.pin_preapproved("kitchen-pi", &pinned_key).await.unwrap();

    // The scenario the review flagged directly: an already-pinned slug, a corrupt ledger, and
    // a different offered key. Treating an unreadable ledger as "no pins ever taken" here would
    // turn a changed key on an already-pinned slug into a *prompt* -- exactly the substitution
    // TOFU exists to refuse outright, turned into something a confused or rushed human could
    // wave through.
    tokio::fs::write(&path, b"{ this is not json").await.unwrap();

    let confirmer = StubConfirmer::answering(true);
    let result = store.authorize(&confirmer, "kitchen-pi", &offered_key).await;

    assert!(matches!(result, Err(TofuError::Malformed { .. })), "got {result:?}");
    assert_eq!(
        confirmer.calls.load(Ordering::SeqCst),
        0,
        "a corrupt ledger must fail closed, never fall through to asking a human"
    );
}

/// The confirmer itself pins a *different* key for the same slug while it is being asked —
/// standing in for a second connection winning a race against a human still reading a
/// fingerprint. This is only possible at all if `authorize` is not holding the in-process
/// write lock while `confirm` runs: `TofuStore::pin_preapproved` takes that same lock (via the
/// shared `store.clone()` below), and `tokio::sync::Mutex` is not reentrant, so a lock held
/// across `confirm` would make this nested `pin_preapproved` call hang forever rather than
/// complete — which the outer `timeout` turns into a clean test failure instead of a wedged
/// test suite.
#[tokio::test]
async fn authorize_does_not_hold_the_lock_while_waiting_on_a_confirmer() {
    struct RacingConfirmer {
        store: TofuStore,
    }

    #[async_trait::async_trait]
    impl PeerConfirmer for RacingConfirmer {
        async fn confirm(&self, _label: &str, _fingerprint: &str) -> bool {
            self.store.pin_preapproved("kitchen-pi", "key-2").await.unwrap();
            true
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    let confirmer = RacingConfirmer { store: store.clone() };
    let offered = fresh_endpoint_id();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        store.authorize(&confirmer, "kitchen-pi", &offered),
    )
    .await
    .expect(
        "authorize hung waiting on its own lock — confirm must not run while a lock is held",
    );

    // Our key lost the race: the confirmer's own pin landed key-2 first, so
    // `pin_preapproved`'s re-check inside `authorize` must catch the mismatch instead of
    // overwriting it.
    assert!(matches!(result, Err(TofuError::Changed { ref pinned, .. }) if pinned == "key-2"), "got {result:?}");
}

/// Same race, but through the `<ledger>.lock` *file* instead of the in-process mutex: the
/// confirmer uses a fresh `TofuStore` over the same path (not a clone), so this exercises the
/// lock that actually matters across processes — the one the module docs say a 60-second-long
/// human absolutely cannot be waited out under. A short `with_lock_timeout` on the racing store
/// means a mutation that holds this lock across `confirm` surfaces as a quick `Err` here rather
/// than a multi-second hang.
#[tokio::test]
async fn authorize_does_not_hold_the_file_lock_while_waiting_on_a_confirmer() {
    struct RacingConfirmer {
        path: std::path::PathBuf,
    }

    #[async_trait::async_trait]
    impl PeerConfirmer for RacingConfirmer {
        async fn confirm(&self, _label: &str, _fingerprint: &str) -> bool {
            let racer = TofuStore::new(&self.path)
                .with_lock_timeout(std::time::Duration::from_millis(500));
            racer.pin_preapproved("kitchen-pi", "key-2").await.unwrap_or_else(|e| {
                panic!("a concurrent pin from a second process could not take the lock file: {e}")
            });
            true
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.json");
    let store = TofuStore::new(&path);
    let confirmer = RacingConfirmer { path };
    let offered = fresh_endpoint_id();

    let result = store.authorize(&confirmer, "kitchen-pi", &offered).await;
    assert!(matches!(result, Err(TofuError::Changed { ref pinned, .. }) if pinned == "key-2"), "got {result:?}");
}
