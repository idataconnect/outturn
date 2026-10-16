//! Which agents a person's authorities apply to.
//!
//! An authority is a workspace-wide statement, and for most workspaces that is
//! right. For one running an accounting agent beside a support agent it is not:
//! the people who should read one have no business reading the other. So a
//! person's narrowed authorities can be confined to named agents.
//!
//! Absence is the important case. Somebody with no scopes holds their
//! authorities across the whole workspace exactly as before, which is what
//! makes this opt-in -- the alternative would take every existing workspace's
//! access away on upgrade. See `docs/authorities.md`.

use std::collections::HashSet;

use uuid::Uuid;

use crate::auth::Authority;

pub mod postgres;
pub use postgres::PostgresScopeStore;

/// Whether an authority is one that a scope narrows.
///
/// The line is existence against contents. Which agents exist is
/// workspace-public -- an administrator has to see what is running, and a
/// roster is not a leak. What an agent has done is: a transcript is what was
/// said, its files are what somebody uploaded.
///
/// Writes narrow with their reads. Somebody who may not read an agent's
/// conversations should certainly not be able to delete them.
pub fn is_narrowed(authority: Authority) -> bool {
    matches!(
        authority,
        Authority::SessionsCreate
            | Authority::SessionsRead
            | Authority::SessionsUpdate
            | Authority::SessionsDelete
            | Authority::StorageAgentRead
            | Authority::StorageAgentWrite
    )
}

/// What a person may reach, for one workspace.
#[derive(Debug, Clone, Default)]
pub struct Reach {
    /// The agents named for them. Empty means nobody narrowed anything, which
    /// is not the same as naming no agents -- there is no way to say "this
    /// person may reach nothing", because removing their role says it better.
    agents: HashSet<Uuid>,
}

impl Reach {
    pub fn of(agents: HashSet<Uuid>) -> Self {
        Self { agents }
    }

    /// Whether a narrowed authority applies to this agent.
    ///
    /// Unnarrowed is the common answer and means yes: the authority is what it
    /// always was, over the whole workspace.
    pub fn covers(&self, agent_id: Uuid) -> bool {
        self.agents.is_empty() || self.agents.contains(&agent_id)
    }

    /// Whether anybody narrowed this person at all.
    pub fn is_narrowed(&self) -> bool {
        !self.agents.is_empty()
    }

    /// The agents named, for a listing that has to filter rather than refuse.
    pub fn agents(&self) -> &HashSet<Uuid> {
        &self.agents
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScopeError {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Internal(String),
}

#[async_trait::async_trait]
pub trait ScopeStore: Send + Sync {
    /// What this person may reach in this workspace.
    ///
    /// Consulted on every request that names an agent, so implementations
    /// cache per workspace and drop the entry when any of its scopes change.
    async fn reach(&self, workspace_id: Uuid, user_id: Uuid) -> Result<Reach, ScopeError>;

    /// Replaces what a person may reach. An empty list removes the narrowing
    /// and returns them to the whole workspace.
    async fn set(
        &self,
        workspace_id: Uuid,
        user_id: Uuid,
        agents: &[Uuid],
    ) -> Result<(), ScopeError>;

    /// Everyone narrowed in this workspace, for showing who is confined to
    /// what.
    async fn listing(&self, workspace_id: Uuid) -> Result<Vec<(Uuid, Vec<Uuid>)>, ScopeError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nobody_narrowed_reaches_everything() {
        // The case every existing workspace is in, and the reason this is
        // opt-in: absence has to mean the authority is unchanged.
        let reach = Reach::default();
        assert!(!reach.is_narrowed());
        assert!(reach.covers(Uuid::now_v7()));
    }

    #[test]
    fn a_narrowed_person_reaches_what_was_named_and_nothing_else() {
        let mine = Uuid::now_v7();
        let theirs = Uuid::now_v7();
        let reach = Reach::of(HashSet::from([mine]));
        assert!(reach.is_narrowed());
        assert!(reach.covers(mine));
        assert!(!reach.covers(theirs));
    }

    #[test]
    fn the_roster_is_not_narrowed_but_its_contents_are() {
        // Existence against contents: an administrator sees what is running.
        assert!(!is_narrowed(Authority::AgentsRead));
        assert!(!is_narrowed(Authority::AgentsUpdate));

        assert!(is_narrowed(Authority::SessionsRead));
        assert!(is_narrowed(Authority::SessionsCreate));
        assert!(is_narrowed(Authority::StorageAgentRead));
    }

    #[test]
    fn a_write_is_narrowed_wherever_its_read_is() {
        // Otherwise somebody could delete conversations they may not read.
        assert!(is_narrowed(Authority::SessionsDelete));
        assert!(is_narrowed(Authority::StorageAgentWrite));
    }

    #[test]
    fn workspace_wide_authorities_are_left_alone() {
        // A scope is about agents, so anything that is not about an agent
        // cannot be narrowed by one.
        assert!(!is_narrowed(Authority::SettingsUpdate));
        assert!(!is_narrowed(Authority::UsageRead));
        assert!(!is_narrowed(Authority::StorageWorkspaceRead));
        assert!(!is_narrowed(Authority::RolesAssign));
    }
}
