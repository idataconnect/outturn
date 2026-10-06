use async_trait::async_trait;
use s3::creds::Credentials;
use s3::{Bucket, Region};

use s3::error::S3Error;

use super::{FileMetadata, ObjectReader, ObjectWriter, StorageBackend, StorageError};

/// Reads the `<Code>` out of an S3 error document.
fn s3_error_code(body: &str) -> Option<&str> {
    let start = body.find("<Code>")? + "<Code>".len();
    let end = body[start..].find("</Code>")? + start;
    Some(&body[start..end])
}

/// Sorts an S3 failure into whose problem it is.
///
/// The client reports a missing bucket as a parse error, because it tried to
/// read the error document as the listing it asked for. The document itself
/// carries a code that says what happened, and that code is what decides
/// whether the caller asked for something that is not there or the platform
/// is not there at all.
fn classify(e: S3Error) -> StorageError {
    match e {
        S3Error::HttpFailWithBody(status, body) => {
            let code = s3_error_code(&body).unwrap_or_default();
            match (status, code) {
                (404, "NoSuchKey") | (404, "") => StorageError::NotFound,
                (_, "NoSuchBucket") => {
                    StorageError::Unavailable("the bucket does not exist".into())
                }
                (403, _)
                | (_, "AccessDenied")
                | (_, "InvalidAccessKeyId")
                | (_, "SignatureDoesNotMatch") => {
                    StorageError::Unavailable(format!("storage refused the credentials ({code})"))
                }
                (status, code) if status >= 500 => {
                    StorageError::Unavailable(format!("storage answered {status} {code}"))
                }
                (status, code) => StorageError::Io(format!("storage answered {status} {code}")),
            }
        }
        // Nothing came back at all: the endpoint is down, misnamed or refusing
        // connections. That is the platform, whatever the caller asked for.
        S3Error::Reqwest(e) => StorageError::Unavailable(format!("could not reach storage: {e}")),
        S3Error::HttpFail => StorageError::Unavailable("storage did not answer".into()),
        // The reply was not what a listing or object looks like. The most
        // common cause is an error document from an endpoint that is not a
        // working bucket, so it is the platform's until proven otherwise.
        S3Error::SerdeXml(e) => {
            StorageError::Unavailable(format!("storage answered with something unexpected: {e}"))
        }
        other => StorageError::Io(other.to_string()),
    }
}

pub struct S3Storage {
    bucket: Box<Bucket>,
    prefix: String,
}

impl S3Storage {
    pub fn new(
        endpoint: &str,
        bucket_name: &str,
        access_key: &str,
        secret_key: &str,
        prefix: String,
    ) -> Result<Self, StorageError> {
        // rust-s3 retries every failed request once, a second later, and to it
        // a 404 is a failure. So every look for an object that is not there --
        // the stale extraction an upload clears, a read of a file that does
        // not exist, a delete's check, the backfill's two per file -- cost a
        // whole second: a one-byte upload took two. A missing object is an
        // answer, not a fault, and the library cannot tell them apart, so its
        // retry is off. Process-wide, which is the only way it is offered.
        s3::set_retries(0);

        let region = Region::Custom {
            region: "us-east-1".to_string(),
            endpoint: endpoint.to_string(),
        };

        let credentials = Credentials::new(Some(access_key), Some(secret_key), None, None, None)
            .map_err(|e| StorageError::Unavailable(format!("storage credentials: {e}")))?;

        let bucket = Bucket::new(bucket_name, region, credentials)
            .map_err(|e| StorageError::Unavailable(format!("storage endpoint: {e}")))?
            .with_path_style();

        Ok(Self {
            bucket: Box::new(*bucket),
            prefix,
        })
    }

