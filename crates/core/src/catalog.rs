//! The model catalog: what each provider's models accept and how far they
//! reach, reduced from models.dev to the fields outturn reads
//! (docs/models.md, *The catalog*).
//!
//! Pure here: reducing a document, reading one in any of the shapes it
//! arrives in, and the snapshot every release ships. Where snapshots are
//! kept, and when a newer one is fetched, belong to the API.

use sha2::{Digest, Sha256};
use std::io::Read;

/// The catalog this release was built with, reduced and gzipped. A
/// deployment starts with it whether or not it can reach anything; fetching
/// is how it stays current, not how it starts. Refreshed by
/// `scripts/refresh-catalog.sh`.
const SHIPPED: &[u8] = include_bytes!("../../../assets/catalog.json.gz");

/// Where the shipped snapshot says it came from.
pub const SHIPPED_SOURCE: &str = "shipped";

/// The fields of a model that are kept. Price is not among them: outturn
/// reports tokens, never money, and a price stored here would be a number
/// somebody mistakes for the bill.
const MODEL_FIELDS: &[&str] = &[
    "name",
    "family",
    "attachment",
    "reasoning",
    "reasoning_options",
    "tool_call",
    "structured_output",
    "temperature",
    "release_date",
    "modalities",
    "limit",
    "canonical_model_id",
    "status",
];

/// The fields of a provider that are kept: what to call it, where its API
/// is, and which variables its key is usually found in.
const PROVIDER_FIELDS: &[&str] = &["name", "api", "env"];

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("the catalog is not readable: {0}")]
    Unreadable(String),
}

/// Reduces a catalog document to the fields above, keyed by provider and
/// model id. Reducing a reduced catalog changes nothing, so a document can be
/// passed through whatever shape it arrived in.
pub fn reduce(raw: &serde_json::Value) -> Result<serde_json::Value, CatalogError> {
    let providers = raw
        .as_object()
        .ok_or_else(|| CatalogError::Unreadable("not an object of providers".into()))?;
    let mut out = serde_json::Map::new();
    for (provider_id, provider) in providers {
        let Some(provider) = provider.as_object() else {
            continue;
        };
        let mut kept = keep(provider, PROVIDER_FIELDS);
        let mut models = serde_json::Map::new();
        for (model_id, model) in provider
            .get("models")
            .and_then(|m| m.as_object())
            .into_iter()
            .flatten()
        {
            let Some(model) = model.as_object() else {
                continue;
            };
            let mut model = keep(model, MODEL_FIELDS);
            if let Some(limit) = model.get("limit").and_then(|l| l.as_object()) {
                let limit = keep(limit, &["context", "output"]);
                model.insert("limit".into(), serde_json::Value::Object(limit));
            }
            models.insert(model_id.clone(), serde_json::Value::Object(model));
        }
        kept.insert("models".into(), serde_json::Value::Object(models));
        out.insert(provider_id.clone(), serde_json::Value::Object(kept));
    }
    if out.is_empty() {
        return Err(CatalogError::Unreadable("it names no providers".into()));
    }
    Ok(serde_json::Value::Object(out))
}

