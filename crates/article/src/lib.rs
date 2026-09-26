//! Turning a fetched web page into something the reading pane can draw.
//!
//! Three stages, in this order, and the order matters:
//!
//! 1. **Preprocess.** Resolve lazy-loading images (`data-src`, `srcset`) into a
//!    plain `src`. Sanitisation strips those attributes, so anything left to
//!    stage 3 is lost.
//! 2. **Extract.** `dom_smoothie` finds the article and discards navigation,
//!    comments and footers.
//! 3. **Sanitise.** `ammonia` reduces what survives to an allowlist.
//!
//! Extraction is not a security boundary. Readability will happily hand back a
//! `<script>` if the article body contained one, so stage 3 is not optional,
//! and it is applied to feed-supplied HTML too, which never goes through
//! stage 2.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use ammonia::Builder;
use dom_query::Document;

#[derive(Debug, thiserror::Error)]
pub enum ArticleError {
    #[error("no article found in the page")]
    NotFound,
    #[error("extract: {0}")]
    Extract(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Article {
    /// Sanitised HTML, safe to render.
    pub html: String,
    /// Plain text, for search and for the list preview.
    pub text: String,
    pub title: Option<String>,
    pub byline: Option<String>,
    pub site_name: Option<String>,
    /// Rough reading time in minutes, minimum 1.
    pub read_minutes: u32,
}

/// The tags the reading pane can draw. This list is the renderer's contract as
/// much as a security boundary: anything not here would be dropped at layout
/// time anyway, so it is dropped here instead.
const ALLOWED_TAGS: &[&str] = &[
    "p",
    "br",
    "hr",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "dl",
    "dt",
    "dd",
    "blockquote",
    "pre",
    "code",
    "em",
    "i",
    "strong",
    "b",
    "u",
    "s",
    "del",
    "ins",
    "mark",
    "sub",
    "sup",
    "a",
    "img",
    "figure",
    "figcaption",
    "picture",
    "source",
    "table",
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "th",
    "td",
    "caption",
    "colgroup",
    "col",
    "abbr",
    "cite",
    "q",
    "small",
    "span",
    "div",
    "time",
];

fn allowed_attributes() -> &'static HashMap<&'static str, HashSet<&'static str>> {
    static MAP: OnceLock<HashMap<&'static str, HashSet<&'static str>>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m: HashMap<&str, HashSet<&str>> = HashMap::new();
        m.insert("a", ["href", "title"].into_iter().collect());
        m.insert(
            "img",
            ["src", "alt", "title", "width", "height"]
                .into_iter()
                .collect(),
        );
        m.insert("time", ["datetime"].into_iter().collect());
        m.insert("td", ["colspan", "rowspan"].into_iter().collect());
        m.insert("th", ["colspan", "rowspan", "scope"].into_iter().collect());
        // language-* class names, so code blocks can be highlighted.
        m.insert("code", ["class"].into_iter().collect());
        m.insert("pre", ["class"].into_iter().collect());
        m
    })
}

fn sanitiser(base: Option<&url::Url>) -> Builder<'_> {
    let mut b = Builder::default();
    b.tags(ALLOWED_TAGS.iter().copied().collect())
        .generic_attributes(HashSet::new())
        .tag_attributes(
            allowed_attributes()
                .iter()
                .map(|(k, v)| (*k, v.clone()))
                .collect(),
        )
        .url_schemes(["http", "https", "mailto", "data"].into_iter().collect())
        // Links open in the user's browser; these are the standard protections.
        .link_rel(Some("noopener noreferrer nofollow"))
        // `data:` is allowed for inline images, which many feeds use, but not
        // for links: a data URL link is a whole document of its choosing.
        //
        // Compared as a browser reads the address: it drops tabs and line
        // breaks anywhere in it, and spaces and control characters around
        // it, so "da&#x09;ta:" is a data URL too.
        .attribute_filter(|element, attribute, value| {
            let scheme = || -> String {
                value
                    .trim_matches(|c: char| c <= ' ')
                    .chars()
                    .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
                    .take(5)
                    .collect()
            };
            if element == "a" && attribute == "href" && scheme().eq_ignore_ascii_case("data:") {
                None
            } else {
                Some(value.into())
            }
        })
        // Remove these entirely, content and all, rather than unwrapping them
        // and keeping the inner text.
        .clean_content_tags(
            ["script", "style", "iframe", "object", "embed", "form"]
                .into_iter()
                .collect(),
        );
    if let Some(u) = base {
        b.url_relative(ammonia::UrlRelative::RewriteWithBase(u.clone()));
    }
    b
}

