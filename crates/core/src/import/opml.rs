//! OPML subscription lists.
//!
//! The format is loose in practice. What is relied on here:
//!
//! * An `<outline>` carrying `xmlUrl` is a feed.
//! * An `<outline>` without one is a folder, and may contain more outlines to
//!   any depth. Some exporters emit empty folders; they are kept.
//! * The display name is `text`, falling back to `title`, falling back to the
//!   host. QuiteRSS writes `text`; a few readers only write `title`.
//!
//! Importing is additive and never destructive: a feed whose `xmlUrl` is
//! already subscribed is counted as a duplicate and skipped, so re-importing
//! the same file twice is a no-op rather than a mess.

use std::collections::HashSet;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader;
use rusqlite::params;

use crate::db::{Db, DbError};
use crate::models::ImportReport;

#[derive(Debug, thiserror::Error)]
pub enum OpmlError {
    #[error("not valid XML: {0}")]
    Xml(String),
    #[error("no <opml> or <body> element; is this an OPML file?")]
    NotOpml,
    #[error("io: {0}")]
    Io(String),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// One parsed outline. Folders carry children; feeds carry a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outline {
    pub title: String,
    pub xml_url: Option<String>,
    pub html_url: Option<String>,
    pub description: Option<String>,
    pub children: Vec<Outline>,
}

impl Outline {
    pub fn is_feed(&self) -> bool {
        self.xml_url.is_some()
    }
}

fn attr_map(e: &quick_xml::events::BytesStart<'_>) -> Vec<(String, String)> {
    e.attributes()
        .flatten()
        .map(|a| {
            let key = String::from_utf8_lossy(a.key.as_ref()).to_ascii_lowercase();
            // A strict decode fails on a bare `&` in a URL ("?a=1&b=2") or an
            // HTML entity like `&nbsp;`, both common in hand-made OPML.
            // Dropping the attribute turned such a feed into an empty folder.
            let val = match a.unescape_value() {
                Ok(v) => v.to_string(),
                Err(_) => lenient_unescape(&String::from_utf8_lossy(&a.value)),
            };
            (key, val)
        })
        .collect()
}

/// Decode what can be decoded and keep the rest as written.
fn lenient_unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let decoded = tail[1..].find(';').filter(|&j| j <= 10).and_then(|j| {
            let name = &tail[1..=j];
            let ch = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ if name.starts_with("#x") || name.starts_with("#X") => {
                    u32::from_str_radix(&name[2..], 16).ok().and_then(char::from_u32)
                }
                _ if name.starts_with('#') => name[1..].parse().ok().and_then(char::from_u32),
                _ => None,
            }?;
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

fn get<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.trim().is_empty())
}

fn outline_from(attrs: &[(String, String)]) -> Outline {
    let xml_url = get(attrs, "xmlurl").map(str::to_string);
    let title = get(attrs, "text")
        .or_else(|| get(attrs, "title"))
        .map(str::to_string)
        .or_else(|| {
            // Neither present: fall back to the feed's host rather than
            // showing the user a row labelled "Untitled".
            xml_url
                .as_deref()
                .and_then(|u| url::Url::parse(u).ok())
                .and_then(|u| u.host_str().map(str::to_string))
        })
        .unwrap_or_else(|| "Untitled".to_string());

    Outline {
        title,
        xml_url,
        html_url: get(attrs, "htmlurl").map(str::to_string),
        description: get(attrs, "description").map(str::to_string),
        children: Vec::new(),
    }
}

/// Parse an OPML document into a tree of outlines.
pub fn parse(xml: &str) -> Result<Vec<Outline>, OpmlError> {
    let mut reader = Reader::from_str(xml);
    let cfg = reader.config_mut();
    cfg.trim_text(true);
    cfg.check_end_names = false;

    let mut saw_opml = false;
    let mut in_body = false;
    // Outlines currently open, innermost last.
    let mut stack: Vec<Outline> = Vec::new();
    let mut roots: Vec<Outline> = Vec::new();

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Err(e) => return Err(OpmlError::Xml(e.to_string())),
            Ok(Event::Eof) => break,

            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_ascii_lowercase();
                match name.as_str() {
                    "opml" => saw_opml = true,
                    "body" => in_body = true,
                    "outline" if in_body => stack.push(outline_from(&attr_map(&e))),
                    _ => {}
                }
            }

            Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_ascii_lowercase();
                if name == "outline" && in_body {
                    let node = outline_from(&attr_map(&e));
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(node),
                        None => roots.push(node),
                    }
                }
            }

            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_ascii_lowercase();
                match name.as_str() {
                    "body" => in_body = false,
                    "outline" if in_body => {
                        if let Some(node) = stack.pop() {
                            match stack.last_mut() {
                                Some(parent) => parent.children.push(node),
                                None => roots.push(node),
                            }
                        }
                    }
                    _ => {}
                }
            }

            _ => {}
        }
        buf.clear();
    }

    // An unclosed <outline> at EOF still carries feeds worth keeping.
    while let Some(node) = stack.pop() {
        match stack.last_mut() {
            Some(parent) => parent.children.push(node),
            None => roots.push(node),
        }
    }

    if !saw_opml && roots.is_empty() {
        return Err(OpmlError::NotOpml);
    }
    Ok(roots)
}

