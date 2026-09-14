//! Egress: what a workspace's agents may reach, and how the tiers agree on it.
//!
//! The rules themselves, the address vetting and the header refusal live in
//! `runtime::egress`, where they are enforced. What is here is the part every
//! tier needs to share: the commitment the API makes to a rule set, and the
//! proof a request carries to show a rule is one the API vouched for.

pub mod commit;
