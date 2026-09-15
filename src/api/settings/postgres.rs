use std::collections::HashMap;

use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use super::{Effective, Level, Resolved, SettingsError, SettingsStore, catalogue, find, validate};
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
        Level::Agent { workspace_id, agent_id } => (workspace_id, agent_id),
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
            .map(|r| (r.get::<String, _>("key"), r.get::<serde_json::Value, _>("value")))
            .collect())
    }

    /// The levels above `level`, nearest first, and then `level` itself.
    async fn chain(&self, level: Level) -> Result<Vec<(super::Source, Rows)>, SettingsError> {
        let mut out = Vec::new();
        out.push((super::Source::Operator, self.rows_at(Level::Operator).await?));
        match level {
            Level::Operator => {}
            Level::Workspace(t) => {
                out.push((super::Source::Workspace, self.rows_at(Level::Workspace(t)).await?));
            }
            Level::Agent { workspace_id, agent_id } => {
                out.push((super::Source::Workspace, self.rows_at(Level::Workspace(workspace_id)).await?));
                out.push((
                    super::Source::Agent,
                    self.rows_at(Level::Agent { workspace_id, agent_id }).await?,
                ));
            }
        }
        Ok(out)
    }
}

/// Walks the chain for one key: the last level that has a row wins.
fn walk(chain: &[(super::Source, Rows)], key: &str, default: &serde_json::Value) -> (serde_json::Value, super::Source) {
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
        let (above, here) = chain.split_at(chain.len() - 1);
        let here = &here[0].1;

        Ok(catalogue()
            .into_iter()
            .map(|setting| {
                let (value, source) = walk(&chain, setting.key, &setting.default);
                let (inherited, _) = walk(above, setting.key, &setting.default);
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

    async fn set(&self, level: Level, key: &str, value: serde_json::Value) -> Result<(), SettingsError> {
        let setting = find(key).ok_or_else(|| SettingsError::Unknown(key.to_string()))?;
        validate(&setting, &value)?;
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
        let chain = self.chain(Level::Agent { workspace_id, agent_id }).await?;
        let get = |key: &str| {
            let setting = find(key).expect("catalogue key");
            walk(&chain, key, &setting.default).0
        };
        Ok(Resolved {
            temperature: get("temperature").as_f64().map(|t| t as f32),
            reasoning_effort: get("reasoning_effort")
                .as_str()
                .filter(|e| *e != "none")
                .map(str::to_string)
                // "none" is sent through as an explicit off where the
                // provider understands it; absent means "provider default",
                // which for most providers means thinking on.
                .or_else(|| Some("none".to_string())),
            max_tool_rounds: get("max_tool_rounds")
                .as_u64()
                .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
                .unwrap_or(100),
            context_budget: get("context_budget")
                .as_u64()
                .map(|n| usize::try_from(n).unwrap_or(usize::MAX))
                .unwrap_or(400_000),
            write_scopes: {
                let mut scopes = vec!["session".to_string()];
                if get("agent_writes_agent_files").as_str() == Some("allow") {
                    scopes.push("agent".to_string());
                }
                if get("agent_writes_workspace_files").as_str() == Some("allow") {
                    scopes.push("workspace".to_string());
                }
                scopes
            },
        })
    }
}
