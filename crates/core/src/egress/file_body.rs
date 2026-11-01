//! A request whose body is a stored file, streamed from the runtime through
//! the gateway without either tier holding it.
//!
//! See `docs/file-bodies.md`. The runtime sends the egress request as a
//! stream: a four-byte big-endian length, that many bytes of the request as
//! JSON with `body_size` in place of `body`, then the file. The gateway reads
//! the head, vets it as it vets any request, and only then reads the file,
//! into the request it sends.

use std::time::Duration;

/// The largest file sent as a body. Bounded by the time a request is given
/// rather than by memory, since neither tier holds the file.
pub const LIMIT: u64 = 32 * 1024 * 1024;

/// The largest head: the request as JSON, gates and proof included. The same
/// bound the JSON form has always had.
pub const HEAD_LIMIT: u32 = 2 * 1024 * 1024;

/// What the streamed form is sent as, so the gateway knows which it is
/// reading before it reads any of it.
pub const CONTENT_TYPE: &str = "application/vnd.outturn.egress-stream";

/// How long the gateway waits for each piece of the stream, forwarding it or
/// draining it. A runtime that stalls longer is given up on.
pub const IDLE: Duration = Duration::from_secs(30);

/// How long a request with a file body is given, in place of the ordinary
/// thirty seconds: at the limit, thirty seconds is a sustained 9 Mbit/s.
pub const TIMEOUT: Duration = Duration::from_secs(600);

/// How much of a file is read from storage at a time.
pub const CHUNK: usize = 64 * 1024;

/// The head that goes before the file: its length, then the request.
pub fn head(request: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + request.len());
    out.extend_from_slice(&(request.len() as u32).to_be_bytes());
    out.extend_from_slice(request);
    out
}

/// The length a head declares, if it is one that may be read.
pub fn head_len(prefix: [u8; 4]) -> Result<usize, String> {
    let n = u32::from_be_bytes(prefix);
    if n == 0 || n > HEAD_LIMIT {
        return Err("that request is not one this can read".to_string());
    }
    Ok(n as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_head_is_its_length_then_the_request() {
        let framed = head(b"{}");
        assert_eq!(framed, [0, 0, 0, 2, b'{', b'}']);
        assert_eq!(head_len(framed[..4].try_into().unwrap()), Ok(2));
    }

    /// Checked before anything is allocated: the length is the runtime's to
    /// write, and a large one would otherwise be a large buffer.
    #[test]
    fn a_head_too_long_or_empty_is_refused() {
        assert!(head_len((HEAD_LIMIT + 1).to_be_bytes()).is_err());
        assert!(head_len(u32::MAX.to_be_bytes()).is_err());
        assert!(head_len(0u32.to_be_bytes()).is_err());
    }
}