    /// Makes sure the bucket is there, creating it when it is not.
    ///
    /// A missing bucket does not fail loudly on its own: every request comes
    /// back as an S3 error document that the client then tries to read as the
    /// listing it asked for, and the guest is told "serde xml: missing field
    /// Name" -- which is true, unhelpful, and was the whole of what an agent
    /// asked to list its files got told. Creating it here means a fresh
    /// MinIO, or a fresh account, works without a hand step nobody wrote down.
    pub async fn ensure_bucket(&self) -> Result<(), StorageError> {
        match self.bucket.exists().await {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(e) => return Err(classify(e)),
        }
        tracing::info!(
            bucket = self.bucket.name(),
            "creating object storage bucket"
        );
        Bucket::create_with_path_style(
            &self.bucket.name(),
            self.bucket.region(),
            self.bucket.credentials().await.map_err(classify)?,
            s3::BucketConfiguration::private(),
        )
        .await
        .map(|_| ())
        .map_err(classify)
    }

    /// Sweeps session-scope files after `days`, with a rule the store applies
    /// on its own. One rule for every workspace, because the scope is the prefix
    /// (see docs/storage.md); a hierarchy laid out the obvious way would need
    /// a rule per agent and hit the per-bucket cap almost at once.
    ///
    /// Best effort: some S3-compatible stores do not do lifecycle, and a
    /// missing sweep is a growing bill rather than a broken agent.
    pub async fn ensure_session_lifecycle(&self, days: u32) -> Result<(), StorageError> {
        use s3::serde_types::{
            AbortIncompleteMultipartUpload, BucketLifecycleConfiguration, Expiration,
            LifecycleFilter, LifecycleRule,
        };
        let sweep = |id: &str, prefix: String| LifecycleRule {
            id: Some(id.into()),
            status: "Enabled".into(),
            filter: Some(LifecycleFilter {
                prefix: Some(prefix),
                ..Default::default()
            }),
            expiration: Some(Expiration {
                days: Some(days),
                ..Default::default()
            }),
            ..Default::default()
        };
        let session = super::scope::Scope::Session.bucket_prefix().to_string();
        let sweeps = vec![
            sweep("sweep-session-files", session.clone()),
            // The text read out of session documents lives under its own
            // prefix and would otherwise outlive what it was read from.
            sweep(
                "sweep-session-text",
                crate::api::extract::text_key(&session),
            ),
        ];
        // Parts of an upload no writer completed or aborted -- a pod that died
        // mid-write. Invisible, but stored and billed until removed.
        let abandoned = LifecycleRule {
            id: Some("abort-abandoned-uploads".into()),
            status: "Enabled".into(),
            filter: Some(LifecycleFilter {
                prefix: Some(String::new()),
                ..Default::default()
            }),
            abort_incomplete_multipart_upload: Some(AbortIncompleteMultipartUpload {
                days_after_initiation: Some(1),
            }),
            ..Default::default()
        };

        let mut rules = sweeps.clone();
        rules.push(abandoned);
        match self
            .bucket
            .put_bucket_lifecycle(BucketLifecycleConfiguration::new(rules))
            .await
            .map_err(classify)
        {
            Ok(_) => Ok(()),
            // MinIO refuses the abort rule outright -- it expires stale
            // uploads on its own -- and a bucket has one lifecycle document,
            // so a refusal of that rule would cost the sweeps with it. They
            // go in alone, and the store is left to its own cleanup.
            Err(StorageError::Io(e)) if e.contains("InvalidArgument") => {
                tracing::info!(
                    "storage refused the rule for abandoned uploads; leaving those to the store"
                );
                self.bucket
                    .put_bucket_lifecycle(BucketLifecycleConfiguration::new(sweeps))
                    .await
                    .map(|_| ())
                    .map_err(classify)
            }
            Err(e) => Err(e),
        }
    }

    fn key(&self, path: &str) -> String {
        if self.prefix.is_empty() {
            path.to_string()
        } else {
            format!("{}/{}", self.prefix, path)
        }
    }
}

