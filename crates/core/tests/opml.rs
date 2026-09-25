//! Parses, imports and round-trips OPML, including a real QuiteRSS export.

use snaprss_core::import::{export_opml, identify, import_any, opml, SourceKind};
use snaprss_core::Db;

/// A real QuiteRSS 0.19 export, trimmed. CRLF line endings, UTF-8 titles,
/// an HTML entity in one title, and a mix of http and https.
const QUITERSS_EXPORT: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\r\n",
    "<opml version=\"2.0\">\r\n",
    "    <head>\r\n",
    "        <title>QuiteRSS</title>\r\n",
    "        <dateModified>Thu Sep 17 20:31:00 2026</dateModified>\r\n",
    "    </head>\r\n",
    "    <body>\r\n",
    "        <outline text=\"It's The Tie!\" type=\"rss\" htmlUrl=\"https://itsthetie.com\" xmlUrl=\"https://itsthetie.com/feed/\"/>\r\n",
    "        <outline text=\"In Otherworlds\" type=\"rss\" htmlUrl=\"http://masterrite.github.io/\" xmlUrl=\"https://masterrite.github.io/atom.xml\"/>\r\n",
    "        <outline text=\"Existential Comics\" type=\"rss\" htmlUrl=\"https://existentialcomics.com\" xmlUrl=\"http://existentialcomics.com/rss.xml\"/>\r\n",
    "        <outline text=\"机核网\" type=\"rss\" htmlUrl=\"https://www.gcores.com\" xmlUrl=\"https://www.gcores.com/rss\"/>\r\n",
    "        <outline text=\"Nerd and Jock\" type=\"rss\" htmlUrl=\"https://www.webtoons.com/en/canvas/nerd-and-jock/list?title_no=135963\" xmlUrl=\"https://www.webtoons.com/en/challenge/nerd-and-jock/rss?title_no=135963\"/>\r\n",
    "        <outline text=\"The Medieverse: Tim's Realistic &quot;medieval&quot; FANTASY Blog\" type=\"rss\" htmlUrl=\"https://timothyrjeveland.com\" xmlUrl=\"https://timothyrjeveland.com/feed/\"/>\r\n",
    "    </body>\r\n",
    "</opml>\r\n",
);

const NESTED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<opml version="2.0">
  <head><title>Nested</title></head>
  <body>
    <outline text="Engineering">
      <outline text="Kernel Notes" type="rss" xmlUrl="https://kn.test/feed.xml" htmlUrl="https://kn.test"/>
      <outline text="Deeper">
        <outline text="Signal Path" type="rss" xmlUrl="https://sp.test/feed.xml"/>
      </outline>
    </outline>
    <outline text="Loose Feed" type="rss" xmlUrl="https://loose.test/feed.xml"/>
    <outline text="Empty Folder"></outline>
  </body>
</opml>"#;

fn tmp(name: &str, body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join(name);
    std::fs::write(&p, body).unwrap();
    (dir, p)
}

#[test]
fn parses_a_real_quiterss_export() {
    let roots = opml::parse(QUITERSS_EXPORT).unwrap();
    assert_eq!(roots.len(), 6, "all six outlines, flat");
    assert!(roots.iter().all(|o| o.is_feed()));

    let first = &roots[0];
    assert_eq!(first.title, "It's The Tie!");
    assert_eq!(
        first.xml_url.as_deref(),
        Some("https://itsthetie.com/feed/")
    );
    assert_eq!(first.html_url.as_deref(), Some("https://itsthetie.com"));

    // Non-ASCII survives.
    assert!(roots.iter().any(|o| o.title == "机核网"));

    // &quot; is decoded to a real quote, not left as an entity.
    let medieverse = roots
        .iter()
        .find(|o| o.title.contains("Medieverse"))
        .unwrap();
    assert!(
        medieverse.title.contains('"') && !medieverse.title.contains("&quot;"),
        "entity not decoded: {}",
        medieverse.title
    );

    // A query string in the URL is not mangled.
    let webtoons = roots.iter().find(|o| o.title == "Nerd and Jock").unwrap();
    assert!(webtoons
        .xml_url
        .as_deref()
        .unwrap()
        .contains("?title_no=135963"));
}

#[test]
fn imports_the_quiterss_export() {
    let (_d, path) = tmp("test.opml", QUITERSS_EXPORT);
    assert_eq!(identify(&path), SourceKind::Opml);

    let mut db = Db::open_in_memory().unwrap();
    let (kind, r) = import_any(&mut db, &path).unwrap();
    assert_eq!(kind, SourceKind::Opml);
    assert_eq!(r.feeds, 6);
    assert_eq!(r.folders, 0);
    assert_eq!(r.duplicates, 0);

    let roots = db.children(None).unwrap();
    assert_eq!(roots.len(), 6);
    // Order from the file is preserved.
    assert_eq!(roots[0].text.as_deref(), Some("It's The Tie!"));
    assert_eq!(roots[3].text.as_deref(), Some("机核网"));
    assert!(roots
        .iter()
        .all(|n| n.kind == snaprss_core::models::NodeKind::Feed));
}

