//! Outbound requests: matched and proven here, made by the gateway, with
//! the idempotency every declared write gets.

use super::{AgentHost, GatedRequest, HttpRequest, HttpResponse, MAX_FETCHES_PER_TURN};

/// A request as `dispatch` sent it, keyed and all.
struct Sent {
    method: String,
    url: String,
    host: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Option<String>,
    operation: Option<String>,
}

/// What `dispatch` made of a request.
enum Dispatched {
    /// A declared write already done, answered from the record.
    Replayed(HttpResponse),
    /// A declared write sent before with no answer, so not sent again.
    Attempted(String),
    /// Through the gateway, and whatever it said.
    Sent(Result<crate::fetch::GatewayFetched, crate::fetch::Unanswered>),
}

impl AgentHost {
    /// Sends one request through the gateway, under the token and gates given,
    /// with the idempotency handling every declared write gets.
    ///
    /// Shared by the guest's own calls and by an approved request sent before
    /// the guest runs, so the second is the same write as the first would have
    /// been: keyed the same, claimed against the same record, and settled by
    /// the same rule.
    async fn dispatch(
        &mut self,
        request: HttpRequest,
        token: String,
        gates: outturn_core::egress::gate::RawGates,
    ) -> Result<(Sent, Dispatched), String> {
        use outturn_core::egress::rule as egress;

        // This tier makes no outbound request of its own. It runs workspace
        // code, so a socket here is a socket a compromised guest's host could
        // use for anything -- and an approval it was trusted to honor would
        // be worth nothing, because it could simply not ask. The gateway makes
        // the call: it holds the credentials, it can read the commitment out
        // of the turn token this presents, and it is the tier that never runs
        // anybody's code. The cluster should say the same thing with a
        // NetworkPolicy, so this is a property of the network rather than of
        // this function staying honest.
        //
        // What is still done here is finding which rule the URL matches, and
        // building the proof for it. Neither is trusted: the gateway matches
        // again against the rules that verified, and a proof for a rule the
        // API never committed to verifies nowhere. Doing it here only saves
        // sending a workspace's whole rule list on every fetch.
        let url =
            reqwest::Url::parse(&request.url).map_err(|e| format!("that URL is not one: {e}"))?;
        let (_host, rule) = egress::check_url(&self.egress, &url).map_err(|e| e.to_string())?;
        let proof = outturn_core::egress::commit::prove(self.workspace_id, &self.egress, rule)
            .ok_or_else(|| {
                // The rule is in the list this holds but not in what the API
                // vouched for, so nothing can be shown about it. Refused here
                // rather than sent, since the gateway would refuse it anyway
                // and the message is the same fact about the workspace's
                // settings either way.
                egress::Refused::Unproven(rule.host.clone()).to_string()
            })?;

        // A declared write: keyed for a recipient that takes a key, or
        // checked against the record for one that does not.
        let mut request = request;
        let identity = request
            .operation
            .as_deref()
            .and_then(|name| self.skill_operations.iter().find(|o| o.name == name))
            .filter(|op| !op.identity.is_empty())
            .cloned();
        let mut claimed: Option<String> = None;
        let mut replay: Option<Dispatched> = None;
        if let Some(op) = &identity {
            let key = idempotency_key(op, &url, request.body.as_deref())?;
            if let Some(header) = &op.key_header {
                request
                    .headers
                    .retain(|(n, _)| !n.eq_ignore_ascii_case(header));
                request.headers.push((header.clone(), key));
            } else if let Some(field) = &op.key_field {
                request.body = Some(keyed_body(request.body.as_deref(), field, &op.name, &key)?);
            } else if let Some(record) = &self.invocations {
                match record.claim(key.clone()).await? {
                    outturn_core::protocol::work::Claimed::New => claimed = Some(key),
                    outturn_core::protocol::work::Claimed::Completed { response, at } => {
                        replay = Some(Dispatched::Replayed(replayed(&response, at)));
                    }
                    outturn_core::protocol::work::Claimed::Attempted { at } => {
                        replay = Some(Dispatched::Attempted(format!(
                            "this exact {} was sent at {at} and its answer never came back, so \
                             whether it happened is not known. Check before sending it again: \
                             it is not sent twice from here.",
                            op.name
                        )));
                    }
                }
            }
        }

        let sent = Sent {
            method: request.method.clone(),
            url: request.url.clone(),
            host: url.host_str().unwrap_or_default().to_string(),
            path: url.path().to_string(),
            headers: request.headers.clone(),
            body: request.body.clone(),
            operation: request.operation.clone(),
        };
        if let Some(replay) = replay {
            return Ok((sent, replay));
        }

        let outcome = crate::fetch::through_gateway(
            &self.gateway_url,
            &token,
            crate::fetch::GatewayFetch {
                method: request.method,
                url: request.url,
                headers: request.headers,
                body: request.body,
                proof,
                gates,
            },
        )
        .await;

        // What became of a claimed write. Answered and taken, it is recorded
        // to answer a repeat with; refused by the recipient (4xx) or by the
        // gateway, or never sent at all, nothing happened and the claim is
        // released. A connection that broke after sending is left `attempted`,
        // and so is a 5xx: it may have landed, since a proxy's 502 or 504 often
        // follows a write the server behind it committed.
        if let (Some(key), Some(record)) = (claimed, &self.invocations) {
            match &outcome {
                Ok(answer) if (200..300).contains(&answer.status) => {
                    record
                        .settle(
                            key,
                            Some(serde_json::json!({
                                "status": answer.status,
                                "headers": answer.headers,
                                "body": answer.body,
                                "truncated": answer.truncated,
                            })),
                        )
                        .await
                }
                Ok(answer) if (400..500).contains(&answer.status) => record.settle(key, None).await,
                Ok(_) => {}
                Err(why) if why.did_not_complete => {}
                Err(_) => record.settle(key, None).await,
            }
        }

        Ok((sent, Dispatched::Sent(outcome)))
    }

