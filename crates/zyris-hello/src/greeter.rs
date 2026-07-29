use rand::seq::IndexedRandom;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What `hello.greet` hands back. Every field is agent-visible, so each one is documented — the
/// doc comments become the JSON-Schema field descriptions the model reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Greeting {
    /// The greeting itself, e.g. "Hej, Ada!".
    pub message: String,
    /// Language tag of the greeting that was picked, e.g. "ko".
    pub language: String,
    /// Which node answered, so an agent with several nodes can tell them apart.
    pub node: String,
}

/// The capability this node announces. One tool, no arguments beyond an optional name.
///
/// The macro turns this trait into five things: the trait itself (async, `Send + Sync + 'static`),
/// a `HelloGreetRequest` argument struct, a `hello_capability()` descriptor, a `HelloServer<T>`
/// wrapper to hand to `Node::builder().capability(..)`, and a `HelloClient` that a consumer can
/// call. The doc comment below becomes the tool description the model sees.
#[zyris::capability(name = "hello", version = 1)]
pub trait Hello {
    /// Return a random friendly greeting, optionally addressed to `name`.
    async fn greet(&self, name: Option<String>) -> zyris::Result<Greeting>;
}

const GREETINGS: &[(&str, &str)] = &[
    ("en", "Hello"),
    ("ko", "안녕하세요"),
    ("ja", "こんにちは"),
    ("fr", "Bonjour"),
    ("de", "Hallo"),
    ("es", "Hola"),
    ("sv", "Hej"),
    ("pt", "Olá"),
];

/// Deliberately synchronous. `ThreadRng` is `!Send` and every capability method's future must be
/// `Send`, so holding one across an `.await` inside `greet` would not compile. Drawing here makes
/// that mistake structurally impossible for anyone extending this file.
fn pick() -> (&'static str, &'static str) {
    *GREETINGS.choose(&mut rand::rng()).unwrap_or(&("en", "Hello"))
}

pub struct RandomGreeter {
    node_name: String,
}

impl RandomGreeter {
    pub fn new(node_name: impl Into<String>) -> RandomGreeter {
        RandomGreeter { node_name: node_name.into() }
    }
}

#[zyris::async_trait]
impl Hello for RandomGreeter {
    async fn greet(&self, name: Option<String>) -> zyris::Result<Greeting> {
        let (language, hello) = pick();
        let message = match name.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(who) => format!("{hello}, {who}!"),
            None => format!("{hello}!"),
        };
        Ok(Greeting { message, language: language.to_string(), node: self.node_name.clone() })
    }
}
