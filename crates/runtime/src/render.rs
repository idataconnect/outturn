//! Asks the PDF renderer for a document.
//!
//! The renderer is a service of its own (`renderers/pdf`), deployed only where
//! it is wanted, as Tika is: an operator who never needs a PDF ships none of
//! its fonts, layout or memory in the runtime. It holds nothing and reaches
//! nothing -- markdown in, PDF out -- and the runtime stores what comes back.
//! See docs/pdf-rendering.md.

use std::time::Duration;

/// Where the renderer listens, when one is deployed.
pub const URL_ENV: &str = "OUTTURN_PDF_RENDERER_URL";

/// The most markdown sent to be rendered. The renderer enforces the same
/// figure; checked here too so an oversized document is refused with a
/// sentence before it is sent anywhere.
pub const MAX_MARKDOWN_BYTES: usize = 2 * 1024 * 1024;

/// The most of the renderer's answer that is read. A PDF of a two-megabyte
/// document is far smaller; past this the renderer is misbehaving, and the
/// runtime does not buffer what it sends.
pub const MAX_PDF_BYTES: usize = 32 * 1024 * 1024;

/// How long a render may take, end to end. The renderer gives up at thirty
/// seconds; a little longer here so its own answer is the one heard.
const TIMEOUT: Duration = Duration::from_secs(40);

/// The renderer's address from the environment, if one is configured.
pub fn url_from_env() -> Option<String> {
    std::env::var(URL_ENV)
        .ok()
        .map(|u| u.trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
}

/// Renders markdown as a PDF. Errors are said for the guest that asked.
pub async fn render(
    http: &reqwest::Client,
    url: Option<&str>,
    markdown: String,
) -> Result<Vec<u8>, String> {
    let Some(url) = url else {
        return Err("PDF rendering is not available here".to_string());
    };
    if markdown.len() > MAX_MARKDOWN_BYTES {
        return Err(format!(
            "the markdown is {} bytes; a PDF is rendered from at most {MAX_MARKDOWN_BYTES}",
            markdown.len()
        ));
    }
    let response = http
        .post(format!("{url}/v1/render/pdf"))
        .header(
            reqwest::header::CONTENT_TYPE,
            "text/markdown; charset=utf-8",
        )
        .timeout(TIMEOUT)
        .body(markdown)
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, "could not reach the PDF renderer");
            "the PDF renderer could not be reached; try again shortly".to_string()
        })?;
    let status = response.status();
    let body = capped(response, MAX_PDF_BYTES).await?;
    if status.is_success() {
        return Ok(body);
    }
    // The renderer's refusals are written to be read -- too large, busy, too
    // slow, could not be laid out -- so they are passed on as they are.
    let said = String::from_utf8_lossy(&body);
    Err(said.chars().take(300).collect())
}

/// The body, abandoned the moment it passes `max`.
async fn capped(response: reqwest::Response, max: usize) -> Result<Vec<u8>, String> {
    use outturn_core::http_client::{Capped, read_capped};
    read_capped(response, max).await.map_err(|e| match e {
        Capped::TooLarge => {
            format!("the PDF renderer sent more than {max} bytes; a PDF may be at most that")
        }
        Capped::Stopped(e) => format!("the PDF renderer stopped partway: {e}"),
    })
}