fn keep(
    object: &serde_json::Map<String, serde_json::Value>,
    fields: &[&str],
) -> serde_json::Map<String, serde_json::Value> {
    object
        .iter()
        .filter(|(k, _)| fields.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Reads a catalog as it arrives -- the site's document, a reduced one, or
/// either gzipped -- and reduces it.
pub fn read(bytes: &[u8]) -> Result<serde_json::Value, CatalogError> {
    let plain = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_end(&mut out)
            .map_err(|e| CatalogError::Unreadable(format!("not gzip after all: {e}")))?;
        out
    } else {
        bytes.to_vec()
    };
    let raw: serde_json::Value =
        serde_json::from_slice(&plain).map_err(|e| CatalogError::Unreadable(e.to_string()))?;
    reduce(&raw)
}

/// The catalog this release shipped with.
pub fn shipped() -> serde_json::Value {
    read(SHIPPED).expect("the shipped catalog is built by this repository and reads")
}

/// Names a catalog's content, so a fetch that brought nothing new stores
/// nothing. Over the serialized form, whose object keys are sorted.
pub fn digest(catalog: &serde_json::Value) -> String {
    hex::encode(Sha256::digest(catalog.to_string().as_bytes()))
}

/// How many providers and models a catalog names.
pub fn counts(catalog: &serde_json::Value) -> (usize, usize) {
    let providers = catalog.as_object().map(|p| p.len()).unwrap_or(0);
    let models = catalog
        .as_object()
        .into_iter()
        .flat_map(|p| p.values())
        .filter_map(|p| p["models"].as_object())
        .map(|m| m.len())
        .sum();
    (providers, models)
}

/// The catalog's provider for an account: the one whose API is at the
/// account's host, else the one that is the protocol's own vendor at its own
/// host. An OpenAI-protocol account anywhere else -- ollama, a proxy -- is
/// none of them, and its models are entered by hand.
pub fn provider_for(catalog: &serde_json::Value, protocol: &str, base_url: &str) -> Option<String> {
    let host = host_of(base_url)?;
    let providers = catalog.as_object()?;
    // Host and port, not host alone: several providers in the catalog are
    // programs that run on the machine itself, each at `localhost` or
    // `127.0.0.1` on a port of its own, and matching the host alone took an
    // account at `localhost:11434` for one of them and offered its models.
    let place = place_of(base_url)?;
    let by_api = providers.iter().find(|(_, p)| {
        p["api"]
            .as_str()
            .and_then(place_of)
            .is_some_and(|api| api == place)
    });
    if let Some((id, _)) = by_api {
        return Some(id.clone());
    }
    let (id, vendor_host) = match protocol {
        "anthropic" => ("anthropic", "api.anthropic.com"),
        "gemini" => ("google", "generativelanguage.googleapis.com"),
        "openai" => ("openai", "api.openai.com"),
        // Bedrock's host names its region, and every region is Bedrock.
        "bedrock" if host.starts_with("bedrock-runtime.") => return Some("amazon-bedrock".into()),
        _ => return None,
    };
    (host == vendor_host && providers.contains_key(id)).then(|| id.to_string())
}

/// A model's catalog entry, by provider and the id the provider is asked for.
pub fn entry<'a>(
    catalog: &'a serde_json::Value,
    provider: &str,
    model: &str,
) -> Option<&'a serde_json::Value> {
    catalog.get(provider)?.get("models")?.get(model)
}

/// The newest version in `current`'s line at `provider`, for a model that
/// tracks "Latest" (docs/models.md). `current` itself when nothing is newer;
/// None when the catalog no longer has it.
///
/// A line is narrower than a family. The catalog's families are coarse --
/// Gemini Pro's holds image and translation models beside the chat ones -- so
/// a candidate must also be what the current model is: the same output, at
/// least the same input, tools and reasoning where it has them, and the same
/// id up to the family's name, which keeps Bedrock's `us.` ids among `us.`
/// ones. Not a provider's own moving alias (`-latest`), whose meaning shifts
/// without the catalog saying; not a preview, unless the current one is.
/// Alpha, beta and deprecated versions are passed over while anything else
/// is left. Ties on release date go to the shorter id: the alias over its
/// dated twin.
pub fn newest_in_line(
    catalog: &serde_json::Value,
    provider: &str,
    current: &str,
) -> Option<String> {
    let models = catalog.get(provider)?.get("models")?.as_object()?;
    let this = models.get(current)?;
    let family = this["family"].as_str()?;
    let prefix = current.find(family).map_or("", |at| &current[..at]);
    let preview = current.contains("preview");
    let set = |v: &serde_json::Value| -> std::collections::BTreeSet<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| m.as_str().map(str::to_string))
            .collect()
    };
    let output = set(&this["modalities"]["output"]);
    let input = set(&this["modalities"]["input"]);
    let wants = |field: &str, m: &serde_json::Value| this[field] != true || m[field] == true;

    let in_line: Vec<(&String, &serde_json::Value)> = models
        .iter()
        .filter(|(id, m)| {
            m["family"] == family
                && id.starts_with(prefix)
                && (prefix.is_empty() || id[prefix.len()..].starts_with(family))
                && set(&m["modalities"]["output"]) == output
                && set(&m["modalities"]["input"]).is_superset(&input)
                && wants("tool_call", m)
                && wants("reasoning", m)
                && !id.ends_with("-latest")
                && (preview || !id.contains("preview"))
        })
        .collect();
    let settled = |m: &serde_json::Value| {
        !matches!(m["status"].as_str(), Some("alpha" | "beta" | "deprecated"))
    };
    let pool: Vec<_> = if in_line.iter().any(|(_, m)| settled(m)) {
        in_line.into_iter().filter(|(_, m)| settled(m)).collect()
    } else {
        in_line
    };
    pool.into_iter()
        .max_by(|(a_id, a), (b_id, b)| {
            let date = |m: &serde_json::Value| m["release_date"].as_str().unwrap_or("").to_string();
            date(a).cmp(&date(b)).then(b_id.len().cmp(&a_id.len()))
        })
        .map(|(id, _)| id.clone())
        .or_else(|| Some(current.to_string()))
}

