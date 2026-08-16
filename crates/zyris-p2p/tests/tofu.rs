use std::sync::atomic::{AtomicUsize, Ordering};

use zyris_p2p::fingerprint::{fingerprint, PeerConfirmer};
use zyris_p2p::tofu::{TofuError, TofuStore};

/// A fresh, genuinely valid `EndpointId` string. `check`, `authorize`, and `pin_preapproved`
/// all parse and canonicalize their `endpoint_id` argument before doing anything else with it
/// (see `tofu.rs`'s `canonical_endpoint_id`), so placeholder strings like `"key-1"` no longer
/// work anywhere in this file — every `endpoint_id` position needs something that genuinely
/// parses as an `iroh::EndpointId`. `peer_slug` positions (e.g. `"kitchen-pi"`) are unaffected;
/// slugs are never parsed.
fn fresh_endpoint_id() -> String {
    iroh::SecretKey::generate().public().to_string()
}

#[tokio::test]
async fn an_unknown_peer_passes() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    let key = fresh_endpoint_id();
    assert!(store.check("kitchen-pi", &key).await.is_ok());
}

#[tokio::test]
async fn the_same_key_passes_after_pinning() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    let key = fresh_endpoint_id();
    store.pin_preapproved("kitchen-pi", &key).await.unwrap();
    assert!(store.check("kitchen-pi", &key).await.is_ok());
}

#[tokio::test]
async fn a_changed_key_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    let key1 = fresh_endpoint_id();
    let key2 = fresh_endpoint_id();
    store.pin_preapproved("kitchen-pi", &key1).await.unwrap();

    match store.check("kitchen-pi", &key2).await {
        Err(TofuError::Changed { pinned, offered }) => {
            assert_eq!(pinned, key1);
            assert_eq!(offered, key2);
        }
        other => panic!("a changed key must be refused, got {other:?}"),
    }
}

#[tokio::test]
async fn the_pin_survives_a_new_store_instance() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.json");
    let key1 = fresh_endpoint_id();
    let key2 = fresh_endpoint_id();
    TofuStore::new(&path).pin_preapproved("kitchen-pi", &key1).await.unwrap();

    // A fresh instance: this has to come off disk.
    let result = TofuStore::new(&path).check("kitchen-pi", &key2).await;
    assert!(matches!(result, Err(TofuError::Changed { .. })));
}

#[tokio::test]
async fn pinning_twice_keeps_the_first_key() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    let key1 = fresh_endpoint_id();
    let key2 = fresh_endpoint_id();
    store.pin_preapproved("kitchen-pi", &key1).await.unwrap();

    // `pin_preapproved` keeps the first value. If a later call could overwrite it, pinning
    // would mean nothing — the substitution we are trying to catch would pin itself. The
    // second call must also *report* the mismatch, not just refuse to act on it — a caller
    // that pins without calling `check` first must still learn a substitution was attempted.
    let result = store.pin_preapproved("kitchen-pi", &key2).await;
    match result {
        Err(TofuError::Changed { pinned, offered }) => {
            assert_eq!(pinned, key1);
            assert_eq!(offered, key2);
        }
        other => panic!("a re-pin with a different key must be reported, got {other:?}"),
    }

    assert!(matches!(store.check("kitchen-pi", &key2).await, Err(TofuError::Changed { .. })));
}

#[tokio::test]
async fn re_pinning_the_identical_key_is_a_silent_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    let key = fresh_endpoint_id();
    store.pin_preapproved("kitchen-pi", &key).await.unwrap();

    // Only a re-pin of the *same* key is a true no-op — this is not a substitution attempt,
    // so it must not be reported as one.
    assert!(store.pin_preapproved("kitchen-pi", &key).await.is_ok());
    assert!(store.check("kitchen-pi", &key).await.is_ok());
}

#[tokio::test]
async fn a_corrupt_pin_file_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.json");
    let pinned_key = fresh_endpoint_id();
    let other_key = fresh_endpoint_id();

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
        store.pin_preapproved("kitchen-pi", &pinned_key).await.unwrap();
        tokio::fs::write(&path, content).await.unwrap();

        // Treating an unreadable ledger as "nothing is pinned" would let anyone erase every
        // pin by corrupting one file — which is exactly what you would do right before
        // swapping a key.
        let result = store.check("kitchen-pi", &other_key).await;
        assert!(matches!(result, Err(TofuError::Malformed { .. })), "{name}: got {result:?}");
        // Even the peer that IS pinned correctly must not slip through while we cannot read.
        let same = store.check("kitchen-pi", &pinned_key).await;
        assert!(matches!(same, Err(TofuError::Malformed { .. })), "{name}: got {same:?}");

        tokio::fs::remove_file(&path).await.unwrap();
    }
}

