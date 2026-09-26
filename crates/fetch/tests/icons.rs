//! Site icons: found on the site, stored as QuiteRSS stores them, shown.

use snaprss_core::Db;
use snaprss_fetch::icons::find_icon;
use snaprss_fetch::{build_client, FetchConfig};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRsmall";
const ICO: &[u8] = &[0, 0, 1, 0, 1, 0, 16, 16];

async fn serve(server: &MockServer, at: &str, body: &[u8], ty: &str) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.to_vec(), ty))
        .mount(server)
        .await;
}

#[tokio::test]
async fn the_icon_the_page_names_wins_over_favicon_ico() {
    let server = MockServer::start().await;
    serve(
        &server,
        "/",
        br#"<html><head>
            <link rel="apple-touch-icon" href="/touch.png">
            <link rel="icon" sizes="192x192" href="/big.png">
            <link rel="icon" sizes="32x32" href="/img/icon-32.png">
        </head></html>"#,
        "text/html",
    )
    .await;
    serve(&server, "/img/icon-32.png", PNG, "image/png").await;
    serve(&server, "/favicon.ico", ICO, "image/x-icon").await;
    let cfg = FetchConfig::default();
    let client = build_client(&cfg).unwrap();
    let got = find_icon(&client, &cfg, &format!("{}/blog/post", server.uri())).await;
    assert_eq!(got.as_deref(), Some(PNG), "the 32px icon the page names");
}

#[tokio::test]
async fn falls_back_to_favicon_ico_and_refuses_non_images() {
    let server = MockServer::start().await;
    serve(&server, "/", b"<html><head><link rel=icon href=/broken.png></head></html>", "text/html").await;
    serve(&server, "/broken.png", b"<html>not found</html>", "text/html").await;
    serve(&server, "/favicon.ico", ICO, "image/x-icon").await;
    let cfg = FetchConfig::default();
    let client = build_client(&cfg).unwrap();
    assert_eq!(find_icon(&client, &cfg, &server.uri()).await.as_deref(), Some(ICO));

    let empty = MockServer::start().await;
    serve(&empty, "/", b"<html></html>", "text/html").await;
    serve(&empty, "/favicon.ico", b"<html>soft 404</html>", "text/html").await;
    assert!(find_icon(&client, &cfg, &empty.uri()).await.is_none(), "a page is not an icon");
}

#[test]
fn icons_are_stored_as_quiterss_stores_them_and_shown_as_data_urls() {
    let db = Db::open_in_memory().unwrap();
    // An imported QuiteRSS icon: base64 text in a blob.
    db.conn()
        .execute(
            "INSERT INTO feeds(id, kind, text, xml_url, image) VALUES (1, 1, 'Old', 'https://o.test/rss', ?1)",
            [b"iVBORw0KGgpzbWFsbA==".to_vec()],
        )
        .unwrap();
    db.conn()
        .execute_batch(
            "INSERT INTO feeds(id, kind, text, xml_url, html_url) VALUES (2, 1, 'New', 'https://n.test/rss', 'https://n.test/');
             INSERT INTO feeds(id, kind, text, xml_url) VALUES (3, 1, 'None', 'https://x.test/rss');",
        )
        .unwrap();
    let icons = db.feed_icons().unwrap();
    assert_eq!(icons, vec![(1, "data:image/png;base64,iVBORw0KGgpzbWFsbA==".to_string())]);

    let need = db.feeds_needing_icons(10).unwrap();
    assert_eq!(need, vec![(2, "https://n.test/".to_string()), (3, "https://x.test/rss".to_string())],
        "feeds without an icon, looked up at their site");

    db.store_icon(2, "https://n.test/", Some(ICO)).unwrap();
    db.store_icon(3, "https://x.test/rss", None).unwrap();
    let icons = db.feed_icons().unwrap();
    assert_eq!(icons.len(), 2);
    assert!(icons[1].1.starts_with("data:image/x-icon;base64,"));
    assert!(db.feeds_needing_icons(10).unwrap().is_empty(), "a feed with no icon is not asked again for a while");

    // Something that is not an image is not kept.
    db.conn().execute("UPDATE feeds SET icon_checked = NULL WHERE id = 3", []).unwrap();
    db.store_icon(3, "https://x.test/rss", Some(b"<html>")).unwrap();
    assert_eq!(db.feed_icons().unwrap().len(), 2);

    // Feed 3 was removed during the lookup and a feed for another site got
    // its id: the icon found for the old site is not given to it.
    db.conn()
        .execute_batch(
            "DELETE FROM feeds WHERE id = 3;
             INSERT INTO feeds(id, kind, text, xml_url) VALUES (3, 1, 'Other', 'https://other.test/rss');",
        )
        .unwrap();
    db.store_icon(3, "https://x.test/rss", Some(ICO)).unwrap();
    assert_eq!(db.feed_icons().unwrap().len(), 2, "the new feed 3 has no icon");
}

#[test]
fn a_feed_update_records_the_site_address() {
    let mut db = Db::open_in_memory().unwrap();
    db.conn().execute("INSERT INTO feeds(id, kind, text, xml_url) VALUES (1, 1, 'x', 'https://s.test/rss')", []).unwrap();
    let rss = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>S</title><link>https://s.test/</link>
        <item><guid>1</guid><title>a</title></item></channel></rss>"#;
    snaprss_fetch::apply_fetched(
        &mut db,
        vec![snaprss_fetch::Fetched {
            feed: snaprss_fetch::DueFeed {
                id: 1,
                title: "x".into(),
                xml_url: "https://s.test/rss".into(),
                etag: None,
                last_modified: None,
                credentials: None,
            },
            result: Ok(snaprss_fetch::FetchOutcome::Body {
                bytes: rss.as_bytes().to_vec(),
                validators: Default::default(),
                final_url: None,
                content_type: None,
            }),
        }],
    )
    .unwrap();
    let site: Option<String> = db.conn().query_row("SELECT html_url FROM feeds WHERE id = 1", [], |r| r.get(0)).unwrap();
    assert_eq!(site.as_deref(), Some("https://s.test/"));
}
