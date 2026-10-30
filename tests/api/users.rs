//! Users and workspaces: who may manage which, and what each admin sees.

use super::*;

#[tokio::test]
async fn system_admin_sees_all_workspaces_and_manages_them() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    h.make_workspace("Globex", "globex").await;

    // No explicit grant in either workspace — system admin still reaches them.
    let token = h
        .login_as(
            "admin@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;

    let (status, body) = h.get("/v1/workspaces", Some(&token)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body.contains("Acme") && body.contains("Globex"),
        "body: {body}"
    );

    let (status, body) = h
        .post(
            "/v1/workspaces",
            Some(&token),
            r#"{"name":"Initech","slug":"initech"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    finish!(h);
}

#[tokio::test]
async fn workspace_admin_cannot_manage_workspaces() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, _) = h.get("/v1/workspaces", Some(&token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _) = h
        .post("/v1/workspaces", Some(&token), r#"{"name":"X","slug":"x"}"#)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    finish!(h);
}

#[tokio::test]
async fn workspace_admin_can_manage_users() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (status, body) = h
        .post(
            "/v1/users",
            Some(&token),
            r#"{"email":"new@acme.example","display_name":"New","password":"correct-horse","role":"viewer"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    let (status, body) = h.get("/v1/users", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("new@acme.example"), "body: {body}");

    finish!(h);
}

/// A workspace's administrator sees their own workspace's accounts and nobody else's.
///
/// The user list used to be every account on the platform, which named every
/// other customer's staff to anyone holding users:read in any workspace.
#[tokio::test]
async fn a_workspace_admin_sees_only_their_own_workspaces_users() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let globex = h.make_workspace("Globex", "globex").await;
    let acme_admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    h.login_as("someone@globex.example", None, Some((globex, "viewer")))
        .await;

    let (status, body) = h.get("/v1/users", Some(&acme_admin)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("admin@acme.example"), "body: {body}");
    assert!(
        !body.contains("globex.example"),
        "another workspace's account was listed: {body}"
    );

    // A system administrator still sees everyone.
    let root = h
        .login_as(
            "root@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let (status, body) = h.get("/v1/users", Some(&root)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("globex.example"), "body: {body}");

    finish!(h);
}

/// The user routes stop at the workspace boundary.
///
/// They used to resolve their target by id alone, so anyone holding
/// `users:update` in one workspace could attach a password to any account on
/// the platform and sign in as them. The boundary answers 404, not 403, so a
/// guessed id learns nothing about who is where.
#[tokio::test]
async fn a_workspace_admin_cannot_reach_another_workspaces_users() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let globex = h.make_workspace("Globex", "globex").await;
    let acme_admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    h.login_as("admin@globex.example", None, Some((globex, "admin")))
        .await;
    let victim = h
        .users
        .get_in_workspace(
            h.users
                .list_for_workspace(globex, None, 10)
                .await
                .expect("list")[0]
                .id,
            globex,
        )
        .await
        .expect("the globex admin");

    let (status, body) = h
        .post(
            &format!("/v1/users/{}/identities", victim.id),
            Some(&acme_admin),
            r#"{"email":"takeover@acme.example","password":"correct-horse"}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "identity added across workspaces: {body}"
    );
    let (status, body) = h
        .post(
            "/v1/login",
            None,
            &format!(
                r#"{{"email":"takeover@acme.example","password":"correct-horse","workspace_id":"{globex}"}}"#
            ),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the takeover address signs in: {body}"
    );

    let (status, body) = h
        .get(&format!("/v1/users/{}", victim.id), Some(&acme_admin))
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "read across workspaces: {body}"
    );

    let (status, body) = h
        .patch(
            &format!("/v1/users/{}", victim.id),
            Some(&acme_admin),
            r#"{"display_name":"Renamed"}"#,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "renamed across workspaces: {body}"
    );

    let req = Request::builder()
        .method("DELETE")
        .uri(format!(
            "/v1/users/{}/identities/{}",
            victim.id, victim.identities[0].id
        ))
        .header("authorization", format!("Bearer {acme_admin}"))
        .body(Body::empty())
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "identity removed across workspaces: {body}"
    );

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/users/{}", victim.id))
        .header("authorization", format!("Bearer {acme_admin}"))
        .body(Body::empty())
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "deleted across workspaces: {body}"
    );
    assert_eq!(
        h.users
            .get(victim.id)
            .await
            .expect("still there")
            .display_name,
        "Test User"
    );

    // A system administrator is scoped to one workspace but administers all.
    let root = h
        .login_as(
            "root@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let (status, body) = h
        .get(&format!("/v1/users/{}", victim.id), Some(&root))
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let (status, body) = h
        .patch(
            &format!("/v1/users/{}", victim.id),
            Some(&root),
            r#"{"display_name":"Renamed"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    finish!(h);
}

#[tokio::test]
async fn viewer_cannot_manage_users() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("viewer@acme.example", None, Some((acme, "viewer")))
        .await;

    let (status, _) = h.get("/v1/users", Some(&token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    finish!(h);
}

#[tokio::test]
async fn duplicate_email_conflicts() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let token = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let body = r#"{"email":"dup@acme.example","display_name":"Dup","password":"correct-horse"}"#;
    let (status, _) = h.post("/v1/users", Some(&token), body).await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _) = h.post("/v1/users", Some(&token), body).await;
    assert_eq!(status, StatusCode::CONFLICT);

    finish!(h);
}

/// The platform's own row is not a workspace to manage: it is not listed, and
/// it cannot be opened, renamed or deleted as one. Deleting it was refused
/// before this, by a trigger, as an internal error.
#[tokio::test]
async fn the_platform_workspace_is_not_managed_as_a_workspace() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("root@test.invalid", Some(Role::SystemAdmin), None)
        .await;
    let platform = outturn::api::usage::PLATFORM_WORKSPACE;

    let (status, body) = h.get("/v1/workspaces", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed: Vec<String> = items(&body)
        .iter()
        .map(|w| w["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(listed, vec![acme.to_string()], "{body}");

    let (status, _) = h
        .get(&format!("/v1/workspaces/{platform}"), Some(&admin))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let req = Request::builder()
        .method("PATCH")
        .uri(format!("/v1/workspaces/{platform}"))
        .header("authorization", format!("Bearer {admin}"))
        .header("content-type", "application/json")
        .body(Body::from(r#"{"name":"Renamed"}"#))
        .expect("request");
    let (status, _) = h.send(req).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the platform workspace was renamed"
    );
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/v1/workspaces/{platform}"))
        .header("authorization", format!("Bearer {admin}"))
        .body(Body::empty())
        .expect("request");
    let (status, _) = h.send(req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A real workspace still opens.
    let (status, _) = h.get(&format!("/v1/workspaces/{acme}"), Some(&admin)).await;
    assert_eq!(status, StatusCode::OK);

    h.db.cleanup().await;
}
