//! Signed tokens, and what each kind is allowed to mean.
//!
//! Two audiences, because the tiers that verify tokens trust different
//! things. A browser token is a person acting inside the API; a turn token is
//! the platform letting one turn reach the gateway on a tenant's behalf. Both
//! used to verify under the same rules and differed only in their roles, which
//! made every turn token a working API credential for its tenant -- and every
//! Operator's cookie a working gateway credential. The audience claim is what
//! separates them now, and each validator insists on its own.
//!
//! The runtime tier holds no signing key. It presents a shared key that only
//! ever means "I am the tier that runs turns", checked in constant time, so a
//! compromised runtime pod -- the tier that executes tenant code -- cannot
//! mint anything for anyone. See `RuntimeKey`.

use std::time::Duration;

use ed25519_dalek::SigningKey;
use pasetors::claims::{Claims, ClaimsValidationRules};
use pasetors::errors::{ClaimValidationError, Error as PasetoError};
use pasetors::footer::Footer;
use pasetors::keys::{AsymmetricPublicKey, AsymmetricSecretKey};
use pasetors::paserk::{FormatAsPaserk, Id};
use pasetors::token::UntrustedToken;
use pasetors::version4::V4;
use pasetors::{Public, public};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::rbac::Role;

/// Tokens the API accepts: people signed in through a browser.
pub const AUDIENCE_API: &str = "outturn:api";

/// Tokens the gateway accepts: one turn, minted by the API for the runtime
/// running it, able to call a model and nothing else.
pub const AUDIENCE_GATEWAY: &str = "outturn:gateway";

/// Per-turn gateway tokens: short-lived because one is minted for every turn,
/// so a leaked one is useful only briefly.
pub const SERVICE_TOKEN_LIFETIME_SECS: u64 = 300;

/// Browser access tokens. Short, because the refresh token silently renews
/// them: this bounds how long a stolen access token is useful, and how long a
/// revoked session or a changed role keeps working.
pub const SESSION_TOKEN_LIFETIME_SECS: u64 = 15 * 60;

/// What a verified token says about its bearer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionClaims {
    /// Who or what this token is for. In an API token it is the user's
    /// account id; in a gateway token it is the chat session the turn
    /// belongs to. The audience says which, so a validator never has to
    /// guess -- and a token of one kind is never read as the other.
    pub subject: Uuid,
    pub tenant_id: Uuid,
    pub roles: Vec<Role>,
}

/// The claim names as they appear in the payload.
const SUBJECT: &str = "sub";
const TENANT: &str = "tid";
const SCOPE: &str = "scp";
/// Footer claim naming which public key signed the token, so verifiers can
/// hold more than one during a rotation.
const KEY_ID: &str = "kid";

pub struct TokenMinter {
    secret_key: AsymmetricSecretKey<V4>,
    kid: String,
}

/// The identifier a public key is known by: its PASERK id, which is the
/// standard's own way of naming a key in a footer.
fn key_id(public_key: &AsymmetricPublicKey<V4>) -> String {
    let mut id = String::new();
    Id::from(public_key)
        .fmt(&mut id)
        .expect("formatting into a String cannot fail");
    id
}

impl TokenMinter {
    /// `seed_bytes` is the 32-byte Ed25519 seed. PASETO v4.public expects the
    /// full 64-byte key (seed || public), so the public half is derived here.
    pub fn new(seed_bytes: &[u8; 32]) -> Result<Self, AuthError> {
        let signing_key = SigningKey::from_bytes(seed_bytes);
        let public_bytes = signing_key.verifying_key().to_bytes();
        let mut key_bytes = [0u8; 64];
        key_bytes[..32].copy_from_slice(seed_bytes);
        key_bytes[32..].copy_from_slice(&public_bytes);

        let secret_key = AsymmetricSecretKey::<V4>::from(key_bytes.as_slice())
            .map_err(|e| AuthError::Internal(e.to_string()))?;
        let public_key = AsymmetricPublicKey::<V4>::from(public_bytes.as_slice())
            .map_err(|e| AuthError::Internal(e.to_string()))?;
        Ok(Self {
            secret_key,
            kid: key_id(&public_key),
        })
    }

    /// The public half of `seed`, for a validator to be built from.
    pub fn public_key_of(seed_bytes: &[u8; 32]) -> [u8; 32] {
        SigningKey::from_bytes(seed_bytes).verifying_key().to_bytes()
    }

