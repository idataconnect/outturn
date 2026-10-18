//! The AWS credentials the gateway signs Bedrock requests with, wherever its
//! pod gets them.
//!
//! Three sources, in the order AWS's own tools try them:
//!
//! - **Keys** in the environment (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
//!   `AWS_SESSION_TOKEN`), which never change while the process runs.
//! - **A role for the pod's service account** (`AWS_ROLE_ARN` and
//!   `AWS_WEB_IDENTITY_TOKEN_FILE`): the token Kubernetes mounts is exchanged
//!   with STS for temporary keys (`AssumeRoleWithWebIdentity`).
//! - **The pod's identity** (`AWS_CONTAINER_CREDENTIALS_FULL_URI` and
//!   `AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE`): the node's agent hands out
//!   temporary keys for the token.
//!
//! The last two are what a gateway running on AWS should use, since nothing
//! long-lived is kept anywhere: keys from either are held until five minutes
//! before they expire and then asked for again, and the token file is read
//! afresh each time, because Kubernetes rotates it. One source for the whole
//! gateway -- a pod has one identity -- however many accounts sign with it.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Utc};

use super::ProviderError;
use super::sigv4::Credentials;

/// How long before they expire temporary keys are replaced: long enough that
/// a request signed just before cannot reach AWS just after.
const REFRESH_BEFORE: Duration = Duration::from_secs(300);

enum Source {
    Keys(Credentials),
    WebIdentity {
        role_arn: String,
        token_file: String,
        session_name: String,
        sts_url: String,
    },
    PodIdentity {
        url: String,
        token_file: String,
    },
}

pub struct AwsCredentials {
    source: Source,
    client: reqwest::Client,
    held: tokio::sync::Mutex<Option<(Credentials, Option<DateTime<Utc>>)>>,
}

impl AwsCredentials {
    /// The gateway's, read from its environment once. None when it was given
    /// none of the three.
    pub fn shared() -> Option<Arc<Self>> {
        static SHARED: OnceLock<Option<Arc<AwsCredentials>>> = OnceLock::new();
        SHARED
            .get_or_init(|| Self::from_env(|name| std::env::var(name).ok()).map(Arc::new))
            .clone()
    }

    fn from_env(var: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let var = |name: &str| var(name).filter(|v| !v.trim().is_empty());
        let source = if let (Some(access_key_id), Some(secret_access_key)) =
            (var("AWS_ACCESS_KEY_ID"), var("AWS_SECRET_ACCESS_KEY"))
        {
            Source::Keys(Credentials {
                access_key_id,
                secret_access_key,
                session_token: var("AWS_SESSION_TOKEN"),
            })
        } else if let (Some(role_arn), Some(token_file)) =
            (var("AWS_ROLE_ARN"), var("AWS_WEB_IDENTITY_TOKEN_FILE"))
        {
            let region = var("AWS_REGION")
                .or_else(|| var("AWS_DEFAULT_REGION"))
                .unwrap_or_else(|| "us-east-1".into());
            Source::WebIdentity {
                role_arn,
                token_file,
                session_name: var("AWS_ROLE_SESSION_NAME")
                    .unwrap_or_else(|| "outturn-gateway".into()),
                sts_url: var("AWS_ENDPOINT_URL_STS")
                    .unwrap_or_else(|| format!("https://sts.{region}.amazonaws.com")),
            }
        } else if let (Some(url), Some(token_file)) = (
            var("AWS_CONTAINER_CREDENTIALS_FULL_URI"),
            var("AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE"),
        ) {
            Source::PodIdentity { url, token_file }
        } else {
            return None;
        };
        Some(Self::new(source))
    }

    /// Keys given outright, which never change.
    pub fn fixed(keys: Credentials) -> Self {
        Self::new(Source::Keys(keys))
    }

    fn new(source: Source) -> Self {
        Self {
            source,
            client: outturn_core::http_client::reporting_client(),
            held: tokio::sync::Mutex::new(None),
        }
    }

    /// Keys to sign with now: the ones held while they are good for another
    /// five minutes, else fresh ones. Concurrent callers wait on one fetch.
    pub async fn current(&self) -> Result<Credentials, ProviderError> {
        let mut held = self.held.lock().await;
        if let Some((keys, expires)) = held.as_ref()
            && expires.is_none_or(|at| Utc::now() + REFRESH_BEFORE < at)
        {
            return Ok(keys.clone());
        }
        let fresh = self.fetch().await?;
        *held = Some(fresh.clone());
        Ok(fresh.0)
    }

