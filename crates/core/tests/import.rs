//! Builds QuiteRSS-shaped databases and imports them.
//!
//! Two shapes are exercised: a current-era database with every optional column,
//! and a legacy one missing the columns QuiteRSS added in later ALTER TABLE
//! migrations. Both must import.

use rusqlite::{params, Connection};
use snaprss_core::import::{import_quiterss, looks_like_quiterss};
use snaprss_core::models::NodeKind;
use snaprss_core::Db;

/// The subset of the QuiteRSS feeds table the importer reads, in a modern file.
const FEEDS_MODERN: &str = "
CREATE TABLE feeds(
  id integer primary key, text varchar, title varchar, description varchar,
  xmlUrl varchar, htmlUrl varchar, language varchar, image blob,
  unread integer, newCount integer, currentNews integer, label varchar,
  undeleteCount integer, hasChildren integer default 0, parentId integer default 0,
  rowToParent integer, updateIntervalEnable int, updateInterval int,
  updateIntervalType varchar, updateOnStartup int, displayOnStartup int,
  markReadAfterSecondsEnable int, markReadAfterSeconds int,
  markDisplayedOnSwitchingFeed int, layout text, filter text, groupBy int,
  displayNews int, displayEmbeddedImages integer default 1, loadTypes text,
  columns text, sort text, sortType int, maximumToKeep int,
  maximumToKeepEnable int, maximumAgeOfNews int, maximumAgoOfNewEnable int,
  deleteReadNews int, neverDeleteUnreadNews int, neverDeleteStarredNews int,
  neverDeleteLabeledNews int, status text, created text, updated text,
  lastDisplayed text, f_Expanded integer default 1, authentication integer default 0,
  duplicateNewsMode integer default 0, addSingleNewsAnyDateOn integer default 1,
  avoidedOldSingleNewsDateOn integer default 0, avoidedOldSingleNewsDate varchar,
  showNotification integer default 0, disableUpdate integer default 0,
  layoutDirection integer default 0)";

/// An older file: no duplicateNewsMode, no layoutDirection, no f_Expanded,
/// no showNotification, no disableUpdate, no avoided* columns.
const FEEDS_LEGACY: &str = "
CREATE TABLE feeds(
  id integer primary key, text varchar, title varchar, description varchar,
  xmlUrl varchar, htmlUrl varchar, language varchar, image blob,
  unread integer, newCount integer, currentNews integer, label varchar,
  undeleteCount integer, hasChildren integer default 0, parentId integer default 0,
  rowToParent integer, updateIntervalEnable int, updateInterval int,
  updateIntervalType varchar, updateOnStartup int, displayOnStartup int,
  markReadAfterSecondsEnable int, markReadAfterSeconds int,
  markDisplayedOnSwitchingFeed int, layout text, filter text, groupBy int,
  displayNews int, columns text, sort text, sortType int, maximumToKeep int,
  maximumToKeepEnable int, maximumAgeOfNews int, maximumAgoOfNewEnable int,
  deleteReadNews int, neverDeleteUnreadNews int, neverDeleteStarredNews int,
  neverDeleteLabeledNews int, status text, created text, updated text,
  lastDisplayed text, authentication integer default 0)";

const NEWS_MODERN: &str = "
CREATE TABLE news(
  id integer primary key, feedId integer, guid varchar,
  guidislink varchar default 'true', description varchar, content varchar,
  title varchar, published varchar, modified varchar, received varchar,
  author_name varchar, author_uri varchar, author_email varchar,
  category varchar, label varchar, new integer default 1, read integer default 0,
  starred integer default 0, deleted integer default 0, attachment varchar,
  comments varchar, enclosure_length, enclosure_type, enclosure_url,
  source varchar, link_href varchar, link_enclosure varchar,
  link_related varchar, link_alternate varchar, contributor varchar,
  rights varchar, deleteDate varchar, feedParentId integer default 0)";

