//! The filter engine: conditions matched against arriving articles, actions
//! applied to the ones that match.
//!
//! # Why this is not SQL
//!
//! QuiteRSS builds a `WHERE` clause by string concatenation, runs it over the
//! feed, and then runs an `UPDATE` over the result. That is fast and it is also
//! how `'` in a filter's text became something you had to remember to double.
//! Here a condition is a predicate evaluated in Rust against the article that
//! was just parsed. Nothing the user types is ever concatenated into SQL, and
//! the engine can run against an article that has not been inserted yet — which
//! is what "applied on ingest" means.
//!
//! # Reading QuiteRSS's encoding
//!
//! A filter imported from QuiteRSS stores every field, operator and action as a
//! *combo box index*, and the operator list differs per field: index 2 is "is"
//! for Title and "regular expression" for Description, because Description only
//! offers three operators. Reading those numbers with one flat table silently
//! turns a substring filter into a regex one. [`Op::parse`] therefore takes the
//! field, and [`OPS_FOR`] is that per-field table, transcribed from
//! `src/newsfilters/itemcondition.cpp`.
//!
//! Filters this application writes use the names instead (`"title"`,
//! `"contains"`, `"mark_read"`), which are unambiguous. Both are accepted.

use rusqlite::{params, Connection, Transaction};
use std::collections::HashSet;

use crate::DbError;

// ---------------------------------------------------------------------------
// vocabulary
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Description,
    Author,
    Category,
    Status,
    Link,
    /// Title or description. QuiteRSS calls this "News".
    News,
}

impl Field {
    pub fn parse(s: &str) -> Option<Field> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "title" | "0" => Field::Title,
            "description" | "desc" | "1" => Field::Description,
            "author" | "2" => Field::Author,
            "category" | "3" => Field::Category,
            "status" | "state" | "4" => Field::Status,
            "link" | "url" | "5" => Field::Link,
            "news" | "any" | "6" => Field::News,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Field::Title => "title",
            Field::Description => "description",
            Field::Author => "author",
            Field::Category => "category",
            Field::Status => "status",
            Field::Link => "link",
            Field::News => "news",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Contains,
    NotContains,
    Is,
    IsNot,
    BeginsWith,
    EndsWith,
    Regex,
}

/// The operator list each field offers, in QuiteRSS's order. A numeric operator
/// indexes into the row for its field.
const OPS_FOR: &[(Field, &[Op])] = &[
    (
        Field::Title,
        &[
            Op::Contains,
            Op::NotContains,
            Op::Is,
            Op::IsNot,
            Op::BeginsWith,
            Op::EndsWith,
            Op::Regex,
        ],
    ),
    (
        Field::Description,
        &[Op::Contains, Op::NotContains, Op::Regex],
    ),
    (
        Field::Author,
        &[
            Op::Contains,
            Op::NotContains,
            Op::Is,
            Op::IsNot,
            Op::Regex,
        ],
    ),
    (
        Field::Category,
        &[
            Op::Contains,
            Op::NotContains,
            Op::Is,
            Op::IsNot,
            Op::BeginsWith,
            Op::EndsWith,
            Op::Regex,
        ],
    ),
    (Field::Status, &[Op::Is, Op::IsNot]),
    (
        Field::Link,
        &[
            Op::Contains,
            Op::NotContains,
            Op::Is,
            Op::IsNot,
            Op::BeginsWith,
            Op::EndsWith,
            Op::Regex,
        ],
    ),
    (Field::News, &[Op::Contains, Op::NotContains, Op::Regex]),
];

fn ops_for(field: Field) -> &'static [Op] {
    OPS_FOR
        .iter()
        .find(|(f, _)| *f == field)
        .map(|(_, o)| *o)
        .unwrap_or(&[])
}