#[tokio::test]
async fn pinning_a_second_peer_keeps_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    let key_a = fresh_endpoint_id();
    let key_b = fresh_endpoint_id();
    let other = fresh_endpoint_id();
    store.pin_preapproved("garage-cam", &key_a).await.unwrap();
    store.pin_preapproved("kitchen-pi", &key_b).await.unwrap();

    assert!(matches!(store.check("garage-cam", &other).await, Err(TofuError::Changed { .. })));
    assert!(matches!(store.check("kitchen-pi", &other).await, Err(TofuError::Changed { .. })));
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
            let key = fresh_endpoint_id();
            tasks.push(tokio::spawn(async move {
                store.pin_preapproved(&format!("gadget-{i}"), &key).await.unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }

        let store = TofuStore::new(&path);
        let someone_else = fresh_endpoint_id();
        // A lost pin is not a lost log line: that peer is "unknown" again, so the next key
        // change for it passes unnoticed.
        for i in 0..16 {
            let result = store.check(&format!("gadget-{i}"), &someone_else).await;
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
    let key = fresh_endpoint_id();

    store.pin_preapproved("kitchen-pi", &key).await.unwrap();
    assert!(store.check("kitchen-pi", &key).await.is_ok());

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
    let key = fresh_endpoint_id();
    // A lock left behind by a dead writer (killed, crashed, OOM-reaped) must not block every
    // later pin forever — this machine runs earlyoom and systemd-oomd for exactly that
    // reason, so it is not hypothetical here.
    store.pin_preapproved("kitchen-pi", &key).await.unwrap();
    assert!(store.check("kitchen-pi", &key).await.is_ok());
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
    let key = fresh_endpoint_id();
    // A live writer's lock must never be stolen: this has to fail loudly, not proceed as if
    // no one held it and not silently drop the pin. A genuinely valid key is used here so this
    // fails at lock acquisition specifically, not at the (separate) input-validation step.
    let result = store.pin_preapproved("kitchen-pi", &key).await;
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
    let key = fresh_endpoint_id();
    store.pin_preapproved("kitchen-pi", &key).await.unwrap();

    let confirmer = StubConfirmer::answering(true);
    let result = store.authorize(&confirmer, "kitchen-pi", &key).await;

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
    let key1 = fresh_endpoint_id();
    let key2 = fresh_endpoint_id();
    store.pin_preapproved("kitchen-pi", &key1).await.unwrap();

    // Answers `true` on purpose: even a confirmer that would say yes to anything must never
    // be given the chance here. A changed key on an already-pinned slug is exactly the
    // substitution TOFU exists to catch — it must be refused outright, not turned into a
    // prompt an attacker could get lucky on.
    let confirmer = StubConfirmer::answering(true);
    let result = store.authorize(&confirmer, "kitchen-pi", &key2).await;

    assert!(matches!(result, Err(TofuError::Changed { .. })), "got {result:?}");
    assert_eq!(
        confirmer.calls.load(Ordering::SeqCst),
        0,
        "a known peer offering a different key must be refused without ever asking"
    );
    // And still not pinned as key2 — the refusal must not have side effects.
    assert!(matches!(store.check("kitchen-pi", &key2).await, Err(TofuError::Changed { .. })));
}

#[tokio::test]
async fn authorize_asks_once_for_an_unknown_peer_and_does_not_pin_on_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    let offered = fresh_endpoint_id();
    let anything = fresh_endpoint_id();

    let confirmer = StubConfirmer::answering(false);
    let result = store.authorize(&confirmer, "kitchen-pi", &offered).await;

    assert!(matches!(result, Err(TofuError::Refused { .. })), "got {result:?}");
    assert_eq!(confirmer.calls.load(Ordering::SeqCst), 1, "an unknown peer must be asked");
    // Refused, not pinned: the peer must still read as unknown afterward.
    assert!(
        store.check("kitchen-pi", &anything).await.is_ok(),
        "a refused peer must not have been pinned"
    );
}

#[tokio::test]
async fn authorize_pins_an_unknown_peer_on_acceptance() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    let offered = fresh_endpoint_id();
    let some_other_key = fresh_endpoint_id();

    let confirmer = StubConfirmer::answering(true);
    let result = store.authorize(&confirmer, "kitchen-pi", &offered).await;

    assert!(result.is_ok(), "got {result:?}");
    assert_eq!(confirmer.calls.load(Ordering::SeqCst), 1);
    assert!(store.check("kitchen-pi", &offered).await.is_ok());
    assert!(matches!(
        store.check("kitchen-pi", &some_other_key).await,
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
    let anything = fresh_endpoint_id();

    let result = store.authorize(&zyris_p2p::fingerprint::DenyUnknown, "kitchen-pi", &offered).await;

    assert!(matches!(result, Err(TofuError::Refused { .. })), "got {result:?}");
    assert!(store.check("kitchen-pi", &anything).await.is_ok());
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

/// A person must never be shown a fingerprint (or asked to approve a pin) rendered from
/// something that was not even a valid key — the guarantee `fingerprint.rs`'s module doc
/// states (`InvalidEndpointId`'s own doc: "returned instead of a fingerprint"), but which
/// nothing directly held `authorize` to before this test. A stub that always accepts is used
/// deliberately: if this ever regresses, the failure must be "the confirmer got asked and said
/// yes to garbage," not "the confirmer happened to say no" masking the real problem.
#[tokio::test]
async fn authorize_refuses_an_unparseable_endpoint_id_without_asking_or_pinning() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));

    let confirmer = StubConfirmer::answering(true);
    let result = store.authorize(&confirmer, "kitchen-pi", "not-a-key").await;

    assert!(matches!(result, Err(TofuError::InvalidEndpointId(_))), "got {result:?}");
    assert_eq!(
        confirmer.calls.load(Ordering::SeqCst),
        0,
        "an unparseable endpoint_id must be rejected before a person is ever shown anything"
    );
    // Nothing pinned under this slug, valid or not.
    let anything = fresh_endpoint_id();
    assert!(store.check("kitchen-pi", &anything).await.is_ok());
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
        racing_key: String,
    }

    #[async_trait::async_trait]
    impl PeerConfirmer for RacingConfirmer {
        async fn confirm(&self, _label: &str, _fingerprint: &str) -> bool {
            self.store.pin_preapproved("kitchen-pi", &self.racing_key).await.unwrap();
            true
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    let racing_key = fresh_endpoint_id();
    let confirmer = RacingConfirmer { store: store.clone(), racing_key: racing_key.clone() };
    let offered = fresh_endpoint_id();

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        store.authorize(&confirmer, "kitchen-pi", &offered),
    )
    .await
    .expect(
        "authorize hung waiting on its own lock — confirm must not run while a lock is held",
    );

    // Our key lost the race: the confirmer's own pin landed `racing_key` first, so
    // `pin_preapproved`'s re-check inside `authorize` must catch the mismatch instead of
    // overwriting it.
    assert!(matches!(result, Err(TofuError::Changed { ref pinned, .. }) if *pinned == racing_key), "got {result:?}");
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
        racing_key: String,
    }

    #[async_trait::async_trait]
    impl PeerConfirmer for RacingConfirmer {
        async fn confirm(&self, _label: &str, _fingerprint: &str) -> bool {
            let racer = TofuStore::new(&self.path)
                .with_lock_timeout(std::time::Duration::from_millis(500));
            racer.pin_preapproved("kitchen-pi", &self.racing_key).await.unwrap_or_else(|e| {
                panic!("a concurrent pin from a second process could not take the lock file: {e}")
            });
            true
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.json");
    let store = TofuStore::new(&path);
    let racing_key = fresh_endpoint_id();
    let confirmer = RacingConfirmer { path, racing_key: racing_key.clone() };
    let offered = fresh_endpoint_id();

    let result = store.authorize(&confirmer, "kitchen-pi", &offered).await;
    assert!(matches!(result, Err(TofuError::Changed { ref pinned, .. }) if *pinned == racing_key), "got {result:?}");
}

/// N2's regression: pinning one accepted spelling of a key and then authorizing (or checking)
/// with a *different* accepted spelling of the identical key must read as the same key, not a
/// substitution. Exercises both directions the review asked for in one test — the cross-
/// spelling case must be `Ok`, and a genuinely different key must still be `Err(Changed)`.
#[tokio::test]
async fn pinning_in_one_spelling_and_authorizing_in_another_is_ok_but_a_different_key_still_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));

    let key = iroh::SecretKey::generate().public();
    let hex_spelling = key.to_string();
    let base32_spelling = base32_nopad(key.as_bytes());
    // Sanity check on the test's own setup: confirm this really is a second, independently
    // parseable spelling of the same key before relying on it below.
    assert_eq!(base32_spelling.parse::<iroh::EndpointId>().unwrap(), key);

    store.pin_preapproved("kitchen-pi", &hex_spelling).await.unwrap();

    // Cross-spelling: must be Ok, not Changed.
    let confirmer = StubConfirmer::answering(true);
    let result = store.authorize(&confirmer, "kitchen-pi", &base32_spelling).await;
    assert!(result.is_ok(), "the same key spelled differently must not read as a change: {result:?}");
    assert_eq!(
        confirmer.calls.load(Ordering::SeqCst),
        0,
        "a key that canonicalizes to what is already pinned must not be confirmed again"
    );
    assert!(store.check("kitchen-pi", &base32_spelling).await.is_ok());

    // A genuinely different key, offered in yet another spelling, must still be refused.
    let other_key = fresh_endpoint_id();
    assert!(matches!(
        store.check("kitchen-pi", &other_key).await,
        Err(TofuError::Changed { .. })
    ));
}

