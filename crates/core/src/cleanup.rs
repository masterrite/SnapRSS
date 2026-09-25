//! Retention: deciding which articles have outlived their usefulness.
//!
//! Two stages, deliberately separate, because they have very different
//! consequences:
//!
//! * **Clean up** sets `deleted = 1`. The article leaves every view but the
//!   Deleted folder, and can be restored. This is what runs automatically.
//! * **Purge** runs `DELETE FROM news`. Nothing comes back. This only ever
//!   happens when someone asks for it.
//!
//! Protections win over rules. If a feed says never delete starred articles,
//! no amount of age or count pressure will touch one. That ordering is the
//! whole reason people trust a cleanup routine to run unattended, so the
//! protections are applied as SQL predicates on every rule rather than as a
//! filter someone could forget to reapply.

use chrono::{Duration, Utc};
use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};

use crate::db::{Db, DbError};

/// What a cleanup run did, per rule, so the UI can say something more useful
/// than "done".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CleanupReport {
    /// Feeds examined.
    pub feeds: usize,
    /// Marked deleted because they were read.
    pub by_read: usize,
    /// Marked deleted for being older than the age limit.
    pub by_age: usize,
    /// Marked deleted for falling outside the keep-newest-N limit.
    pub by_count: usize,
    /// Rows actually removed from the database.
    pub purged: usize,
    /// Bytes the file shrank by, when a vacuum ran.
    pub bytes_freed: i64,
}

impl CleanupReport {
    pub fn total_deleted(&self) -> usize {
        self.by_read + self.by_age + self.by_count
    }

    pub fn is_empty(&self) -> bool {
        self.total_deleted() == 0 && self.purged == 0
    }
}

/// The retention rules for one feed, after global defaults have been folded in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retention {
    pub delete_read: bool,
    pub max_age_days: Option<i64>,
    pub max_to_keep: Option<i64>,
    pub never_delete_unread: bool,
    pub never_delete_starred: bool,
    pub never_delete_labeled: bool,
}

impl Default for Retention {
    fn default() -> Self {
        Retention {
            delete_read: false,
            max_age_days: None,
            max_to_keep: None,
            // Deleting something unread means the user never saw it. That is
            // the one outcome a reader must never produce by itself, so it is
            // on by default and has to be turned off deliberately.
            never_delete_unread: true,
            never_delete_starred: true,
            never_delete_labeled: true,
        }
    }
}

impl Retention {
    /// True when nothing would ever be deleted, so the feed can be skipped
    /// without running three queries to find that out.
    pub fn is_noop(&self) -> bool {
        !self.delete_read
            && self.max_age_days.map(|d| d <= 0).unwrap_or(true)
            && self.max_to_keep.map(|n| n <= 0).unwrap_or(true)
    }

    fn from_row(r: &Row<'_>, defaults: &Retention) -> rusqlite::Result<Self> {
        let enabled_or = |flag: i64, value: Option<i64>, fallback: Option<i64>| -> Option<i64> {
            if flag != 0 {
                value.filter(|v| *v > 0)
            } else {
                fallback
            }
        };

        // A rule and its protections travel together.
        //
        // The feeds table gives every protection column a default of 1, so
        // there is no way to tell "the user turned this on for this feed" from
        // "nobody has ever touched this feed". Reading the column
        // unconditionally therefore made the global protection switches dead:
        // every feed claimed to protect everything, whatever Settings said.
        //
        // So a feed that has opted into retention of its own owns its
        // protections, and a feed that has not follows the global ones — the
        // same source decides both halves.
        let feed_owns_rules = r.get::<_, i64>("max_age_enable")? != 0
            || r.get::<_, i64>("max_to_keep_enable")? != 0
            || r.get::<_, i64>("delete_read")? != 0;

        let protect = |column: &str, fallback: bool| -> rusqlite::Result<bool> {
            Ok(if feed_owns_rules {
                r.get::<_, i64>(column)? != 0
            } else {
                fallback
            })
        };

        Ok(Retention {
            delete_read: r.get::<_, i64>("delete_read")? != 0 || defaults.delete_read,
            max_age_days: enabled_or(
                r.get("max_age_enable")?,
                r.get("max_age_days")?,
                defaults.max_age_days,
            ),
            max_to_keep: enabled_or(
                r.get("max_to_keep_enable")?,
                r.get("max_to_keep")?,
                defaults.max_to_keep,
            ),
            never_delete_unread: protect("never_delete_unread", defaults.never_delete_unread)?,
            never_delete_starred: protect("never_delete_starred", defaults.never_delete_starred)?,
            never_delete_labeled: protect("never_delete_labeled", defaults.never_delete_labeled)?,
        })
    }