impl Op {
    /// `field` matters: a bare number is an index into that field's own list.
    pub fn parse(field: Field, s: &str) -> Option<Op> {
        let s = s.trim();
        if let Ok(n) = s.parse::<usize>() {
            return ops_for(field).get(n).copied();
        }
        Some(
            match s
                .to_ascii_lowercase()
                .replace([' ', '-'], "_")
                .trim_matches('_')
            {
                "contains" => Op::Contains,
                "not_contains" | "doesn't_contain" | "doesnt_contain" => Op::NotContains,
                "is" | "equals" => Op::Is,
                "is_not" | "isn't" | "isnt" => Op::IsNot,
                "begins_with" | "starts_with" => Op::BeginsWith,
                "ends_with" => Op::EndsWith,
                "regex" | "regexp" | "regular_expressions" | "regular_expression" => Op::Regex,
                _ => return None,
            },
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Op::Contains => "contains",
            Op::NotContains => "not_contains",
            Op::Is => "is",
            Op::IsNot => "is_not",
            Op::BeginsWith => "begins_with",
            Op::EndsWith => "ends_with",
            Op::Regex => "regex",
        }
    }

    /// The operators that make sense for a field, for the UI to offer.
    pub fn for_field(field: Field) -> &'static [Op] {
        ops_for(field)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    New,
    Read,
    Starred,
}

impl Status {
    fn parse(s: &str) -> Option<Status> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "new" | "0" => Status::New,
            "read" | "1" => Status::Read,
            "starred" | "star" | "2" => Status::Starred,
            _ => return None,
        })
    }
}

/// How a filter's conditions combine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// Every article in scope, conditions ignored. QuiteRSS index 0.
    Every,
    /// All conditions. QuiteRSS index 1.
    All,
    /// Any condition. QuiteRSS index 2.
    Any,
}

impl Match {
    pub fn from_i64(n: i64) -> Match {
        match n {
            1 => Match::All,
            2 => Match::Any,
            _ => Match::Every,
        }
    }
    pub fn as_i64(self) -> i64 {
        match self {
            Match::Every => 0,
            Match::All => 1,
            Match::Any => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    MarkRead,
    AddStar,
    Delete,
    AddLabel(i64),
    PlaySound(String),
    /// QuiteRSS's "show in notifier", whose parameter is a colour.
    Notify(String),
}

impl Action {
    pub fn parse(action: &str, params: Option<&str>) -> Option<Action> {
        let p = params.unwrap_or("").trim();
        Some(
            match action
                .trim()
                .to_ascii_lowercase()
                .replace([' ', '-'], "_")
                .as_str()
            {
                "mark_read" | "0" => Action::MarkRead,
                "add_star" | "star" | "1" => Action::AddStar,
                "delete" | "2" => Action::Delete,
                "add_label" | "label" | "3" => Action::AddLabel(p.parse().ok()?),
                "play_sound" | "4" => Action::PlaySound(p.to_string()),
                "notify" | "show_in_notifier" | "5" => Action::Notify(p.to_string()),
                _ => return None,
            },
        )
    }

    pub fn as_pair(&self) -> (&'static str, Option<String>) {
        match self {
            Action::MarkRead => ("mark_read", None),
            Action::AddStar => ("add_star", None),
            Action::Delete => ("delete", None),
            Action::AddLabel(id) => ("add_label", Some(id.to_string())),
            Action::PlaySound(p) => ("play_sound", Some(p.clone())),
            Action::Notify(c) => ("notify", Some(c.clone())),
        }
    }
}

// ---------------------------------------------------------------------------
// conditions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Condition {
    pub field: Field,
    pub op: Op,
    pub content: String,
    /// Compiled once when the filter is loaded, not once per article.
    regex: Option<regex::Regex>,
}

impl Condition {
    pub fn new(field: Field, op: Op, content: impl Into<String>) -> Condition {
        let content = content.into();
        // A regex that does not compile must never match. Silently matching
        // everything would mark a feed read; silently matching nothing is the
        // failure the user can see and fix.
        let regex = (op == Op::Regex)
            .then(|| regex::RegexBuilder::new(&content).case_insensitive(true).build().ok())
            .flatten();
        Condition {
            field,
            op,
            content,
            regex,
        }
    }

    /// True when the regex was asked for and would not compile.
    pub fn is_broken(&self) -> bool {
        self.op == Op::Regex && self.regex.is_none()
    }

