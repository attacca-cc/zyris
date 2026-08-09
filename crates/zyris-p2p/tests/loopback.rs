//! Boots two iroh endpoints in one process and connects them **for real**. No relay involved —
//! it dials over loopback, so this runs in CI.

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zyris::transport::Transport;
use zyris::{AcceptOptions, Node, NodeKind};
use zyris_proto::WireMessage;

/// Every test in this file runs inside this. A regression in the handshake (this feature has
/// already produced two real hangs — a dropped `Endpoint` and, before that, a wrong theory
/// chased for hours) should fail the test suite in seconds, not wedge CI indefinitely.
const TEST_DEADLINE: Duration = Duration::from_secs(20);

/// Bound for turning an accepted-but-unestablished connection into a usable one. Generous for a
/// same-process loopback handshake, but still finite — see `peer::establish`.
const ESTABLISH_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Echoed {
    pub said: String,
}

#[zyris::capability(name = "echo", version = 1)]
pub trait Echo {
    async fn say(&self, text: String) -> zyris::Result<Echoed>;
}

struct EchoImpl;

#[async_trait::async_trait]
impl Echo for EchoImpl {
    async fn say(&self, text: String) -> zyris::Result<Echoed> {
        Ok(Echoed { said: text })
    }
}

/// Both tests share this wiring: bind two endpoints, have B `accept_next` + `establish` and
/// serve `Echo`, have A `dial` and `connect_over`. Returns `(a_conn, b_conn, b_endpoint_id,
/// a_ep, b_ep)` so a caller that needs the peer id A saw, or the endpoint handles, does not
/// have to duplicate the setup.
///
/// The trailing `a_ep`/`b_ep` are not decorative: `iroh::Endpoint` is `Arc`-backed, and dropping
/// its last clone aborts the underlying socket ungracefully out from under every `Connection` it
/// produced — even ones still in active use. `IrohTransport` keeps its own clone alive for as
/// long as the connection itself lives (see `transport.rs`), which is the production fix, but
/// the caller's own endpoint handle is a separate value with its own lifetime, and this test
/// used to drop it: `b_ep` was moved into `b_task`'s closure and died the moment that task
/// returned, well before the test finished using the connection it produced. Returning both
/// endpoints keeps them alive for the caller's whole test by default — see
/// `connection_survives_dropping_every_local_endpoint_handle` below for the test that
/// deliberately does *not* keep them alive, which is what actually exercises the production
/// fix rather than just avoiding the bug it fixes.
async fn connect_pair(
) -> (zyris::Connection, zyris::Connection, iroh::EndpointId, iroh::Endpoint, iroh::Endpoint) {
    let a_key = iroh::SecretKey::generate();
    let b_key = iroh::SecretKey::generate();
    let a_id = a_key.public();

    let b_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(b_key.clone())
        .alpns(vec![zyris_p2p::transport::ALPN.to_vec()])
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let b_addr = b_ep.addr();
    let b_ep_keep = b_ep.clone();

    // B: accept and stand up the acceptor-side Connection. `accept_next` only waits for iroh's
    // connection attempt; `establish` is the part with a deadline that can actually block on
    // the peer, matching how a real accept loop would spawn it per connection.
    let b_task = tokio::spawn(async move {
        let pending = zyris_p2p::peer::accept_next(&b_ep).await.unwrap();
        let (peer, transport) =
            zyris_p2p::peer::establish(pending, ESTABLISH_DEADLINE).await.unwrap();
        let node = Node::builder()
            .name("b")
            .kind(NodeKind::Cli)
            .capability(EchoServer(EchoImpl))
            .build()
            .unwrap();
        // Do not use `AcceptOptions::default()` — see the warning in the task-2.5 brief's
        // "constraints" section. Leaving it at the default here would let a wiring mistake go
        // undetected until it hit live traffic. `connect_pair` below asserts that A actually
        // observes this node_id, so swapping this back to `::default()` fails the test instead
        // of merely being obeyed and never checked.
        let opts = AcceptOptions { node_id: "b".into(), ..AcceptOptions::default() };
        let conn = node.accept(transport, opts).await.unwrap();
        (peer, conn)
    });

    let a_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(a_key)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind()
        .await
        .unwrap();
    let transport = zyris_p2p::peer::dial(&a_ep, b_addr).await.unwrap();
    let a_node = Node::builder().name("a").kind(NodeKind::Cli).build().unwrap();

    // Run both sides' handshake concurrently rather than awaiting A first: `accept_next`'s
    // peer id and B's half of `establish` do not depend on A ever finishing `connect_over`,
    // so joining lets that assertion run (and a mutation there get caught) independently of
    // whichever side, if either, fails.
    let (a_result, b_result) = tokio::join!(a_node.connect_over(transport), b_task);
    let (peer_seen, b_conn) = b_result.unwrap();
    assert_eq!(peer_seen, a_id, "B must learn A's EndpointId");
    let a_conn = a_result.unwrap();
    // The AcceptOptions constraint above is only real if A actually observes it. `HelloAck`
    // carries B's `node_id` back to A, so this is the identity A learned from the wire, not a
    // local echo of what B configured — swapping `AcceptOptions::default()` in above changes
    // this to a random UUID and fails here.
    assert_eq!(a_conn.info().node_id, "b", "A must observe B's configured node_id");

    (a_conn, b_conn, peer_seen, a_ep, b_ep_keep)
}

