//! Retention is the one feature whose bugs destroy data silently, so the
//! protections get tested harder than the rules do.

use chrono::{Duration, Utc};
use snaprss_core::cleanup::{self, Retention};
use snaprss_core::Db;

fn ts(days_ago: i64) -> String {
    (Utc::now() - Duration::days(days_ago)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A feed with `n` articles, newest first, all read and unstarred unless the
/// test says otherwise.
fn db_with(n: i64) -> (Db, i64) {
    let db = Db::open_in_memory().unwrap();
    db.conn()
        .execute(
            "INSERT INTO feeds(kind, text, xml_url) VALUES(1, 'Feed', 'https://f.test/x.xml')",
            [],
        )
        .unwrap();
    let feed = db.conn().last_insert_rowid();
    for i in 0..n {
        db.conn()
            .execute(
                "INSERT INTO news(feed_id, guid, title, published, received, read, starred, deleted)
                 VALUES(?1, ?2, ?3, ?4, ?4, 1, 0, 0)",
                rusqlite::params![feed, format!("g{i}"), format!("Article {i}"), ts(i)],
            )
            .unwrap();
    }
    db.recompute_counters().unwrap();
    (db, feed)
}

fn set(db: &Db, feed: i64, sql: &str) {
    db.conn()
        .execute(&format!("UPDATE feeds SET {sql} WHERE id = {feed}"), [])
        .unwrap();
}

fn live(db: &Db) -> i64 {
    db.conn()
        .query_row("SELECT COUNT(*) FROM news WHERE deleted = 0", [], |r| r.get(0))
        .unwrap()
}

fn titles_live(db: &Db) -> Vec<String> {
    let mut stmt = db
        .conn()
        .prepare("SELECT title FROM news WHERE deleted = 0 ORDER BY published DESC")
        .unwrap();
    let v = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    v
}

#[test]
fn a_feed_with_no_rules_is_left_alone() {
    let (mut db, _) = db_with(50);
    let r = cleanup::run(&mut db, None).unwrap();
    assert_eq!(r.feeds, 1);
    assert_eq!(r.total_deleted(), 0);
    assert_eq!(live(&db), 50);
}

#[test]
fn keeps_only_the_newest_n() {
    let (mut db, feed) = db_with(50);
    set(&db, feed, "max_to_keep = 10, max_to_keep_enable = 1");

    let r = cleanup::run(&mut db, None).unwrap();
    assert_eq!(r.by_count, 40);
    assert_eq!(live(&db), 10);

    // The ten kept are the ten newest, in order.
    let kept = titles_live(&db);
    assert_eq!(kept[0], "Article 0", "newest survived");
    assert_eq!(kept[9], "Article 9");
}

#[test]
fn deletes_past_an_age_limit() {
    let (mut db, feed) = db_with(40);
    set(&db, feed, "max_age_days = 14, max_age_enable = 1");

    cleanup::run(&mut db, None).unwrap();
    let kept = titles_live(&db);

    // An article aged exactly the limit is a sub-second race between the
    // fixture's timestamp and the cutoff, so assert either side of it rather
    // than on it.
    assert!(kept.contains(&"Article 13".to_string()), "13 days old was deleted");
    assert!(!kept.contains(&"Article 15".to_string()), "15 days old survived");
    assert!(!kept.contains(&"Article 39".to_string()));
    assert!(kept.len() == 14 || kept.len() == 15, "kept {}", kept.len());
}

#[test]
fn unread_articles_are_protected_by_default() {
    let (mut db, feed) = db_with(30);
    db.conn()
        .execute("UPDATE news SET read = 0 WHERE title IN ('Article 25','Article 26')", [])
        .unwrap();
    set(&db, feed, "max_to_keep = 5, max_to_keep_enable = 1");

    cleanup::run(&mut db, None).unwrap();
    let kept = titles_live(&db);
    assert!(kept.contains(&"Article 25".to_string()), "unread was deleted: {kept:?}");
    assert!(kept.contains(&"Article 26".to_string()));
    // 5 newest plus the 2 protected unread ones.
    assert_eq!(kept.len(), 7);
}

#[test]
fn starred_articles_are_protected_by_default() {
    let (mut db, feed) = db_with(30);
    db.conn()
        .execute("UPDATE news SET starred = 1 WHERE title = 'Article 29'", [])
        .unwrap();
    set(&db, feed, "max_age_days = 3, max_age_enable = 1");

    cleanup::run(&mut db, None).unwrap();
    assert!(
        titles_live(&db).contains(&"Article 29".to_string()),
        "a starred article 29 days old was deleted"
    );
}

#[test]
fn labelled_articles_are_protected_by_default() {
    let (mut db, feed) = db_with(20);
    db.conn()
        .execute_batch("INSERT INTO labels(name, num) VALUES('Keep', 0);")
        .unwrap();
    db.conn()
        .execute(
            "INSERT INTO news_labels(news_id, label_id)
             SELECT id, 1 FROM news WHERE title = 'Article 18'",
            [],
        )
        .unwrap();
    set(&db, feed, "max_to_keep = 2, max_to_keep_enable = 1");

    cleanup::run(&mut db, None).unwrap();
    assert!(
        titles_live(&db).contains(&"Article 18".to_string()),
        "a labelled article was deleted"
    );
}

#[test]
fn protections_can_be_turned_off() {
    let (mut db, feed) = db_with(20);
    db.conn()
        .execute("UPDATE news SET read = 0, starred = 1", [])
        .unwrap();
    set(
        &db,
        feed,
        "max_to_keep = 3, max_to_keep_enable = 1,
         never_delete_unread = 0, never_delete_starred = 0, never_delete_labeled = 0",
    );

    cleanup::run(&mut db, None).unwrap();
    assert_eq!(live(&db), 3, "explicitly unprotected articles should go");
}

#[test]
fn delete_read_leaves_unread_behind() {
    let (mut db, feed) = db_with(10);
    db.conn()
        .execute("UPDATE news SET read = 0 WHERE title IN ('Article 3','Article 7')", [])
        .unwrap();
    set(&db, feed, "delete_read = 1");

    let r = cleanup::run(&mut db, None).unwrap();
    assert_eq!(r.by_read, 8);
    assert_eq!(titles_live(&db).len(), 2);
}

#[test]
fn keep_count_measures_the_whole_feed_not_just_deletable_rows() {
    // "Keep 5" has to mean the 5 newest articles. If the count only ranked
    // rows the rules were allowed to touch, a feed with many starred articles
    // would quietly retain far more than asked.
    let (mut db, feed) = db_with(20);
    db.conn()
        .execute("UPDATE news SET starred = 1 WHERE title IN ('Article 10','Article 11')", [])
        .unwrap();
    set(&db, feed, "max_to_keep = 5, max_to_keep_enable = 1");

    cleanup::run(&mut db, None).unwrap();
    let kept = titles_live(&db);
    // The 5 newest, plus the 2 starred that were protected.
    assert_eq!(kept.len(), 7, "{kept:?}");
    assert!(kept.contains(&"Article 0".to_string()));
    assert!(kept.contains(&"Article 4".to_string()));
    assert!(!kept.contains(&"Article 5".to_string()));
    assert!(kept.contains(&"Article 10".to_string()));
}

#[test]
fn a_feed_can_be_cleaned_on_its_own() {
    let db0 = Db::open_in_memory().unwrap();
    drop(db0);
    let (mut db, feed_a) = db_with(20);
    db.conn()
        .execute(
            "INSERT INTO feeds(kind, text, xml_url) VALUES(1, 'Other', 'https://o.test/x.xml')",
            [],
        )
        .unwrap();
    let feed_b = db.conn().last_insert_rowid();
    for i in 0..20 {
        db.conn()
            .execute(
                "INSERT INTO news(feed_id, guid, title, published, received, read, deleted)
                 VALUES(?1, ?2, ?3, ?4, ?4, 1, 0)",
                rusqlite::params![feed_b, format!("b{i}"), format!("B {i}"), ts(i)],
            )
            .unwrap();
    }
    db.conn()
        .execute(
            "UPDATE feeds SET max_to_keep = 3, max_to_keep_enable = 1 WHERE id IN (?1, ?2)",
            rusqlite::params![feed_a, feed_b],
        )
        .unwrap();

    let r = cleanup::run(&mut db, Some(feed_a)).unwrap();
    assert_eq!(r.feeds, 1, "only the named feed was examined");
    assert_eq!(
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM news WHERE feed_id = ?1 AND deleted = 0",
                [feed_b],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        20,
        "the other feed was untouched"
    );
}

#[test]
fn global_defaults_apply_to_feeds_with_no_rules_of_their_own() {
    let (mut db, _) = db_with(30);
    db.conn()
        .execute(
            "INSERT INTO settings(key, value) VALUES('cleanup.max_to_keep', '4')",
            [],
        )
        .unwrap();

    let g = cleanup::global_retention(&db).unwrap();
    assert_eq!(g.max_to_keep, Some(4));

    cleanup::run(&mut db, None).unwrap();
    assert_eq!(live(&db), 4);
}

#[test]
fn a_feeds_own_limit_beats_the_global_one() {
    let (mut db, feed) = db_with(30);
    db.conn()
        .execute(
            "INSERT INTO settings(key, value) VALUES('cleanup.max_to_keep', '4')",
            [],
        )
        .unwrap();
    set(&db, feed, "max_to_keep = 12, max_to_keep_enable = 1");

    assert_eq!(cleanup::retention_for(&db, feed).unwrap().max_to_keep, Some(12));
    cleanup::run(&mut db, None).unwrap();
    assert_eq!(live(&db), 12);
}

#[test]
fn cleanup_is_reversible_until_purged() {
    let (mut db, feed) = db_with(20);
    set(&db, feed, "max_to_keep = 5, max_to_keep_enable = 1");
    cleanup::run(&mut db, None).unwrap();

    // Still there, just flagged.
    assert_eq!(
        db.conn()
            .query_row("SELECT COUNT(*) FROM news", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        20
    );
    assert_eq!(
        db.conn()
            .query_row("SELECT COUNT(*) FROM news WHERE deleted = 1", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        15
    );

    // And restorable.
    db.conn()
        .execute("UPDATE news SET deleted = 0, delete_date = NULL", [])
        .unwrap();
    db.recompute_counters().unwrap();
    assert_eq!(live(&db), 20);
}

/// Purged articles leave every view, Deleted included, and keep nothing but
/// what identifies them: a row removed outright is an article ingestion has
/// never seen, and it came back new and unread on the next poll.
#[test]
fn purge_empties_the_trash_and_keeps_only_identity() {
    let (mut db, feed) = db_with(20);
    set(&db, feed, "max_to_keep = 5, max_to_keep_enable = 1");
    cleanup::run(&mut db, None).unwrap();

    let r = cleanup::purge_deleted(&mut db, None).unwrap();
    assert_eq!(r.purged, 15);
    let count = |sql: &str| db.conn().query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
    assert_eq!(count("SELECT COUNT(*) FROM news WHERE deleted = 0"), 5);
    assert_eq!(count("SELECT COUNT(*) FROM news WHERE deleted = 1"), 0, "trash is empty");
    assert_eq!(
        count("SELECT COUNT(*) FROM news WHERE deleted = 2 AND description IS NULL AND content IS NULL
                 AND title IS NOT NULL"),
        15,
        "stubs keep identity, drop the body"
    );
}

#[test]
fn purge_can_spare_recently_deleted_articles() {
    let (mut db, _) = db_with(6);
    // Two deleted long ago, two deleted just now.
    db.conn()
        .execute(
            "UPDATE news SET deleted = 1, delete_date = ?1 WHERE title IN ('Article 0','Article 1')",
            [ts(30)],
        )
        .unwrap();
    db.conn()
        .execute(
            "UPDATE news SET deleted = 1, delete_date = ?1 WHERE title IN ('Article 2','Article 3')",
            [ts(0)],
        )
        .unwrap();

    let r = cleanup::purge_deleted(&mut db, Some(7)).unwrap();
    assert_eq!(r.purged, 2, "only the ones deleted over a week ago");
    assert_eq!(
        db.conn()
            .query_row("SELECT COUNT(*) FROM news WHERE deleted < 2", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        4
    );
}

#[test]
fn counters_are_rebuilt_after_a_run() {
    let (mut db, feed) = db_with(20);
    db.conn().execute("UPDATE news SET read = 0", []).unwrap();
    db.recompute_counters().unwrap();
    assert_eq!(db.total_unread().unwrap(), 20);

    set(
        &db,
        feed,
        "max_to_keep = 5, max_to_keep_enable = 1, never_delete_unread = 0",
    );
    cleanup::run(&mut db, None).unwrap();

    assert_eq!(db.total_unread().unwrap(), 5, "tree counts follow the deletion");
    let feed_unread: i64 = db
        .conn()
        .query_row("SELECT unread FROM feeds WHERE id = ?1", [feed], |r| r.get(0))
        .unwrap();
    assert_eq!(feed_unread, 5);
}

#[test]
fn rules_compose_without_double_counting() {
    let (mut db, feed) = db_with(40);
    set(
        &db,
        feed,
        "delete_read = 1, max_age_days = 10, max_age_enable = 1,
         max_to_keep = 5, max_to_keep_enable = 1",
    );
    // Everything is read, so delete_read takes all 40 and the later rules find
    // nothing left to do. The report must not claim 40 + 30 + 35.
    let r = cleanup::run(&mut db, None).unwrap();
    assert_eq!(r.total_deleted(), 40);
    assert_eq!(live(&db), 0);
}

#[test]
fn stats_describe_the_database() {
    let (mut db, feed) = db_with(10);
    db.conn()
        .execute("UPDATE news SET read = 0 WHERE title = 'Article 1'", [])
        .unwrap();
    db.conn()
        .execute("UPDATE news SET starred = 1 WHERE title = 'Article 2'", [])
        .unwrap();
    db.conn()
        .execute("UPDATE news SET article_html = '<p>x</p>' WHERE title = 'Article 3'", [])
        .unwrap();
    db.recompute_counters().unwrap();

    let s = cleanup::stats(&db).unwrap();
    assert_eq!(s.feeds, 1);
    assert_eq!(s.articles, 10);
    assert_eq!(s.unread, 1);
    assert_eq!(s.starred, 1);
    assert_eq!(s.with_cached_article, 1);
    assert!(s.bytes > 0);
    assert!(s.oldest.is_some());

    assert_eq!(cleanup::clear_article_cache(&db).unwrap(), 1);
    assert_eq!(cleanup::stats(&db).unwrap().with_cached_article, 0);

    set(&db, feed, "max_to_keep = 2, max_to_keep_enable = 1");
    cleanup::run(&mut db, None).unwrap();
    // 10 articles, keep the 2 newest; Article 2 is starred so it survives too.
    assert_eq!(cleanup::stats(&db).unwrap().deleted, 7);
}

#[test]
fn a_noop_ruleset_is_recognised() {
    assert!(Retention::default().is_noop());
    assert!(Retention { max_to_keep: Some(0), ..Default::default() }.is_noop());
    assert!(!Retention { delete_read: true, ..Default::default() }.is_noop());
    assert!(!Retention { max_age_days: Some(1), ..Default::default() }.is_noop());
}

#[test]
fn articles_with_no_publication_date_still_age_out() {
    let db = Db::open_in_memory().unwrap();
    db.conn()
        .execute(
            "INSERT INTO feeds(kind, text, xml_url, max_age_days, max_age_enable)
             VALUES(1, 'F', 'https://f.test/x', 5, 1)",
            [],
        )
        .unwrap();
    let feed = db.conn().last_insert_rowid();
    db.conn()
        .execute(
            "INSERT INTO news(feed_id, guid, title, published, received, read, deleted)
             VALUES(?1, 'g', 'No date', NULL, ?2, 1, 0)",
            rusqlite::params![feed, ts(30)],
        )
        .unwrap();

    let mut db = db;
    cleanup::run(&mut db, None).unwrap();
    assert_eq!(live(&db), 0, "received timestamp should be the fallback");
}

#[test]
fn global_protections_reach_feeds_with_no_rules_of_their_own() {
    // The feeds table defaults every protection column to 1, so reading those
    // columns unconditionally made the global switches dead: a user could turn
    // "never delete unread" off in Settings and nothing would change.
    let (mut db, _) = db_with(30);
    db.conn().execute("UPDATE news SET read = 0", []).unwrap();
    db.recompute_counters().unwrap();

    db.conn()
        .execute_batch(
            "INSERT INTO settings(key, value) VALUES('cleanup.max_to_keep', '5');
             INSERT INTO settings(key, value) VALUES('cleanup.never_delete_unread', '0');",
        )
        .unwrap();

    let g = cleanup::global_retention(&db).unwrap();
    assert!(!g.never_delete_unread, "global switch not read");

    cleanup::run(&mut db, None).unwrap();
    assert_eq!(live(&db), 5, "the global protection switch was ignored");
}

#[test]
fn a_feed_with_its_own_rules_keeps_its_own_protections() {
    // The mirror image: a feed that has been configured deliberately must not
    // have its protections overridden by a global switch.
    let (mut db, feed) = db_with(30);
    db.conn().execute("UPDATE news SET read = 0", []).unwrap();
    db.recompute_counters().unwrap();

    db.conn()
        .execute(
            "INSERT INTO settings(key, value) VALUES('cleanup.never_delete_unread', '0')",
            [],
        )
        .unwrap();
    set(
        &db,
        feed,
        "max_to_keep = 5, max_to_keep_enable = 1, never_delete_unread = 1",
    );

    let r = cleanup::retention_for(&db, feed).unwrap();
    assert!(r.never_delete_unread, "the feed's own protection was overridden");

    cleanup::run(&mut db, None).unwrap();
    assert_eq!(live(&db), 30, "unread articles were deleted anyway");
}

/// QuiteRSS marks read articles 2 as often as 1. "Never delete unread" must
/// treat both as read, or imported articles are never cleaned up.
#[test]
fn quiterss_read_value_counts_as_read() {
    let (mut db, feed) = db_with(4);
    db.conn().execute("UPDATE news SET read = 2", []).unwrap();
    set(&db, feed, "delete_read = 1");
    cleanup::run(&mut db, None).unwrap();
    assert_eq!(live(&db), 0);
}

/// "Delete once read" must not delete unread articles when the "never delete
/// unread" protection is switched off: that protection is about the age and
/// count rules, and without it this rule deleted everything.
#[test]
fn delete_once_read_leaves_unread_articles_whatever_the_protections() {
    let (mut db, feed) = db_with(4);
    db.conn().execute("UPDATE news SET read = 0 WHERE title IN ('Article 0', 'Article 1')", []).unwrap();
    set(&db, feed, "delete_read = 1, never_delete_unread = 0");
    cleanup::run(&mut db, None).unwrap();
    assert_eq!(live(&db), 2);
}

#[test]
fn the_starred_count_leaves_out_deleted_articles() {
    let (db, _) = db_with(3);
    db.conn().execute("UPDATE news SET starred = 1", []).unwrap();
    db.conn().execute("UPDATE news SET deleted = 1 WHERE title = 'Article 0'", []).unwrap();
    assert_eq!(cleanup::stats(&db).unwrap().starred, 2);
}

#[test]
fn compacting_leaves_no_large_wal_file_behind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.db");
    let mut db = snaprss_core::Db::open(&path).unwrap();
    db.conn().execute("INSERT INTO feeds(kind, text, xml_url) VALUES(1, 'f', 'https://f.test/')", []).unwrap();
    let big = "x".repeat(4000);
    for i in 0..500 {
        db.conn()
            .execute(
                "INSERT INTO news(feed_id, guid, title, description, received, deleted) VALUES(1, ?1, 't', ?2, '2026-01-01T00:00:00Z', 1)",
                rusqlite::params![format!("g{i}"), big],
            )
            .unwrap();
    }
    cleanup::purge_deleted(&mut db, None).unwrap();
    cleanup::vacuum(&db).unwrap();
    let wal = std::fs::metadata(dir.path().join("c.db-wal")).map(|m| m.len()).unwrap_or(0);
    assert!(wal < 64 * 1024, "wal is {wal} bytes");
}

#[test]
fn recounting_one_feed_after_a_change_matches_a_full_recount() {
    let (db, feed) = db_with(6);
    db.conn().execute("INSERT INTO feeds(kind, text, xml_url) VALUES(1, 'Other', 'https://o.test/')", []).unwrap();
    let ids: Vec<i64> = db
        .conn()
        .prepare("SELECT id FROM news WHERE feed_id = ?1 LIMIT 2")
        .unwrap()
        .query_map([feed], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    db.conn().execute("UPDATE news SET read = 0", []).unwrap();
    db.conn().execute("UPDATE news SET deleted = 1 WHERE id = ?1", [ids[0]]).unwrap();
    db.recompute_counters_for_news(&ids).unwrap();
    let partial: (i64, i64) = db
        .conn()
        .query_row("SELECT unread, undelete_count FROM feeds WHERE id = ?1", [feed], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    db.recompute_counters().unwrap();
    let full: (i64, i64) = db
        .conn()
        .query_row("SELECT unread, undelete_count FROM feeds WHERE id = ?1", [feed], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(partial, full);
    assert_eq!(full, (5, 5));
}

/// The queries run on every click stay on indexes. Measured on 300,000
/// articles, a full scan here was hundreds of milliseconds with the database
/// locked.
#[test]
fn common_queries_use_indexes() {
    let (db, _) = db_with(3);
    db.conn().execute_batch("ANALYZE").unwrap();
    for q in [
        "SELECT COUNT(*) FROM news WHERE read = 0 AND deleted = 0",
        "SELECT COUNT(*) FROM news WHERE starred = 1 AND deleted = 0",
        "SELECT id FROM news WHERE deleted = 0 AND read = 0 ORDER BY COALESCE(published, received) DESC LIMIT 500",
        "SELECT id FROM news WHERE deleted = 0 AND starred = 1 ORDER BY COALESCE(published, received) DESC LIMIT 500",
        "SELECT id FROM news WHERE deleted = 0 ORDER BY COALESCE(published, received) DESC LIMIT 500",
        "SELECT COUNT(*) FROM news WHERE feed_id = 1 AND deleted = 0 AND read = 0",
        "SELECT id FROM news WHERE feed_id = 1 AND link_href = 'x' AND title = 'y'",
    ] {
        let plan: Vec<String> = db
            .conn()
            .prepare(&format!("EXPLAIN QUERY PLAN {q}"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(
            !plan.iter().any(|p| p.starts_with("SCAN news") || p.contains("TEMP B-TREE")),
            "{q}\n{plan:?}"
        );
    }
}

/// Status posts have no guid, link or title; ingestion recognises them by
/// their text alone. A stub without the text is a stranger, and the post came
/// back new and unread on the next poll.
#[test]
fn purging_an_item_with_only_a_description_keeps_the_description() {
    let (mut db, feed) = db_with(1);
    db.conn()
        .execute(
            "INSERT INTO news(feed_id, description, published, received, read, deleted, delete_date)
             VALUES(?1, 'Just a status update', ?2, ?2, 1, 1, ?2)",
            rusqlite::params![feed, ts(30)],
        )
        .unwrap();
    db.conn()
        .execute("UPDATE news SET deleted = 1, delete_date = ?1, description = 'long body' WHERE guid = 'g0'", [ts(30)])
        .unwrap();
    cleanup::purge_deleted(&mut db, None).unwrap();

    // The query ingestion uses for an entry with nothing but text.
    let hit: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM news WHERE feed_id = ?1
               AND IFNULL(link_href, '') = '' AND IFNULL(title, '') = ''
               AND description = ?2 AND IFNULL(published, '') = IFNULL(?3, '')
               AND deleted = 2",
            rusqlite::params![feed, "Just a status update", ts(30)],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(hit, 1, "the stub is still recognisable");
    // An article with a title is still identified by it; its body goes.
    let body: Option<String> = db
        .conn()
        .query_row("SELECT description FROM news WHERE guid = 'g0'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(body, None);
}
