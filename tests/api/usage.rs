//! The usage ledger and its export.

use super::*;

// -- Usage ledger ---------------------------------------------------------------

/// Every model call a turn reports becomes a ledger row, tagged with the
/// session's account, and the export pages through them by cursor.
#[tokio::test]
async fn each_model_call_is_written_to_the_ledger_and_exported() {
    let h = harness_or_skip!();
    let acme = h.make_workspace("Acme", "acme").await;
    let admin = h
        .login_as("admin@acme.example", None, Some((acme, "admin")))
        .await;

    let (_, body) = h
        .post(
            "/v1/agents",
            Some(&admin),
            r#"{"name":"A","slug":"a","policy":{"model":"test-model"}}"#,
        )
        .await;
    let agent: serde_json::Value = serde_json::from_str(&body).expect("agent");
    let (status, body) = h
        .post(
            "/v1/agent-sessions",
            Some(&admin),
            &format!(
                r#"{{"agent_id":"{}","title":"","account":"shipper-northvale"}}"#,
                agent["id"].as_str().expect("id")
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    let session: serde_json::Value = serde_json::from_str(&body).expect("session");
    assert_eq!(
        session["account"], "shipper-northvale",
        "the account was not stored: {body}"
    );
    let session_id = session["id"].as_str().expect("id");

    let (status, _) = h
        .post(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
            r#"{"content":"hello"}"#,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let runtime = h.runtime_token(acme);
    let (status, body) = h.post("/v1/work", Some(&runtime), "{}").await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let assignment: serde_json::Value = serde_json::from_str(&body).expect("assignment");
    let job = assignment["job_id"].as_str().expect("job");
    let lease = assignment["lease_token"].as_str().expect("lease");

    // A turn of two model calls: the second fell back to another endpoint.
    let stream = [
        r#"{"kind":"delta","idx":0,"text":"Hi"}"#,
        r#"{"kind":"usage","round":0,"endpoint":"openai:http://vllm:8000","model":"qwen","paid_by":"operator","prompt_tokens":10,"completion_tokens":2,"cache_read_tokens":0,"cache_write_tokens":0,"reasoning_tokens":0}"#,
        r#"{"kind":"usage","round":1,"endpoint":"anthropic:https://api.anthropic.com","model":"claude-sonnet-5","paid_by":"operator","prompt_tokens":20,"completion_tokens":5,"cache_read_tokens":3,"cache_write_tokens":0,"reasoning_tokens":1,"service_tier":"priority","provider_usage":{"input_tokens":20,"cache_creation":{"ephemeral_1h_input_tokens":7}}}"#,
        r#"{"kind":"done","content":"Hi","prompt_tokens":30,"completion_tokens":7,"cache_read_tokens":3,"cache_write_tokens":0,"reasoning_tokens":1,"provider":"anthropic:https://api.anthropic.com","model":"claude-sonnet-5"}"#,
    ]
    .join("\n");
    let req = Request::builder()
        .method("POST")
        .uri(format!("/v1/work/{job}/events"))
        .header("authorization", format!("Bearer {runtime}"))
        .header(outturn::api::work::LEASE_HEADER, lease)
        .body(Body::from(format!("{stream}\n")))
        .expect("request");
    let (status, body) = h.send(req).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "body: {body}");

    // The reply names the model that served its last round, not the one the
    // agent was configured with.
    let (status, body) = h
        .get(
            &format!("/v1/agent-sessions/{session_id}/messages"),
            Some(&admin),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body.contains(r#""role":"assistant""#) && body.contains(r#""model":"claude-sonnet-5""#),
        "the reply was not credited to the model that served it: {body}"
    );

    // Paged one at a time, to prove the cursor.
    let (status, body) = h.get("/v1/usage?limit=1", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let page: serde_json::Value = serde_json::from_str(&body).expect("page");
    let first = &page["entries"][0];
    assert_eq!(first["round"], 0);
    assert_eq!(first["model"], "qwen");
    assert_eq!(first["account"], "shipper-northvale");
    assert_eq!(first["session_id"], session_id);
    assert_eq!(first["credential_owner"], "operator");
    assert_eq!(first["prompt_tokens"], 10);
    // This round carried no usage object, so its numbers came from nowhere a
    // bill can be argued from. The row says so rather than presenting them as
    // measured.
    assert_eq!(
        first["usage_source"], "unknown",
        "a round with no provider usage was recorded as though somebody had \
         counted it: {body}"
    );
    let next = page["next"].as_str().expect("cursor");

    let (status, body) = h
        .get(&format!("/v1/usage?limit=1&after={next}"), Some(&admin))
        .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let page: serde_json::Value = serde_json::from_str(&body).expect("page");
    assert_eq!(page["entries"][0]["round"], 1);
    assert_eq!(page["entries"][0]["model"], "claude-sonnet-5");
    assert_eq!(page["entries"][0]["reasoning_tokens"], 1);
    // What the provider said, verbatim, beside what was normalized from it.
    assert_eq!(page["entries"][0]["service_tier"], "priority");
    assert_eq!(
        page["entries"][0]["provider_usage"]["cache_creation"]["ephemeral_1h_input_tokens"], 7,
        "the raw usage object was not kept: {body}"
    );
    let next = page["next"].as_str().expect("cursor");

    let (_, body) = h
        .get(&format!("/v1/usage?limit=1&after={next}"), Some(&admin))
        .await;
    let page: serde_json::Value = serde_json::from_str(&body).expect("page");
    assert!(
        page["next"].is_null(),
        "the ledger should be exhausted: {body}"
    );

    // Another workspace's ledger is not this admin's to read.
    let globex = h.make_workspace("Globex", "globex").await;
    let (status, _) = h
        .get(&format!("/v1/usage?workspace_id={globex}"), Some(&admin))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The summary is a reading of the same rows: what the export pages, these
    // figures add up.
    let (status, body) = h.get("/v1/usage/summary", Some(&admin)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let summary: serde_json::Value = serde_json::from_str(&body).expect("summary");
    assert_eq!(
        summary["totals"]["calls"], 2,
        "both rounds should be counted: {body}"
    );
    assert_eq!(summary["totals"]["prompt_tokens"], 30);
    assert_eq!(summary["totals"]["completion_tokens"], 7);
    assert_eq!(summary["totals"]["sessions"], 1);

    // Every day of the window is present, including the ones nothing happened
    // on -- a chart drawn from a series that skips them draws an idle day as no
    // day at all.
    let daily = summary["daily"].as_array().expect("daily");
    assert_eq!(
        daily.len(),
        30,
        "the default window is 30 days of buckets: {body}"
    );
    // The last bucket is today's, whole rather than cut off at the moment of
    // the call, which is why the window ends at the next midnight.
    assert_eq!(
        daily.last().expect("a bucket")["calls"],
        2,
        "the turn just recorded belongs in the last bucket: {body}"
    );
    let charged: i64 = daily
        .iter()
        .map(|d| d["prompt_tokens"].as_i64().expect("tokens"))
        .sum();
    assert_eq!(
        charged, 30,
        "the daily series must add up to the total above it: {body}"
    );

    // Every bucket opens at midnight UTC, whatever timezone the database
    // session is in. `date_trunc` in its two-argument form reads the session's
    // TimeZone, which nothing in this path sets, so a deployment whose server
    // or pooler defaults elsewhere would shift every boundary -- and the
    // browser, which labels buckets in UTC, would print the wrong day against
    // the right figures. The explicit zone in the query is what pins this.
    for day in daily {
        let at = day["at"].as_str().expect("a bucket opens somewhere");
        assert!(
            at.ends_with("T00:00:00Z"),
            "a bucket opened at {at} rather than at midnight UTC: {body}"
        );
    }

    // The slices carry names, not just ids, and the account label the session
    // was opened with.
    let by_model = summary["by_model"].as_array().expect("by_model");
    assert_eq!(by_model.len(), 2, "two models answered: {body}");
    let models: Vec<&str> = by_model
        .iter()
        .map(|m| m["key"].as_str().expect("key"))
        .collect();
    assert!(
        models.contains(&"qwen") && models.contains(&"claude-sonnet-5"),
        "{body}"
    );
    assert_eq!(
        summary["by_account"][0]["key"], "shipper-northvale",
        "{body}"
    );
    assert_eq!(
        summary["by_workspace"][0]["label"], "Acme",
        "the workspace was not named: {body}"
    );

    // Whose spend it was. A turn's own rounds bill to the agent that ran them,
    // and both carry the session's account label -- which is what a workspace
    // joins its bill to its own records by, so a row missing it is spend they
    // cannot attribute to anybody.
    let (_, body) = h.get("/v1/usage?limit=100", Some(&admin)).await;
    let page: serde_json::Value = serde_json::from_str(&body).expect("page");
    for entry in page["entries"].as_array().expect("entries") {
        assert_eq!(
            entry["account"], "shipper-northvale",
            "a row the session produced lost its account label: {entry}"
        );
        // Every row a session produced bills to that session's agent, whoever
        // started the call. A session cannot exist without one, so a null here
        // is spend nobody can explain rather than spend nobody owns -- and the
        // platform's own work (naming, compaction) is still done for one
        // agent's conversation.
        assert!(
            !entry["agent_id"].is_null(),
            "a row a session produced was left unattributed: {entry}"
        );
    }

    // Scope is what widens a summary, and it is not this admin's to ask for.
    let (status, _) = h.get("/v1/usage/summary?scope=all", Some(&admin)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a workspace administrator must not see every workspace"
    );

    // The operator may. Globex has no rows, so the platform total is still Acme's.
    let root = h
        .login_as(
            "root@example.com",
            Some(Role::SystemAdmin),
            Some((acme, "admin")),
        )
        .await;
    let (status, body) = h.get("/v1/usage/summary?scope=all", Some(&root)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let platform: serde_json::Value = serde_json::from_str(&body).expect("summary");
    assert_eq!(platform["totals"]["calls"], 2, "body: {body}");

    // A window that does not close is refused rather than guessed at.
    let (status, _) = h
        .get(
            "/v1/usage/summary?from=2026-02-01T00:00:00Z&to=2026-01-01T00:00:00Z",
            Some(&admin),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    finish!(h);
}