    async fn fetch(&self) -> Result<(Credentials, Option<DateTime<Utc>>), ProviderError> {
        let failed = |what: String| ProviderError::Upstream {
            status: None,
            detail: format!("no AWS credentials: {what}"),
        };
        match &self.source {
            Source::Keys(keys) => Ok((keys.clone(), None)),
            Source::WebIdentity {
                role_arn,
                token_file,
                session_name,
                sts_url,
            } => {
                let token = read_token(token_file).map_err(failed)?;
                // A form body rather than a query string, so the token is
                // never in a URL a proxy or a log might keep.
                let response = self
                    .client
                    .post(sts_url)
                    .timeout(Duration::from_secs(30))
                    .form(&[
                        ("Action", "AssumeRoleWithWebIdentity"),
                        ("Version", "2011-06-15"),
                        ("RoleArn", role_arn.as_str()),
                        ("RoleSessionName", session_name.as_str()),
                        ("WebIdentityToken", token.as_str()),
                    ])
                    .send()
                    .await
                    .map_err(|e| failed(format!("STS could not be reached: {e}")))?;
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                if !status.is_success() {
                    return Err(failed(format!(
                        "STS answered {status}: {}",
                        element(&body, "Message").unwrap_or(&body)
                    )));
                }
                let keys = Credentials {
                    access_key_id: element(&body, "AccessKeyId")
                        .ok_or_else(|| failed("STS gave no access key".into()))?
                        .to_string(),
                    secret_access_key: element(&body, "SecretAccessKey")
                        .ok_or_else(|| failed("STS gave no secret key".into()))?
                        .to_string(),
                    session_token: element(&body, "SessionToken").map(str::to_string),
                };
                Ok((keys, element(&body, "Expiration").and_then(parse_time)))
            }
            Source::PodIdentity { url, token_file } => {
                let token = read_token(token_file).map_err(failed)?;
                let response = self
                    .client
                    .get(url)
                    .timeout(Duration::from_secs(30))
                    .header("authorization", token)
                    .send()
                    .await
                    .map_err(|e| {
                        failed(format!("the pod identity agent could not be reached: {e}"))
                    })?;
                let status = response.status();
                if !status.is_success() {
                    return Err(failed(format!("the pod identity agent answered {status}")));
                }
                let body: serde_json::Value = response
                    .json()
                    .await
                    .map_err(|e| failed(format!("the pod identity agent's answer: {e}")))?;
                let text = |k: &str| body[k].as_str().map(str::to_string);
                let keys = Credentials {
                    access_key_id: text("AccessKeyId")
                        .ok_or_else(|| failed("the agent gave no access key".into()))?,
                    secret_access_key: text("SecretAccessKey")
                        .ok_or_else(|| failed("the agent gave no secret key".into()))?,
                    session_token: text("Token"),
                };
                Ok((keys, body["Expiration"].as_str().and_then(parse_time)))
            }
        }
    }
}

fn read_token(path: &str) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map(|t| t.trim().to_string())
        .map_err(|e| format!("the token at {path} could not be read: {e}"))
}

fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// The text of the first `<name>` element in an STS answer. STS answers in
/// a small, fixed XML shape whose values never hold markup, so reading the
/// element by its tags is the whole of what parsing it needs.
fn element<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&format!("</{name}>"))? + start;
    Some(xml[start..end].trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn keys_come_first_then_a_role_then_the_pod() {
        let keys = AwsCredentials::from_env(env(&[
            ("AWS_ACCESS_KEY_ID", "a"),
            ("AWS_SECRET_ACCESS_KEY", "s"),
            ("AWS_ROLE_ARN", "r"),
            ("AWS_WEB_IDENTITY_TOKEN_FILE", "/t"),
        ]))
        .unwrap();
        assert!(matches!(keys.source, Source::Keys(_)));
        let role = AwsCredentials::from_env(env(&[
            ("AWS_ROLE_ARN", "r"),
            ("AWS_WEB_IDENTITY_TOKEN_FILE", "/t"),
            ("AWS_REGION", "eu-west-2"),
        ]))
        .unwrap();
        assert!(
            matches!(&role.source, Source::WebIdentity { sts_url, .. } if sts_url == "https://sts.eu-west-2.amazonaws.com")
        );
        let pod = AwsCredentials::from_env(env(&[
            (
                "AWS_CONTAINER_CREDENTIALS_FULL_URI",
                "http://169.254.170.23/v1/credentials",
            ),
            ("AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE", "/t"),
        ]))
        .unwrap();
        assert!(matches!(pod.source, Source::PodIdentity { .. }));
        assert!(AwsCredentials::from_env(env(&[])).is_none());
    }

    /// A stand-in for STS or the pod identity agent: answers `body`, counts
    /// the calls, and records the token each arrived with.
    async fn stand_in(path: &'static str, body: String) -> (String, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let kept = seen.clone();
        let app = axum::Router::new().route(
            path,
            axum::routing::any(move |headers: axum::http::HeaderMap, form: String| {
                let kept = kept.clone();
                let body = body.clone();
                async move {
                    let token = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string)
                        .unwrap_or(form);
                    kept.lock().unwrap().push(token);
                    body
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}{path}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, seen)
    }

    fn token_file(contents: &str) -> String {
        let path = std::env::temp_dir().join(format!("outturn-token-{}", uuid::Uuid::now_v7()));
        std::fs::write(&path, contents).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn a_role_is_assumed_with_the_mounted_token_and_held_until_near_expiry() {
        let later = (Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        let answer = format!(
            "<AssumeRoleWithWebIdentityResponse><AssumeRoleWithWebIdentityResult><Credentials>\
             <AccessKeyId>ASIAROLE</AccessKeyId><SecretAccessKey>secret</SecretAccessKey>\
             <SessionToken>session</SessionToken><Expiration>{later}</Expiration>\
             </Credentials></AssumeRoleWithWebIdentityResult></AssumeRoleWithWebIdentityResponse>"
        );
        let (url, seen) = stand_in("/", answer).await;
        let creds = AwsCredentials::new(Source::WebIdentity {
            role_arn: "arn:aws:iam::123456789012:role/gateway".into(),
            token_file: token_file("service-account-token\n"),
            session_name: "outturn-gateway".into(),
            sts_url: url,
        });
        let keys = creds.current().await.unwrap();
        assert_eq!(keys.access_key_id, "ASIAROLE");
        assert_eq!(keys.session_token.as_deref(), Some("session"));
        creds.current().await.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1, "keys good for an hour were asked for again");
        assert!(
            seen[0].contains("WebIdentityToken=service-account-token"),
            "{}",
            seen[0]
        );
        assert!(
            seen[0].contains("Action=AssumeRoleWithWebIdentity"),
            "{}",
            seen[0]
        );
    }

    #[tokio::test]
    async fn the_pods_identity_is_asked_again_when_its_keys_are_about_to_expire() {
        let soon = (Utc::now() + chrono::Duration::minutes(2)).to_rfc3339();
        let answer = serde_json::json!({
            "AccessKeyId": "ASIAPOD", "SecretAccessKey": "secret",
            "Token": "session", "Expiration": soon,
        })
        .to_string();
        let (url, seen) = stand_in("/v1/credentials", answer).await;
        let creds = AwsCredentials::new(Source::PodIdentity {
            url,
            token_file: token_file("pod-token"),
        });
        assert_eq!(creds.current().await.unwrap().access_key_id, "ASIAPOD");
        creds.current().await.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "keys two minutes from expiry were kept");
        assert_eq!(seen[0], "pod-token");
    }

    #[tokio::test]
    async fn a_refusal_from_sts_says_why() {
        let answer = "<ErrorResponse><Error><Code>AccessDenied</Code>\
                      <Message>Not authorized to perform sts:AssumeRoleWithWebIdentity</Message>\
                      </Error></ErrorResponse>";
        let app = axum::Router::new().route(
            "/",
            axum::routing::post(move || async move { (axum::http::StatusCode::FORBIDDEN, answer) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let creds = AwsCredentials::new(Source::WebIdentity {
            role_arn: "r".into(),
            token_file: token_file("t"),
            session_name: "s".into(),
            sts_url: url,
        });
        let err = creds.current().await.err().unwrap().to_string();
        assert!(err.contains("Not authorized"), "{err}");
    }
}
