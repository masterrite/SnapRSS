//! Timings of the real commands against a large database.
//!
//!     SNAPRSS_BENCH_DB=/tmp/big.db cargo test -p snaprss-app --release bench -- --ignored --nocapture
//!
//! The database is built on first run: 150 feeds in 12 folders, 2,000
//! articles each (300,000), bodies of a few KB, a tenth with a cached
//! article, a third unread, some starred and labelled.

use std::sync::Arc;
use std::time::Instant;

use tauri::Manager;
use tokio::sync::Mutex;

use crate::commands::{self, AppState};
use snaprss_core::Db;

const FEEDS: i64 = 150;
const PER_FEED: i64 = 2000;

fn build(path: &str) {
    if std::path::Path::new(path).exists() {
        return;
    }
    let mut db = Db::open(path).unwrap();
    let tx = db.conn_mut().transaction().unwrap();
    let body = "<p>".to_string() + &"Lorem ipsum dolor sit amet, 机核网 文章 正文。 ".repeat(25) + "</p>";
    for f in 0..12 {
        tx.execute(
            "INSERT INTO feeds(id, kind, text, xml_url, row_to_parent) VALUES(?1, 0, ?2, '', ?3)",
            rusqlite::params![10_000 + f, format!("Folder {f}"), f],
        )
        .unwrap();
    }
    tx.execute_batch(
        "INSERT INTO labels(id, name, num) VALUES(1, 'Important', 0), (2, 'Read later', 1);",
    )
    .unwrap();
    for feed in 1..=FEEDS {
        tx.execute(
            "INSERT INTO feeds(id, kind, parent_id, text, xml_url, row_to_parent) VALUES(?1, 1, ?2, ?3, ?4, ?1)",
            rusqlite::params![feed, 10_000 + feed % 12, format!("Feed {feed}"), format!("https://f{feed}.test/rss")],
        )
        .unwrap();
        let mut ins = tx
            .prepare(
                "INSERT INTO news(feed_id, guid, title, description, published, received, link_href,
                                  read, starred, new, deleted, article_html)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7, ?8, 0, 0, ?9)",
            )
            .unwrap();
        for n in 0..PER_FEED {
            let day = n / 10;
            let ts = format!(
                "{}Z",
                (chrono::NaiveDate::from_ymd_opt(2026, 9, 20).unwrap() - chrono::Duration::days(day))
                    .and_hms_opt((n % 24) as u32, 0, 0)
                    .unwrap()
                    .format("%Y-%m-%dT%H:%M:%S")
            );
            ins.execute(rusqlite::params![
                feed,
                format!("f{feed}-{n}"),
                format!("Article {n} from feed {feed} 标题"),
                body,
                ts,
                format!("https://f{feed}.test/{n}"),
                (n % 3 != 0) as i64,
                (n % 97 == 0) as i64,
                (n % 10 == 0).then(|| body.clone()),
            ])
            .unwrap();
        }
    }
    tx.execute_batch(
        "INSERT INTO news_labels(news_id, label_id) SELECT id, 1 + (id % 2) FROM news WHERE id % 50 = 0;",
    )
    .unwrap();
    tx.commit().unwrap();
    db.recompute_counters().unwrap();
    db.conn().execute_batch("ANALYZE;").unwrap();
}

async fn time<T, F: std::future::Future<Output = T>>(label: &str, f: F) -> T {
    let t = Instant::now();
    let out = f.await;
    println!("{:>9.1} ms  {label}", t.elapsed().as_secs_f64() * 1000.0);
    out
}

