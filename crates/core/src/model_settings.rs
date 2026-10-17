//! The settings that belong to a model, resolved for the model about to be
//! asked. See docs/models.md, "Settings belong to a model".
//!
//! Here rather than with the rest of the settings, in the API, because the
//! model is not known until the gateway is walking a chain: a turn that falls
//! from Sonnet to Gemini sends Gemini's temperature, which only the tier
//! choosing Gemini can look up. The API reads the same rows the same way for
//! what it decides before the turn -- the conversation's size, and whether the
//! prompt is repeated.
//!
//! The cascade is the settings cascade with the model in it. At each level a
//! value chosen for this model beats one chosen for every model, and a nearer
//! level beats a farther: agent and model, agent, workspace and model,
//! workspace, platform and model, platform -- and a template's fixed value
//! over all of them. A value that holds for every model but not this one -- a
//! temperature, for a model that takes none -- is passed over for this model,
//! and the next one down is used.

use std::collections::HashMap;

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::facts::ModelFacts;

/// The settings that are about a model, and so may be set per model.
pub const PER_MODEL: &[&str] = &["temperature", "reasoning_effort", "context_budget"];

/// Where the operator's own rows are kept: the platform's workspace.
pub const PLATFORM_WORKSPACE: Uuid = Uuid::from_u128(1);

/// The catalog's defaults for these three, here so both tiers agree.
pub const DEFAULT_REASONING_EFFORT: &str = "low";
pub const DEFAULT_CONTEXT_BUDGET: u64 = 400_000;

/// The `reasoning_effort` value that turns thinking off and repeats the
/// turn's prompt to the model. Never reaches a provider.
pub const REPEAT_PROMPT: &str = "none_repeat_prompt";

/// The version of its template an agent runs: pinned where allowed, else the
/// newest. A join from `agents a`, giving `v`.
pub const RUNNING_VERSION: &str = "join agent_templates t on t.id = a.template_id \
     join lateral (select * from agent_template_versions tv \
                    where tv.template_id = a.template_id \
                      and (not t.allow_pinning or a.template_version_id is null \
                           or tv.id = a.template_version_id) \
                    order by tv.ordinal desc limit 1) v on true";

/// What one model is asked with.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelSettings {
    pub temperature: Option<f32>,
    /// Never `none_repeat_prompt`: that is `none` here and `repeat_prompt`.
    pub reasoning_effort: String,
    pub repeat_prompt: bool,
    pub context_budget: u64,
}

/// Whether `value` of `key` means anything to a model with these facts.
pub fn valid_for(key: &str, value: &Value, facts: &ModelFacts) -> bool {
    match key {
        // Unset is always valid: it leaves the choice to the provider.
        "temperature" => value.is_null() || facts.sampling,
        "reasoning_effort" => match value.as_str() {
            // Off is something any model can be asked; one that cannot stop
            // thinking is asked for its least instead.
            Some("none") | Some(REPEAT_PROMPT) => true,
            // A model nobody described the levels of is sent what was chosen.
            Some(level) => facts.efforts.is_empty() || facts.efforts.iter().any(|e| e == level),
            None => false,
        },
        // Bytes against tokens: four to a token is generous for any text, so
        // only a budget no model could hold is refused.
        "context_budget" => match (value.as_u64(), facts.context_window) {
            (Some(bytes), Some(tokens)) => bytes <= u64::from(tokens) * 4,
            (Some(_), None) => true,
            _ => false,
        },
        _ => true,
    }
}

/// Resolves the three for `model` -- none for a model nobody configured --
/// as `workspace`'s `agent` would have them, where there is an agent.
pub async fn resolve(
    pool: &PgPool,
    workspace: Uuid,
    agent: Option<Uuid>,
    model: Option<Uuid>,
    facts: &ModelFacts,
) -> Result<ModelSettings, sqlx::Error> {
    let nil = Uuid::nil();
    let rows: Vec<(String, Value, i32)> = sqlx::query_as(
        "select key, value, \
                case when workspace_id = $1 and agent_id = $5 then 4 \
                     when agent_id = $5 then 2 else 0 end \
                + case when model_id = $5 then 1 else 0 end as rank \
         from setting_overrides \
         where key = any($4) \
           and model_id in ($5, $6) \
           and ((workspace_id = $1 and agent_id = $5) \
                or (workspace_id = $2 and agent_id = $5) \
                or (workspace_id = $2 and agent_id = $3))",
    )
    .bind(PLATFORM_WORKSPACE)
    .bind(workspace)
    .bind(agent.unwrap_or(nil))
    .bind(PER_MODEL)
    .bind(nil)
    .bind(model.unwrap_or(nil))
    .fetch_all(pool)
    .await?;

    let fixed: Option<Value> = match agent {
        Some(agent) => {
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "select v.settings from agents a {RUNNING_VERSION} where a.id = $1"
            )))
            .bind(agent)
            .fetch_optional(pool)
            .await?
        }
        None => None,
    };

    Ok(cascade(rows, fixed.as_ref(), facts))
}

