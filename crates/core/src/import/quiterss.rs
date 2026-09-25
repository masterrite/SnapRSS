//! Import a QuiteRSS `feeds.db` into a SnapRSS database.
//!
//! The source file is opened read-only and never written to. Everything lands
//! inside one transaction: either the whole import succeeds or the target is
//! untouched.
//!
//! Two structural differences between the schemas need real work rather than a
//! column rename:
//!
//! 1. QuiteRSS has no folder/feed discriminator. A row is a folder if
//!    `xmlUrl` is empty. `hasChildren` is unreliable — an emptied folder keeps
//!    `hasChildren = 1` — so it is not used here.
//! 2. Labels are stored as a comma-joined string of label ids in `news.label`,
//!    with leading and trailing commas (",1,4,"). Those become rows in
//!    `news_labels`.

use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::Path;

use crate::db::{Db, DbError};
use crate::models::ImportReport;

/// True if the file looks like a QuiteRSS database. Cheap enough to call
/// before showing the user an import button.
pub fn looks_like_quiterss(path: impl AsRef<Path>) -> bool {
    let Ok(conn) = Db::open_foreign_readonly(path) else {
        return false;
    };
    // SnapRSS's own database has tables with the same names; QuiteRSS's
    // `feeds` has `parentId` where SnapRSS has `parent_id`.
    has_tables(&conn, &["feeds", "news"]).unwrap_or(false)
        && Columns::load(&conn, "feeds").is_ok_and(|c| c.has("parentId"))
}

/// True if the file is a SnapRSS database, which the importer cannot read.
pub fn looks_like_snaprss(path: impl AsRef<Path>) -> bool {
    let Ok(conn) = Db::open_foreign_readonly(path) else {
        return false;
    };
    has_tables(&conn, &["feeds", "news"]).unwrap_or(false)
        && Columns::load(&conn, "feeds").is_ok_and(|c| c.has("parent_id"))
}

/// An integer column, whatever SQLite actually stored in it. QuiteRSS
/// declares many columns `varchar` or with no type at all, so numbers arrive
/// as text ("12345" in `enclosure_length`), and reading one as an integer
/// failed the whole import.
fn int<I: rusqlite::RowIndex>(r: &rusqlite::Row<'_>, col: I) -> rusqlite::Result<Option<i64>> {
    use rusqlite::types::ValueRef;
    Ok(match r.get_ref(col)? {
        ValueRef::Null | ValueRef::Blob(_) => None,
        ValueRef::Integer(i) => Some(i),
        ValueRef::Real(f) => Some(f as i64),
        ValueRef::Text(t) => std::str::from_utf8(t).ok().and_then(|t| t.trim().parse().ok()),
    })
}

/// A text column, whatever SQLite actually stored in it.
fn text<I: rusqlite::RowIndex>(r: &rusqlite::Row<'_>, col: I) -> rusqlite::Result<Option<String>> {
    use rusqlite::types::ValueRef;
    Ok(match r.get_ref(col)? {
        ValueRef::Null => None,
        ValueRef::Integer(i) => Some(i.to_string()),
        ValueRef::Real(f) => Some(f.to_string()),
        ValueRef::Text(t) | ValueRef::Blob(t) => Some(String::from_utf8_lossy(t).into_owned()),
    })
}

fn blob<I: rusqlite::RowIndex>(r: &rusqlite::Row<'_>, col: I) -> rusqlite::Result<Option<Vec<u8>>> {
    use rusqlite::types::ValueRef;
    Ok(match r.get_ref(col)? {
        ValueRef::Text(b) | ValueRef::Blob(b) => Some(b.to_vec()),
        _ => None,
    })
}

/// QuiteRSS writes timestamps as `2026-09-16T10:00:00`, UTC without a zone.
/// Ingestion writes `2026-09-16T10:00:00Z`; unless the two agree, no
/// date-based identity rule can match an imported article.
fn utc(v: Option<String>) -> Option<String> {
    let v = v?;
    let t = v.trim();
    if let Ok(d) = chrono::DateTime::parse_from_rfc3339(t) {
        return Some(
            d.with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        );
    }
    if let Ok(n) = chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S") {
        return Some(n.and_utc().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    }
    Some(v)
}