/// True if the bytes look like an OPML document. Cheap enough to call before
/// deciding which importer to hand a file to.
pub fn looks_like_opml(path: impl AsRef<Path>) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let text = decode(&bytes[..bytes.len().min(64 * 1024)]);
    match root_element(&text) {
        Some(root) => root == "opml",
        // No recognisable root in the first 64 KB: fall back to looking for
        // OPML's shape anywhere in it.
        None => {
            let t = text.to_ascii_lowercase();
            t.contains("<opml") || (t.contains("<body") && t.contains("<outline"))
        }
    }
}

/// The name of the document's first element, skipping the XML declaration,
/// comments, processing instructions and a doctype. Looking for `<opml`
/// anywhere took an RSS feed that mentions OPML in an item for an OPML file,
/// and a long comment before the root hid a real one.
fn root_element(text: &str) -> Option<String> {
    let mut rest = text.trim_start_matches('\u{feff}');
    loop {
        rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = &r[r.find("-->")? + 3..];
        } else if rest.starts_with("<?") || rest.starts_with("<!") {
            rest = &rest[rest.find('>')? + 1..];
        } else if let Some(r) = rest.strip_prefix('<') {
            let end = r.find(|c: char| c.is_whitespace() || c == '>' || c == '/')?;
            let name = &r[..end];
            let local = name.rsplit(':').next().unwrap_or(name);
            return Some(local.to_ascii_lowercase());
        } else {
            return None;
        }
    }
}

/// UTF-8, or UTF-16 when the file starts with its byte-order mark.
fn decode(bytes: &[u8]) -> String {
    let utf16 = |le: bool| {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xFF, 0xFE, ..] => utf16(true),
        [0xFE, 0xFF, ..] => utf16(false),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

pub fn import_file(db: &mut Db, path: impl AsRef<Path>) -> Result<ImportReport, OpmlError> {
    let bytes = std::fs::read(path).map_err(|e| OpmlError::Io(e.to_string()))?;
    // OPML is nominally UTF-8. Lossy rather than failing: one bad byte in a
    // title should not cost the user the other fifty feeds. UTF-16 with a
    // byte-order mark is decoded as such.
    let xml = decode(&bytes);
    // The declaration may still say UTF-16; the text is a Rust string now.
    let xml = xml.trim_start_matches('\u{feff}').to_string();
    import_str(db, &xml)
}

pub fn import_str(db: &mut Db, xml: &str) -> Result<ImportReport, OpmlError> {
    let roots = parse(xml)?;
    let mut report = ImportReport::default();

    let known: HashSet<String> = {
        let mut stmt = db
            .conn()
            .prepare("SELECT xml_url FROM feeds WHERE xml_url IS NOT NULL AND xml_url <> ''")
            .map_err(DbError::from)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(DbError::from)?;
        rows.filter_map(Result::ok).collect()
    };
    let mut seen = known;

    let tx = db.conn_mut().transaction().map_err(DbError::from)?;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let pre_max_id: i64 = tx
        .query_row("SELECT COALESCE(MAX(id), 0) FROM feeds", [], |r| r.get(0))
        .map_err(DbError::from)?;
    // After what is already at the top level, not interleaved with it.
    let root_offset: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(row_to_parent), -1) + 1 FROM feeds WHERE parent_id IS NULL",
            [],
            |r| r.get(0),
        )
        .map_err(DbError::from)?;
    insert_all(&tx, &roots, None, root_offset, &mut seen, &mut report, &now)?;
    // Re-importing a file with folders made a second, empty copy of each.
    super::quiterss::merge_duplicate_folders(&tx, pre_max_id).map_err(DbError::from)?;
    tx.commit().map_err(DbError::from)?;

    db.recompute_counters()?;
    Ok(report)
}

