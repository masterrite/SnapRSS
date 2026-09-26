//! The bridge between the webview and the crates that do the work.
//!
//! Commands are thin on purpose: validate, call into core/fetch/article, map
//! the error. Anything with logic in it belongs in one of those crates where it
//! can be tested without starting a window.
//!
//! The database is behind a `tokio::sync::Mutex` rather than a `std` one
//! because these are async commands and holding a std guard across an `.await`
//! would be a deadlock waiting to happen.

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use snaprss_core::models::{NodeKind, ReadingMode};
use snaprss_core::{Db, DbError};
use snaprss_fetch::{FetchConfig, UpdateSummary};
use tauri::State;
use tokio::sync::Mutex;

pub struct AppState {
    pub db: Arc<Mutex<Db>>,
    pub http: reqwest::Client,
    pub fetch_cfg: FetchConfig,
    /// Read by the window's close handler, which is synchronous and cannot
    /// wait on the database lock.
    pub close_to_tray: Arc<std::sync::atomic::AtomicBool>,
    /// Set just before a deliberate quit, so the close handler lets it through.
    pub quitting: Arc<std::sync::atomic::AtomicBool>,
    /// The update found by the last check, ready to install.
    pub pending_update: Arc<std::sync::Mutex<Option<tauri_plugin_updater::Update>>>,
}

/// Errors cross into JavaScript as a string. Detail is fine — this is a local
/// app and the alternative is a user staring at "an error occurred".
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("{0}")]
    Db(String),
    #[error("{0}")]
    Invalid(String),
}

impl From<DbError> for CommandError {
    fn from(e: DbError) -> Self {
        CommandError::Db(e.to_string())
    }
}

impl Serialize for CommandError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

pub type Res<T> = Result<T, CommandError>;

// ---------------------------------------------------------------- view models

#[derive(Debug, Serialize)]
pub struct TreeNode {
    pub id: i64,
    pub is_folder: bool,
    pub parent_id: Option<i64>,
    pub title: String,
    pub unread: i64,
    pub broken: bool,
    /// The context menu offers "copy feed address" and "open site", and shows
    /// which reading mode is ticked, so the tree carries them.
    pub xml_url: Option<String>,
    pub html_url: Option<String>,
    pub reading_mode: i64,
    pub expanded: bool,
    pub children: Vec<TreeNode>,
}

#[derive(Debug, Serialize)]
pub struct ListItem {
    pub id: i64,
    pub feed_id: i64,
    pub feed_title: String,
    pub title: String,
    pub author: Option<String>,
    pub published: Option<String>,
    pub link: Option<String>,
    pub read: bool,
    pub starred: bool,
    /// Plain text, for the newspaper layout. Never HTML.
    pub excerpt: String,
    pub labels: Vec<i64>,
}

#[derive(Debug, Serialize)]
pub struct ArticleView {
    pub id: i64,
    pub title: String,
    pub byline: Option<String>,
    pub feed_title: String,
    pub published: Option<String>,
    pub link: Option<String>,
    /// Sanitised. Safe to assign to innerHTML.
    pub html: String,
    pub read_minutes: u32,
    pub starred: bool,
    /// True when the body came from the feed rather than the linked page.
    pub from_feed: bool,
    /// The full article is being fetched in the background; `article-ready`
    /// will arrive with this id when it is done.
    pub pending: bool,
}

#[derive(Debug, Serialize)]
pub struct Counts {
    pub unread: i64,
    pub total: i64,
    pub starred: i64,
}

// -------------------------------------------------------------------- reading

#[tauri::command]
pub async fn feed_tree(state: State<'_, AppState>) -> Res<Vec<TreeNode>> {
    let db = state.db.lock().await;
    Ok(build_tree(&db, None)?)
}

fn build_tree(db: &Db, parent: Option<i64>) -> Result<Vec<TreeNode>, DbError> {
    let mut out = Vec::new();
    for node in db.children(parent)? {
        let children = build_tree(db, Some(node.id))?;
        // A folder shows the sum of what is inside it.
        let unread = node.unread + children.iter().map(|c| c.unread).sum::<i64>();
        out.push(TreeNode {
            id: node.id,
            is_folder: node.kind == NodeKind::Folder,
            parent_id: node.parent_id,
            title: node.text.clone().unwrap_or_else(|| "Untitled".into()),
            broken: node.is_broken(),
            xml_url: node.xml_url.clone().filter(|u| !u.is_empty()),
            html_url: node.html_url.clone().filter(|u| !u.is_empty()),
            reading_mode: node.reading_mode.as_i64(),
            expanded: node.expanded,
            unread,
            children,
        });
    }
    Ok(out)
}

/// `scope` is one of: `all`, `unread`, `starred`, `deleted`, `feed:<id>`,
/// `folder:<id>`, `label:<id>`.
///
/// Pages through the whole scope: `offset` skips rows already shown, so
/// every article can be reached, not only the newest page. `query` searches
/// title, author and feed name across the whole scope in SQL; filtering
/// the loaded page in the browser could only ever find what was on it.
/// `sort` is `date`, `title`, `author` or `feed`, with `-` in front for
/// descending (`-date`, newest first, is the default).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn news_list(
    state: State<'_, AppState>,
    scope: String,
    limit: Option<i64>,
    offset: Option<i64>,
    excerpts: Option<bool>,
    query: Option<String>,
    sort: Option<String>,
) -> Res<Vec<ListItem>> {
    let db = state.db.lock().await;
    let limit = limit.unwrap_or(500).clamp(1, 20_000);
    let offset = offset.unwrap_or(0).max(0);

    // Only the newspaper layout shows excerpts. Building them means reading
    // each article's body, which for feeds that carry full content can be
    // tens of kilobytes a row, so the classic layout does not pay for it, and
    // the newspaper one reads only the first 2 KB — an excerpt needs no more.
    let body_col = if excerpts.unwrap_or(false) {
        "substr(COALESCE(news.description, news.content, ''), 1, 2000)"
    } else {
        "''"
    };

    let (where_sql, param) = scope_filter(&scope)?;
    let mut params: Vec<rusqlite::types::Value> = param.into_iter().map(Into::into).collect();

    let mut search_sql = String::new();
    if let Some(q) = query.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        // Every word must appear somewhere in the title, author or feed name,
        // in any letter case and any script.
        for word in q.split_whitespace().take(8) {
            let word = word.to_lowercase();
            let escaped = word.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
            params.push(format!("%{escaped}%").into());
            let n = params.len();
            // SQLite's LIKE already ignores case for A to Z, and does it far
            // faster; the lower-casing that covers every script is only
            // needed for words with other letters in them.
            let fold = |col: &str| if word.is_ascii() { col.to_string() } else { format!("snap_lower({col})") };
            search_sql.push_str(&format!(
                " AND ({} LIKE ?{n} ESCAPE '\\' OR {} LIKE ?{n} ESCAPE '\\' OR {} LIKE ?{n} ESCAPE '\\')",
                fold("news.title"),
                fold("news.author_name"),
                fold("COALESCE(feeds.text, feeds.title, '')"),
            ));
        }
    }

    let order_sql = list_order(sort.as_deref());

    let sql = format!(
        "SELECT news.id, news.feed_id, COALESCE(feeds.text, feeds.title, ''),
                COALESCE(news.title, '(untitled)'), news.author_name,
                COALESCE(news.published, news.received), news.link_href,
                news.read, news.starred,
                {body_col}
         FROM news JOIN feeds ON feeds.id = news.feed_id
         WHERE {where_sql}{search_sql}
         ORDER BY {order_sql}
         LIMIT {limit} OFFSET {offset}"
    );

    let mut stmt = db.conn().prepare(&sql).map_err(DbError::from)?;
    let map = |r: &rusqlite::Row<'_>| {
        Ok(ListItem {
            id: r.get(0)?,
            feed_id: r.get(1)?,
            feed_title: r.get(2)?,
            title: r.get(3)?,
            author: r.get(4)?,
            published: r.get(5)?,
            link: r.get(6)?,
            read: r.get::<_, i64>(7)? != 0,
            starred: r.get::<_, i64>(8)? != 0,
            // The newspaper layout shows a couple of lines under each
            // headline. Built here rather than in the frontend because the
            // source is feed HTML, and the frontend must never be handed HTML
            // it did not get from the sanitiser.
            excerpt: excerpt_of(&r.get::<_, String>(9)?),
            labels: Vec::new(),
        })
    };
    let mut rows: Vec<ListItem> = stmt
        .query_map(rusqlite::params_from_iter(params), map)
        .map_err(DbError::from)?
        .collect::<Result<_, _>>()
        .map_err(DbError::from)?;

    // Labels in one query rather than one per row. The list is capped at a few
    // thousand rows, so loading the whole join for the visible feed and
    // matching in memory beats a correlated subquery per article.
    if !rows.is_empty() {
        let mut by_news: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut stmt = db
            .conn()
            .prepare("SELECT news_id, label_id FROM news_labels")
            .map_err(DbError::from)?;
        let pairs = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
            .map_err(DbError::from)?;
        for p in pairs {
            let (n, l) = p.map_err(DbError::from)?;
            by_news.entry(n).or_default().push(l);
        }
        for row in &mut rows {
            if let Some(ls) = by_news.remove(&row.id) {
                row.labels = ls;
            }
        }
    }

    Ok(rows)
}

/// ORDER BY for the list. Every order ends on the id so pages do not
/// overlap or skip rows that tie.
fn list_order(sort: Option<&str>) -> String {
    let sort = sort.unwrap_or("-date");
    let (desc, key) = match sort.strip_prefix('-') {
        Some(k) => (true, k),
        None => (false, sort),
    };
    let dir = if desc { "DESC" } else { "ASC" };
    let date = "COALESCE(news.published, news.received)";
    match key {
        "title" => format!("COALESCE(news.title, '') COLLATE NOCASE {dir}, {date} DESC, news.id DESC"),
        "author" => format!(
            "COALESCE(news.author_name, '') = '' ASC, COALESCE(news.author_name, '') COLLATE NOCASE {dir}, {date} DESC, news.id DESC"
        ),
        "feed" => format!(
            "COALESCE(feeds.text, feeds.title, '') COLLATE NOCASE {dir}, {date} DESC, news.id DESC"
        ),
        _ => format!("{date} {dir}, news.id {dir}"),
    }
}

