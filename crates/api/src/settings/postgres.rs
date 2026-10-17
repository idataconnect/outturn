use std::collections::HashMap;

use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use super::{Effective, Level, Resolved, SettingsError, SettingsStore, catalog, find, validate};
use crate::usage::PLATFORM_WORKSPACE;

pub struct PostgresSettingsStore {
    pool: PgPool,
}

impl PostgresSettingsStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn internal(e: sqlx::Error) -> SettingsError {
    SettingsError::Internal(e.to_string())
}

/// The rows at one level, by key.
type Rows = HashMap<String, serde_json::Value>;

/// The (workspace, agent) pair a level's rows live under.
fn address(level: Level) -> (Uuid, Uuid) {
    match level {
        Level::Operator => (PLATFORM_WORKSPACE, Uuid::nil()),
        Level::Workspace(t) => (t, Uuid::nil()),
        Level::Agent {
            workspace_id,
            agent_id,
        } => (workspace_id, agent_id),
    }
}

impl PostgresSettingsStore {
    /// A level's rows: those for every model, with those for `model` over
    /// them, which is how the cascade orders them within a level.
    async fn rows_at(&self, level: Level, model: Option<Uuid>) -> Result<Rows, SettingsError> {
        let (workspace_id, agent_id) = address(level);
        let rows = sqlx::query(
            "select key, value from setting_overrides \
             where workspace_id = $1 and agent_id = $2 and model_id in ($3, $4) \
             order by model_id = $3 desc",
        )
        .bind(workspace_id)
        .bind(agent_id)
        .bind(Uuid::nil())
        .bind(model.unwrap_or(Uuid::nil()))
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<String, _>("key"),
                    r.get::<serde_json::Value, _>("value"),
                )
            })
            .collect())
    }

    /// A level's rows for one model, without those for every model.
    async fn rows_for_model_only(&self, level: Level, model: Uuid) -> Result<Rows, SettingsError> {
        let (workspace_id, agent_id) = address(level);
        let rows = sqlx::query(
            "select key, value from setting_overrides \
             where workspace_id = $1 and agent_id = $2 and model_id = $3",
        )
        .bind(workspace_id)
        .bind(agent_id)
        .bind(model)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;
        Ok(rows
            .iter()
            .map(|r| {
                (
                    r.get::<String, _>("key"),
                    r.get::<serde_json::Value, _>("value"),
                )
            })
            .collect())
    }

    /// What a model the caller can see accepts: a platform model, or one of
    /// the workspace's own. None for a model it cannot.
    async fn facts_of(
        &self,
        level: Level,
        model: Uuid,
    ) -> Result<Option<outturn_core::facts::ModelFacts>, SettingsError> {
        let workspace = match level {
            Level::Operator => None,
            Level::Workspace(w)
            | Level::Agent {
                workspace_id: w, ..
            } => Some(w),
        };
        let row: Option<(Option<serde_json::Value>, serde_json::Value)> = sqlx::query_as(
            "select facts, overrides from models \
             where id = $1 and (workspace_id is null or workspace_id = $2)",
        )
        .bind(model)
        .bind(workspace)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        Ok(row.map(|(facts, overrides)| {
            outturn_core::facts::ModelFacts::of(facts.as_ref(), Some(&overrides))
        }))
    }

    /// The levels above `level`, nearest first, and then `level` itself.
    async fn chain(
        &self,
        level: Level,
        model: Option<Uuid>,
    ) -> Result<Vec<(super::Source, Rows)>, SettingsError> {
        let mut out = Vec::new();
        out.push((
            super::Source::Operator,
            self.rows_at(Level::Operator, model).await?,
        ));
        match level {
            Level::Operator => {}
            Level::Workspace(t) => {
                out.push((
                    super::Source::Workspace,
                    self.rows_at(Level::Workspace(t), model).await?,
                ));
            }
            Level::Agent {
                workspace_id,
                agent_id,
            } => {
                out.push((
                    super::Source::Workspace,
                    self.rows_at(Level::Workspace(workspace_id), model).await?,
                ));
                out.push((
                    super::Source::Agent,
                    self.rows_at(
                        Level::Agent {
                            workspace_id,
                            agent_id,
                        },
                        model,
                    )
                    .await?,
                ));
                // Last, so it wins: a value a workspace or agent could change
                // is not one the template fixed.
                out.push((
                    super::Source::Template,
                    self.fixed_by_template(agent_id).await?,
                ));
            }
        }
        Ok(out)
    }

    /// What the template an agent was made from fixes, from the version it
    /// runs: pinned where allowed, else the newest. Empty for an agent made by
    /// hand.
    async fn fixed_by_template(&self, agent_id: Uuid) -> Result<Rows, SettingsError> {
        let settings: Option<serde_json::Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "select v.settings from agents a {} where a.id = $1",
            crate::agent_template::RUNNING_VERSION
        )))
        .bind(agent_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(internal)?;
        Ok(match settings {
            Some(serde_json::Value::Object(map)) => map.into_iter().collect(),
            _ => Rows::new(),
        })
    }
}

