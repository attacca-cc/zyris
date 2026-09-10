//! Everything this machine offers an agent, and the two things in front of it.

pub mod audit;
pub mod gate;

pub use audit::{AuditLog, Entry, Outcome};
pub use gate::Gate;
