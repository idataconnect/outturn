//! What every tier shares: the database, the queue and the event log, tokens,
//! egress rules and their commitment, object storage, and the protocols the
//! tiers speak to one another. Nothing here depends on a tier, so a change to
//! one rebuilds that tier alone.

pub mod auth;
pub mod breaker;
pub mod catalog;
pub mod chains;
pub mod db;
pub mod egress;
pub mod events;
pub mod extract;
pub mod facts;
pub mod http_client;
pub mod inhibitor;
pub mod jobs;
pub mod lifecycle;
pub mod llm;
pub mod model_settings;
pub mod protocol;
pub mod scope;
pub mod storage;
pub mod utf8;
pub mod vision;
