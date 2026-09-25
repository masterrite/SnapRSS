//! Row types. Only the fields the UI actually reads are modelled; everything
//! else stays in SQL until something needs it.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Folder,
    Feed,
}

impl NodeKind {
    pub fn as_i64(self) -> i64 {
        match self {
            NodeKind::Folder => 0,
            NodeKind::Feed => 1,
        }
    }

    pub fn from_i64(v: i64) -> Self {
        if v == 0 {
            NodeKind::Folder
        } else {
            NodeKind::Feed
        }
    }
}

/// What the reading pane shows for a feed. There is no web-page variant:
/// SnapRSS has no embedded browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingMode {
    /// Fetch the linked page, extract the article, sanitise it.
    FullArticle,
    /// Render only what the feed supplied.
    DescriptionOnly,
}

impl ReadingMode {
    pub fn as_i64(self) -> i64 {
        match self {
            ReadingMode::FullArticle => 0,
            ReadingMode::DescriptionOnly => 1,
        }
    }

    pub fn from_i64(v: i64) -> Self {
        if v == 1 {
            ReadingMode::DescriptionOnly
        } else {
            ReadingMode::FullArticle
        }
    }
}

#[derive(Debug, Clone)]
pub struct FeedNode {
    pub id: i64,
    pub kind: NodeKind,
    pub parent_id: Option<i64>,
    pub row_to_parent: i64,
    pub text: Option<String>,
    pub xml_url: Option<String>,
    pub html_url: Option<String>,
    pub unread: i64,
    pub undelete_count: i64,
    pub status: Option<String>,
    pub reading_mode: ReadingMode,
    /// Folders only. Imported from QuiteRSS, so a collapsed folder stays
    /// collapsed after the move.
    pub expanded: bool,
}

impl FeedNode {
    /// A feed whose last update failed. The tree shows these with a warning.
    pub fn is_broken(&self) -> bool {
        self.kind == NodeKind::Feed
            && self
                .status
                .as_deref()
                .map(|s| !s.is_empty())
                .unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
pub struct NewsItem {
    pub id: i64,
    pub feed_id: i64,
    pub guid: Option<String>,
    pub title: Option<String>,
    pub author_name: Option<String>,
    pub published: Option<String>,
    pub received: String,
    pub link_href: Option<String>,
    pub read: bool,
    pub starred: bool,
    pub deleted: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    pub folders: usize,
    pub feeds: usize,
    pub news: usize,
    pub labels: usize,
    pub label_links: usize,
    pub filters: usize,
    /// Feeds already subscribed, skipped rather than duplicated. OPML import
    /// is additive, so re-importing the same file is a no-op.
    pub duplicates: usize,
    pub conditions: usize,
    pub actions: usize,
    pub passwords: usize,
    /// Rows the source database had but that did not survive the mapping,
    /// with a reason. Anything in here is a bug or a deliberate drop.
    pub skipped: Vec<String>,
}
