//! The HTTP face of the converter.
//!
//! One route: HEIC in, JPEG out. It holds nothing -- no credentials, no
//! storage, no database -- and calls nothing; the API stores what comes back.
//! So the most a request can do here is spend this pod's time and memory, and
//! everything below bounds that: how much may be sent, how large a photo may
//! decode to, how many run at once, and how long one may take.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tokio::sync::Semaphore;

/// The largest file accepted: the most the API stores from one upload.
pub const MAX_HEIC_BYTES: usize = 25 * 1024 * 1024;

/// Conversions one pod runs at once. Each peaks near 350MB at
/// [`crate::MAX_PIXELS`] -- the decoded frame and libheif's own planes beside
/// it -- so this and the pod's 1Gi limit are one decision. A request beyond it
/// is told the converter is busy at once, not queued.
pub const CONCURRENT: usize = 2;

/// How long a caller is kept waiting. A phone photo converts in about a
/// second; this is far past that.
///
/// It bounds the wait, not the work: a decode already running cannot be
/// stopped from outside its thread, so its slot stays taken until it ends.
pub const TIME_LIMIT: Duration = Duration::from_secs(30);

pub fn app() -> Router {
    let slots = Arc::new(Semaphore::new(CONCURRENT));
    Router::new()
        .route("/v1/convert/jpeg", post(convert))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(slots)
        .layer(DefaultBodyLimit::max(MAX_HEIC_BYTES + 1024))
}

async fn convert(State(slots): State<Arc<Semaphore>>, body: Bytes) -> Response {
    if body.len() > MAX_HEIC_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "the photo is {} bytes; at most {MAX_HEIC_BYTES} are converted",
                body.len()
            ),
        )
            .into_response();
    }
    if !crate::is_heif(&body) {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "this is not a HEIC photo",
        )
            .into_response();
    }
    let Ok(slot) = Arc::clone(&slots).try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the converter is busy; try again shortly",
        )
            .into_response();
    };

    let started = Instant::now();
    let size = body.len();
    // The slot travels with the work rather than the wait, so a decode that
    // outlives its caller's patience still counts against the pod.
    let work = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        crate::to_jpeg(&body)
    });
    let converted = match tokio::time::timeout(TIME_LIMIT, work).await {
        Ok(Ok(converted)) => converted,
        Ok(Err(e)) => {
            tracing::error!(error = %e, heic_bytes = size, "a conversion panicked");
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                "this photo could not be converted",
            )
                .into_response();
        }
        Err(_) => {
            tracing::warn!(heic_bytes = size, "a conversion ran past its time limit");
            return (
                StatusCode::GATEWAY_TIMEOUT,
                "the photo took too long to convert",
            )
                .into_response();
        }
    };

    match converted {
        Ok(jpeg) => {
            tracing::info!(
                heic_bytes = size,
                jpeg_bytes = jpeg.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "converted a photo"
            );
            ([(header::CONTENT_TYPE, "image/jpeg")], jpeg).into_response()
        }
        Err(e) => (StatusCode::UNPROCESSABLE_ENTITY, e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn post(body: Vec<u8>) -> (StatusCode, Vec<u8>) {
        let response = app()
            .oneshot(
                Request::post("/v1/convert/jpeg")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, bytes.to_vec())
    }

    #[tokio::test]
    async fn heic_in_jpeg_out() {
        let (status, body) = post(include_bytes!("../tests/gradient.heic").to_vec()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.starts_with(b"\xff\xd8\xff"));
    }

    #[tokio::test]
    async fn something_else_is_refused_as_the_wrong_type() {
        let (status, _) = post(b"\x89PNG\r\n\x1a\n".to_vec()).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn a_heic_that_does_not_decode_is_unprocessable() {
        let (status, _) = post(b"\0\0\0\x18ftypheic\0\0\0\0".to_vec()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    }
}