fn has_tables(conn: &Connection, names: &[&str]) -> Result<bool, rusqlite::Error> {
    for name in names {
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [name],
                |r| r.get(0),
            )
            .optional()?;
        if found.is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Which optional columns this particular QuiteRSS file actually has.
/// Older databases predate several `ALTER TABLE` migrations, so probe rather
/// than assume.
struct Columns {
    set: Vec<String>,
}

impl Columns {
    fn load(conn: &Connection, table: &str) -> Result<Self, rusqlite::Error> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let set = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Columns { set })
    }

    fn has(&self, name: &str) -> bool {
        self.set.iter().any(|c| c == name)
    }

    /// `col` if present, otherwise the literal `NULL`, so one SELECT works
    /// across schema versions.
    fn pick(&self, name: &str) -> String {
        if self.has(name) {
            name.to_string()
        } else {
            format!("NULL AS {name}")
        }
    }
}

pub fn import_quiterss(db: &mut Db, source: impl AsRef<Path>) -> Result<ImportReport, DbError> {
    let path = source.as_ref().to_path_buf();
    let display = path.display().to_string();
    let src = Db::open_foreign_readonly(&path)?;

    if !has_tables(&src, &["feeds", "news"]).unwrap_or(false) {
        return Err(DbError::NotQuiteRss(display, "no feeds/news tables"));
    }

    let mut report = ImportReport::default();
    let tx = db.conn.transaction()?;

    // --- feeds and folders -------------------------------------------------
    // Old id -> new id. QuiteRSS ids are dense and we could reuse them, but a
    // remap means importing into a database that already has feeds works too.
    let mut id_map: HashMap<i64, i64> = HashMap::new();
    let fc = Columns::load(&src, "feeds")?;

    // What was here before, so importing the same file twice is a no-op
    // rather than a second copy of everything.
    let existing_urls: HashMap<String, i64> = {
        let mut stmt = tx.prepare("SELECT xml_url, id FROM feeds WHERE kind = 1 AND xml_url IS NOT NULL")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    let pre_max_id: i64 = tx.query_row("SELECT COALESCE(MAX(id), 0) FROM feeds", [], |r| r.get(0))?;
    // Imported top-level items go after the existing ones, not interleaved.
    let root_offset: i64 = tx.query_row(
        "SELECT COALESCE(MAX(row_to_parent), -1) + 1 FROM feeds WHERE parent_id IS NULL",
        [],
        |r| r.get(0),
    )?;
    let mut already_subscribed: std::collections::HashSet<i64> = Default::default();

    let sql = format!(
        "SELECT id, parentId, rowToParent, text, title, description, xmlUrl, htmlUrl,
                language, image, currentNews, {expanded},
                updateIntervalEnable, updateInterval, updateIntervalType,
                updateOnStartup, displayOnStartup, {disable_update},
                displayNews, {load_types}, {embedded}, {rtl},
                markReadAfterSecondsEnable, markReadAfterSeconds, markDisplayedOnSwitchingFeed,
                layout, filter, groupBy, columns, sort, sortType,
                maximumToKeep, maximumToKeepEnable, maximumAgeOfNews, maximumAgoOfNewEnable,
                deleteReadNews, neverDeleteUnreadNews, neverDeleteStarredNews, neverDeleteLabeledNews,
                {dup_mode}, {any_date}, {avoid_on}, {avoid_date},
                status, {auth}, {notify}, created, updated, lastDisplayed
         FROM feeds ORDER BY parentId, rowToParent, id",
        expanded = fc.pick("f_Expanded"),
        disable_update = fc.pick("disableUpdate"),
        load_types = fc.pick("loadTypes"),
        embedded = fc.pick("displayEmbeddedImages"),
        rtl = fc.pick("layoutDirection"),
        dup_mode = fc.pick("duplicateNewsMode"),
        any_date = fc.pick("addSingleNewsAnyDateOn"),
        avoid_on = fc.pick("avoidedOldSingleNewsDateOn"),
        avoid_date = fc.pick("avoidedOldSingleNewsDate"),
        auth = fc.pick("authentication"),
        notify = fc.pick("showNotification"),
    );

    // Two passes: insert every node with a null parent, then wire parents up.
    // A single pass would need the tree to be topologically ordered, and a
    // hand-edited database is not guaranteed to be.
    let mut pending_parent: Vec<(i64, i64)> = Vec::new();

    {
        let mut stmt = src.prepare(&sql)?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let old_id: i64 = r.get("id")?;
            let old_parent: i64 = int(r, "parentId")?.unwrap_or(0);
            let xml_url: Option<String> = text(r, "xmlUrl")?;

            let is_feed = xml_url
                .as_deref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            let kind = if is_feed { 1i64 } else { 0i64 };

            if is_feed && xml_url.as_deref().is_some_and(|u| existing_urls.contains_key(u.trim())) {
                already_subscribed.insert(old_id);
                report.duplicates += 1;
                continue;
            }

            // displayNews: 0 = show content from the news, 1 = fetch the link.
            // That maps onto reading_mode inverted: fetching the link is our
            // FullArticle (0), showing feed content is DescriptionOnly (1).
            let display_news: Option<i64> = int(r, "displayNews")?;
            let reading_mode = match display_news {
                Some(1) => 0i64,
                Some(_) => 1i64,
                None => 0i64,
            };

            // loadTypes is a space separated list like "images sounds".
            let load_types: Option<String> = text(r, "loadTypes")?;
            let load_images = load_types
                .as_deref()
                .map(|s| s.contains("images"))
                .unwrap_or(true) as i64;

            tx.execute(
                "INSERT INTO feeds (
                    kind, parent_id, row_to_parent, expanded,
                    text, title, description, xml_url, html_url, language, image, current_news,
                    update_interval_enable, update_interval, update_interval_type,
                    update_on_startup, display_on_startup, disable_update,
                    reading_mode, open_on_enter, load_images, embedded_images, layout_direction,
                    mark_read_after_enable, mark_read_after_seconds, mark_read_on_switch,
                    layout, filter, group_by, columns, sort, sort_type,
                    max_to_keep, max_to_keep_enable, max_age_days, max_age_enable,
                    delete_read, never_delete_unread, never_delete_starred, never_delete_labeled,
                    duplicate_news_mode, add_any_date, avoid_old_enable, avoid_old_before,
                    status, authentication, show_notification, created, updated, last_displayed
                 ) VALUES (
                    ?1, NULL, ?2, ?3,
                    ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                    ?12, ?13, ?14,
                    ?15, ?16, ?17,
                    ?18, 1, ?19, ?20, ?21,
                    ?22, ?23, ?24,
                    ?25, ?26, ?27, ?28, ?29, ?30,
                    ?31, ?32, ?33, ?34,
                    ?35, ?36, ?37, ?38,
                    ?39, ?40, ?41, ?42,
                    ?43, ?44, ?45, ?46, ?47, ?48
                 )",
                params![
                    kind,
                    int(r, "rowToParent")?.unwrap_or(0) + if old_parent == 0 { root_offset } else { 0 },
                    int(r, "f_Expanded")?.unwrap_or(1),
                    text(r, "text")?,
                    text(r, "title")?,
                    text(r, "description")?,
                    xml_url,
                    text(r, "htmlUrl")?,
                    text(r, "language")?,
                    blob(r, "image")?,
                    int(r, "currentNews")?,
                    // -1 is QuiteRSS for "use the global interval".
                    int(r, "updateIntervalEnable")?.unwrap_or(0).max(0),
                    int(r, "updateInterval")?,
                    // QuiteRSS stores the unit as a code: -1 seconds, 0
                    // minutes, 1 hours. Read as minutes, an hourly feed was
                    // polled sixty times too often.
                    text(r, "updateIntervalType")?.map(|t| match t.trim() {
                        "-1" => "seconds".to_string(),
                        "0" => "minutes".to_string(),
                        "1" => "hours".to_string(),
                        _ => t,
                    }),
                    int(r, "updateOnStartup")?.unwrap_or(1),
                    int(r, "displayOnStartup")?.unwrap_or(0),
                    int(r, "disableUpdate")?.unwrap_or(0),
                    reading_mode,
                    load_images,
                    int(r, "displayEmbeddedImages")?
                        .unwrap_or(1),
                    int(r, "layoutDirection")?.unwrap_or(0),
                    int(r, "markReadAfterSecondsEnable")?
                        .unwrap_or(0),
                    int(r, "markReadAfterSeconds")?,
                    int(r, "markDisplayedOnSwitchingFeed")?
                        .unwrap_or(0),
                    text(r, "layout")?,
                    text(r, "filter")?,
                    int(r, "groupBy")?,
                    text(r, "columns")?,
                    text(r, "sort")?,
                    int(r, "sortType")?,
                    int(r, "maximumToKeep")?,
                    int(r, "maximumToKeepEnable")?.unwrap_or(0),
                    int(r, "maximumAgeOfNews")?,
                    int(r, "maximumAgoOfNewEnable")?
                        .unwrap_or(0),
                    int(r, "deleteReadNews")?.unwrap_or(0),
                    int(r, "neverDeleteUnreadNews")?
                        .unwrap_or(1),
                    int(r, "neverDeleteStarredNews")?
                        .unwrap_or(1),
                    int(r, "neverDeleteLabeledNews")?
                        .unwrap_or(1),
                    int(r, "duplicateNewsMode")?.unwrap_or(0),
                    int(r, "addSingleNewsAnyDateOn")?
                        .unwrap_or(1),
                    int(r, "avoidedOldSingleNewsDateOn")?
                        .unwrap_or(0),
                    text(r, "avoidedOldSingleNewsDate")?,
                    text(r, "status")?,
                    int(r, "authentication")?.unwrap_or(0),
                    int(r, "showNotification")?.unwrap_or(0),
                    text(r, "created")?,
                    text(r, "updated")?,
                    text(r, "lastDisplayed")?,
                ],
            )?;

            let new_id = tx.last_insert_rowid();
            id_map.insert(old_id, new_id);
            if old_parent != 0 {
                pending_parent.push((new_id, old_parent));
            }

            if is_feed {
                report.feeds += 1;
            } else {
                report.folders += 1;
            }
        }
    }

    for (new_id, old_parent) in pending_parent {
        match id_map.get(&old_parent) {
            Some(&new_parent) => {
                tx.execute(
                    "UPDATE feeds SET parent_id = ?1 WHERE id = ?2",
                    params![new_parent, new_id],
                )?;
            }
            None => {
                // Parent row is missing. Keep the node rather than dropping the
                // feed and its articles on the floor, but append it to the end
                // of the root so a recovered feed does not shove itself into
                // the middle of an ordering the user chose.
                let next: i64 = tx.query_row(
                    "SELECT COALESCE(MAX(row_to_parent), -1) + 1 FROM feeds WHERE parent_id IS NULL",
                    [],
                    |r| r.get(0),
                )?;
                tx.execute(
                    "UPDATE feeds SET parent_id = NULL, row_to_parent = ?1 WHERE id = ?2",
                    params![next, new_id],
                )?;
                report.skipped.push(format!(
                    "feed {new_id}: parent {old_parent} missing, moved to root"
                ));
            }
        }
    }

    // A parent chain that loops (1 -> 2 -> 1, from a hand-edited file) hangs
    // off nothing and would never be shown. Put such nodes at the root.
    let new_ids: Vec<i64> = id_map.values().copied().collect();
    for &id in &new_ids {
        let mut seen = std::collections::HashSet::new();
        let mut cur = Some(id);
        let mut looped = false;
        while let Some(c) = cur {
            if !seen.insert(c) {
                looped = true;
                break;
            }
            cur = tx
                .query_row("SELECT parent_id FROM feeds WHERE id = ?1", [c], |r| r.get(0))
                .optional()?
                .flatten();
        }
        if looped {
            let next: i64 = tx.query_row(
                "SELECT COALESCE(MAX(row_to_parent), -1) + 1 FROM feeds WHERE parent_id IS NULL",
                [],
                |r| r.get(0),
            )?;
            tx.execute(
                "UPDATE feeds SET parent_id = NULL, row_to_parent = ?1 WHERE id = ?2",
                params![next, id],
            )?;
            report.skipped.push(format!("feed {id}: its folders form a loop, moved to root"));
        }
    }

    // An imported folder with the same name and place as one already here is
    // the same folder: its contents move in and it goes. Repeated until
    // nothing merges, so nested folders merge level by level.
    merge_duplicate_folders(&tx, pre_max_id)?;

    // --- labels ------------------------------------------------------------
    let mut label_map: HashMap<i64, i64> = HashMap::new();
    if has_tables(&src, &["labels"]).unwrap_or(false) {
        {
            let mut stmt = src.prepare(
                "SELECT id, name, image, color_text, color_bg, num FROM labels ORDER BY num, id",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let name = text(r, 1)?.unwrap_or_default();
                let same: Option<i64> = tx
                    .query_row("SELECT id FROM labels WHERE name = ?1", [&name], |r| r.get(0))
                    .optional()?;
                if let Some(existing) = same {
                    label_map.insert(r.get::<_, i64>(0)?, existing);
                    continue;
                }
                tx.execute(
                    "INSERT INTO labels (name, image, color_text, color_bg, num)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        name,
                        blob(r, 2)?,
                        text(r, 3)?,
                        text(r, 4)?,
                        int(r, 5)?.unwrap_or(0),
                    ],
                )?;
                label_map.insert(r.get::<_, i64>(0)?, tx.last_insert_rowid());
                report.labels += 1;
            }
        }
    }

    // --- news --------------------------------------------------------------
    let nc = Columns::load(&src, "news")?;
    let news_sql = format!(
        "SELECT id, feedId, guid, guidislink, title, description, {content},
                published, modified, received,
                author_name, author_uri, author_email, category, label,
                link_href, link_alternate, comments, source, rights,
                enclosure_url, enclosure_type, enclosure_length,
                new, read, starred, deleted, {delete_date}
         FROM news",
        content = nc.pick("content"),
        delete_date = nc.pick("deleteDate"),
    );

    {
        let mut stmt = src.prepare(&news_sql)?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let old_feed: i64 = int(r, "feedId")?.unwrap_or(0);
            if already_subscribed.contains(&old_feed) {
                continue;
            }
            let Some(&feed_id) = id_map.get(&old_feed) else {
                report
                    .skipped
                    .push(format!("news row for unknown feed {old_feed}"));
                continue;
            };

            // guidislink is stored as the text 'true'/'false', not an integer.
            let guid_is_link = match text(r, "guidislink")? {
                Some(s) => (s.eq_ignore_ascii_case("true") || s == "1") as i64,
                None => 1,
            };

            let received: String = utc(text(r, "received")?)
                .or(utc(text(r, "published")?))
                .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

            tx.execute(
                "INSERT INTO news (
                    feed_id, guid, guid_is_link, title, description, content,
                    published, modified, received,
                    author_name, author_uri, author_email, category,
                    link_href, link_alternate, comments, source, rights,
                    enclosure_url, enclosure_type, enclosure_length,
                    new, read, starred, deleted, delete_date
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6,
                    ?7, ?8, ?9,
                    ?10, ?11, ?12, ?13,
                    ?14, ?15, ?16, ?17, ?18,
                    ?19, ?20, ?21,
                    ?22, ?23, ?24, ?25, ?26
                 )",
                params![
                    feed_id,
                    text(r, "guid")?,
                    guid_is_link,
                    text(r, "title")?,
                    text(r, "description")?,
                    text(r, "content")?,
                    utc(text(r, "published")?),
                    utc(text(r, "modified")?),
                    received,
                    text(r, "author_name")?,
                    text(r, "author_uri")?,
                    text(r, "author_email")?,
                    text(r, "category")?,
                    text(r, "link_href")?,
                    text(r, "link_alternate")?,
                    text(r, "comments")?,
                    text(r, "source")?,
                    text(r, "rights")?,
                    text(r, "enclosure_url")?,
                    text(r, "enclosure_type")?,
                    int(r, "enclosure_length")?,
                    int(r, "new")?.unwrap_or(0),
                    // QuiteRSS marks read articles 1 or 2; SnapRSS uses 1.
                    int(r, "read")?.unwrap_or(0).clamp(0, 1),
                    int(r, "starred")?.unwrap_or(0),
                    int(r, "deleted")?.unwrap_or(0),
                    text(r, "deleteDate")?,
                ],
            )?;
            let news_id = tx.last_insert_rowid();
            report.news += 1;

            // ",1,4," -> label ids 1 and 4
            if let Some(raw) = text(r, "label")? {
                for part in raw.split(',').filter(|s| !s.trim().is_empty()) {
                    let Ok(old_label) = part.trim().parse::<i64>() else {
                        continue;
                    };
                    if let Some(&label_id) = label_map.get(&old_label) {
                        tx.execute(
                            "INSERT OR IGNORE INTO news_labels(news_id, label_id) VALUES(?1, ?2)",
                            params![news_id, label_id],
                        )?;
                        report.label_links += 1;
                    }
                }
            }
        }
    }

    // --- filters, conditions, actions --------------------------------------
    if has_tables(&src, &["filters"]).unwrap_or(false) {
        let mut filter_map: HashMap<i64, i64> = HashMap::new();
        let mut filters_already_here: std::collections::HashSet<i64> = Default::default();
        {
            let mut stmt = src.prepare(
                "SELECT id, name, type, feeds, enable, num FROM filters ORDER BY num, id",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                // QuiteRSS writes ",3,7,12," and queries it with
                // `feeds LIKE '%,<id>,%'`, so the separator is a comma and the
                // string is comma-wrapped. Splitting on tab here silently
                // produced a one-element list that never remapped, and the
                // filter then applied to nothing.
                //
                // NULL stays NULL, which this engine reads as "every feed".
                // A list that remaps to nothing stays an (empty) list rather
                // than becoming NULL: a filter whose feeds all vanished should
                // apply to no feed, not to all of them.
                let feeds = text(r, 3)?.map(|s| {
                    let ids: Vec<i64> = s
                        .split([',', '\t'])
                        .filter_map(|p| p.trim().parse::<i64>().ok())
                        .filter_map(|old| id_map.get(&old).copied())
                        .collect();
                    crate::filters::format_feed_list(&ids)
                });
                let name = text(r, 1)?.unwrap_or_default();
                let here: Option<i64> = tx
                    .query_row("SELECT id FROM filters WHERE name = ?1", [&name], |r| r.get(0))
                    .optional()?;
                if here.is_some() {
                    filters_already_here.insert(r.get::<_, i64>(0)?);
                    continue;
                }
                tx.execute(
                    "INSERT INTO filters (name, type, feeds, enable, num)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        name,
                        int(r, 2)?.unwrap_or(0),
                        feeds,
                        int(r, 4)?.unwrap_or(1),
                        int(r, 5)?.unwrap_or(0),
                    ],
                )?;
                filter_map.insert(r.get::<_, i64>(0)?, tx.last_insert_rowid());
                report.filters += 1;
            }
        }

        if has_tables(&src, &["filterConditions"]).unwrap_or(false) {
            let mut stmt =
                src.prepare("SELECT idFilter, field, condition, content FROM filterConditions")?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let old = r.get::<_, i64>(0)?;
                let Some(&fid) = filter_map.get(&old) else {
                    if !filters_already_here.contains(&old) {
                        report.skipped.push("filter condition with no filter".into());
                    }
                    continue;
                };
                tx.execute(
                    "INSERT INTO filter_conditions (filter_id, field, condition, content)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        fid,
                        text(r, 1)?.unwrap_or_default(),
                        text(r, 2)?.unwrap_or_default(),
                        text(r, 3)?,
                    ],
                )?;
                report.conditions += 1;
            }
        }

        if has_tables(&src, &["filterActions"]).unwrap_or(false) {
            let mut stmt = src.prepare("SELECT idFilter, action, params FROM filterActions")?;
            let mut rows = stmt.query([])?;
            while let Some(r) = rows.next()? {
                let old = r.get::<_, i64>(0)?;
                let Some(&fid) = filter_map.get(&old) else {
                    if !filters_already_here.contains(&old) {
                        report.skipped.push("filter action with no filter".into());
                    }
                    continue;
                };
                let action = text(r, 1)?.unwrap_or_default();
                let mut params_v = text(r, 2)?;
                // "Add label" (index 3) names a label by its QuiteRSS id, and
                // labels are renumbered on the way in. Copied as it was, the
                // action pointed at a label that does not exist, or at the
                // wrong one.
                if matches!(action.trim(), "3" | "add_label" | "label") {
                    let mapped = params_v
                        .as_deref()
                        .and_then(|p| p.trim().parse::<i64>().ok())
                        .and_then(|old| label_map.get(&old).copied());
                    match mapped {
                        Some(id) => params_v = Some(id.to_string()),
                        None => {
                            report.skipped.push(format!(
                                "filter action: label {} not found",
                                params_v.as_deref().unwrap_or("")
                            ));
                            continue;
                        }
                    }
                }
                tx.execute(
                    "INSERT INTO filter_actions (filter_id, action, params) VALUES (?1, ?2, ?3)",
                    params![fid, action, params_v],
                )?;
                report.actions += 1;
            }
        }
    }

    // --- passwords ---------------------------------------------------------
    if has_tables(&src, &["passwords"]).unwrap_or(false) {
        let mut stmt = src.prepare("SELECT server, username, password FROM passwords")?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            tx.execute(
                "INSERT INTO passwords (server, username, password)
                 SELECT ?1, ?2, ?3
                 WHERE NOT EXISTS (SELECT 1 FROM passwords WHERE server = ?1 AND username IS ?2)",
                params![
                    text(r, 0)?.unwrap_or_default(),
                    text(r, 1)?,
                    text(r, 2)?,
                ],
            )?;
            report.passwords += 1;
        }
    }

    tx.commit()?;
    db.recompute_counters()?;

    Ok(report)
}