const NEWS_LEGACY: &str = "
CREATE TABLE news(
  id integer primary key, feedId integer, guid varchar,
  guidislink varchar default 'true', description varchar,
  title varchar, published varchar, modified varchar, received varchar,
  author_name varchar, author_uri varchar, author_email varchar,
  category varchar, label varchar, new integer default 1, read integer default 0,
  starred integer default 0, deleted integer default 0,
  comments varchar, enclosure_length, enclosure_type, enclosure_url,
  source varchar, link_href varchar, link_alternate varchar, rights varchar)";

const AUX: &str = "
CREATE TABLE labels(id integer primary key, name varchar, image blob,
  color_text varchar, color_bg varchar, num integer, currentNews integer);
CREATE TABLE filters(id integer primary key, name varchar, type integer,
  feeds varchar, enable integer default 1, num integer);
CREATE TABLE filterConditions(id integer primary key, idFilter int,
  field varchar, condition varchar, content varchar);
CREATE TABLE filterActions(id integer primary key, idFilter int,
  action varchar, params varchar);
CREATE TABLE passwords(id integer primary key, server varchar,
  username varchar, password varchar);";

struct Fixture {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
}

fn build_modern() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("feeds.db");
    let c = Connection::open(&path).unwrap();
    c.execute_batch(FEEDS_MODERN).unwrap();
    c.execute_batch(NEWS_MODERN).unwrap();
    c.execute_batch(AUX).unwrap();

    // id 1 root folder, id 2 subfolder, feeds 10/11/12.
    // Folders are identified by an empty xmlUrl, as QuiteRSS does.
    let node = |id: i64,
                parent: i64,
                row: i64,
                text: &str,
                url: Option<&str>,
                display_news: i64,
                status: &str| {
        c.execute(
            "INSERT INTO feeds(id, parentId, rowToParent, text, title, xmlUrl, htmlUrl,
                               displayNews, loadTypes, f_Expanded, status, layoutDirection,
                               duplicateNewsMode, updateInterval, updateIntervalType)
             VALUES(?1, ?2, ?3, ?4, ?4, ?5, 'https://example.test', ?6, 'images', 1, ?7, 0, 1, 15, 'minutes')",
            params![id, parent, row, text, url.unwrap_or(""), display_news, status],
        )
        .unwrap();
    };
    node(1, 0, 0, "Engineering", None, 0, "");
    node(2, 1, 0, "Nested", None, 0, "");
    node(
        10,
        1,
        1,
        "Kernel Notes",
        Some("https://kn.test/feed.xml"),
        1,
        "",
    );
    node(
        11,
        2,
        0,
        "Signal Path",
        Some("https://sp.test/feed.xml"),
        0,
        "",
    );
    node(
        12,
        0,
        1,
        "Cold Storage",
        Some("https://cs.test/feed.xml"),
        1,
        "Network error",
    );
    // Orphan: parent 999 does not exist.
    node(
        13,
        999,
        0,
        "Orphan Feed",
        Some("https://orphan.test/feed.xml"),
        0,
        "",
    );

    c.execute_batch(
        "INSERT INTO labels(id, name, color_bg, num) VALUES(1, 'Important', '#B33A35', 0);
         INSERT INTO labels(id, name, color_bg, num) VALUES(4, 'Read later', '#3B6EA5', 1);",
    )
    .unwrap();

    let art =
        |id: i64, feed: i64, title: &str, read: i64, starred: i64, deleted: i64, label: &str| {
            c.execute(
                "INSERT INTO news(id, feedId, guid, guidislink, title, description, content,
                              published, received, author_name, link_href, label,
                              read, starred, deleted, new)
             VALUES(?1, ?2, ?3, 'true', ?4, 'summary', 'full text',
                    '2026-09-16T10:00:00', '2026-09-16T10:05:00', 'A. Author',
                    'https://example.test/a', ?5, ?6, ?7, ?8, 0)",
                params![
                    id,
                    feed,
                    format!("guid-{id}"),
                    title,
                    label,
                    read,
                    starred,
                    deleted
                ],
            )
            .unwrap();
        };
    art(100, 10, "Unread one", 0, 0, 0, ",1,");
    art(101, 10, "Read one", 1, 0, 0, "");
    art(102, 10, "Starred unread", 0, 1, 0, ",1,4,");
    art(103, 11, "Signal unread", 0, 0, 0, "");
    art(104, 11, "Deleted one", 0, 0, 1, "");
    art(105, 12, "Cold unread", 0, 0, 0, ",4,");
    // Article whose feed does not exist.
    art(106, 777, "Ghost", 0, 0, 0, "");

    c.execute_batch(
        // Exactly what QuiteRSS writes: `feeds` is comma-wrapped because it is
        // queried with `LIKE '%,<id>,%'`, and field, condition and action are
        // combo box indices, not names. type 1 is "match all conditions".
        // Condition index 1 means "doesn't contain" for Author, and action 1 is
        // "Add Star". See src/newsfilters/ and src/parseobject.cpp.
        "INSERT INTO filters(id, name, type, feeds, enable, num) VALUES(1, 'Star releases', 1, ',10,11,', 1, 0);
         INSERT INTO filterConditions(id, idFilter, field, condition, content)
           VALUES(1, 1, '0', '0', 'release');
         INSERT INTO filterConditions(id, idFilter, field, condition, content)
           VALUES(2, 1, '2', '1', 'bot');
         INSERT INTO filterActions(id, idFilter, action, params) VALUES(1, 1, '1', '');
         INSERT INTO filterActions(id, idFilter, action, params) VALUES(2, 1, '3', '1');
         INSERT INTO filterConditions(id, idFilter, field, condition, content)
           VALUES(3, 42, '0', '0', 'orphaned condition');
         INSERT INTO passwords(id, server, username, password)
           VALUES(1, 'kn.test', 'me', 'hunter2');",
    )
    .unwrap();

    Fixture { _dir: dir, path }
}