#[async_trait]
impl StorageBackend for S3Storage {
    async fn read(&self, path: &str, offset: u64, len: u32) -> Result<Vec<u8>, StorageError> {
        let key = self.key(path);

        if offset == 0 && len == u32::MAX {
            // A missing key is an error from the client now (fail-on-err), and
            // `classify` turns it into NotFound; nothing reaches here but 2xx.
            let response = self.bucket.get_object(&key).await.map_err(classify)?;
            return Ok(response.to_vec());
        }

        let end = offset.saturating_add(len as u64).saturating_sub(1);

        let response = match self.bucket.get_object_range(&key, offset, Some(end)).await {
            Ok(r) => r,
            // Past the end is empty, not a failure. A caller reading in
            // windows asks for one more window than there is, and must be
            // told "nothing" rather than refused -- or handed the XML that
            // says so as if it were the file.
            Err(S3Error::HttpFailWithBody(416, _)) => return Ok(Vec::new()),
            Err(e) => return Err(classify(e)),
        };
        Ok(response.to_vec())
    }

    async fn write(&self, path: &str, data: &[u8]) -> Result<u64, StorageError> {
        let key = self.key(path);
        self.bucket.put_object(&key, data).await.map_err(classify)?;
        Ok(data.len() as u64)
    }

    async fn open_writer(&self, path: &str) -> Result<Box<dyn ObjectWriter>, StorageError> {
        Ok(Box::new(S3Writer {
            bucket: self.bucket.clone(),
            key: self.key(path),
            buf: Vec::new(),
            upload_id: None,
            parts: Vec::new(),
            size: 0,
        }))
    }

    async fn open_reader(&self, path: &str) -> Result<Box<dyn ObjectReader>, StorageError> {
        let response = self
            .bucket
            .get_object_stream(self.key(path))
            .await
            .map_err(classify)?;
        Ok(Box::new(S3Reader {
            stream: response.bytes,
            pending: bytes::Bytes::new(),
        }))
    }

    async fn stat(&self, path: &str) -> Result<FileMetadata, StorageError> {
        let key = self.key(path);
        let (head, _code) = self.bucket.head_object(&key).await.map_err(classify)?;

        let size = head.content_length.unwrap_or(0) as u64;

        Ok(FileMetadata {
            path: path.to_string(),
            size,
            is_dir: false,
        })
    }

    async fn list(&self, prefix: &str) -> Result<Vec<FileMetadata>, StorageError> {
        let full_prefix = self.key(prefix);
        let results = self
            .bucket
            .list(full_prefix.clone(), None)
            .await
            .map_err(classify)?;

        let prefix_strip = if self.prefix.is_empty() {
            "".to_string()
        } else {
            format!("{}/", self.prefix)
        };

        let entries = results
            .into_iter()
            .flat_map(|page| page.contents)
            .map(|obj| {
                let path = obj.key.strip_prefix(&prefix_strip).unwrap_or(&obj.key);
                FileMetadata {
                    path: path.to_string(),
                    size: obj.size,
                    is_dir: false,
                }
            })
            .collect();

        Ok(entries)
    }

