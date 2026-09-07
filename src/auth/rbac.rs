//! Authorities, and the roles the platform reserves for itself.
//!
//! Authorities are the fixed vocabulary: each names an action the code can
//! take, so the list changes when the code changes and nowhere else. Roles
//! bundle authorities, and for tenants they are data -- rows a tenant's
//! administrator can create and edit (see `api::role`). Only the roles the
//! platform grants to itself live here, because nobody else may define what
//! those mean.
//!
//! A token carries role names. The server resolves them to authorities on
//! every request: platform roles through `platform_authorities`, tenant roles
//! through the role store. That keeps a token small however many authorities a
//! role bundles, and makes an edit to a role take effect on the next request.

use std::collections::HashSet;

/// A role the platform defines. Never a row, never editable, never granted to
/// a person by a tenant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Holds every authority, in every tenant. Granted through
    /// `user_system_roles`, which admits only this value.
    SystemAdmin,
    /// The tier that runs turns. Not a person, and not grantable to one: it
    /// exists so the work endpoints can be closed to every tenant role. Never
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
    TenantsCreate,
    TenantsRead,
    TenantsUpdate,
    TenantsDelete,
    UsersCreate,
    UsersRead,
    UsersUpdate,
    UsersDelete,
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
    SessionsDelete,
    SettingsRead,
    SettingsUpdate,
    /// Files kept for the whole tenant: reference material, procedures.
    StorageTenantRead,
    StorageTenantWrite,
    /// Files belonging to one agent's work.
    StorageAgentRead,
    StorageAgentWrite,
    GatewayInvoke,
    /// Taking turns off the queue and reporting what they produced.
    ///
    /// Held by no tenant role, however senior. A turn handed out carries the
    /// transcript of whichever tenant it belongs to, that tenant's egress
    /// rules, and a token minted for it -- so anything that can ask for work
    /// can ask for everyone's. This is the platform's own tier asking, and the
    /// distinction has to be an authority rather than a comment.
    WorkTake,
}

impl Authority {
    /// Every authority there is, for listing the vocabulary.
    pub const ALL: &'static [Authority] = &[
        Authority::TenantsCreate,
        Authority::TenantsRead,
        Authority::TenantsUpdate,
        Authority::TenantsDelete,
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
        Authority::SessionsDelete,
        Authority::SettingsRead,
        Authority::SettingsUpdate,
        Authority::StorageTenantRead,
        Authority::StorageTenantWrite,
        Authority::StorageAgentRead,
        Authority::StorageAgentWrite,
        Authority::GatewayInvoke,
        Authority::WorkTake,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Authority::TenantsCreate => "tenants:create",
            Authority::TenantsRead => "tenants:read",
            Authority::TenantsUpdate => "tenants:update",
            Authority::TenantsDelete => "tenants:delete",
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
            Authority::SessionsDelete => "sessions:delete",
            Authority::SettingsRead => "settings:read",
            Authority::SettingsUpdate => "settings:update",
            Authority::StorageTenantRead => "storage:tenant:read",
            Authority::StorageTenantWrite => "storage:tenant:write",
            Authority::StorageAgentRead => "storage:agent:read",
            Authority::StorageAgentWrite => "storage:agent:write",
            Authority::GatewayInvoke => "gateway:invoke",
            Authority::WorkTake => "work:take",
        }
    }

    /// What this authority is for, in a sentence for whoever is editing a role.
    pub fn describe(self) -> &'static str {
        match self {
            Authority::TenantsCreate => "Create tenants",
            Authority::TenantsRead => "See every tenant",
            Authority::TenantsUpdate => "Rename tenants",
            Authority::TenantsDelete => "Delete tenants",
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
            Authority::SessionsDelete => "Delete conversations",
            Authority::SettingsRead => "See settings, including allowed hosts",
            Authority::SettingsUpdate => "Change settings, including allowed hosts",
            Authority::StorageTenantRead => "Read files kept for the whole workspace",
            Authority::StorageTenantWrite => "Add and replace files kept for the whole workspace",
            Authority::StorageAgentRead => "Read an agent's files",
            Authority::StorageAgentWrite => "Add and replace an agent's files",
            Authority::GatewayInvoke => "Call a model",
            Authority::WorkTake => "Take turns off the queue (the runtime tier)",
        }
    }

    /// Whether a tenant may put this in one of its roles.
    ///
    /// Platform-wide authorities are reserved: a tenant that could grant
    /// itself `tenants:create` could make tenants, and one that could grant
    /// `work:take` could ask for every other tenant's turns. This is the
    /// replacement for the check constraint that used to pin role names.
    pub fn tenant_assignable(self) -> bool {
        !matches!(
            self,
            Authority::TenantsCreate
                | Authority::TenantsRead
                | Authority::TenantsUpdate
                | Authority::TenantsDelete
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
/// Tenant role names are ignored here: they mean whatever the tenant's role
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

/// A role every new tenant starts with.
pub struct RoleTemplate {
    pub name: &'static str,
    pub description: &'static str,
    pub authorities: &'static [Authority],
}

/// What a tenant gets when it is created.
///
/// A starting point, copied into the tenant's own rows and editable there.
/// Enterprise customers with different vocabularies get different templates,
/// or edit these; nothing here is load-bearing after the copy.
pub const DEFAULT_ROLES: &[RoleTemplate] = &[
    RoleTemplate {
        name: "admin",
        description: "Runs the workspace: people, roles, agents, settings and files.",
        authorities: &[
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
            Authority::SessionsDelete,
            Authority::SettingsRead,
            Authority::SettingsUpdate,
            Authority::StorageTenantRead,
            Authority::StorageTenantWrite,
            Authority::StorageAgentRead,
            Authority::StorageAgentWrite,
            Authority::GatewayInvoke,
        ],
    },
    RoleTemplate {
        name: "operator",
        description: "Builds and runs agents, and works with their files.",
        authorities: &[
            Authority::AgentsCreate,
            Authority::AgentsRead,
            Authority::AgentsUpdate,
            Authority::SessionsCreate,
            Authority::SessionsRead,
            Authority::StorageTenantRead,
            Authority::StorageAgentRead,
            Authority::StorageAgentWrite,
            Authority::GatewayInvoke,
        ],
    },
    RoleTemplate {
        name: "viewer",
        description: "Reads conversations, agents, settings and files without changing them.",
        authorities: &[
            Authority::AgentsRead,
            Authority::SessionsRead,
            Authority::SettingsRead,
            Authority::StorageTenantRead,
            Authority::StorageAgentRead,
        ],
    },
];

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
            // is that tenant roles are rows in `roles`, and the role store
            // refuses to create a row with a platform role's name; and
            // `user_system_roles` admits only 'system_admin'.
            "runtime" => Ok(Role::Runtime),
            "turn" => Ok(Role::Turn),
            other => Err(format!("not a platform role: {other}")),
        }
    }
}

/// Names a tenant may not use for a role of its own.
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
    fn default_roles_bundle_only_what_a_tenant_may_grant() {
        for role in DEFAULT_ROLES {
            for a in role.authorities {
                assert!(a.tenant_assignable(), "{} bundles reserved {a}", role.name);
            }
        }
    }

    #[test]
    fn a_turn_can_only_call_the_gateway() {
        let got = platform_authorities(&["turn".to_string()]);
        assert_eq!(got, HashSet::from([Authority::GatewayInvoke]));
    }

    #[test]
    fn tenant_role_names_mean_nothing_to_the_platform_resolver() {
        assert!(platform_authorities(&["admin".to_string(), "anything".to_string()]).is_empty());
    }
}