#[test]
#[ignore]
fn bench() {
    let Ok(path) = std::env::var("SNAPRSS_BENCH_DB") else { return };
    let t = Instant::now();
    build(&path);
    println!("database ready in {:.1}s", t.elapsed().as_secs_f64());

    let app = tauri::test::mock_app();
    let cfg = snaprss_fetch::FetchConfig::default();
    app.manage(AppState {
        db: Arc::new(Mutex::new(Db::open(&path).unwrap())),
        http: snaprss_fetch::build_client(&cfg).unwrap(),
        fetch_cfg: cfg,
        close_to_tray: Default::default(),
        quitting: Default::default(),
        pending_update: Default::default(),
    });
    let st = || app.state::<AppState>();

    tauri::async_runtime::block_on(async {
        time("feed_tree", commands::feed_tree(st())).await.unwrap();
        time("counts", commands::read_counts(&st())).await.unwrap();
        for scope in ["unread", "all", "starred", "feed:7", "folder:10003", "label:1", "deleted"] {
            time(&format!("news_list {scope}"), commands::news_list(st(), scope.into(), Some(500), None, Some(false), None, None))
                .await
                .unwrap();
        }
        time("news_list all (newspaper excerpts)", commands::news_list(st(), "all".into(), Some(500), None, Some(true), None, None))
            .await
            .unwrap();
        time("news_list all, page at 200,000", commands::news_list(st(), "all".into(), Some(500), Some(200_000), Some(false), None, None))
            .await
            .unwrap();
        time("news_list all, search \"feed 77\"", commands::news_list(st(), "all".into(), Some(500), None, Some(false), Some("feed 77".into()), None))
            .await
            .unwrap();
        time("news_list all, search that matches nothing", commands::news_list(st(), "all".into(), Some(500), None, Some(false), Some("zzzqqq".into()), None))
            .await
            .unwrap();
        time("news_list all, search новости (matches nothing)", commands::news_list(st(), "all".into(), Some(500), None, Some(false), Some("новости".into()), None))
            .await
            .unwrap();
        time("news_list all, search 标题 (every article)", commands::news_list(st(), "all".into(), Some(500), None, Some(false), Some("标题".into()), None))
            .await
            .unwrap();
        time("news_list all, sorted by title", commands::news_list(st(), "all".into(), Some(500), None, Some(false), None, Some("title".into())))
            .await
            .unwrap();
        time("news_list feed:7, sorted by title", commands::news_list(st(), "feed:7".into(), Some(500), None, Some(false), None, Some("title".into())))
            .await
            .unwrap();
        let first: i64 = {
            let handle = st().db.clone();
            let db = handle.lock().await;
            db.conn().query_row("SELECT id FROM news WHERE article_html IS NOT NULL LIMIT 1", [], |r| r.get(0)).unwrap()
        };
        time("set_read one", commands::set_read(st(), vec![first], true)).await.unwrap();
        time("set_starred one", commands::set_starred(st(), vec![first], true)).await.unwrap();
        time("set_deleted one", commands::set_deleted(st(), vec![first], true)).await.unwrap();
        time("set_deleted restore", commands::set_deleted(st(), vec![first], false)).await.unwrap();
        time("labels", commands::labels(st())).await.unwrap();
        time("mark_scope_read feed:9", commands::mark_scope_read(st(), "feed:9".into())).await.unwrap();
        time("mark_scope_read unread (everything)", commands::mark_scope_read(st(), "unread".into())).await.unwrap();
        time("db_stats", commands::db_stats(st())).await.unwrap();
        time("run_cleanup (no rules)", commands::run_cleanup(st(), None)).await.unwrap();

        // A poll: every feed answers with its 50 newest items, 5 of them new.
        let fetched: Vec<snaprss_fetch::Fetched> = (1..=FEEDS)
            .map(|feed| {
                let mut xml = String::from("<rss version=\"2.0\"><channel><title>t</title>");
                for n in (0..45).chain(PER_FEED..PER_FEED + 5) {
                    xml.push_str(&format!(
                        "<item><guid>f{feed}-{n}</guid><title>Article {n} from feed {feed} 标题</title>\
                         <link>https://f{feed}.test/{n}</link><pubDate>Sun, 20 Sep 2026 10:00:00 GMT</pubDate>\
                         <description>&lt;p&gt;body&lt;/p&gt;</description></item>"
                    ));
                }
                xml.push_str("</channel></rss>");
                snaprss_fetch::Fetched {
                    feed: snaprss_fetch::DueFeed {
                        id: feed,
                        title: format!("Feed {feed}"),
                        xml_url: format!("https://f{feed}.test/rss"),
                        etag: None,
                        last_modified: None,
                        credentials: None,
                    },
                    result: Ok(snaprss_fetch::FetchOutcome::Body {
                        bytes: xml.into_bytes(),
                        validators: Default::default(),
                        final_url: None,
                        content_type: None,
                    }),
                }
            })
            .collect();
        let db = st().db.clone();
        time("apply_fetched: 150 feeds x 50 items, 750 new", async move {
            let mut db = db.lock().await;
            snaprss_fetch::apply_fetched(&mut db, fetched).unwrap()
        })
        .await;
    });
}
