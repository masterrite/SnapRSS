//! Finding a site's feed from the address of one of its pages.

use reqwest::Client;

use crate::http::{fetch, FetchConfig, FetchError, FetchOutcome, Validators};

/// A feed found at or from an address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundFeed {
    pub url: String,
    pub title: Option<String>,
}

/// Paths sites commonly serve their feed at, tried when a page names none.
const COMMON: &[&str] = &["/feed", "/rss", "/feed.xml", "/rss.xml", "/atom.xml", "/index.xml"];

/// Adds `https://` to an address typed without a scheme.
pub fn normalise(address: &str) -> String {
    let a = address.trim();
    if a.contains("://") {
        a.to_string()
    } else {
        format!("https://{a}")
    }
}

async fn get(client: &Client, cfg: &FetchConfig, url: &str) -> Result<(Vec<u8>, String, Option<String>), FetchError> {
    match fetch(client, url, &Validators::default(), cfg).await? {
        FetchOutcome::Body { bytes, final_url, content_type, .. } => {
            Ok((bytes, final_url.unwrap_or_else(|| url.to_string()), content_type))
        }
        FetchOutcome::NotModified { .. } => Err(FetchError::Status { status: 304 }),
    }
}

fn as_feed(bytes: &[u8], url: &str, content_type: Option<&str>) -> Option<FoundFeed> {
    let parsed = crate::ingest::parse_feed_with(bytes, url, content_type).ok()?;
    Some(FoundFeed {
        url: url.to_string(),
        title: parsed.title.map(|t| t.content.trim().to_string()).filter(|t| !t.is_empty()),
    })
}

/// The feeds behind `address`. The address itself if it is a feed; else
/// the feeds its page links to; else the first of the usual feed paths on
/// the site that answers with a feed. Empty when there is none.
pub async fn discover(client: &Client, cfg: &FetchConfig, address: &str) -> Result<Vec<FoundFeed>, FetchError> {
    let url = normalise(address);
    let (bytes, landed, content_type) = get(client, cfg, &url).await?;
    if let Some(f) = as_feed(&bytes, &url, content_type.as_deref()) {
        // The address as typed, not where a redirect landed: the typed one is
        // the stable name (see update_feed on temporary redirects).
        return Ok(vec![f]);
    }

    let html = snaprss_article::decode_html(&bytes, content_type.as_deref());
    let linked = snaprss_article::feed_links(&html, &landed);
    if !linked.is_empty() {
        return Ok(linked.into_iter().map(|l| FoundFeed { url: l.url, title: l.title }).collect());
    }

    let Ok(root) = url::Url::parse(&landed) else { return Ok(Vec::new()) };
    for path in COMMON {
        let Ok(candidate) = root.join(path) else { continue };
        if let Ok((b, _, ct)) = get(client, cfg, candidate.as_str()).await {
            if let Some(f) = as_feed(&b, candidate.as_str(), ct.as_deref()) {
                return Ok(vec![f]);
            }
        }
    }
    Ok(Vec::new())
}