#[tokio::test]
async fn two_endpoints_connect_and_speak_zyris() {
    tokio::time::timeout(TEST_DEADLINE, async {
        let (a_conn, _b_conn, _peer, _a_ep, _b_ep) = connect_pair().await;

        let echo: EchoClient = a_conn.wait_capability(Duration::from_secs(5)).await.unwrap();
        let reply = echo.say("hello".into()).await.unwrap();
        assert_eq!(reply.said, "hello");
    })
    .await
    .expect("test exceeded its deadline");
}

#[tokio::test]
async fn a_large_message_round_trips_whole() {
    tokio::time::timeout(TEST_DEADLINE, async {
        // Checks that framing does not break across a chunk boundary. QUIC is a byte stream, so
        // a single `read_exact` does not necessarily get everything at once — the header can
        // arrive split across reads, and the body almost certainly will for a message this
        // size.
        let (a_conn, _b_conn, _peer, _a_ep, _b_ep) = connect_pair().await;
        let echo: EchoClient = a_conn.wait_capability(Duration::from_secs(5)).await.unwrap();

        let long_text = "가".repeat(300_000); // 900,000 bytes in UTF-8
        assert!(long_text.len() > 512 * 1024, "must exceed one QUIC read");
        let reply = echo.say(long_text.clone()).await.unwrap();
        assert_eq!(reply.said.len(), long_text.len(), "length must survive the round trip");
        assert_eq!(reply.said, long_text, "content must survive the round trip");
    })
    .await
    .expect("test exceeded its deadline");
}

#[tokio::test]
async fn connection_survives_dropping_every_local_endpoint_handle() {
    tokio::time::timeout(TEST_DEADLINE, async {
        // Regression test for the fix in `transport.rs`: `IrohTransport`/`IrohSink`/`IrohRead`
        // each hold their own clone of the owning `iroh::Endpoint` specifically so a caller
        // that drops its own endpoint handle after establishing a connection cannot silently
        // lose that connection. The other two tests above keep `_a_ep`/`_b_ep` alive for their
        // whole body, which means they would still pass even if that production-level clone
        // were deleted entirely — they exercise the bug's *absence*, not the fix. This test
        // drops every endpoint handle the caller holds before doing anything else, so the only
        // clones left alive afterwards are the ones the transport itself is holding — if those
        // were removed, this specific test is the one that would hang or error.
        let (a_conn, _b_conn, _peer, a_ep, b_ep) = connect_pair().await;
        drop(a_ep);
        drop(b_ep);

        let echo: EchoClient = a_conn.wait_capability(Duration::from_secs(5)).await.unwrap();
        let reply = echo.say("still here".into()).await.unwrap();
        assert_eq!(reply.said, "still here");
    })
    .await
    .expect("test exceeded its deadline");
}

