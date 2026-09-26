//! One feed, end to end: conditional GET, parse, ingest, record status.

use chrono::Utc;
use futures::stream::{self, StreamExt};
use reqwest::Client;

use snaprss_core::{Db, DbError};

use crate::http::{fetch_as, FetchConfig, FetchError, FetchOutcome, Validators};
use crate::ingest::{ingest, IngestReport, IngestRules};
use crate::schedule::{due_feeds, DueFeed};

#[derive(Debug)]
pub enum UpdateOutcome {
    /// Server said 304. No parse, no writes beyond the timestamp.
    NotModified,
    Ingested(IngestReport),
    Failed {
        message: String,
        transient: bool,
    },
}

#[derive(Debug, Default)]
pub struct UpdateSummary {
    pub attempted: usize,
    pub not_modified: usize,
    pub ingested: usize,
    pub failed: usize,
    pub new_articles: usize,
    /// Sound files filters asked to play, each once.
    pub sounds: Vec<String>,
    /// (feed title, article title) for new articles a filter asked to be
    /// notified about.
    pub notices: Vec<(String, String)>,
}

fn record_success(db: &Db, feed_id: i64, v: &Validators) -> Result<(), DbError> {
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    db.conn().execute(
        "UPDATE feeds SET
            updated = ?1,
            status = '',
            consecutive_failures = 0,
            http_etag = COALESCE(?2, http_etag),
            http_last_modified = COALESCE(?3, http_last_modified)
         WHERE id = ?4",
        rusqlite::params![now, v.etag, v.last_modified, feed_id],
    )?;
    Ok(())
}

fn record_failure(db: &Db, feed_id: i64, message: &str) -> Result<(), DbError> {
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    // `updated` moves even on failure, otherwise a permanently broken feed is
    // due on every single tick and the backoff never applies.
    db.conn().execute(
        "UPDATE feeds SET
            updated = ?1,
            status = ?2,
            consecutive_failures = consecutive_failures + 1
         WHERE id = ?3",
        rusqlite::params![now, message, feed_id],
    )?;
    Ok(())
}

/// Fetch one feed and apply whatever came back. The same writes as
/// [`apply_fetched`], for one feed.
///
/// A redirect does not rewrite `xml_url`: reqwest does not say whether it
/// followed a permanent or a temporary one, and adopting a temporary target
/// (a tokenised or maintenance URL) would lose the real address.
pub async fn update_feed(
    db: &mut Db,
    client: &Client,
    feed: &DueFeed,
    cfg: &FetchConfig,
) -> Result<UpdateOutcome, DbError> {
    let known = Validators {
        etag: feed.etag.clone(),
        last_modified: feed.last_modified.clone(),
    };
    let result = fetch_as(client, &feed.xml_url, &known, cfg, feed.credentials.as_ref()).await;
    apply_one(db, feed, result)
}

/// Update everything due, `concurrency` at a time.
///
/// Feeds are fetched concurrently but written serially: SQLite takes one writer
/// One feed's fetch result, waiting to be written.
pub struct Fetched {
    pub feed: DueFeed,
    pub result: Result<FetchOutcome, FetchError>,
}

/// Progress for the UI, one per feed as it lands.
#[derive(Debug, Clone)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub title: String,
    pub ok: bool,
}

/// Fetch every feed in `feeds`. Touches no database.
///
/// This is split from [`apply_fetched`] on purpose. The obvious version takes
/// `&mut Db` and awaits the network with it held, which means the caller holds
/// the application's single database lock for as long as the slowest server
/// takes to answer. Every click in the UI then blocks behind a poll it did not
/// ask for. Separating the two lets the caller hold the lock only for the
/// milliseconds it takes to read the due list and write the results.
pub async fn fetch_all<F>(
    client: &Client,
    cfg: &FetchConfig,
    feeds: Vec<DueFeed>,
    concurrency: usize,
    on_progress: F,
) -> Vec<Fetched>
where
    F: Fn(Progress) + Send + Sync,
{
    let total = feeds.len();
    let done = std::sync::atomic::AtomicUsize::new(0);
    let on_progress = &on_progress;
    let done = &done;

    stream::iter(feeds)
        .map(move |f| async move {
            let known = Validators {
                etag: f.etag.clone(),
                last_modified: f.last_modified.clone(),
            };
            let result = fetch_as(client, &f.xml_url, &known, cfg, f.credentials.as_ref()).await;
            let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            on_progress(Progress {
                done: n,
                total,
                title: f.title.clone(),
                ok: result.is_ok(),
            });
            Fetched { feed: f, result }
        })
        .buffer_unordered(concurrency.max(1))
        .collect()
        .await
}