/// Walks the chain for one key: the last level that has a row wins.
fn walk(
    chain: &[(super::Source, Rows)],
    key: &str,
    default: &serde_json::Value,
) -> (serde_json::Value, super::Source) {
    let mut value = default.clone();
    let mut source = super::Source::Default;
    for (level, rows) in chain {
        if let Some(v) = rows.get(key) {
            value = v.clone();
            source = *level;
        }
    }
    (value, source)
}

#[async_trait]
impl SettingsStore for PostgresSettingsStore {
    async fn view(
        &self,
        level: Level,
        model: Option<Uuid>,
    ) -> Result<Vec<Effective>, SettingsError> {
        let chain = self.chain(level, model).await?;
        // This level's own rows, and everything else: the levels above it, and
        // a template's fixed values, which apply whether or not it has a row.
        let own = match level {
            Level::Operator => super::Source::Operator,
            Level::Workspace(_) => super::Source::Workspace,
            Level::Agent { .. } => super::Source::Agent,
        };
        // For one model, this level's own rows are the ones for that model
        // alone: its value for every model is something this model inherits,
        // and removing the model's row falls back to it rather than past it.
        let every_model_here = match model {
            Some(_) => Some(self.rows_at(level, None).await?),
            None => None,
        };
        let here = match model {
            Some(m) => self.rows_for_model_only(level, m).await?,
            None => chain
                .iter()
                .find(|(source, _)| *source == own)
                .map(|(_, rows)| rows.clone())
                .unwrap_or_default(),
        };
        let without: Vec<(super::Source, Rows)> = chain
            .iter()
            .filter_map(|(source, rows)| match (*source == own, &every_model_here) {
                (false, _) => Some((*source, rows.clone())),
                (true, Some(every)) => Some((*source, every.clone())),
                (true, None) => None,
            })
            .collect();

        Ok(catalog()
            .into_iter()
            // A ceiling stops at the workspace: it is about the people using
            // the product, and an agent's page offering it would be offering
            // something that does nothing.
            .filter(|setting| {
                !(setting.owner == super::Owner::OperatorCeiling
                    && matches!(level, Level::Agent { .. }))
            })
            .map(|setting| {
                let (mut value, mut source) = walk(&chain, setting.key, &setting.default);
                let (inherited, _) = walk(&without, setting.key, &setting.default);
                if setting.owner == super::Owner::OperatorCeiling {
                    // Whatever a workspace chose, never looser than the
                    // operator's: the walk alone lets the nearer level win.
                    let operator = walk(&chain[..1], setting.key, &setting.default).0;
                    let bounded = super::stricter(&setting, &operator, &value);
                    if bounded != value {
                        value = bounded;
                        source = super::Source::Operator;
                    }
                }
                Effective {
                    override_value: here.get(setting.key).cloned(),
                    inherited,
                    value,
                    source,
                    setting,
                }
            })
            .collect())
    }

