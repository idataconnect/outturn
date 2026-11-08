//! Asks the PDF renderer for a document.
//!
//! The renderer is a service of its own (`renderers/pdf`), deployed only where
//! it is wanted, as Tika is: an operator who never needs a PDF ships none of
//! its fonts, layout or memory in the runtime. It holds nothing and reaches
//! nothing -- markdown in, PDF out -- and the runtime stores what comes back.
//! So the images a document draws are read here, where the agent's scopes are
//! enforced, and sent with it. See docs/pdf-rendering.md.

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

/// The most images one document may send, and the most bytes they may come
/// to together. The renderer holds them all at once and enforces the same
/// figures; checked here first so that nothing is read past them.
pub const MAX_IMAGES: usize = 32;
pub const MAX_IMAGE_BYTES: u64 = 24 * 1024 * 1024;

/// The content type of markdown sent with its images: each part a big-endian
/// u16 length and its name, then a big-endian u32 length and its bytes, the
/// markdown first under the name "". The renderer's `unbundle` reads it.
const BUNDLE: &str = "application/x-outturn-bundle";

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

/// The images the markdown draws, each once, in the order it names them.
///
/// Only where the renderer draws one: an image in a table cell or a heading
/// is shown as its alt text, so reading it would fetch bytes nobody uses.
pub fn image_paths(markdown: &str) -> Vec<String> {
    use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut paths: Vec<String> = Vec::new();
    let mut in_table = false;
    let mut in_heading = false;
    for event in Parser::new_ext(markdown, options) {
        match event {
            Event::Start(Tag::Table(_)) => in_table = true,
            Event::End(TagEnd::Table) => in_table = false,
            Event::Start(Tag::Heading { .. }) => in_heading = true,
            Event::End(TagEnd::Heading(_)) => in_heading = false,
            Event::Start(Tag::Image { dest_url, .. })
                if !in_table && !in_heading && !paths.iter().any(|p| **p == *dest_url) =>
            {
                paths.push(dest_url.to_string());
            }
            _ => {}
        }
    }
    paths
}

/// Renders markdown as a PDF, with the images it draws. Errors are said for
/// the guest that asked.
pub async fn render(
    http: &reqwest::Client,
    url: Option<&str>,
    markdown: String,
    images: Vec<(String, Vec<u8>)>,
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
    let (content_type, body) = if images.is_empty() {
        ("text/markdown; charset=utf-8", markdown.into_bytes())
    } else {
        (BUNDLE, bundle(markdown, images))
    };
    let response = http
        .post(format!("{url}/v1/render/pdf"))
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .timeout(TIMEOUT)
        .body(body)
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

/// Markdown and its images as one body. Each image is moved into it and
/// dropped, so the request is held once rather than beside its parts.
fn bundle(markdown: String, images: Vec<(String, Vec<u8>)>) -> Vec<u8> {
    let size = markdown.len()
        + images
            .iter()
            .map(|(n, b)| n.len() + b.len() + 6)
            .sum::<usize>();
    let mut out = Vec::with_capacity(size + 6);
    let mut part = |name: &str, bytes: &[u8]| {
        // Names are paths the storage scopes accepted, far short of a u16,
        // and every image is far short of a u32.
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(bytes);
    };
    part("", markdown.as_bytes());
    drop(markdown);
    for (name, bytes) in images {
        part(&name, &bytes);
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_drawn_image_is_named_once() {
        let md = "![a](session/a.png)\n\n![again](session/a.png) ![b](agent/b.jpg \"B\")\n\n\
                  # ![in a heading](session/h.png)\n\n\
                  | x |\n|---|\n| ![in a cell](session/c.png) |\n\n\
                  ```\n![in code](session/code.png)\n```\n";
        assert_eq!(image_paths(md), ["session/a.png", "agent/b.jpg"]);
    }

    #[test]
    fn a_bundle_is_its_parts_in_order() {
        let body = bundle("# K".into(), vec![("a.png".into(), vec![1, 2])]);
        let expected: Vec<u8> = [
            &[0, 0][..],
            &[0, 0, 0, 3],
            b"# K",
            &[0, 5],
            b"a.png",
            &[0, 0, 0, 2],
            &[1, 2],
        ]
        .concat();
        assert_eq!(body, expected);
    }
}
