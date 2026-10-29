//! The settings catalog and the cascade that resolves it.
//!
//! A setting is defined once, here: its key, its type, its default and who may
//! override it. Values that differ from the default are rows, one per level
//! that has chosen to differ -- operator, workspace, agent -- and resolution
//! walks agent, workspace, operator, default. A row's existence is the override
//! toggle; deleting it is turning the toggle off. See docs/settings.md.
//!
//! A catalog in code rather than a free key-value store, because the
//! catalog is what keeps the settings page honest about types, defaults
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
    /// The operator's value is a ceiling: a workspace may choose something
    /// stricter and never something looser, so an operator who turns a
    /// feature off turns it off everywhere, and one who leaves it on leaves
    /// each workspace free to turn it off for itself. Not set per agent --
    /// these are about the people using the product, not the agents in it.
    /// "Stricter" is earlier in the choice's options, which are listed
    /// strictest first.
    OperatorCeiling,
}

/// The stricter of two values of a ceiling setting: the one earlier in its
/// options. A value that is not among them -- a row written before an option
/// was removed -- loses to one that is.
pub fn stricter(
    setting: &Setting,
    a: &serde_json::Value,
    b: &serde_json::Value,
) -> serde_json::Value {
    let rank = |v: &serde_json::Value| match &setting.kind {
        Kind::Choice { options } => v
            .as_str()
            .and_then(|s| options.iter().position(|o| o.value == s))
            .unwrap_or(usize::MAX),
        _ => usize::MAX,
    };
    if rank(b) < rank(a) {
        b.clone()
    } else {
        a.clone()
    }
}