fn build_legacy() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("feeds.db");
    let c = Connection::open(&path).unwrap();
    c.execute_batch(FEEDS_LEGACY).unwrap();
    c.execute_batch(NEWS_LEGACY).unwrap();
    c.execute_batch(AUX).unwrap();
    c.execute(
        "INSERT INTO feeds(id, parentId, rowToParent, text, xmlUrl, displayNews)
         VALUES(1, 0, 0, 'Old Feed', 'https://old.test/feed.xml', 0)",
        [],
    )
    .unwrap();
    c.execute(
        "INSERT INTO news(id, feedId, guid, guidislink, title, description, published,
                          received, read, starred, deleted)
         VALUES(1, 1, 'g1', 'false', 'Old article', 'summary', '2019-01-01T00:00:00',
                '2019-01-01T00:01:00', 0, 1, 0)",
        [],
    )
    .unwrap();
    Fixture { _dir: dir, path }
}

#[test]
fn detects_a_quiterss_database() {
    let fx = build_modern();
    assert!(looks_like_quiterss(&fx.path));

    let dir = tempfile::tempdir().unwrap();
    let other = dir.path().join("something.db");
    Connection::open(&other)
        .unwrap()
        .execute_batch("CREATE TABLE t(a)")
        .unwrap();
    assert!(!looks_like_quiterss(&other));
    assert!(!looks_like_quiterss(dir.path().join("does-not-exist.db")));
}

