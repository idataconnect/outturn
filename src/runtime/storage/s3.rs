use async_trait::async_trait;
use s3::creds::Credentials;
use s3::{Bucket, Region};

use s3::error::S3Error;

use super::{FileMetadata, StorageBackend, StorageError};

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
                (403, _) | (_, "AccessDenied") | (_, "InvalidAccessKeyId") | (_, "SignatureDoesNotMatch") => {
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
        tracing::info!(bucket = self.bucket.name(), "creating object storage bucket");
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
    /// on its own. One rule for every tenant, because the scope is the prefix
    /// (see docs/storage.md); a hierarchy laid out the obvious way would need
    /// a rule per agent and hit the per-bucket cap almost at once.
    ///
    /// Best effort: some S3-compatible stores do not do lifecycle, and a
    /// missing sweep is a growing bill rather than a broken agent.
    pub async fn ensure_session_lifecycle(&self, days: u32) -> Result<(), StorageError> {
        use s3::serde_types::{BucketLifecycleConfiguration, Expiration, LifecycleFilter, LifecycleRule};
        let rule = LifecycleRule {
            id: Some("sweep-session-files".into()),
            status: "Enabled".into(),
            filter: Some(LifecycleFilter {
                prefix: Some(super::scope::Scope::Session.bucket_prefix().to_string()),
                ..Default::default()
            }),
            expiration: Some(Expiration {
                days: Some(days),
                ..Default::default()
            }),
            ..Default::default()
        };
        self.bucket
            .put_bucket_lifecycle(BucketLifecycleConfiguration::new(vec![rule]))
            .await
            .map(|_| ())
            .map_err(classify)
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
    async fn read(
        &self,
        path: &str,
        offset: u64,
        len: u32,
    ) -> Result<Vec<u8>, StorageError> {
        let key = self.key(path);

        if offset == 0 && len == u32::MAX {
            let response = self
                .bucket
                .get_object(&key)
                .await
                .map_err(classify)?;

            if response.status_code() == 404 {
                return Err(StorageError::NotFound);
            }

            return Ok(response.to_vec());
        }

        let end = offset.saturating_add(len as u64).saturating_sub(1);

        let response = self
            .bucket
            .get_object_range(&key, offset, Some(end))
            .await
            .map_err(classify)?;

        Ok(response.to_vec())
    }

    async fn write(
        &self,
        path: &str,
        offset: u64,
        data: &[u8],
    ) -> Result<u64, StorageError> {
        let key = self.key(path);

        if offset != 0 {
            let existing = match self.bucket.get_object(&key).await {
                Ok(resp) => resp.to_vec(),
                Err(_) => vec![],
            };
            let start = offset as usize;
            let needed = start + data.len();
            let mut buf = existing;
            if buf.len() < needed {
                buf.resize(needed, 0);
            }
            buf[start..start + data.len()].copy_from_slice(data);
            self.bucket
                .put_object(&key, &buf)
                .await
                .map_err(classify)?;
        } else {
            self.bucket
                .put_object(&key, data)
                .await
                .map_err(classify)?;
        }

        Ok(data.len() as u64)
    }

    async fn stat(&self, path: &str) -> Result<FileMetadata, StorageError> {
        let key = self.key(path);
        let (head, _code) = self
            .bucket
            .head_object(&key)
            .await
            .map_err(classify)?;

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

    async fn delete(&self, path: &str) -> Result<(), StorageError> {
        let key = self.key(path);
        self.bucket
            .delete_object(&key)
            .await
            .map_err(classify)?;
        Ok(())
    }
}
