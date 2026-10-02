//! Signed tokens, and what each kind is allowed to mean.
//!
//! Two audiences, because the tiers that verify tokens trust different
//! things. A browser token is a person acting inside the API; a turn token is
//! the platform letting one turn reach the gateway on a workspace's behalf. Both
//! used to verify under the same rules and differed only in their roles, which
//! made every turn token a working API credential for its workspace -- and every
//! Operator's cookie a working gateway credential. The audience claim is what
//! separates them now, and each validator insists on its own.
//!
//! The runtime tier holds no signing key. It presents a shared key that only
//! ever means "I am the tier that runs turns", checked in constant time, so a
//! compromised runtime pod -- the tier that executes workspace code -- cannot
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

use crate::egress::commit;

use super::rbac::Role;

/// Tokens the API accepts: people signed in through a browser.
pub const AUDIENCE_API: &str = "outturn:api";

/// Tokens the gateway accepts: one turn, minted by the API for the runtime
/// running it, able to call a model and nothing else.
pub const AUDIENCE_GATEWAY: &str = "outturn:gateway";

/// Per-turn gateway tokens: short-lived because one is minted for every turn,
/// so a leaked one is useful only briefly.
///
/// Half an hour rather than the five minutes this once was, and not the bound
/// on a turn: a turn has no upper bound while it is working, and one running
/// for hours is a use this platform is for. A runtime asks for a fresh token
/// once less than `TURN_TOKEN_REFRESH_BELOW_SECS` remains -- see
/// `api::work::refresh_token`. Five minutes failed an eight-minute turn on its
/// next `fetch_url` with "token expired".
pub const SERVICE_TOKEN_LIFETIME_SECS: u64 = 1800;

/// Long enough for one fetch of one document, which the gateway gives up on
/// well inside it.
const DOCUMENT_FETCH_TOKEN_LIFETIME_SECS: u64 = 60;

/// How much life a turn token may have left before the runtime replaces it.
///
/// Half the lifetime, so the gap it covers -- the longest stretch a turn can
/// go without calling the gateway and still find its token good -- is fifteen
/// minutes. The check runs before every gateway call rather than on a timer,
/// which also catches a single long model call followed by a tool call in the
/// same round.
pub const TURN_TOKEN_REFRESH_BELOW_SECS: u64 = 900;

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
    pub workspace_id: Uuid,
    /// Role names. Platform roles resolve in code (`rbac::platform_authorities`);
    /// workspace roles mean whatever the workspace's role rows say, which the API
    /// resolves on every request. Never authorities: a role may bundle
    /// hundreds, and a token that carried them would grow with every one.
    pub roles: Vec<String>,
    /// What the API committed to over this turn's egress rules, for the tokens
    /// that authorise a turn. `None` on a browser session token, which never
    /// carries a turn's rules and has no business making a statement about
    /// them -- a claim meaning "allows nothing" on a token that could never
    /// prove a rule anyway is a claim waiting to be read as an answer.
    ///
    /// `None` here means the claim was absent, never that the workspace allows
    /// nothing: those are different answers and the empty set has a commitment
    /// of its own to say so. Whoever needs one asks with `egress_commitment()`,
    /// which refuses rather than defaults.
    pub egress_commitment: Option<commit::Hash>,
    /// What the API committed to over this turn's *gates* -- the requests it may
    /// not make without somebody's word (`egress::gate`).
    ///
    /// A second claim rather than folded into the one above, because the two
    /// answer opposite questions and a stripped claim must fail in opposite
    /// directions. An egress rule is a permission: absence means refused, and
    /// one commitment covering both would let a stripped gate set read as
    /// "nothing is gated", which is exactly the answer a forger would choose.
    ///
    /// `None` means the claim was absent, which the gateway refuses. "Nothing
    /// gates this turn" is `Gates::none()`, which has a root of its own and is
    /// signed.
    pub gate_commitment: Option<commit::Hash>,
}

impl SessionClaims {
    /// The commitment this token vouches for, for a caller about to check a
    /// rule against it.
    ///
    /// Refuses when the claim is absent rather than standing in the empty
    /// commitment. A token predating this claim, or one a forger stripped it
    /// from, must not be read as "this workspace allows nothing" -- that is
    /// precisely the answer that makes a stripped token useful, and the tier
    /// asking this question is the one deciding whether a request goes out.
    ///
    /// Nothing calls this yet. Today the runtime receives the commitment with
    /// the turn and enforces there, so the claim rides along unread; it is
    /// what a tier that only ever sees the token -- the gateway, if the
    /// outbound call moves behind it to attach a credential the runtime must
    /// not hold -- would ask. Minting it now costs a hash and means such a
    /// tier can be added without every turn token in flight predating it.
    pub fn egress_commitment(&self) -> Result<commit::Hash, AuthError> {
        self.egress_commitment.ok_or(AuthError::Invalid)
    }

