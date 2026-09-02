use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ed25519_dalek::SigningKey;
use pasetors::claims::{Claims, ClaimsValidationRules};
use pasetors::keys::{AsymmetricPublicKey, AsymmetricSecretKey};
use pasetors::token::UntrustedToken;
use pasetors::version4::V4;
use pasetors::{Public, public};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::rbac::Role;

/// Service-to-service tokens: short-lived because the runtime refreshes them
/// at half-life, so a leaked one is useful only briefly.
pub const SERVICE_TOKEN_LIFETIME_SECS: u64 = 300;

/// Browser access tokens. Short, because the refresh token silently renews
/// them: this bounds how long a stolen access token is useful, and how long a
/// revoked session or a changed role keeps working.
pub const SESSION_TOKEN_LIFETIME_SECS: u64 = 15 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionClaims {
    pub session_id: Uuid,
    pub tenant_id: Uuid,
    pub roles: Vec<Role>,
}

pub struct TokenMinter {
    secret_key: AsymmetricSecretKey<V4>,
}

impl TokenMinter {
    /// `seed_bytes` is the 32-byte Ed25519 seed. PASETO v4.public expects the
    /// full 64-byte key (seed || public), so the public half is derived here.
    pub fn new(seed_bytes: &[u8; 32]) -> Result<Self, AuthError> {
        let signing_key = SigningKey::from_bytes(seed_bytes);
        let mut key_bytes = [0u8; 64];
        key_bytes[..32].copy_from_slice(seed_bytes);
        key_bytes[32..].copy_from_slice(&signing_key.verifying_key().to_bytes());

        let secret_key = AsymmetricSecretKey::<V4>::from(key_bytes.as_slice())
            .map_err(|e| AuthError::Internal(e.to_string()))?;
        Ok(Self { secret_key })
    }

    pub fn generate() -> Result<(Self, Vec<u8>), AuthError> {
        let mut rng = rand::thread_rng();
        let signing_key = SigningKey::generate(&mut rng);
        let public_bytes = signing_key.verifying_key().to_bytes();
        let minter = Self::new(signing_key.as_bytes())?;
        Ok((minter, public_bytes.to_vec()))
    }

    pub fn from_env() -> Result<(Self, Vec<u8>), AuthError> {
        match std::env::var("OUTTURN_TOKEN_SECRET") {
            Ok(hex) => {
                let bytes = hex_to_bytes(&hex)?;
                let signing_key = SigningKey::from_bytes(&bytes);
                let public_bytes = signing_key.verifying_key().to_bytes();
                let minter = Self::new(&bytes)?;
                Ok((minter, public_bytes.to_vec()))
            }
            Err(_) => Self::generate(),
        }
    }

    /// Mints a service-to-service token.
    pub fn mint(&self, session_id: Uuid, tenant_id: Uuid, roles: &[Role]) -> Result<String, AuthError> {
        self.mint_with_lifetime(
            session_id,
            tenant_id,
            roles,
            Duration::from_secs(SERVICE_TOKEN_LIFETIME_SECS),
        )
    }

    /// Mints a browser session token.
    pub fn mint_session(
        &self,
        session_id: Uuid,
        tenant_id: Uuid,
        roles: &[Role],
    ) -> Result<String, AuthError> {
        self.mint_with_lifetime(
            session_id,
            tenant_id,
            roles,
            Duration::from_secs(SESSION_TOKEN_LIFETIME_SECS),
        )
    }

    pub fn mint_with_lifetime(
        &self,
        session_id: Uuid,
        tenant_id: Uuid,
        roles: &[Role],
        lifetime: Duration,
    ) -> Result<String, AuthError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| AuthError::Internal(e.to_string()))?;

        let exp = UNIX_EPOCH + now + lifetime;
        let iat = UNIX_EPOCH + now;

        let mut claims = Claims::new()
            .map_err(|e| AuthError::Internal(e.to_string()))?;

        claims
            .expiration(&format_rfc3339(exp))
            .map_err(|e| AuthError::Internal(e.to_string()))?;
        claims
            .issued_at(&format_rfc3339(iat))
            .map_err(|e| AuthError::Internal(e.to_string()))?;
        claims
            .add_additional("sid", session_id.to_string())
            .map_err(|e| AuthError::Internal(e.to_string()))?;
        claims
            .add_additional("tid", tenant_id.to_string())
            .map_err(|e| AuthError::Internal(e.to_string()))?;

        let scp: Vec<String> = roles.iter().map(|r| r.to_string()).collect();
        let scp_json = serde_json::to_value(&scp)
            .map_err(|e| AuthError::Internal(e.to_string()))?;
        claims
            .add_additional("scp", scp_json)
            .map_err(|e| AuthError::Internal(e.to_string()))?;

        public::sign(&self.secret_key, &claims, None, None)
            .map_err(|e| AuthError::Internal(e.to_string()))
    }
}

