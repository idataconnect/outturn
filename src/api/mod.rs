pub mod actions;
mod actions_api;
pub mod agent;
pub mod agent_template;
mod agent_templates;
mod agents;
pub mod approvals;
pub mod chat;
pub mod compact;
mod credentials;
pub mod egress;
mod events;
pub mod extract;
mod files;
pub mod gated;
pub mod grant;
pub mod inhibitor;
mod login;
pub mod naming;
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
mod skill_sources;
mod skills;
pub mod trigger;
pub mod usage;
pub mod user;
pub mod wake;
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
    /// A page from rows read with `limit + 1`.
    ///
    /// The extra row is how a page knows there is another: `next` is set only
    /// when it was there, and the row itself is dropped. Setting `next` on any
    /// non-empty page had every client ask once more past the end, and a client
    /// that stopped at the first empty answer could never tell a full page from
    /// the last one.
    pub fn from_rows(mut items: Vec<T>, limit: i64, id: impl Fn(&T) -> Uuid) -> Self {
        let limit = usize::try_from(limit).unwrap_or(0);
        let next = if items.len() > limit {
            items.truncate(limit);
            items.last().map(&id)
        } else {
            None
        };
        Self { items, next }
    }
}

#[derive(Debug, Deserialize)]
pub struct PageQuery {
    pub after: Option<Uuid>,
    pub limit: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: u128) -> Vec<Uuid> {
        (1..=n).map(Uuid::from_u128).collect()
    }

    #[test]
    fn the_extra_row_is_dropped_and_names_the_next_page() {
        let page = Page::from_rows(ids(3), 2, |id| *id);
        assert_eq!(page.items, ids(2));
        assert_eq!(page.next, Some(Uuid::from_u128(2)));
    }

    #[test]
    fn a_page_with_nothing_after_it_says_so() {
        assert_eq!(Page::from_rows(ids(2), 2, |id| *id).next, None);
        assert_eq!(Page::from_rows(ids(1), 2, |id| *id).next, None);
        assert_eq!(Page::from_rows(Vec::<Uuid>::new(), 2, |id| *id).next, None);
    }
}