/// First couple of sentences of a feed body, as plain text.
///
/// Tags are dropped rather than sanitised: this is going into a text node, so
/// the safe move is to have no markup at all in it.
fn excerpt_of(html: &str) -> String {
    let mut out = String::with_capacity(220);
    let mut in_tag = false;
    let mut last_was_space = true;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                if !last_was_space {
                    out.push(' ');
                    last_was_space = true;
                }
            }
            _ if in_tag => {}
            c if c.is_whitespace() => {
                if !last_was_space {
                    out.push(' ');
                    last_was_space = true;
                }
            }
            c => {
                out.push(c);
                last_was_space = false;
            }
        }
        if out.len() > 400 {
            break;
        }
    }
    let out = decode_entities(out.trim());
    if out.chars().count() > 240 {
        let mut s: String = out.chars().take(240).collect();
        // Cut on a word so the ellipsis does not land mid-word.
        if let Some(i) = s.rfind(' ') {
            s.truncate(i);
        }
        format!("{s}\u{2026}")
    } else {
        out
    }
}

/// The handful of entities that actually show up in feed summaries.
fn decode_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&hellip;", "\u{2026}")
        .replace("&mdash;", "\u{2014}")
        .replace("&ndash;", "\u{2013}")
        .replace("&rsquo;", "\u{2019}")
        .replace("&lsquo;", "\u{2018}")
        .replace("&ldquo;", "\u{201c}")
        .replace("&rdquo;", "\u{201d}")
}

fn parse_id(scope: &str) -> Res<i64> {
    scope
        .split_once(':')
        .and_then(|(_, id)| id.parse().ok())
        .ok_or_else(|| CommandError::Invalid(format!("bad scope {scope:?}")))
}

/// Body for the reading pane.
///
/// Cached in `news.article_html` after the first fetch, so revisiting an
/// article is instant and works offline. A feed set to description-only never
/// makes a network request.
#[tauri::command]
pub async fn article(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    id: i64,
) -> Res<ArticleView> {
    struct Row {
        feed_id: i64,
        feed_title: String,
        title: String,
        author: Option<String>,
        published: Option<String>,
        link: Option<String>,
        description: Option<String>,
        content: Option<String>,
        cached: Option<String>,
        starred: bool,
        mode: ReadingMode,
        feed_site: Option<String>,
    }

    let row = {
        let db = state.db.lock().await;
        db.conn()
            .query_row(
                "SELECT news.feed_id, COALESCE(feeds.text, feeds.title, ''),
                        COALESCE(news.title, '(untitled)'), news.author_name,
                        COALESCE(news.published, news.received), news.link_href,
                        news.description, news.content, news.article_html,
                        news.starred, feeds.reading_mode,
                        COALESCE(NULLIF(feeds.html_url, ''), feeds.xml_url)
                 FROM news JOIN feeds ON feeds.id = news.feed_id
                 WHERE news.id = ?1",
                [id],
                |r| {
                    Ok(Row {
                        feed_id: r.get(0)?,
                        feed_title: r.get(1)?,
                        title: r.get(2)?,
                        author: r.get(3)?,
                        published: r.get(4)?,
                        link: r.get(5)?,
                        description: r.get(6)?,
                        content: r.get(7)?,
                        cached: r.get(8)?,
                        starred: r.get::<_, i64>(9)? != 0,
                        mode: ReadingMode::from_i64(r.get(10)?),
                        feed_site: r.get(11)?,
                    })
                },
            )
            .map_err(DbError::from)?
    };
    let _ = row.feed_id;
    // Relative image and link addresses in feed text resolve against the
    // article, or, for an item with no link, against the feed's site. Left
    // relative they point into the app and break.
    let base = row.link.clone().or_else(|| row.feed_site.clone());

    // 1. Cached extraction. The feed's lead image is restored here rather than
    //    when the cache is written, so articles cached before this existed get
    //    it too.
    if let Some(cached) = row.cached.filter(|h| !h.trim().is_empty()) {
        let summary = snaprss_article::sanitise_feed_html(
            row.content.as_deref().or(row.description.as_deref()).unwrap_or(""),
            base.as_deref(),
        );
        let html = snaprss_article::keep_lead_image(&cached, &summary);
        let minutes = read_minutes_of(&html);
        return Ok(ArticleView {
            id,
            title: row.title,
            byline: row.author,
            feed_title: row.feed_title,
            published: row.published,
            link: row.link,
            html,
            read_minutes: minutes,
            starred: row.starred,
            from_feed: false,
            pending: false,
        });
    }

    // 2. Feed-supplied body, when that is what the feed is set to, or when
    //    there is no link to fetch.
    let feed_body = row
        .content
        .clone()
        .or_else(|| row.description.clone())
        .unwrap_or_default();

    if row.mode == ReadingMode::DescriptionOnly || row.link.is_none() {
        let html = snaprss_article::sanitise_feed_html(&feed_body, base.as_deref());
        let minutes = read_minutes_of(&html);
        return Ok(ArticleView {
            id,
            title: row.title,
            byline: row.author,
            feed_title: row.feed_title,
            published: row.published,
            link: row.link,
            html,
            read_minutes: minutes,
            starred: row.starred,
            from_feed: true,
            pending: false,
        });
    }

    // 3. Not cached, and this feed wants the full article. Return the feed's
    //    own summary immediately and do the fetch in the background.
    //
    //    Doing it inline is what made opening an article feel slow: the reply
    //    could not arrive until a third-party server had answered and the page
    //    had been parsed. The pane now paints at once and upgrades itself when
    //    `article-ready` fires.
    let link = row.link.clone().unwrap();
    let html = snaprss_article::sanitise_feed_html(&feed_body, Some(&link));
    let minutes = read_minutes_of(&html);

    spawn_extraction(app, state.http.clone(), state.db.clone(), id, link);

    Ok(ArticleView {
        id,
        title: row.title,
        byline: row.author,
        feed_title: row.feed_title,
        published: row.published,
        link: row.link,
        html,
        read_minutes: minutes,
        starred: row.starred,
        from_feed: true,
        pending: true,
    })
}

/// Fetch and extract one article, cache it, and tell the frontend.
///
/// Only one extraction runs per article: a second request while one is in
/// flight is dropped rather than queued, because the user clicking back and
/// forth through a list would otherwise start a fetch per click.
fn spawn_extraction(
    app: tauri::AppHandle,
    http: reqwest::Client,
    db: Arc<Mutex<Db>>,
    id: i64,
    link: String,
) {
    {
        let mut inflight = EXTRACTING.lock().unwrap();
        if !inflight.insert(id) {
            return;
        }
    }

    tauri::async_runtime::spawn(async move {
        let done = |ok: bool| {
            EXTRACTING.lock().unwrap().remove(&id);
            use tauri::Emitter;
            let _ = app.emit(
                "article-ready",
                serde_json::json!({ "id": id, "ok": ok }),
            );
        };

        // Only a web page is worth extracting. A link to an MP3 or a PDF was
        // read whole into memory and handed to readability, and the failure
        // it ended in left "fetching full article…" on screen.
        let Some(body) = fetch_page(&http, &link).await else {
            return done(false);
        };

        // Readability is CPU-bound and can take tens of milliseconds on a big
        // page. Off the async runtime so it cannot stall other commands.
        let link_for_store = link.clone();
        let extracted =
            tokio::task::spawn_blocking(move || snaprss_article::extract(&body, &link).ok())
                .await
                .ok()
                .flatten();

        let Some(a) = extracted else { return done(false) };

        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        {
            let db = db.lock().await;
            // Matched on the link too: if the article's feed was removed
            // while the page was being fetched, SQLite can hand its id to a
            // new article, which would have been given this page's text.
            let _ = db.conn().execute(
                "UPDATE news SET article_html = ?1, article_fetched = ?2 WHERE id = ?3 AND link_href = ?4",
                rusqlite::params![a.html, now, id, link_for_store],
            );
        }
        done(true);
    });
}

/// The page at `link` as text, if it is HTML and not absurdly large.
async fn fetch_page(http: &reqwest::Client, link: &str) -> Option<String> {
    const MAX_PAGE_BYTES: usize = 16 * 1024 * 1024;
    // Asking for a web page. The client's default Accept prefers feed
    // formats, and a site that negotiates on it answered with XML.
    let mut resp = http
        .get(link)
        .header(reqwest::header::ACCEPT, "text/html,application/xhtml+xml;q=0.9,*/*;q=0.5")
        .send()
        .await
        .ok()
        .filter(|r| r.status().is_success())?;
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let html = content_type
        .as_deref()
        .map(|t| {
            let t = t.to_ascii_lowercase();
            t.contains("html") || t.contains("xml") || t.starts_with("text/plain")
        })
        // No Content-Type at all: let readability decide.
        .unwrap_or(true);
    if !html || resp.content_length().is_some_and(|n| n as usize > MAX_PAGE_BYTES) {
        return None;
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await.ok()? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > MAX_PAGE_BYTES {
            return None;
        }
    }
    Some(snaprss_article::decode_html(&bytes, content_type.as_deref()))
}

/// Articles currently being extracted, so a burst of clicks does not start a
/// fetch each time.
static EXTRACTING: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<i64>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

fn read_minutes_of(html: &str) -> u32 {
    snaprss_article::read_minutes(&snaprss_article::to_plain_text(html))
}

#[tauri::command]
pub async fn counts(app: tauri::AppHandle, state: State<'_, AppState>) -> Res<Counts> {
    // The page asks for counts after every change it makes, which is also
    // when the tray's number goes stale.
    crate::tray_badge::refresh(&app).await;
    read_counts(&state).await
}

