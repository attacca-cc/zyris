use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Out {
    pub ok: bool,
}

#[zyris::capability(name = "bad", version = 1)]
pub trait Bad {
    async fn run<T: Send>(&self, input: T) -> zyris::Result<Out>;
}

fn main() {}
