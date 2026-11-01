//! A file streamed in as a request's body, behind the request's head.
//!
//! See `docs/file-bodies.md`. The runtime sends the head and then the file on
//! one connection; this reads the head, holds the rest unread while the
//! request is vetted, and then either forwards exactly the declared length
//! into the request being sent or drains it so a refusal reaches the runtime
//! intact.

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use outturn_core::egress::file_body::{IDLE, head_len};

type Chunks = BoxStream<'static, Result<Bytes, std::io::Error>>;

/// What is left of the stream once the head has been read.
struct Rest {
    chunks: Chunks,
    /// Read from the network beyond what was taken.
    pending: Bytes,
    /// How much of the file is still to come.
    remaining: u64,
}

impl Rest {
    /// The next piece of the file, no more than what is left of it, or why
    /// there is none. Each piece is waited for for `IDLE` at most.
    async fn next(&mut self) -> Option<Result<Bytes, String>> {
        if self.remaining == 0 {
            return None;
        }
        if self.pending.is_empty() {
            match tokio::time::timeout(IDLE, self.chunks.next()).await {
                Err(_) => return Some(Err("the file stopped arriving".into())),
                Ok(None) => return Some(Err("the file ended before its length".into())),
                Ok(Some(Err(e))) => return Some(Err(format!("the file did not arrive: {e}"))),
                Ok(Some(Ok(chunk))) => self.pending = chunk,
            }
        }
        let n = self.pending.len().min(self.remaining as usize);
        self.remaining -= n as u64;
        Some(Ok(self.pending.split_to(n)))
    }
}

/// A file arriving as a body, which a transport sends and a refusal drains.
///
/// Shared, because the request that carries it is cloned on its way to the
/// transport, and because whatever the transport did not read -- a connection
/// that was never made, say -- still has to be drained before anybody is
/// answered.
#[derive(Clone)]
pub struct FileBody {
    pub size: u64,
    rest: Arc<tokio::sync::Mutex<Rest>>,
}

impl std::fmt::Debug for FileBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileBody")
            .field("size", &self.size)
            .finish()
    }
}

impl FileBody {
    /// Reads the head from a streamed body and keeps the rest unread: the
    /// request, as JSON, and what follows it. The file's length is not known
    /// until the head is read, so it is given with `declare`.
    pub async fn read_head(
        chunks: impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
    ) -> Result<(Vec<u8>, Unsized), String> {
        let mut chunks: Chunks = chunks.boxed();
        let mut buffered: Vec<u8> = Vec::new();
        let mut want = 4usize;
        let mut declared: Option<usize> = None;
        loop {
            if buffered.len() >= want {
                match declared {
                    None => {
                        let n = head_len(buffered[..4].try_into().expect("four bytes"))?;
                        declared = Some(n);
                        want = 4 + n;
                        continue;
                    }
                    Some(n) => {
                        let pending = Bytes::from(buffered.split_off(4 + n));
                        let head = buffered.split_off(4);
                        return Ok((head, Unsized { chunks, pending }));
                    }
                }
            }
            match tokio::time::timeout(IDLE, chunks.next()).await {
                Ok(Some(Ok(chunk))) => buffered.extend_from_slice(&chunk),
                Ok(Some(Err(e))) => return Err(format!("the request did not arrive: {e}")),
                Ok(None) | Err(_) => return Err("the request did not arrive whole".into()),
            }
        }
    }

    /// The file as the body of the request being sent: exactly `size` bytes,
    /// or an error that fails the send.
    pub fn stream(
        &self,
    ) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Send + use<> {
        let rest = self.rest.clone();
        futures::stream::unfold(rest, |rest| async move {
            let next = rest.lock().await.next().await?;
            Some((next.map_err(std::io::Error::other), rest))
        })
    }

    /// Reads and discards whatever of the file has not been sent, so an answer
    /// written now is read rather than lost to a reset. Gives up on a runtime
    /// that stalls, and the answer goes anyway.
    pub async fn drain(&self) {
        let mut rest = self.rest.lock().await;
        while let Some(Ok(_)) = rest.next().await {}
    }
}

/// The stream after its head, before the head has said how long the file is.
pub struct Unsized {
    chunks: Chunks,
    pending: Bytes,
}

impl Unsized {
    pub fn declare(self, size: u64) -> FileBody {
        FileBody {
            size,
            rest: Arc::new(tokio::sync::Mutex::new(Rest {
                chunks: self.chunks,
                pending: self.pending,
                remaining: size,
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outturn_core::egress::file_body::head;

    fn chunks(
        parts: Vec<Vec<u8>>,
    ) -> impl futures::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static {
        futures::stream::iter(parts.into_iter().map(|p| Ok(Bytes::from(p))))
    }

    async fn collect(file: &FileBody) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        let mut s = std::pin::pin!(file.stream());
        while let Some(chunk) = s.next().await {
            out.extend_from_slice(&chunk.map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// However the network splits it, the head is the head and the file is
    /// the file, to the byte.
    #[tokio::test]
    async fn a_head_and_its_file_are_told_apart_however_they_arrive() {
        let mut framed = head(br#"{"a":1}"#);
        framed.extend_from_slice(b"file bytes");
        for split in 1..framed.len() {
            let (one, two) = framed.split_at(split);
            let (h, rest) = FileBody::read_head(chunks(vec![one.to_vec(), two.to_vec()]))
                .await
                .expect("a head");
            assert_eq!(h, br#"{"a":1}"#);
            let file = rest.declare(10);
            assert_eq!(collect(&file).await.unwrap(), b"file bytes");
        }
    }

    /// Never more than was declared, so a runtime sending extra cannot make
    /// the request carry it.
    #[tokio::test]
    async fn no_more_than_the_declared_length_is_forwarded() {
        let mut framed = head(b"{}");
        framed.extend_from_slice(b"0123456789extra");
        let (_, rest) = FileBody::read_head(chunks(vec![framed])).await.unwrap();
        assert_eq!(collect(&rest.declare(10)).await.unwrap(), b"0123456789");
    }

    #[tokio::test]
    async fn a_file_that_ends_short_fails_the_send() {
        let mut framed = head(b"{}");
        framed.extend_from_slice(b"short");
        let (_, rest) = FileBody::read_head(chunks(vec![framed])).await.unwrap();
        let failed = collect(&rest.declare(10)).await.unwrap_err();
        assert!(failed.contains("ended before its length"), "{failed}");
    }

    #[tokio::test]
    async fn a_head_that_never_finishes_is_not_a_request() {
        let mut framed = head(br#"{"a":1}"#);
        framed.truncate(6);
        assert!(FileBody::read_head(chunks(vec![framed])).await.is_err());
    }

    /// Draining leaves nothing for the transport, and a drained file sends
    /// nothing if it is somehow sent after.
    #[tokio::test]
    async fn a_drained_file_is_read_to_its_end() {
        let mut framed = head(b"{}");
        framed.extend_from_slice(&[7u8; 1000]);
        let (_, rest) = FileBody::read_head(chunks(vec![framed])).await.unwrap();
        let file = rest.declare(1000);
        file.drain().await;
        assert_eq!(file.rest.lock().await.remaining, 0);
        assert_eq!(collect(&file).await.unwrap(), Vec::<u8>::new());
    }
}