pub async fn read_counts(state: &AppState) -> Res<Counts> {
    let db = state.db.lock().await;
    let (unread, total, starred) = db
        .conn()
        .query_row(
            "SELECT
               (SELECT COUNT(*) FROM news WHERE read = 0 AND deleted = 0),
               (SELECT COUNT(*) FROM news WHERE deleted = 0),
               (SELECT COUNT(*) FROM news WHERE starred = 1 AND deleted = 0)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(DbError::from)?;
    Ok(Counts {
        unread,
        total,
        starred,
    })
}

// -------------------------------------------------------------------- writing

#[tauri::command]
pub async fn set_read(state: State<'_, AppState>, ids: Vec<i64>, read: bool) -> Res<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut db = state.db.lock().await;
    {
        let tx = db.conn_mut().transaction().map_err(DbError::from)?;
        {
            let mut upd = tx
                .prepare("UPDATE news SET read = ?1, new = 0 WHERE id = ?2")
                .map_err(DbError::from)?;
            for id in &ids {
                upd.execute(rusqlite::params![read as i64, id]).map_err(DbError::from)?;
            }
        }
        tx.commit().map_err(DbError::from)?;
    }
    db.recompute_counters_for_news(&ids)?;
    Ok(())
}

#[tauri::command]
pub async fn set_starred(state: State<'_, AppState>, ids: Vec<i64>, starred: bool) -> Res<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut db = state.db.lock().await;
    let tx = db.conn_mut().transaction().map_err(DbError::from)?;
    for id in &ids {
        tx.execute(
            "UPDATE news SET starred = ?1 WHERE id = ?2",
            rusqlite::params![starred as i64, id],
        )
        .map_err(DbError::from)?;
    }
    tx.commit().map_err(DbError::from)?;
    Ok(())
}

/// Soft delete, so the Deleted folder can restore it.
#[tauri::command]
pub async fn set_deleted(state: State<'_, AppState>, ids: Vec<i64>, deleted: bool) -> Res<()> {
    let mut db = state.db.lock().await;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    {
        let tx = db.conn_mut().transaction().map_err(DbError::from)?;
        for id in &ids {
            tx.execute(
                // deleted = 2 is what emptying Deleted leaves behind: a stub
                // kept only so the article is not downloaded again. Undoing a
                // delete after that brought the stubs back as blank articles.
                "UPDATE news SET deleted = ?1, delete_date = ?2 WHERE id = ?3 AND deleted <> 2",
                rusqlite::params![deleted as i64, if deleted { Some(&now) } else { None }, id],
            )
            .map_err(DbError::from)?;
        }
        tx.commit().map_err(DbError::from)?;
    }
    db.recompute_counters_for_news(&ids)?;
    Ok(())
}

#[tauri::command]
/// Mark everything unread in a scope as read, and return what it marked so
/// the action can be undone exactly. Done in SQL: going through `news_list`
/// capped it at the newest 5,000 articles, and older unread ones stayed
/// unread.
pub async fn mark_scope_read(state: State<'_, AppState>, scope: String) -> Res<Vec<i64>> {
    let (where_sql, param) = scope_filter(&scope)?;
    let mut db = state.db.lock().await;
    let tx = db.conn_mut().transaction().map_err(DbError::from)?;
    let ids: Vec<i64> = {
        let sql = format!("SELECT news.id FROM news WHERE {where_sql} AND news.read = 0");
        let mut stmt = tx.prepare(&sql).map_err(DbError::from)?;
        let params: Vec<i64> = param.into_iter().collect();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params), |r| r.get(0))
            .map_err(DbError::from)?;
        rows.collect::<Result<_, _>>().map_err(DbError::from)?
    };
    // One statement over the same selection, not one per article: marking
    // 100,000 articles one by one took seconds.
    {
        let sql = format!("UPDATE news SET read = 1, new = 0 WHERE {where_sql} AND news.read = 0");
        let params: Vec<i64> = param.into_iter().collect();
        tx.execute(&sql, rusqlite::params_from_iter(params))
            .map_err(DbError::from)?;
    }
    tx.commit().map_err(DbError::from)?;
    if ids.len() > 1000 {
        db.recompute_counters()?;
    } else {
        db.recompute_counters_for_news(&ids)?;
    }
    Ok(ids)
}

// ------------------------------------------------------------------- feed crud

#[derive(Debug, Serialize)]
pub struct FoundFeed {
    pub url: String,
    pub title: Option<String>,
    /// Already in the subscription list.
    pub subscribed: bool,
}

/// What Add feed looks at before adding: the address itself if it is a
/// feed, or the feeds the page links to. No lock is held while fetching.
#[tauri::command]
pub async fn discover_feed(state: State<'_, AppState>, address: String) -> Res<Vec<FoundFeed>> {
    let url = snaprss_fetch::discover::normalise(&address);
    let parsed = url::Url::parse(&url).map_err(|e| CommandError::Invalid(format!("not a web address: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(CommandError::Invalid("only http and https feeds are supported".into()));
    }
    let found = snaprss_fetch::discover::discover(&state.http, &state.fetch_cfg, &url)
        .await
        .map_err(|e| CommandError::Invalid(format!("could not open {url}: {e}")))?;
    let db = state.db.lock().await;
    let have: Vec<url::Url> = db
        .conn()
        .prepare("SELECT xml_url FROM feeds WHERE kind = 1 AND xml_url IS NOT NULL")
        .map_err(DbError::from)?
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(DbError::from)?
        .filter_map(Result::ok)
        .filter_map(|u| url::Url::parse(u.trim()).ok())
        .collect();
    Ok(found
        .into_iter()
        .map(|f| {
            let subscribed = url::Url::parse(&f.url).is_ok_and(|u| have.contains(&u));
            FoundFeed { url: f.url, title: f.title, subscribed }
        })
        .collect())
}

#[tauri::command]
pub async fn add_feed(state: State<'_, AppState>, url: String, parent_id: Option<i64>) -> Res<i64> {
    let parsed = url::Url::parse(url.trim())
        .map_err(|e| CommandError::Invalid(format!("not a url: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(CommandError::Invalid(
            "only http and https feeds are supported".into(),
        ));
    }

    let db = state.db.lock().await;
    // Compared as URLs, not as strings: an imported "https://example.com"
    // and a typed "https://example.com/" are the same feed.
    let existing: Option<i64> = {
        let mut stmt = db
            .conn()
            .prepare("SELECT id, xml_url FROM feeds WHERE kind = 1 AND xml_url IS NOT NULL")
            .map_err(DbError::from)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .map_err(DbError::from)?;
        let mut hit = None;
        for row in rows {
            let (id, u) = row.map_err(DbError::from)?;
            if url::Url::parse(u.trim()).is_ok_and(|u| u == parsed) {
                hit = Some(id);
                break;
            }
        }
        hit
    };
    if let Some(id) = existing {
        return Err(CommandError::Invalid(format!(
            "already subscribed (feed {id})"
        )));
    }

    let description_only = db
        .conn()
        .query_row(
            "SELECT value FROM settings WHERE key = 'reading.default_description_only'",
            [],
            |r| r.get::<_, String>(0),
        )
        .is_ok_and(|v| v == "1");
    let mode = if description_only {
        ReadingMode::DescriptionOnly
    } else {
        ReadingMode::FullArticle
    };

    db.conn()
        .execute(
            "INSERT INTO feeds (kind, parent_id, row_to_parent, text, xml_url, created, reading_mode)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                parent_id,
                next_row(&db, parent_id)?,
                parsed.host_str().unwrap_or("New feed"),
                parsed.as_str(),
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                mode.as_i64(),
            ],
        )
        .map_err(DbError::from)?;
    Ok(db.conn().last_insert_rowid())
}

/// The position after the last child of `parent`, so something new goes at
/// the end. Inserting at 0 put it second, after whatever already had 0.
fn next_row(db: &Db, parent: Option<i64>) -> Res<i64> {
    Ok(db
        .conn()
        .query_row(
            "SELECT COALESCE(MAX(row_to_parent), -1) + 1 FROM feeds WHERE parent_id IS ?1",
            [parent],
            |r| r.get(0),
        )
        .map_err(DbError::from)?)
}

#[tauri::command]
pub async fn remove_feed(state: State<'_, AppState>, id: i64) -> Res<()> {
    let mut db = state.db.lock().await;
    let tx = db.conn_mut().transaction().map_err(DbError::from)?;
    let gone: std::collections::HashSet<i64> = {
        let mut stmt = tx
            .prepare(
                "WITH RECURSIVE sub(id) AS (
                     SELECT id FROM feeds WHERE id = ?1
                     UNION ALL
                     SELECT f.id FROM feeds f JOIN sub ON f.parent_id = sub.id
                 ) SELECT id FROM sub",
            )
            .map_err(DbError::from)?;
        let rows = stmt.query_map([id], |r| r.get(0)).map_err(DbError::from)?;
        rows.collect::<Result<_, _>>().map_err(DbError::from)?
    };
    // Filters name feeds by id, and SQLite hands a deleted feed's id to the
    // next one added. Left in place, a filter aimed at the removed feed would
    // start acting on whatever was subscribed next.
    snaprss_core::filters::forget_feeds(&tx, &gone)?;
    // ON DELETE CASCADE takes the articles and any child nodes with it.
    tx.execute("DELETE FROM feeds WHERE id = ?1", [id])
        .map_err(DbError::from)?;
    tx.commit().map_err(DbError::from)?;
    Ok(())
}

#[tauri::command]
pub async fn rename_node(state: State<'_, AppState>, id: i64, name: String) -> Res<()> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CommandError::Invalid("name cannot be empty".into()));
    }
    let db = state.db.lock().await;
    db.conn()
        .execute(
            "UPDATE feeds SET text = ?1 WHERE id = ?2",
            rusqlite::params![name, id],
        )
        .map_err(DbError::from)?;
    Ok(())
}

#[tauri::command]
pub async fn add_folder(
    state: State<'_, AppState>,
    name: String,
    parent_id: Option<i64>,
) -> Res<i64> {
    let db = state.db.lock().await;
    db.conn()
        .execute(
            "INSERT INTO feeds (kind, parent_id, row_to_parent, text, xml_url)
             VALUES (0, ?1, ?2, ?3, '')",
            rusqlite::params![parent_id, next_row(&db, parent_id)?, name.trim()],
        )
        .map_err(DbError::from)?;
    Ok(db.conn().last_insert_rowid())
}

