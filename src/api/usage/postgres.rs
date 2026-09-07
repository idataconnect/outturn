use async_trait::async_trait;
use sqlx::Row;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use super::{RecordUsage, UsageEntry, UsageError, UsagePage, UsageStore};

pub struct PostgresUsageStore {
    pool: PgPool,
}

impl PostgresUsageStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn internal(e: sqlx::Error) -> UsageError {
    UsageError::Internal(e.to_string())
}

fn read_entry(row: &sqlx::postgres::PgRow) -> UsageEntry {
    UsageEntry {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        occurred_at: row.get("occurred_at"),
        agent_id: row.get("agent_id"),
        session_id: row.get("session_id"),
        user_id: row.get("user_id"),
        account: row.get("account"),
        reply_id: row.get("reply_id"),
        job_id: row.get("job_id"),
        round: row.get("round"),
        traffic_type: row.get("traffic_type"),
        endpoint: row.get("endpoint"),
        model: row.get("model"),
        credential_owner: row.get("credential_owner"),
        fallback: row.get("fallback"),
        prompt_tokens: row.get("prompt_tokens"),
        completion_tokens: row.get("completion_tokens"),
        cache_read_tokens: row.get("cache_read_tokens"),
        cache_write_tokens: row.get("cache_write_tokens"),
        reasoning_tokens: row.get("reasoning_tokens"),
        provider_usage: row.get("provider_usage"),
        service_tier: row.get("service_tier"),
    }
}

#[async_trait]
impl UsageStore for PostgresUsageStore {
    async fn record(&self, e: RecordUsage) -> Result<(), UsageError> {
        sqlx::query(
            "insert into usage_ledger \
                 (tenant_id, id, agent_id, session_id, user_id, account, reply_id, job_id, \
                  round, traffic_type, endpoint, model, credential_owner, fallback, \
                  prompt_tokens, completion_tokens, cache_read_tokens, cache_write_tokens, \
                  reasoning_tokens, provider_usage, service_tier) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, \
                     $15, $16, $17, $18, $19, $20, $21)",
        )
        .bind(e.tenant_id)
        .bind(Uuid::now_v7())
        .bind(e.agent_id)
        .bind(e.session_id)
        .bind(e.user_id)
        .bind(e.account)
        .bind(e.reply_id)
        .bind(e.job_id)
        .bind(e.round)
        .bind(e.traffic_type)
        .bind(e.endpoint)
        .bind(e.model)
        .bind(e.credential_owner)
        .bind(e.fallback)
        .bind(e.prompt_tokens)
        .bind(e.completion_tokens)
        .bind(e.cache_read_tokens)
        .bind(e.cache_write_tokens)
        .bind(e.reasoning_tokens)
        .bind(e.provider_usage)
        .bind(e.service_tier)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    async fn export(
        &self,
        tenant_id: Uuid,
        from: Option<chrono::DateTime<chrono::Utc>>,
        to: Option<chrono::DateTime<chrono::Utc>>,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<UsagePage, UsageError> {
        let rows = sqlx::query(
            "select id, tenant_id, occurred_at, agent_id, session_id, user_id, account, \
                    reply_id, job_id, round, traffic_type, endpoint, model, \
                    credential_owner, fallback, prompt_tokens, completion_tokens, \
                    cache_read_tokens, cache_write_tokens, reasoning_tokens, \
                    provider_usage, service_tier \
             from usage_ledger \
             where tenant_id = $1 \
               and ($2::timestamptz is null or occurred_at >= $2) \
               and ($3::timestamptz is null or occurred_at < $3) \
               and ($4::uuid is null or id > $4) \
             order by id \
             limit $5",
        )
        .bind(tenant_id)
        .bind(from)
        .bind(to)
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        let entries: Vec<UsageEntry> = rows.iter().map(read_entry).collect();
        let next = entries.last().map(|e| e.id);
        Ok(UsagePage { entries, next })
    }
}