/// What kind of value a setting takes, for validation and for the control
/// the page draws.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Kind {
    /// A number in a range, or null meaning "let the provider decide" where
    /// `nullable` is set.
    Number {
        min: f64,
        max: f64,
        step: f64,
        nullable: bool,
    },
    Integer {
        min: i64,
        max: i64,
    },
    /// One of a fixed set of strings, each with a human-readable label.
    Choice {
        options: &'static [ChoiceOption],
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct ChoiceOption {
    pub value: &'static str,
    pub label: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Setting {
    pub key: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub kind: Kind,
    pub default: serde_json::Value,
    pub owner: Owner,
    /// About a model, and so may be chosen for one model as well as for all
    /// of them (`model_settings::PER_MODEL`).
    pub per_model: bool,
    /// For an operator-only setting a deployment may hand to workspaces: the
    /// operator-only setting that says so, and the value of it that does.
    /// While it says so the setting is workspace-overridable -- per
    /// workspace, never per agent. Who may set something is the deployment's
    /// choice here rather than the catalog's: a plan sold per workspace wants
    /// the operator to set it, storage sold by use wants the workspace to.
    pub delegated_by: Option<(&'static str, &'static str)>,
}

impl Setting {
    /// Who may set it below the operator, now: `owner`, unless the operator
    /// has handed it to workspaces. `delegating` is the operator's value of
    /// the setting that would.
    pub fn owner_given(&self, delegating: Option<&serde_json::Value>) -> Owner {
        match (self.delegated_by, delegating) {
            (Some((_, on)), Some(v)) if v.as_str() == Some(on) => Owner::WorkspaceOverridable,
            _ => self.owner,
        }
    }
}

/// The `reasoning_effort` value that turns thinking off and repeats the
/// turn's prompt to the model. Never reaches a provider: it resolves to
/// "none" and `Resolved::repeat_prompt`.
pub const REPEAT_PROMPT: &str = outturn_core::model_settings::REPEAT_PROMPT;

/// Every setting there is.
pub fn catalog() -> Vec<Setting> {
    let mut all = settings();
    for setting in &mut all {
        setting.per_model = outturn_core::model_settings::PER_MODEL.contains(&setting.key);
    }
    all
}

fn settings() -> Vec<Setting> {
    vec![
        Setting {
            key: "temperature",
            label: "Temperature",
            description: "How much the model varies its wording. Lower is more \
                          predictable; higher is more varied. Unset leaves it to the \
                          provider.",
            kind: Kind::Number {
                min: 0.0,
                max: 2.0,
                step: 0.1,
                nullable: true,
            },
            default: serde_json::Value::Null,
            owner: Owner::WorkspaceOverridable,
            per_model: false,
            delegated_by: None,
        },
        Setting {
            key: "reasoning_effort",
            label: "Thinking before answering",
            description: "How much the model reasons before answering, where the \
                          provider supports it. Higher does better on hard questions \
                          and replies more slowly. Off makes most models noticeably \
                          less capable.",
            kind: Kind::Choice {
                options: &[
                    ChoiceOption {
                        value: "none",
                        label: "Off",
                    },
                    // Off, and the turn's prompt sent twice. Repetition is
                    // what a model that does not deliberate gets instead: it
                    // reads the question a second time with all of it in view.
                    ChoiceOption {
                        value: REPEAT_PROMPT,
                        label: "Off, with prompt repetition",
                    },
                    ChoiceOption {
                        value: "low",
                        label: "Low",
                    },
                    ChoiceOption {
                        value: "medium",
                        label: "Medium",
                    },
                    ChoiceOption {
                        value: "high",
                        label: "High",
                    },
                ],
            },
            default: serde_json::json!(outturn_core::model_settings::DEFAULT_REASONING_EFFORT),
            owner: Owner::WorkspaceOverridable,
            per_model: false,
            delegated_by: None,
        },
        // One ordered choice per scope rather than a read flag beside a write
        // flag. Prevents strange combinations, such as write but not read.
        Setting {
            key: "agent_file_access",
            label: "Agent files",
            description: "What an agent may do under agent/, the files it keeps between \
                          conversations.",
            kind: Kind::Choice {
                options: &[
                    ChoiceOption {
                        value: "none",
                        label: "No access",
                    },
                    ChoiceOption {
                        value: "read",
                        label: "Read only",
                    },
                    ChoiceOption {
                        value: "read_write",
                        label: "Read and write",
                    },
                ],
            },
            default: serde_json::json!("read_write"),
            owner: Owner::WorkspaceOverridable,
            per_model: false,
            delegated_by: None,
        },
        Setting {
            key: "workspace_file_access",
            label: "Workspace files",
            description: "What an agent may do under workspace/, the files the whole \
                          workspace shares.",
            kind: Kind::Choice {
                options: &[
                    ChoiceOption {
                        value: "none",
                        label: "No access",
                    },
                    ChoiceOption {
                        value: "read",
                        label: "Read only",
                    },
                    ChoiceOption {
                        value: "read_write",
                        label: "Read and write",
                    },
                ],
            },
            default: serde_json::json!("read"),
            owner: Owner::WorkspaceOverridable,
            per_model: false,
            delegated_by: None,
        },
        // A ceiling rather than an addition, which is why it is a setting and not
        // a declaration in a skill. The per-operation gates in docs/approvals.md
        // are opt-in by whoever documents an operation, and that is right for them:
        // a skill adding a rule about its own endpoint can only ever make the
        // platform stricter. This one is the other direction -- it has to hold for
        // an agent with no skills at all, and a workspace must not be able to
        // escape it by publishing a skill that omits a line.
        Setting {
            key: "approve_new_hosts",
            label: "Approve new hosts",
            description: "Whether reaching a host on the public internet needs a \
                          person's approval the first time an agent asks for it in a \
                          conversation. Hosts declared in skills are already allowed.",
            // A choice rather than a boolean, which the catalog has no kind for
            // anyway -- and the naming is the better reason. "off"/"on" would leave
            // a reader guessing what is on; these say what happens.
            kind: Kind::Choice {
                options: &[
                    ChoiceOption {
                        value: "reach_freely",
                        label: "Reach freely",
                    },
                    ChoiceOption {
                        value: "approve_new_hosts",
                        label: "Ask for approval",
                    },
                ],
            },
            default: serde_json::json!("reach_freely"),
            owner: Owner::WorkspaceOverridable,
            per_model: false,
            delegated_by: None,
        },
        Setting {
            key: "max_tool_rounds",
            label: "Model calls per turn",
            description: "How many times one turn may call the model before it is \
                          stopped. Zero means no limit.",
            kind: Kind::Integer {
                min: 0,
                max: 10_000,
            },
            default: serde_json::json!(100),
            owner: Owner::WorkspaceOverridable,
            per_model: false,
            delegated_by: None,
        },
        Setting {
            key: "max_written_bytes",
            label: "File writes per turn",
            description: "How many bytes one turn may write to files. Zero means no \
                          limit.",
            kind: Kind::Integer {
                min: 0,
                max: 1 << 40,
            },
            default: serde_json::json!(1u64 << 30),
            owner: Owner::WorkspaceOverridable,
            per_model: false,
            delegated_by: None,
        },
        Setting {
            key: "max_stored_bytes",
            label: "Storage",
            description: "How many bytes of files a workspace may hold, session \
                          files included. Past it, files can still be read and \
                          deleted, but not written. Zero means no limit.",
            kind: Kind::Integer {
                min: 0,
                max: 1 << 50,
            },
            default: serde_json::json!(0),
            owner: Owner::OperatorOnly,
            per_model: false,
            delegated_by: Some(("stored_bytes_set_by", "workspace")),
        },
        Setting {
            key: "stored_bytes_set_by",
            label: "Who sets storage",
            description: "Whether each workspace's storage is set by the operator, \
                          or by the workspace's own admins.",
            kind: Kind::Choice {
                options: &[
                    ChoiceOption {
                        value: "operator",
                        label: "The operator",
                    },
                    ChoiceOption {
                        value: "workspace",
                        label: "Each workspace's admins",
                    },
                ],
            },
            default: serde_json::json!("operator"),
            owner: Owner::OperatorOnly,
            per_model: false,
            delegated_by: None,
        },
        Setting {
            key: "context_budget",
            label: "Conversation size sent to the model",
            description: "How much of a conversation is sent to the model, in bytes. \
                          Past it, the oldest tool results are dropped first. Unset, it \
                          follows the model's context window.",
            // Bytes rather than tokens, because nothing here counts tokens and
            // a per-model tokeniser is wrong for every model it was not built
            // for. Roughly three bytes to a token is the pessimistic end, so
            // this default is about 130k tokens against a 200k window --
            // conservative on purpose, since being wrong costs headroom while
            // the alternative costs the turn.
            kind: Kind::Integer {
                min: 1_000,
                max: 100_000_000,
            },
            default: serde_json::json!(outturn_core::model_settings::DEFAULT_CONTEXT_BUDGET),
            owner: Owner::WorkspaceOverridable,
            per_model: false,
            delegated_by: None,
        },
        Setting {
            key: "session_naming",
            label: "When to name a conversation",
            description: "When a conversation is given a title: after the first \
                          message, after the first reply, or after two exchanges. \
                          Later titles are more accurate.",
            kind: Kind::Choice {
                options: &[
                    ChoiceOption {
                        value: "after_message",
                        label: "After the first message",
                    },
                    ChoiceOption {
                        value: "after_first_turn",
                        label: "After the first reply",
                    },
                    ChoiceOption {
                        value: "after_second_turn",
                        label: "After two exchanges",
                    },
                ],
            },
            default: serde_json::json!("after_first_turn"),
            owner: Owner::WorkspaceOverridable,
            per_model: false,
            delegated_by: None,
        },
        // The browser's own speech recognition, which in most browsers sends
        // the audio to the browser's maker to transcribe. On by default, since
        // that custodian already sees everything typed into the page; a
        // ceiling, so a deployment or a workspace whose policy forbids it can
        // turn it off, and a workspace cannot turn back on what the operator
        // turned off.
        Setting {
            key: "dictation",
            label: "Dictation",
            description: "A microphone in the message box, using the browser's own speech \
                          recognition. Most browsers send the audio to their maker to \
                          transcribe.",
            kind: Kind::Choice {
                options: &[
                    ChoiceOption {
                        value: "off",
                        label: "Off",
                    },
                    ChoiceOption {
                        value: "on",
                        label: "On",
                    },
                ],
            },
            default: serde_json::json!("on"),
            owner: Owner::OperatorCeiling,
            per_model: false,
            delegated_by: None,
        },
        // How far a workspace's auto-approval policies may reach, by the risk an
        // operation declares (docs/auto-approval.md). A ceiling, so a deployment
        // opts in once and a workspace may only narrow it; `none` at the
        // operator by default, so nothing is waved through until somebody
        // decided it may be.
        Setting {
            key: "auto_approve_max_risk",
            label: "Auto-approval",
            description: "The highest risk an auto-approval policy may cover, as each \
                          operation declares its own. None means every gated request \
                          asks a person.",
            kind: Kind::Choice {
                options: &[
                    ChoiceOption {
                        value: "none",
                        label: "None",
                    },
                    ChoiceOption {
                        value: "low",
                        label: "Low risk",
                    },
                    ChoiceOption {
                        value: "medium",
                        label: "Medium risk or lower",
                    },
                    ChoiceOption {
                        value: "high",
                        label: "Any risk",
                    },
                ],
            },
            default: serde_json::json!("none"),
            owner: Owner::OperatorCeiling,
            per_model: false,
            delegated_by: None,
        },
    ]
}

pub fn find(key: &str) -> Option<Setting> {
    catalog().into_iter().find(|s| s.key == key)
}

/// Where a resolved value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Default,
    Operator,
    Workspace,
    Agent,
    /// Fixed by the template the agent was made from. Wins over the workspace
    /// and the agent, which is what fixing a value means.
    Template,
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
    /// the levels above. Shown grayed beside the toggle so turning it off has
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
    /// Send the turn's own prompt to the model twice. Only what is sent: the
    /// stored message stays as typed. See `worker::repeated`.
    pub repeat_prompt: bool,
    pub max_tool_rounds: u32,
    /// Bytes one turn may write to files; zero for no limit.
    pub max_written_bytes: u64,
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
    /// Whether a host on the public internet needs approving the first time a
    /// turn asks for it. See the setting's own description for what is exempt.
    pub approve_new_hosts: bool,
    /// When the platform names an untitled conversation: after the first user
    /// message, after the first turn, or after the second.
    pub session_naming: String,
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
    ///
    /// With a model, as that model sees it: its own values over those for
    /// every model, level by level.
    async fn view(
        &self,
        level: Level,
        model: Option<Uuid>,
    ) -> Result<Vec<Effective>, SettingsError>;
    /// Turns the override on at `level` with `value`, validated against the
    /// catalog -- and, for one model, against what that model accepts. The
    /// caller has already decided the level may.
    async fn set(
        &self,
        level: Level,
        model: Option<Uuid>,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), SettingsError>;
    /// Turns the override off at `level`.
    async fn clear(
        &self,
        level: Level,
        model: Option<Uuid>,
        key: &str,
    ) -> Result<(), SettingsError>;
    /// The values a turn of `agent_id` in `workspace_id` runs with.
    async fn resolve(&self, workspace_id: Uuid, agent_id: Uuid) -> Result<Resolved, SettingsError>;
}

/// Checks a value against a setting's kind.
pub fn validate(setting: &Setting, value: &serde_json::Value) -> Result<(), SettingsError> {
    match &setting.kind {
        Kind::Number {
            min, max, nullable, ..
        } => {
            if value.is_null() {
                return if *nullable {
                    Ok(())
                } else {
                    Err(SettingsError::Invalid(format!(
                        "{} needs a number",
                        setting.label
                    )))
                };
            }
            let n = value.as_f64().ok_or_else(|| {
                SettingsError::Invalid(format!("{} needs a number", setting.label))
            })?;
            if n < *min || n > *max {
                return Err(SettingsError::Invalid(format!(
                    "{} must be between {min} and {max}",
                    setting.label
                )));
            }
            Ok(())
        }
        Kind::Integer { min, max } => {
            let n = value.as_i64().ok_or_else(|| {
                SettingsError::Invalid(format!("{} needs a whole number", setting.label))
            })?;
            if n < *min || n > *max {
                return Err(SettingsError::Invalid(format!(
                    "{} must be between {min} and {max}",
                    setting.label
                )));
            }
            Ok(())
        }
        Kind::Choice { options } => {
            let labels: Vec<&str> = options.iter().map(|o| o.label).collect();
            let s = value.as_str().ok_or_else(|| {
                SettingsError::Invalid(format!(
                    "{} needs one of {}",
                    setting.label,
                    labels.join(", ")
                ))
            })?;
            if !options.iter().any(|o| o.value == s) {
                return Err(SettingsError::Invalid(format!(
                    "{} must be one of {}",
                    setting.label,
                    labels.join(", ")
                )));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The help button beside a setting opens `settings/#<key>`, so every
    /// setting needs a section there with its key as the anchor; one added
    /// without would open the page at its top.
    #[test]
    fn every_setting_has_a_section_in_the_docs() {
        let docs = include_str!("../../../../docs/settings.md");
        for s in catalog() {
            assert!(
                docs.contains(&format!("{{#{}}}", s.key)),
                "docs/settings.md has no section headed {{#{}}}",
                s.key
            );
        }
    }

    #[test]
    fn defaults_satisfy_their_own_kinds() {
        for s in catalog() {
            validate(&s, &s.default).unwrap_or_else(|e| panic!("{}: {e}", s.key));
        }
    }

    #[test]
    fn values_outside_a_kind_are_refused() {
        let t = find("temperature").expect("temperature");
        assert!(validate(&t, &serde_json::json!(0.5)).is_ok());
        assert!(
            validate(&t, &serde_json::Value::Null).is_ok(),
            "temperature may be unset"
        );
        assert!(validate(&t, &serde_json::json!(3.0)).is_err());
        assert!(validate(&t, &serde_json::json!("hot")).is_err());

        let e = find("reasoning_effort").expect("effort");
        assert!(validate(&e, &serde_json::json!("high")).is_ok());
        assert!(validate(&e, &serde_json::json!("maximum")).is_err());
        assert!(validate(&e, &serde_json::json!(REPEAT_PROMPT)).is_ok());

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
        let json = serde_json::to_value(effective(None)).expect("serializes");
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
        let json =
            serde_json::to_value(effective(Some(serde_json::Value::Null))).expect("serializes");
        assert!(
            json.get("override_value").is_some(),
            "an override to null vanished: {json}"
        );
        assert!(json["override_value"].is_null());
    }

    #[test]
    fn an_ordinary_override_is_sent_as_its_value() {
        let json =
            serde_json::to_value(effective(Some(serde_json::json!(0.7)))).expect("serializes");
        assert_eq!(json["override_value"], 0.7);
    }
}