    fn matches(&self, c: &Candidate<'_>) -> bool {
        if self.field == Field::Status {
            let Some(want) = Status::parse(&self.content) else {
                return false;
            };
            let holds = match want {
                Status::New => c.new,
                Status::Read => c.read,
                Status::Starred => c.starred,
            };
            return match self.op {
                Op::Is => holds,
                Op::IsNot => !holds,
                _ => false,
            };
        }

        // "News" is title or description. QuiteRSS's negation of it is
        // `(title NOT LIKE x OR description NOT LIKE x)`, which is true
        // whenever *either* field lacks the text — so an article with the word
        // only in its body still counts as not containing it. That is a bug,
        // not a convention, so the negation here is over the pair.
        let haystacks: Vec<&str> = match self.field {
            Field::Title => vec![c.title.unwrap_or("")],
            Field::Description => vec![c.description.unwrap_or("")],
            Field::Author => vec![c.author.unwrap_or("")],
            Field::Category => vec![c.category.unwrap_or("")],
            Field::Link => vec![c.link.unwrap_or("")],
            Field::News => vec![c.title.unwrap_or(""), c.description.unwrap_or("")],
            Field::Status => unreachable!(),
        };

        let positive = |op: Op| -> bool {
            haystacks.iter().any(|h| self.holds(op, h))
        };

        match self.op {
            Op::NotContains => !positive(Op::Contains),
            Op::IsNot => !positive(Op::Is),
            op => positive(op),
        }
    }

