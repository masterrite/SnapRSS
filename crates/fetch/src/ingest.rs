//! Turning a parsed feed into rows.
//!
//! The interesting part is deciding whether an entry is already in the
//! database. Feeds are inconsistent about identity:
//!
//! - A well-behaved feed has a stable `<guid>` / `<id>`. Match on that.
//! - Plenty of feeds omit it, or regenerate it on every publish. Fall back to
//!   the link.
//! - A few reuse one link for everything (some aggregators, some CMS bugs).
//!   Fall back to title plus published date.
//!
//! Matching is scoped to the feed. Two feeds carrying the same syndicated
//! article are two articles here; cross-feed duplicate collapsing is a separate
//! user-facing setting and does not belong in the insert path.

use chrono::{DateTime, Utc};
use feed_rs::model::{Entry, Feed as ParsedFeed};
use rusqlite::{params, OptionalExtension, Transaction};

use snaprss_core::filters::{self, Candidate, Engine};
use snaprss_core::{Db, DbError};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestReport {
    pub inserted: usize,
    /// Already present under one of the identity rules.
    pub duplicates: usize,
    /// Older than the feed's cutoff.
    pub too_old: usize,
    /// Updated in place because the feed changed the title or body.
    pub updated: usize,
    /// Newly inserted articles that at least one filter acted on.
    pub filtered: usize,
    /// Of those, how many a filter sent straight to Deleted.
    pub filtered_away: usize,
    /// Sound files the filters asked to play. Nothing here can play or show
    /// anything, so they are handed up to the app.
    pub sounds: Vec<String>,
    /// Titles of new articles a filter asked to be notified about.
    pub notify: Vec<String>,
}

/// Per-feed rules that affect what gets stored.
#[derive(Debug, Clone)]
pub struct IngestRules {
    /// false means only import entries newer than `avoid_old_before`.
    pub add_any_date: bool,
    pub avoid_old_before: Option<DateTime<Utc>>,
    /// Rewrite an existing row when the feed changes the title or content,
    /// rather than leaving the first version in place.
    pub apply_edits: bool,
}

impl Default for IngestRules {
    fn default() -> Self {
        IngestRules {
            add_any_date: true,
            avoid_old_before: None,
            apply_edits: true,
        }
    }
}

impl IngestRules {
    pub fn load(db: &Db, feed_id: i64) -> Result<Self, DbError> {
        let (any_date, enable, before): (i64, i64, Option<String>) = db.conn().query_row(
            "SELECT add_any_date, avoid_old_enable, avoid_old_before FROM feeds WHERE id = ?1",
            [feed_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok(IngestRules {
            add_any_date: any_date != 0,
            avoid_old_before: if enable != 0 {
                before.as_deref().and_then(parse_ts)
            } else {
                None
            },
            apply_edits: true,
        })
    }
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(|n| n.and_utc())
        })
        // QuiteRSS stores "avoid news before" as a bare date.
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(s.trim(), "%Y-%m-%d")
                .ok()
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .map(|n| n.and_utc())
        })
}

/// Parse a feed the way ingestion expects: an entry the feed gave no id keeps
/// no id.
///
/// feed-rs invents one otherwise, from the link and title, or a random UUID
/// when there is neither. The random one made an item with no guid, link or
/// title look new on every poll, and a made-up id can never be told apart from
/// a real one, which the identity rules need to do.
pub fn parse_feed(bytes: &[u8], base_url: &str) -> Result<ParsedFeed, feed_rs::parser::ParseFeedError> {
    parse_feed_with(bytes, base_url, None)
}

