//! AWS Signature Version 4, for the requests the gateway makes to AWS.
//!
//! Only what a JSON POST to one of AWS's own APIs needs: the headers signed are
//! `host`, `x-amz-date` and, with temporary credentials, the session token.
//! Signing more is allowed and buys nothing here; signing fewer is refused.
//! Checked offline against AWS's published test vectors and against AWS's own
//! Python signer on a Bedrock-shaped request, since SigV4 is deterministic:
//! the same request, keys and time make the same signature.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// Keys to sign with: an access key pair, and a session token where the keys
/// are temporary (a role, or a pod's identity).
#[derive(Clone)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

/// The headers that make a request signed, to add to it as they are.
pub struct Signed {
    pub headers: Vec<(&'static str, String)>,
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(data.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

/// A path as SigV4 canonicalizes it for every service but S3: each segment
/// percent-encoded again, so a segment that already carries an escape --
/// Bedrock's model ids, with their `%3A` -- is signed as `%253A`.
fn canonical_path(path: &str) -> String {
    if path.is_empty() {
        return "/".to_string();
    }
    path.split('/')
        .map(|segment| {
            segment
                .bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                        (b as char).to_string()
                    }
                    other => format!("%{other:02X}"),
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Signs a request.
///
/// `path` is the request's path as it will be sent, already escaped; `query` is
/// the raw query string or empty; `amz_date` is the time as `YYYYMMDDTHHMMSSZ`,
/// taken as an argument so a test can fix it.
#[allow(clippy::too_many_arguments)]
pub fn sign(
    method: &str,
    host: &str,
    path: &str,
    query: &str,
    body: &[u8],
    credentials: &Credentials,
    region: &str,
    service: &str,
    amz_date: &str,
) -> Signed {
    let date = &amz_date[..8];

    // Lowercase names, trimmed values, sorted by name -- which these already
    // are, and a token sorts after the date.
    let mut headers: Vec<(&'static str, String)> = vec![
        ("host", host.to_string()),
        ("x-amz-date", amz_date.to_string()),
    ];
    if let Some(token) = &credentials.session_token {
        headers.push(("x-amz-security-token", token.clone()));
    }
    let canonical_headers: String = headers
        .iter()
        .map(|(name, value)| format!("{name}:{}\n", value.trim()))
        .collect();
    let signed_headers = headers
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(";");

    let canonical_request = format!(
        "{method}\n{}\n{query}\n{canonical_headers}\n{signed_headers}\n{}",
        canonical_path(path),
        hex::encode(Sha256::digest(body)),
    );
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes())),
    );

    let key = hmac(
        format!("AWS4{}", credentials.secret_access_key).as_bytes(),
        date,
    );
    let key = hmac(&key, region);
    let key = hmac(&key, service);
    let key = hmac(&key, "aws4_request");
    let signature = hex::encode(hmac(&key, &string_to_sign));

    let mut out: Vec<(&'static str, String)> = headers
        .into_iter()
        .filter(|(name, _)| *name != "host")
        .collect();
    out.push((
        "authorization",
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            credentials.access_key_id
        ),
    ));
    Signed { headers: out }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> Credentials {
        Credentials {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
        }
    }

    fn authorization(signed: &Signed) -> &str {
        signed
            .headers
            .iter()
            .find(|(name, _)| *name == "authorization")
            .map(|(_, value)| value.as_str())
            .expect("an authorization header")
    }

    /// `get-vanilla` from AWS's SigV4 test suite: the simplest request there
    /// is, and the signature AWS publishes for it.
    #[test]
    fn matches_aws_published_vector() {
        let signed = sign(
            "GET",
            "example.amazonaws.com",
            "/",
            "",
            b"",
            &example(),
            "us-east-1",
            "service",
            "20150830T123600Z",
        );
        assert_eq!(
            authorization(&signed),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    /// A streamed Bedrock invoke, signed by AWS's own Python signer
    /// (botocore's `SigV4Auth`) with the same made-up keys, body and time --
    /// including the path's escaped colon, which is signed escaped again.
    #[test]
    fn matches_awss_own_signer_on_a_bedrock_request() {
        let body = br#"{"anthropic_version":"bedrock-2023-05-31","max_tokens":16,"messages":[{"role":"user","content":"hi"}]}"#;
        let signed = sign(
            "POST",
            "bedrock-runtime.us-east-1.amazonaws.com",
            "/model/anthropic.claude-sonnet-4-5-20250929-v1%3A0/invoke-with-response-stream",
            "",
            body,
            &Credentials {
                session_token: Some("SESSIONTOKENEXAMPLE".into()),
                ..example()
            },
            "us-east-1",
            "bedrock",
            "20261014T120000Z",
        );
        assert_eq!(authorization(&signed), BOTOCORE_BEDROCK);
        assert!(
            signed
                .headers
                .iter()
                .any(|(n, v)| *n == "x-amz-security-token" && v == "SESSIONTOKENEXAMPLE")
        );
    }

    /// Computed by botocore; see the test above for the request.
    const BOTOCORE_BEDROCK: &str = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261014/us-east-1/bedrock/aws4_request, SignedHeaders=host;x-amz-date;x-amz-security-token, Signature=34ec3eb5cf682f9416edf48d2a64da5ed02ecba203280597fda77b2ffcea7188";
}
