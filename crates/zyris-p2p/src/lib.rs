//! Zyris over a **direct node-to-node connection**.
//!
//! Not one line of the zyris core changes. All the `Transport` trait carries is
//! `WireMessage::{Binary, Text}`, and the only client/server asymmetry is
//! `Role::Dial` / `Role::Accept` — so once it is settled who dialled, everything else already
//! fits.

pub mod frame;
pub mod key;
pub mod tofu;