    async fn set(
        &self,
        level: Level,
        model: Option<Uuid>,
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), SettingsError> {
        let setting = find(key).ok_or_else(|| SettingsError::Unknown(key.to_string()))?;
        validate(&setting, &value)?;
        // A value for one model is checked against what that model accepts,
        // so only values that mean something to it can be saved.
        if let Some(model) = model {
            if !setting.per_model {
                return Err(SettingsError::Invalid(format!(
                    "{} is not set per model",
                    setting.label
                )));
            }
            let facts = self
                .facts_of(level, model)
                .await?
                .ok_or_else(|| SettingsError::Invalid("no such model".into()))?;
            if !outturn_core::model_settings::valid_for(key, &value, &facts) {
                return Err(SettingsError::Invalid(format!(
                    "{} cannot be {value} for this model",
                    setting.label
                )));
            }
        }
        if let Level::Agent { agent_id, .. } = level
            && self.fixed_by_template(agent_id).await?.contains_key(key)
        {
            return Err(SettingsError::Invalid(format!(
                "{} is fixed by the template this agent was made from",
                setting.label
            )));
        }
        let (workspace_id, agent_id) = address(level);
        sqlx::query(
            "insert into setting_overrides (workspace_id, agent_id, model_id, key, value) \
             values ($1, $2, $5, $3, $4) \
             on conflict (workspace_id, agent_id, model_id, key) \
             do update set value = excluded.value, updated_at = now()",
        )
        .bind(workspace_id)
        .bind(agent_id)
        .bind(key)
        .bind(value)
        .bind(model.unwrap_or(Uuid::nil()))
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    async fn clear(
        &self,
        level: Level,
        model: Option<Uuid>,
        key: &str,
    ) -> Result<(), SettingsError> {
        find(key).ok_or_else(|| SettingsError::Unknown(key.to_string()))?;
        let (workspace_id, agent_id) = address(level);
        sqlx::query(
            "delete from setting_overrides \
             where workspace_id = $1 and agent_id = $2 and key = $3 and model_id = $4",
        )
        .bind(workspace_id)
        .bind(agent_id)
        .bind(key)
        .bind(model.unwrap_or(Uuid::nil()))
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    async fn resolve(&self, workspace_id: Uuid, agent_id: Uuid) -> Result<Resolved, SettingsError> {
        let chain = self
            .chain(
                Level::Agent {
                    workspace_id,
                    agent_id,
                },
                None,
            )
            .await?;
        let get = |key: &str| {
            let setting = find(key).expect("catalog key");
            walk(&chain, key, &setting.default).0
        };
        let effort = get("reasoning_effort");
        Ok(Resolved {
            repeat_prompt: effort.as_str() == Some(super::REPEAT_PROMPT),
            temperature: get("temperature").as_f64().map(|t| t as f32),
            reasoning_effort: effort
                .as_str()
                // Repetition is thinking off as far as a provider knows.
                .filter(|e| *e != "none" && *e != super::REPEAT_PROMPT)
                .map(str::to_string)
                // "none" is sent through as an explicit off where the
                // provider understands it; absent means "provider default",
                // which for most providers means thinking on.
                .or_else(|| Some("none".to_string())),
            approve_new_hosts: get("approve_new_hosts").as_str() == Some("approve_new_hosts"),
            max_tool_rounds: get("max_tool_rounds")
                .as_u64()
                .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
                .unwrap_or(100),
            context_budget: get("context_budget")
                .as_u64()
                .map(|n| usize::try_from(n).unwrap_or(usize::MAX))
                .unwrap_or(400_000),
            session_naming: get("session_naming")
                .as_str()
                .unwrap_or("after_first_turn")
                .to_string(),
            // Session is in both lists whatever the cascade says: it is the
            // agent's own scratch space, and an agent that could not write it
            // could not hold a thought for the length of a turn.
            write_scopes: {
                let mut scopes = vec!["session".to_string()];
                for (key, scope) in [
                    ("agent_file_access", "agent"),
                    ("workspace_file_access", "workspace"),
                ] {
                    if get(key).as_str() == Some("read_write") {
                        scopes.push(scope.to_string());
                    }
                }
                scopes
            },
            read_scopes: {
                let mut scopes = vec!["session".to_string()];
                for (key, scope) in [
                    ("agent_file_access", "agent"),
                    ("workspace_file_access", "workspace"),
                ] {
                    // Read or read/write, so writing always implies reading.
                    if matches!(get(key).as_str(), Some("read") | Some("read_write")) {
                        scopes.push(scope.to_string());
                    }
                }
                scopes
            },
        })
    }
}
