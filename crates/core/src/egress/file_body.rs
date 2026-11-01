//! A request whose body is a stored file, sent without the file passing
//! through the tier that asked.
//!
//! See `docs/file-bodies.md`. The guest names a path; the runtime, which can
//! already read it, signs a link that reads that one object for a little while;
//! the gateway reads the link into the request it sends. What travels between
//! the tiers is this, never the bytes.
//!
//! The link is the one thing here a compromised runtime chooses, so the
//! gateway reads nothing but the object store with it (`Store::admit`).
//! Without that check the gateway would fetch any URL it was handed from
//! inside the cluster and send the answer somewhere an agent may reach, which
//! is a way past every egress rule there is.

use serde::{Deserialize, Serialize};

/// The largest file sent as a body. Bounded by the time a request is given
/// rather than by memory, since the bytes are streamed: a body much larger
/// than this does not reach most APIs inside the gateway's allowance.
pub const LIMIT: u64 = 32 * 1024 * 1024;

/// How long the link stays good. Long enough for the request it was signed
/// for, and no longer, since it outlives nothing it was made for.
pub const LINK_SECONDS: u32 = 120;

/// A file to send as a body, as the runtime hands it to the gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BodyFile {
    /// A signed link that reads the object.
    pub url: String,
    /// Its size as stored, sent as the request's `Content-Length`. A read that
    /// comes back a different length fails the request rather than sending
    /// something other than what was said.
    pub size: u64,
}

/// The object store a gateway may read a body from.
#[derive(Debug, Clone)]
pub struct Store {
    origin: reqwest::Url,
    bucket: String,
}

impl Store {
    /// The store the runtime writes to, named the same way it is named there.
    /// `None` when none is configured, and a file body is then refused.
    pub fn from_env() -> Option<Self> {
        let endpoint = std::env::var("OUTTURN_S3_ENDPOINT").ok()?;
        let bucket = std::env::var("OUTTURN_S3_BUCKET").unwrap_or_else(|_| "outturn".into());
        Self::new(&endpoint, &bucket)
    }

    pub fn new(endpoint: &str, bucket: &str) -> Option<Self> {
        let origin = reqwest::Url::parse(endpoint).ok()?;
        if !matches!(origin.scheme(), "http" | "https") || origin.host_str().is_none() {
            return None;
        }
        Some(Self {
            origin,
            bucket: bucket.to_string(),
        })
    }

    /// The link, if it reads from this store's bucket and nowhere else.
    ///
    /// Same scheme, host and port as the endpoint, a path inside the bucket,
    /// and nothing that could make the request mean something else on the way:
    /// no user, no fragment. Whether the signature is good is the store's to
    /// say when it is read.
    pub fn admit(&self, link: &str) -> Result<reqwest::Url, String> {
        let refused = || "that file cannot be sent from here".to_string();
        let url = reqwest::Url::parse(link).map_err(|_| refused())?;
        let same_origin = url.scheme() == self.origin.scheme()
            && url.host_str() == self.origin.host_str()
            && url.port_or_known_default() == self.origin.port_or_known_default();
        let in_bucket = url
            .path()
            .strip_prefix('/')
            .and_then(|p| p.strip_prefix(self.bucket.as_str()))
            .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1);
        let plain =
            url.username().is_empty() && url.password().is_none() && url.fragment().is_none();
        // A path that walks out of the bucket once the store resolves it. The
        // parser has already folded `..` away; an encoded one it leaves alone.
        let lowered = url.path().to_ascii_lowercase();
        let walks = lowered.contains("%2e") || lowered.contains("%2f") || lowered.contains("%5c");
        if same_origin && in_bucket && plain && !walks {
            Ok(url)
        } else {
            Err(refused())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::new("http://outturn-minio:9000", "outturn").unwrap()
    }

    #[test]
    fn a_link_into_the_bucket_is_admitted() {
        let link = "http://outturn-minio:9000/outturn/ws/session/a.heic?X-Amz-Signature=ab";
        assert!(store().admit(link).is_ok());
    }

    /// Everything a compromised runtime might hand the gateway instead, each of
    /// which would have it fetch from inside the cluster on the runtime's say.
    #[test]
    fn a_link_anywhere_else_is_refused() {
        for link in [
            "http://outturn-api:8080/v1/admin",
            "http://outturn-minio:9001/outturn/x",
            "https://outturn-minio:9000/outturn/x",
            "http://outturn-minio:9000/other/x",
            "http://outturn-minio:9000/outturnx/y",
            "http://outturn-minio:9000/outturn/",
            "http://outturn-minio:9000/outturn",
            "http://outturn-minio:9000/outturn/../other/x",
            "http://outturn-minio:9000/outturn/%2e%2e/other/x",
            "http://outturn-minio:9000/outturn/a%2F..%2Fb",
            "http://user@outturn-minio:9000/outturn/x",
            "http://outturn-minio:9000/outturn/x#y",
            "file:///etc/passwd",
            "not a url",
        ] {
            assert!(store().admit(link).is_err(), "{link} was admitted");
        }
    }
}
