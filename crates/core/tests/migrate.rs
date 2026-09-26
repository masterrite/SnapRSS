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
