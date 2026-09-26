//! Deciding which feeds are due.
//!
//! Intervals are per feed with a global fallback. A feed that has never
//! succeeded is always due. A feed whose last attempt failed backs off
//! exponentially from its normal interval so a dead host is not hammered every
//! fifteen minutes forever, capped so it still recovers within a day.

use chrono::{DateTime, Duration, Utc};

use snaprss_core::passwords::{server_of, Credentials};
use snaprss_core::{Db, DbError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueFeed {
    pub id: i64,
    /// For progress reporting. Not used to decide anything.
    pub title: String,
    pub xml_url: String,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    /// Sent as HTTP basic authentication, for feeds marked as needing it.
    pub credentials: Option<Credentials>,
}

/// Fill in the sign-in for feeds whose `authentication` flag is set, from
/// the stored passwords for their host.
pub fn attach_credentials(db: &Db, feeds: &mut [DueFeed]) -> Result<(), DbError> {
    let flagged: std::collections::HashSet<i64> = db
        .conn()
        .prepare("SELECT id FROM feeds WHERE authentication = 1")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    if flagged.is_empty() {
        return Ok(());
    }
    let creds = db.all_credentials()?;
    for f in feeds.iter_mut().filter(|f| flagged.contains(&f.id)) {
        f.credentials = creds.get(&server_of(&f.xml_url)).cloned();
    }
    Ok(())
}

/// The longest interval honoured. Beyond this the arithmetic overflows and
/// panics, and nobody means "poll once a century".
const MAX_INTERVAL_DAYS: i64 = 365;

/// `unit` is SnapRSS's own word, or QuiteRSS's code as imported from its
/// database: "-1" seconds, "0" minutes, "1" hours. Reading the codes as
/// minutes polled an hourly feed sixty times too often.
pub fn interval_to_duration(value: i64, unit: Option<&str>) -> Duration {
    let v = value.max(1);
    let d = match unit.map(str::trim).unwrap_or("minutes") {
        "days" => Duration::try_days(v),
        "hours" | "1" => Duration::try_hours(v),
        // Nothing polls more often than once a minute.
        "seconds" | "-1" => Duration::try_seconds(v).map(|d| d.max(Duration::minutes(1))),
        _ => Duration::try_minutes(v),
    };
    d.unwrap_or(Duration::days(MAX_INTERVAL_DAYS))
        .min(Duration::days(MAX_INTERVAL_DAYS))
}

/// Exponential backoff on consecutive failures: 1x, 2x, 4x, 8x, capped at 24h
/// or the feed's own interval, whichever is longer. Capping below the interval
/// made a weekly feed poll daily after one failure: backing off, it polled
/// more often.
pub fn backoff(base: Duration, failures: i64) -> Duration {
    if failures <= 0 {
        return base;
    }
    let shift = failures.min(6) as u32;
    let scaled = base
        .checked_mul(2i32.saturating_pow(shift))
        .unwrap_or(Duration::days(MAX_INTERVAL_DAYS));
    scaled.min(base.max(Duration::hours(24)))
}

/// Feeds that should be fetched now. `global_minutes` applies to feeds without
/// their own interval.
/// Every updatable feed, ignoring intervals and backoff.
///
/// What the Update button means. `due_feeds` answers "what should the
/// background loop poll now"; pressing a button is a different question, and
/// answering it with the schedule is why the button usually did nothing.
pub fn all_feeds(db: &Db) -> Result<Vec<DueFeed>, DbError> {
    let mut stmt = db.conn().prepare(
        "SELECT id, COALESCE(NULLIF(text, ''), NULLIF(title, ''), xml_url),
                xml_url, http_etag, http_last_modified
         FROM feeds
         WHERE kind = 1 AND disable_update = 0
           AND xml_url IS NOT NULL AND xml_url <> ''
         ORDER BY row_to_parent, id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(DueFeed {
            id: r.get(0)?,
            title: r.get(1)?,
            xml_url: r.get(2)?,
            etag: r.get(3)?,
            last_modified: r.get(4)?,
            credentials: None,
        })
    })?;
    let mut feeds = rows.collect::<Result<Vec<_>, _>>()?;
    attach_credentials(db, &mut feeds)?;
    Ok(feeds)
}

pub fn due_feeds(
    db: &Db,
    now: DateTime<Utc>,
    global_minutes: i64,
) -> Result<Vec<DueFeed>, DbError> {
    let mut stmt = db.conn().prepare(
        "SELECT id, xml_url, http_etag, http_last_modified,
                update_interval_enable, update_interval, update_interval_type,
                updated, COALESCE(NULLIF(text, ''), NULLIF(title, ''), xml_url),
                consecutive_failures
         FROM feeds
         WHERE kind = 1 AND disable_update = 0
           AND xml_url IS NOT NULL AND xml_url <> ''",
    )?;

    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, Option<String>>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, i64>(9)?,
        ))
    })?;

    let mut due = Vec::new();
    for row in rows {
        let (id, xml_url, etag, last_modified, iv_enable, iv, iv_type, updated, title, failures) =
            row?;

        // QuiteRSS writes -1 for "use the global interval".
        let base = if iv_enable > 0 {
            interval_to_duration(iv.unwrap_or(global_minutes), iv_type.as_deref())
        } else {
            interval_to_duration(global_minutes, Some("minutes"))
        };
        let wait = backoff(base, failures);

        let ready = match updated.as_deref().and_then(super::ingest_parse_ts) {
            // Never fetched, or an unparseable timestamp: fetch it.
            None => true,
            Some(last) => now.signed_duration_since(last) >= wait,
        };

        if ready {
            due.push(DueFeed {
                id,
                title,
                xml_url,
                etag,
                last_modified,
                credentials: None,
            });
        }
    }
    attach_credentials(db, &mut due)?;
    Ok(due)
}
