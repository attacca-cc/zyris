use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Out {
    pub ok: bool,
}

#[zyris::capability(name = "bad", version = 1)]
pub trait Bad {
    #[zyris(bi_stream)]
    async fn chat(&self, n: u32) -> zyris::Result<Out>;
}

fn main() {}
