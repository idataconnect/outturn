//! Roles: defining them, granting them, and the seeded ladder.

use super::*;

/// Granting a role is bounded the way editing one is: by what the granter holds.
///
/// `roles:assign` alone was a ladder -- hand yourself the admin role and climb --
/// and a grant to any id pulled an account from another workspace into this one.
#[tokio::test]
async fn a_role_grant_stays_within_the_granters_reach_and_workspace() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let globex = h.make_workspace("Globex", "globex").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (status, body) = h
        .post(
            "/v1/roles",
            Some(&admin),
            r#"{"name":"assigner","authorities":["roles:assign","users:read","agents:read"]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let assigner = h
        .login_as("assigner@acme.example", None, Some((acme, "assigner")))
        .await;
    let viewer = h
        .users
        .create(CreateUser {
            email: "viewer@acme.example".into(),
            display_name: "Viewer".into(),
            password: "correct-horse".into(),
        })
        .await
        .expect("create");
    h.users
        .grant_workspace_role(viewer.id, acme, "viewer")
        .await
        .expect("grant");

    // Above reach: the admin role carries what the assigner does not hold.
    let (status, body) = h
        .post(
            &format!("/v1/users/{}/workspaces/{acme}/roles", viewer.id),
            Some(&assigner),
            r#"{"role":"admin"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a grant climbed: {body}");
    // Within it: a role bundling no more than the granter holds.
    let (status, body) = h
        .post(
            &format!("/v1/users/{}/workspaces/{acme}/roles", viewer.id),
            Some(&assigner),
            r#"{"role":"assigner"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");
    // The same bound on the role a new account is created with.
    let (status, body) = h
        .post(
            "/v1/users",
            Some(&admin),
            r#"{"email":"new@acme.example","display_name":"New","password":"correct-horse","role":"admin"}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "an admin creates admins: {body}"
    );

    // Somebody from another workspace cannot be granted in here.
    h.login_as("stranger@globex.example", None, Some((globex, "viewer")))
        .await;
    let stranger = h
        .users
        .list_for_workspace(globex, None, 10)
        .await
        .expect("list")[0]
        .id;
    let (status, body) = h
        .post(
            &format!("/v1/users/{stranger}/workspaces/{acme}/roles"),
            Some(&admin),
            r#"{"role":"viewer"}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a stranger was granted: {body}"
    );
    assert!(
        h.users
            .memberships(stranger)
            .await
            .expect("memberships")
            .iter()
            .all(|m| m.workspace_id != acme),
        "the stranger was pulled into the workspace"
    );

    // A system administrator may bring somebody in, and grants in any workspace.
    let root = h
        .login_as(
            "root@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let (status, body) = h
        .post(
            &format!("/v1/users/{stranger}/workspaces/{acme}/roles"),
            Some(&root),
            r#"{"role":"admin"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");

    finish!(h);
}

// -- Roles as data -----------------------------------------------------------

/// A workspace defines its own roles, and an edit takes effect on the next request.
#[tokio::test]
async fn a_workspace_can_define_a_role_and_it_works_at_once() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post(
            "/v1/roles",
            Some(&admin),
            r#"{"name":"analyst","description":"Reads things","authorities":["agents:read","sessions:read"]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let role: serde_json::Value = serde_json::from_str(&body).expect("role");

    // Somebody holding only that role can read agents and nothing more.
    let analyst = h
        .login_as("ann@acme.example", None, Some((acme, "analyst")))
        .await;
    let (status, _) = h.get("/v1/agents", Some(&analyst)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = h
        .post("/v1/agents", Some(&analyst), r#"{"name":"X","slug":"x"}"#)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The role is edited to allow it, and the same token now may.
    let req = Request::builder()
        .method("PATCH")
        .uri(format!("/v1/roles/{}", role["id"].as_str().expect("id")))
        .header("authorization", format!("Bearer {admin}"))
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"authorities":["agents:read","agents:create","sessions:read"]}"#,
        ))
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let (status, body) = h
        .post("/v1/agents", Some(&analyst), r#"{"name":"X","slug":"x"}"#)
        .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "the role edit did not reach a token minted before it: {body}"
    );

    finish!(h);
}

/// What a role may bundle is bounded twice: by the platform, and by its editor.
#[tokio::test]
async fn a_role_cannot_reach_past_the_platform_or_its_editor() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    // Reserved to the platform, whoever asks.
    let (status, body) = h
        .post(
            "/v1/roles",
            Some(&admin),
            r#"{"name":"overlord","authorities":["workspaces:create"]}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a workspace role took workspaces:create: {body}"
    );
    let (status, body) = h
        .post(
            "/v1/roles",
            Some(&admin),
            r#"{"name":"taker","authorities":["work:take"]}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a workspace role took work:take: {body}"
    );

    // A platform role's name cannot be reused.
    let (status, _) = h
        .post(
            "/v1/roles",
            Some(&admin),
            r#"{"name":"system_admin","authorities":[]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // An editor with roles:manage but without agents:delete cannot hand it out.
    let (status, body) = h
        .post(
            "/v1/roles",
            Some(&admin),
            r#"{"name":"role-editor","authorities":["roles:manage","agents:read"]}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let editor = h
        .login_as("ed@acme.example", None, Some((acme, "role-editor")))
        .await;
    let (status, body) = h
        .post(
            "/v1/roles",
            Some(&editor),
            r#"{"name":"ladder","authorities":["agents:delete"]}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "an editor granted an authority they do not hold: {body}"
    );

    finish!(h);
}

/// A role somebody holds stays; a name the workspace has no role for is refused.
#[tokio::test]
async fn roles_in_use_stay_and_unknown_roles_cannot_be_granted() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h.get("/v1/roles", Some(&admin)).await;
    let roles = items(&body);
    let admin_role = roles
        .iter()
        .find(|r| r["name"] == "admin")
        .expect("the default admin role");
    assert_eq!(admin_role["holders"], 1);

    let req = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/v1/roles/{}",
            admin_role["id"].as_str().expect("id")
        ))
        .header("authorization", format!("Bearer {admin}"))
        .body(Body::empty())
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a held role was deleted: {body}"
    );

    let (status, body) = h
        .post(
            "/v1/users",
            Some(&admin),
            r#"{"email":"x@acme.example","display_name":"X","password":"correct-horse","role":"wizard"}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a role that does not exist was granted: {body}"
    );

    finish!(h);
}