pub struct TokenValidator {
    public_key: AsymmetricPublicKey<V4>,
}

impl TokenValidator {
    pub fn new(public_bytes: &[u8; 32]) -> Result<Self, AuthError> {
        let public_key = AsymmetricPublicKey::<V4>::from(public_bytes.as_slice())
            .map_err(|e| AuthError::Internal(e.to_string()))?;
        Ok(Self { public_key })
    }

    pub fn from_env() -> Result<Self, AuthError> {
        let hex = std::env::var("OUTTURN_TOKEN_PUBLIC_KEY")
            .map_err(|_| AuthError::Internal("OUTTURN_TOKEN_PUBLIC_KEY not set".into()))?;
        let bytes = hex_to_bytes(&hex)?;
        Self::new(&bytes)
    }

    pub fn validate(&self, token: &str) -> Result<SessionClaims, AuthError> {
        let rules = ClaimsValidationRules::new();

        let untrusted = UntrustedToken::<Public, V4>::try_from(token)
            .map_err(|_| AuthError::Invalid)?;

        let trusted = public::verify(&self.public_key, &untrusted, &rules, None, None)
            .map_err(|e| {
                let msg = e.to_string();
                if msg.contains("expired") {
                    AuthError::Expired
                } else {
                    AuthError::Invalid
                }
            })?;

        let parsed: serde_json::Value = serde_json::from_str(trusted.payload())
            .map_err(|e| AuthError::Internal(e.to_string()))?;

        let session_id = parsed["sid"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or(AuthError::Invalid)?;

        let tenant_id = parsed["tid"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or(AuthError::Invalid)?;

        let roles: Vec<Role> = parsed["scp"]
            .as_array()
            .ok_or(AuthError::Invalid)?
            .iter()
            .filter_map(|v| v.as_str()?.parse().ok())
            .collect();

        if roles.is_empty() {
            return Err(AuthError::Invalid);
        }

        Ok(SessionClaims {
            session_id,
            tenant_id,
            roles,
        })
    }
}

fn format_rfc3339(time: SystemTime) -> String {
    let duration = time.duration_since(UNIX_EPOCH).unwrap();
    let secs = duration.as_secs();
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Days since unix epoch to y/m/d (civil_from_days algorithm)
    let z = days as i64 + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hours:02}:{minutes:02}:{seconds:02}+00:00")
}

fn hex_to_bytes(hex: &str) -> Result<[u8; 32], AuthError> {
    let hex = hex.trim();
    if hex.len() != 64 {
        return Err(AuthError::Internal(format!(
            "expected 64 hex chars, got {}",
            hex.len()
        )));
    }
    let mut bytes = [0u8; 32];
    for i in 0..32 {
        bytes[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|e| AuthError::Internal(e.to_string()))?;
    }
    Ok(bytes)
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("missing authorization header")]
    Missing,
    #[error("invalid token")]
    Invalid,
    #[error("token expired")]
    Expired,
    #[error("insufficient permissions")]
    Forbidden,
    #[error("internal auth error: {0}")]
    Internal(String),
}

impl SessionClaims {
    pub fn has_authority(&self, authority: super::rbac::Authority) -> bool {
        super::rbac::resolve_authorities(&self.roles).contains(&authority)
    }

    pub fn require(&self, authority: super::rbac::Authority) -> Result<(), AuthError> {
        if self.has_authority(authority) {
            Ok(())
        } else {
            Err(AuthError::Forbidden)
        }
    }
}

pub fn extract_bearer(headers: &axum::http::HeaderMap) -> Result<&str, AuthError> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(AuthError::Missing)
}
