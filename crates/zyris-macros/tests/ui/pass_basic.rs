use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Out {
    pub ok: bool,
}

#[zyris::capability(name = "basic", version = 1)]
pub trait Basic {
    /// Do the thing.
    async fn run(&self, input: String) -> zyris::Result<Out>;

    /// Stream things.
    #[zyris(uni_stream)]
    async fn feed(&self, n: u32) -> zyris::Result<zyris::Streaming<Out, Out>>;

    /// No arguments.
    async fn ping(&self) -> zyris::Result<Out>;
}

fn main() {
    let descriptor = basic_capability();
    assert_eq!(descriptor.name, "basic");
    assert_eq!(descriptor.tools.len(), 3);
}
