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

#[cfg(test)]
mod tests {
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
