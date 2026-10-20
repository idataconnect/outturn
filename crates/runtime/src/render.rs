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

/// The body, read chunk by chunk and abandoned the moment it passes `max`.
async fn capped(mut response: reqwest::Response, max: usize) -> Result<Vec<u8>, String> {
    let too_large =
        || format!("the PDF renderer sent more than {max} bytes; a PDF may be at most that");
    if response.content_length().is_some_and(|n| n > max as u64) {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("the PDF renderer stopped partway: {e}"))?
    {
        if body.len() + chunk.len() > max {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Serves one response with `body` and returns where to reach it.
    async fn serve(body: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(&body).await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_body_past_the_cap_is_refused() {
        let url = serve(vec![0; 64]).await;
        let response = reqwest::get(url).await.unwrap();
        assert!(capped(response, 32).await.is_err());
    }

    #[tokio::test]
    async fn a_body_within_the_cap_is_returned() {
        let url = serve(vec![7; 32]).await;
        let response = reqwest::get(url).await.unwrap();
        assert_eq!(capped(response, 32).await.unwrap(), vec![7; 32]);
    }
}
