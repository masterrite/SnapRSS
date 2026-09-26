//! Sign-in details for feeds that need them.
//!
//! Stored the way QuiteRSS stores them, so an imported database works
//! unchanged: one row per host name, the password base64-encoded (an
//! encoding, not encryption), used only for feeds whose `authentication`
//! flag is set.

use std::collections::HashMap;
use std::fmt;

use base64::{engine::general_purpose::STANDARD, Engine};

use crate::db::{Db, DbError};

#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub user: String,
    pub password: String,
}

/// Never prints the password, so a feed logged with `{:?}` does not leak it.
impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials").field("user", &self.user).field("password", &"…").finish()
    }
}

/// The key QuiteRSS uses: the URL's host, or the whole URL when it has none.
pub fn server_of(url: &str) -> String {
    match url::Url::parse(url.trim()) {
        Ok(u) => u.host_str().map(str::to_string).unwrap_or_else(|| url.trim().to_string()),
        Err(_) => url.trim().to_string(),
    }
}

fn decode(stored: &str) -> String {
    STANDARD
        .decode(stored.trim())
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        // Not base64: taken as written rather than lost.
        .unwrap_or_else(|| stored.to_string())
}

impl Db {
    /// Every stored sign-in, by server.
    pub fn all_credentials(&self) -> Result<HashMap<String, Credentials>, DbError> {
        let mut stmt = self.conn().prepare(
            "SELECT server, COALESCE(username, ''), COALESCE(password, '') FROM passwords ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (server, user, pass) = row?;
            // The first row for a server wins, as in QuiteRSS.
            out.entry(server).or_insert(Credentials { user, password: decode(&pass) });
        }
        Ok(out)
    }

    /// The sign-in a feed at `url` would use, if it asks for one.
    pub fn feed_credentials(&self, feed_id: i64) -> Result<Option<Credentials>, DbError> {
        let (auth, url): (i64, Option<String>) = self.conn().query_row(
            "SELECT authentication, xml_url FROM feeds WHERE id = ?1",
            [feed_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if auth == 0 {
            return Ok(None);
        }
        let Some(url) = url else { return Ok(None) };
        Ok(self.all_credentials()?.remove(&server_of(&url)))
    }

    /// What Feed properties saves: a user name, and a new password or None
    /// to keep the one saved for the feed's site. The saved password is the
    /// site's, shared by every feed on it, so it is looked up by site: a
    /// second feed given the same user name with the password left blank
    /// used to save an empty password over the one the first feed uses.
    pub fn save_feed_sign_in(&self, feed_id: i64, user: &str, password: Option<&str>) -> Result<(), DbError> {
        let password = match password {
            Some(p) => p.to_string(),
            None => {
                let url: Option<String> =
                    self.conn().query_row("SELECT xml_url FROM feeds WHERE id = ?1", [feed_id], |r| r.get(0))?;
                self.all_credentials()?
                    .remove(&server_of(url.as_deref().unwrap_or("")))
                    .map(|c| c.password)
                    .unwrap_or_default()
            }
        };
        self.set_feed_credentials(feed_id, user, &password)
    }

    /// Set or clear a feed's sign-in. An empty user name clears it and turns
    /// the feed's `authentication` flag off. The row is per server, so other
    /// feeds on the same host share it, as they do in QuiteRSS.
    pub fn set_feed_credentials(&self, feed_id: i64, user: &str, password: &str) -> Result<(), DbError> {
        let url: Option<String> =
            self.conn().query_row("SELECT xml_url FROM feeds WHERE id = ?1", [feed_id], |r| r.get(0))?;
        let server = server_of(url.as_deref().unwrap_or(""));
        if user.trim().is_empty() {
            self.conn().execute("UPDATE feeds SET authentication = 0 WHERE id = ?1", [feed_id])?;
            // Drop the row only when no other feed on the host still uses it.
            let others: i64 = self.conn().query_row(
                "SELECT COUNT(*) FROM feeds WHERE authentication = 1 AND id <> ?1 AND xml_url IS NOT NULL",
                [feed_id],
                |r| r.get(0),
            )?;
            let still_used = others > 0
                && self
                    .conn()
                    .prepare("SELECT xml_url FROM feeds WHERE authentication = 1 AND id <> ?1")?
                    .query_map([feed_id], |r| r.get::<_, String>(0))?
                    .filter_map(Result::ok)
                    .any(|u| server_of(&u) == server);
            if !still_used {
                self.conn().execute("DELETE FROM passwords WHERE server = ?1", [&server])?;
            }
            return Ok(());
        }
        let encoded = STANDARD.encode(password.as_bytes());
        let changed = self.conn().execute(
            "UPDATE passwords SET username = ?2, password = ?3 WHERE server = ?1",
            rusqlite::params![server, user.trim(), encoded],
        )?;
        if changed == 0 {
            self.conn().execute(
                "INSERT INTO passwords (server, username, password) VALUES (?1, ?2, ?3)",
                rusqlite::params![server, user.trim(), encoded],
            )?;
        }
        self.conn().execute("UPDATE feeds SET authentication = 1 WHERE id = ?1", [feed_id])?;
        Ok(())
    }
}
