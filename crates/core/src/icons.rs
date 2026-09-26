//! Feed icons.
//!
//! Kept in `feeds.image` the way QuiteRSS keeps them, base64 text of the
//! image file, so imported icons show without being fetched again.

use base64::{engine::general_purpose::STANDARD, Engine};

use crate::db::{Db, DbError};

/// Largest icon kept. Favicons are a few KB; anything this big is not one.
pub const MAX_ICON_BYTES: usize = 200 * 1024;

/// The image type of `bytes` from its first bytes, for the formats a
/// favicon comes in. None for anything else, which is then not stored.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    let head = &bytes[..bytes.len().min(512)];
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if head.starts_with(&[0, 0, 1, 0]) {
        Some("image/x-icon")
    } else if head.starts_with(b"GIF8") {
        Some("image/gif")
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        let text = String::from_utf8_lossy(head).to_ascii_lowercase();
        let t = text.trim_start_matches('\u{feff}').trim_start();
        (t.starts_with("<svg") || (t.starts_with("<?xml") && t.contains("<svg"))).then_some("image/svg+xml")
    }
}

/// A stored icon as a `data:` URL for an `<img>`, or None if it is empty or
/// not an image.
pub fn data_url(stored: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(stored).ok()?;
    let clean: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if clean.is_empty() {
        return None;
    }
    let bytes = STANDARD.decode(clean.as_bytes()).ok()?;
    let mime = sniff(&bytes)?;
    Some(format!("data:{mime};base64,{clean}"))
}

impl Db {
    /// Every feed that has an icon, as `data:` URLs.
    pub fn feed_icons(&self) -> Result<Vec<(i64, String)>, DbError> {
        let mut stmt = self
            .conn()
            .prepare("SELECT id, image FROM feeds WHERE kind = 1 AND image IS NOT NULL AND length(image) > 0")?;
        let rows = stmt.query_map([], |r| {
            let id: i64 = r.get(0)?;
            // Imported rows hold the base64 as a blob, ours as text.
            let raw: Vec<u8> = match r.get_ref(1)? {
                rusqlite::types::ValueRef::Blob(b) => b.to_vec(),
                rusqlite::types::ValueRef::Text(t) => t.to_vec(),
                _ => Vec::new(),
            };
            Ok((id, raw))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, raw) = row?;
            if let Some(url) = data_url(&raw) {
                out.push((id, url));
            }
        }
        Ok(out)
    }

    /// Feeds with no icon that have not been looked at for a fortnight, and
    /// the address to look for one at.
    pub fn feeds_needing_icons(&self, limit: usize) -> Result<Vec<(i64, String)>, DbError> {
        let mut stmt = self.conn().prepare(
            "SELECT id, COALESCE(NULLIF(html_url, ''), xml_url) FROM feeds
             WHERE kind = 1 AND xml_url IS NOT NULL AND xml_url <> ''
               AND (image IS NULL OR length(image) = 0)
               AND (icon_checked IS NULL OR icon_checked < ?1)
             ORDER BY icon_checked IS NOT NULL, id
             LIMIT ?2",
        )?;
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(14)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let rows = stmt.query_map(rusqlite::params![cutoff, limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Record the result of looking for a feed's icon: the image, or that
    /// none was found (so it is not looked for again for a while).
    ///
    /// `site` is the address the icon was looked up for, as
    /// `feeds_needing_icons` gave it. A feed removed during the lookup can
    /// have its id reused by a new feed, which must not get the old site's
    /// icon.
    pub fn store_icon(&self, feed_id: i64, site: &str, image: Option<&[u8]>) -> Result<(), DbError> {
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let encoded = image
            .filter(|b| b.len() <= MAX_ICON_BYTES && sniff(b).is_some())
            .map(|b| STANDARD.encode(b));
        self.conn().execute(
            "UPDATE feeds SET icon_checked = ?1, image = COALESCE(?2, image)
             WHERE id = ?3 AND COALESCE(NULLIF(html_url, ''), xml_url) = ?4",
            rusqlite::params![now, encoded, feed_id, site],
        )?;
        Ok(())
    }
}
