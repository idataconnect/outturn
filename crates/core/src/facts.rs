//! What a model accepts, as a request is built for it.
//!
//! From the model's catalog entry, with whatever the deployment overrode laid
//! over it (docs/models.md). Two facts the catalog does not carry are
//! outturn's own, given by the family's defaults below and overridable the
//! same way: whether a Claude thinks adaptively, and whether it takes
//! `block_binding`. Both are fixed by when the model came out, not by its
//! name, so a model released tomorrow under a name nothing has seen is
//! described correctly the day its catalog entry appears.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct ModelFacts {
    /// Accepts `temperature` and its siblings.
    pub sampling: bool,
    /// The most it may write in one reply, used where a request names none.
    pub output_limit: Option<u32>,
    /// How many tokens it reads at once.
    pub context_window: Option<u32>,
    /// What model it is, whichever door it is reached through: Sonnet at
    /// Anthropic and Sonnet on Bedrock share one, so a fallback between them
    /// is the same model.
    pub canonical: Option<String>,
    /// The effort levels it offers, lowest first.
    pub efforts: Vec<String>,
    /// Thinks adaptively, with effort as its control.
    pub adaptive_thinking: bool,
    /// Takes `thinking.block_binding`, which says what to do with a stale
    /// thinking block. Sent to a model that does not, it is refused on some
    /// platforms -- Bedrock's 4.6, for one -- and buys nothing there.
    pub thinking_binding: bool,
}

impl Default for ModelFacts {
    /// What is assumed of a model nobody has described: a hand-entered one,
    /// or one asked for with no configured model behind it. Anything it is
    /// sent is the request's own, so nothing is left out on its behalf and
    /// nothing is added.
    fn default() -> Self {
        Self {
            sampling: true,
            output_limit: None,
            context_window: None,
            canonical: None,
            efforts: Vec::new(),
            adaptive_thinking: false,
            thinking_binding: false,
        }
    }
}

/// The first Claude to think adaptively, Opus 4.6, and the first to take
/// `block_binding`, Fable 5. Earlier ones of the same family list effort
/// levels too -- Opus 4.5 does -- but take thinking as a token budget.
const CLAUDE_ADAPTIVE_FROM: &str = "2026-02-04";
const CLAUDE_BINDING_FROM: &str = "2026-06-07";

impl ModelFacts {
    /// From a catalog entry and the overrides laid over it; either may be
    /// absent. An override is a field of the entry, or one of the two
    /// outturn adds, under the same name as here.
    pub fn of(entry: Option<&Value>, overrides: Option<&Value>) -> Self {
        let mut merged = entry.cloned().unwrap_or(Value::Null);
        if !merged.is_object() {
            merged = Value::Object(Default::default());
        }
        for (key, value) in family_defaults(&merged) {
            merged[key] = value;
        }
        if let Some(Value::Object(over)) = overrides {
            for (key, value) in over {
                merged[key.as_str()] = value.clone();
            }
        }
        let defaults = Self::default();
        let efforts = merged["reasoning_options"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|option| option["type"] == "effort")
            .and_then(|option| option["values"].as_array())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            sampling: merged["temperature"].as_bool().unwrap_or(defaults.sampling),
            output_limit: merged["limit"]["output"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok()),
            context_window: merged["limit"]["context"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok()),
            canonical: merged["canonical_model_id"].as_str().map(str::to_string),
            efforts,
            adaptive_thinking: merged["adaptive_thinking"].as_bool().unwrap_or(false),
            thinking_binding: merged["thinking_binding"].as_bool().unwrap_or(false),
        }
    }
}

/// The facts a family's entries leave unsaid.
fn family_defaults(entry: &Value) -> Vec<(&'static str, Value)> {
    let family = entry["family"].as_str().unwrap_or("");
    let released = entry["release_date"].as_str().unwrap_or("");
    if !family.starts_with("claude") || released.is_empty() {
        return Vec::new();
    }
    // Dates as written, YYYY-MM-DD, compare as strings.
    vec![
        (
            "adaptive_thinking",
            Value::Bool(released >= CLAUDE_ADAPTIVE_FROM),
        ),
        (
            "thinking_binding",
            Value::Bool(released >= CLAUDE_BINDING_FROM),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::ModelFacts;
    use crate::catalog;

    fn of(provider: &str, model: &str) -> ModelFacts {
        let held = catalog::shipped();
        let entry = catalog::entry(&held, provider, model).expect("in the shipped catalog");
        ModelFacts::of(Some(entry), None)
    }

    #[test]
    fn the_newest_claude_thinks_adaptively_binds_and_refuses_sampling() {
        let facts = of("anthropic", "claude-sonnet-5-5");
        assert!(facts.adaptive_thinking && facts.thinking_binding && !facts.sampling);
        assert_eq!(facts.output_limit, Some(128_000));
        assert_eq!(facts.efforts, ["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(
            of("amazon-bedrock", "us.anthropic.claude-sonnet-5-5"),
            facts
        );
    }

    #[test]
    fn the_four_series_is_told_apart_by_release() {
        let opus_4_8 = of("anthropic", "claude-opus-4-8");
        assert!(opus_4_8.adaptive_thinking && !opus_4_8.thinking_binding && !opus_4_8.sampling);
        let opus_4_6 = of("anthropic", "claude-opus-4-6");
        assert!(opus_4_6.adaptive_thinking && opus_4_6.sampling);
        assert_eq!(opus_4_6.efforts, ["low", "medium", "high", "max"]);
        // Effort levels, but thinking by budget: not adaptive.
        assert!(!of("anthropic", "claude-opus-4-5").adaptive_thinking);
    }

    #[test]
    fn an_override_wins_over_the_entry_and_the_family() {
        let held = catalog::shipped();
        let entry = catalog::entry(&held, "anthropic", "claude-opus-4-8");
        let over = serde_json::json!({"thinking_binding": true, "limit": {"output": 32000}});
        let facts = ModelFacts::of(entry, Some(&over));
        assert!(facts.thinking_binding);
        assert_eq!(facts.output_limit, Some(32_000));
    }

    #[test]
    fn a_model_nobody_described_is_sent_what_it_was_asked() {
        assert_eq!(ModelFacts::of(None, None), ModelFacts::default());
    }
}
