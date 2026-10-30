//! Uploads, HEIC conversion and the session file sweep.

use super::*;

// -- Files -----------------------------------------------------------------------

/// A file a person uploads is one the agent can name, and who may write where
/// follows the storage authorities.
#[tokio::test]
async fn uploads_land_in_the_scope_the_agent_reads_them_from() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let viewer = h
        .login_as("viewer@acme.example", None, Some((acme, "viewer")))
        .await;

    let (_, body) = h
        .post("/v1/agents", Some(&admin), r#"{"name":"A","slug":"a"}"#)
        .await;
    let agent: serde_json::Value = serde_json::from_str(&body).expect("agent");
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(
                r#"{{"agent_id":"{}","title":""}}"#,
                agent["id"].as_str().expect("id")
            ),
        )
        .await;
    let session: serde_json::Value = serde_json::from_str(&body).expect("session");
    let session_id = session["id"].as_str().expect("id");

    let put = |token: &str, scoped: &str, bytes: &str| {
        Request::builder()
            .method("PUT")
            .uri(format!("/v1/agent-sessions/{session_id}/files/{scoped}"))
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/octet-stream")
            .body(Body::from(bytes.to_string()))
            .expect("request")
    };

    // The admin puts a file in session scope; it lists under the name the
    // agent would use.
    let (status, body) = h.send(put(&admin, "session/report.txt", "quarterly")).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert!(
        body.contains(r#""path":"session/report.txt""#),
        "body: {body}"
    );

    let (status, body) = h
        .get(
            &format!("/v1/agent-sessions/{session_id}/files"),
            Some(&admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.contains("session/report.txt"), "body: {body}");

    // A page at a time, per scope, with a cursor to go on from.
    for name in ["a.txt", "b.txt"] {
        let (status, body) = h.send(put(&admin, &format!("session/{name}"), "x")).await;
        assert_eq!(status, StatusCode::CREATED, "body: {body}");
    }
    let page = |q: String| {
        let admin = &admin;
        let h = &h;
        async move {
            let (status, body) = h
                .get(
                    &format!("/v1/agent-sessions/{session_id}/files?{q}"),
                    Some(admin),
                )
                .await;
            assert_eq!(status, StatusCode::OK, "body: {body}");
            serde_json::from_str::<serde_json::Value>(&body).unwrap()
        }
    };
    let first = page("scope=session&limit=2".into()).await;
    assert_eq!(first["items"].as_array().unwrap().len(), 2, "{first}");
    assert_eq!(first["more"]["session"], "b.txt", "{first}");
    let rest = page("scope=session&limit=2&after=b.txt".into()).await;
    assert_eq!(rest["items"][0]["path"], "session/report.txt", "{rest}");
    assert!(rest["more"].get("session").is_none(), "{rest}");

    // It comes back out as bytes.
    let (status, body) = h
        .get(
            &format!("/v1/agent-sessions/{session_id}/files/session/report.txt"),
            Some(&admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "quarterly");

    // A viewer may read the conversation's files but not add to the
    // workspace's; the admin may.
    let (status, body) = h.send(put(&viewer, "workspace/pricing.csv", "x")).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a viewer wrote workspace files: {body}"
    );
    let (status, body) = h.send(put(&viewer, "session/mine.txt", "x")).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a viewer wrote session files without sessions:create: {body}"
    );
    let (status, body) = h.send(put(&admin, "workspace/pricing.csv", "x")).await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");

    // Traversal in an upload path is refused, not repaired.
    let (status, _) = h
        .send(put(&admin, "session/../workspace/oops.txt", "x"))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    finish!(h);
}

/// A HEIC photo's first bytes: enough for the API to know it, and nothing a
/// decoder could read, which is the fake converter's business.
const HEIC: &[u8] = b"\0\0\0\x18ftypheic\0\0\0\0mif1heic";

/// What the fake converter answers a photo with: a JPEG's magic and a marker.
const CONVERTED: &[u8] = b"\xff\xd8\xff\xe0 converted";