/// Fold each folder created by this import (`id > pre_max_id`) into an older
/// folder with the same name under the same parent. Shared by both importers.
pub(crate) fn merge_duplicate_folders(
    tx: &rusqlite::Transaction<'_>,
    pre_max_id: i64,
) -> Result<(), rusqlite::Error> {
    loop {
        let pair: Option<(i64, i64)> = tx
            .query_row(
                "SELECT n.id, o.id FROM feeds n JOIN feeds o
                   ON o.kind = 0 AND n.kind = 0
                  AND o.id <= ?1 AND n.id > ?1
                  AND o.parent_id IS n.parent_id
                  AND TRIM(o.text) = TRIM(n.text)
                 ORDER BY n.id LIMIT 1",
                [pre_max_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((new, old)) = pair else { return Ok(()) };
        let base: i64 = tx.query_row(
            "SELECT COALESCE(MAX(row_to_parent), -1) + 1 FROM feeds WHERE parent_id = ?1",
            [old],
            |r| r.get(0),
        )?;
        tx.execute(
            "UPDATE feeds SET parent_id = ?1, row_to_parent = row_to_parent + ?2 WHERE parent_id = ?3",
            params![old, base, new],
        )?;
        // Now-empty imported folders that were children of the merged one
        // become candidates for the next round, so the id bound is kept:
        // they still count as new.
        tx.execute("DELETE FROM feeds WHERE id = ?1", [new])?;
    }
}