#[tokio::test]
async fn close_after_send_does_not_drop_the_last_message() {
    tokio::time::timeout(TEST_DEADLINE, async {
        // Regression for the C1 fix in `transport.rs`: `IrohSink::close` used to call
        // `send.finish()` immediately followed by `Connection::close`, which discards whatever
        // hadn't been acknowledged yet — on this exact shape (a large message, then close
        // right away) that dropped a 900 KB frame entirely. This goes straight at
        // `IrohSink`/`IrohRead` (skipping the zyris envelope handshake, which is a separate
        // layer above this one) so the send-then-close race is deterministic instead of having
        // to win a race against an RPC reply.
        let b_key = iroh::SecretKey::generate();
        let b_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(b_key)
            .alpns(vec![zyris_p2p::transport::ALPN.to_vec()])
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let b_addr = b_ep.addr();

        let b_task = tokio::spawn(async move {
            let pending = zyris_p2p::peer::accept_next(&b_ep).await.unwrap();
            zyris_p2p::peer::establish(pending, ESTABLISH_DEADLINE).await.unwrap()
        });

        let a_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let a_transport = zyris_p2p::peer::dial(&a_ep, b_addr).await.unwrap();
        let (mut a_sink, _a_read) = Box::new(a_transport).split();

        // The dialer has to write first: `accept_bi()` on B's side (inside `establish` above)
        // only returns once bytes have actually arrived on the stream A just opened, so send
        // (and close) before awaiting `b_task` — awaiting it first would deadlock both sides
        // waiting on each other.
        let big = vec![0xABu8; 900_000];
        a_sink.send(WireMessage::Binary(big.clone().into())).await.unwrap();
        a_sink.close(0, "done".into()).await.unwrap();

        let (_peer, b_transport) = b_task.await.unwrap();
        let (_b_sink, mut b_read) = Box::new(b_transport).split();

        let received =
            b_read.next().await.expect("the message must arrive, not be silently dropped");
        match received.expect("must arrive as a message, not a transport error") {
            WireMessage::Binary(bytes) => {
                assert_eq!(bytes.len(), big.len(), "the tail of the message must not be dropped");
                assert_eq!(bytes.as_ref(), big.as_slice(), "content must survive close");
            }
            other => panic!("expected Binary, got {other:?}"),
        }
    })
    .await
    .expect("test exceeded its deadline");
}

#[tokio::test]
async fn close_does_not_hang_when_the_peer_vanishes_without_closing() {
    tokio::time::timeout(TEST_DEADLINE, async {
        // Regression for N1: the `tokio::time::timeout` around `stopped()` in `IrohSink::close`
        // had zero coverage — deleting it left all 33 tests green. It is reachable: a peer that
        // reads a frame and then simply disappears (crash, `kill -9`, a dropped process — not
        // the same as calling our own `close()`, which always sends a real `CONNECTION_CLOSE`)
        // never acknowledges the stream, so an unbounded `stopped()` would wait forever. Here B
        // reads A's message and then has every local handle to the connection *and* its
        // endpoint dropped without either side calling `close`/`finish` — there is nothing left
        // on B's side to ever send A anything, implicit or otherwise.
        let b_key = iroh::SecretKey::generate();
        let b_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(b_key)
            .alpns(vec![zyris_p2p::transport::ALPN.to_vec()])
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let b_addr = b_ep.addr();

        let b_task = tokio::spawn(async move {
            let pending = zyris_p2p::peer::accept_next(&b_ep).await.unwrap();
            let (_peer, b_transport) =
                zyris_p2p::peer::establish(pending, ESTABLISH_DEADLINE).await.unwrap();
            let (b_sink, mut b_read) = Box::new(b_transport).split();
            let received = b_read.next().await.unwrap().unwrap();
            // B vanishes here: both halves — and with them B's only remaining `Endpoint`
            // clones — drop at the end of this task without either side calling `close`.
            drop(b_sink);
            drop(b_read);
            received
        });

        let a_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let a_transport = zyris_p2p::peer::dial(&a_ep, b_addr).await.unwrap();
        let (mut a_sink, _a_read) = Box::new(a_transport).split();

        a_sink.send(WireMessage::Text("one frame, then silence".into())).await.unwrap();
        // Wait for B to have actually read it and fully vanished (the task's own return,
        // including every endpoint clone it held, is dropped by the time `.await` resolves)
        // before measuring `close`, so the test is not racing B's teardown.
        b_task.await.unwrap();

        let started = std::time::Instant::now();
        a_sink.close(0, "bye".into()).await.unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(6),
            "close must be bounded by CLOSE_STOPPED_WAIT even when the peer never \
             acknowledges, not hang indefinitely: took {elapsed:?}"
        );
    })
    .await
    .expect("test exceeded its deadline");
}