/// The newest chat model of a family at a provider: text in and out, with
/// tools, not a preview or a moving alias, and settled where anything
/// settled is left. What a recipe names a tier by, so no model id is
/// written down anywhere to go stale.
///
/// `prefix` narrows the ids, for a provider that lists one model several
/// times over: Bedrock's `us.`, `eu.` and `global.` profiles. Empty for any
/// other.
pub fn newest_of_family(
    catalog: &serde_json::Value,
    provider: &str,
    family: &str,
    prefix: &str,
) -> Option<String> {
    let models = catalog.get(provider)?.get("models")?.as_object()?;
    let chat: Vec<(&String, &serde_json::Value)> = models
        .iter()
        .filter(|(id, m)| {
            m["family"] == family
                && id.starts_with(prefix)
                && m["modalities"]["output"] == serde_json::json!(["text"])
                && m["tool_call"] == true
                && !id.ends_with("-latest")
                && !id.contains("preview")
        })
        .collect();
    let settled = |m: &serde_json::Value| {
        !matches!(m["status"].as_str(), Some("alpha" | "beta" | "deprecated"))
    };
    let pool: Vec<_> = if chat.iter().any(|(_, m)| settled(m)) {
        chat.into_iter().filter(|(_, m)| settled(m)).collect()
    } else {
        chat
    };
    pool.into_iter()
        .max_by(|(a_id, a), (b_id, b)| {
            let date = |m: &serde_json::Value| m["release_date"].as_str().unwrap_or("").to_string();
            date(a).cmp(&date(b)).then(b_id.len().cmp(&a_id.len()))
        })
        .map(|(id, _)| id.clone())
}

