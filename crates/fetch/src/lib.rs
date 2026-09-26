//! Fetching, parsing and ingesting feeds.
//!
//! Split so the pieces are testable on their own: `http` does conditional GET
//! and knows nothing about the database, `ingest` does identity and insertion
//! and knows nothing about the network, `schedule` decides what is due.

pub mod dates;
pub mod discover;
pub mod http;
pub mod icons;
pub mod ingest;
pub mod schedule;

mod update;

pub use http::{build_client, fetch, fetch_as, FetchConfig, FetchError, FetchOutcome, Validators};
pub use ingest::{ingest, parse_feed, parse_feed_with, IngestReport, IngestRules};
pub use schedule::{all_feeds, due_feeds, DueFeed};
pub use update::{
    apply_fetched, fetch_all, update_due, update_feed, Fetched, Progress, UpdateOutcome,
    UpdateSummary,
};

pub(crate) fn ingest_parse_ts(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&chrono::Utc))
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|n| n.and_utc())
        })
}
