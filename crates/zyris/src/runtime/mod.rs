//! Batteries: the config, the credentials, and the reconnect loop, so a node is a `main` that says
//! what it serves and nothing else.
//!
//! Every node ends up needing the same four things — read some configuration, get a bearer, dial,
//! and come back when the connection drops. Written once per node those are four chances to get
//! backoff, credential rotation, or an exit code subtly wrong; written here they are one
//! implementation everyone shares.
//!
//! Nothing in this module is required. [`Node::connect`](crate::Node::connect) remains the
//! primitive, and a node with its own supervision story should keep using it.
//!
//! ```no_run
//! # use std::process::ExitCode;
//! # use zyris::{NodeKind, runtime::Runner};
//! #[tokio::main]
//! async fn main() -> ExitCode {
//!     Runner::from_env()
//!         .kind(NodeKind::Service)
//!         // .capability(YourServer(your_impl))
//!         .run()
//!         .await
//! }
//! ```

pub mod credentials;
mod runner;

pub use credentials::{
    token_prefix, Credentials, CredentialsError, StaticToken, TokenFile, NODE_TOKEN_PREFIX,
};
pub use runner::{RunConfig, RunError, Runner};

#[cfg(feature = "enroll")]
pub use credentials::DeviceGrant;
