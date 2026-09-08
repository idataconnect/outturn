//! The usage ledger: one row per model call, with every dimension a bill
//! might be cut along.
//!
//! Append-only, tokens not prices, exported rather than summed. See
//! `migrations/0004_usage_ledger.sql` for why, and docs/routing.md for the
//! columns that routing will fill in once workspaces can bring their own keys.

mod postgres;

use async_trait::async_trait;
use serde::Serialize;
use uuid::Uuid;

pub use postgres::PostgresUsageStore;

/// The workspace the platform's own work bills to.
pub const PLATFORM_WORKSPACE: Uuid = Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_0000_0001);

/// One model call, as it will appear on a bill.
#[derive(Debug, Clone, Serialize)]
pub struct UsageEntry {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub agent_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    /// The workspace's own label for whose conversation this was.
    pub account: Option<String>,
    pub reply_id: Option<Uuid>,
    pub job_id: Option<Uuid>,
    pub round: i32,
    pub traffic_type: String,
    pub endpoint: String,
    pub model: String,
    pub credential_owner: String,
    pub fallback: String,
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    pub cache_read_tokens: i32,
    pub cache_write_tokens: i32,
    pub reasoning_tokens: i32,
    /// The provider's usage object as it came off the wire.
    pub provider_usage: Option<serde_json::Value>,
    pub service_tier: Option<String>,
}

/// What a caller records. The id and timestamp are the store's.
#[derive(Debug, Clone)]
pub struct RecordUsage {
    pub workspace_id: Uuid,
    pub agent_id: Option<Uuid>,
    pub session_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub account: Option<String>,
    pub reply_id: Option<Uuid>,
    pub job_id: Option<Uuid>,
    pub round: i32,
    pub traffic_type: String,
    pub endpoint: String,
    pub model: String,
    pub credential_owner: String,
    pub fallback: String,
    pub prompt_tokens: i32,
    pub completion_tokens: i32,
    pub cache_read_tokens: i32,
    pub cache_write_tokens: i32,
    pub reasoning_tokens: i32,
    pub provider_usage: Option<serde_json::Value>,
    pub service_tier: Option<String>,
}

/// A page of the ledger.
#[derive(Debug, Serialize)]
pub struct UsagePage {
    pub entries: Vec<UsageEntry>,
    /// The last id in `entries`, to pass back as `after`. Absent when the page
    /// is empty, which is how a reader knows it has everything.
    pub next: Option<Uuid>,
}

#[derive(Debug, thiserror::Error)]
pub enum UsageError {
    #[error("usage store error: {0}")]
    Internal(String),
}

#[async_trait]
pub trait UsageStore: Send + Sync {
    async fn record(&self, entry: RecordUsage) -> Result<(), UsageError>;

    /// A workspace's rows in id order -- which is time order, ids being UUIDv7 --
    /// within a window, from a cursor. Stable for a closed window: nothing is
    /// ever updated or deleted, so the same call returns the same rows.
    async fn export(
        &self,
        workspace_id: Uuid,
        from: Option<chrono::DateTime<chrono::Utc>>,
        to: Option<chrono::DateTime<chrono::Utc>>,
        after: Option<Uuid>,
        limit: i64,
    ) -> Result<UsagePage, UsageError>;
}
