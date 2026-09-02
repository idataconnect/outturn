use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    SystemAdmin,
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
            "admin" => Ok(Role::Admin),
            "operator" => Ok(Role::Operator),
            "viewer" => Ok(Role::Viewer),
            other => Err(format!("unknown role: {other}")),
        }
    }
}
