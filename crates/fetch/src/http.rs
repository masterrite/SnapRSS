//! The HTTP half of updating a feed.
//!
//! Every request carries `If-None-Match` and `If-Modified-Since` when the feed
//! has stored validators, so a server that supports them answers 304 with no
//! body. On a feed polled every fifteen minutes that is the difference between
//! a few hundred bytes a day and a few megabytes.

use std::time::Duration;

use reqwest::header::{
    HeaderMap, HeaderValue, ACCEPT, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED,
    USER_AGENT,
};
use reqwest::{Client, StatusCode};

pub const DEFAULT_USER_AGENT: &str = concat!(
    "SnapRSS/",
    env!("CARGO_PKG_VERSION"),
    " (+https://snaprss.example)"
);

/// Feed formats first, then a weak fallback. Some servers content-negotiate.
const ACCEPT_FEEDS: &str = "application/atom+xml, application/rss+xml, application/feed+json, \
                            application/xml;q=0.9, text/xml;q=0.9, */*;q=0.8";

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("network: {0}")]
    Network(String),
    #[error("http {status}")]
    Status { status: u16 },
    #[error("body too large: {0} bytes")]
    TooLarge(u64),
    #[error("bad url: {0}")]
    BadUrl(String),
}

impl FetchError {
    /// Whether retrying soon could plausibly work. 404 and 410 will not fix
    /// themselves; 429, 5xx and timeouts might.
    pub fn is_transient(&self) -> bool {
        match self {
            FetchError::Network(_) => true,
            FetchError::Status { status } => {
                *status == 408 || *status == 429 || (500..600).contains(status)
            }
            FetchError::TooLarge(_) | FetchError::BadUrl(_) => false,
        }
    }
}

/// Validators the server gave us, to send back next time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Validators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug)]
pub enum FetchOutcome {
    /// A 304: nothing to parse. The validators are still refreshed if the
    /// server sent new ones, and the feed is recorded as checked.
    NotModified { validators: Validators },
    Body {
        bytes: Vec<u8>,
        validators: Validators,
        /// Where the body actually came from, when a redirect was followed.
        /// Relative links in the feed resolve against this.
        final_url: Option<String>,
        /// The response's `Content-Type`, for its charset.
        content_type: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct FetchConfig {
    pub user_agent: String,
    pub timeout: Duration,
    pub max_body_bytes: u64,
}

impl Default for FetchConfig {
    fn default() -> Self {
        FetchConfig {
            user_agent: DEFAULT_USER_AGENT.to_string(),
            timeout: Duration::from_secs(30),
            // Feeds that big are broken or hostile; 16 MiB is generous.
            max_body_bytes: 16 * 1024 * 1024,
        }
    }
}

pub fn build_client(cfg: &FetchConfig) -> Result<Client, FetchError> {
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static(ACCEPT_FEEDS));
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(&cfg.user_agent).map_err(|e| FetchError::BadUrl(e.to_string()))?,
    );
    Client::builder()
        .default_headers(headers)
        .timeout(cfg.timeout)
        .build()
        .map_err(|e| FetchError::Network(e.to_string()))
}

pub async fn fetch(
    client: &Client,
    url: &str,
    known: &Validators,
    cfg: &FetchConfig,
) -> Result<FetchOutcome, FetchError> {
    let parsed = url::Url::parse(url).map_err(|e| FetchError::BadUrl(e.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(FetchError::BadUrl(format!(
            "unsupported scheme {}",
            parsed.scheme()
        )));
    }

    let mut req = client.get(parsed.clone());
    if let Some(etag) = known.etag.as_deref() {
        if let Ok(v) = HeaderValue::from_str(etag) {
            req = req.header(IF_NONE_MATCH, v);
        }
    }
    if let Some(lm) = known.last_modified.as_deref() {
        if let Ok(v) = HeaderValue::from_str(lm) {
            req = req.header(IF_MODIFIED_SINCE, v);
        }
    }

    let resp = req
        .send()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;

    let status = resp.status();
    let validators = Validators {
        etag: header_string(resp.headers(), ETAG),
        last_modified: header_string(resp.headers(), LAST_MODIFIED),
    };

    if status == StatusCode::NOT_MODIFIED {
        return Ok(FetchOutcome::NotModified { validators });
    }
    if !status.is_success() {
        return Err(FetchError::Status {
            status: status.as_u16(),
        });
    }

    if let Some(len) = resp.content_length() {
        if len > cfg.max_body_bytes {
            return Err(FetchError::TooLarge(len));
        }
    }

    let final_url = {
        let landed = resp.url().clone();
        (landed != parsed).then(|| landed.to_string())
    };

    let content_type = header_string(resp.headers(), reqwest::header::CONTENT_TYPE);

    // Counted as it arrives. Content-Length is absent on chunked responses and
    // on anything reqwest decompresses, so checking it alone let a small
    // gzip that unpacks to gigabytes fill memory before the size check ran.
    let mut resp = resp;
    let mut bytes = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?
    {
        bytes.extend_from_slice(&chunk);
        if bytes.len() as u64 > cfg.max_body_bytes {
            return Err(FetchError::TooLarge(bytes.len() as u64));
        }
    }

    Ok(FetchOutcome::Body {
        bytes,
        validators,
        final_url,
        content_type,
    })
}

fn header_string(headers: &HeaderMap, name: reqwest::header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}