#[tauri::command]
pub async fn set_reading_mode(
    state: State<'_, AppState>,
    id: i64,
    description_only: bool,
) -> Res<()> {
    let mode = if description_only {
        ReadingMode::DescriptionOnly
    } else {
        ReadingMode::FullArticle
    };
    let db = state.db.lock().await;
    db.conn()
        .execute(
            "UPDATE feeds SET reading_mode = ?1 WHERE id = ?2",
            rusqlite::params![mode.as_i64(), id],
        )
        .map_err(DbError::from)?;
    Ok(())
}

// -------------------------------------------------------------------- updating

#[derive(Debug, Default, Serialize)]
pub struct UpdateResult {
    pub attempted: usize,
    pub not_modified: usize,
    pub ingested: usize,
    pub failed: usize,
    pub new_articles: usize,
}

impl From<UpdateSummary> for UpdateResult {
    fn from(s: UpdateSummary) -> Self {
        UpdateResult {
            attempted: s.attempted,
            not_modified: s.not_modified,
            ingested: s.ingested,
            failed: s.failed,
            new_articles: s.new_articles,
        }
    }
}

/// The WHERE clause for a list scope, and its one parameter if it has one.
fn scope_filter(scope: &str) -> Res<(String, Option<i64>)> {
    Ok(match scope {
        "all" => ("news.deleted = 0".into(), None),
        "unread" => ("news.deleted = 0 AND news.read = 0".into(), None),
        "starred" => ("news.deleted = 0 AND news.starred = 1".into(), None),
        "deleted" => ("news.deleted = 1".into(), None),
        s if s.starts_with("feed:") => (
            "news.deleted = 0 AND news.feed_id = ?1".into(),
            Some(parse_id(s)?),
        ),
        s if s.starts_with("folder:") => (
            // Recursive so a folder shows everything beneath it, at any depth.
            "news.deleted = 0 AND news.feed_id IN (
                 WITH RECURSIVE sub(id) AS (
                     SELECT id FROM feeds WHERE id = ?1
                     UNION ALL
                     SELECT f.id FROM feeds f JOIN sub ON f.parent_id = sub.id
                 ) SELECT id FROM sub)"
                .into(),
            Some(parse_id(s)?),
        ),
        s if s.starts_with("label:") => (
            "news.deleted = 0 AND news.id IN
                 (SELECT news_id FROM news_labels WHERE label_id = ?1)"
                .into(),
            Some(parse_id(s)?),
        ),
        other => return Err(CommandError::Invalid(format!("unknown scope {other:?}"))),
    })
}

#[tauri::command]
/// Update every feed that is due.
///
/// The lock is taken twice, for a few milliseconds each time, and is *not*
/// held across the network. The obvious version passes `&mut Db` into the
/// fetcher and awaits with it held, which freezes every other command for as
/// long as the slowest server takes — that is what made the app feel like it
/// stalled at random.
///
/// Progress is emitted per feed as `update-progress`, so the toolbar can show
/// what it is doing rather than going quiet and then reporting a total.
pub async fn update_all(app: tauri::AppHandle, state: State<'_, AppState>) -> Res<UpdateResult> {
    // Every feed, not just the ones the schedule says are due. Pressing a
    // button is a request, not a tick: honouring the interval here meant the
    // button usually did nothing at all and reported "nothing due yet".
    // Conditional GET keeps that cheap — an unchanged feed is a 304.
    let feeds = {
        let db = state.db.lock().await;
        snaprss_fetch::all_feeds(&db)?
    };

    if feeds.is_empty() {
        return Ok(UpdateResult::default());
    }

    let total = feeds.len();
    emit_progress(&app, 0, total, "", true);

    let handle = app.clone();
    let fetched = snaprss_fetch::fetch_all(
        &state.http,
        &state.fetch_cfg,
        feeds,
        6,
        move |p| emit_progress(&handle, p.done, p.total, &p.title, p.ok),
    )
    .await;

    let mut db = state.db.lock().await;
    let summary = snaprss_fetch::apply_fetched(&mut db, fetched)?;
    drop(db);
    crate::alerts::announce(&app, &summary);
    Ok(summary.into())
}

fn emit_progress(app: &tauri::AppHandle, done: usize, total: usize, title: &str, ok: bool) {
    use tauri::Emitter;
    let _ = app.emit(
        "update-progress",
        serde_json::json!({ "done": done, "total": total, "title": title, "ok": ok }),
    );
}

/// Ignores the schedule: this is the user pressing the button. A folder
/// updates every feed inside it, at any depth; passing a folder used to fail,
/// because a folder has no URL to fetch.
#[tauri::command]
pub async fn update_feed_now(app: tauri::AppHandle, state: State<'_, AppState>, id: i64) -> Res<UpdateResult> {
    let feeds: Vec<snaprss_fetch::DueFeed> = {
        let db = state.db.lock().await;
        let mut stmt = db
            .conn()
            .prepare(
                "WITH RECURSIVE sub(id) AS (
                     SELECT id FROM feeds WHERE id = ?1
                     UNION ALL
                     SELECT f.id FROM feeds f JOIN sub ON f.parent_id = sub.id
                 )
                 SELECT id, COALESCE(NULLIF(text, ''), NULLIF(title, ''), xml_url),
                        xml_url, http_etag, http_last_modified
                 FROM feeds
                 WHERE id IN (SELECT id FROM sub) AND kind = 1
                   AND xml_url IS NOT NULL AND xml_url <> ''",
            )
            .map_err(DbError::from)?;
        let rows = stmt
            .query_map([id], |r| {
                Ok(snaprss_fetch::DueFeed {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    xml_url: r.get(2)?,
                    etag: r.get(3)?,
                    last_modified: r.get(4)?,
                    credentials: None,
                })
            })
            .map_err(DbError::from)?;
        let mut feeds: Vec<_> = rows.collect::<Result<_, _>>().map_err(DbError::from)?;
        snaprss_fetch::schedule::attach_credentials(&db, &mut feeds)?;
        feeds
    };
    if feeds.is_empty() {
        return Ok(UpdateResult::default());
    }

    // Same reason as update_all: fetch first, then take the lock.
    let fetched =
        snaprss_fetch::fetch_all(&state.http, &state.fetch_cfg, feeds, 6, |_| {}).await;

    let mut db = state.db.lock().await;
    let summary = snaprss_fetch::apply_fetched(&mut db, fetched)?;
    drop(db);
    crate::alerts::announce(&app, &summary);
    Ok(summary.into())
}

// ------------------------------------------------------- importing / exporting

#[derive(Debug, Serialize)]
pub struct ImportResult {
    /// "quiterss" or "opml", so the UI can word the confirmation.
    pub kind: String,
    pub folders: usize,
    pub feeds: usize,
    pub news: usize,
    pub labels: usize,
    pub filters: usize,
    pub duplicates: usize,
    pub skipped: Vec<String>,
}

/// Import a subscription file. The kind is sniffed from the contents, not the
/// extension: a QuiteRSS database and an OPML export are both things people
/// have lying around, and neither is reliably named.
#[tauri::command]
pub async fn import_file(state: State<'_, AppState>, path: String) -> Res<ImportResult> {
    let mut db = state.db.lock().await;
    let (kind, r) = snaprss_core::import::import_any(&mut db, &path)?;
    Ok(ImportResult {
        kind: match kind {
            snaprss_core::import::SourceKind::QuiteRssDatabase => "quiterss",
            snaprss_core::import::SourceKind::Opml => "opml",
            snaprss_core::import::SourceKind::Unknown => "unknown",
        }
        .to_string(),
        folders: r.folders,
        feeds: r.feeds,
        news: r.news,
        labels: r.labels,
        filters: r.filters,
        duplicates: r.duplicates,
        skipped: r.skipped,
    })
}

/// Write the subscription tree to an OPML file. Returns how many feeds it
/// contained.
#[tauri::command]
pub async fn export_opml(state: State<'_, AppState>, path: String) -> Res<usize> {
    let db = state.db.lock().await;
    snaprss_core::import::export_opml_to_file(&db, &path)
        .map_err(|e| CommandError::Invalid(e.to_string()))?;
    let n: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM feeds WHERE kind = 1 AND xml_url <> ''",
            [],
            |r| r.get(0),
        )
        .map_err(DbError::from)?;
    Ok(n as usize)
}

// ------------------------------------------------------- settings and storage

/// Settings are key/value strings in the database rather than a typed struct,
/// because the set changes often and the frontend is the only consumer. Typing
/// them here would mean a schema migration every time a checkbox is added.
#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Res<HashMap<String, String>> {
    let db = state.db.lock().await;
    let mut stmt = db
        .conn()
        .prepare("SELECT key, value FROM settings WHERE key NOT LIKE '\\_%' ESCAPE '\\'")
        .map_err(DbError::from)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(DbError::from)?;
    let mut out = HashMap::new();
    for row in rows {
        let (k, v) = row.map_err(DbError::from)?;
        out.insert(k, v);
    }
    Ok(out)
}

#[tauri::command]
pub async fn set_settings(
    state: State<'_, AppState>,
    values: HashMap<String, String>,
) -> Res<()> {
    let mut db = state.db.lock().await;
    let tx = db.conn_mut().transaction().map_err(DbError::from)?;
    for (k, v) in values {
        // `window.*` belongs to the window, which writes it as it changes;
        // the Settings sheet holds a copy from when it opened, and saving
        // that copy put back a stale maximised state.
        if k.starts_with('_') || k == "schema_version" || k.starts_with("window.") {
            continue;
        }
        tx.execute(
            "INSERT INTO settings(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![k, v],
        )
        .map_err(DbError::from)?;
    }
    tx.commit().map_err(DbError::from)?;
    Ok(())
}

#[tauri::command]
pub async fn db_stats(state: State<'_, AppState>) -> Res<snaprss_core::cleanup::DbStats> {
    let db = state.db.lock().await;
    Ok(snaprss_core::cleanup::stats(&db)?)
}

/// Mark whatever the retention rules cover as deleted. Reversible.
#[tauri::command]
pub async fn run_cleanup(
    state: State<'_, AppState>,
    feed_id: Option<i64>,
) -> Res<snaprss_core::cleanup::CleanupReport> {
    let mut db = state.db.lock().await;
    Ok(snaprss_core::cleanup::run(&mut db, feed_id)?)
}

