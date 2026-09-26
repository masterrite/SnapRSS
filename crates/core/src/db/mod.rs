//! SQLite layer. One file, WAL, no ORM.

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::path::Path;

use crate::models::{FeedNode, NewsItem, NodeKind, ReadingMode};

pub const SCHEMA_VERSION: i64 = 3;

const SCHEMA: &str = include_str!("schema.sql");

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database was written by a newer SnapRSS (schema v{found}, this build understands v{supported})")]
    TooNew { found: i64, supported: i64 },
    #[error("{0} is not a QuiteRSS database: {1}")]
    NotQuiteRss(String, &'static str),
    #[error("{0}")]
    Import(String),
}

pub struct Db {
    pub(crate) conn: Connection,
}

impl Db {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self, DbError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, DbError> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Lower case for every script. SQLite's own LIKE and NOCASE fold only
        // A to Z, so a search for "новости" missed "Новости". Used in queries
        // only, never in the schema, so the file stays readable without it.
        conn.create_scalar_function(
            "snap_lower",
            1,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8 | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
            |ctx| Ok(ctx.get::<Option<String>>(0)?.map(|s| s.to_lowercase())),
        )?;
        conn.execute_batch(SCHEMA)?;
        // Columns added after a table first shipped. Added in place rather
        // than by bumping the schema version, so an older SnapRSS can still
        // open the file: it ignores a column it does not know.
        add_column_if_missing(&conn, "feeds", "icon_checked", "TEXT")?;
        // QuiteRSS writes status 0 for a feed that updated fine, and imports
        // copied it, so every imported feed showed a warning. Success here is
        // an empty status, as an update writes it. Run on every open rather
        // than as a version step for the same reason as the column above; it
        // touches a few hundred rows at most and nothing once they are fixed.
        conn.execute("UPDATE feeds SET status = '' WHERE TRIM(status) = '0'", [])?;

