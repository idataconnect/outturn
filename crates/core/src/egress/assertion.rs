//! The on-behalf-of header: a statement, signed by the gateway, of who asked.
//!
//! Some backends would rather trust the platform than hold a key per person.
//! For a rule that says so, the gateway sends the agent's own credential as
//! ever, and beside it `Outturn-On-Behalf-Of`: a JWT naming the person whose
//! turn it is, for the one host the request is going to, good for a minute.
//! See docs/user-credentials.md, *A signed on-behalf-of header, instead*.
//!
//! Signed by the gateway rather than the API, because only the gateway knows
//! where a request is going and when: an assertion minted with the turn would
//! be good for every host for half an hour. Its public half is published by
//! the API, the tier with a public name, at [`JWKS_PATH`].
//!
//! A JWT rather than a PASETO like the turn token's, because the reader is
//! somebody else's backend and every language has a JWT library. EdDSA only,
//! and a verifier here that takes nothing else, so `alg` is never a choice
//! the token makes.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ed25519_dalek::{Signer as _, SigningKey, Verifier as _, VerifyingKey};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// The header the assertion travels in. Reserved: a guest cannot set it and a
/// rule cannot attach a credential in it, so a backend that sees it knows the
/// gateway put it there.
pub const HEADER: &str = "outturn-on-behalf-of";

/// Where the API publishes the keys an assertion is verified with.
pub const JWKS_PATH: &str = "/.well-known/jwks.json";

/// How long an assertion is good for. Long enough for a request to arrive, and
/// short enough that one a backend logged is worth little to whoever reads the
/// log.
pub const LIFETIME_SECS: u64 = 60;

/// What an assertion says.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Claims {
    /// The deployment, as `OUTTURN_ASSERTION_ISSUER` names it.
    pub iss: String,
    /// The host the request went to: an assertion for one backend is nothing
    /// to another.
    pub aud: String,
    /// The person whose turn it is -- or the machine user an agent named for
    /// turns nobody sent. Never absent: a turn with nobody sends no header.
    pub sub: Uuid,
    pub workspace: Uuid,
    pub iat: u64,
    pub exp: u64,
    pub jti: Uuid,
}

/// RFC 7638's thumbprint of an Ed25519 public key, used as its `kid`, so a
/// key's id is a fact about the key and the API and gateway cannot disagree.
pub fn key_id(public: &[u8; 32]) -> String {
    // The members in lexicographic order with no whitespace, as the RFC
    // requires.
    let canonical = format!(
        r#"{{"crv":"Ed25519","kty":"OKP","x":"{}"}}"#,
        B64.encode(public)
    );
    B64.encode(Sha256::digest(canonical.as_bytes()))
}

/// One public key as a JWK (RFC 8037).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Jwk {
    pub kty: String,
    pub crv: String,
    pub x: String,
    pub kid: String,
    pub alg: String,
    #[serde(rename = "use")]
    pub use_: String,
}

impl Jwk {
    pub fn of(public: &[u8; 32]) -> Self {
        Jwk {
            kty: "OKP".into(),
            crv: "Ed25519".into(),
            x: B64.encode(public),
            kid: key_id(public),
            alg: "EdDSA".into(),
            use_: "sig".into(),
        }
    }
}

/// What [`JWKS_PATH`] serves.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Jwks {
    pub keys: Vec<Jwk>,
}

