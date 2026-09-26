-- SnapRSS schema v1
--
-- Derived from the QuiteRSS schema (src/database/database.cpp, GPL-3.0-or-later).
-- Concepts and most column names are kept so the importer maps one-to-one and a
-- QuiteRSS feeds.db can be read side by side while debugging. Dropped from the
-- original: rssCloud columns, skipHours/skipDays, docs, ttl, webMaster,
-- contributor, typeFeed, the newspaper/tab-close read timers, and the three
-- click-action columns. Added: a folders/feeds discriminator, ETag and
-- Last-Modified for conditional GET, and reading_mode.

PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- Tree of folders and feeds. kind = 0 folder, 1 feed.
CREATE TABLE IF NOT EXISTS feeds (
    id                      INTEGER PRIMARY KEY,
    kind                    INTEGER NOT NULL DEFAULT 1,
    parent_id               INTEGER REFERENCES feeds(id) ON DELETE CASCADE,
    row_to_parent           INTEGER NOT NULL DEFAULT 0,
    expanded                INTEGER NOT NULL DEFAULT 1,

    -- identity
    text                    TEXT,              -- user-visible name, may be renamed
    title                   TEXT,              -- title as the feed reports it
    description             TEXT,
    xml_url                 TEXT,              -- the feed itself
    html_url                TEXT,              -- the site
    language                TEXT,
    image                   BLOB,              -- favicon, base64 as QuiteRSS stores it
    icon_checked            TEXT,              -- when an icon was last looked for

    -- counters, denormalised on purpose: the tree redraws constantly
    unread                  INTEGER NOT NULL DEFAULT 0,
    new_count               INTEGER NOT NULL DEFAULT 0,
    undelete_count          INTEGER NOT NULL DEFAULT 0,
    current_news            INTEGER,

    -- updating
    update_interval_enable  INTEGER NOT NULL DEFAULT 0,
    update_interval         INTEGER,
    update_interval_type    TEXT,              -- 'minutes' | 'hours' | 'days'
    update_on_startup       INTEGER NOT NULL DEFAULT 1,
    disable_update          INTEGER NOT NULL DEFAULT 0,
    display_on_startup      INTEGER NOT NULL DEFAULT 0,

    -- conditional GET; QuiteRSS had no equivalent
    http_etag               TEXT,
    http_last_modified      TEXT,

    -- reading. 0 = full article (sanitised), 1 = feed description only.
    reading_mode            INTEGER NOT NULL DEFAULT 0,
    open_on_enter           INTEGER NOT NULL DEFAULT 1,  -- Enter opens in browser
    load_images             INTEGER NOT NULL DEFAULT 1,
    embedded_images         INTEGER NOT NULL DEFAULT 1,
    layout_direction        INTEGER NOT NULL DEFAULT 0,  -- 0 ltr, 1 rtl
    save_offline            INTEGER NOT NULL DEFAULT 0,
    mark_read_after_enable  INTEGER NOT NULL DEFAULT 0,
    mark_read_after_seconds INTEGER,
    mark_read_on_switch     INTEGER NOT NULL DEFAULT 0,

    -- list presentation, per feed
    layout                  TEXT,              -- 'classic' | 'newspaper'
    density                 TEXT,              -- 'relaxed' | 'compact' | null = inherit
    filter                  TEXT,
    group_by                INTEGER,
    columns                 TEXT,              -- ordered column keys, tab separated
    sort                    TEXT,
    sort_type               INTEGER,

    -- retention
    max_to_keep             INTEGER,
    max_to_keep_enable      INTEGER NOT NULL DEFAULT 0,
    max_age_days            INTEGER,
    max_age_enable          INTEGER NOT NULL DEFAULT 0,
    delete_read             INTEGER NOT NULL DEFAULT 0,
    never_delete_unread     INTEGER NOT NULL DEFAULT 1,
    never_delete_starred    INTEGER NOT NULL DEFAULT 1,
    never_delete_labeled    INTEGER NOT NULL DEFAULT 1,

    -- duplicate handling and import cutoff
    duplicate_news_mode     INTEGER NOT NULL DEFAULT 0,
    add_any_date            INTEGER NOT NULL DEFAULT 1,
    avoid_old_enable        INTEGER NOT NULL DEFAULT 0,
    avoid_old_before        TEXT,

    -- status
    status                  TEXT,              -- last update result, empty = ok
    consecutive_failures    INTEGER NOT NULL DEFAULT 0,  -- drives fetch backoff
    authentication          INTEGER NOT NULL DEFAULT 0,
    show_notification       INTEGER NOT NULL DEFAULT 0,
    created                 TEXT,
    updated                 TEXT,
    last_displayed          TEXT
);

CREATE INDEX IF NOT EXISTS idx_feeds_parent ON feeds(parent_id, row_to_parent);
CREATE INDEX IF NOT EXISTS idx_feeds_xml_url ON feeds(xml_url);