    /// Reads `OUTTURN_TOKEN_SECRET`, and refuses to start without it.
    ///
    /// An earlier version generated a key when the variable was missing. Every
    /// token it minted was then rejected by every other tier, which in the API
    /// looked like a fault and in the runtime looked like a healthy pod that
    /// took no work. A misconfiguration should say so at startup.
    pub fn from_env() -> Result<Self, AuthError> {
        let hex = std::env::var("OUTTURN_TOKEN_SECRET")
            .map_err(|_| AuthError::Internal("OUTTURN_TOKEN_SECRET not set".into()))?;
        Self::new(&hex_to_bytes(&hex)?)
    }

    /// Mints a browser session token for a signed-in user.
    pub fn mint_session(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
        roles: &[Role],
    ) -> Result<String, AuthError> {
        self.mint_with_lifetime(
            AUDIENCE_API,
            user_id,
            tenant_id,
            roles,
            Duration::from_secs(SESSION_TOKEN_LIFETIME_SECS),
        )
    }

    /// Mints the token one turn carries to the gateway.
    ///
    /// Carries `Role::Turn`, which holds `GatewayInvoke` and nothing else, and
    /// the gateway audience, so even if it leaked to something that could
    /// reach the API it would be refused there.
    pub fn mint_turn(&self, chat_session_id: Uuid, tenant_id: Uuid) -> Result<String, AuthError> {
        self.mint_with_lifetime(
            AUDIENCE_GATEWAY,
            chat_session_id,
            tenant_id,
            &[Role::Turn],
            Duration::from_secs(SERVICE_TOKEN_LIFETIME_SECS),
        )
    }

    pub fn mint_with_lifetime(
        &self,
        audience: &str,
        subject: Uuid,
        tenant_id: Uuid,
        roles: &[Role],
        lifetime: Duration,
    ) -> Result<String, AuthError> {
        use chrono::SecondsFormat;

        let internal = |e: PasetoError| AuthError::Internal(e.to_string());
        let now = chrono::Utc::now();
        let exp = now
            + chrono::Duration::from_std(lifetime)
                .map_err(|e| AuthError::Internal(e.to_string()))?;

        let mut claims = Claims::new().map_err(internal)?;
        claims
            .expiration(&exp.to_rfc3339_opts(SecondsFormat::Secs, false))
            .map_err(internal)?;
        claims
            .issued_at(&now.to_rfc3339_opts(SecondsFormat::Secs, false))
            .map_err(internal)?;
        claims
            .not_before(&now.to_rfc3339_opts(SecondsFormat::Secs, false))
            .map_err(internal)?;
        claims.audience(audience).map_err(internal)?;
        claims.subject(&subject.to_string()).map_err(internal)?;
        claims
            .add_additional(TENANT, tenant_id.to_string())
            .map_err(internal)?;

        let scp: Vec<String> = roles.iter().map(|r| r.to_string()).collect();
        let scp_json = serde_json::to_value(&scp).map_err(|e| AuthError::Internal(e.to_string()))?;
        claims.add_additional(SCOPE, scp_json).map_err(internal)?;

        // `kid` is a registered footer claim, so it goes in through the
        // paserk path rather than as an arbitrary key.
        let mut footer = Footer::new();
        footer.parse_string(&format!("{{\"{KEY_ID}\":{}}}", serde_json::json!(self.kid))).map_err(internal)?;

        public::sign(&self.secret_key, &claims, Some(&footer), None).map_err(internal)
    }
}

pub struct TokenValidator {
    /// Every key currently trusted, by id. More than one during a rotation:
    /// the new key is added to every verifier first, the minter switches to
    /// it, and the old key is removed once nothing it signed is still alive.
    keys: Vec<(String, AsymmetricPublicKey<V4>)>,
    audience: &'static str,
}

impl TokenValidator {
    pub fn new(public_bytes: &[u8; 32], audience: &'static str) -> Result<Self, AuthError> {
        Self::with_keys(&[*public_bytes], audience)
    }