    /// Sends a request somebody approved, before the guest runs, and says what
    /// became of it.
    ///
    /// Under the token and gates the API minted for exactly these requests, so
    /// the grant that lets one out is not in the commitment the guest's own
    /// calls travel under: the model reading the answer and making the call
    /// again is refused, and asks again, rather than charging twice.
    pub(crate) async fn send_held(
        &mut self,
        call: &outturn_core::egress::held::HeldCall,
        token: String,
        gates: outturn_core::egress::gate::RawGates,
    ) -> outturn_core::egress::held::Outcome {
        use outturn_core::egress::held::Outcome;

        let request = HttpRequest {
            method: call.method.clone(),
            url: call.url.clone(),
            headers: call.headers.clone(),
            body: call.body.clone(),
            operation: call.operation.clone(),
        };
        match self.dispatch(request, token, gates).await {
            Err(message) => Outcome::Refused { message },
            // Done already, by an earlier send whose report never arrived: the
            // record's answer is the answer, and nothing went again.
            Ok((_, Dispatched::Replayed(response))) => Outcome::Answered {
                status: response.status,
                headers: response.headers,
                body: response.body,
                truncated: response.truncated,
            },
            Ok((_, Dispatched::Attempted(message))) => Outcome::NoAnswer { message },
            Ok((_, Dispatched::Sent(Ok(answer)))) => Outcome::Answered {
                status: answer.status,
                headers: answer.headers,
                body: answer.body,
                truncated: answer.truncated,
            },
            Ok((_, Dispatched::Sent(Err(why)))) if why.never_sent => Outcome::NeverSent {
                message: why.message,
            },
            Ok((_, Dispatched::Sent(Err(why)))) if why.did_not_complete => Outcome::NoAnswer {
                message: why.message,
            },
            Ok((_, Dispatched::Sent(Err(why)))) => Outcome::Refused {
                message: why.message,
            },
        }
    }

