//! The settings catalogue and the cascade that resolves it.
//!
//! A setting is defined once, here: its key, its type, its default and who may
//! override it. Values that differ from the default are rows, one per level
//! that has chosen to differ -- operator, workspace, agent -- and resolution
//! walks agent, workspace, operator, default. A row's existence is the override
//! toggle; deleting it is turning the toggle off. See docs/settings.md.
//!
//! A catalogue in code rather than a free key-value store, because the
//! catalogue is what keeps the settings page honest about types, defaults
//! and who may touch what, and what lets a workspace who has never heard of
//! temperature see a value that is already right and a checkbox to leave
//! alone.

mod postgres;

use async_trait::async_trait;
use serde::Serialize;
use uuid::Uuid;

pub use postgres::PostgresSettingsStore;

/// Who may set a value for this setting below the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Owner {
    /// The operator sets it once for everyone. Workspaces see it, cannot change
    /// it.
    OperatorOnly,
    /// Workspaces may override, and agents within a workspace may override again.
    WorkspaceOverridable,
}

/// What kind of value a setting takes, for validation and for the control
/// the page draws.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Kind {
    /// A number in a range, or null meaning "let the provider decide" where
    /// `nullable` is set.
    Number { min: f64, max: f64, step: f64, nullable: bool },
    Integer { min: i64, max: i64 },
    /// One of a fixed set of strings.
    Choice { options: &'static [&'static str] },
}

#[derive(Debug, Clone, Serialize)]
pub struct Setting {
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub kind: Kind,
    pub default: serde_json::Value,
    pub owner: Owner,
}

/// Every setting there is.
pub fn catalogue() -> Vec<Setting> {
    vec![
        Setting {
            key: "temperature",
            label: "Temperature",
            description: "How much the model varies its wording. Lower is more \
                          predictable, which suits agents that follow procedures; \
                          higher is more varied. Unset leaves it to the provider.",
            kind: Kind::Number { min: 0.0, max: 2.0, step: 0.1, nullable: true },
            default: serde_json::Value::Null,
            owner: Owner::WorkspaceOverridable,
        },
        Setting {
            key: "reasoning_effort",
            label: "Thinking before answering",
            description: "How much the model deliberates where the provider supports \
                          it. Better on hard questions and slower to reply; on a local \
                          model, a few dozen characters of answer has been measured \
                          costing three hundred tokens of thought first.",
            kind: Kind::Choice { options: &["none", "low", "medium", "high"] },
            default: serde_json::json!("none"),
            owner: Owner::WorkspaceOverridable,
        },
        Setting {
            key: "agent_writes_agent_files",
            label: "Agents may write agent files",
            description: "Whether an agent may write under agent/, the files it keeps \
                          between conversations. Session files are always writable; \
                          they are the agent's scratch space.",
            kind: Kind::Choice { options: &["allow", "deny"] },
            default: serde_json::json!("allow"),
            owner: Owner::WorkspaceOverridable,
        },
        Setting {
            key: "agent_writes_workspace_files",
            label: "Agents may write workspace files",
            description: "Whether an agent may write under workspace/, the files the whole \
                          workspace shares. Off unless somebody decides otherwise: a \
                          prompt that talks an agent into overwriting shared reference \
                          material should find it cannot.",
            kind: Kind::Choice { options: &["allow", "deny"] },
            default: serde_json::json!("deny"),
            owner: Owner::WorkspaceOverridable,
        },
        Setting {
            key: "max_tool_rounds",
            label: "Model calls per turn",
            description: "How many times one turn may call the model before it is \
                          stopped. A runaway guard, not a budget: what costs money is \
                          tokens. Zero means no limit.",
            kind: Kind::Integer { min: 0, max: 10_000 },
            default: serde_json::json!(100),
            owner: Owner::WorkspaceOverridable,
        },
    ]
}

pub fn find(key: &str) -> Option<Setting> {
    catalogue().into_iter().find(|s| s.key == key)
}

/// Where a resolved value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Default,
    Operator,
    Workspace,
    Agent,
}

