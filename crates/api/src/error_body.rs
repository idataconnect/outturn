//! Every error the API answers with is `{"code": ..., "message": ...}`.
//!
//! Handlers return `(StatusCode, String)`, which axum sends as plain text.
//! Rather than change four hundred of those, this rewrites the response on its
//! way out: a client gets a field it can match on without reading prose, and
//! the message stays what a person is shown. A handler that has already
//! answered in JSON -- with a code of its own, where one status means two
//! things to a client -- is left alone.
//!
//! The code is one per status until a client needs to tell two cases apart.

use axum::body::{Body, to_bytes};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

/// More than any message a handler writes; a body larger than this was not one.
const MAX_MESSAGE: usize = 64 * 1024;

pub async fn as_json(response: Response) -> Response {
    let status = response.status();
    if !(status.is_client_error() || status.is_server_error()) {
        return response;
    }
    let plain = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|t| t.as_bytes().starts_with(b"text/plain"))
        // An empty error, such as a bare status, has no type at all.
        .unwrap_or(true);
    if !plain {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = to_bytes(body, MAX_MESSAGE).await else {
        return (status, Body::empty()).into_response();
    };
    let message = String::from_utf8_lossy(&bytes);
    let message = if message.is_empty() {
        status.canonical_reason().unwrap_or_default()
    } else {
        &message
    };
    let body = serde_json::json!({ "code": code(status), "message": message }).to_string();
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    parts.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(body))
}

fn code(status: StatusCode) -> &'static str {
    match status {
        StatusCode::BAD_REQUEST => "invalid",
        StatusCode::UNAUTHORIZED => "unauthenticated",
        StatusCode::FORBIDDEN => "forbidden",
        StatusCode::NOT_FOUND => "not_found",
        StatusCode::CONFLICT => "conflict",
        StatusCode::PAYLOAD_TOO_LARGE => "too_large",
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_type",
        StatusCode::UNPROCESSABLE_ENTITY => "unprocessable",
        StatusCode::TOO_MANY_REQUESTS => "too_many_requests",
        StatusCode::BAD_GATEWAY => "upstream_failed",
        StatusCode::SERVICE_UNAVAILABLE => "unavailable",
        StatusCode::GATEWAY_TIMEOUT => "upstream_timeout",
        s if s.is_client_error() => "client_error",
        _ => "internal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn body_of(response: Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), MAX_MESSAGE).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn a_plain_error_becomes_a_code_and_its_message() {
        let r =
            as_json((StatusCode::CONFLICT, "that name is taken".to_string()).into_response()).await;
        assert_eq!(r.status(), StatusCode::CONFLICT);
        assert_eq!(r.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(
            body_of(r).await,
            serde_json::json!({"code": "conflict", "message": "that name is taken"})
        );
    }

    #[tokio::test]
    async fn a_bare_status_is_given_its_reason() {
        let r = as_json(StatusCode::NOT_FOUND.into_response()).await;
        assert_eq!(
            body_of(r).await,
            serde_json::json!({"code": "not_found", "message": "Not Found"})
        );
    }

    #[tokio::test]
    async fn success_and_json_errors_pass_through() {
        let ok = as_json((StatusCode::OK, "fine".to_string()).into_response()).await;
        assert_eq!(to_bytes(ok.into_body(), MAX_MESSAGE).await.unwrap(), "fine");

        let own = serde_json::json!({"code": "wrong_password", "message": "no"});
        let r = as_json((StatusCode::BAD_REQUEST, axum::Json(own.clone())).into_response()).await;
        assert_eq!(body_of(r).await, own);
    }
}