    /// Makes a request an agent asked for, if everything about it is allowed.
    ///
    /// Every refusal comes back as an error the model can read, because a tool
    /// that fails opaquely gets called again the same way. None of them tell
    /// the model anything it could use: that a host is not allowed is a fact
    /// about the workspace's settings, and that one resolves inside the cluster is
    /// a fact it already had to guess to ask.
    pub(super) async fn fetch(&mut self, request: HttpRequest) -> Result<HttpResponse, String> {
        Self::count(&mut self.fetches_used, MAX_FETCHES_PER_TURN, "requests")?;
        // The body as the guest gave it and as it is sent on, held for the
        // call. What comes back is not charged: nothing yet bounds it (see
        // docs/roadmap.md).
        let body = request.body.as_ref().map_or(0, |b| b.len() as u64);
        let _charge = self.charge(body.saturating_mul(2), "make a request")?;
        let token = self.current_token().await;
        let gates = self.gates.clone();
        let (sent, dispatched) = self.dispatch(request, token, gates).await?;
        let outcome = match dispatched {
            Dispatched::Replayed(response) => return Ok(response),
            Dispatched::Attempted(message) => return Err(message),
            Dispatched::Sent(outcome) => outcome,
        };

        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(refusal) => {
                // A refusal for want of an approval is the one the API has to
                // hear about, because it owns the queue and the job. Recognized
                // by asking the gates this turn carries rather than by reading
                // the gateway's wording: matching on prose would break the moment
                // somebody rephrased a message, and silently -- the failure would
                // be an approval nobody is ever asked for.
                //
                // Nothing here decides whether the request was allowed. The API
                // re-derives the gates and refuses a shape that matches none, so
                // this relaying a refusal it invented gets nowhere. Read from a
                // parse of the bytes it carries, which is never what is sent on.
                // Gates this build cannot read -- a field added by a newer API --
                // are relayed anyway: the API decides, and the alternative is an
                // approval nobody is ever asked for.
                if gates_cover(&self.gates, &sent)
                    && let Some(on_gated) = &self.on_gated
                {
                    // The whole request, as it was about to go, so that what a
                    // person approves is what is later sent -- see
                    // `outturn_core::egress::held`. The idempotency key the host
                    // added is part of it, so the send is the same write.
                    let pending = on_gated(GatedRequest {
                        method: sent.method.clone(),
                        host: sent.host.clone(),
                        path: sent.path.clone(),
                        url: sent.url.clone(),
                        headers: sent.headers.clone(),
                        body: sent.body.clone(),
                        operation: sent.operation.clone(),
                    })
                    .await;
                    if pending {
                        // The turn winds down at its next round boundary, on the
                        // same flag a stop uses -- which is the only place
                        // stopping is safe, because a tool that has started has
                        // either had its effect or not and nothing can say which
                        // afterwards. The guest finishes what it is holding, says
                        // what it has, and returns; the hold the API took is what
                        // keeps it stopped until somebody answers.
                        //
                        // Not parked here. This tier runs workspace code and owns
                        // no job: what it can do is stop asking for more, and the
                        // API decides what that means for the turn.
                        self.cancelled = true;
                        self.held = Some("waiting on an approval".to_string());
                        self.awaiting_approval = true;

                        // Said plainly, because this reaches the model and then
                        // the transcript. The guest may still do other work
                        // before it stops -- what it must not do is treat this as
                        // a transient failure and retry the same call.
                        // The sentence and its mark are shared with the API,
                        // which finds them again to put the answer in their
                        // place once somebody approves. Rewording it here alone
                        // would leave the refusal standing after the call went.
                        return Err(format!(
                            "{refusal}. Somebody has been asked to approve it, \
                             and this conversation will pause until they answer. \
                             {}",
                            outturn_core::egress::gate::gated_suffix()
                        ));
                    }
                }
                return Err(refusal.message);
            }
        };

