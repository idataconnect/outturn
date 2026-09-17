//! Authorities, and the roles the platform reserves for itself.
//!
//! Authorities are the fixed vocabulary: each names an action the code can
//! take, so the list changes when the code changes and nowhere else. Roles
//! bundle authorities, and for workspaces they are data -- rows a workspace's
//! administrator can create and edit (see `api::role`). Only the roles the
//! platform grants to itself live here, because nobody else may define what
//! those mean.
//!
//! A token carries role names. The server resolves them to authorities on
//! every request: platform roles through `platform_authorities`, workspace roles
//! through the role store. That keeps a token small however many authorities a
//! role bundles, and makes an edit to a role take effect on the next request.

use std::collections::HashSet;

/// A role the platform defines. Never a row, never editable, never granted to
/// a person by a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Holds every authority, in every workspace. Granted through
    /// `user_system_roles`, which admits only this value.
    SystemAdmin,
    /// The tier that runs turns. Not a person, and not grantable to one: it
    /// exists so the work endpoints can be closed to every workspace role. Never
    /// carried in a signed token -- the runtime presents a shared key and is
    /// given these claims by the API (see `RuntimeKey`).
    Runtime,
    /// One turn, reaching the gateway. Holds `GatewayInvoke` and nothing
    /// else, so the token a turn travels with cannot be used against the API
    /// even before the audience check refuses it there.
    Turn,
}

/// Something the code can be asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Authority {
    WorkspacesCreate,
    WorkspacesRead,
    WorkspacesUpdate,
    WorkspacesDelete,
    UsersCreate,
    UsersRead,
    UsersUpdate,
    UsersDelete,
    /// Stopping and restarting everything a workspace runs.
    ///
    /// Separate from `WorkspacesUpdate` deliberately: a credential that can
    /// halt an org should not thereby be able to rename or delete it, and an
    /// admin UI should not carry kill rights on every request it makes.
    WorkspacesInhibit,
    /// Stopping and restarting one agent.
    ///
    /// Narrower than the workspace switch and held more widely: an operator who
    /// builds agents is the right person to stop one misbehaving, without also
    /// being able to halt everything the workspace runs.
    AgentsInhibit,
    /// Granting and revoking roles to people.
    RolesAssign,
    /// Defining what a role means: creating, editing and deleting roles.
    RolesManage,
    AgentsCreate,
    AgentsRead,
    AgentsUpdate,
    AgentsDelete,
    SessionsCreate,
    SessionsRead,
    SessionsUpdate,
    SessionsDelete,
    SettingsRead,
    SettingsUpdate,
    /// Files kept for the whole workspace: reference material, procedures.
    StorageWorkspaceRead,
    StorageWorkspaceWrite,
    /// Files belonging to one agent's work.
    StorageAgentRead,
    StorageAgentWrite,
    /// Skills: the prose an agent is given beside its system prompt.
    ///
    /// Reading covers the operator's skills as well as the workspace's own,
    /// since a workspace cannot decide whether to override one it cannot see.
    /// Writing is over its own: an operator's skill is never edited in place
    /// by a workspace, only overridden or forked.
    SkillsRead,
    SkillsWrite,
    /// Reading the usage ledger: what was spent, by whom, for which customer.
    UsageRead,
    GatewayInvoke,
    /// Taking turns off the queue and reporting what they produced.
    ///
    /// Held by no workspace role, however senior. A turn handed out carries the
    /// transcript of whichever workspace it belongs to, that workspace's egress
    /// rules, and a token minted for it -- so anything that can ask for work
    /// can ask for everyone's. This is the platform's own tier asking, and the
    /// distinction has to be an authority rather than a comment.
    WorkTake,
}