    /// The protections, as a SQL fragment appended to every rule's WHERE
    /// clause. Building it once and reusing it is what stops a rule being
    /// written that quietly ignores one.
    fn guard_sql(&self) -> String {
        let mut parts = vec!["deleted = 0".to_string()];
        if self.never_delete_unread {
            // Not `read = 1`: QuiteRSS marks read articles 2, and imported
            // ones kept that, so they counted as unread and were never
            // cleaned up.
            parts.push("read <> 0".into());
        }
        if self.never_delete_starred {
            parts.push("starred = 0".into());
        }
        if self.never_delete_labeled {
            parts.push("id NOT IN (SELECT news_id FROM news_labels)".into());
        }
        parts.join(" AND ")
    }
}

/// Global fallbacks, used by any feed that has not set its own.
pub fn global_retention(db: &Db) -> Result<Retention, DbError> {
    let get = |key: &str| -> Option<String> {
        db.conn()
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get::<_, String>(0)
            })
            .ok()
            .filter(|v| !v.trim().is_empty())
    };
    let num = |key: &str| get(key).and_then(|v| v.parse::<i64>().ok()).filter(|n| *n > 0);
    let flag = |key: &str, default: bool| {
        get(key)
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(default)
    };

    Ok(Retention {
        delete_read: flag("cleanup.delete_read", false),
        max_age_days: num("cleanup.max_age_days"),
        max_to_keep: num("cleanup.max_to_keep"),
        never_delete_unread: flag("cleanup.never_delete_unread", true),
        never_delete_starred: flag("cleanup.never_delete_starred", true),
        never_delete_labeled: flag("cleanup.never_delete_labeled", true),
    })
}

/// The rules that will actually be applied to one feed.
pub fn retention_for(db: &Db, feed_id: i64) -> Result<Retention, DbError> {
    let defaults = global_retention(db)?;
    let r = db.conn().query_row(
        "SELECT delete_read, max_age_days, max_age_enable, max_to_keep, max_to_keep_enable,
                never_delete_unread, never_delete_starred, never_delete_labeled
         FROM feeds WHERE id = ?1",
        [feed_id],
        |row| Retention::from_row(row, &defaults),
    )?;
    Ok(r)
}

