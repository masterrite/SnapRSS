//! Exercises the update path against a fake HTTP server.
//!
//! The conditional-GET behaviour is the part worth testing properly: it is easy
//! to write code that looks like it sends validators and quietly does not.

use chrono::{Duration, Utc};
use snaprss_core::Db;
use snaprss_fetch::{
    build_client, due_feeds, fetch, ingest, schedule, update_feed, DueFeed, FetchConfig,
    FetchOutcome, IngestRules, UpdateOutcome, Validators,
};
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Kernel Notes</title>
  <id>urn:kn</id>
  <updated>2026-09-16T10:00:00Z</updated>
  <entry>
    <title>First post</title>
    <id>urn:kn:1</id>
    <updated>2026-09-16T10:00:00Z</updated>
    <published>2026-09-16T10:00:00Z</published>
    <link rel="alternate" href="https://kn.test/1"/>
    <summary>the first one</summary>
    <author><name>Marta Vogel</name></author>
  </entry>
  <entry>
    <title>Second post</title>
    <id>urn:kn:2</id>
    <published>2026-09-15T09:00:00Z</published>
    <link rel="alternate" href="https://kn.test/2"/>
    <summary>the second one</summary>
  </entry>
</feed>"#;

/// Same feed, one entry edited and one added.
const ATOM_V2: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <title>Kernel Notes</title>
  <id>urn:kn</id>
  <entry>
    <title>First post (corrected)</title>
    <id>urn:kn:1</id>
    <published>2026-09-16T10:00:00Z</published>
    <link rel="alternate" href="https://kn.test/1"/>
    <summary>the first one, fixed</summary>
  </entry>
  <entry>
    <title>Second post</title>
    <id>urn:kn:2</id>
    <published>2026-09-15T09:00:00Z</published>
    <link rel="alternate" href="https://kn.test/2"/>
    <summary>the second one</summary>
  </entry>
  <entry>
    <title>Third post</title>
    <id>urn:kn:3</id>
    <published>2026-09-17T08:00:00Z</published>
    <link rel="alternate" href="https://kn.test/3"/>
    <summary>brand new</summary>
  </entry>
</feed>"#;

/// No <id> anywhere: identity has to fall back to the link.
const RSS_NO_GUID: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
  <title>Signal Path</title>
  <item>
    <title>Only by link</title>
    <link>https://sp.test/a</link>
    <description>body</description>
    <pubDate>Tue, 16 Sep 2026 10:00:00 GMT</pubDate>
  </item>
</channel></rss>"#;

/// No id and every item shares one link: only title + date can tell them apart.
const RSS_SAME_LINK: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
  <title>Broken Aggregator</title>
  <item>
    <title>Alpha</title><link>https://agg.test/</link>
    <description>a</description><pubDate>Tue, 16 Sep 2026 10:00:00 GMT</pubDate>
  </item>
  <item>
    <title>Beta</title><link>https://agg.test/</link>
    <description>b</description><pubDate>Tue, 16 Sep 2026 11:00:00 GMT</pubDate>
  </item>
</channel></rss>"#;

fn db_with_feed(url: &str) -> (Db, i64) {
    let mut db = Db::open_in_memory().unwrap();
    db.conn()
        .execute(
            "INSERT INTO feeds(kind, text, xml_url, update_interval_enable, update_interval,
                               update_interval_type)
             VALUES(1, 'Test feed', ?1, 1, 15, 'minutes')",
            [url],
        )
        .unwrap();
    let id = db.conn().last_insert_rowid();
    let _ = &mut db;
    (db, id)
}

fn due(id: i64, url: &str) -> DueFeed {
    DueFeed {
        id,
        title: "Test feed".into(),
        xml_url: url.to_string(),
        etag: None,
        last_modified: None,
    }
}

#[tokio::test]
async fn fetches_parses_and_ingests() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(ATOM)
                .insert_header("etag", "\"v1\"")
                .insert_header("last-modified", "Wed, 16 Sep 2026 10:00:00 GMT"),
        )
        .mount(&server)
        .await;

    let url = format!("{}/feed.xml", server.uri());
    let (mut db, id) = db_with_feed(&url);
    let client = build_client(&FetchConfig::default()).unwrap();

    let out = update_feed(&mut db, &client, &due(id, &url), &FetchConfig::default())
        .await
        .unwrap();
    match out {
        UpdateOutcome::Ingested(r) => assert_eq!(r.inserted, 2),
        other => panic!("expected Ingested, got {other:?}"),
    }

    assert_eq!(db.total_unread().unwrap(), 2);

    // Validators were stored for next time.
    let (etag, lm): (Option<String>, Option<String>) = db
        .conn()
        .query_row(
            "SELECT http_etag, http_last_modified FROM feeds WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(etag.as_deref(), Some("\"v1\""));
    assert!(lm.is_some());

    // The feed's own title was adopted.
    let title: String = db
        .conn()
        .query_row("SELECT title FROM feeds WHERE id = ?1", [id], |r| r.get(0))
        .unwrap();
    assert_eq!(title, "Kernel Notes");
}

