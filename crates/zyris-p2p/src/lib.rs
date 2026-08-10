//! Zyris over a **direct node-to-node connection**.
//!
//! Not one line of the zyris core changes. All the `Transport` trait carries is
//! `WireMessage::{Binary, Text}`, and the only client/server asymmetry is
//! `Role::Dial` / `Role::Accept` — so once it is settled who dialled, everything else already
//! fits.

pub mod fingerprint;
pub mod frame;
pub mod key;
pub mod peer;
pub mod tofu;
pub mod transport;

/// Re-exported so a crate that dials through [`peer::dial`] can name the `Endpoint` and
/// `EndpointAddr` it has to pass **without declaring its own `iroh` dependency**. Two `iroh`
/// versions resolved side by side would not be a version-bump chore: `Endpoint` from one of them
/// is a different type from `Endpoint` in the other, so the mismatch surfaces as "expected
/// `Endpoint`, found `Endpoint`" at every call site. Depending on this one instead makes that
/// impossible to express.
pub use iroh;