    fn holds(&self, op: Op, haystack: &str) -> bool {
        if op == Op::Regex {
            return self.regex.as_ref().is_some_and(|r| r.is_match(haystack));
        }
        // Case-insensitive throughout, including links: QuiteRSS compares link
        // case-sensitively, which surprises anyone filtering on a host.
        let h = haystack.to_lowercase();
        let n = self.content.to_lowercase();
        match op {
            Op::Contains => h.contains(&n),
            Op::Is => h == n,
            Op::BeginsWith => h.starts_with(&n),
            Op::EndsWith => h.ends_with(&n),
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// filters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Filter {
    pub id: i64,
    pub name: String,
    pub mode: Match,
    /// `None` means every feed. `Some(set)` means exactly those feeds, and an
    /// empty set therefore means no feed at all — which is what an imported
    /// filter whose feeds all failed to remap should become, rather than
    /// quietly widening to everything.
    pub feeds: Option<HashSet<i64>>,
    pub enabled: bool,
    pub num: i64,
    pub conditions: Vec<Condition>,
    pub actions: Vec<Action>,
}

impl Filter {
    pub fn applies_to_feed(&self, feed_id: i64) -> bool {
        match &self.feeds {
            None => true,
            Some(set) => set.contains(&feed_id),
        }
    }

    fn matches(&self, c: &Candidate<'_>) -> bool {
        if !self.applies_to_feed(c.feed_id) {
            return false;
        }
        match self.mode {
            Match::Every => true,
            // An "all conditions" filter with no conditions would otherwise
            // match everything, which is the destructive direction.
            Match::All => !self.conditions.is_empty() && self.conditions.iter().all(|x| x.matches(c)),
            Match::Any => self.conditions.iter().any(|x| x.matches(c)),
        }
    }
}

/// Parse `filters.feeds`. `NULL` is every feed; a comma or tab separated list
/// is those feeds. QuiteRSS writes `,1,7,12,` and queries it with `LIKE`.
pub fn parse_feed_list(raw: Option<&str>) -> Option<HashSet<i64>> {
    let raw = raw?;
    Some(
        raw.split([',', '\t'])
            .filter_map(|p| p.trim().parse::<i64>().ok())
            .collect(),
    )
}

/// Take removed feeds out of every filter's feed list.
pub fn forget_feeds(
    conn: &Connection,
    gone: &std::collections::HashSet<i64>,
) -> Result<(), DbError> {
    let lists: Vec<(i64, String)> = {
        let mut stmt = conn.prepare("SELECT id, feeds FROM filters WHERE feeds IS NOT NULL")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    for (fid, raw) in lists {
        let ids = parse_feed_list(Some(&raw)).unwrap_or_default();
        if ids.iter().any(|i| gone.contains(i)) {
            let mut kept: Vec<i64> = ids.into_iter().filter(|i| !gone.contains(i)).collect();
            kept.sort_unstable();
            conn.execute(
                "UPDATE filters SET feeds = ?1 WHERE id = ?2",
                params![format_feed_list(&kept), fid],
            )?;
        }
    }
    Ok(())
}

pub fn format_feed_list(ids: &[i64]) -> String {
    let mut s = String::from(",");
    for id in ids {
        s.push_str(&id.to_string());
        s.push(',');
    }
    s
}

// ---------------------------------------------------------------------------
// evaluation
// ---------------------------------------------------------------------------

/// An article being considered. Borrowed, because on ingest the values exist
/// only as locals in the insert loop.
#[derive(Debug, Clone, Default)]
pub struct Candidate<'a> {
    pub feed_id: i64,
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
    pub author: Option<&'a str>,
    pub category: Option<&'a str>,
    pub link: Option<&'a str>,
    pub new: bool,
    pub read: bool,
    pub starred: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Effects {
    pub mark_read: bool,
    pub star: bool,
    pub delete: bool,
    pub labels: Vec<i64>,
    pub sounds: Vec<String>,
    pub notify: Vec<String>,
    /// Which filters fired, in order. The UI shows this; the tests assert it.
    pub matched: Vec<i64>,
}

impl Effects {
    pub fn is_empty(&self) -> bool {
        !self.mark_read && !self.star && !self.delete && self.labels.is_empty()
    }
}

pub struct Engine {
    filters: Vec<Filter>,
}

impl Engine {
    pub fn new(filters: Vec<Filter>) -> Engine {
        Engine { filters }
    }

    /// Enabled filters only, in the order the user arranged them.
    pub fn load(conn: &Connection) -> Result<Engine, DbError> {
        Ok(Engine::new(load_filters(conn, true)?))
    }

    pub fn filters(&self) -> &[Filter] {
        &self.filters
    }

    pub fn is_empty(&self) -> bool {
        self.filters.is_empty()
    }

    /// Run every filter in order. Later filters see what earlier ones did, so
    /// "mark read" followed by a `status is read` filter behaves the way the
    /// list reads top to bottom.
    pub fn evaluate(&self, candidate: &Candidate<'_>) -> Effects {
        let mut eff = Effects::default();
        let mut c = candidate.clone();

        for f in &self.filters {
            if !f.matches(&c) {
                continue;
            }
            eff.matched.push(f.id);
            for a in &f.actions {
                match a {
                    Action::MarkRead => {
                        eff.mark_read = true;
                        c.read = true;
                        c.new = false;
                    }
                    Action::AddStar => {
                        eff.star = true;
                        c.starred = true;
                    }
                    Action::Delete => {
                        eff.delete = true;
                        eff.mark_read = true;
                        c.read = true;
                        c.new = false;
                    }
                    Action::AddLabel(id) => {
                        if !eff.labels.contains(id) {
                            eff.labels.push(*id);
                        }
                    }
                    Action::PlaySound(p) => eff.sounds.push(p.clone()),
                    Action::Notify(c) => eff.notify.push(c.clone()),
                }
            }
        }
        eff
    }
}

/// Write `effects` onto an existing row.
pub fn apply(tx: &Transaction<'_>, news_id: i64, eff: &Effects) -> rusqlite::Result<()> {
    if eff.mark_read {
        tx.execute(
            "UPDATE news SET read = 1, new = 0 WHERE id = ?1",
            [news_id],
        )?;
    }
    if eff.star {
        tx.execute("UPDATE news SET starred = 1 WHERE id = ?1", [news_id])?;
    }
    if eff.delete {
        tx.execute(
            "UPDATE news SET deleted = 1, delete_date = ?2 WHERE id = ?1",
            params![news_id, now_iso()],
        )?;
    }
    // Only labels that exist. `OR IGNORE` does not cover a foreign-key
    // failure, and one filter naming a deleted label rolled back the whole
    // feed's update, on every poll.
    for label in &eff.labels {
        tx.execute(
            "INSERT OR IGNORE INTO news_labels (news_id, label_id)
             SELECT ?1, id FROM labels WHERE id = ?2",
            params![news_id, label],
        )?;
    }
    Ok(())
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

// ---------------------------------------------------------------------------
// storage
// ---------------------------------------------------------------------------

pub fn load_filters(conn: &Connection, enabled_only: bool) -> Result<Vec<Filter>, DbError> {
    let sql = if enabled_only {
        "SELECT id, name, type, feeds, enable, num FROM filters WHERE enable <> 0 ORDER BY num, id"
    } else {
        "SELECT id, name, type, feeds, enable, num FROM filters ORDER BY num, id"
    };
    let mut stmt = conn.prepare(sql)?;
    let heads = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                Match::from_i64(r.get::<_, i64>(2)?),
                r.get::<_, Option<String>>(3)?,
                r.get::<_, i64>(4)? != 0,
                r.get::<_, i64>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut out = Vec::with_capacity(heads.len());
    for (id, name, mode, feeds, enabled, num) in heads {
        let (mut conditions, unreadable) = load_conditions(conn, id)?;
        // Dropping an unreadable condition from "all of" widens the filter:
        // [title contains x, <unknown>] with Delete would delete everything
        // containing x. With no conditions an "all" filter matches nothing,
        // which is what a filter that cannot be understood should do.
        if unreadable && mode == Match::All {
            conditions.clear();
        }
        out.push(Filter {
            id,
            name,
            mode,
            feeds: parse_feed_list(feeds.as_deref()),
            enabled,
            num,
            conditions,
            actions: load_actions(conn, id)?,
        });
    }
    Ok(out)
}

/// The conditions that could be read, and whether any could not.
fn load_conditions(conn: &Connection, filter_id: i64) -> Result<(Vec<Condition>, bool), DbError> {
    let mut stmt = conn.prepare(
        "SELECT field, condition, content FROM filter_conditions WHERE filter_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([filter_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        ))
    })?;
    let mut out = Vec::new();
    let mut unreadable = false;
    for row in rows {
        let (f, o, content) = row?;
        // An unreadable condition is never treated as true: a filter that
        // cannot be understood should do nothing, not everything.
        let parsed = Field::parse(&f).and_then(|field| Some((field, Op::parse(field, &o)?)));
        match parsed {
            Some((field, op)) => out.push(Condition::new(field, op, content)),
            None => unreadable = true,
        }
    }
    Ok((out, unreadable))
}

fn load_actions(conn: &Connection, filter_id: i64) -> Result<Vec<Action>, DbError> {
    let mut stmt =
        conn.prepare("SELECT action, params FROM filter_actions WHERE filter_id = ?1 ORDER BY id")?;
    let rows = stmt.query_map([filter_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (a, p) = row?;
        if let Some(action) = Action::parse(&a, p.as_deref()) {
            out.push(action);
        }
    }
    Ok(out)
}

/// Everything a filter is, as the settings window edits it.
#[derive(Debug, Clone)]
pub struct FilterDraft {
    pub id: Option<i64>,
    pub name: String,
    pub mode: Match,
    pub feeds: Option<Vec<i64>>,
    pub enabled: bool,
    pub conditions: Vec<(Field, Op, String)>,
    pub actions: Vec<Action>,
}

/// Insert or replace a whole filter. Conditions and actions are rewritten
/// wholesale, because editing them individually would need stable ids the UI
/// has no reason to carry.
pub fn save_filter(conn: &mut Connection, draft: &FilterDraft) -> Result<i64, DbError> {
    let tx = conn.transaction()?;
    let feeds = draft.feeds.as_ref().map(|v| format_feed_list(v));

    let id = match draft.id {
        Some(id) => {
            tx.execute(
                "UPDATE filters SET name = ?2, type = ?3, feeds = ?4, enable = ?5 WHERE id = ?1",
                params![
                    id,
                    draft.name,
                    draft.mode.as_i64(),
                    feeds,
                    draft.enabled as i64
                ],
            )?;
            tx.execute("DELETE FROM filter_conditions WHERE filter_id = ?1", [id])?;
            tx.execute("DELETE FROM filter_actions WHERE filter_id = ?1", [id])?;
            id
        }
        None => {
            let num: i64 = tx
                .query_row("SELECT IFNULL(MAX(num), -1) + 1 FROM filters", [], |r| {
                    r.get(0)
                })
                .unwrap_or(0);
            tx.execute(
                "INSERT INTO filters (name, type, feeds, enable, num) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    draft.name,
                    draft.mode.as_i64(),
                    feeds,
                    draft.enabled as i64,
                    num
                ],
            )?;
            tx.last_insert_rowid()
        }
    };

    for (field, op, content) in &draft.conditions {
        tx.execute(
            "INSERT INTO filter_conditions (filter_id, field, condition, content)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, field.as_str(), op.as_str(), content],
        )?;
    }
    for action in &draft.actions {
        let (a, p) = action.as_pair();
        tx.execute(
            "INSERT INTO filter_actions (filter_id, action, params) VALUES (?1, ?2, ?3)",
            params![id, a, p],
        )?;
    }
    tx.commit()?;
    Ok(id)
}

pub fn delete_filter(conn: &Connection, id: i64) -> Result<(), DbError> {
    conn.execute("DELETE FROM filters WHERE id = ?1", [id])?;
    Ok(())
}

pub fn set_filter_enabled(conn: &Connection, id: i64, on: bool) -> Result<(), DbError> {
    conn.execute(
        "UPDATE filters SET enable = ?2 WHERE id = ?1",
        params![id, on as i64],
    )?;
    Ok(())
}

/// Move a filter up or down the list. Order matters, because later filters see
/// what earlier ones did.
pub fn reorder_filter(conn: &mut Connection, id: i64, delta: i64) -> Result<(), DbError> {
    let all = load_filters(conn, false)?;
    let Some(pos) = all.iter().position(|f| f.id == id) else {
        return Ok(());
    };
    let target = (pos as i64 + delta).clamp(0, all.len() as i64 - 1) as usize;
    if target == pos {
        return Ok(());
    }
    let mut order: Vec<i64> = all.iter().map(|f| f.id).collect();
    let moved = order.remove(pos);
    order.insert(target, moved);

    let tx = conn.transaction()?;
    for (i, fid) in order.iter().enumerate() {
        tx.execute(
            "UPDATE filters SET num = ?2 WHERE id = ?1",
            params![fid, i as i64],
        )?;
    }
    tx.commit()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// running over articles already stored
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterRunReport {
    pub considered: usize,
    pub matched: usize,
    pub marked_read: usize,
    pub starred: usize,
    pub deleted: usize,
    pub labelled: usize,
}

/// Apply the filters to articles already in the database. This is the
/// "apply to existing articles" button; ingest does not use it.
///
/// `only_filter` restricts the run to one filter, for testing a rule before
/// letting it loose on everything.
pub fn run_on_existing(
    conn: &mut Connection,
    feed_id: Option<i64>,
    only_filter: Option<i64>,
) -> Result<FilterRunReport, DbError> {
    let mut filters = load_filters(conn, true)?;
    if let Some(want) = only_filter {
        // Deliberately ignores `enable`, so an off filter can still be tried.
        filters = load_filters(conn, false)?
            .into_iter()
            .filter(|f| f.id == want)
            .collect();
    }
    let engine = Engine::new(filters);
    let mut report = FilterRunReport::default();
    if engine.is_empty() {
        return Ok(report);
    }

    type Row = (
        i64,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        bool,
        bool,
        bool,
    );
    let rows: Vec<Row> = {
        let mut stmt = conn.prepare(
            "SELECT id, feed_id, title, description, content, author_name, category,
                    link_href, new, read, starred
             FROM news
             WHERE deleted = 0 AND (?1 IS NULL OR feed_id = ?1)",
        )?;
        let out = stmt
            .query_map([feed_id], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                    r.get::<_, i64>(8)? != 0,
                    r.get::<_, i64>(9)? != 0,
                    r.get::<_, i64>(10)? != 0,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        out
    };

    let tx = conn.transaction()?;
    for (id, fid, title, description, content, author, category, link, new, read, starred) in &rows
    {
        report.considered += 1;
        // Description is what the feed gave as a summary; content is the full
        // body when it carried one. A filter on "description" should see both,
        // otherwise the same rule behaves differently per feed.
        let body = match (description.as_deref(), content.as_deref()) {
            (Some(d), Some(c)) => Some(format!("{d}\n{c}")),
            (d, c) => d.or(c).map(str::to_string),
        };
        let cand = Candidate {
            feed_id: *fid,
            title: title.as_deref(),
            description: body.as_deref(),
            author: author.as_deref(),
            category: category.as_deref(),
            link: link.as_deref(),
            new: *new,
            read: *read,
            starred: *starred,
        };
        let eff = engine.evaluate(&cand);
        if eff.matched.is_empty() {
            continue;
        }
        report.matched += 1;
        if eff.mark_read && !read {
            report.marked_read += 1;
        }
        if eff.star && !starred {
            report.starred += 1;
        }
        if eff.delete {
            report.deleted += 1;
        }
        report.labelled += eff.labels.len();
        apply(&tx, *id, &eff)?;
    }
    tx.commit()?;
    Ok(report)
}