/// Mark whatever the rules cover as deleted. `feed_id` of `None` runs over
/// every feed.
///
/// Nothing is removed from the database here; see [`purge_deleted`].
pub fn run(db: &mut Db, feed_id: Option<i64>) -> Result<CleanupReport, DbError> {
    let defaults = global_retention(db)?;
    let now = Utc::now();

    let feeds: Vec<(i64, Retention)> = {
        let sql = "SELECT id, delete_read, max_age_days, max_age_enable, max_to_keep,
                          max_to_keep_enable, never_delete_unread, never_delete_starred,
                          never_delete_labeled
                   FROM feeds
                   WHERE kind = 1 AND (?1 IS NULL OR id = ?1)";
        let mut stmt = db.conn().prepare(sql)?;
        let rows = stmt.query_map([feed_id], |r| {
            Ok((r.get::<_, i64>("id")?, Retention::from_row(r, &defaults)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    let mut report = CleanupReport::default();
    let deleted_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let tx = db.conn_mut().transaction()?;

    for (id, rules) in feeds {
        report.feeds += 1;
        if rules.is_noop() {
            continue;
        }
        let guard = rules.guard_sql();

        if rules.delete_read {
            report.by_read += tx.execute(
                &format!(
                    // `read <> 0` here, not only in the guard: with "never
                    // delete unread" off the guard drops it, and "delete once
                    // read" then deleted everything, read or not.
                    "UPDATE news SET deleted = 1, delete_date = ?1
                     WHERE feed_id = ?2 AND read <> 0 AND {guard}"
                ),
                params![deleted_at, id],
            )?;
        }

        // An absurd age (a typo with extra zeros) overflows the date
        // arithmetic, which panicked inside the hourly background task.
        if let Some(cutoff_at) = rules
            .max_age_days
            .and_then(Duration::try_days)
            .and_then(|d| now.checked_sub_signed(d))
        {
            let cutoff = cutoff_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            // COALESCE because a feed that supplies no publication date still
            // has a received timestamp, and an article with neither should not
            // be immortal.
            report.by_age += tx.execute(
                &format!(
                    "UPDATE news SET deleted = 1, delete_date = ?1
                     WHERE feed_id = ?2 AND {guard}
                       AND COALESCE(published, received) < ?3"
                ),
                params![deleted_at, id, cutoff],
            )?;
        }

        if let Some(keep) = rules.max_to_keep {
            // Rank over everything still live in the feed, protected or not,
            // so "keep 50" means the 50 newest articles rather than 50 plus
            // however many happen to be starred.
            report.by_count += tx.execute(
                &format!(
                    "UPDATE news SET deleted = 1, delete_date = ?1
                     WHERE feed_id = ?2 AND {guard}
                       AND id NOT IN (
                           SELECT id FROM news
                           WHERE feed_id = ?2 AND deleted = 0
                           ORDER BY COALESCE(published, received) DESC
                           LIMIT ?3
                       )"
                ),
                params![deleted_at, id, keep],
            )?;
        }
    }

    tx.commit()?;
    db.recompute_counters()?;
    Ok(report)
}

/// Permanently remove articles already marked deleted. Irreversible.
///
/// `older_than_days` limits it to rows deleted at least that long ago, which
/// is what an automatic purge should use; `None` takes all of them.
pub fn purge_deleted(db: &mut Db, older_than_days: Option<i64>) -> Result<CleanupReport, DbError> {
    let before_bytes = db_bytes(db)?;
    let cutoff = match older_than_days {
        Some(days) => match Duration::try_days(days).and_then(|d| Utc::now().checked_sub_signed(d)) {
            Some(t) => t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            // Older than the start of time: nothing qualifies.
            None => String::new(),
        },
        None => "9999".to_string(),
    };

    // A purged article leaves a stub: its identity (guid, link, title, date)
    // and nothing else, with `deleted = 2`, which no view shows. Removing the
    // row outright removed the only record that the article had been seen,
    // and every one still in its feed came back as new and unread on the
    // next poll. QuiteRSS keeps the same kind of stub, with the same value.
    let tx = db.conn_mut().transaction()?;
    tx.execute(
        "DELETE FROM news_labels WHERE news_id IN
             (SELECT id FROM news WHERE deleted = 1 AND COALESCE(delete_date, '') < ?1)",
        [&cutoff],
    )?;
    let n = tx.execute(
        "UPDATE news SET deleted = 2, read = 1, new = 0, starred = 0,
                description = NULL, content = NULL, article_html = NULL, article_fetched = NULL,
                author_name = NULL, author_uri = NULL, author_email = NULL, category = NULL,
                comments = NULL, source = NULL, rights = NULL,
                enclosure_url = NULL, enclosure_type = NULL, enclosure_length = NULL
         WHERE deleted = 1 AND COALESCE(delete_date, '') < ?1",
        [&cutoff],
    )?;
    tx.commit()?;

    db.recompute_counters()?;
    Ok(CleanupReport {
        purged: n,
        bytes_freed: (before_bytes - db_bytes(db)?).max(0),
        ..Default::default()
    })
}

/// Rebuild the file to reclaim space freed by deletions. SQLite does not
/// return pages to the filesystem on its own.
///
/// This rewrites the whole database, so it is slow on a large file and is
/// never run automatically.
pub fn vacuum(db: &Db) -> Result<i64, DbError> {
    let before = db_bytes(db)?;
    // In WAL mode VACUUM writes the whole database into the -wal file, which
    // then sits beside it, as large, until the app exits. Checkpointing
    // folds it back and truncates it, so the disk space is actually freed.
    db.conn().execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok((before - db_bytes(db)?).max(0))
}

fn db_bytes(db: &Db) -> Result<i64, DbError> {
    let page_size: i64 = db.conn().query_row("PRAGMA page_size", [], |r| r.get(0))?;
    let page_count: i64 = db.conn().query_row("PRAGMA page_count", [], |r| r.get(0))?;
    Ok(page_size * page_count)
}

/// What the storage page shows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbStats {
    pub bytes: i64,
    pub feeds: i64,
    pub articles: i64,
    pub unread: i64,
    pub starred: i64,
    pub deleted: i64,
    /// Articles holding a cached extracted body, which is the bulk of the size.
    pub with_cached_article: i64,
    pub oldest: Option<String>,
}

pub fn stats(db: &Db) -> Result<DbStats, DbError> {
    let c = db.conn();
    Ok(DbStats {
        bytes: db_bytes(db)?,
        feeds: c.query_row("SELECT COUNT(*) FROM feeds WHERE kind = 1", [], |r| r.get(0))?,
        articles: c.query_row("SELECT COUNT(*) FROM news WHERE deleted = 0", [], |r| {
            r.get(0)
        })?,
        unread: c.query_row(
            "SELECT COUNT(*) FROM news WHERE read = 0 AND deleted = 0",
            [],
            |r| r.get(0),
        )?,
        starred: c.query_row("SELECT COUNT(*) FROM news WHERE starred = 1 AND deleted = 0", [], |r| {
            r.get(0)
        })?,
        deleted: c.query_row("SELECT COUNT(*) FROM news WHERE deleted = 1", [], |r| {
            r.get(0)
        })?,
        with_cached_article: c.query_row(
            "SELECT COUNT(*) FROM news WHERE article_html IS NOT NULL AND article_html <> ''",
            [],
            |r| r.get(0),
        )?,
        oldest: c
            .query_row(
                "SELECT MIN(COALESCE(published, received)) FROM news WHERE deleted = 0",
                [],
                |r| r.get::<_, Option<String>>(0),
            )
            .unwrap_or(None),
    })
}

/// Drop cached extracted article bodies. They are re-fetchable, and on a large
/// database they are most of the file.
pub fn clear_article_cache(db: &Db) -> Result<usize, DbError> {
    Ok(db.conn().execute(
        "UPDATE news SET article_html = NULL, article_fetched = NULL
         WHERE article_html IS NOT NULL",
        [],
    )?)
}
