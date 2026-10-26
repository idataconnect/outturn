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

/// Bytes of conversation a model's window holds with room to spare: two to a
/// token. Text runs three to four bytes a token, so this is about two thirds of
/// the window, and the rest is for what the budget does not count -- the
/// reply, tool results arriving mid-turn, the request's own framing.
pub const BYTES_PER_WINDOW_TOKEN: u64 = 2;

/// The largest conversation a model with this window should be sent, in
/// bytes; `None` where nobody said how large its window is.
pub fn largest_budget(facts: &ModelFacts) -> Option<u64> {
    facts
        .context_window
        .map(|tokens| u64::from(tokens) * BYTES_PER_WINDOW_TOKEN)
}

/// The budget when nobody chose one: the catalog's default, or less for a
/// model whose window would not hold that. A 32k-token model sent the default
/// was a turn refused for its length, or a conversation cut by the provider.
pub fn default_budget(facts: &ModelFacts) -> u64 {
    largest_budget(facts).map_or(DEFAULT_CONTEXT_BUDGET, |most| {
        most.min(DEFAULT_CONTEXT_BUDGET)
    })
}

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
        // Bytes against tokens, with the window's room for the reply kept:
        // four to a token once passed here, which on code or JSON filled the
        // window and left the reply nowhere to go.
        "context_budget" => match (value.as_u64(), largest_budget(facts)) {
            (Some(bytes), Some(most)) => bytes <= most,
            (Some(_), None) => true,
            _ => false,
        },
        _ => true,
    }
}

/// The nearest value of `key` a model with these facts takes, for a value
/// it refuses: the closest effort level it offers, the lower on a tie; no
/// temperature; the largest budget it could hold.
pub fn adjust(key: &str, value: &Value, facts: &ModelFacts) -> Value {
    match key {
        "temperature" => Value::Null,
        "reasoning_effort" => {
            const ORDER: &[&str] = &["low", "medium", "high", "xhigh", "max"];
            let at = |level: &str| ORDER.iter().position(|l| *l == level);
            let asked = value.as_str().and_then(at).unwrap_or(0);
            facts
                .efforts
                .iter()
                .filter_map(|e| at(e).map(|i| (i.abs_diff(asked), i, e)))
                .min_by_key(|(distance, i, _)| (*distance, *i))
                .map_or(Value::from("none"), |(_, _, e)| Value::from(e.as_str()))
        }
        "context_budget" => largest_budget(facts).map_or_else(|| value.clone(), Value::from),
        _ => value.clone(),
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
            .unwrap_or_else(|| default_budget(facts)),
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

    /// Unset, the budget follows the model: a window too small for the
    /// default gets two bytes a token of it, a large one the default, and one
    /// nobody described the default.
    #[test]
    fn an_unset_budget_follows_the_models_window() {
        let window = |tokens| ModelFacts {
            context_window: tokens,
            ..ModelFacts::default()
        };
        let budget = |facts: &ModelFacts| cascade(Vec::new(), None, facts).context_budget;
        assert_eq!(budget(&window(Some(32_768))), 65_536);
        assert_eq!(budget(&window(Some(1_000_000))), DEFAULT_CONTEXT_BUDGET);
        assert_eq!(budget(&window(None)), DEFAULT_CONTEXT_BUDGET);
        // A chosen budget the window cannot hold is passed over, here for the
        // default the window allows.
        let chosen = rows(&[("context_budget", json!(300_000), 5)]);
        assert_eq!(
            cascade(chosen, None, &window(Some(32_768))).context_budget,
            65_536
        );
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
    fn a_refused_value_is_moved_to_the_nearest_one_taken() {
        let two = ModelFacts {
            efforts: vec!["low".into(), "high".into()],
            context_window: Some(1000),
            ..ModelFacts::default()
        };
        assert_eq!(
            adjust("reasoning_effort", &json!("medium"), &two),
            json!("low")
        );
        assert_eq!(
            adjust("reasoning_effort", &json!("max"), &two),
            json!("high")
        );
        assert_eq!(adjust("temperature", &json!(0.4), &two), Value::Null);
        assert_eq!(adjust("context_budget", &json!(9000), &two), json!(2000));
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