CREATE TABLE IF NOT EXISTS news (
    id              INTEGER PRIMARY KEY,
    feed_id         INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE,
    feed_parent_id  INTEGER NOT NULL DEFAULT 0,

    guid            TEXT,
    guid_is_link    INTEGER NOT NULL DEFAULT 1,
    title           TEXT,
    description     TEXT,               -- summary from the feed
    content         TEXT,               -- full content if the feed carried it
    article_html    TEXT,               -- extracted + sanitised, filled lazily
    article_fetched TEXT,               -- when article_html was produced

    published       TEXT,
    modified        TEXT,
    received        TEXT NOT NULL,

    author_name     TEXT,
    author_uri      TEXT,
    author_email    TEXT,
    category        TEXT,

    link_href       TEXT,
    link_alternate  TEXT,
    comments        TEXT,
    source          TEXT,
    rights          TEXT,

    enclosure_url   TEXT,
    enclosure_type  TEXT,
    enclosure_length INTEGER,

    new             INTEGER NOT NULL DEFAULT 1,
    read            INTEGER NOT NULL DEFAULT 0,
    starred         INTEGER NOT NULL DEFAULT 0,
    deleted         INTEGER NOT NULL DEFAULT 0,
    delete_date     TEXT
);

CREATE INDEX IF NOT EXISTS idx_news_feed ON news(feed_id, deleted, published DESC);
-- Covered by idx_news_unread_date, and one index less to update when
-- articles are marked read.
DROP INDEX IF EXISTS idx_news_unread;
-- Starred articles, in list order. The earlier index on `starred` alone was
-- passed over for a full scan once the table had statistics.
DROP INDEX IF EXISTS idx_news_starred;
CREATE INDEX IF NOT EXISTS idx_news_starred_date
    ON news(deleted, COALESCE(published, received)) WHERE starred = 1;
CREATE INDEX IF NOT EXISTS idx_news_guid ON news(feed_id, guid);
-- The identity rules for articles without a matching guid look up by link
-- and by title within the feed.
CREATE INDEX IF NOT EXISTS idx_news_link ON news(feed_id, link_href);
CREATE INDEX IF NOT EXISTS idx_news_title ON news(feed_id, title);
-- Unread and article counts per feed from the index alone. Without it every
-- count read every row, 300 ms at 300,000 articles, and counts are redone
-- after every change.
CREATE INDEX IF NOT EXISTS idx_news_counts ON news(feed_id, deleted, read);
-- The lists sort newest first by this expression; indexed, the newest 500
-- are read in order instead of every match being sorted.
CREATE INDEX IF NOT EXISTS idx_news_date ON news(deleted, COALESCE(published, received));
CREATE INDEX IF NOT EXISTS idx_news_unread_date ON news(deleted, read, COALESCE(published, received));

CREATE TABLE IF NOT EXISTS labels (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    image       BLOB,
    color_text  TEXT,
    color_bg    TEXT,
    num         INTEGER NOT NULL DEFAULT 0
);

-- QuiteRSS stored labels as a comma-joined string in news.label. A join table
-- is the same data without the substring matching.
CREATE TABLE IF NOT EXISTS news_labels (
    news_id  INTEGER NOT NULL REFERENCES news(id) ON DELETE CASCADE,
    label_id INTEGER NOT NULL REFERENCES labels(id) ON DELETE CASCADE,
    PRIMARY KEY (news_id, label_id)
);

CREATE INDEX IF NOT EXISTS idx_news_labels_label ON news_labels(label_id);

-- type follows QuiteRSS's combo box, because the importer copies it verbatim:
--   0 every article in scope, conditions ignored
--   1 match all conditions
--   2 match any condition
-- feeds: NULL = every feed. Otherwise a comma-wrapped list, ",3,7,12,", which
-- is QuiteRSS's format. An empty list means no feed, not every feed.
CREATE TABLE IF NOT EXISTS filters (
    id      INTEGER PRIMARY KEY,
    name    TEXT NOT NULL,
    type    INTEGER NOT NULL DEFAULT 0,
    feeds   TEXT,
    enable  INTEGER NOT NULL DEFAULT 1,
    num     INTEGER NOT NULL DEFAULT 0    -- order; later filters see earlier effects
);

CREATE INDEX IF NOT EXISTS idx_filters_order ON filters(enable, num);

-- field and condition are stored as names by this application ('title',
-- 'contains') and as QuiteRSS combo box indices by anything imported. The
-- indices are read per field, because each field offers a different operator
-- list -- see crates/core/src/filters.rs.
CREATE TABLE IF NOT EXISTS filter_conditions (
    id        INTEGER PRIMARY KEY,
    filter_id INTEGER NOT NULL REFERENCES filters(id) ON DELETE CASCADE,
    field     TEXT NOT NULL,
    condition TEXT NOT NULL,
    content   TEXT
);

CREATE INDEX IF NOT EXISTS idx_filter_conditions ON filter_conditions(filter_id);

CREATE TABLE IF NOT EXISTS filter_actions (
    id        INTEGER PRIMARY KEY,
    filter_id INTEGER NOT NULL REFERENCES filters(id) ON DELETE CASCADE,
    action    TEXT NOT NULL,
    params    TEXT
);

CREATE INDEX IF NOT EXISTS idx_filter_actions ON filter_actions(filter_id);

CREATE TABLE IF NOT EXISTS passwords (
    id       INTEGER PRIMARY KEY,
    server   TEXT NOT NULL,
    username TEXT,
    password TEXT
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT
);