    async fn list_page(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<(Vec<FileMetadata>, bool), StorageError> {
        // `start_after` rather than a continuation token: the caller holds a
        // key, which it can carry in a URL and which means the same thing to
        // the next request whichever replica serves it.
        let (result, _code) = self
            .bucket
            .list_page(
                self.key(prefix),
                None,
                None,
                after.map(|a| self.key(a)),
                Some(limit),
            )
            .await
            .map_err(classify)?;
        let prefix_strip = if self.prefix.is_empty() {
            "".to_string()
        } else {
            format!("{}/", self.prefix)
        };
        let entries = result
            .contents
            .into_iter()
            .map(|obj| FileMetadata {
                path: obj
                    .key
                    .strip_prefix(&prefix_strip)
                    .unwrap_or(&obj.key)
                    .to_string(),
                size: obj.size,
                is_dir: false,
            })
            .collect();
        Ok((entries, result.is_truncated))
    }

    async fn delete(&self, path: &str) -> Result<(), StorageError> {
        // S3 answers a delete of a missing key with success, so without this
        // a mistyped path is reported as a file removed. Racy against a
        // concurrent delete, which only means both callers are told it went.
        self.stat(path).await?;
        let key = self.key(path);
        self.bucket.delete_object(&key).await.map_err(classify)?;
        Ok(())
    }
}

/// How much a writer holds before it sends a part.
///
/// S3's floor for every part but the last. Larger would mean fewer requests
/// for a large object, and more held per writer for every object; this is the
/// figure admission charges an open writer, so it stays at the floor.
pub const PART_BYTES: usize = 5 * 1024 * 1024;

const OCTET_STREAM: &str = "application/octet-stream";

/// A multipart upload, started only once there is more than one part's worth.
///
/// An object smaller than a part goes up whole with `put_object` when the
/// writer finishes: one request rather than three, and no upload to abort.
struct S3Writer {
    bucket: Box<Bucket>,
    key: String,
    buf: Vec<u8>,
    upload_id: Option<String>,
    parts: Vec<s3::serde_types::Part>,
    size: u64,
}

impl S3Writer {
    async fn send_part(&mut self, part: Vec<u8>) -> Result<(), StorageError> {
        let upload_id = match &self.upload_id {
            Some(id) => id.clone(),
            None => {
                let started = self
                    .bucket
                    .initiate_multipart_upload(&self.key, OCTET_STREAM)
                    .await
                    .map_err(classify)?;
                self.upload_id = Some(started.upload_id.clone());
                started.upload_id
            }
        };
        let number = self.parts.len() as u32 + 1;
        let sent = self
            .bucket
            .put_multipart_chunk(part, &self.key, number, &upload_id, OCTET_STREAM)
            .await
            .map_err(classify)?;
        self.parts.push(sent);
        Ok(())
    }
}

#[async_trait]
impl ObjectWriter for S3Writer {
    async fn write(&mut self, chunk: &[u8]) -> Result<(), StorageError> {
        self.buf.extend_from_slice(chunk);
        self.size += chunk.len() as u64;
        while self.buf.len() >= PART_BYTES {
            let rest = self.buf.split_off(PART_BYTES);
            let part = std::mem::replace(&mut self.buf, rest);
            self.send_part(part).await?;
        }
        Ok(())
    }

    async fn finish(mut self: Box<Self>) -> Result<u64, StorageError> {
        let Some(upload_id) = self.upload_id.clone() else {
            let buf = std::mem::take(&mut self.buf);
            self.bucket
                .put_object(&self.key, &buf)
                .await
                .map_err(classify)?;
            return Ok(self.size);
        };
        if !self.buf.is_empty() {
            let last = std::mem::take(&mut self.buf);
            self.send_part(last).await?;
        }
        let parts = std::mem::take(&mut self.parts);
        self.bucket
            .complete_multipart_upload(&self.key, &upload_id, parts)
            .await
            .map_err(classify)?;
        // Completed, so there is nothing for Drop to abort.
        self.upload_id = None;
        Ok(self.size)
    }
}

impl Drop for S3Writer {
    /// Abandons an upload that never completed.
    ///
    /// The parts are invisible -- no object exists until completion -- but
    /// they are stored and billed until somebody aborts them. Spawned because
    /// a drop cannot wait, and best effort because the bucket's lifecycle
    /// rule catches whatever this misses.
    fn drop(&mut self) {
        let Some(upload_id) = self.upload_id.take() else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let bucket = self.bucket.clone();
        let key = std::mem::take(&mut self.key);
        handle.spawn(async move {
            if let Err(e) = bucket.abort_upload(&key, &upload_id).await {
                tracing::debug!(error = %e, "could not abort an abandoned upload");
            }
        });
    }
}

struct S3Reader {
    stream: s3::request::DataStream,
    /// What the last chunk from the network held beyond what was asked for.
    pending: bytes::Bytes,
}

#[async_trait]
impl ObjectReader for S3Reader {
    async fn read(&mut self, max: usize) -> Result<Vec<u8>, StorageError> {
        use futures::StreamExt;
        while self.pending.is_empty() {
            match self.stream.next().await {
                Some(Ok(chunk)) => self.pending = chunk,
                Some(Err(e)) => return Err(classify(e)),
                None => return Ok(Vec::new()),
            }
        }
        let n = max.min(self.pending.len());
        Ok(self.pending.split_to(n).to_vec())
    }
}
