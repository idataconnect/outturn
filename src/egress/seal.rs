//! Sealed credentials: a secret encrypted to the gateway's public key, with
//! where it may go bound into the encryption. See `docs/sealed-credentials.md`.
//!
//! HPKE (RFC 9180) in base mode: X25519, HKDF-SHA256, AES-256-GCM. The secret
//! is the plaintext and nothing else. The binding is the associated data, so
//! the API and the browser can read where a key goes without opening it, and
//! nobody can change that without the tag failing.
//!
//! What a seal does not prove is who made it: the public key is public, so
//! anyone can seal a secret they hold under any binding. That is the
//! provenance gap the design document describes, and why nothing here treats
//! a seal that opens as one somebody in particular wrote.

use std::collections::HashMap;

use hpke::{
    Deserializable, Kem as _, OpModeR, OpModeS, Serializable, aead::AesGcm256, kdf::HkdfSha256,
    kem::X25519HkdfSha256,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// What a credential seal is, so a ciphertext made for anything else -- a
/// manage token, a refresh token -- can never be opened as one, or the reverse.
pub const INFO: &[u8] = b"outturn seal v1 egress-credential";

/// The length of an encapsulated X25519 key, which leads every seal.
const ENC_LEN: usize = 32;

/// How a credential is used. Bound, so a client secret -- bound to the
/// resource host for the token it buys -- can never be attached there as a
/// plain header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Static,
    ClientId,
    ClientSecret,
}

/// Where a credential may go: the associated data it was sealed under.
///
/// Read from the exact bytes that were sealed, never re-serialised to check a
/// tag, so a change in how JSON is written cannot make a valid seal stop
/// opening.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// The credential's own id, so a ciphertext copied onto another row fails.
    pub credential: Uuid,
    pub kind: Kind,
    /// Workspace ids, or `"*"` alone for every workspace.
    pub workspaces: Vec<String>,
    /// Exact names: no wildcard, port or scheme.
    pub hosts: Vec<String>,
    /// The header a static credential travels in. Required for `static`,
    /// absent otherwise.
    #[serde(default)]
    pub header: Option<String>,
    /// The token endpoint a client pair is sent to. Required for the client
    /// kinds, absent for `static`.
    #[serde(default)]
    pub token_url: Option<String>,
}

impl Binding {
    /// Reads and checks a binding from the bytes it was sealed under.
    ///
    /// As strict as `OUTTURN_CREDENTIAL_BINDINGS` is read, and for the same
    /// reason: anything not understood is refused rather than half-applied.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let binding: Binding =
            serde_json::from_slice(bytes).map_err(|e| format!("binding is not readable: {e}"))?;
        binding.check()?;
        Ok(binding)
    }

    fn check(&self) -> Result<(), String> {
        match self.workspaces.as_slice() {
            [] => return Err("binding names no workspace".into()),
            [only] if only == "*" => {}
            ids => {
                for id in ids {
                    if id == "*" {
                        return Err(
                            "\"*\" already means every workspace; list ids or \"*\", not both"
                                .into(),
                        );
                    }
                    Uuid::parse_str(id).map_err(|_| format!("{id} is not a workspace id"))?;
                }
            }
        }
        if self.hosts.is_empty() {
            return Err("binding names no host".into());
        }
        for host in &self.hosts {
            let plain = !host.is_empty()
                && host
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
                && !host.starts_with('.')
                && !host.ends_with('.');
            if !plain {
                return Err(format!(
                    "{host} is not a plain host name: lower case, no wildcard, port or scheme"
                ));
            }
        }
        match (self.kind, &self.header, &self.token_url) {
            (Kind::Static, Some(header), None) if !header.trim().is_empty() => Ok(()),
            (Kind::Static, _, _) => {
                Err("a static credential names its header and no token endpoint".into())
            }
            (Kind::ClientId | Kind::ClientSecret, None, Some(url)) => {
                let parsed = reqwest::Url::parse(url).map_err(|_| format!("{url} is not a URL"))?;
                if parsed.scheme() != "https" {
                    return Err(format!("{url} is not https"));
                }
                Ok(())
            }
            (Kind::ClientId | Kind::ClientSecret, _, _) => {
                Err("a client credential names its token endpoint and no header".into())
            }
        }
    }

    /// Whether `workspace` may use this for `host`, in `header`, as `kind`.
    ///
    /// The questions the environment bindings ask, asked of the seal. The host
    /// is compared exactly, without its port, as `Shape` names it.
    pub fn allows(
        &self,
        workspace: Uuid,
        host: &str,
        header: Option<&str>,
        kind: Kind,
    ) -> Result<(), String> {
        let every = matches!(self.workspaces.as_slice(), [only] if only == "*");
        let ours = workspace.to_string();
        if !every
            && !self
                .workspaces
                .iter()
                .any(|w| w.eq_ignore_ascii_case(&ours))
        {
            return Err("this credential is not bound to this workspace".into());
        }
        let host = host.trim_matches(['[', ']']);
        if !self.hosts.iter().any(|h| h.eq_ignore_ascii_case(host)) {
            return Err(format!("this credential is not bound to {host}"));
        }
        if self.kind != kind {
            return Err("this credential is not for this use".into());
        }
        if kind == Kind::Static {
            let bound = self.header.as_deref().unwrap_or_default();
            if !header.is_some_and(|h| h.eq_ignore_ascii_case(bound)) {
                return Err(format!("this credential travels only in {bound}"));
            }
        }
        Ok(())
    }
}

