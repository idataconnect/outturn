use std::collections::HashMap;

use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use super::{Effective, Level, Resolved, SettingsError, SettingsStore, catalog, find, validate};
use crate::api::usage::PLATFORM_WORKSPACE;

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
    async fn rows_at(&self, level: Level) -> Result<Rows, SettingsError> {
        let (workspace_id, agent_id) = address(level);
        let rows = sqlx::query(
            "select key, value from setting_overrides where workspace_id = $1 and agent_id = $2",
        )
        .bind(workspace_id)
        .bind(agent_id)
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

    /// The levels above `level`, nearest first, and then `level` itself.
    async fn chain(&self, level: Level) -> Result<Vec<(super::Source, Rows)>, SettingsError> {
        let mut out = Vec::new();
        out.push((
            super::Source::Operator,
            self.rows_at(Level::Operator).await?,
        ));
        match level {
            Level::Operator => {}
            Level::Workspace(t) => {
                out.push((
                    super::Source::Workspace,
                    self.rows_at(Level::Workspace(t)).await?,
                ));
            }
            Level::Agent {
                workspace_id,
                agent_id,
            } => {
                out.push((
                    super::Source::Workspace,
                    self.rows_at(Level::Workspace(workspace_id)).await?,
                ));
                out.push((
                    super::Source::Agent,
                    self.rows_at(Level::Agent {
                        workspace_id,
                        agent_id,
                    })
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

    /// What the template an agent was made from fixes, from its newest
    /// version. Empty for an agent made by hand.
    async fn fixed_by_template(&self, agent_id: Uuid) -> Result<Rows, SettingsError> {
        let settings: Option<serde_json::Value> = sqlx::query_scalar(
            "select v.settings from agents a \
               join lateral (select settings from agent_template_versions \
                              where template_id = a.template_id \
                              order by ordinal desc limit 1) v on true \
              where a.id = $1",
        )
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
    async fn view(&self, level: Level) -> Result<Vec<Effective>, SettingsError> {
        let chain = self.chain(level).await?;
        // This level's own rows, and everything else: the levels above it, and
        // a template's fixed values, which apply whether or not it has a row.
        let own = match level {
            Level::Operator => super::Source::Operator,
            Level::Workspace(_) => super::Source::Workspace,
            Level::Agent { .. } => super::Source::Agent,
        };
        let here = chain
            .iter()
            .find(|(source, _)| *source == own)
            .map(|(_, rows)| rows.clone())
            .unwrap_or_default();
        let without: Vec<(super::Source, Rows)> = chain
            .iter()
            .filter(|(source, _)| *source != own)
            .cloned()
            .collect();

        Ok(catalog()
            .into_iter()
            .map(|setting| {
                let (value, source) = walk(&chain, setting.key, &setting.default);
                let (inherited, _) = walk(&without, setting.key, &setting.default);
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
        key: &str,
        value: serde_json::Value,
    ) -> Result<(), SettingsError> {
        let setting = find(key).ok_or_else(|| SettingsError::Unknown(key.to_string()))?;
        validate(&setting, &value)?;
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
            "insert into setting_overrides (workspace_id, agent_id, key, value) \
             values ($1, $2, $3, $4) \
             on conflict (workspace_id, agent_id, key) \
             do update set value = excluded.value, updated_at = now()",
        )
        .bind(workspace_id)
        .bind(agent_id)
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    async fn clear(&self, level: Level, key: &str) -> Result<(), SettingsError> {
        find(key).ok_or_else(|| SettingsError::Unknown(key.to_string()))?;
        let (workspace_id, agent_id) = address(level);
        sqlx::query(
            "delete from setting_overrides where workspace_id = $1 and agent_id = $2 and key = $3",
        )
        .bind(workspace_id)
        .bind(agent_id)
        .bind(key)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    async fn resolve(&self, workspace_id: Uuid, agent_id: Uuid) -> Result<Resolved, SettingsError> {
        let chain = self
            .chain(Level::Agent {
                workspace_id,
                agent_id,
            })
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