#[test]
fn reimporting_is_a_no_op() {
    let (_d, path) = tmp("test.opml", QUITERSS_EXPORT);
    let mut db = Db::open_in_memory().unwrap();

    let (_, first) = import_any(&mut db, &path).unwrap();
    assert_eq!(first.feeds, 6);

    let (_, second) = import_any(&mut db, &path).unwrap();
    assert_eq!(second.feeds, 0, "nothing new");
    assert_eq!(
        second.duplicates, 6,
        "all six recognised as already subscribed"
    );
    assert_eq!(db.count("feeds").unwrap(), 6, "no duplicates in the tree");
}

#[test]
fn rebuilds_nested_folders() {
    let mut db = Db::open_in_memory().unwrap();
    let r = opml::import_str(&mut db, NESTED).unwrap();
    assert_eq!(r.feeds, 3);
    assert_eq!(r.folders, 3, "Engineering, Deeper, Empty Folder");

    let roots = db.children(None).unwrap();
    let names: Vec<_> = roots.iter().map(|n| n.text.clone().unwrap()).collect();
    assert_eq!(names, vec!["Engineering", "Loose Feed", "Empty Folder"]);

    let eng = &roots[0];
    assert_eq!(eng.kind, snaprss_core::models::NodeKind::Folder);
    let kids = db.children(Some(eng.id)).unwrap();
    assert_eq!(kids.len(), 2);
    assert_eq!(kids[0].text.as_deref(), Some("Kernel Notes"));
    assert_eq!(kids[1].text.as_deref(), Some("Deeper"));

    let deep = db.children(Some(kids[1].id)).unwrap();
    assert_eq!(deep.len(), 1);
    assert_eq!(deep[0].text.as_deref(), Some("Signal Path"));

    // An empty folder is kept rather than silently dropped.
    let empty = roots
        .iter()
        .find(|n| n.text.as_deref() == Some("Empty Folder"))
        .unwrap();
    assert!(db.children(Some(empty.id)).unwrap().is_empty());
}

#[test]
fn round_trips_through_export() {
    let mut a = Db::open_in_memory().unwrap();
    opml::import_str(&mut a, NESTED).unwrap();

    let xml = export_opml(&a).unwrap();
    assert!(xml.starts_with("<?xml"));
    assert!(xml.contains("<opml version=\"2.0\">"));

    let mut b = Db::open_in_memory().unwrap();
    let r = opml::import_str(&mut b, &xml).unwrap();
    assert_eq!(r.feeds, 3);
    assert_eq!(r.folders, 3);

    let shape = |db: &Db| -> Vec<String> {
        fn walk(db: &Db, parent: Option<i64>, depth: usize, out: &mut Vec<String>) {
            for n in db.children(parent).unwrap() {
                out.push(format!("{}{}", "  ".repeat(depth), n.text.clone().unwrap()));
                walk(db, Some(n.id), depth + 1, out);
            }
        }
        let mut v = Vec::new();
        walk(db, None, 0, &mut v);
        v
    };
    assert_eq!(shape(&a), shape(&b), "tree survives export and re-import");
}

#[test]
fn export_escapes_special_characters() {
    let db = Db::open_in_memory().unwrap();
    db.conn()
        .execute(
            "INSERT INTO feeds(kind, text, xml_url, html_url)
             VALUES(1, ?1, 'https://x.test/feed?a=1&b=2', 'https://x.test')",
            ["Ampersands & \"quotes\" <angles>"],
        )
        .unwrap();

    let xml = export_opml(&db).unwrap();
    assert!(xml.contains("&amp;"), "ampersand not escaped");
    assert!(xml.contains("&quot;"), "quote not escaped");
    assert!(xml.contains("&lt;angles&gt;"), "angles not escaped");
    assert!(!xml.contains("<angles>"), "raw angle brackets in output");

    // And it parses back to the original string.
    let mut db2 = Db::open_in_memory().unwrap();
    opml::import_str(&mut db2, &xml).unwrap();
    let name: String = db2
        .conn()
        .query_row("SELECT text FROM feeds", [], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "Ampersands & \"quotes\" <angles>");
}

#[test]
fn falls_back_to_title_then_host() {
    let xml = r#"<opml version="2.0"><body>
        <outline title="Only Title" type="rss" xmlUrl="https://a.test/f.xml"/>
        <outline type="rss" xmlUrl="https://b.test/f.xml"/>
    </body></opml>"#;
    let roots = opml::parse(xml).unwrap();
    assert_eq!(roots[0].title, "Only Title");
    assert_eq!(
        roots[1].title, "b.test",
        "no text or title, so use the host"
    );
}

#[test]
fn rejects_files_that_are_not_opml() {
    let (_d, p) = tmp("notes.txt", "just some prose, no markup at all");
    assert_eq!(identify(&p), SourceKind::Unknown);

    let mut db = Db::open_in_memory().unwrap();
    let err = import_any(&mut db, &p).unwrap_err().to_string();
    assert!(err.contains("neither"), "unhelpful error: {err}");
    assert_eq!(db.count("feeds").unwrap(), 0);
}

