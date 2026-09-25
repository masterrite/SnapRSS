# SnapRSS

A local feed reader. Apache-2.0; see `LICENSE` and `NOTICE`.

## Layout

    crates/core/      storage, import, OPML, retention, filters        95 tests
    crates/fetch/     conditional GET, scheduling, ingestion           35 tests
    crates/article/   extraction and sanitisation                      23 tests
    crates/app/       the Tauri shell and the frontend                  3 tests

    cargo test        # 156 tests, no live network needed

The first three build and test headlessly. Only `crates/app` needs a windowing
system.

## Running the app

    cargo install tauri-cli --version "^2.0.0" --locked
    cd crates/app && cargo tauri dev

See `crates/app/README.md` for platform dependencies and bundling.

## Design notes

**No embedded browser.** The reading pane renders a sanitised subset of HTML;
anything needing more opens in your browser.

**Conditional GET.** Every poll sends `If-None-Match` and `If-Modified-Since`
from `feeds.http_etag` / `http_last_modified`. A 304 does no parsing and no
writing beyond a timestamp. The tests assert those headers are sent, against a
mock server.

**Entry identity**, in order, scoped to a single feed:

1. `guid` exact match
2. link **and** title
3. link **and** publication date
4. title **and** publication date

A bare link match is not sufficient: some feeds point every item at the same
page, and collapsing those loses articles. Two feeds carrying the same
syndicated article remain two articles; cross-feed collapsing is a setting, not
an insert rule.

Rules 2 to 4 never match a stored row whose guid differs from the entry's: the
feed has said they are different articles (a weekly post at a fixed URL under
a fixed title). An item with no guid, link or title at all is matched on its
text and date. Ids the feed did not supply stay empty: feed-rs would invent
one, a random UUID when there is nothing to hash, which made such items new on
every poll.

**Re-polling** preserves read and starred state. If the source text changed the
row is updated in place and the cached extracted article is invalidated.

**Backoff.** Consecutive failures double the interval, capped at 24 hours.
`updated` advances even on failure, or a dead host is due on every tick and the
backoff never applies.

**Images are fixed before sanitising.** `data-src` and `srcset` are resolved to
a plain `src` first, because the sanitiser strips them. `<picture>` is
flattened to one `<img>` for the same reason: `<source srcset>` does not
survive the allowlist and the inner `<img>` often has no `src`.

**Retention has two halves.** Cleanup sets `deleted = 1`; the article leaves
every view but Deleted and can be restored. That runs hourly. Purge, on
request only, keeps a stub with `deleted = 2`: the identity columns and
nothing else. Deleting the row outright removed the only record that the
article had been seen, and every one still in its feed came back new and
unread on the next poll. QuiteRSS keeps the same stub with the same value.

**One feed's failure is that feed's.** Writing the results of an update, an
error on one feed is recorded as that feed's status and the rest carry on.
Propagating it discarded every other feed's results, on every poll.

Protections beat rules: they are SQL predicates on every rule, not a separate
filter. A rule and its protections travel together — a feed with retention
rules of its own owns its protections, a feed without follows the global ones.
Without that the global switches are dead, since `feeds` defaults every
protection column to 1 and nothing distinguishes "the user chose this" from
"untouched".

**Filters run on insert.** A rule that deletes an arriving article runs in the
same transaction as its `INSERT`, so the article is never briefly visible or
counted as unread. Only new rows are considered; re-running filters on every
poll would undo read and starred states set by hand.

Conditions are predicates evaluated in Rust, not a concatenated `WHERE` clause,
so nothing the user types reaches SQL and the engine can judge a row that does
not exist yet.

Filters run top to bottom and a later one sees what earlier ones did.

**QuiteRSS's filter encoding.** An imported filter stores every field, operator
and action as a combo box index, and the operator list differs per field: index
2 is "is" for Title and "regular expression" for Description, which offers only
three. One flat table turns a substring rule into a regex one. The per-field
table is transcribed from `itemcondition.cpp`; the numbering is asserted
directly, plus one test that imports a filter and runs it.

Other QuiteRSS encodings converted on import: `read = 2` (its ordinary
"read") becomes 1; timestamps without a zone get their `Z`; interval units
`-1`/`0`/`1` become seconds/minutes/hours; an "Add label" action's label id is
renumbered with the labels; numbers stored as text are read as numbers.
Databases imported before these conversions are repaired when opened.
Importing the same file twice adds nothing: subscribed feeds, same-named
labels and filters are skipped, and folders merge into same-named ones.

Two QuiteRSS behaviours are not copied. Its "News doesn't contain" emits
`(title NOT LIKE x OR description NOT LIKE x)`, true whenever either half lacks
the word; here the negation is over the pair. And its link comparisons are
case-sensitive.

**The database lock is never held across the network.** Fetching and writing
are separate phases: `fetch_all` touches no database, `apply_fetched` does no
network. The obvious shape hands `&mut Db` to the fetcher, which freezes every
other command for as long as the slowest server takes, once a minute. Opening
an article works the same way — it returns the cached text or the feed summary
at once and extracts the real article in the background.