#[tokio::test]
async fn a_silent_peer_cannot_block_the_accept_loop() {
    tokio::time::timeout(TEST_DEADLINE, async {
        // Regression for the C2 fix in `peer.rs`: the original `accept_next` did the QUIC
        // handshake *and* `accept_bi()` inline, so a peer that connects and then sends nothing
        // never let it return — wedging the entire accept loop, since nothing else could be
        // accepted while it waited. This drives `accept_next`/`establish` the way a real accept
        // loop would: `establish` is spawned per connection with its own deadline immediately
        // after `accept_next` returns, never awaited inline. (`establish`'s own `incoming.await`
        // is what actually drives the QUIC handshake forward — a connecting peer's `connect()`
        // will not resolve until *something* awaits the `Incoming` it produced, which is why
        // this spawns immediately rather than after the connecting side finishes.)
        let b_key = iroh::SecretKey::generate();
        let b_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(b_key)
            .alpns(vec![zyris_p2p::transport::ALPN.to_vec()])
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let b_addr = b_ep.addr();
        let b_ep_for_loop = b_ep.clone();

        let accept_loop = tokio::spawn(async move {
            let mut established = Vec::new();
            for _ in 0..2u8 {
                let pending = zyris_p2p::peer::accept_next(&b_ep_for_loop).await.unwrap();
                established.push(tokio::spawn(async move {
                    let started = std::time::Instant::now();
                    let result =
                        zyris_p2p::peer::establish(pending, Duration::from_millis(300)).await;
                    (result, started.elapsed())
                }));
            }
            established
        });

        // Peer 1: connects and then goes silent — never opens the zyris bi-stream.
        let silent_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let silent_conn =
            silent_ep.connect(b_addr.clone(), zyris_p2p::transport::ALPN).await.unwrap();

        // Peer 2: a well-behaved peer that dials right after the silent one. If the accept loop
        // were wedged behind peer 1 (the bug `accept_next` used to have), this would never even
        // get accepted, let alone complete its own stream open.
        let good_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let good_transport = tokio::time::timeout(
            Duration::from_secs(3),
            zyris_p2p::peer::dial(&good_ep, b_addr),
        )
        .await
        .expect("a second, well-behaved peer must not be blocked by the silent one")
        .unwrap();
        // Dialer speaks first (same rule as everywhere else in this transport) — write
        // something so `establish`'s `accept_bi()` on B's side for peer 2 can return.
        let (mut good_sink, _good_read) = Box::new(good_transport).split();
        good_sink.send(WireMessage::Text("hi from peer 2".into())).await.unwrap();

        let mut results = tokio::time::timeout(Duration::from_secs(3), accept_loop)
            .await
            .expect("the accept loop itself must not hang")
            .unwrap();
        assert_eq!(results.len(), 2, "both connections must have been accepted");
        let (silent_result, silent_elapsed) =
            tokio::time::timeout(Duration::from_secs(1), results.remove(0)).await.unwrap().unwrap();
        let (good_result, _good_elapsed) =
            tokio::time::timeout(Duration::from_secs(1), results.remove(0)).await.unwrap().unwrap();

        assert!(
            matches!(silent_result, Err(zyris_p2p::peer::PeerError::Timeout)),
            "the silent peer must time out establish, not hang or succeed"
        );
        assert!(
            silent_elapsed < Duration::from_secs(2),
            "establish must give up at its own deadline, not linger: took {silent_elapsed:?}"
        );
        assert!(good_result.is_ok(), "the well-behaved peer must still establish successfully");

        // What happens on the wire once `establish` gives up on the silent peer: the timed-out
        // future drops its `Connection`/`Endpoint` clones (they were local to the cancelled
        // `establish_inner`), and `noq-1.1.1/src/connection.rs`'s `ConnectionRef::drop` — "If
        // the driver is alive, it's just it and us, so we'd better shut it down" — turns that
        // into `implicit_close(0, empty reason)`, i.e. a real `CONNECTION_CLOSE`, not a silent
        // leak. This is the peer-observable proof: the silent peer's own connection handle
        // must see it close, not linger unresolved.
        let silent_close_reason = tokio::time::timeout(Duration::from_secs(2), silent_conn.closed())
            .await
            .expect("the silent peer's connection must actually close, not hang unresolved");
        assert!(
            matches!(
                silent_close_reason,
                iroh::endpoint::ConnectionError::ApplicationClosed(_)
            ),
            "expected the implicit close from the timed-out establish, got {silent_close_reason:?}"
        );
    })
    .await
    .expect("test exceeded its deadline");
}

