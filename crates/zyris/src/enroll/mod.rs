//! Self-registration over the OAuth 2.0 Device Authorization Grant (RFC 8628).
//!
//! A node starts with no configuration, prints a short code, and polls until the user authorizes
//! it from whatever device has a browser. No localhost redirect, and no secret copied between two
//! machines — which is the whole friction on the remote box where a node is most useful.
//!
//! This lives in `zyris` rather than in an example node because every node needs it and a polling
//! loop with `slow_down`/`expired_token` handling is precisely the thing that gets copy-pasted
//! wrong once and stays wrong.
//!
//! `protocol` and `store` are always compiled and do no IO of their own, so the retry semantics and
//! the credential-storage seam are usable with no server and no feature flag; `http` sits behind the
//! non-default `enroll` feature so a node using a static `znt_` pays nothing for it, and the on-disk
//! backend sits behind `persistence` so a node that keeps its credentials elsewhere pays nothing for
//! that either.

pub mod protocol;
pub mod store;

#[cfg(feature = "persistence")]
pub mod file_store;
#[cfg(feature = "enroll")]
pub mod http;

#[cfg(feature = "persistence")]
pub use file_store::{config_dir, FileCredentialStore, StoreError};
#[cfg(feature = "enroll")]
pub use http::{EnrollError, Enroller};
pub use store::{
    CredentialStore, CredentialStoreError, MemoryCredentialStore, StoredCredential,
};

pub use protocol::{
    authorization_notice, authorized_notice, AuthorizeRequest, AuthorizeResponse, ClientHint,
    ErrorResponse, PollOutcome, PollState, TokenResponse,
};
