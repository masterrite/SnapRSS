//! Looking up a site's icon.

use reqwest::Client;

use crate::http::{fetch, FetchConfig, FetchOutcome, Validators};

async fn get(client: &Client, cfg: &FetchConfig, url: &str) -> Option<(Vec<u8>, String, Option<String>)> {
    match fetch(client, url, &Validators::default(), cfg).await.ok()? {
        FetchOutcome::Body { bytes, final_url, content_type, .. } => {
            Some((bytes, final_url.unwrap_or_else(|| url.to_string()), content_type))
        }
        FetchOutcome::NotModified { .. } => None,
    }
}

fn usable(bytes: Vec<u8>) -> Option<Vec<u8>> {
    (bytes.len() <= snaprss_core::icons::MAX_ICON_BYTES && snaprss_core::icons::sniff(&bytes).is_some())
        .then_some(bytes)
}

/// The icon for the site at `site` (its home page, or failing that the feed
/// address): the icons its home page names, then `/favicon.ico`.
pub async fn find_icon(client: &Client, cfg: &FetchConfig, site: &str) -> Option<Vec<u8>> {
    let root = url::Url::parse(site.trim()).ok()?.join("/").ok()?;
    if let Some((page, landed, ct)) = get(client, cfg, root.as_str()).await {
        let html = snaprss_article::decode_html(&page, ct.as_deref());
        for icon in snaprss_article::icon_links(&html, &landed).into_iter().take(3) {
            if let Some((b, _, _)) = get(client, cfg, &icon).await {
                if let Some(ok) = usable(b) {
                    return Some(ok);
                }
            }
        }
    }
    let fallback = root.join("/favicon.ico").ok()?;
    get(client, cfg, fallback.as_str()).await.and_then(|(b, _, _)| usable(b))
}
