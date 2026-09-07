use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
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
    Admin,
    Operator,
    Viewer,
}

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
    RolesAssign,
    AgentsCreate,
    AgentsRead,
    AgentsUpdate,
    AgentsDelete,
    SessionsCreate,
    SessionsRead,
    SessionsDelete,
    SettingsRead,
    SettingsUpdate,
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

impl Role {
    pub fn authorities(self) -> &'static [Authority] {
        use Authority::*;
        match self {
            Role::SystemAdmin => &[
                TenantsCreate,
                TenantsRead,
                TenantsUpdate,
                TenantsDelete,
                UsersCreate,
                UsersRead,
                UsersUpdate,
                UsersDelete,
                RolesAssign,
                AgentsCreate,
                AgentsRead,
                AgentsUpdate,
                AgentsDelete,
                SessionsCreate,
                SessionsRead,
                SessionsDelete,
                SettingsRead,
                SettingsUpdate,
                GatewayInvoke,
            ],
            Role::Runtime => &[WorkTake],
            Role::Turn => &[GatewayInvoke],
            Role::Admin => &[
                UsersCreate,
                UsersRead,
                UsersUpdate,
                UsersDelete,
                RolesAssign,
                AgentsCreate,
                AgentsRead,
                AgentsUpdate,
                AgentsDelete,
                SessionsCreate,
                SessionsRead,
                SessionsDelete,
                SettingsRead,
                SettingsUpdate,
                GatewayInvoke,
            ],
            Role::Operator => &[
                AgentsCreate,
                AgentsRead,
                AgentsUpdate,
                SessionsCreate,
                SessionsRead,
                GatewayInvoke,
            ],
            Role::Viewer => &[
                AgentsRead,
                SessionsRead,
                SettingsRead,
            ],
        }
    }
}

impl Authority {
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
            Authority::AgentsCreate => "agents:create",
            Authority::AgentsRead => "agents:read",
            Authority::AgentsUpdate => "agents:update",
            Authority::AgentsDelete => "agents:delete",
            Authority::SessionsCreate => "sessions:create",
            Authority::SessionsRead => "sessions:read",
            Authority::SessionsDelete => "sessions:delete",
            Authority::SettingsRead => "settings:read",
            Authority::SettingsUpdate => "settings:update",
            Authority::GatewayInvoke => "gateway:invoke",
            Authority::WorkTake => "work:take",
        }
    }
}

impl std::fmt::Display for Authority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub fn resolve_authorities(roles: &[Role]) -> HashSet<Authority> {
    roles.iter().flat_map(|r| r.authorities()).copied().collect()
}

impl std::fmt::Display for Role {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Role::SystemAdmin => write!(f, "system_admin"),
            Role::Runtime => write!(f, "runtime"),
            Role::Turn => write!(f, "turn"),
            Role::Admin => write!(f, "admin"),
            Role::Operator => write!(f, "operator"),
            Role::Viewer => write!(f, "viewer"),
        }
    }
}

impl std::str::FromStr for Role {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "system_admin" => Ok(Role::SystemAdmin),
            // Parseable because a token carries roles as strings and has to
            // read its own back. What keeps this from being grantable is the
            // database: `user_system_roles` admits only 'system_admin', and
            // tenant roles are constrained likewise, so no row can name it and
            // no login can produce it. It exists only in tokens the platform
            // mints for itself.
            "runtime" => Ok(Role::Runtime),
            "turn" => Ok(Role::Turn),
            "admin" => Ok(Role::Admin),
            "operator" => Ok(Role::Operator),
            "viewer" => Ok(Role::Viewer),
            other => Err(format!("unknown role: {other}")),
        }
    }
}
