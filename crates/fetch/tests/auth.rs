//! Feeds that need a user name and password.

use snaprss_core::Db;
use snaprss_fetch::{all_feeds, build_client, fetch_all, apply_fetched, FetchConfig};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const RSS: &str = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Private</title>
<item><guid>p1</guid><title>Members only</title><link>https://p.test/1</link></item></channel></rss>"#;

fn db_with_feed(url: &str, auth: bool) -> Db {
    let db = Db::open_in_memory().unwrap();
    db.conn()
        .execute(
            "INSERT INTO feeds(id, kind, text, xml_url, authentication) VALUES (1, 1, 'Private', ?1, ?2)",
            rusqlite::params![url, auth as i64],
        )
        .unwrap();
    db
}

#[tokio::test]
async fn a_quiterss_password_is_sent_as_basic_auth() {
    let server = MockServer::start().await;
    // "user:s3cret" in base64 is dXNlcjpzM2NyZXQ=
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .and(header("authorization", "Basic dXNlcjpzM2NyZXQ="))
        .respond_with(ResponseTemplate::new(200).set_body_string(RSS))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/feed.xml"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let url = format!("{}/feed.xml", server.uri());
    let mut db = db_with_feed(&url, true);
    // As QuiteRSS stores it: keyed by host, password base64-encoded.
    db.conn()
        .execute(
            "INSERT INTO passwords(server, username, password) VALUES ('127.0.0.1', 'user', 'czNjcmV0')",
            [],
        )
        .unwrap();

    let cfg = FetchConfig::default();
    let client = build_client(&cfg).unwrap();
    let feeds = all_feeds(&db).unwrap();
    assert_eq!(feeds[0].credentials.as_ref().map(|c| c.user.as_str()), Some("user"));
    assert!(!format!("{:?}", feeds[0]).contains("s3cret"), "the password never appears in logs");
    let fetched = fetch_all(&client, &cfg, feeds, 1, |_| {}).await;
    apply_fetched(&mut db, fetched).unwrap();
    let n: i64 = db.conn().query_row("SELECT COUNT(*) FROM news", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1, "signed in, the article arrives");
}

#[tokio::test]
async fn without_a_password_the_error_says_what_is_needed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let url = format!("{}/feed.xml", server.uri());
    let mut db = db_with_feed(&url, false);
    let cfg = FetchConfig::default();
    let client = build_client(&cfg).unwrap();
    let feeds = all_feeds(&db).unwrap();
    assert!(feeds[0].credentials.is_none());
    let fetched = fetch_all(&client, &cfg, feeds, 1, |_| {}).await;
    apply_fetched(&mut db, fetched).unwrap();
    let status: String = db.conn().query_row("SELECT status FROM feeds WHERE id = 1", [], |r| r.get(0)).unwrap();
    assert!(status.contains("user name and password"), "{status}");
}