/// `pin_preapproved`'s own doc recommends calling it directly for a provisioning step that
/// already trusts a fingerprint (no `authorize`/no confirmer in the loop at all) — so its own
/// canonicalization gate has to hold on that path independent of `authorize`'s. Round 2's
/// review found the review-round-1-style `authorize` test does not exercise this: deleting
/// `pin_preapproved`'s own `canonical_endpoint_id` call left 58/58 green, because nothing called
/// it directly with unparseable input.
#[tokio::test]
async fn pin_preapproved_rejects_unparseable_input_directly() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));

    let result = store.pin_preapproved("kitchen-pi", "not-a-key").await;
    assert!(matches!(result, Err(TofuError::InvalidEndpointId(_))), "got {result:?}");

    // Nothing pinned under this slug — a rejected pin must have no side effects.
    let anything = fresh_endpoint_id();
    assert!(store.check("kitchen-pi", &anything).await.is_ok(), "garbage must not have been pinned");
}

/// The other half of the same gate: `pin_preapproved` must canonicalize what it *does* accept,
/// not just reject what it does not. Pins directly (not through `authorize`) using one
/// accepted spelling and checks with another — this is the write-side mirror of
/// `pinning_in_one_spelling_and_authorizing_in_another_is_ok_but_a_different_key_still_refuses`
/// above, which only ever pins via the hex spelling.
#[tokio::test]
async fn pin_preapproved_canonicalizes_so_a_later_check_in_another_spelling_still_matches() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));

    let key = iroh::SecretKey::generate().public();
    let base32_spelling = base32_nopad(key.as_bytes());
    let hex_spelling = key.to_string();

    store.pin_preapproved("kitchen-pi", &base32_spelling).await.unwrap();
    assert!(store.check("kitchen-pi", &hex_spelling).await.is_ok());
}