    pub fn with_keys(keys: &[[u8; 32]], audience: &'static str) -> Result<Self, AuthError> {
        if keys.is_empty() {
            return Err(AuthError::Internal("no public keys to verify with".into()));
        }
        let keys = keys
            .iter()
            .map(|bytes| {
                AsymmetricPublicKey::<V4>::from(bytes.as_slice())
                    .map(|key| (key_id(&key), key))
                    .map_err(|e| AuthError::Internal(e.to_string()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { keys, audience })
    }

    /// Reads `OUTTURN_TOKEN_PUBLIC_KEY`: one hex key, or several separated by
    /// commas while a rotation is in progress.
    pub fn from_env(audience: &'static str) -> Result<Self, AuthError> {
        let hex = std::env::var("OUTTURN_TOKEN_PUBLIC_KEY")
            .map_err(|_| AuthError::Internal("OUTTURN_TOKEN_PUBLIC_KEY not set".into()))?;
        let keys = hex
            .split(',')
            .filter(|k| !k.trim().is_empty())
            .map(hex_to_bytes)
            .collect::<Result<Vec<_>, _>>()?;
        Self::with_keys(&keys, audience)
    }

    pub fn validate(&self, token: &str) -> Result<SessionClaims, AuthError> {
        let untrusted = UntrustedToken::<Public, V4>::try_from(token).map_err(|_| AuthError::Invalid)?;

        // The footer is unauthenticated until the signature is checked, so it
        // only chooses which key to try; it cannot make a bad token good.
        let mut footer = Footer::new();
        footer
            .parse_bytes(untrusted.untrusted_footer())
            .map_err(|_| AuthError::Invalid)?;
        let kid = footer
            .get_claim(KEY_ID)
            .and_then(|v| v.as_str())
            .ok_or(AuthError::Invalid)?;
        let (_, public_key) = self
            .keys
            .iter()
            .find(|(id, _)| id == kid)
            .ok_or(AuthError::Invalid)?;

        let mut rules = ClaimsValidationRules::new();
        rules.validate_audience_with(self.audience);

        let trusted =
            public::verify(public_key, &untrusted, &rules, Some(&footer), None).map_err(|e| match e {
                PasetoError::ClaimValidation(ClaimValidationError::Exp) => AuthError::Expired,
                _ => AuthError::Invalid,
            })?;

        let parsed: serde_json::Value =
            serde_json::from_str(trusted.payload()).map_err(|_| AuthError::Invalid)?;

        let subject = parsed[SUBJECT]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or(AuthError::Invalid)?;

        let tenant_id = parsed[TENANT]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or(AuthError::Invalid)?;

        // A role this build does not know is ignored rather than fatal, so a
        // newer API can mint tokens an older gateway still accepts for the
        // roles it does understand. A token with no recognisable role at all
        // grants nothing and is refused as such.
        let roles: Vec<Role> = parsed[SCOPE]
            .as_array()
            .ok_or(AuthError::Invalid)?
            .iter()
            .filter_map(|v| v.as_str()?.parse().ok())
            .collect();

        if roles.is_empty() {
            return Err(AuthError::Invalid);
        }

        Ok(SessionClaims {
            subject,
            tenant_id,
            roles,
        })
    }
}

/// The credential the runtime tier presents to the API.
///
/// A shared key rather than a signed token, on purpose. The runtime is the
/// tier that executes tenant-supplied components, so it must hold nothing
/// that could mint a credential for anyone else -- and a key that means only
/// "the runtime tier" cannot. It is compared in constant time and checked
/// only on the bearer path, never from a cookie.
pub struct RuntimeKey(Vec<u8>);

/// Shorter than this is a password, not a key.
const RUNTIME_KEY_MIN_BYTES: usize = 32;

impl RuntimeKey {
    pub fn new(key: &str) -> Result<Self, AuthError> {
        if key.len() < RUNTIME_KEY_MIN_BYTES {
            return Err(AuthError::Internal(format!(
                "the runtime key must be at least {RUNTIME_KEY_MIN_BYTES} bytes"
            )));
        }
        Ok(Self(key.as_bytes().to_vec()))
    }

    pub fn from_env() -> Result<Self, AuthError> {
        let key = std::env::var("OUTTURN_RUNTIME_KEY")
            .map_err(|_| AuthError::Internal("OUTTURN_RUNTIME_KEY not set".into()))?;
        Self::new(key.trim())
    }

    /// Whether `presented` is this key, without leaking how much of it was.
    pub fn accepts(&self, presented: &str) -> bool {
        use subtle::ConstantTimeEq;
        let presented = presented.as_bytes();
        if presented.len() != self.0.len() {
            return false;
        }
        self.0.ct_eq(presented).into()
    }

    /// What the runtime tier is, once its key has been accepted.
    pub fn claims() -> SessionClaims {
        SessionClaims {
            subject: Uuid::nil(),
            tenant_id: Uuid::nil(),
            roles: vec![Role::Runtime],
        }
    }
}

fn hex_to_bytes(hex: &str) -> Result<[u8; 32], AuthError> {
    let hex = hex.trim();
    let bytes = hex::decode(hex).map_err(|e| AuthError::Internal(format!("key is not hex: {e}")))?;
    bytes.try_into().map_err(|_| {
        AuthError::Internal(format!(
            "expected a 32-byte key as 64 hex chars, got {} chars",
            hex.len()
        ))
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (TokenMinter, [u8; 32]) {
        let seed = [7u8; 32];
        (TokenMinter::new(&seed).expect("minter"), TokenMinter::public_key_of(&seed))
    }

    #[test]
    fn a_token_is_only_good_for_its_own_audience() {
        let (minter, public) = pair();
        let api = TokenValidator::new(&public, AUDIENCE_API).expect("validator");
        let gateway = TokenValidator::new(&public, AUDIENCE_GATEWAY).expect("validator");

        let session = minter
            .mint_session(Uuid::now_v7(), Uuid::now_v7(), &[Role::Operator])
            .expect("mint");
        assert!(api.validate(&session).is_ok());
        assert!(
            gateway.validate(&session).is_err(),
            "a browser token was accepted by the gateway"
        );

        let turn = minter.mint_turn(Uuid::now_v7(), Uuid::now_v7()).expect("mint");
        assert!(gateway.validate(&turn).is_ok());
        assert!(
            api.validate(&turn).is_err(),
            "a turn token was accepted by the API"
        );
    }

    #[test]
    fn a_turn_token_can_only_call_the_gateway() {
        let (minter, public) = pair();
        let gateway = TokenValidator::new(&public, AUDIENCE_GATEWAY).expect("validator");
        let claims = gateway
            .validate(&minter.mint_turn(Uuid::now_v7(), Uuid::now_v7()).expect("mint"))
            .expect("valid");
        assert!(claims.has_authority(super::super::rbac::Authority::GatewayInvoke));
        assert!(!claims.has_authority(super::super::rbac::Authority::SessionsRead));
        assert!(!claims.has_authority(super::super::rbac::Authority::WorkTake));
    }

    #[test]
    fn a_key_the_verifier_does_not_hold_is_refused_and_a_rotated_one_is_not() {
        let (old, old_public) = pair();
        let new_seed = [9u8; 32];
        let new = TokenMinter::new(&new_seed).expect("minter");
        let new_public = TokenMinter::public_key_of(&new_seed);

        let only_old = TokenValidator::new(&old_public, AUDIENCE_API).expect("validator");
        let both = TokenValidator::with_keys(&[old_public, new_public], AUDIENCE_API).expect("validator");

        let signed_new = new
            .mint_session(Uuid::now_v7(), Uuid::now_v7(), &[Role::Viewer])
            .expect("mint");
        let signed_old = old
            .mint_session(Uuid::now_v7(), Uuid::now_v7(), &[Role::Viewer])
            .expect("mint");

        assert!(only_old.validate(&signed_new).is_err());
        assert!(both.validate(&signed_new).is_ok());
        assert!(both.validate(&signed_old).is_ok());
    }

    #[test]
    fn expiry_is_reported_as_such() {
        let (minter, public) = pair();
        let api = TokenValidator::new(&public, AUDIENCE_API).expect("validator");
        let token = minter
            .mint_with_lifetime(
                AUDIENCE_API,
                Uuid::now_v7(),
                Uuid::now_v7(),
                &[Role::Viewer],
                Duration::from_secs(0),
            )
            .expect("mint");
        std::thread::sleep(Duration::from_millis(1100));
        assert!(matches!(api.validate(&token), Err(AuthError::Expired)));
    }

    #[test]
    fn the_runtime_key_matches_only_itself() {
        let key = RuntimeKey::new("0123456789abcdef0123456789abcdef").expect("key");
        assert!(key.accepts("0123456789abcdef0123456789abcdef"));
        assert!(!key.accepts("0123456789abcdef0123456789abcdeg"));
        assert!(!key.accepts("0123456789abcdef0123456789abcde"));
        assert!(RuntimeKey::new("short").is_err(), "a short key was accepted");
    }
}