#[test]
fn imports_a_modern_database() {
    let fx = build_modern();
    let mut db = Db::open_in_memory().unwrap();
    let report = import_quiterss(&mut db, &fx.path).unwrap();

    assert_eq!(report.folders, 2, "two folders");
    assert_eq!(report.feeds, 4, "four feeds including the orphan");
    assert_eq!(report.news, 6, "six articles; the ghost is skipped");
    assert_eq!(report.labels, 2);
    assert_eq!(report.label_links, 4, ",1, + ,1,4, + ,4,");
    assert_eq!(report.filters, 1);
    assert_eq!(report.conditions, 2, "the orphaned condition is skipped");
    assert_eq!(report.actions, 2);
    assert_eq!(report.passwords, 1);

    // Both skips are reported rather than silent.
    assert_eq!(report.skipped.len(), 3, "{:?}", report.skipped);
    assert!(report
        .skipped
        .iter()
        .any(|s| s.contains("unknown feed 777")));
    assert!(report.skipped.iter().any(|s| s.contains("moved to root")));
    assert!(report.skipped.iter().any(|s| s.contains("no filter")));
}

#[test]
fn rebuilds_the_tree() {
    let fx = build_modern();
    let mut db = Db::open_in_memory().unwrap();
    import_quiterss(&mut db, &fx.path).unwrap();

    let roots = db.children(None).unwrap();
    let names: Vec<_> = roots.iter().map(|n| n.text.clone().unwrap()).collect();
    assert_eq!(names, vec!["Engineering", "Cold Storage", "Orphan Feed"]);

    let engineering = &roots[0];
    assert_eq!(engineering.kind, NodeKind::Folder);
    assert!(engineering.xml_url.as_deref().unwrap_or("").is_empty());

    let kids = db.children(Some(engineering.id)).unwrap();
    let kid_names: Vec<_> = kids.iter().map(|n| n.text.clone().unwrap()).collect();
    assert_eq!(kid_names, vec!["Nested", "Kernel Notes"]);
    assert_eq!(kids[0].kind, NodeKind::Folder);
    assert_eq!(kids[1].kind, NodeKind::Feed);

    let nested = db.children(Some(kids[0].id)).unwrap();
    assert_eq!(nested.len(), 1);
    assert_eq!(nested[0].text.as_deref(), Some("Signal Path"));
}

#[test]
fn recomputes_unread_counts() {
    let fx = build_modern();
    let mut db = Db::open_in_memory().unwrap();
    import_quiterss(&mut db, &fx.path).unwrap();

    // Kernel Notes: 3 articles, 1 read -> 2 unread, 3 undeleted.
    // Signal Path: 2 articles, 1 deleted -> 1 unread, 1 undeleted.
    // Cold Storage: 1 unread.
    assert_eq!(db.total_unread().unwrap(), 4);

    let roots = db.children(None).unwrap();
    let cold = roots
        .iter()
        .find(|n| n.text.as_deref() == Some("Cold Storage"))
        .unwrap();
    assert_eq!(cold.unread, 1);
    assert!(cold.is_broken(), "status was set, so the tree flags it");

    let eng = roots
        .iter()
        .find(|n| n.text.as_deref() == Some("Engineering"))
        .unwrap();
    let kn = db
        .children(Some(eng.id))
        .unwrap()
        .into_iter()
        .find(|n| n.text.as_deref() == Some("Kernel Notes"))
        .unwrap();
    assert_eq!(kn.unread, 2);
    assert_eq!(kn.undelete_count, 3);
    assert!(!kn.is_broken());
}