**Colour never rides on a style attribute.** Tauri rewrites the CSP and puts a
nonce on `style-src`; a nonce makes `'unsafe-inline'` inert for style
attributes as well as style elements, so anything written as
`style="background:…"` inside `innerHTML` is dropped by the webview. Runtime
colours travel on `data-bg` and are applied from script, which CSP does not
govern; a static check keeps them from coming back. `hsl()` uses commas for a
similar reason — older WebKitGTK drops the space-separated form.

**Toolbars are reordered, never rebuilt.** Customisation moves the existing
buttons with `appendChild` and toggles `hidden`. Rebuilding from a registry
drops every handler bound at startup, so a test clicks a button after moving
it.

**Character sets.** Feeds that declare their encoding only in the HTTP
header, and article pages in GBK, Big5 or Shift_JIS, are decoded in that
encoding; UTF-8 was assumed and Chinese titles and pages came out as
replacement characters. Dates: "CST" in a Chinese feed is China Standard
Time (feed-rs reads US Central, 14 hours off), dates without a zone such as
`2024-01-15 10:30:00` are read, and a date in the future is taken as now.

**Bodies are counted as they arrive.** The 16 MiB cap applies to the bytes
after decompression, checked per chunk; a small gzip that unpacks to
gigabytes used to be read whole first. The Windows system proxy and
certificate store are used.

**Speed on a large database.** Measured with 300,000 articles in 150 feeds
(`crates/app/src/bench.rs`, run with `SNAPRSS_BENCH_DB=<path> cargo test -p
snaprss-app --release bench -- --ignored --nocapture`): every list, the tree,
the counts and starring or marking one article take under 40 ms, and a poll of
all 150 feeds writes in under a second. Two things made that true: unread
counts are recounted for the feeds that changed, not for every feed after
every change, and the list and count queries are served by indexes (a test
checks their query plans). Before, marking one article read took 300 ms and a
poll 47 seconds, with the database locked throughout. Marking 100,000 unread
articles read at once still takes about five seconds; that is SQLite
rewriting 100,000 rows.

**Images are requested without a Referer.** Hotlink-protected hosts
(image.gcores.com among them) answer 403 to the webview's `tauri.localhost`
Referer and serve the same request without one. And when extraction loses the
feed's lead image — rendered by script, or dropped with a carousel — it is put
back at the top of the article when the article is opened.

**Sanitisation is separate from extraction.** Readability returns a `<script>`
if the body contained one, so `ammonia` runs on everything, including
feed-supplied `<description>` HTML that skips extraction. The allowlist is also
the renderer's contract.

**Safety.** Only `http` and `https` feed URLs are accepted, so an OPML cannot
point at `file:///`. Bodies are capped at 16 MiB, checked against both
`Content-Length` and the actual byte count.

## Article extraction

`dom_smoothie` (MIT), not `article_scraper` (GPL-3.0-or-later). Measured on six
real pages, 20 runs each, release build, plain text against plain text:

    page        raw text  smoothie   scraper
    danluu         19671    1.49ms    4.97ms
    lwn             1736    0.60ms    3.33ms
    mdn            72276    6.33ms   21.25ms
    nasa           74184    9.55ms   36.23ms
    rustblog        6242    1.28ms    5.44ms
    wikipedia      59984   13.23ms   65.70ms
    total                 32.48ms  136.92ms   (4.2x slower)

Output was equivalent. dom_smoothie is pure Rust; article_scraper needs
libxml2, libclang and clang at build time, which is awkward on Windows.

The cost is article_scraper's ftr-site-config per-site rules, which beat
generic readability on hostile pages. Extraction lives in its own crate so it
stays swappable.

## Importing and exporting

One **Import** button. The file is sniffed by content, not extension:

* **OPML** — subscriptions and folders, nested to any depth. What QuiteRSS
  writes from Feeds → Export Feeds.
* **QuiteRSS database** — feeds, folders, articles, read and starred state,
  labels, filters, saved passwords.

Import is additive. A feed whose `xmlUrl` is already subscribed is counted and
skipped, so importing the same file twice is a no-op.

**Export** writes OPML 2.0, structure and identity only. Per-feed settings like
update interval and reading mode are left out: OPML has no agreed place for
them.

## On QuiteRSS

SnapRSS can import a QuiteRSS `feeds.db`. No QuiteRSS source is included or
copied; the schema here is its own.

## Verification status

`cargo test` covers every crate: 156 tests, no network needed.
`crates/app/ui-test` runs the frontend in jsdom and drives every button: 349
assertions.

**Two of these only break on Windows.** Tauri intercepts drag and drop at the
OS level unless `dragDropEnabled` is false, which kills HTML5 drag and drop
inside the page; and Chromium will not start a drag from a form control, so
tree rows are `<div role="button">` rather than `<button>`. WebKitGTK does
neither, so both passed every check here and failed in WebView2. Both are now
static assertions.

The app has been built and run under Xvfb against real feed data. Toolbars,
context menus, star state, article rendering, retention, "open in browser", the
newspaper layout, toolbar customisation and drag and drop were checked in a
running window; that is how the CSP colour bug was found, since it passes in
jsdom. Not built on Windows or macOS.

## Next

    per-feed toolbar layouts
    label colours in the reading pane header
    a filter's "apply to this feed only" from the feed context menu
    undo for unsubscribing a feed
