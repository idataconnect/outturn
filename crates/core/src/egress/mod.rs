//! Egress: what a workspace's agents may reach, and how the tiers agree on it.
//!
//! The rules themselves, the address vetting and the header refusal are in
//! `rule`; the runtime enforces them and the gateway checks them again. The
//! rest is what every tier shares: the commitment the API makes to a rule
//! set, and the proof a request carries to show a rule is one it vouched for.

pub mod bindings;
pub mod commit;
pub mod gate;
pub mod grant;
pub mod held;
pub mod internal;
pub mod rule;
pub mod seal;
