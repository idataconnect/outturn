//! Text arriving in pieces.
//!
//! A stream of bytes is cut wherever the network cut it, and that is not where
//! characters end: an emoji is four bytes and a curly quote three, and a chunk
//! boundary can fall inside either. Decoding each chunk on its own then fails
//! on a reply that is perfectly valid -- "incomplete utf-8 byte sequence" --
//! and the turn with it, more often the more a model likes emoji.

/// Decodes a byte stream chunk by chunk, holding back a character split across
/// two chunks until the rest of it arrives.
#[derive(Debug, Default)]
pub struct Decoder {
    /// The start of a character whose remaining bytes are still to come. Never
    /// more than three bytes.
    pending: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends what this chunk completes to `out`.
    ///
    /// An error only for bytes that are invalid, not merely unfinished: a
    /// sequence that cannot be the start of any character is wrong however
    /// much more arrives.
    pub fn push(&mut self, chunk: &[u8], out: &mut String) -> Result<(), std::str::Utf8Error> {
        let joined;
        let bytes = if self.pending.is_empty() {
            chunk
        } else {
            self.pending.extend_from_slice(chunk);
            joined = std::mem::take(&mut self.pending);
            &joined[..]
        };
        match std::str::from_utf8(bytes) {
            Ok(text) => out.push_str(text),
            Err(e) if e.error_len().is_none() => {
                let whole = e.valid_up_to();
                // Valid up to `whole` by the error's own account.
                out.push_str(std::str::from_utf8(&bytes[..whole]).expect("valid prefix"));
                self.pending = bytes[whole..].to_vec();
            }
            Err(e) => return Err(e),
        }
        Ok(())
    }

    /// Whether a character was left unfinished: at the end of the stream
    /// that is truncation, and the caller says so rather than dropping it.
    pub fn is_partial(&self) -> bool {
        !self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(chunks: &[&[u8]]) -> Result<String, std::str::Utf8Error> {
        let mut decoder = Decoder::new();
        let mut out = String::new();
        for chunk in chunks {
            decoder.push(chunk, &mut out)?;
        }
        assert!(!decoder.is_partial());
        Ok(out)
    }

    #[test]
    fn a_character_cut_anywhere_is_reassembled() {
        let text = "bleargh 📚 “quoted” → done";
        let bytes = text.as_bytes();
        for cut in 0..=bytes.len() {
            assert_eq!(
                decode(&[&bytes[..cut], &bytes[cut..]]).unwrap(),
                text,
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn a_character_cut_into_one_byte_chunks_is_reassembled() {
        let text = "📚💸🤔";
        let chunks: Vec<&[u8]> = text.as_bytes().chunks(1).collect();
        assert_eq!(decode(&chunks).unwrap(), text);
    }

    #[test]
    fn invalid_bytes_are_still_refused() {
        assert!(decode(&[b"ok \xff then"]).is_err());
    }

    #[test]
    fn a_stream_that_ends_mid_character_says_so() {
        let mut decoder = Decoder::new();
        let mut out = String::new();
        decoder.push(&"📚".as_bytes()[..2], &mut out).unwrap();
        assert!(decoder.is_partial());
        assert_eq!(out, "");
    }
}

/// What a byte string is, when it is plainly not text -- or None when it may be.
///
/// Magic numbers first, because a name helps: told it holds a zip, a model
/// reaches for `expand_archive`; told "binary", it can only apologize. Then a
/// coarse test for the rest: a NUL, or a window that is mostly control
/// characters, is not something anyone wanted to read.
pub fn describe_binary(bytes: &[u8]) -> Option<&'static str> {
    if bytes.is_empty() {
        return None;
    }
    if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
        return Some("a zip archive; use expand_archive to unpack it");
    }
    if bytes.starts_with(b"%PDF") {
        return Some(
            "a PDF, and document extraction is not configured here, so its text cannot be read",
        );
    }
    if bytes.starts_with(b"\x89PNG") {
        return Some("a PNG image");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some("a JPEG image");
    }
    if bytes.starts_with(b"GIF8") {
        return Some("a GIF image");
    }
    if bytes.starts_with(b"\x1f\x8b") {
        return Some("gzip-compressed data");
    }
    let window = &bytes[..bytes.len().min(8192)];
    if window.contains(&0) {
        return Some("binary data");
    }
    let control = window
        .iter()
        .filter(|b| **b < 0x20 && !matches!(**b, b'\t' | b'\n' | b'\r'))
        .count();
    if control * 10 > window.len() {
        return Some("binary data");
    }
    None
}

#[cfg(test)]
mod binary_guard {
    use super::describe_binary;

    #[test]
    fn text_is_not_described() {
        assert_eq!(describe_binary(b"invoice 7 paid in full"), None);
        assert_eq!(
            describe_binary("caf\u{e9} \u{1f600}\n".as_bytes()),
            None,
            "utf-8 is text"
        );
        assert_eq!(describe_binary(b"a,b\r\n1,2\r\n"), None);
        assert_eq!(describe_binary(b""), None);
    }

    #[test]
    fn known_formats_are_named() {
        assert!(
            describe_binary(b"PK\x03\x04rest")
                .unwrap()
                .contains("expand_archive")
        );
        assert!(
            describe_binary(b"%PDF-1.4")
                .unwrap()
                .contains("not configured")
        );
        assert_eq!(describe_binary(b"\x89PNG\r\n"), Some("a PNG image"));
        assert_eq!(describe_binary(b"\xff\xd8\xff\xe0"), Some("a JPEG image"));
    }

    #[test]
    fn a_nul_or_mostly_control_bytes_is_binary() {
        assert_eq!(describe_binary(b"abc\x00def"), Some("binary data"));
        let noisy: Vec<u8> = (0..100u8)
            .map(|i| if i % 2 == 0 { 0x01 } else { b'a' })
            .collect();
        assert_eq!(describe_binary(&noisy), Some("binary data"));
    }
}