/// The walk itself, over rows ranked as `resolve` ranks them: the platform's
/// value for every model is 5, the agent's for this model 0.
fn cascade(
    rows: Vec<(String, Value, i32)>,
    fixed: Option<&Value>,
    facts: &ModelFacts,
) -> ModelSettings {
    let mut by_key: HashMap<&str, Vec<(i32, Value)>> = HashMap::new();
    for (key, value, rank) in rows {
        if let Some(known) = PER_MODEL.iter().find(|k| **k == key) {
            by_key.entry(known).or_default().push((rank, value));
        }
    }
    let pick = |key: &str| -> Option<Value> {
        // A template's value wins wherever it is valid.
        if let Some(value) = fixed.and_then(|f| f.get(key))
            && valid_for(key, value, facts)
        {
            return Some(value.clone());
        }
        let mut found = by_key.get(key).cloned().unwrap_or_default();
        found.sort_by_key(|(rank, _)| *rank);
        found
            .into_iter()
            .map(|(_, value)| value)
            .find(|value| valid_for(key, value, facts))
    };
    let effort = pick("reasoning_effort")
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| DEFAULT_REASONING_EFFORT.to_string());
    ModelSettings {
        temperature: pick("temperature")
            .and_then(|v| v.as_f64())
            .map(|t| t as f32),
        repeat_prompt: effort == REPEAT_PROMPT,
        reasoning_effort: if effort == REPEAT_PROMPT {
            "none".to_string()
        } else {
            effort
        },
        context_budget: pick("context_budget")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_CONTEXT_BUDGET),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sonnet() -> ModelFacts {
        ModelFacts {
            sampling: false,
            efforts: ["low", "medium", "high", "xhigh", "max"]
                .map(String::from)
                .to_vec(),
            context_window: Some(1_000_000),
            ..ModelFacts::default()
        }
    }

    fn rows(rows: &[(&str, Value, i32)]) -> Vec<(String, Value, i32)> {
        rows.iter()
            .map(|(k, v, r)| (k.to_string(), v.clone(), *r))
            .collect()
    }

    #[test]
    fn nothing_set_is_the_catalogs_defaults() {
        let s = cascade(Vec::new(), None, &sonnet());
        assert_eq!(s.reasoning_effort, DEFAULT_REASONING_EFFORT);
        assert_eq!(s.temperature, None);
        assert_eq!(s.context_budget, DEFAULT_CONTEXT_BUDGET);
    }

    /// Nearer wins, and at one level the model's own row beats the row for
    /// every model.
    #[test]
    fn the_nearest_level_wins_and_the_model_wins_within_it() {
        let s = cascade(
            rows(&[
                ("reasoning_effort", json!("high"), 5),
                ("reasoning_effort", json!("medium"), 2),
                ("reasoning_effort", json!("max"), 3),
            ]),
            None,
            &sonnet(),
        );
        assert_eq!(s.reasoning_effort, "medium", "workspace's for this model");
    }

    /// A temperature for every model is passed over for one that takes none,
    /// rather than sent and dropped -- and a lower level's valid value used.
    #[test]
    fn a_value_this_model_refuses_falls_through() {
        let rows = rows(&[
            ("temperature", json!(0.3), 1),
            ("temperature", json!(null), 4),
            ("reasoning_effort", json!("xhigh"), 2),
        ]);
        let gemini = ModelFacts {
            efforts: vec!["low".into(), "high".into()],
            ..ModelFacts::default()
        };
        let on_sonnet = cascade(rows.clone(), None, &sonnet());
        assert_eq!(on_sonnet.temperature, None);
        assert_eq!(on_sonnet.reasoning_effort, "xhigh");
        let on_gemini = cascade(rows, None, &gemini);
        assert_eq!(on_gemini.temperature, Some(0.3));
        assert_eq!(on_gemini.reasoning_effort, DEFAULT_REASONING_EFFORT);
    }

    #[test]
    fn a_template_wins_where_it_is_valid() {
        let rows = rows(&[("reasoning_effort", json!("high"), 0)]);
        let fixed = json!({"reasoning_effort": REPEAT_PROMPT, "temperature": 0.2});
        let s = cascade(rows, Some(&fixed), &sonnet());
        assert!(s.repeat_prompt);
        assert_eq!(s.reasoning_effort, "none");
        assert_eq!(
            s.temperature, None,
            "Sonnet takes no temperature, fixed or not"
        );
    }
}