/// [`parse_feed`], with the response's `Content-Type`.
///
/// feed-rs honours an encoding in the XML declaration but not the HTTP
/// charset, so a GBK feed that declares its charset only in the header
/// parsed as mojibake and lost every title. When the document itself says
/// nothing, the header's charset is applied here.
pub fn parse_feed_with(
    bytes: &[u8],
    base_url: &str,
    content_type: Option<&str>,
) -> Result<ParsedFeed, feed_rs::parser::ParseFeedError> {
    let transcoded = transcode_by_header(bytes, content_type);
    let bytes = transcoded.as_deref().unwrap_or(bytes);
    let chinese = crate::dates::looks_chinese(&String::from_utf8_lossy(&bytes[..bytes.len().min(64 * 1024)]));
    feed_rs::parser::Builder::new()
        .base_uri(Some(base_url))
        .id_generator(|_, _, _| String::new())
        .timestamp_parser(move |s| crate::dates::parse(s, chinese))
        .build()
        .parse(bytes)
}

/// UTF-8 bytes for a document whose only charset is the HTTP header's, or
/// `None` to parse the bytes as they are.
fn transcode_by_header(bytes: &[u8], content_type: Option<&str>) -> Option<Vec<u8>> {
    if encoding_rs::Encoding::for_bom(bytes).is_some() {
        return None;
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
    let declares = head.trim_start().starts_with("<?xml")
        && head.find("?>").is_some_and(|end| head[..end].contains("encoding="));
    if declares {
        return None;
    }
    let enc = snaprss_article::charset_from_content_type(content_type?)?;
    if enc == encoding_rs::UTF_8 {
        return None;
    }
    let (text, _, _) = enc.decode(bytes);
    Some(text.into_owned().into_bytes())
}

fn fmt_ts(d: &DateTime<Utc>) -> String {
    d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The identity check. Returns the existing row id if this entry is already
/// stored for this feed.
///
/// `published` is `None` for an entry with no date, and also for one whose
/// date was in the future and replaced by the time of the poll (`clamped`).
/// That stored value is a different one on every poll, so it cannot tell two
/// polls' copies of an entry apart from two entries.
#[allow(clippy::too_many_arguments)]
fn find_existing(
    tx: &Transaction<'_>,
    feed_id: i64,
    guid: Option<&str>,
    link: Option<&str>,
    title: Option<&str>,
    published: Option<&str>,
    clamped: bool,
    summary: Option<&str>,
) -> Result<Option<i64>, rusqlite::Error> {
    fn present(v: Option<&str>) -> Option<&str> {
        v.filter(|v| !v.trim().is_empty())
    }
    let guid = present(guid);
    let link = present(link);
    let title = present(title);

    let first = |sql: &str, p: &[&dyn rusqlite::ToSql]| -> Result<Option<i64>, rusqlite::Error> {
        tx.query_row(sql, p, |r| r.get(0)).optional()
    };

    if let Some(g) = guid {
        if let Some(hit) = first(
            "SELECT id FROM news WHERE feed_id = ?1 AND guid = ?2 LIMIT 1",
            &[&feed_id, &g],
        )? {
            return Ok(Some(hit));
        }
    }

    // The weaker rules only compare against rows that could be the same
    // entry. When this entry and a stored row both carry a guid and the two
    // differ, the feed has said they are different articles. A weekly post at
    // a fixed URL under a fixed title gets a new guid each week; matching it
    // on link and title swallowed every week after the first and overwrote
    // the old one with it. A feed that regenerates guids on every edit shows
    // an edited article twice instead, which is the lesser failure.
    const SAME_OR_NO_GUID: &str = "(?4 IS NULL OR IFNULL(guid, '') = '')";

    // A bare link match is not enough. Some feeds point every item at the same
    // page, and collapsing those loses articles permanently, which is a far
    // worse failure than showing one twice. Require the link to agree with
    // either the title or the publication date.
    if let Some(l) = link {
        if let Some(t) = title {
            if let Some(hit) = first(
                &format!(
                    "SELECT id FROM news WHERE feed_id = ?1 AND link_href = ?2 AND title = ?3
                       AND {SAME_OR_NO_GUID} LIMIT 1"
                ),
                &[&feed_id, &l, &t, &guid],
            )? {
                return Ok(Some(hit));
            }
        }
        // A title added or dropped, the entry otherwise the same. Two titles
        // that differ are two entries: a changelog or release page lists
        // each day's items under one link and one date.
        if let Some(p) = published {
            if let Some(hit) = first(
                &format!(
                    "SELECT id FROM news WHERE feed_id = ?1 AND link_href = ?2 AND published = ?3
                       AND (?5 IS NULL OR IFNULL(title, '') = '')
                       AND {SAME_OR_NO_GUID} LIMIT 1"
                ),
                &[&feed_id, &l, &p, &guid, &title],
            )? {
                return Ok(Some(hit));
            }
        }
    }

    // For feeds that reuse one link, or give none. Two links that differ are
    // two entries: "New comment" at the same minute on two posts is two
    // comments.
    if let (Some(t), Some(p)) = (title, published) {
        if let Some(hit) = first(
            &format!(
                "SELECT id FROM news WHERE feed_id = ?1 AND title = ?2 AND published = ?3
                   AND (?5 IS NULL OR IFNULL(link_href, '') = '' OR link_href = ?5)
                   AND {SAME_OR_NO_GUID} LIMIT 1"
            ),
            &[&feed_id, &t, &p, &guid, &link],
        )? {
            return Ok(Some(hit));
        }
    }

    // No usable date and only one of title and link, so none of the rules
    // above applies and the item was new on every poll. Whatever the entry
    // has must match exactly, the missing one included, against a row that
    // was stored without a date too. A clamped date was stored, but as the
    // time of some earlier poll, so any stored date will do for that entry.
    if published.is_none() && link.is_some() != title.is_some() {
        if let Some(hit) = first(
            &format!(
                "SELECT id FROM news WHERE feed_id = ?1
                   AND IFNULL(link_href, '') = IFNULL(?2, '') AND IFNULL(title, '') = IFNULL(?3, '')
                   AND (?5 OR IFNULL(published, '') = '')
                   AND {SAME_OR_NO_GUID} LIMIT 1"
            ),
            &[&feed_id, &link, &title, &guid, &clamped],
        )? {
            return Ok(Some(hit));
        }
    }

    // Nothing to go on but the text: no guid, no link, no title. Status and
    // microblog feeds do this, and without a rule the item is new on every
    // poll.
    if guid.is_none() && link.is_none() && title.is_none() {
        if let Some(body) = present(summary) {
            if let Some(hit) = first(
                "SELECT id FROM news WHERE feed_id = ?1
                   AND IFNULL(link_href, '') = '' AND IFNULL(title, '') = ''
                   AND description = ?2 AND (?4 OR IFNULL(published, '') = IFNULL(?3, ''))
                 LIMIT 1",
                &[&feed_id, &body, &published, &clamped],
            )? {
                return Ok(Some(hit));
            }
        }
    }

    Ok(None)
}

fn entry_link(entry: &Entry) -> Option<String> {
    entry
        .links
        .iter()
        .find(|l| l.rel.as_deref() == Some("alternate"))
        .or_else(|| entry.links.first())
        .map(|l| l.href.clone())
}

/// Summary and content as HTML. Atom's `type="text"` is plain text, and was
/// stored and rendered as HTML: "use a &lt;section&gt;" lost its word and
/// `<div>` opened a real element.
fn entry_body(entry: &Entry) -> (Option<String>, Option<String>) {
    let plain = |t: &mediatype::MediaTypeBuf| t.essence().to_string() == "text/plain";
    let summary = entry.summary.as_ref().map(|t| {
        if plain(&t.content_type) {
            snaprss_article::text_to_html(&t.content)
        } else {
            t.content.clone()
        }
    });
    let content = entry
        .content
        .as_ref()
        .and_then(|c| {
            let body = c.body.clone()?;
            Some(if plain(&c.content_type) {
                snaprss_article::text_to_html(&body)
            } else {
                body
            })
        })
        .filter(|b| !b.trim().is_empty());
    (summary, content)
}

/// A title as text. Atom's `type="html"` titles carry markup, shown as
/// literal tags; RSS titles often carry entities escaped twice, shown as
/// "AT&amp;T".
fn entry_title(entry: &Entry) -> Option<String> {
    let t = entry.title.as_ref()?;
    let text = if t.content_type.essence().to_string() == "text/html" {
        snaprss_article::to_plain_text(&t.content)
    } else {
        snaprss_article::decode_entities(&t.content)
    };
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

/// The item's enclosure. feed-rs lists `media:content` before `<enclosure>`,
/// and taking the first put a podcast's thumbnail where its audio belonged.
fn entry_enclosure(entry: &Entry) -> Option<&feed_rs::model::MediaContent> {
    let all: Vec<_> = entry.media.iter().flat_map(|m| m.content.iter()).collect();
    let kind = |c: &&feed_rs::model::MediaContent| {
        c.content_type.as_ref().map(|t| t.ty().to_string()).unwrap_or_default()
    };
    all.iter()
        .find(|c| matches!(kind(c).as_str(), "audio" | "video"))
        .or_else(|| all.iter().find(|c| kind(c) != "image"))
        .or_else(|| all.first())
        .copied()
}

/// Insert everything new from `parsed` into `feed_id`. One transaction.
pub fn ingest(
    db: &mut Db,
    feed_id: i64,
    parsed: &ParsedFeed,
    rules: &IngestRules,
) -> Result<IngestReport, DbError> {
    let mut report = IngestReport::default();
    let now = Utc::now();
    let received = fmt_ts(&now);

    // Loaded once per poll, not once per article: compiling a regex for every
    // entry of every feed is the kind of cost that only shows up at 200 feeds.
    let engine = Engine::load(db.conn())?;

    let tx = db.conn_mut().transaction()?;

    // A guid repeated inside one document names two different items (they
    // differ in title and link). Matching both on it kept one row that flipped
    // between them on every poll, discarding its cached article each time.
    // The second and later ones are identified without the guid.
    let mut guids_seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for entry in &parsed.entries {
        // A date in the future is a wrong clock or a wrong time zone, and it
        // would sit at the top of every list until the date passed.
        let dated = entry.published.or(entry.updated);
        let clamped = dated.is_some_and(|p| p > now + chrono::Duration::hours(1));
        let published: Option<DateTime<Utc>> = if clamped { Some(now) } else { dated };

        if !rules.add_any_date {
            if let (Some(p), Some(cutoff)) = (published, rules.avoid_old_before) {
                if p < cutoff {
                    report.too_old += 1;
                    continue;
                }
            }
        }

        let published_s = published.as_ref().map(fmt_ts);
        let title = entry_title(entry);
        let link = entry_link(entry);
        let guid = (!entry.id.trim().is_empty())
            .then(|| entry.id.clone())
            .filter(|g| guids_seen.insert(g.clone()));
        let (summary, content) = entry_body(entry);

        let existing = find_existing(
            &tx,
            feed_id,
            guid.as_deref(),
            link.as_deref(),
            title.as_deref(),
            published_s.as_deref().filter(|_| !clamped),
            clamped,
            summary.as_deref(),
        )?;

        if let Some(id) = existing {
            report.duplicates += 1;
            if rules.apply_edits {
                // Only rewrite if something actually differs, so read/starred
                // state and the row's position are left alone in the common
                // case of an unchanged entry.
                //
                // Never a purged stub (deleted = 2). Cleanup emptied it to
                // free the space, and it stays only to be recognised; filling
                // it in again brought the body back on the next poll.
                let changed = tx.execute(
                    "UPDATE news SET title = ?1, description = ?2, content = ?3, modified = ?4
                     WHERE id = ?5 AND deleted <> 2
                       AND (IFNULL(title,'') <> IFNULL(?1,'')
                            OR IFNULL(description,'') <> IFNULL(?2,'')
                            OR IFNULL(content,'') <> IFNULL(?3,''))",
                    params![
                        title,
                        summary,
                        content,
                        entry.updated.as_ref().map(fmt_ts),
                        id
                    ],
                )?;
                if changed > 0 {
                    report.updated += 1;
                    // The extracted article is stale once the source text moved.
                    tx.execute(
                        "UPDATE news SET article_html = NULL, article_fetched = NULL WHERE id = ?1",
                        [id],
                    )?;
                }
            }
            continue;
        }

        let author = entry.authors.first().map(|a| a.name.clone());
        let categories = if entry.categories.is_empty() {
            None
        } else {
            Some(
                entry
                    .categories
                    .iter()
                    .map(|c| c.label.clone().unwrap_or_else(|| c.term.clone()))
                    .collect::<Vec<_>>()
                    .join("\t"),
            )
        };
        let enclosure = entry_enclosure(entry);

        tx.execute(
            "INSERT INTO news (
                feed_id, guid, guid_is_link, title, description, content,
                published, modified, received, author_name, category,
                link_href, link_alternate, enclosure_url, enclosure_type, enclosure_length,
                new, read, starred, deleted
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6,
                ?7, ?8, ?9, ?10, ?11,
                ?12, ?12, ?13, ?14, ?15,
                1, 0, 0, 0
             )",
            params![
                feed_id,
                guid,
                guid.as_deref()
                    .map(|g| g.starts_with("http") as i64)
                    .unwrap_or(0),
                title,
                summary,
                content,
                published_s,
                entry.updated.as_ref().map(fmt_ts),
                received,
                author,
                categories,
                link,
                enclosure.and_then(|c| c.url.as_ref().map(|u| u.to_string())),
                enclosure.and_then(|c| c.content_type.as_ref().map(|t| t.to_string())),
                enclosure.and_then(|c| c.size.map(|s| s as i64)),
            ],
        )?;
        report.inserted += 1;

        // Filters run here, on the row that was just written, inside the same
        // transaction. Doing it after the commit would mean an article the
        // user asked to be deleted on arrival is briefly visible and briefly
        // counted as unread, which is exactly what the rule was meant to stop.
        //
        // Only new rows are considered. A re-poll that updates an existing
        // article does not re-run filters over it, because that would undo a
        // read or starred state the user set by hand.
        if !engine.is_empty() {
            let news_id = tx.last_insert_rowid();
            // A condition on "description" should see whatever body the feed
            // carried, whether it arrived as a summary or as content. Feeds
            // differ on which they populate, and a filter should not.
            let body = match (summary.as_deref(), content.as_deref()) {
                (Some(d), Some(c)) => Some(format!("{d}\n{c}")),
                (d, c) => d.or(c).map(str::to_string),
            };
            let eff = engine.evaluate(&Candidate {
                feed_id,
                title: title.as_deref(),
                description: body.as_deref(),
                author: author.as_deref(),
                category: categories.as_deref(),
                link: link.as_deref(),
                new: true,
                read: false,
                starred: false,
            });
            if !eff.matched.is_empty() {
                report.filtered += 1;
                if eff.delete {
                    report.filtered_away += 1;
                }
                report.sounds.extend(eff.sounds.iter().filter(|p| !p.trim().is_empty()).cloned());
                if !eff.notify.is_empty() {
                    report.notify.push(title.clone().unwrap_or_else(|| "(untitled)".into()));
                }
                filters::apply(&tx, news_id, &eff)?;
            }
        }
    }

    tx.commit()?;
    // This feed only: recounting every feed after each one made a poll of
    // 150 feeds take most of a minute on a large database, holding the lock
    // the whole time.
    db.recompute_counters_for(&[feed_id])?;
    Ok(report)
}