/// Permanently remove articles already marked deleted. Not reversible, so the
/// frontend asks first.
#[tauri::command]
pub async fn purge_deleted(
    state: State<'_, AppState>,
    older_than_days: Option<i64>,
) -> Res<snaprss_core::cleanup::CleanupReport> {
    let mut db = state.db.lock().await;
    Ok(snaprss_core::cleanup::purge_deleted(&mut db, older_than_days)?)
}

#[tauri::command]
pub async fn vacuum_db(state: State<'_, AppState>) -> Res<i64> {
    let db = state.db.lock().await;
    Ok(snaprss_core::cleanup::vacuum(&db)?)
}

#[tauri::command]
pub async fn clear_article_cache(state: State<'_, AppState>) -> Res<usize> {
    let db = state.db.lock().await;
    Ok(snaprss_core::cleanup::clear_article_cache(&db)?)
}

/// Copy the database somewhere safe. Uses SQLite's own backup API rather than
/// a file copy, so it is safe to run while the app is using the database.
#[tauri::command]
pub async fn backup_db(state: State<'_, AppState>, path: String) -> Res<i64> {
    let db = state.db.lock().await;
    db.conn()
        .backup(rusqlite::DatabaseName::Main, &path, None)
        .map_err(|e| CommandError::Invalid(format!("backup failed: {e}")))?;
    Ok(std::fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0))
}

// ------------------------------------------------------------- feed settings

/// Everything the feed properties page edits.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedSettings {
    pub id: i64,
    pub title: String,
    pub xml_url: Option<String>,
    pub html_url: Option<String>,
    pub description_only: bool,
    pub load_images: bool,
    pub save_offline: bool,
    pub show_notification: bool,
    pub disable_update: bool,
    pub layout_direction: i64,
    pub update_interval_enable: bool,
    pub update_interval: Option<i64>,
    pub update_interval_type: Option<String>,
    pub duplicate_news_mode: bool,
    // retention
    pub max_to_keep_enable: bool,
    pub max_to_keep: Option<i64>,
    pub max_age_enable: bool,
    pub max_age_days: Option<i64>,
    pub delete_read: bool,
    pub never_delete_unread: bool,
    pub never_delete_starred: bool,
    pub never_delete_labeled: bool,
    // sign-in: the user name, and whether a password is saved. The saved
    // password itself never goes to the page.
    #[serde(default)]
    pub sign_in_user: Option<String>,
    #[serde(default)]
    pub has_password: bool,
    /// On save only: a new password, or None to keep the saved one.
    #[serde(default, skip_serializing)]
    pub sign_in_password: Option<String>,
    // read-only status
    pub status: Option<String>,
    pub updated: Option<String>,
    pub article_count: i64,
}

#[tauri::command]
pub async fn feed_settings(state: State<'_, AppState>, id: i64) -> Res<FeedSettings> {
    let db = state.db.lock().await;
    let s = db
        .conn()
        .query_row(
            "SELECT id, COALESCE(text, title, 'Untitled'), xml_url, html_url,
                    reading_mode, load_images, save_offline, show_notification,
                    disable_update, layout_direction,
                    update_interval_enable, update_interval, update_interval_type,
                    duplicate_news_mode,
                    max_to_keep_enable, max_to_keep, max_age_enable, max_age_days,
                    delete_read, never_delete_unread, never_delete_starred,
                    never_delete_labeled, status, updated,
                    (SELECT COUNT(*) FROM news WHERE news.feed_id = feeds.id AND deleted = 0)
             FROM feeds WHERE id = ?1",
            [id],
            |r| {
                Ok(FeedSettings {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    xml_url: r.get(2)?,
                    html_url: r.get(3)?,
                    description_only: r.get::<_, i64>(4)? == 1,
                    load_images: r.get::<_, i64>(5)? != 0,
                    save_offline: r.get::<_, i64>(6)? != 0,
                    show_notification: r.get::<_, i64>(7)? != 0,
                    disable_update: r.get::<_, i64>(8)? != 0,
                    layout_direction: r.get(9)?,
                    update_interval_enable: r.get::<_, i64>(10)? != 0,
                    update_interval: r.get(11)?,
                    update_interval_type: r.get(12)?,
                    duplicate_news_mode: r.get::<_, i64>(13)? != 0,
                    max_to_keep_enable: r.get::<_, i64>(14)? != 0,
                    max_to_keep: r.get(15)?,
                    max_age_enable: r.get::<_, i64>(16)? != 0,
                    max_age_days: r.get(17)?,
                    delete_read: r.get::<_, i64>(18)? != 0,
                    never_delete_unread: r.get::<_, i64>(19)? != 0,
                    never_delete_starred: r.get::<_, i64>(20)? != 0,
                    never_delete_labeled: r.get::<_, i64>(21)? != 0,
                    sign_in_user: None,
                    has_password: false,
                    sign_in_password: None,
                    status: r.get(22)?,
                    updated: r.get(23)?,
                    article_count: r.get(24)?,
                })
            },
        )
        .map_err(DbError::from)?;
    let mut s = s;
    if let Some(c) = db.feed_credentials(id)? {
        s.has_password = !c.password.is_empty();
        s.sign_in_user = Some(c.user);
    }
    Ok(s)
}

#[tauri::command]
pub async fn save_feed_settings(state: State<'_, AppState>, s: FeedSettings) -> Res<()> {
    let title = s.title.trim();
    if title.is_empty() {
        return Err(CommandError::Invalid("name cannot be empty".into()));
    }
    let db = state.db.lock().await;
    db.conn()
        .execute(
            "UPDATE feeds SET
                text = ?1, reading_mode = ?2, load_images = ?3, save_offline = ?4,
                show_notification = ?5, disable_update = ?6, layout_direction = ?7,
                update_interval_enable = ?8, update_interval = ?9, update_interval_type = ?10,
                duplicate_news_mode = ?11,
                max_to_keep_enable = ?12, max_to_keep = ?13,
                max_age_enable = ?14, max_age_days = ?15,
                delete_read = ?16, never_delete_unread = ?17,
                never_delete_starred = ?18, never_delete_labeled = ?19
             WHERE id = ?20",
            rusqlite::params![
                title,
                s.description_only as i64,
                s.load_images as i64,
                s.save_offline as i64,
                s.show_notification as i64,
                s.disable_update as i64,
                s.layout_direction,
                s.update_interval_enable as i64,
                s.update_interval,
                s.update_interval_type,
                s.duplicate_news_mode as i64,
                s.max_to_keep_enable as i64,
                s.max_to_keep,
                s.max_age_enable as i64,
                s.max_age_days,
                s.delete_read as i64,
                s.never_delete_unread as i64,
                s.never_delete_starred as i64,
                s.never_delete_labeled as i64,
                s.id,
            ],
        )
        .map_err(DbError::from)?;
    // An empty user name signs the feed out; a blank password keeps the
    // saved one.
    if let Some(user) = s.sign_in_user.as_deref() {
        db.save_feed_sign_in(s.id, user, s.sign_in_password.as_deref())?;
    }
    Ok(())
}

/// Apply one feed's retention and update settings to every feed in a folder,
/// which is the only bearable way to configure a large subscription list.
#[tauri::command]
pub async fn apply_settings_to_folder(
    state: State<'_, AppState>,
    from_id: i64,
    folder_id: Option<i64>,
) -> Res<usize> {
    let db = state.db.lock().await;
    let n = db
        .conn()
        .execute(
            "UPDATE feeds SET
                reading_mode = (SELECT reading_mode FROM feeds WHERE id = ?1),
                load_images = (SELECT load_images FROM feeds WHERE id = ?1),
                save_offline = (SELECT save_offline FROM feeds WHERE id = ?1),
                update_interval_enable = (SELECT update_interval_enable FROM feeds WHERE id = ?1),
                update_interval = (SELECT update_interval FROM feeds WHERE id = ?1),
                update_interval_type = (SELECT update_interval_type FROM feeds WHERE id = ?1),
                max_to_keep_enable = (SELECT max_to_keep_enable FROM feeds WHERE id = ?1),
                max_to_keep = (SELECT max_to_keep FROM feeds WHERE id = ?1),
                max_age_enable = (SELECT max_age_enable FROM feeds WHERE id = ?1),
                max_age_days = (SELECT max_age_days FROM feeds WHERE id = ?1),
                delete_read = (SELECT delete_read FROM feeds WHERE id = ?1),
                never_delete_unread = (SELECT never_delete_unread FROM feeds WHERE id = ?1),
                never_delete_starred = (SELECT never_delete_starred FROM feeds WHERE id = ?1),
                never_delete_labeled = (SELECT never_delete_labeled FROM feeds WHERE id = ?1)
             WHERE kind = 1 AND id <> ?1
               AND (?2 IS NULL OR id IN (
                     WITH RECURSIVE sub(id) AS (
                         SELECT id FROM feeds WHERE id = ?2
                         UNION ALL
                         SELECT f.id FROM feeds f JOIN sub ON f.parent_id = sub.id
                     ) SELECT id FROM sub))",
            rusqlite::params![from_id, folder_id],
        )
        .map_err(DbError::from)?;
    Ok(n)
}

// --------------------------------------------------------------------- labels

#[derive(Debug, Serialize)]
pub struct LabelRow {
    pub id: i64,
    pub name: String,
    pub color_bg: Option<String>,
    /// Imported QuiteRSS labels carry their own text colour.
    pub color_text: Option<String>,
    pub count: i64,
}

#[tauri::command]
pub async fn labels(state: State<'_, AppState>) -> Res<Vec<LabelRow>> {
    let db = state.db.lock().await;
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT l.id, l.name, l.color_bg, l.color_text,
                    (SELECT COUNT(*) FROM news_labels nl
                      JOIN news n ON n.id = nl.news_id
                      WHERE nl.label_id = l.id AND n.deleted = 0)
             FROM labels l ORDER BY l.num, l.id",
        )
        .map_err(DbError::from)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(LabelRow {
                id: r.get(0)?,
                name: r.get(1)?,
                color_bg: r.get(2)?,
                color_text: r.get(3)?,
                count: r.get(4)?,
            })
        })
        .map_err(DbError::from)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(DbError::from)
        .map_err(Into::into)
}