/// Parse and write what [`fetch_all`] returned. No network, so the lock is
/// held only for the writes.
///
/// A failure writing one feed is recorded against that feed and the rest carry
/// on. Propagating it threw away every other feed's results: a feed removed
/// while the fetch was in flight, or one filter action that could not be
/// applied, stopped every feed after it from updating, on every poll.
pub fn apply_fetched(db: &mut Db, fetched: Vec<Fetched>) -> Result<UpdateSummary, DbError> {
    let mut summary = UpdateSummary::default();

    for Fetched { feed, result } in fetched {
        // Removed while its fetch was in flight: nothing to write. The id
        // alone does not say so, because SQLite gives a removed feed's id to
        // the next one added, which then took the old feed's articles, title
        // and ETag. Its address has to still be the one fetched.
        let exists = db
            .conn()
            .query_row(
                "SELECT 1 FROM feeds WHERE id = ?1 AND xml_url = ?2",
                rusqlite::params![feed.id, feed.xml_url],
                |_| Ok(()),
            )
            .is_ok();
        if !exists {
            continue;
        }
        summary.attempted += 1;
        match apply_one(db, &feed, result) {
            Ok(UpdateOutcome::NotModified) => summary.not_modified += 1,
            Ok(UpdateOutcome::Ingested(report)) => {
                summary.ingested += 1;
                summary.new_articles += report.inserted;
                for s in report.sounds {
                    if !summary.sounds.contains(&s) {
                        summary.sounds.push(s);
                    }
                }
                summary.notices.extend(report.notify.into_iter().map(|t| (feed.title.clone(), t)));
            }
            Ok(UpdateOutcome::Failed { .. }) => summary.failed += 1,
            Err(e) => {
                summary.failed += 1;
                record_failure(db, feed.id, &e.to_string())?;
            }
        }
    }

    Ok(summary)
}

fn apply_one(
    db: &mut Db,
    feed: &DueFeed,
    result: Result<FetchOutcome, FetchError>,
) -> Result<UpdateOutcome, DbError> {
    match result {
        Ok(FetchOutcome::NotModified { validators }) => {
            record_success(db, feed.id, &validators)?;
            Ok(UpdateOutcome::NotModified)
        }
        Ok(FetchOutcome::Body {
            bytes,
            validators,
            final_url,
            content_type,
        }) => {
            // Relative links resolve against where the feed actually came
            // from, which after a redirect is not the address asked for.
            let base = final_url.as_deref().unwrap_or(&feed.xml_url);
            match crate::ingest::parse_feed_with(bytes.as_slice(), base, content_type.as_deref()) {
                Ok(parsed) => {
                    let rules = IngestRules::load(db, feed.id)?;
                    let report = ingest(db, feed.id, &parsed, &rules)?;
                    adopt_title(db, feed, &parsed)?;
                    record_success(db, feed.id, &validators)?;
                    Ok(UpdateOutcome::Ingested(report))
                }
                Err(e) => {
                    let message = format!("parse: {e}");
                    record_failure(db, feed.id, &message)?;
                    Ok(UpdateOutcome::Failed {
                        message,
                        transient: false,
                    })
                }
            }
        }
        Err(e) => {
            let message = e.to_string();
            record_failure(db, feed.id, &message)?;
            Ok(UpdateOutcome::Failed {
                message,
                transient: e.is_transient(),
            })
        }
    }
}

/// Store the feed's own title, and show it in place of the host name a feed
/// added by URL starts out with. A name the user gave it is left alone.
fn adopt_title(db: &Db, feed: &DueFeed, parsed: &feed_rs::model::Feed) -> Result<(), DbError> {
    let Some(title) = parsed
        .title
        .as_ref()
        .map(|t| t.content.trim().to_string())
        .filter(|t| !t.is_empty())
    else {
        return adopt_site(db, feed, parsed);
    };
    let host = url::Url::parse(&feed.xml_url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_default();
    db.conn().execute(
        "UPDATE feeds SET title = ?1,
                text = CASE WHEN text IS NULL OR text = '' OR text = ?2 THEN ?1 ELSE text END
         WHERE id = ?3",
        rusqlite::params![title, host, feed.id],
    )?;
    adopt_site(db, feed, parsed)
}

/// The site the feed belongs to, from its own link, when none is stored.
/// Its icon is looked up there.
fn adopt_site(db: &Db, feed: &DueFeed, parsed: &feed_rs::model::Feed) -> Result<(), DbError> {
    let site = parsed
        .links
        .iter()
        .filter(|l| matches!(l.rel.as_deref(), None | Some("alternate")))
        .map(|l| l.href.trim())
        .find(|h| (h.starts_with("http://") || h.starts_with("https://")) && *h != feed.xml_url);
    if let Some(site) = site {
        db.conn().execute(
            "UPDATE feeds SET html_url = ?1 WHERE id = ?2 AND (html_url IS NULL OR html_url = '')",
            rusqlite::params![site, feed.id],
        )?;
    }
    Ok(())
}

/// Fetch and apply in one call, holding `db` throughout. Convenient for tests
/// and one-shot tools; the application splits the two phases instead.
pub async fn update_due(
    db: &mut Db,
    client: &Client,
    cfg: &FetchConfig,
    global_minutes: i64,
    concurrency: usize,
) -> Result<UpdateSummary, DbError> {
    let feeds = due_feeds(db, Utc::now(), global_minutes)?;
    let fetched = fetch_all(client, cfg, feeds, concurrency, |_| {}).await;
    apply_fetched(db, fetched)
}
