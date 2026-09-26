//! Opening a database written by an earlier SnapRSS.

use snaprss_core::Db;

#[test]
fn an_older_database_gains_the_icon_column_and_keeps_its_version() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    {
        let db = Db::open(&path).unwrap();
        db.conn().execute_batch("ALTER TABLE feeds DROP COLUMN icon_checked").unwrap();
    }
    let db = Db::open(&path).unwrap();
    let has: bool = db
        .conn()
        .prepare("PRAGMA table_info(feeds)")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .any(|c| c.unwrap() == "icon_checked");
    assert!(has);
    let v: String = db
        .conn()
        .query_row("SELECT value FROM settings WHERE key = 'schema_version'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        v,
        snaprss_core::db::SCHEMA_VERSION.to_string(),
        "not bumped by the column, so an older SnapRSS still opens the file"
    );
}

#[test]
fn cached_webtoon_extractions_are_dropped_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    let insert = "INSERT INTO news(feed_id, guid, title, link_href, article_html, article_fetched, received)
                  VALUES (1, ?1, 't', ?2, '<p>episode list</p>', '2026-09-01T00:00:00Z', '2026-09-01T00:00:00Z')";
    {
        let db = Db::open(&path).unwrap();
        db.conn()
            .execute_batch(
                "INSERT INTO feeds(id, kind, text, xml_url) VALUES (1, 1, 'Nerd and Jock', 'https://www.webtoons.com/en/challenge/nerd-and-jock/rss?title_no=135963');
                 DELETE FROM settings WHERE key = 'repair.webtoon_extract';",
            )
            .unwrap();
        db.conn().execute(insert, ["a", "https://www.webtoons.com/en/canvas/nerd-and-jock/ep-1/viewer?episode_no=1"]).unwrap();
        db.conn().execute(insert, ["b", "https://blog.test/post"]).unwrap();
    }
    let cached = |db: &Db| -> Vec<Option<String>> {
        db.conn()
            .prepare("SELECT article_html FROM news ORDER BY guid")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    let db = Db::open(&path).unwrap();
    assert_eq!(cached(&db), vec![None, Some("<p>episode list</p>".to_string())], "only the Webtoon one goes");

    // Extracted again afterwards, it stays: the repair ran once.
    db.conn().execute("UPDATE news SET article_html = '<p>panels</p>' WHERE guid = 'a'", []).unwrap();
    drop(db);
    let db = Db::open(&path).unwrap();
    assert_eq!(cached(&db)[0].as_deref(), Some("<p>panels</p>"));
}

#[test]
fn the_word_author_stored_as_a_name_is_cleared_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    {
        let db = Db::open(&path).unwrap();
        db.conn()
            .execute_batch(
                "INSERT INTO feeds(id, kind, text, xml_url) VALUES (1, 1, 'F', 'https://a.test/rss');
                 INSERT INTO news(feed_id, guid, title, author_name, received) VALUES
                   (1, 'a', 'x', 'author', '2026-09-01T00:00:00Z'),
                   (1, 'b', 'y', 'Marko R', '2026-09-01T00:00:00Z');
                 DELETE FROM settings WHERE key = 'repair.rss_author';",
            )
            .unwrap();
    }
    let db = Db::open(&path).unwrap();
    let names: Vec<Option<String>> = db
        .conn()
        .prepare("SELECT author_name FROM news ORDER BY guid")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(names, vec![None, Some("Marko R".to_string())]);
}