/// A converter that answers every photo with `status` and `reply`, as the
/// real one's service would, and where to reach it.
async fn fake_converter(status: u16, reply: &'static [u8]) -> String {
    let app = axum::Router::new().route(
        "/v1/convert/jpeg",
        axum::routing::post(move || async move { (StatusCode::from_u16(status).unwrap(), reply) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

/// A workspace admin and a conversation of theirs to upload into.
async fn upload_session(h: &Harness) -> (String, String) {
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let (_, body) = h
        .post("/v1/agents", Some(&admin), r#"{"name":"A","slug":"a"}"#)
        .await;
    let agent: Value = serde_json::from_str(&body).expect("agent");
    let (_, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(
                r#"{{"agent_id":"{}","title":""}}"#,
                agent["id"].as_str().expect("id")
            ),
        )
        .await;
    let session: Value = serde_json::from_str(&body).expect("session");
    (admin, session["id"].as_str().expect("id").to_string())
}

impl Harness {
    /// Puts `bytes` at `scoped` in a conversation's files.
    async fn put_file(
        &self,
        token: &str,
        session_id: &str,
        scoped: &str,
        bytes: &[u8],
    ) -> (StatusCode, String) {
        self.send(
            Request::builder()
                .method("PUT")
                .uri(format!("/v1/agent-sessions/{session_id}/files/{scoped}"))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(bytes.to_vec()))
                .expect("request"),
        )
        .await
    }

    /// The paths of a conversation's session files.
    async fn file_paths(&self, token: &str, session_id: &str) -> Vec<String> {
        let (_, body) = self
            .get(
                &format!("/v1/agent-sessions/{session_id}/files"),
                Some(token),
            )
            .await;
        items(&body)
            .iter()
            .map(|f| f["path"].as_str().expect("path").to_string())
            .collect()
    }
}

/// Without a converter, a HEIC photo is refused when it is uploaded, by its
/// bytes and whatever it is called, rather than stored as a file few models
/// can read and most browsers cannot show.
#[tokio::test]
async fn a_heic_photo_is_refused_at_upload_where_nothing_converts_it() {
    let h = harness_or_skip!();
    let (admin, session) = upload_session(&h).await;

    for name in ["IMG_1234.HEIC", "photo.jpg"] {
        let (status, body) = h
            .put_file(&admin, &session, &format!("session/{name}"), HEIC)
            .await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{name}: {body}");
        assert!(body.contains("HEIC"), "{name}: {body}");
    }
    assert!(h.file_paths(&admin, &session).await.is_empty());
    finish!(h);
}

/// With a converter, the JPEG is what the upload answers with, the original
/// is kept beside it, and neither lands on a file that was already there.
#[tokio::test]
async fn a_heic_photo_is_stored_as_a_jpeg_beside_its_original() {
    let h = harness_converting_at(&fake_converter(200, CONVERTED).await).await;
    let (admin, session) = upload_session(&h).await;

    let (status, body) = h
        .put_file(&admin, &session, "session/IMG_1234.HEIC", HEIC)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let stored: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(stored["path"], "session/IMG_1234.jpg", "{body}");
    assert_eq!(stored["converted_from"], "session/IMG_1234.HEIC", "{body}");
    let (_, jpeg) = h
        .get(
            &format!("/v1/agent-sessions/{session}/files/session/IMG_1234.jpg"),
            Some(&admin),
        )
        .await;
    // Read back as text, so only the marker after the JPEG magic survives.
    assert!(jpeg.ends_with(" converted"), "{jpeg}");

    // A JPEG somebody put there is theirs: the next conversion of that name
    // takes another one.
    h.put_file(&admin, &session, "session/trip.jpg", b"\xff\xd8\xff mine")
        .await;
    let (status, body) = h
        .put_file(&admin, &session, "session/trip.heic", HEIC)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body.contains(r#""path":"session/trip-2.jpg""#), "{body}");
    let (_, mine) = h
        .get(
            &format!("/v1/agent-sessions/{session}/files/session/trip.jpg"),
            Some(&admin),
        )
        .await;
    assert!(mine.ends_with(" mine"), "the existing file was replaced");

    // A HEIC sent under a JPEG's name: the original is named for what it is,
    // and is kept rather than replaced by its own conversion.
    let (status, body) = h
        .put_file(&admin, &session, "session/photo.jpg", HEIC)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(
        body.contains(r#""converted_from":"session/photo.heic""#),
        "{body}"
    );

    let mut paths = h.file_paths(&admin, &session).await;
    paths.sort();
    assert_eq!(
        paths,
        [
            "session/IMG_1234.HEIC",
            "session/IMG_1234.jpg",
            "session/photo.heic",
            "session/photo.jpg",
            "session/trip-2.jpg",
            "session/trip.heic",
            "session/trip.jpg",
        ]
    );
    finish!(h);
}

/// A converter that is busy is said to be busy, not the photo to be broken;
/// one that answers with something other than a JPEG is not believed. Either
/// way nothing is stored.
#[tokio::test]
async fn a_converter_that_cannot_answer_stores_nothing() {
    for (status, reply, expected) in [
        (
            503,
            &b"the converter is busy; try again shortly"[..],
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            502,
            &b"<html>502 Bad Gateway</html>"[..],
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            200,
            &b"<html>not a picture</html>"[..],
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let h = harness_converting_at(&fake_converter(status, reply).await).await;
        let (admin, session) = upload_session(&h).await;
        let (got, body) = h
            .put_file(&admin, &session, "session/IMG_1234.HEIC", HEIC)
            .await;
        assert_eq!(got, expected, "converter said {status}: {body}");
        assert!(!body.contains("<html>"), "passed on as it came: {body}");
        assert!(h.file_paths(&admin, &session).await.is_empty());
        finish!(h);
    }
}

/// Session files are swept from the session's last activity, not their age:
/// an idle session's go, with the text read out of them; one with work still
/// to come keeps them however idle it looks; and a deleted session's go too,
/// though its row is no longer there to say when it was active.
#[tokio::test]
async fn files_are_swept_by_activity_and_by_deletion() {
    use outturn::storage::{MemoryStorage, StorageBackend};

    let h = harness_or_skip!();
    let pool = &h.db.pool;
    let workspace = h.make_workspace("Acme", "acme").await;
    let agent = h
        .agents
        .create(
            workspace,
            outturn::api::agent::CreateAgent {
                name: "Clerk".into(),
                slug: "clerk".into(),
                description: String::new(),
                system_prompt: String::new(),
                policy: None,
            },
        )
        .await
        .expect("agent")
        .id;
    let store: Arc<dyn StorageBackend> = Arc::new(MemoryStorage::new());
    let session = |idle_days: i32| async move {
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into agent_sessions (id, workspace_id, agent_id, last_active_at) \
             values ($1, $2, $3, now() - make_interval(days => $4))",
        )
        .bind(id)
        .bind(workspace)
        .bind(agent)
        .bind(idle_days)
        .execute(pool)
        .await
        .expect("session");
        id
    };
    let file = |id: Uuid| format!("sessions/{workspace}/{agent}/{id}/notes.txt");
    let text = |id: Uuid| format!("extracted/{}", file(id));
    let there = |key: String| {
        let store = store.clone();
        async move { store.read(&key, 0, u32::MAX).await.is_ok() }
    };

    let idle = session(40).await;
    let lively = session(1).await;
    let waiting = session(40).await;
    let deleted = session(1).await;
    for id in [idle, lively, waiting, deleted] {
        store.write(&file(id), b"notes").await.unwrap();
        store.write(&text(id), b"notes").await.unwrap();
    }
    // A wakeup it asked for: idle by its messages, not by what is to come.
    sqlx::query(
        "insert into jobs (id, workspace_id, kind, payload) values ($1, $2, 'chat.wake', $3)",
    )
    .bind(Uuid::now_v7())
    .bind(workspace)
    .bind(serde_json::json!({ "session_id": waiting }))
    .execute(pool)
    .await
    .expect("job");
    sqlx::query("delete from agent_sessions where id = $1")
        .bind(deleted)
        .execute(pool)
        .await
        .expect("delete");

    outturn::api::storage_sweep::sweep(pool, &store, 30).await;

    assert!(!there(file(idle)).await, "an idle session's file stayed");
    assert!(!there(text(idle)).await, "an idle session's text stayed");
    assert!(there(file(lively)).await, "a lively session's file went");
    assert!(
        there(file(waiting)).await,
        "a session waiting to wake lost its file"
    );
    assert!(
        !there(file(deleted)).await,
        "a deleted session's file stayed"
    );
    assert!(
        !there(text(deleted)).await,
        "a deleted session's text stayed"
    );
    let left: i64 = sqlx::query_scalar("select count(*) from storage_deletions")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "a swept deletion was kept");

    // Used again after a sweep, it keeps what it makes now; idle for the
    // window once more, it is swept once more.
    let set_idle = |days: i32| async move {
        sqlx::query(
            "update agent_sessions set last_active_at = now() - make_interval(days => $2) \
             where id = $1",
        )
        .bind(idle)
        .bind(days)
        .execute(pool)
        .await
        .unwrap();
    };
    set_idle(0).await;
    store.write(&file(idle), b"new notes").await.unwrap();
    outturn::api::storage_sweep::sweep(pool, &store, 30).await;
    assert!(there(file(idle)).await, "a session in use again was swept");
    // Its last sweep was a moment ago, and now it was last active before
    // that by more than the window: what a month of quiet looks like.
    sqlx::query(
        "update agent_sessions set files_swept_at = now() - interval '41 days' where id = $1",
    )
    .bind(idle)
    .execute(pool)
    .await
    .unwrap();
    set_idle(40).await;
    outturn::api::storage_sweep::sweep(pool, &store, 30).await;
    assert!(!there(file(idle)).await, "idle again, and not swept again");
}