/// **Hand-editing the ledger is the only documented way to un-pin a peer** (see `tofu.rs`'s
/// module docs). A human doing exactly that — deleting a stale entry and pasting the real key
/// back in from wherever they copied it — has no reason to paste it in this store's own
/// canonical (hex) form. A ledger written by hand in a different, still-valid spelling must
/// behave identically to one in canonical form: `check`ing or `authorize`ing with the
/// canonical spelling of the same key must read as already pinned, not as an unknown peer and
/// not as `Changed`. Constructs the ledger file directly (not via `pin_preapproved`) to
/// simulate exactly this hand-edit, bypassing this store's own write path entirely.
#[tokio::test]
async fn a_hand_edited_ledger_in_a_non_canonical_spelling_reads_the_same_as_canonical() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.json");

    let key = iroh::SecretKey::generate().public();
    let base32_spelling = base32_nopad(key.as_bytes());
    let hex_spelling = key.to_string();
    assert_ne!(
        base32_spelling, hex_spelling,
        "test setup: need two textually different spellings of the same key"
    );

    let contents = format!(
        r#"{{"peers":{{"kitchen-pi":{{"endpoint_id":"{base32_spelling}","first_seen_ms":0}}}}}}"#
    );
    tokio::fs::write(&path, contents).await.unwrap();

    let store = TofuStore::new(&path);
    assert!(
        store.check("kitchen-pi", &hex_spelling).await.is_ok(),
        "a hand-edited non-canonical entry must read as matching the same key in canonical form"
    );

    let confirmer = StubConfirmer::answering(true);
    let result = store.authorize(&confirmer, "kitchen-pi", &hex_spelling).await;
    assert!(result.is_ok(), "got {result:?}");
    assert_eq!(
        confirmer.calls.load(Ordering::SeqCst),
        0,
        "a hand-edited entry in a non-canonical spelling must read as already pinned, not unknown"
    );
}