// --------------------------------------------------------------------- labels

/// The frontend sends `colorBg` / `colorText`. Tauri renames a command's
/// arguments, not the fields inside one, so without this both colours
/// arrived as `None` and saving a label erased its colour.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelDraft {
    pub id: Option<i64>,
    pub name: String,
    pub color_bg: Option<String>,
    pub color_text: Option<String>,
}

/// A colour is only ever written to the database after passing this, because
/// it ends up in a `style` attribute in the list. `#rgb` and `#rrggbb` only.
fn valid_colour(s: &str) -> bool {
    let s = s.trim();
    let Some(hex) = s.strip_prefix('#') else {
        return false;
    };
    (hex.len() == 3 || hex.len() == 6) && hex.chars().all(|c| c.is_ascii_hexdigit())
}

fn clean_colour(c: Option<String>) -> Res<Option<String>> {
    match c {
        None => Ok(None),
        Some(s) if s.trim().is_empty() => Ok(None),
        Some(s) if valid_colour(&s) => Ok(Some(s.trim().to_lowercase())),
        Some(s) => Err(CommandError::Invalid(format!("{s:?} is not a colour"))),
    }
}

#[tauri::command]
pub async fn save_label(state: State<'_, AppState>, draft: LabelDraft) -> Res<i64> {
    let name = draft.name.trim().to_string();
    if name.is_empty() {
        return Err(CommandError::Invalid("a label needs a name".into()));
    }
    let bg = clean_colour(draft.color_bg)?;
    let fg = clean_colour(draft.color_text)?;

    let db = state.db.lock().await;
    match draft.id {
        Some(id) => {
            db.conn()
                .execute(
                    "UPDATE labels SET name = ?2, color_bg = ?3, color_text = ?4 WHERE id = ?1",
                    rusqlite::params![id, name, bg, fg],
                )
                .map_err(DbError::from)?;
            Ok(id)
        }
        None => {
            db.conn()
                .execute(
                    "INSERT INTO labels (name, color_bg, color_text, num)
                     VALUES (?1, ?2, ?3, (SELECT IFNULL(MAX(num), -1) + 1 FROM labels))",
                    rusqlite::params![name, bg, fg],
                )
                .map_err(DbError::from)?;
            Ok(db.conn().last_insert_rowid())
        }
    }
}

#[tauri::command]
pub async fn delete_label(state: State<'_, AppState>, id: i64) -> Res<()> {
    let mut db = state.db.lock().await;
    let tx = db.conn_mut().transaction().map_err(DbError::from)?;
    // news_labels cascades, so the articles keep everything except this tag.
    tx.execute("DELETE FROM labels WHERE id = ?1", [id])
        .map_err(DbError::from)?;
    // A filter action that adds this label now points at nothing; left in
    // place it failed the insert of every article it matched.
    tx.execute(
        "DELETE FROM filter_actions WHERE action IN ('add_label', 'label', '3') AND TRIM(params) = ?1",
        [id.to_string()],
    )
    .map_err(DbError::from)?;
    tx.commit().map_err(DbError::from)?;
    Ok(())
}

/// Put a label on some articles, or take it off them. One transaction so a
/// multi-selection is all-or-nothing.
#[tauri::command]
pub async fn set_label(
    state: State<'_, AppState>,
    ids: Vec<i64>,
    label_id: i64,
    on: bool,
) -> Res<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut db = state.db.lock().await;
    let tx = db.conn_mut().transaction().map_err(DbError::from)?;
    for id in &ids {
        if on {
            tx.execute(
                "INSERT OR IGNORE INTO news_labels (news_id, label_id) VALUES (?1, ?2)",
                rusqlite::params![id, label_id],
            )
            .map_err(DbError::from)?;
        } else {
            tx.execute(
                "DELETE FROM news_labels WHERE news_id = ?1 AND label_id = ?2",
                rusqlite::params![id, label_id],
            )
            .map_err(DbError::from)?;
        }
    }
    tx.commit().map_err(DbError::from)?;
    Ok(())
}

/// Take every label off a selection. The context menu's "clear labels".
#[tauri::command]
pub async fn clear_labels(state: State<'_, AppState>, ids: Vec<i64>) -> Res<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut db = state.db.lock().await;
    let tx = db.conn_mut().transaction().map_err(DbError::from)?;
    for id in &ids {
        tx.execute("DELETE FROM news_labels WHERE news_id = ?1", [id])
            .map_err(DbError::from)?;
    }
    tx.commit().map_err(DbError::from)?;
    Ok(())
}

#[tauri::command]
pub async fn reorder_label(state: State<'_, AppState>, id: i64, delta: i64) -> Res<()> {
    let mut db = state.db.lock().await;
    let order: Vec<i64> = {
        let mut stmt = db
            .conn()
            .prepare("SELECT id FROM labels ORDER BY num, id")
            .map_err(DbError::from)?;
        let v = stmt
            .query_map([], |r| r.get(0))
            .map_err(DbError::from)?
            .collect::<Result<Vec<i64>, _>>()
            .map_err(DbError::from)?;
        v
    };
    let Some(pos) = order.iter().position(|x| *x == id) else {
        return Ok(());
    };
    let target = (pos as i64 + delta).clamp(0, order.len() as i64 - 1) as usize;
    if target == pos {
        return Ok(());
    }
    let mut order = order;
    let moved = order.remove(pos);
    order.insert(target, moved);

    let tx = db.conn_mut().transaction().map_err(DbError::from)?;
    for (i, lid) in order.iter().enumerate() {
        tx.execute(
            "UPDATE labels SET num = ?2 WHERE id = ?1",
            rusqlite::params![lid, i as i64],
        )
        .map_err(DbError::from)?;
    }
    tx.commit().map_err(DbError::from)?;
    Ok(())
}

// -------------------------------------------------------------------- filters