/// Make lazy-loaded images visible to the rest of the pipeline.
///
/// Sites defer images with `data-src`, `data-original`, `data-lazy-src` or a
/// `srcset`, leaving `src` empty or pointing at a placeholder. Sanitisation
/// drops those attributes, so they have to be promoted to `src` first or the
/// article renders with blank boxes.
pub fn resolve_lazy_images(html: &str) -> String {
    let doc = Document::from(html);

    // `<noscript>` holds the image for browsers without script. The sanitiser
    // parses its content as text, so it came out as visible, escaped markup.
    // Where the page already has that image (lazily, beside it) the copy
    // goes; otherwise its content is kept as real markup.
    let srcs = |sel: &dom_query::Selection<'_>| -> Vec<String> {
        sel.select("img")
            .iter()
            .flat_map(|el| {
                ["src", "data-src", "data-original", "data-lazy-src", "data-url"]
                    .iter()
                    .filter_map(|a| el.attr(a).map(|v| v.trim().to_string()))
                    .filter(|v| !v.is_empty())
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    let everywhere = srcs(&doc.select("html"));
    for ns in doc.select("noscript").iter() {
        let inside = srcs(&ns);
        let elsewhere = |src: &String| {
            everywhere.iter().filter(|s| *s == src).count() > inside.iter().filter(|s| *s == src).count()
        };
        let inner = ns.inner_html().to_string();
        if inner.trim().is_empty() || inside.iter().any(elsewhere) {
            ns.remove();
        } else {
            ns.replace_with_html(inner);
        }
    }

    // `<picture>` first, because it is the case that loses the image entirely.
    //
    //   <picture>
    //     <source srcset="big.webp 1x, big2.webp 2x" type="image/webp">
    //     <img alt="...">          <- often has no src at all
    //   </picture>
    //
    // Sanitisation strips `srcset`, and the inner `<img>` frequently carries no
    // `src` because the browser was expected to pick one from the sources. The
    // result is an invisible image. Collapse the whole thing down to a single
    // `<img>` with a real `src`.
    for pic in doc.select("picture").iter() {
        let img = pic.select("img");
        let from_source = || {
            pic.select("source")
                .iter()
                .find_map(|s| {
                    s.attr("data-srcset")
                        .or_else(|| s.attr("srcset"))
                        .or_else(|| s.attr("src"))
                        .map(|v| v.to_string())
                })
                .and_then(|v| first_from_srcset(&v))
        };
        let current = img.attr("src").map(|s| s.to_string()).unwrap_or_default();
        // A real `src` stands. A placeholder is what the page's script would
        // have replaced: with a lazy URL if there is one, or from a source.
        let src = (!is_placeholder(&current))
            .then(|| current.clone())
            .or_else(|| lazy_src(&img))
            .or_else(from_source)
            .or_else(|| img.attr("srcset").and_then(|v| first_from_srcset(&v)))
            .or_else(|| (!current.trim().is_empty()).then_some(current));

        let Some(src) = src else {
            pic.remove();
            continue;
        };
        let alt = img.attr("alt").map(|a| a.to_string()).unwrap_or_default();
        pic.replace_with_html(format!(
            "<img src=\"{}\" alt=\"{}\">",
            html_attr_escape(&src),
            html_attr_escape(&alt)
        ));
    }

    for el in doc.select("img").iter() {
        let current = el.attr("src").map(|s| s.to_string()).unwrap_or_default();

        // A missing or placeholder `src` is replaced by what the page's
        // script would have put there. A real one is left alone.
        let replacement = if is_placeholder(&current) {
            lazy_src(&el).or_else(|| {
                el.attr("data-srcset")
                    .or_else(|| el.attr("srcset"))
                    .and_then(|v| first_from_srcset(&v))
            })
        } else {
            None
        };
        if let Some(r) = replacement {
            el.set_attr("src", &r);
        }

        // Runs whether or not anything was promoted. Leaving these behind would
        // keep a second, possibly stale URL in the markup, and on an image that
        // already had a real src that stale URL is the wrong one.
        for a in [
            "data-src",
            "data-original",
            "data-lazy-src",
            "data-actualsrc",
            "file",
            "data-srcset",
            "srcset",
        ] {
            el.remove_attr(a);
        }
    }

    doc.html().to_string()
}

fn html_attr_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

fn lazy_src(el: &dom_query::Selection<'_>) -> Option<String> {
    // The first one that is set to something: `data-src=""` beside a real
    // `data-original` is common.
    // `data-url` is Webtoon's: every comic panel is a blank until its script
    // copies that in.
    ["data-src", "data-original", "data-lazy-src", "data-actualsrc", "data-url", "file"]
        .iter()
        .filter_map(|a| el.attr(a).map(|v| v.trim().to_string()))
        .find(|v| !v.is_empty() && !v.starts_with("data:"))
}

/// A `src` that is only there until script replaces it: empty, any inline
/// `data:` image (GIF, PNG and SVG spacers are all common), or a file named
/// for the purpose.
fn is_placeholder(src: &str) -> bool {
    let s = src.trim().to_ascii_lowercase();
    let file = s.rsplit('/').next().unwrap_or(&s);
    s.is_empty()
        || s.starts_with("data:")
        // "transparen" covers transparent.gif and Webtoon's bg_transparency.png.
        || ["placeholder", "blank.", "spacer.", "loading", "lazy", "pixel.", "transparen", "1x1"]
            .iter()
            .any(|w| file.contains(w))
        // Discuz and others: a named stand-in GIF.
        || ["none.gif", "grey.gif", "gray.gif", "white.gif", "default.gif"].contains(&file)
}

/// The first candidate URL of a `srcset`: `"a.jpg 1x, b.jpg 2x"` gives
/// `"a.jpg"`. Per the HTML parsing rules the URL runs to the next whitespace,
/// so commas inside it survive: splitting on commas first cut
/// `.../w_400,h_300/a.jpg` and every `data:...;base64,...` in half.
fn first_from_srcset(value: &str) -> Option<String> {
    let url = value
        .trim_start_matches(|c: char| c.is_whitespace() || c == ',')
        .split_whitespace()
        .next()?
        .trim_end_matches(',');
    (!url.is_empty()).then(|| url.to_string())
}

/// Minutes to read `text`. Words for space-separated scripts; characters for
/// Chinese, Japanese and Korean, which have no spaces to count, so a long
/// Chinese article came out as one minute.
pub fn read_minutes(text: &str) -> u32 {
    let is_cjk = |c: char| {
        matches!(c as u32,
            0x3040..=0x30FF   // kana
            | 0x3400..=0x4DBF // CJK extension A
            | 0x4E00..=0x9FFF // CJK unified
            | 0xAC00..=0xD7AF // Hangul syllables
            | 0xF900..=0xFAFF)
    };
    let cjk = text.chars().filter(|&c| is_cjk(c)).count();
    let words = text
        .split(|c: char| c.is_whitespace() || is_cjk(c))
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .count();
    let minutes = words as f64 / 225.0 + cjk as f64 / 450.0;
    (minutes.ceil() as u32).max(1)
}

/// Full pipeline for a fetched page.
pub fn extract(html: &str, url: &str) -> Result<Article, ArticleError> {
    let base = url::Url::parse(url).ok();
    if let Some(article) = base.as_ref().and_then(|u| webtoon_episode(html, u)) {
        return Ok(article);
    }
    let prepared = resolve_lazy_images(html);

    let mut readability = dom_smoothie::Readability::new(prepared.as_str(), Some(url), None)
        .map_err(|e| ArticleError::Extract(e.to_string()))?;
    let parsed = readability
        .parse()
        .map_err(|e| ArticleError::Extract(e.to_string()))?;

    let text = parsed.text_content.trim().to_string();
    if text.is_empty() {
        return Err(ArticleError::NotFound);
    }

    let cleaner = sanitiser(base.as_ref());
    let html = cleaner.clean(&parsed.content).to_string();

    Ok(Article {
        read_minutes: read_minutes(&text),
        html,
        text,
        title: Some(parsed.title.to_string()).filter(|t| !t.trim().is_empty()),
        byline: parsed.byline.map(|b| b.to_string()),
        site_name: parsed.site_name.map(|s| s.to_string()),
    })
}

/// A Webtoon episode: its comic panels and the creator's note.
///
/// The page is pictures with almost no text, and readability, which looks
/// for text, picked the list of every episode's thumbnail instead. The panels
/// are blanks until the page's script copies each `data-url` in, and on
/// webtoon-phinf.pstatic.net they are refused to any request not sent from
/// webtoons.com. swebtoon-phinf.pstatic.net, the host Webtoon's own feeds use
/// for the same pictures, serves them to anyone.
fn webtoon_episode(html: &str, url: &url::Url) -> Option<Article> {
    let host = url.host_str()?;
    if host != "webtoons.com" && !host.ends_with(".webtoons.com") {
        return None;
    }
    let doc = Document::from(html);
    let panels: Vec<String> = doc
        .select("#_imageList img")
        .iter()
        .filter_map(|img| {
            let current = img.attr("src").map(|s| s.to_string()).unwrap_or_default();
            let src = if is_placeholder(&current) { lazy_src(&img)? } else { current };
            let mut u = url.join(src.trim()).ok()?;
            if u.host_str() == Some("webtoon-phinf.pstatic.net") {
                u.set_host(Some("swebtoon-phinf.pstatic.net")).ok()?;
            }
            Some(u.to_string())
        })
        .collect();
    if panels.is_empty() {
        return None;
    }
    let note = doc.select("._creatorNoteText").text().trim().to_string();
    let mut body: String = panels
        .iter()
        .map(|src| format!("<img src=\"{}\" alt=\"\">", html_attr_escape(src)))
        .collect();
    body = format!("<p>{body}</p>");
    if !note.is_empty() {
        body.push_str(&format!("<p>{}</p>", html_attr_escape(&note)));
    }
    let title = doc
        .select("h1.subj_episode")
        .attr("title")
        .map(|t| t.to_string())
        .or_else(|| Some(doc.select("title").text().trim().to_string()))
        .filter(|t| !t.is_empty());
    Some(Article {
        html: sanitiser(Some(url)).clean(&body).to_string(),
        read_minutes: 1,
        text: note,
        title,
        byline: None,
        site_name: Some("WEBTOON".to_string()),
    })
}

/// Sanitise HTML that came straight from a feed's `<description>` or
/// `<content:encoded>`. No extraction: the feed already said what the body is.
/// Still attacker-controlled, so it still gets cleaned.
pub fn sanitise_feed_html(html: &str, base_url: Option<&str>) -> String {
    let base = base_url.and_then(|u| url::Url::parse(u).ok());
    let prepared = resolve_lazy_images(html);
    let cleaner = sanitiser(base.as_ref());
    cleaner.clean(&prepared).to_string()
}

/// Put the feed's lead image back when extraction lost it.
///
/// Some sites render article images from script, so the fetched page holds an
/// empty placeholder where the picture goes and readability has nothing to
/// keep; others put it in a carousel that readability drops as boilerplate.
/// The feed summary usually still carries that image. If none of the extracted
/// images is the summary's first one, it goes back at the top.
///
/// Both inputs are already sanitised. Images are compared on host and path,
/// because the same picture is often served at different sizes through the
/// query string.
pub fn keep_lead_image(extracted: &str, summary: &str) -> String {
    fn key(src: &str) -> String {
        match url::Url::parse(src) {
            Ok(u) => format!("{}{}", u.host_str().unwrap_or(""), u.path()),
            Err(_) => src.split(['?', '#']).next().unwrap_or(src).to_string(),
        }
    }
    let lead = Document::from(summary)
        .select("img[src]")
        .iter()
        .filter_map(|el| {
            let src = el.attr("src")?.trim().to_string();
            (!src.is_empty()).then(|| (src, el.attr("alt").map(|a| a.to_string()).unwrap_or_default()))
        })
        .next();
    let Some((src, alt)) = lead else {
        return extracted.to_string();
    };
    let wanted = key(&src);
    let present = Document::from(extracted)
        .select("img[src]")
        .iter()
        .filter_map(|el| el.attr("src").map(|s| key(s.trim())))
        .any(|k| k == wanted);
    if present {
        return extracted.to_string();
    }
    format!(
        "<p><img src=\"{}\" alt=\"{}\"></p>{}",
        html_attr_escape(&src),
        html_attr_escape(&alt),
        extracted
    )
}

/// Elements after which running two pieces of text together would be wrong.
const BLOCK_TAGS: &[&str] = &[
    "p",
    "div",
    "li",
    "br",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
    "pre",
    "tr",
    "td",
    "th",
    "figcaption",
    "dt",
    "dd",
    "section",
    "article",
    "header",
    "footer",
];

/// Strip tags, for previews and search indexing.
///
/// A naive text extraction glues block elements together: `<p>One</p><p>two</p>`
/// becomes "Onetwo". Closing tags are given a space first so word boundaries
/// survive.
pub fn to_plain_text(html: &str) -> String {
    let mut spaced = html.to_string();
    for tag in BLOCK_TAGS {
        spaced = spaced.replace(&format!("</{tag}>"), &format!("</{tag}> "));
        spaced = spaced.replace(&format!("<{tag}>"), &format!(" <{tag}>"));
    }
    Document::from(spaced.as_str())
        .text()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// character sets
// ---------------------------------------------------------------------------

/// The encoding a `Content-Type` header names, if it names one we know.
pub fn charset_from_content_type(content_type: &str) -> Option<&'static encoding_rs::Encoding> {
    let lower = content_type.to_ascii_lowercase();
    let at = lower.find("charset=")? + "charset=".len();
    let label = lower[at..]
        .trim_start_matches(['"', '\''])
        .split(|c: char| c == ';' || c == '"' || c == '\'' || c.is_whitespace())
        .next()?;
    encoding_rs::Encoding::for_label(label.as_bytes())
}

/// The charset a page declares in a `<meta>` near its top.
fn charset_from_meta(bytes: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let head = &bytes[..bytes.len().min(4096)];
    let text: String = head.iter().map(|&b| (b as char).to_ascii_lowercase()).collect();
    let mut from = 0;
    while let Some(i) = text[from..].find("<meta") {
        let start = from + i;
        let end = text[start..].find('>').map_or(text.len(), |e| start + e);
        let tag = &text[start..end];
        if let Some(e) = charset_from_content_type(tag) {
            return Some(e);
        }
        from = end;
    }
    None
}

/// Decode a fetched web page to text: byte-order mark, then the HTTP
/// `Content-Type` charset, then `<meta charset>`, then UTF-8.
///
/// Decoding everything as UTF-8 turned GBK, Big5 and Shift_JIS pages into
/// replacement characters, and the result was cached for good.
pub fn decode_html(bytes: &[u8], content_type: Option<&str>) -> String {
    let encoding = encoding_rs::Encoding::for_bom(bytes)
        .map(|(e, _)| e)
        .or_else(|| content_type.and_then(charset_from_content_type))
        .or_else(|| charset_from_meta(bytes))
        .unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = encoding.decode(bytes);
    text.into_owned()
}

/// Decode the entities a title may still carry after XML parsing: feeds
/// escape HTML in titles twice ("AT&amp;amp;T" arrives as "AT&amp;T"), and
/// `&nbsp;` and friends appear as they are. Tags are left alone; a plain-text
/// title can legitimately say "use <div>".
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let decoded = tail[1..].find(';').filter(|&j| (1..=10).contains(&j)).and_then(|j| {
            let name = &tail[1..=j];
            let ch = match name {
                "amp" => '&',
                "lt" => '<',
                "gt" => '>',
                "quot" => '"',
                "apos" => '\'',
                "nbsp" => ' ',
                "hellip" => '…',
                "mdash" => '—',
                "ndash" => '–',
                "lsquo" => '‘',
                "rsquo" => '’',
                "ldquo" => '“',
                "rdquo" => '”',
                "laquo" => '«',
                "raquo" => '»',
                "middot" => '·',
                "copy" => '©',
                "reg" => '®',
                "trade" => '™',
                _ if name.starts_with("#x") || name.starts_with("#X") => {
                    u32::from_str_radix(&name[2..], 16).ok().and_then(char::from_u32)?
                }
                _ if name.starts_with('#') => name[1..].parse().ok().and_then(char::from_u32)?,
                _ => return None,
            };
            Some((ch, j + 2))
        });
        match decoded {
            Some((ch, len)) => {
                out.push(ch);
                rest = &tail[len..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Escape plain text for use as HTML, keeping its line breaks.
pub fn text_to_html(s: &str) -> String {
    let escaped = s
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    escaped
        .split("\n\n")
        .map(|p| format!("<p>{}</p>", p.trim().replace('\n', "<br>")))
        .collect::<Vec<_>>()
        .join("")
}

/// A feed a web page advertises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedLink {
    pub url: String,
    pub title: Option<String>,
}

/// The feeds a page names in `<link rel="alternate">`: RSS, Atom and JSON
/// Feed, resolved against `base`, in page order, without repeats. How
/// browsers and QuiteRSS find a site's feed from its address.
pub fn feed_links(html: &str, base: &str) -> Vec<FeedLink> {
    let doc = Document::from(html);
    let base_url = document_base(&doc, base);
    let mut out: Vec<FeedLink> = Vec::new();
    for link in doc.select("link[href]").iter() {
        let rel = link.attr("rel").map(|r| r.to_ascii_lowercase()).unwrap_or_default();
        if !rel.split_whitespace().any(|r| r == "alternate" || r == "feed") {
            continue;
        }
        let ty = link.attr("type").map(|t| t.to_ascii_lowercase()).unwrap_or_default();
        let is_feed = ty.contains("rss") || ty.contains("atom") || ty.contains("feed+json")
            || (ty.is_empty() && rel.split_whitespace().any(|r| r == "feed"));
        if !is_feed {
            continue;
        }
        let Some(href) = link.attr("href").map(|h| h.trim().to_string()).filter(|h| !h.is_empty()) else {
            continue;
        };
        let resolved = match &base_url {
            Some(b) => b.join(&href).map(|u| u.to_string()).ok(),
            None => url::Url::parse(&href).map(|u| u.to_string()).ok(),
        };
        let Some(url) = resolved.filter(|u| u.starts_with("http://") || u.starts_with("https://")) else {
            continue;
        };
        if out.iter().any(|f| f.url == url) {
            continue;
        }
        let title = link.attr("title").map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        out.push(FeedLink { url, title });
    }
    out
}

/// What relative links in `doc` resolve against: `base`, the page's address,
/// unless a `<base href>` changes it.
fn document_base(doc: &Document, base: &str) -> Option<url::Url> {
    let base_url = url::Url::parse(base).ok();
    doc.select("base[href]")
        .attr("href")
        .and_then(|h| match &base_url {
            Some(b) => b.join(&h).ok(),
            None => url::Url::parse(&h).ok(),
        })
        .or(base_url)
}

/// The icons a page names, best first: `rel="icon"` (small PNG, ICO or SVG
/// preferred over large or unsized ones), then `apple-touch-icon`. Resolved
/// against `base`, or the page's `<base href>` as [`feed_links`] does: an icon
/// resolved against the page alone pointed somewhere else and was not found.
pub fn icon_links(html: &str, base: &str) -> Vec<String> {
    let doc = Document::from(html);
    let Some(base_url) = document_base(&doc, base) else { return Vec::new() };
    let mut scored: Vec<(i32, usize, String)> = Vec::new();
    for (i, link) in doc.select("link[href]").iter().enumerate() {
        let rel = link.attr("rel").map(|r| r.to_ascii_lowercase()).unwrap_or_default();
        let rels: Vec<&str> = rel.split_whitespace().collect();
        let touch = rels.iter().any(|r| r.starts_with("apple-touch-icon"));
        if !rels.contains(&"icon") && !touch {
            continue;
        }
        let Some(href) = link.attr("href").map(|h| h.trim().to_string()).filter(|h| !h.is_empty()) else {
            continue;
        };
        let Ok(u) = base_url.join(&href) else { continue };
        if !matches!(u.scheme(), "http" | "https") {
            continue;
        }
        // Smaller is better up to 32px; unsized comes after sized-small.
        let size = link
            .attr("sizes")
            .and_then(|s| s.split(['x', 'X']).next().and_then(|n| n.trim().parse::<i32>().ok()));
        let mut score = match size {
            Some(n) if (16..=64).contains(&n) => (n - 32).abs() / 16,
            None => 3,
            Some(n) => 5 + n / 64,
        };
        if touch {
            score += 20;
        }
        scored.push((score, i, u.to_string()));
    }
    scored.sort();
    let mut out: Vec<String> = Vec::new();
    for (_, _, u) in scored {
        if !out.contains(&u) {
            out.push(u);
        }
    }
    out
}