#[test]
fn maps_reading_mode_and_labels() {
    let fx = build_modern();
    let mut db = Db::open_in_memory().unwrap();
    import_quiterss(&mut db, &fx.path).unwrap();

    // displayNews = 1 (fetch the link) becomes FullArticle = 0.
    // displayNews = 0 (use feed content) becomes DescriptionOnly = 1.
    let mode: i64 = db
        .conn()
        .query_row(
            "SELECT reading_mode FROM feeds WHERE text = 'Kernel Notes'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mode, 0);

    let mode: i64 = db
        .conn()
        .query_row(
            "SELECT reading_mode FROM feeds WHERE text = 'Signal Path'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(mode, 1);

    // ",1,4," became two rows, not a substring match.
    let starred_labels: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM news_labels
             JOIN news ON news.id = news_labels.news_id
             WHERE news.title = 'Starred unread'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(starred_labels, 2);
}

#[test]
fn remaps_filter_feed_references() {
    let fx = build_modern();
    let mut db = Db::open_in_memory().unwrap();
    import_quiterss(&mut db, &fx.path).unwrap();

    let feeds: String = db
        .conn()
        .query_row(
            "SELECT feeds FROM filters WHERE name = 'Star releases'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    // The old ids 10 and 11 must not survive verbatim; they now point at
    // whatever the import assigned.
    let ids: Vec<i64> = snaprss_core::filters::parse_feed_list(Some(&feeds))
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(ids.len(), 2);
    for id in ids {
        let name: String = db
            .conn()
            .query_row("SELECT text FROM feeds WHERE id = ?1", [id], |r| r.get(0))
            .unwrap();
        assert!(
            name == "Kernel Notes" || name == "Signal Path",
            "filter pointed at {name}"
        );
    }
}

/// The importer copies QuiteRSS's combo box indices verbatim, so the only
/// thing proving they were read back correctly is running the imported filter.
/// This is the end-to-end check that the index tables are right.
#[test]
fn an_imported_filter_actually_works() {
    use snaprss_core::filters::{Candidate, Engine, Field, Match, Op};

    let fx = build_modern();
    let mut db = Db::open_in_memory().unwrap();
    import_quiterss(&mut db, &fx.path).unwrap();

    let all = snaprss_core::filters::load_filters(db.conn(), true).unwrap();
    let f = all.iter().find(|f| f.name == "Star releases").unwrap();

    assert_eq!(f.mode, Match::All, "type 1 is 'match all conditions'");
    assert_eq!(f.conditions.len(), 2);
    assert_eq!(f.conditions[0].field, Field::Title);
    assert_eq!(f.conditions[0].op, Op::Contains);
    // Author's operator list has no begins/ends with, so index 1 is
    // "doesn't contain" — reading it from Title's list would give the same
    // answer here, but index 4 would not.
    assert_eq!(f.conditions[1].field, Field::Author);
    assert_eq!(f.conditions[1].op, Op::NotContains);

    let feed = *f.feeds.as_ref().unwrap().iter().next().unwrap();
    let engine = Engine::new(vec![f.clone()]);

    let hit = engine.evaluate(&Candidate {
        feed_id: feed,
        title: Some("Version 2.1 release notes"),
        author: Some("Marta Vogel"),
        ..Default::default()
    });
    assert!(hit.star, "the release should be starred");
    assert_eq!(hit.labels.len(), 1, "and carry the label the filter names");

    let miss = engine.evaluate(&Candidate {
        feed_id: feed,
        title: Some("Version 2.1 release notes"),
        author: Some("release-bot"),
        ..Default::default()
    });
    assert!(miss.matched.is_empty(), "the bot is excluded by condition two");
}

#[test]
fn imports_a_legacy_database_missing_columns() {
    let fx = build_legacy();
    let mut db = Db::open_in_memory().unwrap();
    let report = import_quiterss(&mut db, &fx.path).unwrap();

    assert_eq!(report.feeds, 1);
    assert_eq!(report.news, 1);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);

    // Columns absent from the source fall back to the schema default.
    let (rtl, dup, expanded): (i64, i64, i64) = db
        .conn()
        .query_row(
            "SELECT layout_direction, duplicate_news_mode, expanded FROM feeds",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((rtl, dup, expanded), (0, 0, 1));

    // guidislink 'false' is text, not an integer.
    let gil: i64 = db
        .conn()
        .query_row("SELECT guid_is_link FROM news", [], |r| r.get(0))
        .unwrap();
    assert_eq!(gil, 0);

    assert_eq!(db.total_unread().unwrap(), 1);
}

#[test]
fn import_is_atomic() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("not-quiterss.db");
    Connection::open(&bad)
        .unwrap()
        .execute_batch("CREATE TABLE unrelated(a)")
        .unwrap();

    let mut db = Db::open_in_memory().unwrap();
    db.conn()
        .execute(
            "INSERT INTO feeds(kind, text, xml_url) VALUES(1, 'Existing', 'https://x.test/f.xml')",
            [],
        )
        .unwrap();

    assert!(import_quiterss(&mut db, &bad).is_err());
    assert_eq!(db.count("feeds").unwrap(), 1, "existing rows untouched");
}

#[test]
fn imports_into_a_database_that_already_has_feeds() {
    let fx = build_modern();
    let mut db = Db::open_in_memory().unwrap();
    db.conn()
        .execute(
            "INSERT INTO feeds(kind, text, xml_url) VALUES(1, 'Pre-existing', 'https://pre.test/f.xml')",
            [],
        )
        .unwrap();

    import_quiterss(&mut db, &fx.path).unwrap();

    // 1 pre-existing + 2 folders + 4 feeds
    assert_eq!(db.count("feeds").unwrap(), 7);
    let roots = db.children(None).unwrap();
    assert!(roots
        .iter()
        .any(|n| n.text.as_deref() == Some("Pre-existing")));
    assert!(roots
        .iter()
        .any(|n| n.text.as_deref() == Some("Engineering")));
}

#[test]
fn news_list_orders_newest_first() {
    let fx = build_modern();
    let mut db = Db::open_in_memory().unwrap();
    import_quiterss(&mut db, &fx.path).unwrap();

    let items = db.news_for_feed(None, 100).unwrap();
    assert_eq!(items.len(), 5, "deleted rows are excluded");
    assert!(items.iter().all(|i| !i.deleted));
}

// ---------------------------------------------------------------------------
// found in review
// ---------------------------------------------------------------------------

fn scalar<T: rusqlite::types::FromSql>(db: &Db, sql: &str) -> T {
    db.conn().query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn importing_the_same_file_twice_adds_nothing_the_second_time() {
    let fx = build_modern();
    let mut db = Db::open_in_memory().unwrap();
    import_quiterss(&mut db, &fx.path).unwrap();
    let count = |db: &Db, t: &str| scalar::<i64>(db, &format!("SELECT COUNT(*) FROM {t}"));
    let before: Vec<i64> = ["feeds", "news", "labels", "filters", "filter_actions", "passwords", "news_labels"]
        .iter()
        .map(|t| count(&db, t))
        .collect();

    let r = import_quiterss(&mut db, &fx.path).unwrap();
    let after: Vec<i64> = ["feeds", "news", "labels", "filters", "filter_actions", "passwords", "news_labels"]
        .iter()
        .map(|t| count(&db, t))
        .collect();
    assert_eq!(before, after, "feeds, news, labels, filters, actions, passwords, label links");
    assert_eq!(r.duplicates, 4, "every feed reported as already subscribed");
}

#[test]
fn quiterss_encodings_are_converted_on_import() {
    let fx = build_modern();
    {
        let c = Connection::open(&fx.path).unwrap();
        c.execute_batch(
            // Text where a number is expected: QuiteRSS writes enclosure
            // lengths as strings, which failed the whole import.
            "UPDATE news SET enclosure_length = '12345', enclosure_url = 'https://x.test/a.mp3' WHERE id = 100;
             -- 2 is QuiteRSS's ordinary 'read'.
             UPDATE news SET read = 2 WHERE id = 101;
             -- Interval unit codes: 1 = hours; enable -1 = use the global interval.
             UPDATE feeds SET updateIntervalType = '1', updateInterval = 2, updateIntervalEnable = 1 WHERE id = 10;
             UPDATE feeds SET updateIntervalType = '-1', updateIntervalEnable = -1 WHERE id = 11;
             -- 'Add label' naming QuiteRSS label 4, which is renumbered on import.
             UPDATE filterActions SET params = '4' WHERE id = 2;",
        )
        .unwrap();
    }
    let mut db = Db::open_in_memory().unwrap();
    import_quiterss(&mut db, &fx.path).unwrap();

    assert_eq!(scalar::<i64>(&db, "SELECT enclosure_length FROM news WHERE title = 'Unread one'"), 12345);
    assert_eq!(scalar::<i64>(&db, "SELECT read FROM news WHERE title = 'Read one'"), 1);
    assert_eq!(
        scalar::<String>(&db, "SELECT published FROM news WHERE title = 'Read one'"),
        "2026-09-16T10:00:00Z",
        "dates carry a zone, as ingestion writes them"
    );
    assert_eq!(
        scalar::<String>(&db, "SELECT update_interval_type FROM feeds WHERE text = 'Kernel Notes'"),
        "hours"
    );
    assert_eq!(
        scalar::<i64>(&db, "SELECT update_interval_enable FROM feeds WHERE text = 'Signal Path'"),
        0
    );
    let read_later: i64 = scalar(&db, "SELECT id FROM labels WHERE name = 'Read later'");
    assert_eq!(
        scalar::<String>(&db, "SELECT params FROM filter_actions WHERE action = '3'"),
        read_later.to_string(),
        "the label action follows the label to its new id"
    );
}

#[test]
fn a_snaprss_database_is_not_taken_for_a_quiterss_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snaprss.db");
    drop(Db::open(&path).unwrap());
    assert!(!looks_like_quiterss(&path));
    let mut db = Db::open_in_memory().unwrap();
    let err = snaprss_core::import::import_any(&mut db, &path).unwrap_err();
    assert!(err.to_string().contains("SnapRSS database"), "{err}");
}

#[test]
fn databases_imported_by_earlier_versions_are_repaired_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    {
        let db = Db::open(&path).unwrap();
        db.conn()
            .execute_batch(
                "INSERT INTO feeds(id, kind, text, xml_url, update_interval_enable, update_interval_type)
                   VALUES(1, 1, 'f', 'https://f.test/', -1, '1');
                 INSERT INTO news(feed_id, title, read, published, received)
                   VALUES(1, 't', 2, '2026-01-02T03:04:05', '2026-01-02T03:04:05');
                 UPDATE settings SET value = '1' WHERE key = 'schema_version';",
            )
            .unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(scalar::<i64>(&db, "SELECT read FROM news"), 1);
    assert_eq!(scalar::<String>(&db, "SELECT published FROM news"), "2026-01-02T03:04:05Z");
    assert_eq!(scalar::<String>(&db, "SELECT update_interval_type FROM feeds"), "hours");
    assert_eq!(scalar::<i64>(&db, "SELECT update_interval_enable FROM feeds"), 0);
}

#[test]
fn garbled_cached_articles_are_dropped_to_be_fetched_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    {
        let db = Db::open(&path).unwrap();
        db.conn()
            .execute_batch(
                "INSERT INTO feeds(id, kind, text, xml_url) VALUES(1, 1, 'f', 'https://f.test/');
                 INSERT INTO news(feed_id, title, received, article_html, article_fetched)
                   VALUES(1, 'bad', '2026-01-01T00:00:00Z', '<p>\u{fffd}\u{fffd}\u{fffd}\u{fffd}</p>', '2026-01-01T00:00:00Z');
                 INSERT INTO news(feed_id, title, received, article_html, article_fetched)
                   VALUES(1, 'good', '2026-01-01T00:00:00Z', '<p>中文</p>', '2026-01-01T00:00:00Z');
                 UPDATE settings SET value = '2' WHERE key = 'schema_version';",
            )
            .unwrap();
    }
    let db = Db::open(&path).unwrap();
    assert_eq!(scalar::<Option<String>>(&db, "SELECT article_html FROM news WHERE title = 'bad'"), None);
    assert!(scalar::<Option<String>>(&db, "SELECT article_html FROM news WHERE title = 'good'").is_some());
}