/// A URL's host and port, the port as written or the scheme's default.
fn place_of(url: &str) -> Option<(String, u16)> {
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    Some((host, parsed.port_or_known_default()?))
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', ':']).next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    /// Against the shipped catalog: each line moves to its own newest and
    /// nowhere else.
    #[test]
    fn a_line_moves_to_its_own_newest() {
        let held = super::shipped();
        let newest = |provider, id| super::newest_in_line(&held, provider, id).unwrap();
        assert_eq!(
            newest("anthropic", "claude-sonnet-4-5"),
            "claude-sonnet-5-5"
        );
        assert_eq!(
            newest("anthropic", "claude-sonnet-5-5"),
            "claude-sonnet-5-5"
        );
        assert_eq!(newest("anthropic", "claude-opus-4-8"), "claude-opus-5-5");
        assert_eq!(
            newest("amazon-bedrock", "us.anthropic.claude-sonnet-5"),
            "us.anthropic.claude-sonnet-5-5",
            "a us. id stays among us. ids"
        );
        assert_eq!(newest("google", "gemini-3.7-flash"), "gemini-3.8-flash");
        // Gemini Pro's family holds image, research and translation models,
        // and its only newer chat model is a preview: none of them is next.
        assert_eq!(newest("google", "gemini-2.5-pro"), "gemini-2.5-pro");
        assert_eq!(
            newest("google", "gemini-3.1-pro-preview"),
            "gemini-3.1-pro-preview"
        );
        assert!(super::newest_in_line(&held, "anthropic", "no-such-model").is_none());
        // A family's newest chat model, which is what a recipe asks for.
        let family = |p, f| super::newest_of_family(&held, p, f, "").unwrap();
        assert_eq!(family("anthropic", "claude-opus"), "claude-opus-5-5");
        assert_eq!(family("google", "gemini-pro"), "gemini-2.5-pro");
        assert_eq!(family("google", "gemini-flash"), "gemini-3.8-flash");
        assert_eq!(
            super::newest_of_family(&held, "amazon-bedrock", "claude-sonnet", "eu.").unwrap(),
            "eu.anthropic.claude-sonnet-5-5"
        );
    }

    #[test]
    fn an_account_finds_its_provider() {
        let catalog = serde_json::json!({
            "anthropic": {"models": {}},
            "google": {"models": {}},
            "openai": {"models": {}},
            "amazon-bedrock": {"models": {}},
            "openrouter": {"api": "https://openrouter.ai/api/v1", "models": {}},
        });
        let of = |protocol, url| super::provider_for(&catalog, protocol, url);
        assert_eq!(
            of("anthropic", "https://api.anthropic.com").as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            of("gemini", "https://generativelanguage.googleapis.com/v1beta").as_deref(),
            Some("google")
        );
        assert_eq!(
            of("openai", "https://openrouter.ai/api/v1").as_deref(),
            Some("openrouter")
        );
        assert_eq!(
            of("bedrock", "https://bedrock-runtime.us-east-1.amazonaws.com").as_deref(),
            Some("amazon-bedrock")
        );
        assert_eq!(of("openai", "http://ollama.ollama:11434/v1"), None);
        // A program on this machine is told apart by its port.
        let local = serde_json::json!({
            "privatemode-ai": {"api": "http://localhost:8080/v1", "models": {}},
            "lmstudio": {"api": "http://127.0.0.1:1234/v1", "models": {}},
        });
        assert_eq!(
            super::provider_for(&local, "openai", "http://localhost:11434"),
            None
        );
        assert_eq!(
            super::provider_for(&local, "openai", "http://127.0.0.1:1234/v1").as_deref(),
            Some("lmstudio")
        );
        assert_eq!(of("anthropic", "http://proxy.example.com"), None);
    }

    use super::*;

    fn site() -> serde_json::Value {
        serde_json::json!({
            "anthropic": {
                "id": "anthropic", "name": "Anthropic", "npm": "@ai-sdk/anthropic",
                "doc": "https://example.com", "env": ["ANTHROPIC_API_KEY"],
                "models": {
                    "claude-x": {
                        "id": "claude-x", "name": "Claude X", "family": "claude",
                        "description": "words", "temperature": false, "tool_call": true,
                        "limit": {"context": 1000000, "output": 128000, "input": 9},
                        "cost": {"input": 2.0, "output": 10.0},
                        "release_date": "2026-09-01",
                    }
                }
            }
        })
    }

    /// Price is dropped, with everything else not read.
    #[test]
    fn a_catalog_is_reduced_to_what_is_read() {
        let reduced = reduce(&site()).unwrap();
        let model = &reduced["anthropic"]["models"]["claude-x"];
        assert!(model.get("cost").is_none(), "{model}");
        assert!(model.get("description").is_none());
        assert_eq!(
            model["limit"],
            serde_json::json!({"context": 1000000, "output": 128000})
        );
        assert_eq!(model["temperature"], false);
        assert!(reduced["anthropic"].get("npm").is_none());
        assert_eq!(reduced["anthropic"]["env"][0], "ANTHROPIC_API_KEY");
    }

    /// So any shape can be passed through: the site's, a reduced one, gzip.
    #[test]
    fn reducing_twice_changes_nothing_and_gzip_reads_the_same() {
        let once = reduce(&site()).unwrap();
        assert_eq!(reduce(&once).unwrap(), once);

        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gz, site().to_string().as_bytes()).unwrap();
        assert_eq!(read(&gz.finish().unwrap()).unwrap(), once);
    }

    #[test]
    fn the_shipped_catalog_reads_and_names_the_providers_outturn_speaks_to() {
        let shipped = shipped();
        for provider in ["anthropic", "google", "openai"] {
            assert!(shipped.get(provider).is_some(), "no {provider}");
        }
        let (providers, models) = counts(&shipped);
        assert!(
            providers > 50 && models > 1000,
            "{providers} providers, {models} models"
        );
    }

    #[test]
    fn something_that_is_not_a_catalog_is_refused() {
        assert!(read(b"{}").is_err());
        assert!(read(b"not json").is_err());
    }
}
