use std::time::Duration;

use futures_util::StreamExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zyris::{Node, NodeKind, Streaming, Transfer};

#[derive(Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AddOutput {
    pub sum: i64,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TickHead {
    pub count: u32,
}

#[derive(Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Tick {
    pub n: u32,
}

#[zyris::capability(name = "calc", version = 1)]
pub trait Calc {
    /// Add two numbers.
    async fn add(&self, a: i64, b: i64) -> zyris::Result<AddOutput>;

    /// Stream `count` ticks.
    #[zyris(uni_stream)]
    async fn ticks(&self, count: u32) -> zyris::Result<Streaming<TickHead, Tick>>;

    /// Answer with a fixed sum.
    async fn ping(&self) -> zyris::Result<AddOutput>;
}

struct CalcImpl;

#[zyris::async_trait]
impl Calc for CalcImpl {
    async fn add(&self, a: i64, b: i64) -> zyris::Result<AddOutput> {
        Ok(AddOutput { sum: a + b })
    }

    async fn ticks(&self, count: u32) -> zyris::Result<Streaming<TickHead, Tick>> {
        let items = futures_util::stream::iter((0..count).map(|n| Ok(Tick { n })));
        Ok(Streaming::new(TickHead { count }, items))
    }

    async fn ping(&self) -> zyris::Result<AddOutput> {
        Ok(AddOutput { sum: 0 })
    }
}

fn calc_node() -> Node {
    Node::builder()
        .name("calc-node")
        .kind(NodeKind::Service)
        .capability(CalcServer(CalcImpl))
        .build()
        .unwrap()
}

#[test]
fn descriptor_contains_tools_and_schemas() {
    let descriptor = calc_capability();
    assert_eq!(descriptor.name, "calc");
    assert_eq!(descriptor.version, 1);
    assert_eq!(descriptor.tools.len(), 3);

    let add = descriptor.tool("add").unwrap();
    assert_eq!(add.description, "Add two numbers.");
    assert_eq!(add.transfer, Transfer::Unary);
    let props = add.request_schema["properties"].as_object().unwrap();
    assert!(props.contains_key("a"));
    assert!(props.contains_key("b"));

    let ticks = descriptor.tool("ticks").unwrap();
    assert_eq!(ticks.transfer, Transfer::UniStream);
    assert!(ticks.item_schema.is_some());
    let head_schema = ticks.response_schema.as_ref().unwrap();
    assert!(head_schema["properties"].as_object().unwrap().contains_key("count"));
}

#[tokio::test]
async fn generated_client_calls_generated_server() {
    let dialer = Node::builder().name("consumer").kind(NodeKind::Cli).build().unwrap();
    let acceptor = calc_node();
    let (client_conn, _server_conn) =
        zyris::testing::duplex(&dialer, &acceptor).await.unwrap();

    let calc: CalcClient = client_conn.wait_capability(Duration::from_secs(2)).await.unwrap();

    let out = calc.add(20, 22).await.unwrap();
    assert_eq!(out, AddOutput { sum: 42 });

    let out = calc.ping().await.unwrap();
    assert_eq!(out.sum, 0);

    let mut streaming = calc.ticks(4).await.unwrap();
    assert_eq!(streaming.head, TickHead { count: 4 });
    let mut seen = Vec::new();
    while let Some(item) = streaming.items.next().await {
        seen.push(item.unwrap().n);
    }
    assert_eq!(seen, vec![0, 1, 2, 3]);
}

#[tokio::test]
async fn client_implements_the_trait_for_proxying() {
    async fn use_any_calc(calc: &dyn Calc) -> i64 {
        calc.add(1, 2).await.unwrap().sum
    }

    let dialer = Node::builder().name("consumer").kind(NodeKind::Cli).build().unwrap();
    let acceptor = calc_node();
    let (client_conn, _server_conn) =
        zyris::testing::duplex(&dialer, &acceptor).await.unwrap();
    let calc: CalcClient = client_conn.wait_capability(Duration::from_secs(2)).await.unwrap();
    assert_eq!(use_any_calc(&calc).await, 3);
}