        Ok(HttpResponse {
            status: outcome.status,
            headers: outcome.headers,
            body: outcome.body,
            truncated: outcome.truncated,
        })
    }
}

/// The key a declared write is made under: its operation and the values of
/// its identity, in declared order, from the path, the query or the JSON body.
///
/// The same booking derives the same key however many times, and whatever
/// else the call carried -- a note, an action label -- is not part of it. A
/// path parameter is read from the URL by the operation's own, decoded as a
/// gate decodes it (`egress::gate::path_values`), so `/invoices/{id}` keys on
/// the invoice; a path whose values cannot be read is refused, as the gateway
/// would refuse it under a gate.
fn idempotency_key(
    op: &outturn_core::protocol::work::SkillOperation,
    url: &reqwest::Url,
    body: Option<&str>,
) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let template = outturn_core::egress::gate::url_path(&op.url);
    let from_path = outturn_core::egress::gate::path_values(template, url.path())?;
    let body: serde_json::Value = body
        .and_then(|b| serde_json::from_str(b).ok())
        .unwrap_or_default();
    let values: Vec<serde_json::Value> = op
        .identity
        .iter()
        .map(
            |field| match (from_path.iter().find(|(n, _)| n == field), body.get(field)) {
                (Some((_, v)), _) => serde_json::json!(v),
                (None, Some(v)) => v.clone(),
                (None, None) => {
                    let mut found: Vec<String> = url
                        .query_pairs()
                        .filter(|(k, _)| k == field)
                        .map(|(_, v)| v.into_owned())
                        .collect();
                    match found.len() {
                        0 => serde_json::Value::Null,
                        1 => serde_json::json!(found.remove(0)),
                        _ => serde_json::json!(found),
                    }
                }
            },
        )
        .collect();
    let digest = Sha256::digest(serde_json::json!([op.name, values]).to_string());
    let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("outturn-{hex}"))
}

/// A body with the key set in its `field`, for a recipient that takes the key
/// there.
///
/// Only a JSON object has a field to put it in. Sending any other body
/// without the key would be an unkeyed write that looks keyed, and rewriting
/// it to fit would change what was asked, so the call is refused, in words the
/// model can act on. No body at all is an empty object.
fn keyed_body(
    body: Option<&str>,
    field: &str,
    operation: &str,
    key: &str,
) -> Result<String, String> {
    let mut parsed = match body.filter(|b| !b.trim().is_empty()) {
        None => serde_json::json!({}),
        Some(b) => serde_json::from_str::<serde_json::Value>(b).unwrap_or_default(),
    };
    match parsed.as_object_mut() {
        Some(object) => {
            object.insert(field.to_string(), serde_json::json!(key));
            Ok(parsed.to_string())
        }
        None => Err(format!(
            "{operation} carries its idempotency key in the `{field}` field of a JSON object \
             body, and this body is not one. Nothing was sent. Send the request body as a \
             JSON object."
        )),
    }
}

