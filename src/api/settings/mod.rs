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
                          it. Better on hard questions and slower to reply. \"none\" is \
                          cheapest but not safe on every model: gemma4 with thinking \
                          off answers a tool result with nothing at all, so the turn \
                          ends on the tool and the reader is told nothing.",
            kind: Kind::Choice { options: &["none", "low", "medium", "high"] },
            // Low rather than none. Measured on gemma4 after a file listing:
            // none gives an empty reply (1 completion token); low gives the
            // one-line summary for 60. The silence is what the reader sees.
            default: serde_json::json!("low"),
            owner: Owner::WorkspaceOverridable,
        },
        // One ordered choice per scope rather than a read flag beside a write
        // flag. "May write but not read" is not a thing anyone means, and a
        // pair of booleans invites somebody to configure it and believe it:
        // the extraction a write triggers would read the file back regardless.
        // Ordering the options is what makes that unsayable.
        Setting {
            key: "agent_file_access",
            label: "Agent files",
            description: "What an agent may do under agent/, the files it keeps between \
                          conversations. Session files are always read/write; they are \
                          the agent's scratch space.",
            kind: Kind::Choice { options: &["none", "read", "read_write"] },
            default: serde_json::json!("read_write"),
            owner: Owner::WorkspaceOverridable,
        },
        Setting {
            key: "workspace_file_access",
            label: "Workspace files",
            description: "What an agent may do under workspace/, the files the whole \
                          workspace shares. Read by default: a prompt that talks an \
                          agent into overwriting shared reference material should find \
                          it cannot, and an agent with no business reading that \
                          material at all can be given none.",
            kind: Kind::Choice { options: &["none", "read", "read_write"] },
            default: serde_json::json!("read"),
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
        Setting {
            key: "context_budget",
            label: "Conversation size sent to the model",
            description: "How much of a conversation is sent, in bytes. A long \
                          session outgrows any model's context, and this is what \
                          decides when the oldest tool results start being dropped \
                          to make room. Set it well under the model's real window: \
                          the reply, tool results arriving mid-turn and the request \
                          itself all need space that this does not count.",
            // Bytes rather than tokens, because nothing here counts tokens and
            // a per-model tokeniser is wrong for every model it was not built
            // for. Roughly three bytes to a token is the pessimistic end, so
            // this default is about 130k tokens against a 200k window --
            // conservative on purpose, since being wrong costs headroom while
            // the alternative costs the turn.
            kind: Kind::Integer { min: 1_000, max: 100_000_000 },
            default: serde_json::json!(400_000),
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
    ///
    /// Omitted entirely when there is no row, rather than sent as null. A
    /// nullable setting can be overridden *to* null -- temperature unset is a
    /// real choice, meaning "leave it to the provider" -- so null on the wire
    /// has to mean that and nothing else. Sending it for an absent row made
    /// every setting on a new agent look overridden, and unchecking the box
    /// deleted a row that was never there and changed nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
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
    /// How many bytes of conversation may be sent, before the trim starts
    /// dropping the oldest tool results to fit.
    pub context_budget: usize,
    /// Storage scopes the agent may write: always "session", plus whichever
    /// of "agent" and "workspace" the cascade allows.
    pub write_scopes: Vec<String>,
    /// Storage scopes the agent may read. Always a superset of `write_scopes`:
    /// the vocabulary has no way to say "write but not read", and a write
    /// triggers an extraction that reads the file back anyway.
    pub read_scopes: Vec<String>,
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

#[cfg(test)]
mod wire_tests {
    use super::*;

    fn effective(override_value: Option<serde_json::Value>) -> Effective {
        Effective {
            setting: find("temperature").expect("temperature"),
            value: serde_json::Value::Null,
            source: Source::Default,
            override_value,
            inherited: serde_json::Value::Null,
        }
    }

    #[test]
    fn an_absent_override_is_not_on_the_wire_at_all() {
        // The UI reads absence as "not overridden". Sending null for a row
        // that does not exist made every setting on a new agent look
        // overridden, and unchecking the box deleted nothing and changed
        // nothing -- the box stayed on because the field was still there.
        let json = serde_json::to_value(effective(None)).expect("serialises");
        assert!(
            json.get("override_value").is_none(),
            "an absent override was sent anyway: {json}"
        );
    }

    #[test]
    fn an_override_to_null_is_still_an_override() {
        // Temperature is nullable, and unset is a real choice meaning "leave
        // it to the provider". So null on the wire has to mean that, which is
        // why absence is what says there is no row.
        let json = serde_json::to_value(effective(Some(serde_json::Value::Null)))
            .expect("serialises");
        assert!(
            json.get("override_value").is_some(),
            "an override to null vanished: {json}"
        );
        assert!(json["override_value"].is_null());
    }

    #[test]
    fn an_ordinary_override_is_sent_as_its_value() {
        let json = serde_json::to_value(effective(Some(serde_json::json!(0.7))))
            .expect("serialises");
        assert_eq!(json["override_value"], 0.7);
    }
}
