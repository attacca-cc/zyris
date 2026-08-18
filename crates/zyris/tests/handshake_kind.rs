//! What a dialer says it is has to survive the handshake.
//!
//! It has always been *in* the agent string (`zyris/0.1.0 (zyrisd-cli; cli)`), which is a sentence
//! for a log line. An acceptor that treats a consumer differently from a node — Attacca does,
//! because `zyrisd peers` borrows the node's credential to call an API and must not be mistaken
//! for the node itself — should not have to substring-match its way to that decision.

use zyris::{Node, NodeKind};

fn node_of(kind: NodeKind) -> Node {
    Node::builder().name("probe").kind(kind).build().unwrap()
}

#[tokio::test]
async fn the_acceptor_learns_what_the_dialer_is() {
    for kind in [NodeKind::Cli, NodeKind::Desktop, NodeKind::Server, NodeKind::Service] {
        let dialer = node_of(kind.clone());
        let acceptor = node_of(NodeKind::Server);
        let (dial_conn, accept_conn) = zyris::testing::duplex(&dialer, &acceptor).await.unwrap();

        assert_eq!(
            accept_conn.info().peer_kind.as_deref(),
            Some(kind.as_str()),
            "the acceptor has to be able to tell a {} apart from anything else",
            kind.as_str()
        );

        // The acceptor sends a HelloAck, not a Hello, so the dialing side is told nothing about
        // its peer — the same asymmetry `peer_agent` already has.
        assert_eq!(dial_conn.info().peer_kind, None, "the dialer receives no hello to learn from");
    }
}

/// The distinction the acceptor actually needs: a consumer borrowing a node's credential, beside a
/// node serving on the same one. Both authenticate identically; only this tells them apart.
#[tokio::test]
async fn a_cli_and_a_desktop_are_distinguishable_on_the_same_credential() {
    let acceptor = node_of(NodeKind::Server);

    let (_, from_daemon) = zyris::testing::duplex(&node_of(NodeKind::Desktop), &acceptor)
        .await
        .unwrap();
    let (_, from_cli) = zyris::testing::duplex(&node_of(NodeKind::Cli), &acceptor).await.unwrap();

    assert_ne!(
        from_daemon.info().peer_kind,
        from_cli.info().peer_kind,
        "if these read the same, an acceptor cannot avoid treating the CLI as the node"
    );
    assert_eq!(from_cli.info().peer_kind.as_deref(), Some(NodeKind::Cli.as_str()));
}