/// Minimal RFC 4648 base32 (no padding) encoder — just enough to construct a second, valid
/// spelling of an `EndpointId` for the test above, matching what `iroh::EndpointId::from_str`'s
/// non-hex branch actually decodes (`decode_base32_hex` in `iroh-base`, which uppercases before
#[tokio::test]
async fn forgetting_a_pin_makes_the_next_key_a_first_sight_again() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    let old = fresh_endpoint_id();
    let new = fresh_endpoint_id();
    store.pin_preapproved("kitchen-pi", &old).await.unwrap();
    assert!(matches!(store.check("kitchen-pi", &new).await, Err(TofuError::Changed { .. })));

    assert!(store.forget("kitchen-pi").await.unwrap(), "there was a pin to remove");

    // The point of the operation, and the reason it is not automatic: the refusal is gone, and
    // whatever key turns up next is what gets pinned.
    assert!(store.check("kitchen-pi", &new).await.is_ok());
}

/// Asking to forget something that is not pinned is already true, so it is not an error — but the
/// answer has to say which of the two happened, or a caller cannot tell a person "there was no
/// pin for that" from "removed it".
#[tokio::test]
async fn forgetting_what_was_never_pinned_says_so_without_failing() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    assert!(!store.forget("never-seen").await.unwrap());
}

#[tokio::test]
async fn forgetting_one_peer_leaves_the_others_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    let pi = fresh_endpoint_id();
    let laptop = fresh_endpoint_id();
    store.pin_preapproved("kitchen-pi", &pi).await.unwrap();
    store.pin_preapproved("laptop", &laptop).await.unwrap();

    store.forget("kitchen-pi").await.unwrap();

    assert!(store.check("kitchen-pi", &fresh_endpoint_id()).await.is_ok(), "kitchen-pi is free");
    assert!(
        matches!(store.check("laptop", &fresh_endpoint_id()).await, Err(TofuError::Changed { .. })),
        "laptop's pin was collateral damage"
    );
}

/// It has to come off disk, not out of an in-memory copy — `forget` is the operation most likely
/// to be run from a separate process (a CLI) while the node itself is running.
#[tokio::test]
async fn forgetting_reaches_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("peers.json");
    let key = fresh_endpoint_id();
    TofuStore::new(&path).pin_preapproved("kitchen-pi", &key).await.unwrap();

    TofuStore::new(&path).forget("kitchen-pi").await.unwrap();

    assert!(TofuStore::new(&path).check("kitchen-pi", &fresh_endpoint_id()).await.is_ok());
}

#[tokio::test]
async fn pins_lists_what_is_trusted_sorted_by_slug() {
    let dir = tempfile::tempdir().unwrap();
    let store = TofuStore::new(dir.path().join("peers.json"));
    assert!(store.pins().await.unwrap().is_empty(), "nothing is trusted before anything is pinned");

    let pi = fresh_endpoint_id();
    let laptop = fresh_endpoint_id();
    store.pin_preapproved("kitchen-pi", &pi).await.unwrap();
    store.pin_preapproved("laptop", &laptop).await.unwrap();

    // Sorted, so the output a person reads does not reorder itself between runs.
    assert_eq!(
        store.pins().await.unwrap(),
        vec![("kitchen-pi".to_string(), pi), ("laptop".to_string(), laptop)]
    );
}

/// decoding, so the alphabet's case here does not matter to parsing). Not general-purpose —
/// self-contained on purpose, to avoid pulling in `data_encoding` as a dependency for one test.
fn base32_nopad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let mut buf: u64 = 0;
    let mut bits = 0u32;
    for &b in bytes {
        buf = (buf << 8) | b as u64;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buf >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buf << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}