#[derive(Debug, Serialize)]
pub struct FilterRow {
    pub id: i64,
    pub name: String,
    /// 0 every article, 1 all conditions, 2 any condition.
    pub mode: i64,
    pub enabled: bool,
    /// `None` is every feed.
    pub feeds: Option<Vec<i64>>,
    pub conditions: Vec<ConditionRow>,
    pub actions: Vec<ActionRow>,
    /// True when a condition asked for a regular expression that will not
    /// compile. The settings window shows it; the engine treats it as no match.
    pub broken: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConditionRow {
    pub field: String,
    pub op: String,
    pub content: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ActionRow {
    pub action: String,
    pub params: Option<String>,
}

fn filter_to_row(f: &snaprss_core::filters::Filter) -> FilterRow {
    let mut feeds: Option<Vec<i64>> = f.feeds.as_ref().map(|s| s.iter().copied().collect());
    if let Some(v) = feeds.as_mut() {
        v.sort_unstable();
    }
    FilterRow {
        id: f.id,
        name: f.name.clone(),
        mode: f.mode.as_i64(),
        enabled: f.enabled,
        feeds,
        broken: f.conditions.iter().any(|c| c.is_broken()),
        conditions: f
            .conditions
            .iter()
            .map(|c| ConditionRow {
                field: c.field.as_str().to_string(),
                op: c.op.as_str().to_string(),
                content: c.content.clone(),
            })
            .collect(),
        actions: f
            .actions
            .iter()
            .map(|a| {
                let (name, params) = a.as_pair();
                ActionRow {
                    action: name.to_string(),
                    params,
                }
            })
            .collect(),
    }
}

#[tauri::command]
pub async fn filters(state: State<'_, AppState>) -> Res<Vec<FilterRow>> {
    let db = state.db.lock().await;
    let all = snaprss_core::filters::load_filters(db.conn(), false)?;
    Ok(all.iter().map(filter_to_row).collect())
}

/// The vocabulary the filter editor builds its dropdowns from, so the lists
/// cannot drift from what the engine accepts.
#[tauri::command]
pub async fn filter_vocabulary() -> Res<serde_json::Value> {
    use snaprss_core::filters::{Field, Op};
    let fields = [
        Field::Title,
        Field::Description,
        Field::Author,
        Field::Category,
        Field::Status,
        Field::Link,
        Field::News,
    ];
    let ops: HashMap<&str, Vec<&str>> = fields
        .iter()
        .map(|f| {
            (
                f.as_str(),
                Op::for_field(*f).iter().map(|o| o.as_str()).collect(),
            )
        })
        .collect();
    Ok(serde_json::json!({
        "fields": fields.iter().map(|f| f.as_str()).collect::<Vec<_>>(),
        "ops": ops,
        "statuses": ["new", "read", "starred"],
        "actions": ["mark_read", "add_star", "delete", "add_label", "play_sound", "notify"],
    }))
}

#[derive(Debug, Deserialize)]
pub struct FilterDraftIn {
    pub id: Option<i64>,
    pub name: String,
    pub mode: i64,
    pub enabled: bool,
    /// Absent or null means every feed.
    pub feeds: Option<Vec<i64>>,
    pub conditions: Vec<ConditionRow>,
    pub actions: Vec<ActionRow>,
}

#[tauri::command]
pub async fn save_filter(state: State<'_, AppState>, draft: FilterDraftIn) -> Res<i64> {
    use snaprss_core::filters::{Action, Field, FilterDraft, Match, Op};

    let name = draft.name.trim().to_string();
    if name.is_empty() {
        return Err(CommandError::Invalid("a filter needs a name".into()));
    }

    let mut conditions = Vec::new();
    for c in &draft.conditions {
        let field = Field::parse(&c.field)
            .ok_or_else(|| CommandError::Invalid(format!("unknown field {:?}", c.field)))?;
        let op = Op::parse(field, &c.op).ok_or_else(|| {
            CommandError::Invalid(format!("{:?} does not apply to {:?}", c.op, c.field))
        })?;
        // Reject a regex that will not compile at save time rather than
        // storing a rule that silently never fires.
        if op == Op::Regex {
            regex::Regex::new(&c.content)
                .map_err(|e| CommandError::Invalid(format!("bad regular expression: {e}")))?;
        }
        conditions.push((field, op, c.content.clone()));
    }

    let mut actions = Vec::new();
    for a in &draft.actions {
        actions.push(
            Action::parse(&a.action, a.params.as_deref())
                .ok_or_else(|| CommandError::Invalid(format!("unknown action {:?}", a.action)))?,
        );
    }
    if actions.is_empty() {
        return Err(CommandError::Invalid(
            "a filter with no actions would do nothing".into(),
        ));
    }
    if Match::from_i64(draft.mode) != Match::Every && conditions.is_empty() {
        return Err(CommandError::Invalid(
            "add a condition, or set the filter to match every article".into(),
        ));
    }

    let mut db = state.db.lock().await;
    Ok(snaprss_core::filters::save_filter(
        db.conn_mut(),
        &FilterDraft {
            id: draft.id,
            name,
            mode: Match::from_i64(draft.mode),
            feeds: draft.feeds,
            enabled: draft.enabled,
            conditions,
            actions,
        },
    )?)
}

#[tauri::command]
pub async fn delete_filter(state: State<'_, AppState>, id: i64) -> Res<()> {
    let db = state.db.lock().await;
    snaprss_core::filters::delete_filter(db.conn(), id)?;
    Ok(())
}

#[tauri::command]
pub async fn set_filter_enabled(state: State<'_, AppState>, id: i64, on: bool) -> Res<()> {
    let db = state.db.lock().await;
    snaprss_core::filters::set_filter_enabled(db.conn(), id, on)?;
    Ok(())
}

#[tauri::command]
pub async fn reorder_filter(state: State<'_, AppState>, id: i64, delta: i64) -> Res<()> {
    let mut db = state.db.lock().await;
    snaprss_core::filters::reorder_filter(db.conn_mut(), id, delta)?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct FilterRunRow {
    pub considered: usize,
    pub matched: usize,
    pub marked_read: usize,
    pub starred: usize,
    pub deleted: usize,
    pub labelled: usize,
}

/// Run filters over articles already stored. `filter_id` limits it to one rule,
/// which is how the editor's "apply now" button tries a rule before it is
/// turned on.
#[tauri::command]
pub async fn apply_filters_now(
    state: State<'_, AppState>,
    feed_id: Option<i64>,
    filter_id: Option<i64>,
) -> Res<FilterRunRow> {
    let mut db = state.db.lock().await;
    let r = snaprss_core::filters::run_on_existing(db.conn_mut(), feed_id, filter_id)?;
    db.recompute_counters()?;
    Ok(FilterRunRow {
        considered: r.considered,
        matched: r.matched,
        marked_read: r.marked_read,
        starred: r.starred,
        deleted: r.deleted,
        labelled: r.labelled,
    })
}

// ------------------------------------------------------- moving nodes around

/// Drop `id` onto `target`.
///
/// `where_` is "into" (make it the last child of a folder), "before" or
/// "after" (put it beside the target, sharing its parent). A null target means
/// the root.
///
/// The whole sibling list is renumbered rather than nudged, because sparse or
/// duplicated `row_to_parent` values are what make a tree drift over time.
#[tauri::command]
pub async fn move_node(
    state: State<'_, AppState>,
    id: i64,
    target: Option<i64>,
    #[allow(non_snake_case)] whereTo: String,
) -> Res<()> {
    let mut db = state.db.lock().await;

    let new_parent: Option<i64> = match (target, whereTo.as_str()) {
        (None, _) => None,
        (Some(t), "into") => {
            let kind: i64 = db
                .conn()
                .query_row("SELECT kind FROM feeds WHERE id = ?1", [t], |r| r.get(0))
                .map_err(DbError::from)?;
            if kind != NodeKind::Folder.as_i64() {
                return Err(CommandError::Invalid("only folders can hold feeds".into()));
            }
            Some(t)
        }
        (Some(t), _) => db
            .conn()
            .query_row("SELECT parent_id FROM feeds WHERE id = ?1", [t], |r| {
                r.get(0)
            })
            .map_err(DbError::from)?,
    };

    if Some(id) == new_parent {
        return Err(CommandError::Invalid("a folder cannot hold itself".into()));
    }
    // Dropping a folder inside its own subtree would detach that whole subtree
    // from the root: it becomes a ring, and every query that walks down from
    // the roots stops finding it.
    if let Some(p) = new_parent {
        if is_descendant(&db, p, id)? {
            return Err(CommandError::Invalid(
                "a folder cannot be moved inside itself".into(),
            ));
        }
    }

    let siblings: Vec<i64> = {
        let mut stmt = db
            .conn()
            .prepare(
                "SELECT id FROM feeds WHERE parent_id IS ?1 AND id <> ?2
                 ORDER BY row_to_parent, id",
            )
            .map_err(DbError::from)?;
        let v = stmt
            .query_map(rusqlite::params![new_parent, id], |r| r.get(0))
            .map_err(DbError::from)?
            .collect::<Result<Vec<i64>, _>>()
            .map_err(DbError::from)?;
        v
    };

    let mut order = siblings;
    let at = match (target, whereTo.as_str()) {
        (Some(t), "before") => order.iter().position(|x| *x == t).unwrap_or(order.len()),
        (Some(t), "after") => order
            .iter()
            .position(|x| *x == t)
            .map(|i| i + 1)
            .unwrap_or(order.len()),
        _ => order.len(),
    };
    order.insert(at.min(order.len()), id);

    let tx = db.conn_mut().transaction().map_err(DbError::from)?;
    tx.execute(
        "UPDATE feeds SET parent_id = ?2 WHERE id = ?1",
        rusqlite::params![id, new_parent],
    )
    .map_err(DbError::from)?;
    for (i, nid) in order.iter().enumerate() {
        tx.execute(
            "UPDATE feeds SET row_to_parent = ?2 WHERE id = ?1",
            rusqlite::params![nid, i as i64],
        )
        .map_err(DbError::from)?;
    }
    tx.commit().map_err(DbError::from)?;
    Ok(())
}

/// Is `candidate` inside `ancestor`'s subtree (or `ancestor` itself)?
fn is_descendant(db: &Db, candidate: i64, ancestor: i64) -> Res<bool> {
    let mut cur = Some(candidate);
    // Bounded so a database that already contains a cycle cannot hang the app.
    for _ in 0..1000 {
        let Some(c) = cur else { return Ok(false) };
        if c == ancestor {
            return Ok(true);
        }
        cur = db
            .conn()
            .query_row("SELECT parent_id FROM feeds WHERE id = ?1", [c], |r| {
                r.get(0)
            })
            .map_err(DbError::from)?;
    }
    Ok(true)
}


// ---------------------------------------------------------- startup and tray

#[tauri::command]
pub async fn set_autostart(app: tauri::AppHandle, on: bool) -> Res<bool> {
    use tauri_plugin_autostart::ManagerExt;
    let mgr = app.autolaunch();
    let r = if on { mgr.enable() } else { mgr.disable() };
    r.map_err(|e| CommandError::Invalid(format!("could not change the startup entry: {e}")))?;
    Ok(mgr.is_enabled().unwrap_or(on))
}

#[tauri::command]
pub async fn autostart_enabled(app: tauri::AppHandle) -> Res<bool> {
    use tauri_plugin_autostart::ManagerExt;
    Ok(app.autolaunch().is_enabled().unwrap_or(false))
}

/// Mirrors the stored setting into the flag the close handler reads.
#[tauri::command]
pub async fn set_close_to_tray(state: State<'_, AppState>, on: bool) -> Res<()> {
    state
        .close_to_tray
        .store(on, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub async fn quit_app(app: tauri::AppHandle, state: State<'_, AppState>) -> Res<()> {
    #[cfg(windows)]
    if let Some(w) = tauri::Manager::get_webview_window(&app, "main") {
        crate::placement::save(&w).await;
    }
    state
        .quitting
        .store(true, std::sync::atomic::Ordering::Relaxed);
    app.exit(0);
    Ok(())
}

#[tauri::command]
pub async fn set_expanded(state: State<'_, AppState>, id: i64, expanded: bool) -> Res<()> {
    let db = state.db.lock().await;
    db.set_expanded(id, expanded)?;
    Ok(())
}

// -------------------------------------------------------------------- updates

#[derive(Debug, Serialize)]
pub struct UpdateInfo {
    pub version: String,
    pub current: String,
    pub notes: Option<String>,
}

/// Where to look for `latest.json`: a GitHub repository written `owner/name`,
/// whose releases the release workflow publishes, or a full URL.
/// Where releases are published. Used when `updates.repo` is empty, so a
/// fresh install checks for updates without being set up first.
pub const DEFAULT_UPDATE_REPO: &str = "masterrite/SnapRSS";

pub fn update_endpoint(setting: &str) -> Option<url::Url> {
    let s = setting.trim().trim_end_matches('/');
    if s.is_empty() {
        return None;
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        return url::Url::parse(s).ok();
    }
    let s = s
        .trim_start_matches("github.com/")
        .trim_end_matches(".git");
    let mut parts = s.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    let ok = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if parts.next().is_some() || !ok(owner) || !ok(name) {
        return None;
    }
    url::Url::parse(&format!(
        "https://github.com/{owner}/{name}/releases/latest/download/latest.json"
    ))
    .ok()
}

/// Check for a newer release. `None` when this is the newest.
pub async fn find_update(app: &tauri::AppHandle) -> Res<Option<UpdateInfo>> {
    use tauri::Manager;
    use tauri_plugin_updater::UpdaterExt;
    let state = app.state::<AppState>();
    let repo: String = {
        let db = state.db.lock().await;
        db.conn()
            .query_row("SELECT value FROM settings WHERE key = 'updates.repo'", [], |r| r.get(0))
            .unwrap_or_default()
    };
    let repo = if repo.trim().is_empty() { DEFAULT_UPDATE_REPO.to_string() } else { repo };
    let Some(endpoint) = update_endpoint(&repo) else {
        return Err(CommandError::Invalid(format!(
            "\"{repo}\" in Settings → Updates is not a GitHub owner/name or a URL"
        )));
    };
    let updater = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .and_then(|b| b.build())
        .map_err(|e| CommandError::Invalid(format!("update check: {e}")))?;
    let found = updater.check().await.map_err(|e| {
        let msg = e.to_string();
        // A release without latest.json (still being built, or published by
        // hand) answers 404, which the updater reports as an invalid JSON.
        if msg.contains("valid release JSON") {
            CommandError::Invalid(
                "No update information was found. If a new version is being published, \
                 try again in a few minutes."
                    .into(),
            )
        } else {
            CommandError::Invalid(format!("update check: {msg}"))
        }
    })?;
    let info = found.as_ref().map(|u| UpdateInfo {
        version: u.version.clone(),
        current: u.current_version.clone(),
        notes: u.body.clone(),
    });
    *state.pending_update.lock().unwrap() = found;
    Ok(info)
}

#[tauri::command]
pub async fn check_update(app: tauri::AppHandle) -> Res<Option<UpdateInfo>> {
    find_update(&app).await
}

/// Download, verify against the public key built into the app, and install.
/// Progress goes out as `update-download`. On Windows the installer closes
/// the app and starts the new version itself.
#[tauri::command]
pub async fn install_update(app: tauri::AppHandle, state: State<'_, AppState>) -> Res<()> {
    let update = state.pending_update.lock().unwrap().take();
    let Some(update) = update else {
        return Err(CommandError::Invalid("no update to install; check again".into()));
    };
    #[cfg(windows)]
    if let Some(w) = tauri::Manager::get_webview_window(&app, "main") {
        crate::placement::save(&w).await;
    }
    let handle = app.clone();
    let mut got: usize = 0;
    update
        .download_and_install(
            move |chunk, total| {
                use tauri::Emitter;
                got += chunk;
                let _ = handle.emit("update-download", serde_json::json!({ "got": got, "total": total }));
            },
            || {},
        )
        .await
        .map_err(|e| {
            let text = e.to_string();
            CommandError::Invalid(if text.to_lowercase().contains("signature") {
                "The download did not match its signature, so it was not installed.".into()
            } else {
                format!("The update could not be installed: {text}")
            })
        })?;
    state.quitting.store(true, std::sync::atomic::Ordering::Relaxed);
    app.restart();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list_app() -> tauri::App<tauri::test::MockRuntime> {
        use tauri::Manager;
        let db = Db::open_in_memory().unwrap();
        db.conn()
            .execute_batch(
                "INSERT INTO feeds(id, kind, text, xml_url) VALUES (1, 1, 'Alpha Feed', 'https://a.test/'),
                                                                  (2, 1, 'Beta Feed', 'https://b.test/');",
            )
            .unwrap();
        for n in 0..1200 {
            db.conn()
                .execute(
                    "INSERT INTO news(feed_id, guid, title, author_name, published, received, read, deleted)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5, 0, 0)",
                    rusqlite::params![
                        1 + n % 2,
                        format!("g{n}"),
                        format!("Post {n:04} 100%_done"),
                        if n % 3 == 0 { Some("Zed") } else { None },
                        format!("2026-01-01T{:02}:{:02}:00Z", n / 60 % 24, n % 60),
                    ],
                )
                .unwrap();
        }
        let app = tauri::test::mock_app();
        let cfg = snaprss_fetch::FetchConfig::default();
        app.manage(AppState {
            db: std::sync::Arc::new(tokio::sync::Mutex::new(db)),
            http: snaprss_fetch::build_client(&cfg).unwrap(),
            fetch_cfg: cfg,
            close_to_tray: Default::default(),
            quitting: Default::default(),
            pending_update: Default::default(),
        });
        app
    }

    /// Every article in a scope is reachable page by page, and a search
    /// finds articles on pages that were never loaded.
    #[test]
    fn the_whole_scope_is_reachable_and_searchable() {
        use tauri::Manager;
        let app = list_app();
        let st = || app.state::<AppState>();
        tauri::async_runtime::block_on(async {
            let mut seen = std::collections::HashSet::new();
            let mut offset = 0;
            loop {
                let page = news_list(st(), "all".into(), Some(500), Some(offset), None, None, None).await.unwrap();
                for r in &page {
                    assert!(seen.insert(r.id), "article {} came twice", r.id);
                }
                offset += page.len() as i64;
                if page.len() < 500 {
                    break;
                }
            }
            assert_eq!(seen.len(), 1200);

            let hit = news_list(st(), "all".into(), None, None, None, Some("post 0007".into()), None).await.unwrap();
            assert_eq!(hit.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(), ["Post 0007 100%_done"]);
            // Every word must match; the feed name counts.
            let hit = news_list(st(), "all".into(), None, None, None, Some("beta 0007".into()), None).await.unwrap();
            assert_eq!(hit.len(), 1, "post 7 is in Beta Feed");
            let hit = news_list(st(), "feed:1".into(), None, None, None, Some("0007".into()), None).await.unwrap();
            assert!(hit.is_empty(), "the search stays inside the scope");
            // % and _ are text, not wildcards.
            let all = news_list(st(), "all".into(), Some(5000), None, None, Some("100%_".into()), None).await.unwrap();
            assert_eq!(all.len(), 1200);
            let none = news_list(st(), "all".into(), None, None, None, Some("100_%".into()), None).await.unwrap();
            assert!(none.is_empty());
            // Letter case is ignored in every script, not only A to Z.
            {
                let handle = st().db.clone();
                let db = handle.lock().await;
                db.conn().execute("UPDATE news SET title = 'Новости дня' WHERE guid = 'g5'", []).unwrap();
                db.conn().execute("UPDATE news SET title = 'Élan VITAL' WHERE guid = 'g6'", []).unwrap();
            }
            let ru = news_list(st(), "all".into(), None, None, None, Some("новости".into()), None).await.unwrap();
            assert_eq!(ru.len(), 1, "Cyrillic, typed in lower case");
            let fr = news_list(st(), "all".into(), None, None, None, Some("élan vital".into()), None).await.unwrap();
            assert_eq!(fr.len(), 1, "accented Latin");
        });
    }

    #[test]
    fn undoing_a_delete_after_emptying_deleted_does_not_bring_the_article_back() {
        use tauri::Manager;
        let app = list_app();
        let st = || app.state::<AppState>();
        tauri::async_runtime::block_on(async {
            let first = news_list(st(), "all".into(), Some(1), None, None, None, None).await.unwrap();
            let id = first[0].id;
            set_deleted(st(), vec![id], true).await.unwrap();
            purge_deleted(st(), None).await.unwrap();
            set_deleted(st(), vec![id], false).await.unwrap();
            let all = news_list(st(), "all".into(), Some(5000), None, None, None, None).await.unwrap();
            assert!(all.iter().all(|r| r.id != id), "the emptied article came back as a blank row");
        });
    }

    #[test]
    fn the_list_sorts() {
        use tauri::Manager;
        let app = list_app();
        let st = || app.state::<AppState>();
        tauri::async_runtime::block_on(async {
            let titles = |v: &[ListItem]| v.iter().map(|r| r.title.clone()).collect::<Vec<_>>();
            let asc = news_list(st(), "all".into(), Some(3), None, None, None, Some("title".into())).await.unwrap();
            assert_eq!(titles(&asc), ["Post 0000 100%_done", "Post 0001 100%_done", "Post 0002 100%_done"]);
            let desc = news_list(st(), "all".into(), Some(1), None, None, None, Some("-title".into())).await.unwrap();
            assert_eq!(desc[0].title, "Post 1199 100%_done");
            let by_feed = news_list(st(), "all".into(), Some(5000), None, None, None, Some("-feed".into())).await.unwrap();
            assert!(by_feed[..600].iter().all(|r| r.feed_title == "Beta Feed"));
            // Articles with an author come before those without, either way.
            let by_author = news_list(st(), "all".into(), Some(5000), None, None, None, Some("-author".into())).await.unwrap();
            assert!(by_author[..400].iter().all(|r| r.author.as_deref() == Some("Zed")));
            let newest = news_list(st(), "all".into(), Some(1), None, None, None, None).await.unwrap();
            let oldest = news_list(st(), "all".into(), Some(1), None, None, None, Some("date".into())).await.unwrap();
            assert!(newest[0].published > oldest[0].published);
        });
    }

    /// The label editor sends camelCase keys inside `draft`. Tauri renames
    /// top-level arguments only, so these have to match on their own.
    #[test]
    fn label_colours_arrive() {
        let d: LabelDraft = serde_json::from_str(
            r##"{"id":null,"name":"Later","colorBg":"#3b6ea5","colorText":"#ffffff"}"##,
        )
        .unwrap();
        assert_eq!(d.color_bg.as_deref(), Some("#3b6ea5"));
        assert_eq!(d.color_text.as_deref(), Some("#ffffff"));
    }

    #[test]
    fn every_scope_but_deleted_hides_deleted_articles() {
        for s in ["all", "unread", "starred", "feed:1", "folder:1", "label:1"] {
            assert!(scope_filter(s).unwrap().0.contains("news.deleted = 0"), "{s}");
        }
        assert!(scope_filter("deleted").unwrap().0.contains("news.deleted = 1"));
        assert!(scope_filter("nonsense").is_err());
    }

    #[test]
    fn update_endpoint_from_the_setting() {
        let gh = "https://github.com/me/snaprss/releases/latest/download/latest.json";
        for v in ["me/snaprss", " me/snaprss/ ", "github.com/me/snaprss", "me/snaprss.git"] {
            assert_eq!(update_endpoint(v).map(|u| u.to_string()).as_deref(), Some(gh), "{v}");
        }
        assert_eq!(
            update_endpoint("https://example.test/latest.json").map(|u| u.to_string()).as_deref(),
            Some("https://example.test/latest.json")
        );
        for bad in ["", "me", "me/snap rss", "a/b/c", "../x/y"] {
            assert!(update_endpoint(bad).is_none(), "{bad}");
        }
        assert_eq!(
            update_endpoint(DEFAULT_UPDATE_REPO).map(|u| u.to_string()).as_deref(),
            Some("https://github.com/masterrite/SnapRSS/releases/latest/download/latest.json")
        );
    }
}