#[tokio::test]
async fn dropping_a_pending_connection_without_establishing_rejects_the_peer() {
    tokio::time::timeout(TEST_DEADLINE, async {
        // What happens to a connection whose `PendingConnection` is dropped without ever being
        // passed to `establish` — e.g. a caller shedding load, or shutting down mid-accept.
        // `noq-1.1.1/src/incoming.rs:111` documents this as deliberate, not a leak: dropping an
        // un-awaited `Incoming` sends the peer an explicit refusal ("Implicit reject, similar
        // to Connection's implicit close"). Proved from the connecting peer's side: `connect`
        // must fail promptly, not hang waiting for a handshake response that will never come.
        let b_key = iroh::SecretKey::generate();
        let b_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(b_key)
            .alpns(vec![zyris_p2p::transport::ALPN.to_vec()])
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let b_addr = b_ep.addr();

        // B accepts the connection attempt but declines it outright.
        let accept_task = tokio::spawn(async move {
            let pending = zyris_p2p::peer::accept_next(&b_ep).await.unwrap();
            drop(pending);
        });

        let a_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            a_ep.connect(b_addr, zyris_p2p::transport::ALPN),
        )
        .await
        .expect("connect must not hang once the acceptor declined it");
        assert!(
            result.is_err(),
            "connect must fail once the acceptor dropped the pending connection, not succeed or hang"
        );

        accept_task.await.unwrap();
    })
    .await
    .expect("test exceeded its deadline");
}

