//! The usage ledger: one row per model call, with every dimension a bill
//! might be cut along.
//!
//! Append-only, tokens not prices, exported rather than summed. See
//! `migrations/0004_usage_ledger.sql` for why, and docs/routing.md for the
//! columns that routing will fill in once workspaces can bring their own keys.

mod postgres;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
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
    /// Where the counts above came from, so a reader of an export can tell a
    /// measured row from an estimated one without asking.
    pub usage_source: String,
    /// The provider's usage object as it came off the wire.
    pub provider_usage: Option<serde_json::Value>,
    pub service_tier: Option<String>,
}

/// Where a row's token counts came from.
///
/// Kept beside the counts because zeros are ambiguous without it: a round that
/// genuinely cost nothing and a round nobody measured are written the same way,
/// and the difference is exactly what a disputed bill turns on. It also cannot
/// be worked out later -- a row that did not record it never will -- so it is
/// written from the first row rather than added once somebody asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    /// The provider said so, and the call ran to its end.
    Reported,
    /// The provider said so, but the call was cut short: real numbers for a
    /// partial answer. Anthropic and Gemini both leave these behind when a
    /// stream stops, because they report as they go.
    ReportedPartial,
    /// Nothing was reported and the tokens were counted here. Arithmetic of
    /// ours, not a provider's figure, and marked so it is never mistaken for
    /// one.
    Estimated,
    /// Nothing was reported and nothing could be counted. The zeros mean "not
    /// measured" and say so, rather than looking like a free turn.
    Unknown,
}

impl UsageSource {
    /// The spelling the column checks against.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::ReportedPartial => "reported_partial",
            Self::Estimated => "estimated",
            Self::Unknown => "unknown",
        }
    }
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
    /// Where the counts above came from. See [`UsageSource`].
    pub usage_source: UsageSource,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The spellings are the ones the column checks against. A value the
    /// database refuses fails the insert, which loses the row rather than the
    /// provenance -- so these are pinned rather than trusted to stay in step.
    #[test]
    fn every_source_spells_itself_the_way_the_column_expects() {
        assert_eq!(UsageSource::Reported.as_str(), "reported");
        assert_eq!(UsageSource::ReportedPartial.as_str(), "reported_partial");
        assert_eq!(UsageSource::Estimated.as_str(), "estimated");
        assert_eq!(UsageSource::Unknown.as_str(), "unknown");
    }

    /// The names survive a round trip, since an export is read by whoever is
    /// arguing about the bill.
    #[test]
    fn a_source_survives_being_written_down_and_read_back() {
        for source in [
            UsageSource::Reported,
            UsageSource::ReportedPartial,
            UsageSource::Estimated,
            UsageSource::Unknown,
        ] {
            let json = serde_json::to_string(&source).expect("serialize");
            assert_eq!(
                json,
                format!("\"{}\"", source.as_str()),
                "the wire spelling and the column spelling must not drift apart"
            );
            let back: UsageSource = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, source);
        }
    }
}