impl Authority {
    /// Every authority there is, for listing the vocabulary.
    pub const ALL: &'static [Authority] = &[
        Authority::WorkspacesCreate,
        Authority::WorkspacesRead,
        Authority::WorkspacesUpdate,
        Authority::WorkspacesDelete,
        Authority::UsersCreate,
        Authority::UsersRead,
        Authority::UsersUpdate,
        Authority::UsersDelete,
        Authority::RolesAssign,
        Authority::RolesManage,
        Authority::AgentsCreate,
        Authority::AgentsRead,
        Authority::AgentsUpdate,
        Authority::AgentsDelete,
        Authority::SessionsCreate,
        Authority::SessionsRead,
        Authority::SessionsUpdate,
        Authority::SessionsDelete,
        Authority::SettingsRead,
        Authority::SettingsUpdate,
        Authority::StorageWorkspaceRead,
        Authority::StorageWorkspaceWrite,
        Authority::StorageAgentRead,
        Authority::StorageAgentWrite,
        Authority::SkillsRead,
        Authority::SkillsWrite,
        Authority::UsageRead,
        Authority::WorkspacesInhibit,
        Authority::AgentsInhibit,
        Authority::GatewayInvoke,
        Authority::WorkTake,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Authority::WorkspacesCreate => "workspaces:create",
            Authority::WorkspacesRead => "workspaces:read",
            Authority::WorkspacesUpdate => "workspaces:update",
            Authority::WorkspacesDelete => "workspaces:delete",
            Authority::UsersCreate => "users:create",
            Authority::UsersRead => "users:read",
            Authority::UsersUpdate => "users:update",
            Authority::UsersDelete => "users:delete",
            Authority::RolesAssign => "roles:assign",
            Authority::RolesManage => "roles:manage",
            Authority::AgentsCreate => "agents:create",
            Authority::AgentsRead => "agents:read",
            Authority::AgentsUpdate => "agents:update",
            Authority::AgentsDelete => "agents:delete",
            Authority::SessionsCreate => "sessions:create",
            Authority::SessionsRead => "sessions:read",
            Authority::SessionsUpdate => "sessions:update",
            Authority::SessionsDelete => "sessions:delete",
            Authority::SettingsRead => "settings:read",
            Authority::SettingsUpdate => "settings:update",
            Authority::StorageWorkspaceRead => "storage:workspace:read",
            Authority::StorageWorkspaceWrite => "storage:workspace:write",
            Authority::StorageAgentRead => "storage:agent:read",
            Authority::StorageAgentWrite => "storage:agent:write",
            Authority::SkillsRead => "skills:read",
            Authority::SkillsWrite => "skills:write",
            Authority::UsageRead => "usage:read",
            Authority::WorkspacesInhibit => "workspaces:inhibit",
            Authority::AgentsInhibit => "agents:inhibit",
            Authority::GatewayInvoke => "gateway:invoke",
            Authority::WorkTake => "work:take",
        }
    }

    /// What this authority is for, in a sentence for whoever is editing a role.
    pub fn describe(self) -> &'static str {
        match self {
            Authority::WorkspacesCreate => "Create workspaces",
            Authority::WorkspacesRead => "See every workspace",
            Authority::WorkspacesUpdate => "Rename workspaces",
            Authority::WorkspacesDelete => "Delete workspaces",
            Authority::UsersCreate => "Add users",
            Authority::UsersRead => "See users and how they sign in",
            Authority::UsersUpdate => "Change users' names and sign-ins",
            Authority::UsersDelete => "Delete users",
            Authority::RolesAssign => "Give roles to users and take them away",
            Authority::RolesManage => "Create, edit and delete roles",
            Authority::AgentsCreate => "Create agents",
            Authority::AgentsRead => "See agents",
            Authority::AgentsUpdate => "Edit agents",
            Authority::AgentsDelete => "Delete agents",
            Authority::SessionsCreate => "Start conversations and send messages",
            Authority::SessionsRead => "Read conversations",
            Authority::SessionsUpdate => "Rename conversations",
            Authority::SessionsDelete => "Delete conversations",
            Authority::SettingsRead => "See settings, including allowed hosts",
            Authority::SettingsUpdate => "Change settings, including allowed hosts",
            Authority::StorageWorkspaceRead => "Read files kept for the whole workspace",
            Authority::StorageWorkspaceWrite => "Add and replace files kept for the whole workspace",
            Authority::StorageAgentRead => "Read an agent's files",
            Authority::StorageAgentWrite => "Add and replace an agent's files",
            Authority::SkillsRead => "See skills, the workspace's own and the operator's",
            Authority::SkillsWrite => "Write skills, and override or fork the operator's",
            Authority::UsageRead => "Read the usage ledger",
            Authority::WorkspacesInhibit => "Stop and restart everything this workspace runs",
            Authority::AgentsInhibit => "Stop and restart one agent",
            Authority::GatewayInvoke => "Call a model",
            Authority::WorkTake => "Take turns off the queue (the runtime tier)",
        }
    }

    /// Whether a workspace may put this in one of its roles.
    ///
    /// Platform-wide authorities are reserved: a workspace that could grant
    /// itself `workspaces:create` could make workspaces, and one that could grant
    /// `work:take` could ask for every other workspace's turns. This is the
    /// replacement for the check constraint that used to pin role names.
    ///
    /// `workspaces:inhibit` is not reserved despite the prefix, because it acts
    /// on the caller's own workspace rather than across them -- it is closer to
    /// `settings:update` than to `workspaces:delete`, and a customer holding a
    /// credential that can halt their own org is the case it exists for.
    pub fn workspace_assignable(self) -> bool {
        !matches!(
            self,
            Authority::WorkspacesCreate
                | Authority::WorkspacesRead
                | Authority::WorkspacesUpdate
                | Authority::WorkspacesDelete
                | Authority::WorkTake
        )
    }

    pub fn parse(s: &str) -> Option<Authority> {
        Authority::ALL.iter().copied().find(|a| a.as_str() == s)
    }
}