    /// The gate commitment, for the tier deciding whether a request may go out
    /// without an approval.
    ///
    /// Refuses when absent, as its neighbour does, and the direction matters more
    /// here: a missing egress commitment means no request can prove a rule and
    /// everything is refused, which is safe. A missing gate commitment, defaulted
    /// to the empty set, would mean *nothing* is gated and every request goes
    /// out -- so absence has to be a refusal rather than a default.
    pub fn gate_commitment(&self) -> Result<commit::Hash, AuthError> {
        self.gate_commitment.ok_or(AuthError::Invalid)
    }
}

/// The claim names as they appear in the payload.
const SUBJECT: &str = "sub";
const WORKSPACE: &str = "wid";
const SCOPE: &str = "scp";
const EGRESS: &str = "egr";
const GATES: &str = "gat";
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
        SigningKey::from_bytes(seed_bytes)
            .verifying_key()
            .to_bytes()
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
    ///
    /// Carries no egress commitment. A person acting in the API never holds a
    /// turn's rules, so this token has nothing to vouch for -- and the empty
    /// commitment would be the wrong thing to put here, being an answer
    /// ("allows nothing") rather than the absence of one. Only `mint_turn`
    /// makes that statement, because only a turn can be asked to prove a rule.
    pub fn mint_session(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
        roles: &[String],
    ) -> Result<String, AuthError> {
        self.mint_with_lifetime(
            AUDIENCE_API,
            user_id,
            workspace_id,
            roles,
            None,
            None,
            Duration::from_secs(SESSION_TOKEN_LIFETIME_SECS),
        )
    }

    /// Mints the token one turn carries to the gateway.
    ///
    /// Carries `Role::Turn`, which holds `GatewayInvoke` and nothing else, and
    /// the gateway audience, so even if it leaked to something that could
    /// reach the API it would be refused there. `egress_commitment` is what
    /// this tier computed over the rules handed to the same turn -- minted
    /// alongside them rather than trusted from whoever asks, since the runtime
    /// that will eventually present a rule for enforcement is the tier running
    /// workspace code and cannot be the one vouching for what it was allowed.
    pub fn mint_turn(
        &self,
        chat_session_id: Uuid,
        workspace_id: Uuid,
        egress_commitment: commit::Hash,
        gate_commitment: commit::Hash,
    ) -> Result<String, AuthError> {
        self.mint_with_lifetime(
            AUDIENCE_GATEWAY,
            chat_session_id,
            workspace_id,
            &[Role::Turn.to_string()],
            Some(egress_commitment),
            Some(gate_commitment),
            Duration::from_secs(SERVICE_TOKEN_LIFETIME_SECS),
        )
    }

    /// Mints the token the API fetches one document with, through the
    /// gateway's egress path.
    ///
    /// The gateway audience, `Role::DocumentFetch` and nothing else, and an
    /// egress commitment the caller computed over the single host it means to
    /// reach -- so the token is good for that host, by GET, for a minute, and
    /// cannot call a model. The subject is the operator who asked.
    pub fn mint_document_fetch(
        &self,
        user_id: Uuid,
        workspace_id: Uuid,
        egress_commitment: commit::Hash,
        gate_commitment: commit::Hash,
    ) -> Result<String, AuthError> {
        self.mint_with_lifetime(
            AUDIENCE_GATEWAY,
            user_id,
            workspace_id,
            &[Role::DocumentFetch.to_string()],
            Some(egress_commitment),
            Some(gate_commitment),
            Duration::from_secs(DOCUMENT_FETCH_TOKEN_LIFETIME_SECS),
        )
    }

    pub fn mint_with_lifetime(
        &self,
        audience: &str,
        subject: Uuid,
        workspace_id: Uuid,
        roles: &[String],
        egress_commitment: Option<commit::Hash>,
        gate_commitment: Option<commit::Hash>,
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
            .add_additional(WORKSPACE, workspace_id.to_string())
            .map_err(internal)?;

        let scp_json =
            serde_json::to_value(roles).map_err(|e| AuthError::Internal(e.to_string()))?;
        claims.add_additional(SCOPE, scp_json).map_err(internal)?;

        // Added only where it means something. A token that cannot authorise a
        // turn does not get to carry a statement about what a turn may reach.
        if let Some(committed) = egress_commitment {
            let egr_json =
                serde_json::to_value(committed).map_err(|e| AuthError::Internal(e.to_string()))?;
            claims.add_additional(EGRESS, egr_json).map_err(internal)?;
        }
        if let Some(committed) = gate_commitment {
            let gat_json =
                serde_json::to_value(committed).map_err(|e| AuthError::Internal(e.to_string()))?;
            claims.add_additional(GATES, gat_json).map_err(internal)?;
        }

        // `kid` is a registered footer claim, so it goes in through the
        // paserk path rather than as an arbitrary key.
        let mut footer = Footer::new();
        footer
            .parse_string(&format!("{{\"{KEY_ID}\":{}}}", serde_json::json!(self.kid)))
            .map_err(internal)?;

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
    /// The same trusted keys, checking a different audience.
    ///
    /// For the API reading a turn token back when it reissues one: it trusts
    /// the same keys as the gateway, and needs the gateway's audience check so
    /// a browser token cannot be traded in for a turn token.
    pub fn for_audience(&self, audience: &'static str) -> Self {
        Self {
            keys: self.keys.clone(),
            audience,
        }
    }

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
        let untrusted =
            UntrustedToken::<Public, V4>::try_from(token).map_err(|_| AuthError::Invalid)?;

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

        let trusted = public::verify(public_key, &untrusted, &rules, Some(&footer), None).map_err(
            |e| match e {
                PasetoError::ClaimValidation(ClaimValidationError::Exp) => AuthError::Expired,
                _ => AuthError::Invalid,
            },
        )?;

        let parsed: serde_json::Value =
            serde_json::from_str(trusted.payload()).map_err(|_| AuthError::Invalid)?;

        let subject = parsed[SUBJECT]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or(AuthError::Invalid)?;

        let workspace_id = parsed[WORKSPACE]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or(AuthError::Invalid)?;

        // Names, not meanings: a workspace role means whatever the workspace's
        // rows say at the moment of the request, so nothing about it can be
        // settled here. A token naming no role at all grants nothing and is
        // refused as such.
        let roles: Vec<String> = parsed[SCOPE]
            .as_array()
            .ok_or(AuthError::Invalid)?
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .filter(|r| !r.is_empty())
            .collect();

        if roles.is_empty() {
            return Err(AuthError::Invalid);
        }

        // Absent is carried as absent rather than resolved here: a session
        // token legitimately has no commitment, and only the caller knows
        // whether it needed one. What must never happen is absence becoming
        // the empty commitment, which reads as "this workspace allows
        // nothing" -- so a malformed claim is refused outright, and a missing
        // one stays None for `egress_commitment()` to refuse.
        let egress_commitment: Option<commit::Hash> = match parsed.get(EGRESS) {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => {
                Some(serde_json::from_value(value.clone()).map_err(|_| AuthError::Invalid)?)
            }
        };
        // The same reading, and the same refusal of a malformed claim. Absence
        // stays None for `gate_commitment()` to refuse: defaulted to the empty
        // set it would mean nothing is gated.
        let gate_commitment: Option<commit::Hash> = match parsed.get(GATES) {
            None | Some(serde_json::Value::Null) => None,
            Some(value) => {
                Some(serde_json::from_value(value.clone()).map_err(|_| AuthError::Invalid)?)
            }
        };

        Ok(SessionClaims {
            subject,
            workspace_id,
            roles,
            egress_commitment,
            gate_commitment,
        })
    }
}