fn insert_all(
    tx: &rusqlite::Transaction<'_>,
    nodes: &[Outline],
    parent: Option<i64>,
    first_row: i64,
    seen: &mut HashSet<String>,
    report: &mut ImportReport,
    now: &str,
) -> Result<(), OpmlError> {
    for (this_row, node) in (first_row..).zip(nodes) {
        if let Some(url) = node.xml_url.as_deref() {
            // Outlines nested under a feed are not valid OPML but do occur.
            // They are kept, beside the feed, rather than dropped unseen.
            if !node.children.is_empty() {
                insert_all(tx, &node.children, parent, this_row, seen, report, now)?;
            }
            if !seen.insert(url.to_string()) {
                report.duplicates += 1;
                continue;
            }
            tx.execute(
                "INSERT INTO feeds (kind, parent_id, row_to_parent, text, title,
                                    description, xml_url, html_url, created)
                 VALUES (1, ?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7)",
                params![
                    parent,
                    this_row,
                    node.title,
                    node.description,
                    url,
                    node.html_url,
                    now
                ],
            )
            .map_err(DbError::from)?;
            report.feeds += 1;
        } else {
            tx.execute(
                "INSERT INTO feeds (kind, parent_id, row_to_parent, text, xml_url)
                 VALUES (0, ?1, ?2, ?3, '')",
                params![parent, this_row, node.title],
            )
            .map_err(DbError::from)?;
            let id = tx.last_insert_rowid();
            report.folders += 1;
            insert_all(tx, &node.children, Some(id), 0, seen, report, now)?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------- exporting

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            // Control characters XML 1.0 does not allow at all, even escaped.
            // One in a title (they come in with imported data) made the whole
            // file unreadable to a strict parser.
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{fffe}' || c == '\u{ffff}' => {}
            // Attributes are written double-quoted, so an apostrophe needs no
            // escaping and &apos; is the one XML entity some older readers
            // mishandle. Leave it alone.
            _ => out.push(c),
        }
    }
    out
}

/// Serialise the subscription tree as OPML 2.0.
///
/// Only structure and identity are written. Per-feed settings such as update
/// interval and reading mode are deliberately left out: OPML has no agreed
/// place for them, and inventing namespaced attributes other readers ignore
/// would make the file look richer than it is.
pub fn export(db: &Db) -> Result<String, DbError> {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<opml version=\"2.0\">\n    <head>\n",
    );
    out.push_str("        <title>SnapRSS</title>\n");
    out.push_str(&format!(
        "        <dateCreated>{}</dateCreated>\n",
        chrono::Utc::now().to_rfc2822()
    ));
    out.push_str("    </head>\n    <body>\n");
    write_level(db, None, 1, &mut out)?;
    out.push_str("    </body>\n</opml>\n");
    Ok(out)
}

fn write_level(
    db: &Db,
    parent: Option<i64>,
    depth: usize,
    out: &mut String,
) -> Result<(), DbError> {
    let pad = "    ".repeat(depth + 1);
    for node in db.children(parent)? {
        let title = xml_escape(node.text.as_deref().unwrap_or("Untitled"));
        match node.xml_url.as_deref().filter(|u| !u.is_empty()) {
            Some(xml_url) => {
                out.push_str(&format!(
                    "{pad}<outline text=\"{title}\" title=\"{title}\" type=\"rss\"",
                ));
                if let Some(h) = node.html_url.as_deref().filter(|h| !h.is_empty()) {
                    out.push_str(&format!(" htmlUrl=\"{}\"", xml_escape(h)));
                }
                out.push_str(&format!(" xmlUrl=\"{}\"/>\n", xml_escape(xml_url)));
            }
            None => {
                out.push_str(&format!(
                    "{pad}<outline text=\"{title}\" title=\"{title}\">\n"
                ));
                write_level(db, Some(node.id), depth + 1, out)?;
                out.push_str(&format!("{pad}</outline>\n"));
            }
        }
    }
    Ok(())
}

pub fn export_to_file(db: &Db, path: impl AsRef<Path>) -> Result<usize, OpmlError> {
    let xml = export(db)?;
    std::fs::write(path, &xml).map_err(|e| OpmlError::Io(e.to_string()))?;
    Ok(xml.len())
}
