use std::sync::Arc;

use crate::auth::Role;

use super::role::RoleStore;
use super::tenant::{CreateTenant, TenantStore};
use super::user::{CreateUser, UserStore};

/// Seeds a system admin and a starter tenant for local development.
///
/// Runs only when OUTTURN_DEV_SEED is set and the users table is empty, so it
/// is inert against any database that already has accounts.
pub async fn dev_seed(
    users: &Arc<dyn UserStore>,
    tenants: &Arc<dyn TenantStore>,
    roles: &Arc<dyn RoleStore>,
) -> anyhow::Result<()> {
    if std::env::var("OUTTURN_DEV_SEED").is_err() {
        return Ok(());
    }

    if !users.is_empty().await? {
        tracing::debug!("dev seed skipped: users already exist");
        return Ok(());
    }

    let email = std::env::var("OUTTURN_DEV_ADMIN_EMAIL")
        .unwrap_or_else(|_| "admin@outturn.local".into());
    let password =
        std::env::var("OUTTURN_DEV_ADMIN_PASSWORD").unwrap_or_else(|_| "outturn-dev".into());

    let admin = users
        .create(CreateUser {
            email: email.clone(),
            display_name: "System Admin".into(),
            password: password.clone(),
        })
        .await?;

    users.grant_system_role(admin.id, Role::SystemAdmin).await?;

    let tenant = tenants
        .create(CreateTenant {
            name: "Acme".into(),
            slug: "acme".into(),
        })
        .await?;

    roles.seed_defaults(tenant.id).await?;
    users
        .grant_tenant_role(admin.id, tenant.id, "admin")
        .await?;

    tracing::warn!(
        email = %email,
        password = %password,
        tenant = %tenant.slug,
        "dev seed created a system admin — never enable OUTTURN_DEV_SEED outside local development"
    );

    Ok(())
}
