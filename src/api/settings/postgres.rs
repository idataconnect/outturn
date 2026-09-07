use std::collections::HashMap;

use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use super::{Effective, Level, Resolved, SettingsError, SettingsStore, catalogue, find, validate};
use crate::api::usage::PLATFORM_TENANT;

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

/// The (tenant, agent) pair a level's rows live under.
fn address(level: Level) -> (Uuid, Uuid) {
    match level {
        Level::Operator => (PLATFORM_TENANT, Uuid::nil()),
        Level::Tenant(t) => (t, Uuid::nil()),
        Level::Agent { tenant_id, agent_id } => (tenant_id, agent_id),
    }
}

impl PostgresSettingsStore {
    async fn rows_at(&self, level: Level) -> Result<Rows, SettingsError> {
        let (tenant_id, agent_id) = address(level);
        let rows = sqlx::query(
            "select key, value from setting_overrides where tenant_id = $1 and agent_id = $2",
        )
        .bind(tenant_id)
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
            Level::Tenant(t) => {
                out.push((super::Source::Tenant, self.rows_at(Level::Tenant(t)).await?));
            }
            Level::Agent { tenant_id, agent_id } => {
                out.push((super::Source::Tenant, self.rows_at(Level::Tenant(tenant_id)).await?));
                out.push((
                    super::Source::Agent,
                    self.rows_at(Level::Agent { tenant_id, agent_id }).await?,
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
        let (tenant_id, agent_id) = address(level);
        sqlx::query(
            "insert into setting_overrides (tenant_id, agent_id, key, value) \
             values ($1, $2, $3, $4) \
             on conflict (tenant_id, agent_id, key) \
             do update set value = excluded.value, updated_at = now()",
        )
        .bind(tenant_id)
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
        let (tenant_id, agent_id) = address(level);
        sqlx::query(
            "delete from setting_overrides where tenant_id = $1 and agent_id = $2 and key = $3",
        )
        .bind(tenant_id)
        .bind(agent_id)
        .bind(key)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    async fn resolve(&self, tenant_id: Uuid, agent_id: Uuid) -> Result<Resolved, SettingsError> {
        let chain = self.chain(Level::Agent { tenant_id, agent_id }).await?;
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
            write_scopes: {
                let mut scopes = vec!["session".to_string()];
                if get("agent_writes_agent_files").as_str() == Some("allow") {
                    scopes.push("agent".to_string());
                }
                if get("agent_writes_tenant_files").as_str() == Some("allow") {
                    scopes.push("tenant".to_string());
                }
                scopes
            },
        })
    }
}
