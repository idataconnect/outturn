use async_trait::async_trait;
use sqlx::Row;
use sqlx::AssertSqlSafe;
use sqlx::postgres::PgPool;
use uuid::Uuid;

use super::{
    RecordUsage, UsageBucket, UsageEntry, UsageError, UsagePage, UsageSlice, UsageStore,
    UsageSummary, UsageTotals,
};

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
        workspace_id: row.get("workspace_id"),
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
        usage_source: row.get("usage_source"),
        provider_usage: row.get("provider_usage"),
        service_tier: row.get("service_tier"),
    }
}

/// A dimension the summary cuts along.
///
/// An enum rather than a caller-supplied column name, because the column goes
/// into a statement by interpolation and a string parameter here would be the
/// one place in this file where workspace input could reach SQL. Every arm
/// below is a literal written in this file; nothing outside it can add one.
#[derive(Clone, Copy)]
enum Dimension {
    Workspace,
    Model,
    Agent,
    Account,
    Source,
    Traffic,
}

impl Dimension {
    /// The ledger column this dimension groups by.
    fn column(self) -> &'static str {
        match self {
            Dimension::Workspace => "workspace_id",
            Dimension::Model => "model",
            Dimension::Agent => "agent_id",
            Dimension::Account => "account",
            Dimension::Source => "usage_source",
            Dimension::Traffic => "traffic_type",
        }
    }

    /// The table a name is read from, where the key is an id.
    fn label_table(self) -> Option<&'static str> {
        match self {
            Dimension::Workspace => Some("workspaces"),
            Dimension::Agent => Some("agents"),
            _ => None,
        }
    }
}

/// How many slices a dimension returns before the rest is folded into one.
///
/// A chart that draws every distinct account in a busy month draws a legend
/// nobody can read. The
/// remainder is summed into a single entry rather than dropped, so the parts
/// still add up to the total a reader can see above them.
const SLICE_LIMIT: i64 = 6;

impl PostgresUsageStore {
    async fn slice(
        &self,
        workspace_id: Option<Uuid>,
        from: chrono::DateTime<chrono::Utc>,
        to: chrono::DateTime<chrono::Utc>,
        dimension: Dimension,
    ) -> Result<Vec<UsageSlice>, UsageError> {
        // `AssertSqlSafe` waives sqlx's refusal to run a built string, so what
        // makes it safe has to be written down rather than assumed.
        //
        // It rests on one fact: `Dimension` carries no data. Every arm is a
        // unit variant, `column` and `label_table` are `match`es returning
        // `&'static str`, and the enum is private with no `From<String>` and no
        // public constructor -- so the only strings that can reach a statement
        // are the ones written in this file. The scope predicate is a `const`
        // beside them.
        //
        // That is the invariant to preserve. Give `Dimension` a variant holding
        // a `String`, or take a column name as an argument, and the waiver
        // silently stops being true while everything still compiles. Anything a
        // caller supplies -- the workspace, the window, the id list -- is bound,
        // and must stay bound.
        let column = dimension.column();
        // Ordered by tokens rather than by calls: a reader cutting a bill by
        // model wants the expensive one at the top, and a cheap model called
        // often is not the answer to that question.
        let sql = format!(
            "select {column}::text as key, \
                    count(*) as calls, \
                    (coalesce(sum(prompt_tokens), 0) + coalesce(sum(completion_tokens), 0) \
                     + coalesce(sum(cache_read_tokens), 0) + coalesce(sum(cache_write_tokens), 0) \
                     + coalesce(sum(reasoning_tokens), 0))::bigint as tokens \
             from usage_ledger \
             where ($1::uuid is null or workspace_id = $1) \
               and occurred_at >= $2 and occurred_at < $3 \
             group by {column} \
             order by tokens desc, calls desc"
        );

        let rows = sqlx::query(AssertSqlSafe(sql))
            .bind(workspace_id)
            .bind(from)
            .bind(to)
            .fetch_all(&self.pool)
            .await
            .map_err(internal)?;

        let mut slices: Vec<UsageSlice> = rows
            .iter()
            .map(|r| UsageSlice {
                key: r.get("key"),
                label: None,
                calls: r.get("calls"),
                tokens: r.get("tokens"),
            })
            .collect();

        // Fold the tail into one entry, so the slices still sum to the total.
        if slices.len() as i64 > SLICE_LIMIT {
            let tail: Vec<UsageSlice> = slices.split_off(SLICE_LIMIT as usize);
            slices.push(UsageSlice {
                key: Some(format!("{} others", tail.len())),
                label: None,
                calls: tail.iter().map(|s| s.calls).sum(),
                tokens: tail.iter().map(|s| s.tokens).sum(),
            });
        }

        // Names for the slices whose key is an id. One statement for the whole
        // page rather than one per row, and a missing name is left as None: a
        // workspace deleted since the rows were written still has a ledger, and
        // inventing a name for it would be worse than showing the id.
        if let Some(table) = dimension.label_table() {
            let ids: Vec<Uuid> = slices
                .iter()
                .filter_map(|s| s.key.as_deref().and_then(|k| Uuid::parse_str(k).ok()))
                .collect();
            if !ids.is_empty() {
                let named = sqlx::query(AssertSqlSafe(format!(
                    "select id::text as id, name from {table} where id = any($1)"
                )))
                .bind(&ids)
                .fetch_all(&self.pool)
                .await
                .map_err(internal)?;
                for row in &named {
                    let id: String = row.get("id");
                    let name: String = row.get("name");
                    for slice in slices.iter_mut() {
                        if slice.key.as_deref() == Some(id.as_str()) {
                            slice.label = Some(name.clone());
                        }
                    }
                }
            }
        }

        Ok(slices)
    }
}