impl Jwks {
    /// From `OUTTURN_ASSERTION_PUBLIC_KEY`: one or two public keys, 64 hex
    /// characters each, comma separated -- two during a rotation, as
    /// `OUTTURN_TOKEN_PUBLIC_KEY` takes. Unset is none, and the API serves an
    /// empty set rather than a 404, so a backend polling it sees the
    /// deployment rather than a typo.
    ///
    /// None, too, without `OUTTURN_ASSERTION_ISSUER`: the gateway signs
    /// nothing without one, and a published key would tell the page that it
    /// does. Both tiers read the issuer from the same place, so the page's
    /// warning is about what the gateway will actually do.
    pub fn from_env() -> Self {
        let value = std::env::var("OUTTURN_ASSERTION_PUBLIC_KEY").unwrap_or_default();
        if value.trim().is_empty() {
            return Self::default();
        }
        if std::env::var("OUTTURN_ASSERTION_ISSUER")
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            tracing::error!(
                "OUTTURN_ASSERTION_PUBLIC_KEY is set without OUTTURN_ASSERTION_ISSUER, which the \
                 gateway cannot sign without; no assertion key is published"
            );
            return Self::default();
        }
        Self::parse(&value).unwrap_or_else(|e| {
            tracing::error!(error = %e, "OUTTURN_ASSERTION_PUBLIC_KEY could not be read; no assertion key is published");
            Self::default()
        })
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        let keys = value
            .split(',')
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(|k| {
                let bytes: [u8; 32] = hex::decode(k)
                    .ok()
                    .and_then(|b| b.try_into().ok())
                    .ok_or_else(|| "an assertion public key is not 32 bytes of hex".to_string())?;
                VerifyingKey::from_bytes(&bytes)
                    .map_err(|_| "an assertion public key is not an Ed25519 key".to_string())?;
                Ok(Jwk::of(&bytes))
            })
            .collect::<Result<_, String>>()?;
        Ok(Jwks { keys })
    }
}

/// The gateway's signing key, and the issuer it signs as.
pub struct Signer {
    key: SigningKey,
    kid: String,
    issuer: String,
}

impl Signer {
    /// From `OUTTURN_ASSERTION_KEY`, an Ed25519 seed in 64 hex characters, and
    /// `OUTTURN_ASSERTION_ISSUER`, the deployment's name for itself -- its
    /// public URL, conventionally, where [`JWKS_PATH`] is found. Both or
    /// neither: an assertion with no issuer is one a backend cannot tell from
    /// another deployment's. Neither means none, and a rule asking for the
    /// header is refused rather than sent without it.
    pub fn from_env() -> Option<Self> {
        let key = std::env::var("OUTTURN_ASSERTION_KEY").ok()?;
        if key.trim().is_empty() {
            return None;
        }
        let issuer = std::env::var("OUTTURN_ASSERTION_ISSUER").unwrap_or_default();
        match Self::parse(&key, &issuer) {
            Ok(signer) => Some(signer),
            Err(e) => {
                tracing::error!(error = %e, "the assertion key could not be read; no on-behalf-of header can be sent");
                None
            }
        }
    }

    pub fn parse(seed_hex: &str, issuer: &str) -> Result<Self, String> {
        let seed: [u8; 32] = hex::decode(seed_hex.trim())
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| "OUTTURN_ASSERTION_KEY is not 32 bytes of hex".to_string())?;
        let issuer = issuer.trim();
        if issuer.is_empty() {
            return Err("OUTTURN_ASSERTION_ISSUER is not set".into());
        }
        let key = SigningKey::from_bytes(&seed);
        Ok(Signer {
            kid: key_id(&key.verifying_key().to_bytes()),
            key,
            issuer: issuer.to_string(),
        })
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    /// An assertion that `asker` is asking, for a request to `host`, as of
    /// `now` in seconds since the epoch.
    pub fn sign(&self, host: &str, workspace: Uuid, asker: Uuid, now: u64) -> String {
        let header = serde_json::json!({ "alg": "EdDSA", "typ": "JWT", "kid": self.kid });
        let claims = Claims {
            iss: self.issuer.clone(),
            aud: host.to_ascii_lowercase(),
            sub: asker,
            workspace,
            iat: now,
            exp: now + LIFETIME_SECS,
            jti: Uuid::now_v7(),
        };
        let signing_input = format!(
            "{}.{}",
            B64.encode(header.to_string()),
            B64.encode(serde_json::to_vec(&claims).expect("claims serialize"))
        );
        let signature = self.key.sign(signing_input.as_bytes());
        format!("{signing_input}.{}", B64.encode(signature.to_bytes()))
    }
}