#[test]
fn sign_in_is_saved_the_way_quiterss_reads_it() {
    let db = db_with_feed("https://members.example.com/rss", false);
    db.set_feed_credentials(1, "ann", "pässword").unwrap();
    let (server, user, pass): (String, String, String) = db
        .conn()
        .query_row("SELECT server, username, password FROM passwords", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap();
    assert_eq!((server.as_str(), user.as_str()), ("members.example.com", "ann"));
    assert_eq!(pass, "cMOkc3N3b3Jk", "base64 of the UTF-8 bytes, as QuiteRSS writes it");
    assert_eq!(db.feed_credentials(1).unwrap().unwrap().password, "pässword");

    // Changing it updates the one row.
    db.set_feed_credentials(1, "ann", "new").unwrap();
    let rows: i64 = db.conn().query_row("SELECT COUNT(*) FROM passwords", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 1);

    // A second feed on the same host keeps the row alive when the first signs out.
    db.conn()
        .execute("INSERT INTO feeds(id, kind, text, xml_url, authentication) VALUES (2, 1, 'Two', 'https://members.example.com/other', 1)", [])
        .unwrap();
    db.set_feed_credentials(1, "", "").unwrap();
    assert!(db.feed_credentials(1).unwrap().is_none(), "signed out");
    assert_eq!(db.feed_credentials(2).unwrap().unwrap().user, "ann", "the other feed still signs in");
    db.set_feed_credentials(2, "", "").unwrap();
    let rows: i64 = db.conn().query_row("SELECT COUNT(*) FROM passwords", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 0, "the last one out removes the saved password");
}

/// A filter's sound and notification come back from the update, for the
/// app to play and show: one sound however many articles match, and one
/// notice per matching article.
#[test]
fn filter_sounds_and_notices_reach_the_app() {
    let mut db = Db::open_in_memory().unwrap();
    db.conn()
        .execute_batch(
            "INSERT INTO feeds(id, kind, text, xml_url) VALUES (1, 1, 'Deals', 'https://d.test/rss');
             INSERT INTO filters(id, name, type, enable, num) VALUES (1, 'sale', 1, 1, 0);
             INSERT INTO filter_conditions(filter_id, field, condition, content) VALUES (1, 'title', 'contains', 'sale');
             INSERT INTO filter_actions(filter_id, action, params) VALUES (1, 'play_sound', 'C:\\ding.wav');
             INSERT INTO filter_actions(filter_id, action, params) VALUES (1, 'notify', NULL);",
        )
        .unwrap();
    let rss = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Deals</title>
        <item><guid>a</guid><title>Big sale today</title></item>
        <item><guid>b</guid><title>Another sale</title></item>
        <item><guid>c</guid><title>Nothing special</title></item></channel></rss>"#;
    let feed = snaprss_fetch::DueFeed {
        id: 1,
        title: "Deals".into(),
        xml_url: "https://d.test/rss".into(),
        etag: None,
        last_modified: None,
        credentials: None,
    };
    let s = apply_fetched(
        &mut db,
        vec![snaprss_fetch::Fetched {
            feed,
            result: Ok(snaprss_fetch::FetchOutcome::Body {
                bytes: rss.as_bytes().to_vec(),
                validators: Default::default(),
                final_url: None,
                content_type: None,
            }),
        }],
    )
    .unwrap();
    assert_eq!(s.sounds, vec!["C:\\ding.wav".to_string()], "one sound for the batch");
    assert_eq!(
        s.notices,
        vec![("Deals".to_string(), "Big sale today".to_string()), ("Deals".to_string(), "Another sale".to_string())]
    );
}

/// Saving a second feed on the same site with the user name and a blank
/// password keeps the site's password; it does not blank it for the first.
#[test]
fn a_blank_password_keeps_the_sites_saved_one() {
    let db = db_with_feed("https://members.example.com/a", false);
    db.conn()
        .execute("INSERT INTO feeds(id, kind, text, xml_url) VALUES (2, 1, 'B', 'https://members.example.com/b')", [])
        .unwrap();
    db.set_feed_credentials(1, "bob", "pw").unwrap();
    // What Feed properties saves for feed 2: "bob", no password typed.
    db.save_feed_sign_in(2, "bob", None).unwrap();
    assert_eq!(db.feed_credentials(1).unwrap().unwrap().password, "pw");
    assert_eq!(db.feed_credentials(2).unwrap().unwrap().password, "pw");
}

/// http://host/feed -> https://host/feed changes the port, and reqwest drops
/// the Authorization header on a port change. Two local servers on one host
/// stand in for the two ports.
#[tokio::test]
async fn a_password_survives_a_redirect_to_another_port_on_the_same_host() {
    use snaprss_core::passwords::Credentials;
    use snaprss_fetch::{fetch_as, Validators};

    let target = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header("authorization", "Basic dXNlcjpwYXNz"))
        .respond_with(ResponseTemplate::new(200).set_body_string(RSS))
        .mount(&target)
        .await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(401)).mount(&target).await;
    let origin = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(301).insert_header("location", format!("{}/feed", target.uri())))
        .mount(&origin)
        .await;

    let cfg = FetchConfig::default();
    let client = build_client(&cfg).unwrap();
    let creds = Credentials { user: "user".into(), password: "pass".into() };
    let r = fetch_as(&client, &format!("{}/feed", origin.uri()), &Validators::default(), &cfg, Some(&creds)).await;
    assert!(r.is_ok(), "{r:?}");
}

/// The same redirect to another host never carries the password there.
#[tokio::test]
async fn a_password_is_never_sent_to_the_host_a_redirect_leads_to() {
    use snaprss_core::passwords::Credentials;
    use snaprss_fetch::{fetch_as, FetchError, Validators};

    let target = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(401)).mount(&target).await;
    let origin = MockServer::start().await;
    // 127.0.0.1 and localhost are one machine but two servers as far as a
    // saved password is concerned.
    let elsewhere = target.uri().replace("127.0.0.1", "localhost");
    assert_ne!(elsewhere, target.uri());
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(301).insert_header("location", format!("{elsewhere}/feed")))
        .mount(&origin)
        .await;

    let cfg = FetchConfig::default();
    let client = build_client(&cfg).unwrap();
    let creds = Credentials { user: "user".into(), password: "pass".into() };
    let r = fetch_as(&client, &format!("{}/feed", origin.uri()), &Validators::default(), &cfg, Some(&creds)).await;
    assert!(matches!(r, Err(FetchError::Status { status: 401 })), "{r:?}");
    let seen = target.received_requests().await.unwrap();
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|r| !r.headers.contains_key("authorization")), "{seen:?}");
}
