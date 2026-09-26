//! Add feed with a site's address instead of its feed's.

use snaprss_fetch::discover::{discover, normalise};
use snaprss_fetch::{build_client, FetchConfig};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const RSS: &str = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>The Blog</title>
<item><guid>1</guid><title>Hello</title></item></channel></rss>"#;

async fn serve(server: &MockServer, at: &str, body: &str, ty: &str) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), ty))
        .mount(server)
        .await;
}

#[tokio::test]
async fn finds_feeds_from_a_page_or_takes_a_feed_as_it_is() {
    let server = MockServer::start().await;
    serve(&server, "/feed.xml", RSS, "application/rss+xml").await;
    serve(
        &server,
        "/",
        r#"<html><head><title>Site</title>
           <link rel="stylesheet" href="/s.css">
           <link rel="alternate" type="application/rss+xml" title="Posts" href="/feed.xml">
           <link rel="alternate" type="application/atom+xml" href="comments.atom">
           <link rel="alternate" type="application/rss+xml" href="/feed.xml">
           <link rel="alternate" hreflang="fr" href="/fr/">
           </head><body>hi</body></html>"#,
        "text/html; charset=utf-8",
    )
    .await;
    let cfg = FetchConfig::default();
    let client = build_client(&cfg).unwrap();

    let found = discover(&client, &cfg, &format!("{}/", server.uri())).await.unwrap();
    let urls: Vec<_> = found.iter().map(|f| f.url.clone()).collect();
    assert_eq!(
        urls,
        [format!("{}/feed.xml", server.uri()), format!("{}/comments.atom", server.uri())],
        "both feeds, resolved, once each; the translation link is not a feed"
    );
    assert_eq!(found[0].title.as_deref(), Some("Posts"));

    let direct = discover(&client, &cfg, &format!("{}/feed.xml", server.uri())).await.unwrap();
    assert_eq!(direct.len(), 1);
    assert_eq!(direct[0].title.as_deref(), Some("The Blog"), "a feed address is taken as it is");
}

#[tokio::test]
async fn tries_the_usual_paths_when_the_page_names_none() {
    let server = MockServer::start().await;
    serve(&server, "/", "<html><body>no links</body></html>", "text/html").await;
    serve(&server, "/rss.xml", RSS, "text/xml").await;
    let cfg = FetchConfig::default();
    let client = build_client(&cfg).unwrap();
    let found = discover(&client, &cfg, &server.uri()).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].url, format!("{}/rss.xml", server.uri()));

    let empty = MockServer::start().await;
    serve(&empty, "/", "<html><body>nothing here</body></html>", "text/html").await;
    assert!(discover(&client, &cfg, &empty.uri()).await.unwrap().is_empty());
}

#[test]
fn a_bare_domain_gets_https() {
    assert_eq!(normalise("example.com"), "https://example.com");
    assert_eq!(normalise(" http://x.test/feed "), "http://x.test/feed");
}