#[async_trait]
impl UsageStore for PostgresUsageStore {
    async fn record(&self, e: RecordUsage) -> Result<(), UsageError> {
        sqlx::query(
            "insert into usage_ledger \
                 (workspace_id, id, agent_id, session_id, user_id, account, reply_id, job_id, \
                  round, traffic_type, endpoint, model, credential_owner, fallback, \
                  prompt_tokens, completion_tokens, cache_read_tokens, cache_write_tokens, \
                  reasoning_tokens, usage_source, provider_usage, service_tier) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, \
                     $15, $16, $17, $18, $19, $20, $21, $22)",
        )
        .bind(e.workspace_id)
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
        .bind(e.usage_source.as_str())
        .bind(e.provider_usage)
        .bind(e.service_tier)
        .execute(&self.pool)
        .await
        .map_err(internal)?;
        Ok(())
    }

    async fn export(
        &self,
        workspace_id: Uuid,
        from: Option<chrono::DateTime<chrono::Utc>>,
        to: Option<chrono::DateTime<chrono::Utc>>,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<UsagePage, UsageError> {
        let rows = sqlx::query(
            "select id, workspace_id, occurred_at, agent_id, session_id, user_id, account, \
                    reply_id, job_id, round, traffic_type, endpoint, model, \
                    credential_owner, fallback, prompt_tokens, completion_tokens, \
                    cache_read_tokens, cache_write_tokens, reasoning_tokens, \
                    usage_source, provider_usage, service_tier \
             from usage_ledger \
             where workspace_id = $1 \
               and ($2::timestamptz is null or occurred_at >= $2) \
               and ($3::timestamptz is null or occurred_at < $3) \
               and ($4::uuid is null or id > $4) \
             order by id \
             limit $5",
        )
        .bind(workspace_id)
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

    async fn summarise(
        &self,
        workspace_id: Option<Uuid>,
        from: chrono::DateTime<chrono::Utc>,
        to: chrono::DateTime<chrono::Utc>,
    ) -> Result<UsageSummary, UsageError> {
        // One predicate, written once and bound the same way in every statement
        // below: a null workspace means every workspace, which the route only
        // ever passes for a system administrator. Written as a null check on
        // the bind rather than as string-built SQL so the parameter is always a
        // parameter -- an account label reaches this code from a workspace's own
        // input, and none of it is ever concatenated into a statement.
        const SCOPE: &str = "where ($1::uuid is null or workspace_id = $1) \
                             and occurred_at >= $2 and occurred_at < $3";

        // `count(distinct agent_id)` ignores nulls, and the ledger has them: the
        // platform's own work -- naming a session, compacting a transcript --
        // carries no agent because no agent asked for it. Counted plainly, the
        // tile said "1 agent" beside a list showing that agent *and* a "Not
        // attributed" row with calls in it, so the page contradicted itself on
        // one screen. The null group is one more distinct answer to "who
        // answered", so it is added back as one.
        let totals_row = sqlx::query(AssertSqlSafe(format!(
            "select count(*) as calls, \
                    coalesce(sum(prompt_tokens), 0)::bigint as prompt_tokens, \
                    coalesce(sum(completion_tokens), 0)::bigint as completion_tokens, \
                    coalesce(sum(cache_read_tokens), 0)::bigint as cache_read_tokens, \
                    coalesce(sum(cache_write_tokens), 0)::bigint as cache_write_tokens, \
                    coalesce(sum(reasoning_tokens), 0)::bigint as reasoning_tokens, \
                    count(distinct session_id) as sessions, \
                    count(distinct agent_id) \
                        + (count(*) filter (where agent_id is null) > 0)::int as agents, \
                    count(distinct workspace_id) as workspaces \
             from usage_ledger {SCOPE}"
        )))
        .bind(workspace_id)
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await
        .map_err(internal)?;

        let totals = UsageTotals {
            calls: totals_row.get("calls"),
            prompt_tokens: totals_row.get("prompt_tokens"),
            completion_tokens: totals_row.get("completion_tokens"),
            cache_read_tokens: totals_row.get("cache_read_tokens"),
            cache_write_tokens: totals_row.get("cache_write_tokens"),
            reasoning_tokens: totals_row.get("reasoning_tokens"),
            sessions: totals_row.get("sessions"),
            agents: totals_row.get("agents"),
            workspaces: totals_row.get("workspaces"),
        };

        // The days come from generate_series and the rows are joined onto them,
        // so a day nothing happened is a zero rather than a missing point. The
        // series is built in the database rather than patched up in Rust
        // because the database is the thing that knows what a day is in the
        // presence of the window's bounds.
        //
        // Every truncation names UTC explicitly. The two-argument `date_trunc`
        // reads the session's TimeZone, which nothing in this path sets: on a
        // deployment whose server or pooler defaults to anything but UTC, every
        // bucket boundary would silently shift, `at` would stop being the
        // midnight this struct promises, and the browser -- which labels each
        // bucket in UTC -- would print the wrong day against the right figures.
        // It passes on a UTC database either way, so the bug would not surface
        // here; it would surface on somebody else's cluster.
        let daily_rows = sqlx::query(AssertSqlSafe(format!(
            "select d.day as at, \
                    coalesce(u.calls, 0)::bigint as calls, \
                    coalesce(u.prompt_tokens, 0)::bigint as prompt_tokens, \
                    coalesce(u.completion_tokens, 0)::bigint as completion_tokens, \
                    coalesce(u.cache_read_tokens, 0)::bigint as cache_read_tokens, \
                    coalesce(u.cache_write_tokens, 0)::bigint as cache_write_tokens, \
                    coalesce(u.reasoning_tokens, 0)::bigint as reasoning_tokens \
             from generate_series(date_trunc('day', $2::timestamptz, 'UTC'), \
                                  date_trunc('day', $3::timestamptz - interval '1 microsecond', 'UTC'), \
                                  interval '1 day') as d(day) \
             left join ( \
                 select date_trunc('day', occurred_at, 'UTC') as day, \
                        count(*) as calls, \
                        sum(prompt_tokens)::bigint as prompt_tokens, \
                        sum(completion_tokens)::bigint as completion_tokens, \
                        sum(cache_read_tokens)::bigint as cache_read_tokens, \
                        sum(cache_write_tokens)::bigint as cache_write_tokens, \
                        sum(reasoning_tokens)::bigint as reasoning_tokens \
                 from usage_ledger {SCOPE} \
                 group by 1 \
             ) u on u.day = d.day \
             order by d.day"
        )))
        .bind(workspace_id)
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await
        .map_err(internal)?;

        let daily = daily_rows
            .iter()
            .map(|r| UsageBucket {
                at: r.get("at"),
                calls: r.get("calls"),
                prompt_tokens: r.get("prompt_tokens"),
                completion_tokens: r.get("completion_tokens"),
                cache_read_tokens: r.get("cache_read_tokens"),
                cache_write_tokens: r.get("cache_write_tokens"),
                reasoning_tokens: r.get("reasoning_tokens"),
            })
            .collect();

        let by_workspace = self.slice(workspace_id, from, to, Dimension::Workspace).await?;
        let by_model = self.slice(workspace_id, from, to, Dimension::Model).await?;
        let by_agent = self.slice(workspace_id, from, to, Dimension::Agent).await?;
        let by_account = self.slice(workspace_id, from, to, Dimension::Account).await?;
        let by_source = self.slice(workspace_id, from, to, Dimension::Source).await?;
        let by_traffic = self.slice(workspace_id, from, to, Dimension::Traffic).await?;

        Ok(UsageSummary {
            from,
            to,
            totals,
            daily,
            by_workspace,
            by_model,
            by_agent,
            by_account,
            by_source,
            by_traffic,
        })
    }
}