/// A short, stable name for a public key, so a seal says which key it was made
/// for and the gateway can hold two during a rotation.
pub fn key_id(public: &[u8]) -> String {
    hex::encode(&Sha256::digest(public)[..8])
}

/// The public half of a private key given as 64 hex characters.
pub fn public_of(private_hex: &str) -> Result<Vec<u8>, String> {
    let bytes = hex::decode(private_hex.trim()).map_err(|_| "not hex".to_string())?;
    let sk = <X25519HkdfSha256 as hpke::Kem>::PrivateKey::from_bytes(&bytes)
        .map_err(|_| "not an X25519 private key".to_string())?;
    Ok(X25519HkdfSha256::sk_to_pk(&sk).to_bytes().to_vec())
}

/// Seals `secret` under `binding` (its exact bytes) to `public`.
///
/// What the browser and `outturn-seal` do. Here for the binary and the tests;
/// the API never calls it, since it must never hold a plaintext.
pub fn seal(public: &[u8], binding: &[u8], secret: &[u8]) -> Result<Vec<u8>, String> {
    let pk = <X25519HkdfSha256 as hpke::Kem>::PublicKey::from_bytes(public)
        .map_err(|_| "not an X25519 public key".to_string())?;
    let (enc, ciphertext) = hpke::single_shot_seal::<AesGcm256, HkdfSha256, X25519HkdfSha256>(
        &OpModeS::Base,
        &pk,
        INFO,
        secret,
        binding,
    )
    .map_err(|e| format!("sealing failed: {e}"))?;
    let mut out = enc.to_bytes().to_vec();
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// The private keys a gateway opens seals with, by key id.
///
/// More than one only during a rotation, as `OUTTURN_TOKEN_PUBLIC_KEY` takes
/// two then.
#[derive(Default)]
pub struct Keys {
    by_id: HashMap<String, <X25519HkdfSha256 as hpke::Kem>::PrivateKey>,
    /// Per key, what fingerprints are keyed with: derived from the private key,
    /// so nothing but this gateway can compute one, and a fingerprint is no
    /// help to anybody guessing the secret behind it.
    fingerprint_keys: HashMap<String, [u8; 32]>,
}

impl Keys {
    /// From `OUTTURN_SEAL_KEY`: one or two private keys, 64 hex characters
    /// each, comma separated. Unset or unreadable means none, and every sealed
    /// credential is refused -- loudly, at startup.
    pub fn from_env() -> Self {
        match std::env::var("OUTTURN_SEAL_KEY") {
            Ok(value) if !value.trim().is_empty() => match Self::parse(&value) {
                Ok(keys) => keys,
                Err(e) => {
                    tracing::error!(error = %e, "OUTTURN_SEAL_KEY could not be read, so no sealed credential can be opened");
                    Self::default()
                }
            },
            _ => {
                tracing::info!("no OUTTURN_SEAL_KEY; sealed credentials are refused");
                Self::default()
            }
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        let mut by_id = HashMap::new();
        let mut fingerprint_keys = HashMap::new();
        for hex_key in value.split(',').map(str::trim).filter(|k| !k.is_empty()) {
            let bytes = hex::decode(hex_key).map_err(|_| "a seal key is not hex".to_string())?;
            let sk = <X25519HkdfSha256 as hpke::Kem>::PrivateKey::from_bytes(&bytes)
                .map_err(|_| "a seal key is not an X25519 private key".to_string())?;
            let pk = X25519HkdfSha256::sk_to_pk(&sk);
            let id = key_id(&pk.to_bytes());
            fingerprint_keys.insert(
                id.clone(),
                hmac(&bytes, b"outturn credential fingerprint v1"),
            );
            by_id.insert(id, sk);
        }
        Ok(Self {
            by_id,
            fingerprint_keys,
        })
    }

    /// A short name for an opened secret that only this gateway can compute,
    /// shown beside a credential so a key swapped underneath its owner is seen:
    /// the owner saw one value when they connected it, and a different key
    /// shows a different one. See docs/sealed-credentials.md, "Who can seal".
    pub fn fingerprint(&self, key_id: &str, secret: &[u8]) -> Option<String> {
        let key = self.fingerprint_keys.get(key_id)?;
        Some(hex::encode(&hmac(key, secret)[..4]))
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Opens a seal made for `key_id` under `binding`'s exact bytes.
    ///
    /// Fails, saying nothing about why beyond that, for a wrong key, a binding
    /// edited after sealing, or a ciphertext moved from another row: the
    /// reasons are not the caller's to distinguish and the answer is the same.
    pub fn open(
        &self,
        key_id: &str,
        sealed: &[u8],
        binding: &[u8],
    ) -> Result<zeroize::Zeroizing<Vec<u8>>, String> {
        let sk = self.by_id.get(key_id).ok_or_else(|| {
            "this credential was sealed to a key this gateway does not hold".to_string()
        })?;
        if sealed.len() <= ENC_LEN {
            return Err("this credential could not be opened".into());
        }
        let (enc, ciphertext) = sealed.split_at(ENC_LEN);
        let enc = <X25519HkdfSha256 as hpke::Kem>::EncappedKey::from_bytes(enc)
            .map_err(|_| "this credential could not be opened".to_string())?;
        hpke::single_shot_open::<AesGcm256, HkdfSha256, X25519HkdfSha256>(
            &OpModeR::Base,
            sk,
            &enc,
            INFO,
            ciphertext,
            binding,
        )
        .map(zeroize::Zeroizing::new)
        .map_err(|_| "this credential could not be opened".to_string())
    }
}

fn hmac(key: &[u8], message: &[u8]) -> [u8; 32] {
    use hmac::Mac as _;
    let mut mac =
        hmac::Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(message);
    mac.finalize().into_bytes().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACME: &str = "01920000-0000-7000-8000-00000000000a";
    const OTHER: &str = "01920000-0000-7000-8000-00000000000b";

    fn private() -> String {
        hex::encode([7u8; 32])
    }

    fn binding(credential: Uuid) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "credential": credential,
            "kind": "static",
            "workspaces": [ACME],
            "hosts": ["books.example.com"],
            "header": "Authorization",
        }))
        .unwrap()
    }

    fn keys() -> (Keys, Vec<u8>, String) {
        let keys = Keys::parse(&private()).unwrap();
        let public = public_of(&private()).unwrap();
        let id = key_id(&public);
        (keys, public, id)
    }

    /// `scripts/lib/keys.sh` derives the public key with openssl; the gateway
    /// derives it here. If they disagreed, every seal made to the published key
    /// would fail to open, so the scripts' answer for a fixed key is pinned.
    #[test]
    fn a_seal_key_derives_the_public_key_the_scripts_do() {
        assert_eq!(
            hex::encode(public_of(&private()).unwrap()),
            "13be4feaeaf204c7fd3358fc9c00721881d174278128227ec674f37f7fe97b6d"
        );
    }

    /// A seal made in a browser by `ui/src/lib/seal.ts` (`@hpke/core`), to the
    /// test key above, opens here. The two implementations must agree on the
    /// suite, the label and the layout, or every credential stored from the
    /// page is one the gateway cannot open.
    #[test]
    fn a_seal_made_in_the_browser_opens_here() {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let binding = b64.decode("eyJjcmVkZW50aWFsIjoiMDAwMDAwMDAtMDAwMC0wMDAwLTAwMDAtMDAwMDAwMDAwMDAwIiwia2luZCI6InN0YXRpYyIsIndvcmtzcGFjZXMiOlsiMDE5MjAwMDAtMDAwMC03MDAwLTgwMDAtMDAwMDAwMDAwMDBhIl0sImhvc3RzIjpbImJvb2tzLmV4YW1wbGUuY29tIl0sImhlYWRlciI6ImF1dGhvcml6YXRpb24ifQ==").unwrap();
        let sealed = b64.decode("oCVSksXbRTxwtKsrp4OQ5gWTFGBggfHmNrTgPWSwCmAXP8tDQ8CqHWLEHwQCtI07UR6WG3G3SDMheuZSV+ds5OxVokSi3EUR2tI=").unwrap();
        let (keys, _, id) = keys();
        assert_eq!(
            &*keys.open(&id, &sealed, &binding).unwrap(),
            b"Bearer bc_from_the_browser"
        );
        assert!(
            Binding::parse(&binding).is_ok(),
            "the page writes a binding this reads"
        );
    }

    #[test]
    fn a_fingerprint_tells_two_secrets_apart_and_names_one_alike() {
        let (keys, _, id) = keys();
        let a = keys.fingerprint(&id, b"Bearer one").unwrap();
        assert_eq!(a, keys.fingerprint(&id, b"Bearer one").unwrap());
        assert_ne!(a, keys.fingerprint(&id, b"Bearer two").unwrap());
        assert_eq!(a.len(), 8);
    }

    #[test]
    fn a_seal_opens_under_its_own_binding() {
        let (keys, public, id) = keys();
        let ad = binding(Uuid::nil());
        let sealed = seal(&public, &ad, b"sk_live_123").unwrap();
        assert_eq!(&*keys.open(&id, &sealed, &ad).unwrap(), b"sk_live_123");
    }

    /// The property the design rests on: where a secret goes cannot be
    /// changed without the plaintext.
    #[test]
    fn a_binding_edited_after_sealing_does_not_open() {
        let (keys, public, id) = keys();
        let ad = binding(Uuid::nil());
        let sealed = seal(&public, &ad, b"secret").unwrap();
        let moved = String::from_utf8(ad)
            .unwrap()
            .replace("books.example.com", "collect.example.net");
        assert!(keys.open(&id, &sealed, moved.as_bytes()).is_err());
    }

    #[test]
    fn a_seal_moved_onto_another_credential_does_not_open() {
        let (keys, public, id) = keys();
        let sealed = seal(&public, &binding(Uuid::nil()), b"secret").unwrap();
        let other = binding(Uuid::from_bytes([1; 16]));
        assert!(keys.open(&id, &sealed, &other).is_err());
    }

    #[test]
    fn a_seal_to_another_key_does_not_open() {
        let (keys, _, id) = keys();
        let stranger = public_of(&hex::encode([9u8; 32])).unwrap();
        let ad = binding(Uuid::nil());
        let sealed = seal(&stranger, &ad, b"secret").unwrap();
        assert!(keys.open(&id, &sealed, &ad).is_err());
        assert!(
            keys.open(&key_id(&stranger), &sealed, &ad).is_err(),
            "a key id this gateway does not hold opens nothing"
        );
    }

    #[test]
    fn a_binding_allows_only_its_workspace_host_header_and_kind() {
        let b = Binding::parse(&binding(Uuid::nil())).unwrap();
        let acme = Uuid::parse_str(ACME).unwrap();
        let other = Uuid::parse_str(OTHER).unwrap();
        assert!(
            b.allows(
                acme,
                "books.example.com",
                Some("authorization"),
                Kind::Static
            )
            .is_ok()
        );
        assert!(
            b.allows(
                other,
                "books.example.com",
                Some("Authorization"),
                Kind::Static
            )
            .is_err()
        );
        assert!(
            b.allows(
                acme,
                "collect.example.net",
                Some("Authorization"),
                Kind::Static
            )
            .is_err()
        );
        assert!(
            b.allows(acme, "books.example.com", Some("X-Api-Key"), Kind::Static)
                .is_err()
        );
        assert!(
            b.allows(acme, "books.example.com", None, Kind::ClientSecret)
                .is_err()
        );
    }

    #[test]
    fn a_binding_that_could_be_misread_is_refused() {
        let base = || {
            serde_json::json!({
                "credential": Uuid::nil(), "kind": "static",
                "workspaces": [ACME], "hosts": ["books.example.com"], "header": "Authorization",
            })
        };
        let refused = |edit: &dyn Fn(&mut serde_json::Value)| {
            let mut v = base();
            edit(&mut v);
            Binding::parse(&serde_json::to_vec(&v).unwrap()).is_err()
        };
        assert!(refused(
            &|v| v["hosts"] = serde_json::json!(["*.example.com"])
        ));
        assert!(refused(
            &|v| v["hosts"] = serde_json::json!(["books.example.com:443"])
        ));
        assert!(refused(
            &|v| v["hosts"] = serde_json::json!(["https://books.example.com"])
        ));
        assert!(refused(
            &|v| v["workspaces"] = serde_json::json!(["*", ACME])
        ));
        assert!(refused(&|v| v["workspaces"] = serde_json::json!([])));
        assert!(refused(&|v| v["kind"] = serde_json::json!("client_secret")));
        assert!(refused(&|v| v["extra"] = serde_json::json!(true)));
        assert!(!refused(&|_| {}));
    }
}
