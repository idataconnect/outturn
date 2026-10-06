//! The HTTP face of the renderer.
//!
//! One route: markdown in, PDF out. It holds nothing -- no credentials, no
//! storage, no database -- and calls nothing; whoever asked stores the
//! result. So the most a request can do here is spend this pod's time, and
//! everything below is about bounding that: how much may be sent, how many
//! renders run at once, and how long one may take.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use tokio::sync::Semaphore;

/// The most markdown a render accepts. Several hundred pages already; past it
/// the question is whether one document is what is wanted, and a request that
/// big is refused before it is read rather than after.
pub const MAX_MARKDOWN_BYTES: usize = 2 * 1024 * 1024;

/// Renders one pod runs at once. Rendering is all CPU, and a request beyond
/// this is told the renderer is busy rather than queued behind work it cannot
/// see the end of.
pub const CONCURRENT: usize = 2;

/// How long a caller is kept waiting for one render. Markdown at the size cap
/// renders in a few seconds; this is far past that.
///
/// It bounds the wait, not the work: a render already running cannot be
/// stopped from outside its thread, so a pathological one finishes on its own
/// time while its slot stays taken. The size cap is what actually bounds how
/// long that can be.
pub const TIME_LIMIT: Duration = Duration::from_secs(30);

pub fn app() -> Router {
    let renders = Arc::new(Semaphore::new(CONCURRENT));
    Router::new()
        .route("/v1/render/pdf", post(render_pdf))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(renders)
        // Plus a little for the request around it, so the cap is on the
        // markdown and not on whatever else rides along.
        .layer(DefaultBodyLimit::max(MAX_MARKDOWN_BYTES + 1024))
}

async fn render_pdf(State(renders): State<Arc<Semaphore>>, body: Bytes) -> Response {
    let Ok(markdown) = String::from_utf8(body.to_vec()) else {
        return (StatusCode::BAD_REQUEST, "the markdown is not UTF-8").into_response();
    };
    if markdown.len() > MAX_MARKDOWN_BYTES {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "the markdown is {} bytes; a PDF is rendered from at most {MAX_MARKDOWN_BYTES}",
                markdown.len()
            ),
        )
            .into_response();
    }
    let Ok(slot) = Arc::clone(&renders).try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "the renderer is busy; try again shortly",
        )
            .into_response();
    };

    let started = Instant::now();
    let size = markdown.len();
    // The slot travels with the work rather than the wait, so a render that
    // outlives its caller's patience still counts against the pod until it
    // is actually done.
    let work = tokio::task::spawn_blocking(move || {
        let _slot = slot;
        crate::render(&markdown)
    });
    let rendered = match tokio::time::timeout(TIME_LIMIT, work).await {
        Ok(Ok(rendered)) => rendered,
        // A panic in layout is a bug, but it is this document's bug: said as
        // a failure to render rather than as the service falling over.
        Ok(Err(e)) => {
            tracing::error!(error = %e, markdown_bytes = size, "a render panicked");
            return (
                StatusCode::UNPROCESSABLE_ENTITY,
                "this document could not be rendered",
            )
                .into_response();
        }
        Err(_) => {
            tracing::warn!(markdown_bytes = size, "a render ran past its time limit");
            return (
                StatusCode::GATEWAY_TIMEOUT,
                "the document took too long to render",
            )
                .into_response();
        }
    };

    match rendered {
        Ok(pdf) => {
            tracing::info!(
                markdown_bytes = size,
                pdf_bytes = pdf.len(),
                elapsed_ms = started.elapsed().as_millis() as u64,
                "rendered a PDF"
            );
            ([(header::CONTENT_TYPE, "application/pdf")], pdf).into_response()
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
                Request::post("/v1/render/pdf")
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
    async fn markdown_in_pdf_out() {
        let (status, body) = post(b"# Karl\n\nHe is here.\n".to_vec()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.starts_with(b"%PDF"));
    }

    #[tokio::test]
    async fn markdown_past_the_cap_is_refused() {
        let (status, _) = post(vec![b'a'; MAX_MARKDOWN_BYTES + 1]).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn bytes_that_are_not_text_are_refused() {
        let (status, _) = post(vec![0xff, 0xfe, 0x00]).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
}