/// A setting as one level sees it: what applies, where it came from, and
/// whether this level has its own row.
#[derive(Debug, Clone, Serialize)]
pub struct Effective {
    #[serde(flatten)]
    pub setting: Setting,
    pub value: serde_json::Value,
    pub source: Source,
    /// The row at the level being viewed, if there is one. Present means the
    /// override toggle is on here.
    pub override_value: Option<serde_json::Value>,
    /// What this level would get if its own row were removed: the value from
    /// the levels above. Shown greyed beside the toggle so turning it off has
    /// no surprises.
    pub inherited: serde_json::Value,
}

/// The level a request is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Operator,
    Workspace(Uuid),
    Agent { workspace_id: Uuid, agent_id: Uuid },
}

/// What a turn runs with, after the walk.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub temperature: Option<f32>,
    pub reasoning_effort: Option<String>,
    pub max_tool_rounds: u32,
    /// Storage scopes the agent may write: always "session", plus whichever
    /// of "agent" and "workspace" the cascade allows.
    pub write_scopes: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("no setting named {0}")]
    Unknown(String),
    #[error("{0}")]
    Invalid(String),
    #[error("settings store error: {0}")]
    Internal(String),
}

#[async_trait]
pub trait SettingsStore: Send + Sync {
    /// Every setting as seen from `level`.
    async fn view(&self, level: Level) -> Result<Vec<Effective>, SettingsError>;
    /// Turns the override on at `level` with `value`, validated against the
    /// catalogue. The caller has already decided the level may.
    async fn set(&self, level: Level, key: &str, value: serde_json::Value) -> Result<(), SettingsError>;
    /// Turns the override off at `level`.
    async fn clear(&self, level: Level, key: &str) -> Result<(), SettingsError>;
    /// The values a turn of `agent_id` in `workspace_id` runs with.
    async fn resolve(&self, workspace_id: Uuid, agent_id: Uuid) -> Result<Resolved, SettingsError>;
}

/// Checks a value against a setting's kind.
pub fn validate(setting: &Setting, value: &serde_json::Value) -> Result<(), SettingsError> {
    match &setting.kind {
        Kind::Number { min, max, nullable, .. } => {
            if value.is_null() {
                return if *nullable {
                    Ok(())
                } else {
                    Err(SettingsError::Invalid(format!("{} needs a number", setting.label)))
                };
            }
            let n = value
                .as_f64()
                .ok_or_else(|| SettingsError::Invalid(format!("{} needs a number", setting.label)))?;
            if n < *min || n > *max {
                return Err(SettingsError::Invalid(format!(
                    "{} must be between {min} and {max}",
                    setting.label
                )));
            }
            Ok(())
        }
        Kind::Integer { min, max } => {
            let n = value
                .as_i64()
                .ok_or_else(|| SettingsError::Invalid(format!("{} needs a whole number", setting.label)))?;
            if n < *min || n > *max {
                return Err(SettingsError::Invalid(format!(
                    "{} must be between {min} and {max}",
                    setting.label
                )));
            }
            Ok(())
        }
        Kind::Choice { options } => {
            let s = value
                .as_str()
                .ok_or_else(|| SettingsError::Invalid(format!("{} needs one of {}", setting.label, options.join(", "))))?;
            if !options.contains(&s) {
                return Err(SettingsError::Invalid(format!(
                    "{} must be one of {}",
                    setting.label,
                    options.join(", ")
                )));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_satisfy_their_own_kinds() {
        for s in catalogue() {
            validate(&s, &s.default).unwrap_or_else(|e| panic!("{}: {e}", s.key));
        }
    }

    #[test]
    fn values_outside_a_kind_are_refused() {
        let t = find("temperature").expect("temperature");
        assert!(validate(&t, &serde_json::json!(0.5)).is_ok());
        assert!(validate(&t, &serde_json::Value::Null).is_ok(), "temperature may be unset");
        assert!(validate(&t, &serde_json::json!(3.0)).is_err());
        assert!(validate(&t, &serde_json::json!("hot")).is_err());

        let e = find("reasoning_effort").expect("effort");
        assert!(validate(&e, &serde_json::json!("high")).is_ok());
        assert!(validate(&e, &serde_json::json!("maximum")).is_err());

        let r = find("max_tool_rounds").expect("rounds");
        assert!(validate(&r, &serde_json::json!(0)).is_ok());
        assert!(validate(&r, &serde_json::json!(-1)).is_err());
        assert!(validate(&r, &serde_json::json!(2.5)).is_err());
    }
}