#[test]
fn an_rss_feed_is_not_an_opml_file() {
    // Both are XML with <body>-ish content; make sure the sniff is not fooled.
    let (_d, p) = tmp(
        "feed.xml",
        r#"<?xml version="1.0"?><rss version="2.0"><channel><title>x</title></channel></rss>"#,
    );
    assert_eq!(identify(&p), SourceKind::Unknown);
}

#[test]
fn survives_truncated_and_malformed_input() {
    for junk in [
        "<opml><body><outline text=\"unclosed\" xmlUrl=\"https://a.test/f\">",
        "<opml><body></body></opml>",
        "<opml version=\"2.0\"></opml>",
        "",
    ] {
        let mut db = Db::open_in_memory().unwrap();
        let _ = opml::import_str(&mut db, junk);
    }
}

#[test]
fn imports_alongside_existing_feeds() {
    let mut db = Db::open_in_memory().unwrap();
    db.conn()
        .execute(
            "INSERT INTO feeds(kind, text, xml_url) VALUES(1, 'Existing', 'https://pre.test/f.xml')",
            [],
        )
        .unwrap();
    // One of these is already subscribed.
    let xml = r#"<opml version="2.0"><body>
        <outline text="Existing again" type="rss" xmlUrl="https://pre.test/f.xml"/>
        <outline text="Brand new" type="rss" xmlUrl="https://new.test/f.xml"/>
    </body></opml>"#;
    let r = opml::import_str(&mut db, xml).unwrap();
    assert_eq!(r.feeds, 1);
    assert_eq!(r.duplicates, 1);
    assert_eq!(db.count("feeds").unwrap(), 2);
}

// ---------------------------------------------------------------------------
// found in review
// ---------------------------------------------------------------------------

#[test]
fn reimporting_a_file_with_folders_does_not_copy_the_folders() {
    let xml = r#"<?xml version="1.0"?><opml version="2.0"><body>
        <outline text="Tech"><outline text="Inner"><outline text="A" xmlUrl="https://a.test/f"/></outline></outline>
        <outline text="B" xmlUrl="https://b.test/f"/>
    </body></opml>"#;
    let mut db = Db::open_in_memory().unwrap();
    opml::import_str(&mut db, xml).unwrap();
    let before = db.count("feeds").unwrap();
    opml::import_str(&mut db, xml).unwrap();
    assert_eq!(db.count("feeds").unwrap(), before, "no second Tech or Inner");
}

#[test]
fn imported_items_go_after_what_is_already_there() {
    let mut db = Db::open_in_memory().unwrap();
    opml::import_str(
        &mut db,
        r#"<opml><body><outline text="A" xmlUrl="https://1.test/f"/><outline text="B" xmlUrl="https://2.test/f"/></body></opml>"#,
    )
    .unwrap();
    opml::import_str(&mut db, r#"<opml><body><outline text="C" xmlUrl="https://3.test/f"/></body></opml>"#).unwrap();
    let roots: Vec<_> = db.children(None).unwrap().into_iter().filter_map(|n| n.text).collect();
    assert_eq!(roots, ["A", "B", "C"]);
}

#[test]
fn a_bare_ampersand_or_html_entity_does_not_lose_the_feed() {
    let xml = r#"<opml><body>
        <outline text="Caf&eacute; News&nbsp;Daily" xmlUrl="https://x.test/rss?a=1&b=2"/>
    </body></opml>"#;
    let mut db = Db::open_in_memory().unwrap();
    let r = opml::import_str(&mut db, xml).unwrap();
    assert_eq!(r.feeds, 1, "{r:?}");
    let (text, url): (String, String) = db
        .conn()
        .query_row("SELECT text, xml_url FROM feeds", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!(url, "https://x.test/rss?a=1&b=2");
    assert!(text.contains("News Daily"), "{text}");
}

#[test]
fn export_drops_characters_xml_forbids() {
    let db = Db::open_in_memory().unwrap();
    db.conn()
        .execute("INSERT INTO feeds(kind, text, xml_url) VALUES(1, 'Bad\u{b}Title', 'https://x.test/f')", [])
        .unwrap();
    let xml = export_opml(&db).unwrap();
    assert!(!xml.contains('\u{b}'));
    assert!(opml::parse(&xml).is_ok(), "the export parses");
}

#[test]
fn sniffing_goes_by_the_root_element() {
    // An RSS feed that mentions <opml> in an item is still RSS.
    let (_d1, rss) = tmp(
        "feed.xml",
        r#"<?xml version="1.0"?><rss><channel><item><description>&lt;opml&gt; and <![CDATA[<opml>]]></description></item></channel></rss>"#,
    );
    assert_eq!(identify(&rss), SourceKind::Unknown);
    // A long comment before the root does not hide real OPML.
    let body = format!(
        "<?xml version=\"1.0\"?><!-- {} --><opml version=\"2.0\"><body><outline text=\"A\" xmlUrl=\"https://a.test/f\"/></body></opml>",
        "x".repeat(5000)
    );
    let (_d2, opml_file) = tmp("subs.opml", &body);
    assert_eq!(identify(&opml_file), SourceKind::Opml);
}
