use std::io::{BufRead, Write};
use std::sync::Arc;

use outturn_core::auth::Role;

use super::role::RoleStore;
use super::user::{CreateUser, UserStore};
use super::workspace::{CreateWorkspace, Workspace, WorkspaceStore};

/// Why `api bootstrap` refuses a deployment somebody has already set up.
pub const ALREADY_SET_UP: &str =
    "this deployment already has accounts; sign in and add people from there";

/// The first account on a deployment, and the organization's workspace it
/// administers.
pub struct Operator {
    pub email: String,
    pub display_name: String,
    pub password: String,
    pub organization: String,
    pub slug: String,
}

/// Creates the operator as a system admin, and the organization's workspace
/// with them as its admin. Refused once anybody has an account: after that,
/// accounts are made by somebody signed in, and a command that could add a
/// system admin to a running deployment would be a way around them.
pub async fn first_operator(
    users: &Arc<dyn UserStore>,
    workspaces: &Arc<dyn WorkspaceStore>,
    roles: &Arc<dyn RoleStore>,
    operator: Operator,
) -> anyhow::Result<Workspace> {
    if !users.is_empty().await? {
        anyhow::bail!(ALREADY_SET_UP);
    }
    // Checked before anything is written, so a bad slug leaves no account
    // behind without a workspace.
    let input = CreateWorkspace {
        name: operator.organization,
        slug: operator.slug,
    };
    super::workspace::validate(&input)?;

    let admin = users
        .create(CreateUser {
            email: operator.email,
            display_name: operator.display_name,
            password: operator.password,
        })
        .await?;
    users.grant_system_role(admin.id, Role::SystemAdmin).await?;

    let workspace = workspaces.create(input).await?;
    roles.seed_defaults(workspace.id).await?;
    users
        .grant_workspace_role(admin.id, workspace.id, "admin")
        .await?;
    Ok(workspace)
}

/// A slug from an organization's name: "i Data Connect!" is `i-data-connect`.
pub fn slug_from(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_string()
}

/// Asks at the terminal for who the operator is, for `api bootstrap`. Asked
/// rather than passed as flags, so the password is never on a command line,
/// where the shell's history and the process list would both keep it.
pub fn ask_operator() -> anyhow::Result<Operator> {
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    let mut ask = |prompt: &str, default: Option<&str>| -> anyhow::Result<String> {
        match default {
            Some(d) => print!("{prompt} [{d}]: "),
            None => print!("{prompt}: "),
        }
        std::io::stdout().flush()?;
        let line = lines
            .next()
            .ok_or_else(|| anyhow::anyhow!("no answer to {prompt:?}"))??;
        let line = line.trim();
        Ok(match (line.is_empty(), default) {
            (true, Some(d)) => d.to_string(),
            _ => line.to_string(),
        })
    };

    let email = ask("Your email", None)?;
    let display_name = ask("Your name", None)?;
    let organization = ask("Organization name", None)?;
    let slug = ask("Workspace slug", Some(&slug_from(&organization)))?;
    let password = rpassword::prompt_password("Password: ")?;
    if rpassword::prompt_password("Password again: ")? != password {
        anyhow::bail!("the passwords differ");
    }
    Ok(Operator {
        email,
        display_name,
        password,
        organization,
        slug,
    })
}

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

    let workspace = first_operator(
        users,
        workspaces,
        roles,
        Operator {
            email: email.clone(),
            display_name: "System Admin".into(),
            password,
            organization: "Acme".into(),
            slug: "acme".into(),
        },
    )
    .await?;

    tracing::warn!(
        email = %email,
        workspace = %workspace.slug,
        "dev seed created a system admin; scripts/dev-secrets.sh --print shows its password — never enable OUTTURN_DEV_SEED outside local development"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::slug_from;

    #[test]
    fn slug_from_an_organization_name() {
        assert_eq!(slug_from("i Data Connect!"), "i-data-connect");
        assert_eq!(slug_from("  Kestrel & Sons, Ltd.  "), "kestrel-sons-ltd");
        assert_eq!(slug_from("Café 9"), "caf-9");
        assert_eq!(slug_from("!!!"), "");
    }
}
