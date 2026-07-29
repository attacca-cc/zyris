use crate::connection::{AcceptOptions, Connection};
use crate::error::Result;
use crate::node::Node;
use crate::transport::ChannelTransport;

pub async fn duplex(dialer: &Node, acceptor: &Node) -> Result<(Connection, Connection)> {
    duplex_with(dialer, acceptor, AcceptOptions::default()).await
}

pub async fn duplex_with(
    dialer: &Node,
    acceptor: &Node,
    options: AcceptOptions,
) -> Result<(Connection, Connection)> {
    let (dial_transport, accept_transport) = ChannelTransport::pair();
    let accept = acceptor.accept(accept_transport, options);
    let dial = dialer.connect_over(dial_transport);
    tokio::try_join!(dial, accept)
}