#[tokio::test]
async fn sends_validators_and_handles_304() {
    let server = MockServer::start().await;

    // Only matches when BOTH conditional headers are present, and the etag is
    // the one we stored. If the client fails to send them the mock will not
    // match and the test fails with a 404.
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .and(header("if-none-match", "\"v1\""))
        .and(header_exists("if-modified-since"))
        .respond_with(ResponseTemplate::new(304).insert_header("etag", "\"v1\""))
        .mount(&server)
        .await;

    let url = format!("{}/feed.xml", server.uri());
    let client = build_client(&FetchConfig::default()).unwrap();

    let known = Validators {
        etag: Some("\"v1\"".into()),
        last_modified: Some("Wed, 16 Sep 2026 10:00:00 GMT".into()),
    };
    let out = fetch(&client, &url, &known, &FetchConfig::default())
        .await
        .unwrap();
    assert!(matches!(out, FetchOutcome::NotModified { .. }));
}

#[tokio::test]
async fn not_modified_writes_nothing_but_the_timestamp() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(304))
        .mount(&server)
        .await;

    let url = format!("{}/feed.xml", server.uri());
    let (mut db, id) = db_with_feed(&url);
    let client = build_client(&FetchConfig::default()).unwrap();

    let out = update_feed(&mut db, &client, &due(id, &url), &FetchConfig::default())
        .await
        .unwrap();
    assert!(matches!(out, UpdateOutcome::NotModified));
    assert_eq!(db.count("news").unwrap(), 0);

    // A 304 counts as a success, so any prior failure streak resets.
    let (status, fails): (String, i64) = db
        .conn()
        .query_row(
            "SELECT status, consecutive_failures FROM feeds WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "");
    assert_eq!(fails, 0);
}

#[tokio::test]
async fn second_poll_adds_only_what_is_new() {
    let server = MockServer::start().await;
    let url = format!("{}/feed.xml", server.uri());
    let (mut db, id) = db_with_feed(&url);
    let client = build_client(&FetchConfig::default()).unwrap();
    let cfg = FetchConfig::default();

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ATOM))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    update_feed(&mut db, &client, &due(id, &url), &cfg)
        .await
        .unwrap();
    assert_eq!(db.count("news").unwrap(), 2);

    // Mark one read; a re-poll must not resurrect it as unread.
    db.conn()
        .execute("UPDATE news SET read = 1 WHERE title = 'Second post'", [])
        .unwrap();

    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ATOM_V2))
        .mount(&server)
        .await;
    let out = update_feed(&mut db, &client, &due(id, &url), &cfg)
        .await
        .unwrap();

    match out {
        UpdateOutcome::Ingested(r) => {
            assert_eq!(r.inserted, 1, "only the third post is new");
            assert_eq!(r.duplicates, 2);
            assert_eq!(r.updated, 1, "the first post was edited");
        }
        other => panic!("expected Ingested, got {other:?}"),
    }

    assert_eq!(db.count("news").unwrap(), 3);

    let still_read: i64 = db
        .conn()
        .query_row(
            "SELECT read FROM news WHERE title = 'Second post'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(still_read, 1, "read state survived the re-poll");

    let edited: String = db
        .conn()
        .query_row("SELECT title FROM news WHERE guid = 'urn:kn:1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(edited, "First post (corrected)");
}

#[tokio::test]
async fn dedups_by_link_when_there_is_no_guid() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(RSS_NO_GUID))
        .mount(&server)
        .await;

    let url = format!("{}/feed.xml", server.uri());
    let (mut db, id) = db_with_feed(&url);
    let client = build_client(&FetchConfig::default()).unwrap();
    let cfg = FetchConfig::default();

    update_feed(&mut db, &client, &due(id, &url), &cfg)
        .await
        .unwrap();
    update_feed(&mut db, &client, &due(id, &url), &cfg)
        .await
        .unwrap();
    update_feed(&mut db, &client, &due(id, &url), &cfg)
        .await
        .unwrap();

    assert_eq!(db.count("news").unwrap(), 1, "three polls, one article");
}

#[tokio::test]
async fn distinguishes_items_sharing_a_link() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string(RSS_SAME_LINK))
        .mount(&server)
        .await;

    let url = format!("{}/feed.xml", server.uri());
    let (mut db, id) = db_with_feed(&url);
    let client = build_client(&FetchConfig::default()).unwrap();
    let cfg = FetchConfig::default();

    update_feed(&mut db, &client, &due(id, &url), &cfg)
        .await
        .unwrap();
    // Link matching alone would collapse these into one. Title + date saves it.
    assert_eq!(db.count("news").unwrap(), 2);

    update_feed(&mut db, &client, &due(id, &url), &cfg)
        .await
        .unwrap();
    assert_eq!(db.count("news").unwrap(), 2, "still two after a re-poll");
}