/// Checks an assertion as a backend would: an EdDSA signature by one of
/// `keys`, for `audience`, unexpired at `now`. Here for the tests, and as the
/// shortest statement of what a backend has to do.
pub fn verify(token: &str, keys: &Jwks, audience: &str, now: u64) -> Result<Claims, String> {
    let mut parts = token.split('.');
    let (Some(header), Some(claims), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err("not a compact JWS".into());
    };
    let decoded: serde_json::Value = B64
        .decode(header)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or("the header is not JSON")?;
    if decoded["alg"] != "EdDSA" {
        return Err("only EdDSA is accepted".into());
    }
    let jwk = keys
        .keys
        .iter()
        .find(|k| decoded["kid"] == k.kid.as_str())
        .ok_or("signed by a key not published")?;
    let public: [u8; 32] = B64
        .decode(&jwk.x)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or("a published key is not 32 bytes")?;
    let signature: [u8; 64] = B64
        .decode(signature)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or("the signature is not 64 bytes")?;
    VerifyingKey::from_bytes(&public)
        .map_err(|_| "a published key is not an Ed25519 key")?
        .verify(
            format!("{header}.{claims}").as_bytes(),
            &ed25519_dalek::Signature::from_bytes(&signature),
        )
        .map_err(|_| "the signature does not verify")?;
    let claims: Claims = B64
        .decode(claims)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or("the claims are not an assertion's")?;
    if !claims.aud.eq_ignore_ascii_case(audience) {
        return Err("for another host".into());
    }
    if now >= claims.exp {
        return Err("expired".into());
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";

    fn signer() -> Signer {
        Signer::parse(SEED, "https://outturn.example.com").unwrap()
    }

    fn published(s: &Signer) -> Jwks {
        Jwks {
            keys: vec![Jwk::of(&s.public_key())],
        }
    }

    #[test]
    fn an_assertion_names_the_asker_for_one_host_for_a_minute() {
        let s = signer();
        let (ws, dana) = (Uuid::now_v7(), Uuid::now_v7());
        let token = s.sign("Books.Example.com", ws, dana, 1_000);
        let claims = verify(&token, &published(&s), "books.example.com", 1_000).unwrap();
        assert_eq!(claims.sub, dana);
        assert_eq!(claims.workspace, ws);
        assert_eq!(claims.iss, "https://outturn.example.com");
        assert_eq!(claims.exp, 1_000 + LIFETIME_SECS);

        assert!(verify(&token, &published(&s), "other.example.com", 1_000).is_err());
        assert!(verify(&token, &published(&s), "books.example.com", 1_060).is_err());
        let two = s.sign("books.example.com", ws, dana, 1_000);
        assert_ne!(
            verify(&two, &published(&s), "books.example.com", 1_000)
                .unwrap()
                .jti,
            claims.jti
        );
    }

    #[test]
    fn an_assertion_altered_or_signed_by_another_key_does_not_verify() {
        let s = signer();
        let token = s.sign("books.example.com", Uuid::now_v7(), Uuid::now_v7(), 1_000);
        let mut parts: Vec<String> = token.split('.').map(String::from).collect();
        let mut claims: serde_json::Value =
            serde_json::from_slice(&B64.decode(&parts[1]).unwrap()).unwrap();
        claims["sub"] = serde_json::json!(Uuid::now_v7());
        parts[1] = B64.encode(claims.to_string());
        assert!(verify(&parts.join("."), &published(&s), "books.example.com", 1_000).is_err());

        let other = Signer::parse(&"11".repeat(32), "https://outturn.example.com").unwrap();
        assert!(verify(&token, &published(&other), "books.example.com", 1_000).is_err());
    }

    /// RFC 8037's own example key, whose thumbprint appendix A.3 gives.
    #[test]
    fn a_key_id_is_its_rfc_7638_thumbprint() {
        let public = signer().public_key();
        assert_eq!(
            B64.encode(public),
            "11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo"
        );
        assert_eq!(
            key_id(&public),
            "kPrK_qmxVWaYVA9wwBF6Iuo3vVzz7TxHCTwXBygrS4k"
        );
    }

    #[test]
    fn the_published_set_reads_the_public_keys_a_rotation_names() {
        let a = signer().public_key();
        let b = Signer::parse(&"11".repeat(32), "x").unwrap().public_key();
        let jwks = Jwks::parse(&format!("{}, {}", hex::encode(a), hex::encode(b))).unwrap();
        assert_eq!(jwks.keys, vec![Jwk::of(&a), Jwk::of(&b)]);
        assert!(Jwks::parse("abcd").is_err());
        assert!(Signer::parse(SEED, " ").is_err());
    }
}
