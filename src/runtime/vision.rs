//! Asking a model what is in an image.
//!
//! The guest names a stored object and says what it wants to know; the host
//! reads the bytes and asks a model that can see. What comes back is prose.
//! The bytes stay on this side of the sandbox, which is the same trade
//! `read-object` makes for a document, and for the same reason: an image a
//! guest can hold is an image a compromised component can carry out.
//!
//! Why a question rather than a description: a pass over an image is directed.
//! "What is in this screenshot" and "what is the phone number in the corner"
//! attend differently and answer differently, so one description written when
//! the image arrived is an answer to a question nobody had asked yet. Asking
//! twice with two questions is the intended use. See docs/vision.md.

/// What a model is told an image is, from its first bytes.
///
/// Sniffed rather than taken from the path: an extension is a claim by whoever
/// uploaded the file, and a provider handed a mismatched media type rejects
/// the request in a way that reads like our bug. The magic numbers here are
/// the ones every vision model accepts.
pub fn media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG") {
        return Some("image/png");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF8") {
        return Some("image/gif");
    }
    // RIFF....WEBP: the size sits between the two, so the tail is checked
    // rather than the whole prefix.
    if bytes.starts_with(b"RIFF") && bytes.len() > 12 && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// The traffic type a look at an image travels under.
///
/// Its own class, so which model sees is a row in `traffic_routes` rather than
/// anything in code: the model holding a conversation and the model that can
/// look at a picture need not be the same one, and on most deployments they
/// are not.
pub const TRAFFIC_TYPE: &str = "vision";

/// How much of an image is read before giving up on it.
///
/// Generous next to what a vision model will accept and mean next to what an
/// upload may be: 25MB is allowed into storage, and a model asked to read that
/// as base64 would be sent a third again as much. What this bounds is a
/// sandbox host holding an object in memory while it encodes it.
pub const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// The message sent to a model that can see.
///
/// Built here rather than in the caller so the shape stays in one place: the
/// question leads, because a model reading the image first and the question
/// second is a model that has already decided what to notice.
pub fn request(model: &str, media_type: &str, encoded: &str, question: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "stream": false,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": question},
                {
                    "type": "image_url",
                    "image_url": {"url": format!("data:{media_type};base64,{encoded}")},
                },
            ],
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_is_known_by_its_bytes_rather_than_its_name() {
        // Whoever uploaded `diagram.png` may have uploaded a JPEG, and a
        // provider told the wrong type refuses in a way that reads like ours.
        assert_eq!(media_type(b"\x89PNG\r\n\x1a\n rest"), Some("image/png"));
        assert_eq!(media_type(b"\xff\xd8\xff\xe0 rest"), Some("image/jpeg"));
        assert_eq!(media_type(b"GIF89a rest"), Some("image/gif"));

        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBPVP8 ");
        assert_eq!(media_type(&webp), Some("image/webp"));
    }

    #[test]
    fn something_that_is_not_an_image_is_not_guessed_at() {
        assert_eq!(media_type(b"%PDF-1.7"), None);
        assert_eq!(media_type(b"PK\x03\x04"), None);
        assert_eq!(media_type(b"just some words"), None);
        assert_eq!(media_type(b""), None);
        // Truncated RIFF: a header that has not said WEBP yet is not one.
        assert_eq!(media_type(b"RIFF\0\0\0\0WEB"), None);
    }

    #[test]
    fn the_question_is_asked_before_the_image_is_shown() {
        // A model that reads the image first has already decided what to
        // notice by the time it reads the question.
        let body = request("qwen3.8:27b-mlx", "image/png", "AAAA", "what is the total?");
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["text"], "what is the total?");
        assert_eq!(content[1]["type"], "image_url");
    }

    #[test]
    fn the_image_travels_as_a_data_url_of_its_own_type() {
        let body = request("m", "image/jpeg", "QUJD", "what is this?");
        assert_eq!(
            body["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/jpeg;base64,QUJD"
        );
    }

    #[test]
    fn a_look_does_not_travel_as_a_conversation() {
        // Its own traffic type, so a deployment can route a picture somewhere
        // other than wherever the conversation goes.
        assert_eq!(TRAFFIC_TYPE, "vision");
        assert_ne!(TRAFFIC_TYPE, "assistant");
    }
}
