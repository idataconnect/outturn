use std::sync::Arc;

use crate::auth::Role;

use super::role::RoleStore;
use super::user::{CreateUser, UserStore};
use super::workspace::{CreateWorkspace, WorkspaceStore};

/// Seeds a system admin and a starter workspace for local development.
///
/// Runs only when OUTTURN_DEV_SEED is set and the users table is empty, so it
/// is inert against any database that already has accounts.
pub async fn dev_seed(
    users: &Arc<dyn UserStore>,
    workspaces: &Arc<dyn WorkspaceStore>,
    roles: &Arc<dyn RoleStore>,
) -> anyhow::Result<()> {
    if std::env::var("OUTTURN_DEV_SEED").is_err() {
        return Ok(());
    }

    if !users.is_empty().await? {
        tracing::debug!("dev seed skipped: users already exist");
        return Ok(());
    }

    let email =
        std::env::var("OUTTURN_DEV_ADMIN_EMAIL").unwrap_or_else(|_| "admin@outturn.local".into());
    // No default. "outturn-dev" was one, and it is in this repository's
    // git history, which makes it a password anybody can look up. The seed is
    // already gated on OUTTURN_DEV_SEED and an empty users table, so the only
    // thing a fallback bought was saving a developer one line in a script --
    // against the chance of a reachable deployment whose admin password is
    // published. Asking for it is cheaper than that.
    let password = std::env::var("OUTTURN_DEV_ADMIN_PASSWORD").map_err(|_| {
        anyhow::anyhow!(
            "OUTTURN_DEV_SEED is set but OUTTURN_DEV_ADMIN_PASSWORD is not; \
             run scripts/dev-secrets.sh, which generates one per clone"
        )
    })?;

    let admin = users
        .create(CreateUser {
            email: email.clone(),
            display_name: "System Admin".into(),
            password: password.clone(),
        })
        .await?;

    users.grant_system_role(admin.id, Role::SystemAdmin).await?;

    let workspace = workspaces
        .create(CreateWorkspace {
            name: "Acme".into(),
            slug: "acme".into(),
        })
        .await?;

    roles.seed_defaults(workspace.id).await?;
    users
        .grant_workspace_role(admin.id, workspace.id, "admin")
        .await?;

    tracing::warn!(
        email = %email,
        password = %password,
        workspace = %workspace.slug,
        "dev seed created a system admin — never enable OUTTURN_DEV_SEED outside local development"
    );

    Ok(())
}
