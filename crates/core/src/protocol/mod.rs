//! How the tiers address one another: the headers, names and failure shapes
//! each side of a call must agree on. Here rather than in the tier that
//! answers, so a caller depends on the agreement and not on the code behind it.

pub mod work;

use axum::http::StatusCode;

/// The class of traffic this request belongs to.
///
/// A header rather than a body field: the request body is serialized straight
/// through to the provider, so anything added there leaks upstream. Routing is
/// also not something a model should be told about.
pub const TRAFFIC_HEADER: &str = "x-outturn-traffic";

/// The class of work a request belongs to, when the caller does not say.
pub const DEFAULT_TRAFFIC_TYPE: &str = "assistant";

/// A turn nobody is waiting on -- a schedule's, a webhook's -- so an operator
/// can send it somewhere cheaper or slower than a person's, and the ledger can
/// tell the two apart.
pub const BACKGROUND_TRAFFIC_TYPE: &str = "background";

/// Names what kind of failure a 502 from the streaming route is, where it is
/// one the caller should treat differently from the provider being down.
pub const FAILURE_HEADER: &str = "x-outturn-failure";
/// The model answered, and its answer was unusable. See `is_malformed_reply`.
pub const MALFORMED_REPLY: &str = "malformed-reply";
/// An agent's outbound request was sent, or may have been, and no answer came
/// back -- so a write may have landed. Said on the egress route's 502, for the
/// runtime to leave a claimed write `attempted` rather than release it; see
/// docs/idempotency.md. A header rather than the sentence beside it, which is
/// written for a model and may be reworded.
pub const DID_NOT_COMPLETE: &str = "did-not-complete";

/// What a caller makes of a failed response from the gateway: its body, which
/// is already the sentence `failure_message` wrote, passed on as it is -- a
/// status in front of it adds nothing a person can use. An empty body says
/// only the status. One rule for every caller, so a turn and a session's title
/// read the same failure the same way.
pub fn gateway_failure(status: StatusCode, body: &str) -> anyhow::Error {
    match body.trim() {
        "" => anyhow::anyhow!("the gateway answered {status}"),
        said => anyhow::anyhow!("{said}"),
    }
}

pub const MAX_SPEC_BYTES: usize = 16 * 1024 * 1024;