/// The templates a workspace's roles are copied from must be ones the code can
/// honor.
///
/// They are rows now, not constants, so nothing about them is checked when this
/// builds. Read raw rather than through the store, which filters: the point is
/// to catch a seed that would come out quietly narrower than it reads.
#[tokio::test]
async fn seeded_role_templates_are_all_honorable() {
    let h = harness_or_skip!();

    let rows: Vec<(String, String)> =
        sqlx::query_as("select template_name, authority from role_template_authorities")
            .fetch_all(&h.db.pool)
            .await
            .expect("template authorities");
    assert!(!rows.is_empty(), "no role templates were seeded");

    for (template, raw) in &rows {
        let parsed = outturn::auth::Authority::parse(raw);
        assert!(
            parsed.is_some(),
            "{template} names {raw}, which is not an authority"
        );
        assert!(
            parsed.unwrap().workspace_assignable(),
            "{template} bundles {raw}, which is reserved to the platform"
        );
    }

    // And what the store hands back is what a new workspace actually gets.
    let names: Vec<String> = h
        .roles
        .templates()
        .await
        .expect("templates")
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert!(
        names.contains(&"admin".to_string()),
        "no admin template: {names:?}"
    );
}

/// The seeded roles are a ladder: each rung holds everything the one below it
/// does, and more.
///
/// Not a matter of taste. A role that is not a superset of the one beneath it
/// makes "promote this person" a question nobody can answer from the names --
/// and the way it went wrong is instructive: `viewer` was seeded with
/// `settings:read` and `operator` was not, so the person who builds agents
/// could not read the settings those agents inherit while the person who only
/// looks could. It survived until somebody signed in as an operator and found
/// a page they had been invited to open and could not read.
#[tokio::test]
async fn the_seeded_roles_are_a_ladder() {
    let h = harness_or_skip!();

    let rows: Vec<(String, String)> =
        sqlx::query_as("select template_name, authority from role_template_authorities")
            .fetch_all(&h.db.pool)
            .await
            .expect("template authorities");

    let held = |name: &str| -> std::collections::BTreeSet<String> {
        rows.iter()
            .filter(|(template, _)| template == name)
            .map(|(_, authority)| authority.clone())
            .collect()
    };

    let viewer = held("viewer");
    let operator = held("operator");
    let admin = held("admin");
    assert!(!viewer.is_empty(), "no viewer template was seeded");

    let below_not_above = |lower: &std::collections::BTreeSet<String>,
                           upper: &std::collections::BTreeSet<String>| {
        lower.difference(upper).cloned().collect::<Vec<_>>()
    };

    assert!(
        below_not_above(&viewer, &operator).is_empty(),
        "viewer holds what operator does not: {:?}",
        below_not_above(&viewer, &operator)
    );
    assert!(
        below_not_above(&operator, &admin).is_empty(),
        "operator holds what admin does not: {:?}",
        below_not_above(&operator, &admin)
    );

    // And each rung is strictly higher, or two of them are the same role
    // wearing different names.
    assert!(
        operator.len() > viewer.len(),
        "operator adds nothing to viewer"
    );
    assert!(
        admin.len() > operator.len(),
        "admin adds nothing to operator"
    );
}