#[tokio::test]
async fn a_peer_that_stalls_after_one_byte_does_not_hang_the_first_read() {
    // Regression for F1: `establish`'s own deadline covers the QUIC handshake and opening the
    // bi-stream, and a peer satisfies it the moment it opens the stream and writes anything at
    // all — even one byte. Before this fix, nothing bounded what happened next: the resulting
    // transport's first read (which `Node::accept`'s `read_handshake` performs, with its own
    // heartbeat watchdog not yet running) could hang forever behind a peer that wrote one byte
    // and then went silent. Takes ~`FIRST_MESSAGE_DEADLINE` (10s) to run — that duration being
    // exactly what's under test, not incidental.
    tokio::time::timeout(TEST_DEADLINE, async {
        let b_key = iroh::SecretKey::generate();
        let b_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(b_key)
            .alpns(vec![zyris_p2p::transport::ALPN.to_vec()])
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let b_addr = b_ep.addr();

        let b_task = tokio::spawn(async move {
            let pending = zyris_p2p::peer::accept_next(&b_ep).await.unwrap();
            let (_peer, b_transport) =
                zyris_p2p::peer::establish(pending, ESTABLISH_DEADLINE).await.unwrap();
            let (_b_sink, mut b_read) = Box::new(b_transport).split();
            let started = std::time::Instant::now();
            let result = b_read.next().await;
            (result.is_some(), started.elapsed())
        });

        let a_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let a_conn =
            a_ep.connect(b_addr, zyris_p2p::transport::ALPN).await.unwrap();
        let (mut a_send, _a_recv) = a_conn.open_bi().await.unwrap();
        // Satisfies `establish`'s own deadline (the stream is open and *something* arrived) but
        // never completes even the 5-byte frame header, let alone a full message — exactly the
        // shape F1 exists for.
        a_send.write_all(&[0u8]).await.unwrap();

        let (got_a_result, elapsed) = b_task.await.unwrap();
        assert!(got_a_result, "a stalled peer's first read must resolve (as an error), not hang");
        assert!(
            elapsed < Duration::from_secs(15),
            "must give up at FIRST_MESSAGE_DEADLINE (10s), not linger indefinitely: took {elapsed:?}"
        );
        assert!(
            elapsed >= Duration::from_secs(9),
            "must actually wait close to the full FIRST_MESSAGE_DEADLINE, not bail early for an \
             unrelated reason: took {elapsed:?}"
        );

        // Kept alive for the whole test so the "one byte, then stall" is genuine — an early
        // drop would end the stream cleanly instead, which is a different scenario entirely.
        drop(a_send);
    })
    .await
    .expect("test exceeded its deadline");
}

#[tokio::test]
async fn establish_rejects_a_connection_that_negotiated_a_different_alpn() {
    // Regression for F2: `establish` never checked the negotiated ALPN. Measured: bind the
    // endpoint with two ALPNs, connect on the *other* one, write a few bytes, and `establish`
    // returned `Ok` — happily treating a peer speaking an unrelated protocol as if it were
    // speaking zyris.
    tokio::time::timeout(TEST_DEADLINE, async {
        const OTHER_ALPN: &[u8] = b"other/1";
        let b_key = iroh::SecretKey::generate();
        let b_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(b_key)
            .alpns(vec![zyris_p2p::transport::ALPN.to_vec(), OTHER_ALPN.to_vec()])
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        let b_addr = b_ep.addr();

        let b_task = tokio::spawn(async move {
            let pending = zyris_p2p::peer::accept_next(&b_ep).await.unwrap();
            zyris_p2p::peer::establish(pending, ESTABLISH_DEADLINE).await
        });

        let a_ep = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind()
            .await
            .unwrap();
        // Connects on the *other* ALPN the endpoint happens to also accept — not zyris's.
        let a_conn = a_ep.connect(b_addr, OTHER_ALPN).await.unwrap();
        let (mut a_send, _a_recv) = a_conn.open_bi().await.unwrap();
        a_send.write_all(&[0u8; 5]).await.unwrap();

        match b_task.await.unwrap() {
            Err(zyris_p2p::peer::PeerError::AlpnMismatch { negotiated, expected }) => {
                assert_eq!(negotiated, OTHER_ALPN, "must report the ALPN actually negotiated");
                assert_eq!(expected, zyris_p2p::transport::ALPN);
            }
            Ok(_) => panic!("a connection on a different ALPN must be rejected, not accepted"),
            Err(other) => panic!("expected AlpnMismatch, got a different error: {other}"),
        }
    })
    .await
    .expect("test exceeded its deadline");
}