impl std::fmt::Display for Authority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Role {
    pub fn authorities(self) -> HashSet<Authority> {
        match self {
            Role::SystemAdmin => Authority::ALL
                .iter()
                .copied()
                .filter(|a| *a != Authority::WorkTake)
                .collect(),
            Role::Runtime => HashSet::from([Authority::WorkTake]),
            Role::Turn => HashSet::from([Authority::GatewayInvoke]),
        }
    }
}

/// Authorities that follow from the platform roles among `roles`.
///
/// Workspace role names are ignored here: they mean whatever the workspace's role
/// rows say, which is the role store's business. A tier without a database --
/// the gateway -- can still make every decision it needs from this alone,
/// because the only role it ever sees is `turn`.
pub fn platform_authorities(roles: &[String]) -> HashSet<Authority> {
    roles
        .iter()
        .filter_map(|r| r.parse::<Role>().ok())
        .flat_map(|r| r.authorities())
        .collect()
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Role::SystemAdmin => write!(f, "system_admin"),
            Role::Runtime => write!(f, "runtime"),
            Role::Turn => write!(f, "turn"),
        }
    }
}

impl std::str::FromStr for Role {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "system_admin" => Ok(Role::SystemAdmin),
            // Parseable because a token carries role names and has to read
            // its own back. What keeps these from being granted to a person
            // is that workspace roles are rows in `roles`, and the role store
            // refuses to create a row with a platform role's name; and
            // `user_system_roles` admits only 'system_admin'.
            "runtime" => Ok(Role::Runtime),
            "turn" => Ok(Role::Turn),
            other => Err(format!("not a platform role: {other}")),
        }
    }
}

/// Names a workspace may not use for a role of its own.
pub fn is_platform_role_name(name: &str) -> bool {
    name.parse::<Role>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_authority_round_trips_through_its_name() {
        for a in Authority::ALL {
            assert_eq!(Authority::parse(a.as_str()), Some(*a));
        }
    }

    #[test]
    fn a_turn_can_only_call_the_gateway() {
        let got = platform_authorities(&["turn".to_string()]);
        assert_eq!(got, HashSet::from([Authority::GatewayInvoke]));
    }

    #[test]
    fn workspace_role_names_mean_nothing_to_the_platform_resolver() {
        assert!(platform_authorities(&["admin".to_string(), "anything".to_string()]).is_empty());
    }
}