        let found: i64 = conn
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM settings WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);

        if found > SCHEMA_VERSION {
            return Err(DbError::TooNew {
                found,
                supported: SCHEMA_VERSION,
            });
        }
        if found < SCHEMA_VERSION {
            // v0 means freshly created; the repairs are no-ops on an empty
            // database.
            if found < 2 {
                repair_imported_values(&conn)?;
            }
            if found < 3 {
                // Pages in GBK, Big5 or Shift_JIS were decoded as UTF-8 and
                // cached as replacement characters. Drop those so they are
                // fetched again, now decoded properly.
                conn.execute(
                    "UPDATE news SET article_html = NULL, article_fetched = NULL
                     WHERE article_html LIKE '%' || char(65533) || char(65533) || char(65533) || '%'",
                    [],
                )?;
            }
            conn.execute(
                "INSERT INTO settings(key, value) VALUES('schema_version', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [SCHEMA_VERSION.to_string()],
            )?;
        }

        Ok(Db { conn })
    }

    /// Open a foreign SQLite file read-only. Used for importing.
    pub(crate) fn open_foreign_readonly(path: impl AsRef<Path>) -> Result<Connection, DbError> {
        Ok(Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )?)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Needed by callers that open their own transaction, such as ingestion.
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Direct children of `parent`, ordered as the tree shows them.
    /// Pass `None` for the roots.
    pub fn children(&self, parent: Option<i64>) -> Result<Vec<FeedNode>, DbError> {
        let sql = "SELECT id, kind, parent_id, row_to_parent, text, xml_url, html_url,
                          unread, undelete_count, status, reading_mode, expanded
                   FROM feeds
                   WHERE parent_id IS ?1
                   ORDER BY row_to_parent, id";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([parent], |r| {
            Ok(FeedNode {
                id: r.get(0)?,
                kind: NodeKind::from_i64(r.get(1)?),
                parent_id: r.get(2)?,
                row_to_parent: r.get(3)?,
                text: r.get(4)?,
                xml_url: r.get(5)?,
                html_url: r.get(6)?,
                unread: r.get(7)?,
                undelete_count: r.get(8)?,
                status: r.get(9)?,
                reading_mode: ReadingMode::from_i64(r.get(10)?),
                expanded: r.get::<_, i64>(11)? != 0,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn set_expanded(&self, id: i64, expanded: bool) -> Result<(), DbError> {
        self.conn.execute(
            "UPDATE feeds SET expanded = ?2 WHERE id = ?1",
            rusqlite::params![id, expanded as i64],
        )?;
        Ok(())
    }

    pub fn total_unread(&self) -> Result<i64, DbError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM news WHERE read = 0 AND deleted = 0",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn count(&self, table: &str) -> Result<i64, DbError> {
        // Callers are internal; table names are literals in this crate.
        Ok(self
            .conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
    }

    /// Recompute the denormalised per-feed counters from `news`. Run after an
    /// import, and after anything that touches read/deleted in bulk.
    /// Recount only the feeds these articles belong to.
    pub fn recompute_counters_for_news(&self, news_ids: &[i64]) -> Result<(), DbError> {
        if news_ids.is_empty() {
            return Ok(());
        }
        let mut feeds = std::collections::BTreeSet::new();
        let mut stmt = self.conn.prepare_cached("SELECT feed_id FROM news WHERE id = ?1")?;
        for id in news_ids {
            if let Some(f) = stmt.query_row([id], |r| r.get::<_, i64>(0)).optional()? {
                feeds.insert(f);
            }
        }
        self.recompute_counters_for(&feeds.into_iter().collect::<Vec<_>>())
    }

    /// Recount some feeds. After a change to a few articles this is what
    /// needs redoing; the full recount scaled with the whole database.
    pub fn recompute_counters_for(&self, feed_ids: &[i64]) -> Result<(), DbError> {
        let mut stmt = self.conn.prepare_cached(
            "UPDATE feeds SET
                 unread = (SELECT COUNT(*) FROM news
                           WHERE news.feed_id = feeds.id AND deleted = 0 AND read = 0),
                 undelete_count = (SELECT COUNT(*) FROM news
                                   WHERE news.feed_id = feeds.id AND deleted = 0)
             WHERE id = ?1",
        )?;
        for id in feed_ids {
            stmt.execute([id])?;
        }
        Ok(())
    }

    pub fn recompute_counters(&self) -> Result<(), DbError> {
        self.conn.execute_batch(
            "UPDATE feeds SET
                 unread = (SELECT COUNT(*) FROM news
                           WHERE news.feed_id = feeds.id AND deleted = 0 AND read = 0),
                 undelete_count = (SELECT COUNT(*) FROM news
                                   WHERE news.feed_id = feeds.id AND deleted = 0)
             WHERE kind = 1;",
        )?;
        Ok(())
    }

    /// Articles in a feed, newest first. `None` means every feed.
    pub fn news_for_feed(
        &self,
        feed_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<NewsItem>, DbError> {
        let sql = "SELECT id, feed_id, guid, title, author_name, published, received,
                          link_href, read, starred, deleted
                   FROM news
                   WHERE deleted = 0 AND (?1 IS NULL OR feed_id = ?1)
                   ORDER BY COALESCE(published, received) DESC
                   LIMIT ?2";
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(rusqlite::params![feed_id, limit], |r| {
            Ok(NewsItem {
                id: r.get(0)?,
                feed_id: r.get(1)?,
                guid: r.get(2)?,
                title: r.get(3)?,
                author_name: r.get(4)?,
                published: r.get(5)?,
                received: r.get(6)?,
                link_href: r.get(7)?,
                read: r.get::<_, i64>(8)? != 0,
                starred: r.get::<_, i64>(9)? != 0,
                deleted: r.get::<_, i64>(10)? != 0,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

/// Values that earlier versions copied verbatim from a QuiteRSS database, in
/// QuiteRSS's encoding rather than this one. The importer now converts them;
/// this fixes databases imported before it did.
///
/// * `read = 2` is QuiteRSS's ordinary "read". Cleanup's "never delete
///   unread" tested `read = 1`, so these were never cleaned up.
/// * Dates without a zone. Ingestion writes `...Z`, so the date-based identity
///   rules never matched an imported article and edited ones came back new.
/// * Update interval units as QuiteRSS's codes, "-1"/"0"/"1" for seconds,
///   minutes and hours, and -1 for "use the global interval".
pub(crate) fn repair_imported_values(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "UPDATE news SET read = 1 WHERE read > 1;
         UPDATE news SET published = published || 'Z'
          WHERE published GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]';
         UPDATE news SET received = received || 'Z'
          WHERE received GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]';
         UPDATE news SET modified = modified || 'Z'
          WHERE modified GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]';
         UPDATE feeds SET update_interval_type = CASE TRIM(update_interval_type)
                 WHEN '-1' THEN 'seconds' WHEN '0' THEN 'minutes' WHEN '1' THEN 'hours'
                 ELSE update_interval_type END;
         UPDATE feeds SET update_interval_enable = 0 WHERE update_interval_enable < 0;",
    )
}

fn add_column_if_missing(conn: &Connection, table: &str, column: &str, ty: &str) -> Result<(), DbError> {
    let exists = conn
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(Result::ok)
        .any(|c| c == column);
    if !exists {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ty}"))?;
    }
    Ok(())
}
