//! Files sent in pieces: resumed, checked, and landed as a single upload is.

use super::*;

const PIECE: usize = 16 * 1024 * 1024;

/// A session the admin of a new workspace may write into, and a token for a
/// second person in the same workspace.
async fn session(h: &Harness) -> (String, String, String) {
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;
    let other = h
        .login_as("other@acme.example", None, Some((acme, "admin")))
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
    (
        session["id"].as_str().expect("id").to_string(),
        admin,
        other,
    )
}

fn piece(session: &str, upload: &str, n: u32, token: &str, bytes: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(format!(
            "/v1/agent-sessions/{session}/uploads/{upload}/pieces/{n}"
        ))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/octet-stream")
        .body(Body::from(bytes))
        .expect("request")
}

#[tokio::test]
async fn a_file_sent_in_pieces_lands_whole_whatever_order_they_arrive_in() {
    let h = harness_or_skip!();
    let (session, admin, other) = session(&h).await;
    let file: Vec<u8> = (0..PIECE + 10).map(|i| (i % 251) as u8).collect();

    let (status, body) = h
        .post(
            &format!("/v1/agent-sessions/{session}/uploads"),
            Some(&admin),
            &format!(
                r#"{{"scope":"agent","path":"onboarding/export.bin","size":{}}}"#,
                file.len()
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let started: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(started["pieces"], 2, "{body}");
    let upload = started["upload_id"].as_str().unwrap().to_string();

    // The last piece first; then a piece of the wrong length is refused.
    let (status, body) = h
        .send(piece(&session, &upload, 2, &admin, file[PIECE..].to_vec()))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (status, _) = h
        .send(piece(&session, &upload, 1, &admin, vec![0; 10]))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a short middle piece");
    let (status, _) = h
        .send(piece(&session, &upload, 3, &admin, vec![0; 1]))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a piece past the end");

    // Nobody else's to send to, or to ask about.
    let (status, _) = h
        .send(piece(&session, &upload, 1, &other, file[..PIECE].to_vec()))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "another person's upload");

    // Not complete while a piece is missing, and says what arrived.
    let complete = format!("/v1/agent-sessions/{session}/uploads/{upload}/complete");
    let (status, _) = h.post(&complete, Some(&admin), "").await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, body) = h
        .get(
            &format!("/v1/agent-sessions/{session}/uploads/{upload}"),
            Some(&admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let progress: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(progress["received"], serde_json::json!([2]), "{body}");

    // A piece sent twice is the same piece.
    for _ in 0..2 {
        let (status, body) = h
            .send(piece(&session, &upload, 1, &admin, file[..PIECE].to_vec()))
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    }
    let (status, body) = h.post(&complete, Some(&admin), "").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(
        body.contains(r#""path":"agent/onboarding/export.bin""#),
        "{body}"
    );
    let (status, _) = h.post(&complete, Some(&admin), "").await;
    assert_eq!(status, StatusCode::CONFLICT, "completed once");

    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/v1/agent-sessions/{session}/files/agent/onboarding/export.bin"
                ))
                .header("authorization", format!("Bearer {admin}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let got = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(got == file, "the file is the pieces, in order");
    finish!(h);
}

#[tokio::test]
async fn an_abandoned_upload_takes_no_more_pieces() {
    let h = harness_or_skip!();
    let (session, admin, _) = session(&h).await;
    let (_, body) = h
        .post(
            &format!("/v1/agent-sessions/{session}/uploads"),
            Some(&admin),
            r#"{"scope":"session","path":"big.zip","size":100}"#,
        )
        .await;
    let upload = serde_json::from_str::<serde_json::Value>(&body).unwrap()["upload_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = h
        .send(
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/agent-sessions/{session}/uploads/{upload}"))
                .header("authorization", format!("Bearer {admin}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = h
        .send(piece(&session, &upload, 1, &admin, vec![0; 100]))
        .await;
    assert_eq!(status, StatusCode::GONE);
    finish!(h);
}

#[tokio::test]
async fn an_upload_too_large_for_the_quota_is_refused_before_it_starts() {
    let h = harness_or_skip!();
    let (session, admin, _) = session(&h).await;
    let (status, body) = h
        .post(
            &format!("/v1/agent-sessions/{session}/uploads"),
            Some(&admin),
            r#"{"scope":"session","path":"huge.bin","size":99999999999999}"#,
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    finish!(h);
}
