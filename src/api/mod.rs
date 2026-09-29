pub mod actions;
mod actions_api;
pub mod agent;
mod agents;
pub mod approvals;
pub mod chat;
pub mod egress;
mod events;
pub mod extract;
mod files;
pub mod gated;
pub mod grant;
pub mod inhibitor;
mod login;
pub mod naming;
pub mod wake;
pub mod role;
mod router;
pub mod schedule;
mod schedules;
pub mod scope;
pub mod seed;
pub mod session;
mod sessions;
pub mod settings;
pub mod skill;
mod skills;
pub mod trigger;
pub mod usage;
pub mod user;
pub mod webhook;
mod webhooks;
pub mod work;
pub mod worker;
pub mod workspace;

pub use router::{ApiState, routes};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct Page<T: Serialize> {
    pub items: Vec<T>,
    pub next: Option<Uuid>,
}

impl<T: Serialize> Page<T> {
    pub fn from_rows(items: Vec<T>, id: impl Fn(&T) -> Uuid) -> Self {
        let next = items.last().map(&id);
        Self { items, next }
    }
}

#[derive(Debug, Deserialize)]
pub struct PageQuery {
    pub after: Option<Uuid>,
    pub limit: Option<i64>,
}