#[tokio::test]
async fn records_failure_and_backs_off() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let url = format!("{}/feed.xml", server.uri());
    let (mut db, id) = db_with_feed(&url);
    let client = build_client(&FetchConfig::default()).unwrap();
    let cfg = FetchConfig::default();

    for expected in 1..=3 {
        let out = update_feed(&mut db, &client, &due(id, &url), &cfg)
            .await
            .unwrap();
        match out {
            UpdateOutcome::Failed { transient, .. } => assert!(transient, "503 is retryable"),
            other => panic!("expected Failed, got {other:?}"),
        }
        let fails: i64 = db
            .conn()
            .query_row(
                "SELECT consecutive_failures FROM feeds WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fails, expected);
    }

    let status: String = db
        .conn()
        .query_row("SELECT status FROM feeds WHERE id = ?1", [id], |r| r.get(0))
        .unwrap();
    assert!(status.contains("503"), "status was {status:?}");
}

#[tokio::test]
async fn a_404_is_not_transient() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let url = format!("{}/feed.xml", server.uri());
    let (mut db, id) = db_with_feed(&url);
    let client = build_client(&FetchConfig::default()).unwrap();

    match update_feed(&mut db, &client, &due(id, &url), &FetchConfig::default())
        .await
        .unwrap()
    {
        UpdateOutcome::Failed { transient, .. } => assert!(!transient),
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn garbage_body_fails_the_feed_not_the_process() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html><body>hi</body></html>"))
        .mount(&server)
        .await;

    let url = format!("{}/feed.xml", server.uri());
    let (mut db, id) = db_with_feed(&url);
    let client = build_client(&FetchConfig::default()).unwrap();

    match update_feed(&mut db, &client, &due(id, &url), &FetchConfig::default())
        .await
        .unwrap()
    {
        UpdateOutcome::Failed { message, transient } => {
            assert!(message.starts_with("parse:"), "{message}");
            assert!(!transient);
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(db.count("news").unwrap(), 0);
}

#[test]
fn rejects_non_http_urls() {
    let client = build_client(&FetchConfig::default()).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    for bad in [
        "file:///etc/passwd",
        "ftp://example.test/feed.xml",
        "nonsense",
    ] {
        let r = rt.block_on(fetch(
            &client,
            bad,
            &Validators::default(),
            &FetchConfig::default(),
        ));
        assert!(r.is_err(), "{bad} should be rejected");
    }
}

#[test]
fn backoff_grows_then_caps() {
    let base = Duration::minutes(15);
    assert_eq!(schedule::backoff(base, 0), base);
    assert_eq!(schedule::backoff(base, 1), Duration::minutes(30));
    assert_eq!(schedule::backoff(base, 3), Duration::hours(2));
    assert_eq!(
        schedule::backoff(Duration::hours(12), 5),
        Duration::hours(24),
        "capped at a day"
    );
}

#[test]
fn schedules_only_what_is_due() {
    let db = Db::open_in_memory().unwrap();
    let now = Utc::now();
    let long_ago = (now - Duration::hours(6)).to_rfc3339();
    let recent = (now - Duration::minutes(2)).to_rfc3339();

    db.conn()
        .execute_batch(&format!(
            "INSERT INTO feeds(kind, text, xml_url, update_interval_enable, update_interval,
                               update_interval_type, updated)
               VALUES(1, 'stale', 'https://a.test/f.xml', 1, 15, 'minutes', '{long_ago}');
             INSERT INTO feeds(kind, text, xml_url, update_interval_enable, update_interval,
                               update_interval_type, updated)
               VALUES(1, 'fresh', 'https://b.test/f.xml', 1, 15, 'minutes', '{recent}');
             INSERT INTO feeds(kind, text, xml_url, updated)
               VALUES(1, 'never', 'https://c.test/f.xml', NULL);
             INSERT INTO feeds(kind, text, xml_url, disable_update, updated)
               VALUES(1, 'paused', 'https://d.test/f.xml', 1, '{long_ago}');
             INSERT INTO feeds(kind, text, xml_url)
               VALUES(0, 'a folder', '');"
        ))
        .unwrap();

    let names: Vec<String> = due_feeds(&db, now, 15)
        .unwrap()
        .into_iter()
        .map(|f| {
            db.conn()
                .query_row("SELECT text FROM feeds WHERE id = ?1", [f.id], |r| r.get(0))
                .unwrap()
        })
        .collect();

    assert!(names.contains(&"stale".to_string()));
    assert!(names.contains(&"never".to_string()));
    assert!(!names.contains(&"fresh".to_string()));
    assert!(!names.contains(&"paused".to_string()));
    assert!(!names.contains(&"a folder".to_string()));
    assert_eq!(names.len(), 2);
}

#[test]
fn respects_the_old_article_cutoff() {
    let mut db = Db::open_in_memory().unwrap();
    db.conn()
        .execute(
            "INSERT INTO feeds(kind, text, xml_url, add_any_date, avoid_old_enable, avoid_old_before)
             VALUES(1, 'cutoff', 'https://x.test/f.xml', 0, 1, '2026-09-16T00:00:00Z')",
            [],
        )
        .unwrap();
    let id = db.conn().last_insert_rowid();

    let parsed = feed_rs::parser::parse(ATOM.as_bytes()).unwrap();
    let rules = IngestRules::load(&db, id).unwrap();
    assert!(!rules.add_any_date);

    let report = ingest(&mut db, id, &parsed, &rules).unwrap();
    assert_eq!(report.inserted, 1, "the 15 September entry is too old");
    assert_eq!(report.too_old, 1);
}

#[test]
fn interval_units() {
    assert_eq!(
        schedule::interval_to_duration(30, Some("minutes")),
        Duration::minutes(30)
    );
    assert_eq!(
        schedule::interval_to_duration(2, Some("hours")),
        Duration::hours(2)
    );
    assert_eq!(
        schedule::interval_to_duration(1, Some("days")),
        Duration::days(1)
    );
    assert_eq!(
        schedule::interval_to_duration(0, None),
        Duration::minutes(1),
        "zero is clamped, never a busy loop"
    );
}

// ---------------------------------------------------------------------------
// filters on ingest
// ---------------------------------------------------------------------------
//
// The engine itself is tested in snaprss-core. What matters here is that
// ingestion actually consults it, in the same transaction as the insert, and
// only for rows that are genuinely new.

fn add_filter(db: &Db, conds: &[(&str, &str, &str)], acts: &[(&str, Option<&str>)]) {
    db.conn()
        .execute(
            "INSERT INTO filters (name, type, feeds, enable, num) VALUES ('f', 1, NULL, 1, 0)",
            [],
        )
        .unwrap();
    let fid = db.conn().last_insert_rowid();
    for (f, c, content) in conds {
        db.conn()
            .execute(
                "INSERT INTO filter_conditions (filter_id, field, condition, content)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![fid, f, c, content],
            )
            .unwrap();
    }
    for (a, p) in acts {
        db.conn()
            .execute(
                "INSERT INTO filter_actions (filter_id, action, params) VALUES (?1, ?2, ?3)",
                rusqlite::params![fid, a, p],
            )
            .unwrap();
    }
}

fn flag(db: &Db, title: &str, column: &str) -> i64 {
    db.conn()
        .query_row(
            &format!("SELECT {column} FROM news WHERE title = ?1"),
            [title],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn ingest_applies_filters_to_new_articles() {
    let (mut db, id) = db_with_feed("https://kn.test/feed");
    add_filter(
        &db,
        &[("title", "contains", "second")],
        &[("mark_read", None)],
    );

    let parsed = feed_rs::parser::parse(ATOM.as_bytes()).unwrap();
    let report = ingest(&mut db, id, &parsed, &IngestRules::default()).unwrap();

    assert_eq!(report.inserted, 2);
    assert_eq!(report.filtered, 1);
    assert_eq!(flag(&db, "Second post", "read"), 1);
    assert_eq!(flag(&db, "First post", "read"), 0);
}

#[test]
fn an_article_a_filter_deletes_never_counts_as_unread() {
    // The point of filtering on ingest rather than after it: the unread badge
    // must never have counted this article, not even for an instant.
    let (mut db, id) = db_with_feed("https://kn.test/feed");
    add_filter(&db, &[("title", "contains", "second")], &[("delete", None)]);

    let parsed = feed_rs::parser::parse(ATOM.as_bytes()).unwrap();
    let report = ingest(&mut db, id, &parsed, &IngestRules::default()).unwrap();

    assert_eq!(report.inserted, 2);
    assert_eq!(report.filtered_away, 1);
    assert_eq!(flag(&db, "Second post", "deleted"), 1);

    let unread: i64 = db
        .conn()
        .query_row("SELECT unread FROM feeds WHERE id = ?1", [id], |r| r.get(0))
        .unwrap();
    assert_eq!(unread, 1, "the deleted one is not in the counter");
    assert_eq!(db.total_unread().unwrap(), 1);
}

#[test]
fn filters_apply_labels_on_arrival() {
    let (mut db, id) = db_with_feed("https://kn.test/feed");
    db.conn()
        .execute("INSERT INTO labels (id, name) VALUES (4, 'Work')", [])
        .unwrap();
    add_filter(&db, &[("author", "contains", "vogel")], &[("add_label", Some("4"))]);

    let parsed = feed_rs::parser::parse(ATOM.as_bytes()).unwrap();
    ingest(&mut db, id, &parsed, &IngestRules::default()).unwrap();

    let labelled: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM news_labels nl
             JOIN news n ON n.id = nl.news_id WHERE n.title = 'First post'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(labelled, 1);
}

#[test]
fn re_polling_does_not_re_run_filters_over_existing_articles() {
    // Otherwise a filter added later would reach back and undo a read or
    // starred state the user set by hand, on every single poll.
    let (mut db, id) = db_with_feed("https://kn.test/feed");
    let parsed = feed_rs::parser::parse(ATOM.as_bytes()).unwrap();
    ingest(&mut db, id, &parsed, &IngestRules::default()).unwrap();

    db.conn()
        .execute("UPDATE news SET starred = 1 WHERE title = 'First post'", [])
        .unwrap();
    add_filter(&db, &[("title", "contains", "first")], &[("delete", None)]);

    // V2 edits "First post" and adds a third entry.
    let v2 = feed_rs::parser::parse(ATOM_V2.as_bytes()).unwrap();
    let report = ingest(&mut db, id, &v2, &IngestRules::default()).unwrap();

    assert_eq!(report.inserted, 1, "only the third entry is new");
    assert_eq!(
        report.filtered, 0,
        "the edited first post is not a new row, so no filter ran on it"
    );
    assert_eq!(
        flag(&db, "First post (corrected)", "deleted"),
        0,
        "the starred article the user kept must survive"
    );
}

#[test]
fn ingest_without_filters_is_unchanged() {
    let (mut db, id) = db_with_feed("https://kn.test/feed");
    let parsed = feed_rs::parser::parse(ATOM.as_bytes()).unwrap();
    let report = ingest(&mut db, id, &parsed, &IngestRules::default()).unwrap();
    assert_eq!(report.inserted, 2);
    assert_eq!(report.filtered, 0);
    assert_eq!(db.total_unread().unwrap(), 2);
}

// ---------------------------------------------------------------------------
// found in review
// ---------------------------------------------------------------------------

use snaprss_fetch::{apply_fetched, parse_feed, Fetched};

fn body(xml: &str) -> Result<FetchOutcome, snaprss_fetch::FetchError> {
    Ok(FetchOutcome::Body {
        bytes: xml.as_bytes().to_vec(),
        validators: Validators::default(),
        final_url: None,
        content_type: None,
    })
}

fn rows(db: &Db) -> i64 {
    db.conn().query_row("SELECT COUNT(*) FROM news", [], |r| r.get(0)).unwrap()
}

#[test]
fn an_item_with_no_guid_link_or_title_is_stored_once() {
    let xml = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Status</title>
        <item><description>All systems normal.</description><pubDate>Tue, 16 Sep 2026 10:00:00 GMT</pubDate></item>
    </channel></rss>"#;
    let (mut db, id) = db_with_feed("https://status.test/rss");
    let rules = IngestRules::load(&db, id).unwrap();
    for _ in 0..3 {
        let parsed = parse_feed(xml.as_bytes(), "https://status.test/rss").unwrap();
        ingest(&mut db, id, &parsed, &rules).unwrap();
    }
    assert_eq!(rows(&db), 1);
}

#[test]
fn a_new_guid_at_the_same_link_and_title_is_a_new_article() {
    let week = |g: &str, date: &str, text: &str| {
        format!(
            r#"<?xml version="1.0"?><rss version="2.0"><channel><title>S</title>
            <item><guid isPermaLink="false">{g}</guid><title>Weekly update</title>
              <link>https://s.test/updates</link><pubDate>{date}</pubDate><description>{text}</description></item>
            </channel></rss>"#
        )
    };
    let (mut db, id) = db_with_feed("https://s.test/rss");
    let rules = IngestRules::load(&db, id).unwrap();
    let one = parse_feed(week("wk-1", "Tue, 09 Sep 2026 10:00:00 GMT", "week one").as_bytes(), "https://s.test/rss").unwrap();
    ingest(&mut db, id, &one, &rules).unwrap();
    db.conn().execute("UPDATE news SET read = 1", []).unwrap();
    let two = parse_feed(week("wk-2", "Tue, 16 Sep 2026 10:00:00 GMT", "week two").as_bytes(), "https://s.test/rss").unwrap();
    let r = ingest(&mut db, id, &two, &rules).unwrap();
    assert_eq!(r.inserted, 1, "week two is its own article");
    let old: String = db
        .conn()
        .query_row("SELECT description FROM news WHERE guid = 'wk-1'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(old, "week one", "and week one is left as it was");
}

#[test]
fn one_feed_failing_to_apply_does_not_lose_the_others() {
    let mut db = Db::open_in_memory().unwrap();
    for u in ["https://a.test/f", "https://b.test/f", "https://c.test/f"] {
        db.conn()
            .execute("INSERT INTO feeds(kind, text, xml_url) VALUES(1, 'f', ?1)", [u])
            .unwrap();
    }
    // A filter that adds a label that no longer exists, matching everything.
    db.conn()
        .execute_batch(
            "INSERT INTO filters(id, name, type, feeds, enable, num) VALUES(1, 'x', 0, NULL, 1, 0);
             INSERT INTO filter_actions(filter_id, action, params) VALUES(1, 'add_label', '99');",
        )
        .unwrap();
    let fetched = vec![
        Fetched { feed: due(1, "https://a.test/f"), result: body(ATOM) },
        // Removed while its fetch was in flight.
        Fetched { feed: due(77, "https://gone.test/f"), result: body(ATOM) },
        Fetched { feed: due(3, "https://c.test/f"), result: body(ATOM_V2) },
    ];
    let s = apply_fetched(&mut db, fetched).unwrap();
    assert_eq!(s.new_articles, 5, "{s:?}");
    assert_eq!(rows(&db), 5);
}

#[test]
fn a_feed_added_by_url_takes_its_own_title_but_a_chosen_name_stays() {
    let mut db = Db::open_in_memory().unwrap();
    db.conn()
        .execute_batch(
            "INSERT INTO feeds(id, kind, text, xml_url) VALUES(1, 1, 'kn.test', 'https://kn.test/atom');
             INSERT INTO feeds(id, kind, text, xml_url) VALUES(2, 1, 'My name', 'https://kn2.test/atom');",
        )
        .unwrap();
    apply_fetched(
        &mut db,
        vec![
            Fetched { feed: due(1, "https://kn.test/atom"), result: body(ATOM) },
            Fetched { feed: due(2, "https://kn2.test/atom"), result: body(ATOM) },
        ],
    )
    .unwrap();
    let text = |id: i64| -> String {
        db.conn().query_row("SELECT text FROM feeds WHERE id = ?1", [id], |r| r.get(0)).unwrap()
    };
    assert_eq!(text(1), "Kernel Notes");
    assert_eq!(text(2), "My name");
}

#[test]
fn a_date_only_cutoff_is_honoured() {
    let mut db = Db::open_in_memory().unwrap();
    db.conn()
        .execute(
            "INSERT INTO feeds(kind, text, xml_url, add_any_date, avoid_old_enable, avoid_old_before)
             VALUES(1, 'cutoff', 'https://x.test/f.xml', 0, 1, '2026-09-16')",
            [],
        )
        .unwrap();
    let id = db.conn().last_insert_rowid();
    let parsed = parse_feed(ATOM.as_bytes(), "https://x.test/f.xml").unwrap();
    let rules = IngestRules::load(&db, id).unwrap();
    let r = ingest(&mut db, id, &parsed, &rules).unwrap();
    assert_eq!(r.too_old, 1);
}

#[test]
fn interval_codes_backoff_and_absurd_values() {
    assert_eq!(schedule::interval_to_duration(2, Some("1")), Duration::hours(2), "QuiteRSS hours");
    assert_eq!(schedule::interval_to_duration(30, Some("0")), Duration::minutes(30));
    assert_eq!(schedule::interval_to_duration(90, Some("-1")), Duration::seconds(90));
    assert_eq!(schedule::interval_to_duration(5, Some("-1")), Duration::minutes(1), "never under a minute");
    // Backing off never polls more often than the feed's own interval.
    assert_eq!(schedule::backoff(Duration::days(7), 1), Duration::days(7));
    // Large numbers are clamped instead of panicking.
    let _ = schedule::interval_to_duration(i64::MAX / 2, Some("days"));
    let _ = schedule::backoff(Duration::days(365), 6);
}

#[test]
fn a_purged_article_does_not_come_back_as_new() {
    let (mut db, id) = db_with_feed("https://kn.test/atom");
    let rules = IngestRules::load(&db, id).unwrap();
    let parsed = parse_feed(ATOM.as_bytes(), "https://kn.test/atom").unwrap();
    ingest(&mut db, id, &parsed, &rules).unwrap();
    db.conn()
        .execute("UPDATE news SET read = 1, deleted = 1, delete_date = '2020-01-01T00:00:00Z'", [])
        .unwrap();
    snaprss_core::cleanup::purge_deleted(&mut db, None).unwrap();

    let r = ingest(&mut db, id, &parsed, &rules).unwrap();
    assert_eq!(r.inserted, 0, "still in the feed, already seen");
    let unread: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM news WHERE read = 0 AND deleted = 0", [], |r| r.get(0))
        .unwrap();
    assert_eq!(unread, 0);
}

#[test]
fn a_feed_whose_write_fails_is_marked_failed_and_the_rest_are_written() {
    let mut db = Db::open_in_memory().unwrap();
    for u in ["https://a.test/f", "https://b.test/f"] {
        db.conn()
            .execute("INSERT INTO feeds(kind, text, xml_url) VALUES(1, 'f', ?1)", [u])
            .unwrap();
    }
    // Any failure while writing feed 1.
    db.conn()
        .execute_batch(
            "CREATE TRIGGER boom BEFORE INSERT ON news WHEN NEW.feed_id = 1
             BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
        .unwrap();
    let s = apply_fetched(
        &mut db,
        vec![
            Fetched { feed: due(1, "https://a.test/f"), result: body(ATOM) },
            Fetched { feed: due(2, "https://b.test/f"), result: body(ATOM) },
        ],
    )
    .unwrap();
    assert_eq!((s.failed, s.new_articles), (1, 2), "{s:?}");
    let status: String = db
        .conn()
        .query_row("SELECT status FROM feeds WHERE id = 1", [], |r| r.get(0))
        .unwrap();
    assert!(status.contains("boom"), "{status}");
}

// ---------------------------------------------------------------------------
// second review
// ---------------------------------------------------------------------------

use snaprss_fetch::parse_feed_with;

#[tokio::test]
async fn a_small_gzip_that_unpacks_huge_is_refused_while_it_arrives() {
    use std::io::Write;
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(&vec![b' '; 8 * 1024 * 1024]).unwrap();
    let packed = gz.finish().unwrap();
    assert!(packed.len() < 100_000);

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Encoding", "gzip")
                .set_body_bytes(packed),
        )
        .mount(&server)
        .await;
    let cfg = FetchConfig {
        max_body_bytes: 1024 * 1024,
        ..FetchConfig::default()
    };
    let client = build_client(&cfg).unwrap();
    let r = fetch(&client, &format!("{}/f", server.uri()), &Validators::default(), &cfg).await;
    assert!(matches!(r, Err(snaprss_fetch::FetchError::TooLarge(_))), "{r:?}");
}

#[test]
fn a_charset_given_only_in_the_http_header_is_honoured() {
    let xml = "<rss version=\"2.0\"><channel><title>机核网</title><item><guid>1</guid><title>中文标题</title><link>https://g.test/1</link></item></channel></rss>";
    let (gbk, _, _) = encoding_rs::GBK.encode(xml);
    let parsed = parse_feed_with(&gbk, "https://g.test/rss", Some("application/rss+xml; charset=gbk")).unwrap();
    assert_eq!(parsed.entries[0].title.as_ref().unwrap().content, "中文标题");
    // Declared in the document: left to the parser, as before.
    let declared = format!("<?xml version=\"1.0\" encoding=\"gbk\"?>{xml}");
    let (gbk2, _, _) = encoding_rs::GBK.encode(&declared);
    let parsed = parse_feed_with(&gbk2, "https://g.test/rss", Some("text/xml; charset=utf-8")).unwrap();
    assert_eq!(parsed.entries[0].title.as_ref().unwrap().content, "中文标题");
}

#[test]
fn a_guid_repeated_inside_one_feed_keeps_both_items_and_settles() {
    let xml = r#"<rss version="2.0"><channel><title>x</title>
        <item><guid>https://x.test/</guid><title>Post A</title><link>https://x.test/a</link><description>a</description></item>
        <item><guid>https://x.test/</guid><title>Post B</title><link>https://x.test/b</link><description>b</description></item>
    </channel></rss>"#;
    let (mut db, id) = db_with_feed("https://x.test/rss");
    let rules = IngestRules::load(&db, id).unwrap();
    let parsed = parse_feed(xml.as_bytes(), "https://x.test/rss").unwrap();
    ingest(&mut db, id, &parsed, &rules).unwrap();
    assert_eq!(rows(&db), 2);
    let again = ingest(&mut db, id, &parsed, &rules).unwrap();
    assert_eq!((again.inserted, again.updated), (0, 0), "{again:?}");
}

#[test]
fn the_audio_enclosure_wins_over_a_thumbnail() {
    let xml = r#"<rss version="2.0" xmlns:media="http://search.yahoo.com/mrss/"><channel><title>p</title>
        <item><guid>ep1</guid><title>Episode 1</title>
          <media:content url="https://p.test/thumb.jpg" medium="image" type="image/jpeg"/>
          <enclosure url="https://p.test/ep1.mp3" type="audio/mpeg" length="1000"/>
        </item></channel></rss>"#;
    let (mut db, id) = db_with_feed("https://p.test/rss");
    let rules = IngestRules::load(&db, id).unwrap();
    ingest(&mut db, id, &parse_feed(xml.as_bytes(), "https://p.test/rss").unwrap(), &rules).unwrap();
    let url: String = db.conn().query_row("SELECT enclosure_url FROM news", [], |r| r.get(0)).unwrap();
    assert_eq!(url, "https://p.test/ep1.mp3");
}

#[test]
fn chinese_feed_dates() {
    use snaprss_fetch::dates::parse;
    let at = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
    // CST in a Chinese feed is UTC+8, not US Central.
    assert_eq!(parse("Thu, 24 Sep 2026 09:00:00 CST", true), Some(at("2026-09-24T01:00:00Z")));
    assert_eq!(parse("Thu, 24 Sep 2026 09:00:00 CST", false), Some(at("2026-09-24T15:00:00Z")));
    assert_eq!(parse("2024-01-15 10:30:00", true), Some(at("2024-01-15T02:30:00Z")));
    assert_eq!(parse("2024/01/15 10:30:00", false), Some(at("2024-01-15T10:30:00Z")));
    assert_eq!(parse("Mon, 15 Jan 2024 10:30:00 GMT+8", false), Some(at("2024-01-15T02:30:00Z")));
    assert_eq!(parse("2024-01-15", false), Some(at("2024-01-15T00:00:00Z")));
    // Still reads what feed-rs read.
    assert_eq!(parse("Tue, 16 Sep 2026 10:00:00 GMT", false), Some(at("2026-09-16T10:00:00Z")));
    assert_eq!(parse("2026-09-16T10:00:00+0200", false), Some(at("2026-09-16T08:00:00Z")));
    assert_eq!(parse("Thurs, 13 Jul 2011 07:38:00 GMT", false), Some(at("2011-07-13T07:38:00Z")));
}

#[test]
fn a_date_in_the_future_is_taken_as_now() {
    let xml = r#"<rss version="2.0"><channel><title>x</title>
        <item><guid>f</guid><title>From the future</title><pubDate>Fri, 01 Jan 2100 00:00:00 GMT</pubDate></item>
    </channel></rss>"#;
    let (mut db, id) = db_with_feed("https://x.test/rss");
    let rules = IngestRules::load(&db, id).unwrap();
    ingest(&mut db, id, &parse_feed(xml.as_bytes(), "https://x.test/rss").unwrap(), &rules).unwrap();
    let p: String = db.conn().query_row("SELECT published FROM news", [], |r| r.get(0)).unwrap();
    assert!(p.as_str() < "2030", "{p}");
}

#[test]
fn titles_and_text_bodies_come_out_as_written() {
    let atom = r#"<feed xmlns="http://www.w3.org/2005/Atom"><title>t</title><id>urn:t</id>
      <entry><id>urn:1</id><title type="html">A &lt;b&gt;bold&lt;/b&gt; move</title>
        <content type="text">Wrap it in a &lt;section&gt; element, not &lt;div&gt;.</content></entry>
    </feed>"#;
    let (mut db, id) = db_with_feed("https://t.test/atom");
    let rules = IngestRules::load(&db, id).unwrap();
    ingest(&mut db, id, &parse_feed(atom.as_bytes(), "https://t.test/atom").unwrap(), &rules).unwrap();
    let (title, content): (String, String) = db
        .conn()
        .query_row("SELECT title, content FROM news", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(title, "A bold move");
    assert!(content.contains("&lt;section&gt;") && !content.contains("<div>"), "{content}");

    let rss = r#"<rss version="2.0"><channel><title>r</title>
        <item><guid>1</guid><title>AT&amp;amp;T &amp;nbsp;news</title></item></channel></rss>"#;
    let (mut db, id) = db_with_feed("https://r.test/rss");
    let rules = IngestRules::load(&db, id).unwrap();
    ingest(&mut db, id, &parse_feed(rss.as_bytes(), "https://r.test/rss").unwrap(), &rules).unwrap();
    let title: String = db.conn().query_row("SELECT title FROM news", [], |r| r.get(0)).unwrap();
    assert_eq!(title, "AT&T news");
}

#[test]
fn relative_links_resolve_against_where_the_feed_came_from() {
    let xml = r#"<rss version="2.0"><channel><title>x</title>
        <item><guid>1</guid><title>One</title><link>/posts/1</link></item></channel></rss>"#;
    let mut db = Db::open_in_memory().unwrap();
    db.conn()
        .execute("INSERT INTO feeds(id, kind, text, xml_url) VALUES(1, 1, 'f', 'http://old.test/feed')", [])
        .unwrap();
    apply_fetched(
        &mut db,
        vec![Fetched {
            feed: due(1, "http://old.test/feed"),
            result: Ok(FetchOutcome::Body {
                bytes: xml.as_bytes().to_vec(),
                validators: Validators::default(),
                final_url: Some("https://new.test/feed".into()),
                content_type: None,
            }),
        }],
    )
    .unwrap();
    let link: String = db.conn().query_row("SELECT link_href FROM news", [], |r| r.get(0)).unwrap();
    assert_eq!(link, "https://new.test/posts/1");
}