/// A recorded answer, given again, marked as such so the model is not told a
/// write happened twice.
fn replayed(response: &serde_json::Value, at: chrono::DateTime<chrono::Utc>) -> HttpResponse {
    let mut headers: Vec<(String, String)> = response["headers"]
        .as_array()
        .map(|h| {
            h.iter()
                .filter_map(|pair| {
                    Some((
                        pair.get(0)?.as_str()?.to_string(),
                        pair.get(1)?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    headers.push(("x-outturn-replayed".to_string(), at.to_rfc3339()));
    HttpResponse {
        status: response["status"].as_u64().unwrap_or(200) as u16,
        headers,
        body: response["body"].as_str().unwrap_or_default().to_string(),
        truncated: response["truncated"].as_bool().unwrap_or(false),
    }
}

/// Whether a refusal of this request may be for want of an approval: some gate
/// the turn carries covers it, or the gates cannot be read here to say none
/// does.
fn gates_cover(gates: &outturn_core::egress::gate::RawGates, sent: &Sent) -> bool {
    match gates.parse() {
        Ok(gates) => gates
            .covering(&sent.host, &sent.method.to_ascii_uppercase(), &sent.path)
            .is_some(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                host = %sent.host,
                "this turn's gates could not be read, so a refusal is relayed for the API to judge"
            );
            true
        }
    }
}

#[cfg(test)]
mod idempotency_key_tests {
    use super::*;

    fn void() -> outturn_core::protocol::work::SkillOperation {
        outturn_core::protocol::work::SkillOperation {
            name: "books_void_invoice".into(),
            method: "POST".into(),
            url: "https://books.example.com/invoices/{id}/void".into(),
            body: vec!["reason".into()],
            identity: vec!["id".into()],
            ..Default::default()
        }
    }

    fn key(url: &str, body: &str) -> Result<String, String> {
        idempotency_key(&void(), &reqwest::Url::parse(url).unwrap(), Some(body))
    }

    /// One invoice voided twice is one write, whatever reason was given; a
    /// second invoice is another.
    #[test]
    fn an_identity_can_name_a_path_parameter() {
        let one = key(
            "https://books.example.com/invoices/inv_1/void",
            r#"{"reason":"a"}"#,
        )
        .unwrap();
        assert_eq!(
            one,
            key(
                "https://books.example.com/invoices/inv%5F1/void",
                r#"{"reason":"b"}"#
            )
            .unwrap()
        );
        assert_ne!(
            one,
            key("https://books.example.com/invoices/inv_2/void", "{}").unwrap()
        );
        // The body cannot stand in for the path.
        assert_eq!(
            one,
            key(
                "https://books.example.com/invoices/inv_1/void",
                r#"{"id":"inv_2"}"#
            )
            .unwrap()
        );
    }

    #[test]
    fn a_path_value_that_decodes_to_a_slash_is_refused() {
        assert!(key("https://books.example.com/invoices/a%2Fb/void", "{}").is_err());
    }
}

#[cfg(test)]
mod gates_cover_tests {
    use super::*;
    use outturn_core::egress::gate::RawGates;

    fn sent(path: &str) -> Sent {
        Sent {
            method: "post".into(),
            url: format!("https://api.example.com{path}"),
            host: "api.example.com".into(),
            path: path.into(),
            headers: Vec::new(),
            body: None,
            operation: None,
        }
    }

    #[test]
    fn gates_this_build_cannot_read_still_relay_the_refusal() {
        let unreadable: RawGates = serde_json::from_str(r#"{"not": "gates"}"#).unwrap();
        assert!(unreadable.parse().is_err());
        assert!(gates_cover(&unreadable, &sent("/charges")));
    }

    #[test]
    fn gates_that_cover_nothing_relay_nothing() {
        let none = RawGates::from(outturn_core::egress::gate::Gates::none());
        assert!(!gates_cover(&none, &sent("/charges")));
    }
}

#[cfg(test)]
mod keyed_body_tests {
    use super::keyed_body;

    #[test]
    fn the_key_goes_in_an_object_and_nowhere_else() {
        let keyed = |body| keyed_body(body, "request_id", "book", "outturn-1");
        assert_eq!(
            keyed(Some(r#"{"a":1}"#)).unwrap(),
            r#"{"a":1,"request_id":"outturn-1"}"#
        );
        assert_eq!(keyed(None).unwrap(), r#"{"request_id":"outturn-1"}"#);
        for body in ["[1,2]", r#""text""#, "7", "null", "not json"] {
            let refused = keyed(Some(body)).expect_err(body);
            assert!(
                refused.contains("request_id") && refused.contains("Nothing was sent"),
                "{refused}"
            );
        }
    }
}
