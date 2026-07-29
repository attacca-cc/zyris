//! Proves the announce path end to end without a server or a database: two nodes over an
//! in-memory duplex, one serving `hello`, the other calling it.

use std::time::Duration;

use zyris::{Node, NodeKind, Transfer};

#[path = "../src/greeter.rs"]
mod greeter;

use greeter::{hello_capability, Hello, HelloClient, HelloServer, RandomGreeter};

fn hello_node() -> Node {
    Node::builder()
        .name("hello-node")
        .kind(NodeKind::Service)
        .capability(HelloServer(RandomGreeter::new("hello-node")))
        .build()
        .unwrap()
}

#[test]
fn descriptor_advertises_one_unary_tool() {
    let descriptor = hello_capability();
    assert_eq!(descriptor.name, "hello");
    assert_eq!(descriptor.version, 1);
    assert_eq!(descriptor.tools.len(), 1);

    let greet = descriptor.tool("greet").unwrap();
    assert_eq!(greet.transfer, Transfer::Unary);
    assert_eq!(greet.description, "Return a random friendly greeting, optionally addressed to `name`.");
    assert!(greet.request_schema["properties"].as_object().unwrap().contains_key("name"));
}

#[tokio::test]
async fn consumer_calls_greet_over_duplex() {
    let consumer = Node::builder().name("consumer").kind(NodeKind::Cli).build().unwrap();
    let (_server_side, client_side) = zyris::testing::duplex(&hello_node(), &consumer).await.unwrap();

    let hello: HelloClient = client_side.wait_capability(Duration::from_secs(2)).await.unwrap();

    let greeting = hello.greet(Some("Ada".into())).await.unwrap();
    assert!(greeting.message.contains("Ada"), "got {:?}", greeting.message);
    assert_eq!(greeting.node, "hello-node");
    assert!(!greeting.language.is_empty());

    // No name still greets, just without an addressee.
    let anonymous = hello.greet(None).await.unwrap();
    assert!(!anonymous.message.contains("Ada"));
    assert!(anonymous.message.ends_with('!'));
}

#[tokio::test]
async fn peer_sees_the_announced_capability() {
    let consumer = Node::builder().name("consumer").kind(NodeKind::Cli).build().unwrap();
    let (_server_side, client_side) = zyris::testing::duplex(&hello_node(), &consumer).await.unwrap();
    let _: HelloClient = client_side.wait_capability(Duration::from_secs(2)).await.unwrap();

    let descriptors = client_side.peer_descriptors();
    assert_eq!(descriptors.len(), 1);
    assert_eq!(descriptors[0].name, "hello");
    assert_eq!(descriptors[0].tools[0].name, "greet");
}