/// The credential the runtime tier presents to the API.
///
/// A shared key rather than a signed token, on purpose. The runtime is the
/// tier that executes workspace-supplied components, so it must hold nothing
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
    ///
    /// Not a turn's claims and not signed for any workspace, so it commits to
    /// no rule set at all. A turn's commitment travels on the turn token the
    /// runtime presents alongside its key, never on its own identity: this
    /// says which tier is calling, and nothing about what any turn may reach.
    pub fn claims() -> SessionClaims {
        SessionClaims {
            subject: Uuid::nil(),
            workspace_id: Uuid::nil(),
            roles: vec![Role::Runtime.to_string()],
            egress_commitment: None,
            gate_commitment: None,
        }
    }
}

fn hex_to_bytes(hex: &str) -> Result<[u8; 32], AuthError> {
    let hex = hex.trim();
    let bytes =
        hex::decode(hex).map_err(|e| AuthError::Internal(format!("key is not hex: {e}")))?;
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
    /// Whether a platform role in this token grants `authority`.
    ///
    /// Only platform roles are consulted: this is what a tier without a
    /// database can decide on its own, and it is all the gateway needs. The
    /// API resolves workspace roles as well, through its role store.
    pub fn has_platform_authority(&self, authority: super::rbac::Authority) -> bool {
        super::rbac::platform_authorities(&self.roles).contains(&authority)
    }

    pub fn require_platform(&self, authority: super::rbac::Authority) -> Result<(), AuthError> {
        if self.has_platform_authority(authority) {
            Ok(())
        } else {
            Err(AuthError::Forbidden)
        }
    }

    pub fn is_system_admin(&self) -> bool {
        self.roles.iter().any(|r| r == "system_admin")
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
        (
            TokenMinter::new(&seed).expect("minter"),
            TokenMinter::public_key_of(&seed),
        )
    }

    #[test]
    fn a_token_is_only_good_for_its_own_audience() {
        let (minter, public) = pair();
        let api = TokenValidator::new(&public, AUDIENCE_API).expect("validator");
        let gateway = TokenValidator::new(&public, AUDIENCE_GATEWAY).expect("validator");

        let session = minter
            .mint_session(Uuid::now_v7(), Uuid::now_v7(), &["operator".to_string()])
            .expect("mint");
        assert!(api.validate(&session).is_ok());
        assert!(
            gateway.validate(&session).is_err(),
            "a browser token was accepted by the gateway"
        );

        let turn = minter
            .mint_turn(
                Uuid::now_v7(),
                Uuid::now_v7(),
                commit::empty_root(),
                crate::egress::gate::Gates::none().root(Uuid::nil()),
            )
            .expect("mint");
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
            .validate(
                &minter
                    .mint_turn(
                        Uuid::now_v7(),
                        Uuid::now_v7(),
                        commit::empty_root(),
                        crate::egress::gate::Gates::none().root(Uuid::nil()),
                    )
                    .expect("mint"),
            )
            .expect("valid");
        assert!(claims.has_platform_authority(super::super::rbac::Authority::GatewayInvoke));
        assert!(!claims.has_platform_authority(super::super::rbac::Authority::SessionsRead));
        assert!(!claims.has_platform_authority(super::super::rbac::Authority::WorkTake));
    }

    #[test]
    fn a_document_fetch_token_fetches_and_cannot_call_a_model() {
        let (minter, public) = pair();
        let gateway = TokenValidator::new(&public, AUDIENCE_GATEWAY).expect("validator");
        let api = TokenValidator::new(&public, AUDIENCE_API).expect("validator");
        let token = minter
            .mint_document_fetch(
                Uuid::now_v7(),
                Uuid::now_v7(),
                commit::empty_root(),
                crate::egress::gate::Gates::none().root(Uuid::nil()),
            )
            .expect("mint");
        let claims = gateway.validate(&token).expect("valid");
        assert!(claims.has_platform_authority(super::super::rbac::Authority::GatewayFetch));
        assert!(!claims.has_platform_authority(super::super::rbac::Authority::GatewayInvoke));
        assert!(claims.egress_commitment().is_ok());
        assert!(
            api.validate(&token).is_err(),
            "a fetch token opened the API"
        );
    }

    #[test]
    fn a_key_the_verifier_does_not_hold_is_refused_and_a_rotated_one_is_not() {
        let (old, old_public) = pair();
        let new_seed = [9u8; 32];
        let new = TokenMinter::new(&new_seed).expect("minter");
        let new_public = TokenMinter::public_key_of(&new_seed);

        let only_old = TokenValidator::new(&old_public, AUDIENCE_API).expect("validator");
        let both =
            TokenValidator::with_keys(&[old_public, new_public], AUDIENCE_API).expect("validator");

        let signed_new = new
            .mint_session(Uuid::now_v7(), Uuid::now_v7(), &["viewer".to_string()])
            .expect("mint");
        let signed_old = old
            .mint_session(Uuid::now_v7(), Uuid::now_v7(), &["viewer".to_string()])
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
                &["viewer".to_string()],
                None,
                None,
                Duration::from_secs(0),
            )
            .expect("mint");
        std::thread::sleep(Duration::from_millis(1100));
        assert!(matches!(api.validate(&token), Err(AuthError::Expired)));
    }

    #[test]
    fn a_turn_token_carries_the_commitment_it_was_minted_with() {
        let (minter, public) = pair();
        let gateway = TokenValidator::new(&public, AUDIENCE_GATEWAY).expect("validator");
        let committed = commit::root(
            Uuid::now_v7(),
            &[crate::runtime::egress::EgressRule {
                host: "api.example.com".into(),
                header: None,
                credential_env: None,
            }],
        );

        let turn = minter
            .mint_turn(
                Uuid::now_v7(),
                Uuid::now_v7(),
                committed,
                crate::egress::gate::Gates::none().root(Uuid::nil()),
            )
            .expect("mint");
        let claims = gateway.validate(&turn).expect("valid");
        assert_eq!(claims.egress_commitment().expect("committed"), committed);
    }

    #[test]
    fn a_workspace_with_no_rules_still_gets_a_commitment() {
        // The trap this whole claim exists to avoid: a turn for a workspace
        // that allows nothing must carry a claim saying so explicitly, rather
        // than simply lacking one. `empty_root` is what that claim says, and
        // it round-trips like any other commitment.
        let (minter, public) = pair();
        let gateway = TokenValidator::new(&public, AUDIENCE_GATEWAY).expect("validator");

        let turn = minter
            .mint_turn(
                Uuid::now_v7(),
                Uuid::now_v7(),
                commit::empty_root(),
                crate::egress::gate::Gates::none().root(Uuid::nil()),
            )
            .expect("mint");
        let claims = gateway.validate(&turn).expect("valid");
        assert_eq!(
            claims.egress_commitment().expect("committed"),
            commit::empty_root()
        );
    }

    #[test]
    fn a_token_missing_the_egress_claim_is_refused_rather_than_read_as_empty() {
        // A token minted before this claim existed, or one a forger stripped
        // it from, must not verify. If it did, "no commitment" would be
        // silently equivalent to "the empty commitment" -- exactly the answer
        // that lets a stripped token be read as "this workspace allows
        // nothing" instead of being refused outright.
        let (minter, public) = pair();
        let gateway = TokenValidator::new(&public, AUDIENCE_GATEWAY).expect("validator");

        let internal = |e: PasetoError| e;
        let now = chrono::Utc::now();
        let exp = now + chrono::Duration::seconds(300);
        let mut claims = Claims::new().expect("claims");
        claims
            .expiration(&exp.to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
            .map_err(internal)
            .expect("exp");
        claims.audience(AUDIENCE_GATEWAY).expect("aud");
        claims.subject(&Uuid::now_v7().to_string()).expect("sub");
        claims
            .add_additional(WORKSPACE, Uuid::now_v7().to_string())
            .expect("wid");
        claims
            .add_additional(SCOPE, serde_json::json!([Role::Turn.to_string()]))
            .expect("scp");
        // EGRESS deliberately left unset.

        let mut footer = Footer::new();
        footer
            .parse_string(&format!(
                "{{\"{KEY_ID}\":{}}}",
                serde_json::json!(minter.kid)
            ))
            .expect("footer");
        let token = public::sign(&minter.secret_key, &claims, Some(&footer), None).expect("sign");

        // The token itself verifies -- it is properly signed -- but the claim
        // it would need to vouch for a rule is not there, and asking for one
        // refuses rather than handing back the empty commitment.
        let claims = gateway.validate(&token).expect("signed");
        assert_eq!(claims.egress_commitment, None);
        assert!(matches!(
            claims.egress_commitment(),
            Err(AuthError::Invalid)
        ));
    }

    #[test]
    fn a_browser_token_commits_to_nothing_at_all() {
        // A session token never carries a turn's rules, so it makes no
        // statement about them. The empty commitment would be the wrong thing
        // to put here: it is an answer ("this workspace allows nothing")
        // rather than the absence of one, and nothing should be able to lift
        // it off a browser token and treat it as a turn's.
        let (minter, public) = pair();
        let api = TokenValidator::new(&public, AUDIENCE_API).expect("validator");

        let token = minter
            .mint_session(Uuid::now_v7(), Uuid::now_v7(), &["viewer".to_string()])
            .expect("mint");
        let claims = api.validate(&token).expect("valid");

        assert_eq!(claims.egress_commitment, None);
        assert!(matches!(
            claims.egress_commitment(),
            Err(AuthError::Invalid)
        ));
    }

    #[test]
    fn the_runtime_key_matches_only_itself() {
        let key = RuntimeKey::new("0123456789abcdef0123456789abcdef").expect("key");
        assert!(key.accepts("0123456789abcdef0123456789abcdef"));
        assert!(!key.accepts("0123456789abcdef0123456789abcdeg"));
        assert!(!key.accepts("0123456789abcdef0123456789abcde"));
        assert!(
            RuntimeKey::new("short").is_err(),
            "a short key was accepted"
        );
    }
}
