//! Filter engine tests.
//!
//! Weighted towards two things that are easy to get wrong and impossible to
//! see: decoding QuiteRSS's per-field operator indices, and the direction of a
//! negation. A filter that silently matches everything marks a feed read.

use rusqlite::params;
use snaprss_core::filters::*;
use snaprss_core::Db;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn db() -> Db {
    let db = Db::open_in_memory().unwrap();
    db.conn()
        .execute(
            "INSERT INTO feeds (id, kind, text, xml_url) VALUES (1, 1, 'A', 'https://a.example/f')",
            [],
        )
        .unwrap();
    db.conn()
        .execute(
            "INSERT INTO feeds (id, kind, text, xml_url) VALUES (2, 1, 'B', 'https://b.example/f')",
            [],
        )
        .unwrap();
    db
}

fn art<'a>(title: &'a str, body: &'a str) -> Candidate<'a> {
    Candidate {
        feed_id: 1,
        title: Some(title),
        description: Some(body),
        new: true,
        ..Default::default()
    }
}

/// Insert a filter with raw stored strings, the way an import would.
fn raw_filter(
    db: &Db,
    id: i64,
    mode: i64,
    feeds: Option<&str>,
    conds: &[(&str, &str, &str)],
    acts: &[(&str, Option<&str>)],
) {
    db.conn()
        .execute(
            "INSERT INTO filters (id, name, type, feeds, enable, num) VALUES (?1, ?1, ?2, ?3, 1, ?1)",
            params![id, mode, feeds],
        )
        .unwrap();
    for (f, c, content) in conds {
        db.conn()
            .execute(
                "INSERT INTO filter_conditions (filter_id, field, condition, content)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, f, c, content],
            )
            .unwrap();
    }
    for (a, p) in acts {
        db.conn()
            .execute(
                "INSERT INTO filter_actions (filter_id, action, params) VALUES (?1, ?2, ?3)",
                params![id, a, p],
            )
            .unwrap();
    }
}

fn news(db: &Db, feed: i64, title: &str, body: &str) -> i64 {
    db.conn()
        .execute(
            "INSERT INTO news (feed_id, title, description, received, link_href, new, read, starred, deleted)
             VALUES (?1, ?2, ?3, '2026-01-01T00:00:00Z', 'https://x.example/1', 1, 0, 0, 0)",
            params![feed, title, body],
        )
        .unwrap();
    db.conn().last_insert_rowid()
}

// ---------------------------------------------------------------------------
// the QuiteRSS index encoding
// ---------------------------------------------------------------------------

#[test]
fn operator_indices_are_read_per_field() {
    // Index 2 is "is" for Title, because Title offers seven operators...
    assert_eq!(Op::parse(Field::Title, "2"), Some(Op::Is));
    // ...and "regular expression" for Description, which offers three. One
    // flat table would turn a substring rule into a regex one.
    assert_eq!(Op::parse(Field::Description, "2"), Some(Op::Regex));
    assert_eq!(Op::parse(Field::News, "2"), Some(Op::Regex));
    // Author has no begins/ends with, so index 4 is its regex.
    assert_eq!(Op::parse(Field::Author, "4"), Some(Op::Regex));
    // Category does have them, so index 4 is "begins with".
    assert_eq!(Op::parse(Field::Category, "4"), Some(Op::BeginsWith));
    // Status offers only two.
    assert_eq!(Op::parse(Field::Status, "1"), Some(Op::IsNot));
    assert_eq!(Op::parse(Field::Status, "2"), None);
}

#[test]
fn an_out_of_range_index_is_not_a_filter() {
    assert_eq!(Op::parse(Field::Description, "6"), None);
    assert_eq!(Field::parse("99"), None);
    assert_eq!(Field::parse("nonsense"), None);
}

#[test]
fn field_indices_match_quiterss_order() {
    for (i, want) in [
        Field::Title,
        Field::Description,
        Field::Author,
        Field::Category,
        Field::Status,
        Field::Link,
        Field::News,
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(Field::parse(&i.to_string()), Some(*want), "index {i}");
    }
}

#[test]
fn action_indices_match_quiterss_order() {
    assert_eq!(Action::parse("0", None), Some(Action::MarkRead));
    assert_eq!(Action::parse("1", None), Some(Action::AddStar));
    assert_eq!(Action::parse("2", None), Some(Action::Delete));
    assert_eq!(Action::parse("3", Some("7")), Some(Action::AddLabel(7)));
    assert_eq!(
        Action::parse("4", Some("/s.wav")),
        Some(Action::PlaySound("/s.wav".into()))
    );
}

#[test]
fn match_mode_zero_is_every_article_not_match_all() {
    // QuiteRSS's combo reads "Match all news", "Match all conditions",
    // "Match any condition". Reading 0 as "all conditions" would make an
    // every-article filter do nothing at all.
    assert_eq!(Match::from_i64(0), Match::Every);
    assert_eq!(Match::from_i64(1), Match::All);
    assert_eq!(Match::from_i64(2), Match::Any);
}

#[test]
fn names_are_accepted_as_well_as_indices() {
    assert_eq!(Op::parse(Field::Title, "begins with"), Some(Op::BeginsWith));
    assert_eq!(Op::parse(Field::Title, "begins_with"), Some(Op::BeginsWith));
    assert_eq!(
        Op::parse(Field::Description, "doesn't contain"),
        Some(Op::NotContains)
    );
    assert_eq!(Field::parse("Title"), Some(Field::Title));
    assert_eq!(Action::parse("mark_read", None), Some(Action::MarkRead));
}

// ---------------------------------------------------------------------------
// matching
// ---------------------------------------------------------------------------

fn one(field: Field, op: Op, content: &str, mode: Match) -> Engine {
    Engine::new(vec![Filter {
        id: 1,
        name: "f".into(),
        mode,
        feeds: None,
        enabled: true,
        num: 0,
        conditions: vec![Condition::new(field, op, content)],
        actions: vec![Action::MarkRead],
    }])
}

fn hits(e: &Engine, c: &Candidate<'_>) -> bool {
    !e.evaluate(c).matched.is_empty()
}

#[test]
fn text_operators() {
    let a = art("Rust 1.90 released", "notes about the release");
    assert!(hits(&one(Field::Title, Op::Contains, "rust", Match::All), &a));
    assert!(hits(
        &one(Field::Title, Op::BeginsWith, "Rust", Match::All),
        &a
    ));
    assert!(hits(
        &one(Field::Title, Op::EndsWith, "released", Match::All),
        &a
    ));
    assert!(hits(
        &one(Field::Title, Op::Is, "Rust 1.90 released", Match::All),
        &a
    ));
    assert!(!hits(&one(Field::Title, Op::Is, "Rust", Match::All), &a));
}

#[test]
fn matching_ignores_case_including_links() {
    let mut a = art("Title", "body");
    a.link = Some("https://Example.COM/Path");
    assert!(hits(
        &one(Field::Link, Op::Contains, "example.com", Match::All),
        &a
    ));
}

#[test]
fn news_field_spans_title_and_body() {
    let e = one(Field::News, Op::Contains, "tokio", Match::All);
    assert!(hits(&e, &art("tokio 2.0", "nothing here")));
    assert!(hits(&e, &art("nothing here", "all about tokio")));
    assert!(!hits(&e, &art("nothing", "here either")));
}

#[test]
fn not_contains_on_the_news_field_means_neither_half_has_it() {
    // QuiteRSS emits `(title NOT LIKE x OR description NOT LIKE x)`, which is
    // true whenever either half lacks the word. An article whose body is all
    // about tokio therefore still counted as "doesn't contain tokio".
    let e = one(Field::News, Op::NotContains, "tokio", Match::All);
    assert!(!hits(&e, &art("nothing here", "all about tokio")));
    assert!(!hits(&e, &art("tokio 2.0", "nothing here")));
    assert!(hits(&e, &art("nothing", "here either")));
}

#[test]
fn a_broken_regex_matches_nothing_rather_than_everything() {
    let c = Condition::new(Field::Title, Op::Regex, "([unclosed");
    assert!(c.is_broken());
    let e = Engine::new(vec![Filter {
        id: 1,
        name: "f".into(),
        mode: Match::All,
        feeds: None,
        enabled: true,
        num: 0,
        conditions: vec![c],
        actions: vec![Action::MarkRead],
    }]);
    assert!(!hits(&e, &art("anything at all", "body")));
}

#[test]
fn regex_works_and_is_case_insensitive() {
    let e = one(Field::Title, Op::Regex, r"^rust \d+\.\d+", Match::All);
    assert!(hits(&e, &art("Rust 1.90 released", "")));
    assert!(!hits(&e, &art("Go 1.90 released", "")));
}

#[test]
fn all_versus_any() {
    let two = |mode| {
        Engine::new(vec![Filter {
            id: 1,
            name: "f".into(),
            mode,
            feeds: None,
            enabled: true,
            num: 0,
            conditions: vec![
                Condition::new(Field::Title, Op::Contains, "rust"),
                Condition::new(Field::Title, Op::Contains, "async"),
            ],
            actions: vec![Action::MarkRead],
        }])
    };
    let a = art("rust without the other word", "");
    assert!(!hits(&two(Match::All), &a));
    assert!(hits(&two(Match::Any), &a));
}

#[test]
fn every_article_mode_ignores_conditions() {
    let e = Engine::new(vec![Filter {
        id: 1,
        name: "f".into(),
        mode: Match::Every,
        feeds: None,
        enabled: true,
        num: 0,
        conditions: vec![Condition::new(Field::Title, Op::Is, "never matches this")],
        actions: vec![Action::MarkRead],
    }]);
    assert!(hits(&e, &art("anything", "")));
}

#[test]
fn match_all_with_no_conditions_matches_nothing() {
    // The empty `all()` is true, which would mark every article read. An
    // unfinished filter should be inert, not maximally destructive.
    let e = Engine::new(vec![Filter {
        id: 1,
        name: "f".into(),
        mode: Match::All,
        feeds: None,
        enabled: true,
        num: 0,
        conditions: vec![],
        actions: vec![Action::Delete],
    }]);
    assert!(!hits(&e, &art("anything", "")));
}

#[test]
fn status_conditions_read_the_articles_flags() {
    let e = one(Field::Status, Op::Is, "starred", Match::All);
    let mut a = art("t", "b");
    assert!(!hits(&e, &a));
    a.starred = true;
    assert!(hits(&e, &a));

    let e = one(Field::Status, Op::IsNot, "read", Match::All);
    let mut a = art("t", "b");
    assert!(hits(&e, &a));
    a.read = true;
    assert!(!hits(&e, &a));
}

// ---------------------------------------------------------------------------
// feed scope
// ---------------------------------------------------------------------------

#[test]
fn a_null_feed_list_means_every_feed() {
    assert!(parse_feed_list(None).is_none());
    let f = Filter {
        id: 1,
        name: "f".into(),
        mode: Match::Every,
        feeds: None,
        enabled: true,
        num: 0,
        conditions: vec![],
        actions: vec![],
    };
    assert!(f.applies_to_feed(1));
    assert!(f.applies_to_feed(999));
}

#[test]
fn an_empty_feed_list_means_no_feed() {
    // Not the same as NULL. An imported filter whose feeds all failed to remap
    // must not quietly widen to every feed.
    let set = parse_feed_list(Some(",")).unwrap();
    assert!(set.is_empty());
    let f = Filter {
        id: 1,
        name: "f".into(),
        mode: Match::Every,
        feeds: Some(set),
        enabled: true,
        num: 0,
        conditions: vec![],
        actions: vec![],
    };
    assert!(!f.applies_to_feed(1));
}

#[test]
fn feed_lists_are_comma_wrapped_like_quiterss() {
    assert_eq!(format_feed_list(&[3, 7, 12]), ",3,7,12,");
    let set = parse_feed_list(Some(",3,7,12,")).unwrap();
    assert_eq!(set.len(), 3);
    assert!(set.contains(&7));
    // QuiteRSS matches with `LIKE '%,7,%'`, which is why the wrapping commas
    // exist: without them ",17," would match a search for 7.
    assert!(!parse_feed_list(Some(",17,")).unwrap().contains(&7));
}

#[test]
fn a_filter_scoped_to_one_feed_leaves_the_other_alone() {
    let db = db();
    raw_filter(
        &db,
        1,
        1,
        Some(",1,"),
        &[("title", "contains", "news")],
        &[("mark_read", None)],
    );
    let e = Engine::load(db.conn()).unwrap();
    assert!(hits(
        &e,
        &Candidate {
            feed_id: 1,
            title: Some("some news"),
            ..Default::default()
        }
    ));
    assert!(!hits(
        &e,
        &Candidate {
            feed_id: 2,
            title: Some("some news"),
            ..Default::default()
        }
    ));
}

// ---------------------------------------------------------------------------
// ordering and effects
// ---------------------------------------------------------------------------

#[test]
fn filters_run_in_order_and_later_ones_see_earlier_effects() {
    let db = db();
    // 1: mark anything containing "ads" as read.
    raw_filter(
        &db,
        1,
        1,
        None,
        &[("title", "contains", "ads")],
        &[("mark_read", None)],
    );
    // 2: delete anything already read.
    raw_filter(
        &db,
        2,
        1,
        None,
        &[("status", "is", "read")],
        &[("delete", None)],
    );
    let e = Engine::load(db.conn()).unwrap();

    let eff = e.evaluate(&art("sponsored ads", ""));
    assert_eq!(eff.matched, vec![1, 2], "both should fire, in order");
    assert!(eff.delete);

    let eff = e.evaluate(&art("a real article", ""));
    assert!(eff.matched.is_empty());
}

#[test]
fn ordering_the_other_way_round_does_not_cascade() {
    let db = db();
    raw_filter(
        &db,
        1,
        1,
        None,
        &[("status", "is", "read")],
        &[("delete", None)],
    );
    raw_filter(
        &db,
        2,
        1,
        None,
        &[("title", "contains", "ads")],
        &[("mark_read", None)],
    );
    let e = Engine::load(db.conn()).unwrap();
    let eff = e.evaluate(&art("sponsored ads", ""));
    assert_eq!(eff.matched, vec![2]);
    assert!(!eff.delete, "the delete rule ran before anything was read");
}

#[test]
fn delete_also_marks_read() {
    // Otherwise a deleted article keeps contributing to the unread count.
    let db = db();
    raw_filter(&db, 1, 0, None, &[], &[("delete", None)]);
    let eff = Engine::load(db.conn()).unwrap().evaluate(&art("x", ""));
    assert!(eff.delete && eff.mark_read);
}

#[test]
fn disabled_filters_are_not_loaded() {
    let db = db();
    raw_filter(&db, 1, 0, None, &[], &[("mark_read", None)]);
    set_filter_enabled(db.conn(), 1, false).unwrap();
    assert!(Engine::load(db.conn()).unwrap().is_empty());
    assert_eq!(load_filters(db.conn(), false).unwrap().len(), 1);
}

#[test]
fn an_unreadable_condition_is_never_treated_as_true() {
    // "Any of": the readable condition still works on its own.
    let db = db();
    raw_filter(
        &db,
        1,
        2,
        None,
        &[("nonsense", "contains", "x"), ("title", "contains", "keep")],
        &[("mark_read", None)],
    );
    let e = Engine::load(db.conn()).unwrap();
    assert_eq!(e.filters()[0].conditions.len(), 1);
    assert!(hits(&e, &art("keep this", "")));
    assert!(!hits(&e, &art("drop this", "")));

    // "All of": dropping it would widen the filter to everything matching
    // the rest. The filter matches nothing instead.
    let db = self::db();
    raw_filter(
        &db,
        1,
        1,
        None,
        &[("nonsense", "contains", "x"), ("title", "contains", "keep")],
        &[("delete", None)],
    );
    let e = Engine::load(db.conn()).unwrap();
    assert!(!hits(&e, &art("keep this", "")));
}

// ---------------------------------------------------------------------------
// applying to stored rows
// ---------------------------------------------------------------------------

#[test]
fn run_on_existing_writes_read_starred_deleted_and_labels() {
    let mut db = db();
    db.conn()
        .execute("INSERT INTO labels (id, name) VALUES (5, 'Later')", [])
        .unwrap();
    raw_filter(
        &db,
        1,
        1,
        None,
        &[("title", "contains", "keep")],
        &[("add_star", None), ("add_label", Some("5"))],
    );
    raw_filter(
        &db,
        2,
        1,
        None,
        &[("title", "contains", "junk")],
        &[("delete", None)],
    );

    let keep = news(&db, 1, "keep this", "");
    let junk = news(&db, 1, "junk mail", "");
    let other = news(&db, 1, "ordinary", "");

    let report = run_on_existing(db.conn_mut(), None, None).unwrap();
    assert_eq!(report.considered, 3);
    assert_eq!(report.matched, 2);
    assert_eq!(report.starred, 1);
    assert_eq!(report.deleted, 1);
    assert_eq!(report.labelled, 1);

    let starred: i64 = db
        .conn()
        .query_row("SELECT starred FROM news WHERE id = ?1", [keep], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(starred, 1);

    let deleted: i64 = db
        .conn()
        .query_row("SELECT deleted FROM news WHERE id = ?1", [junk], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(deleted, 1);

    let labels: i64 = db
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM news_labels WHERE news_id = ?1",
            [keep],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(labels, 1);

    let untouched: i64 = db
        .conn()
        .query_row(
            "SELECT read + starred + deleted FROM news WHERE id = ?1",
            [other],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(untouched, 0);
}

#[test]
fn run_on_existing_can_be_scoped_to_one_filter_even_a_disabled_one() {
    let mut db = db();
    raw_filter(&db, 1, 0, None, &[], &[("mark_read", None)]);
    raw_filter(&db, 2, 0, None, &[], &[("add_star", None)]);
    set_filter_enabled(db.conn(), 2, false).unwrap();
    news(&db, 1, "a", "");

    let r = run_on_existing(db.conn_mut(), None, Some(2)).unwrap();
    assert_eq!(r.starred, 1);
    assert_eq!(r.marked_read, 0, "filter 1 must not have run");
}

#[test]
fn run_on_existing_can_be_scoped_to_one_feed() {
    let mut db = db();
    raw_filter(&db, 1, 0, None, &[], &[("mark_read", None)]);
    news(&db, 1, "a", "");
    news(&db, 2, "b", "");
    let r = run_on_existing(db.conn_mut(), Some(2), None).unwrap();
    assert_eq!(r.considered, 1);
    assert_eq!(r.marked_read, 1);
}

#[test]
fn applying_a_label_twice_is_not_an_error() {
    let mut db = db();
    db.conn()
        .execute("INSERT INTO labels (id, name) VALUES (5, 'L')", [])
        .unwrap();
    raw_filter(&db, 1, 0, None, &[], &[("add_label", Some("5"))]);
    news(&db, 1, "a", "");
    run_on_existing(db.conn_mut(), None, None).unwrap();
    run_on_existing(db.conn_mut(), None, None).unwrap();
    let n: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM news_labels", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

// ---------------------------------------------------------------------------
// saving
// ---------------------------------------------------------------------------

#[test]
fn saving_round_trips_through_names_not_indices() {
    let mut db = db();
    let draft = FilterDraft {
        id: None,
        name: "No ads".into(),
        mode: Match::All,
        feeds: Some(vec![1]),
        enabled: true,
        conditions: vec![
            (Field::Title, Op::Contains, "sponsored".into()),
            (Field::Description, Op::Regex, r"\bad\b".into()),
        ],
        actions: vec![Action::Delete, Action::AddLabel(3)],
    };
    let id = save_filter(db.conn_mut(), &draft).unwrap();

    let stored: String = db
        .conn()
        .query_row(
            "SELECT condition FROM filter_conditions WHERE filter_id = ?1 ORDER BY id LIMIT 1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, "contains", "own filters store readable names");

    let all = load_filters(db.conn(), false).unwrap();
    assert_eq!(all.len(), 1);
    let f = &all[0];
    assert_eq!(f.name, "No ads");
    assert_eq!(f.mode, Match::All);
    assert_eq!(f.conditions.len(), 2);
    assert_eq!(f.actions, vec![Action::Delete, Action::AddLabel(3)]);
    assert!(f.applies_to_feed(1) && !f.applies_to_feed(2));
}

#[test]
fn saving_over_a_filter_replaces_its_conditions_rather_than_adding_to_them() {
    let mut db = db();
    let mut draft = FilterDraft {
        id: None,
        name: "f".into(),
        mode: Match::All,
        feeds: None,
        enabled: true,
        conditions: vec![(Field::Title, Op::Contains, "a".into())],
        actions: vec![Action::MarkRead],
    };
    let id = save_filter(db.conn_mut(), &draft).unwrap();
    draft.id = Some(id);
    draft.conditions = vec![(Field::Title, Op::Contains, "b".into())];
    save_filter(db.conn_mut(), &draft).unwrap();

    let n: i64 = db
        .conn()
        .query_row("SELECT COUNT(*) FROM filter_conditions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(load_filters(db.conn(), false).unwrap()[0].conditions[0].content, "b");
}

#[test]
fn deleting_a_filter_takes_its_conditions_and_actions_with_it() {
    let db = db();
    raw_filter(
        &db,
        1,
        1,
        None,
        &[("title", "contains", "x")],
        &[("mark_read", None)],
    );
    delete_filter(db.conn(), 1).unwrap();
    for t in ["filter_conditions", "filter_actions"] {
        let n: i64 = db
            .conn()
            .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "{t} should have been cascaded");
    }
}

#[test]
fn reordering_changes_which_filter_runs_first() {
    let mut db = db();
    raw_filter(&db, 1, 0, None, &[], &[("mark_read", None)]);
    raw_filter(&db, 2, 0, None, &[], &[("add_star", None)]);
    assert_eq!(
        Engine::load(db.conn())
            .unwrap()
            .evaluate(&art("x", ""))
            .matched,
        vec![1, 2]
    );
    reorder_filter(db.conn_mut(), 2, -1).unwrap();
    assert_eq!(
        Engine::load(db.conn())
            .unwrap()
            .evaluate(&art("x", ""))
            .matched,
        vec![2, 1]
    );
}

#[test]
fn reordering_past_the_end_is_a_clamp_not_a_panic() {
    let mut db = db();
    raw_filter(&db, 1, 0, None, &[], &[("mark_read", None)]);
    raw_filter(&db, 2, 0, None, &[], &[("add_star", None)]);
    reorder_filter(db.conn_mut(), 1, 99).unwrap();
    reorder_filter(db.conn_mut(), 2, -99).unwrap();
    assert_eq!(
        Engine::load(db.conn())
            .unwrap()
            .evaluate(&art("x", ""))
            .matched,
        vec![2, 1]
    );
}

#[test]
fn a_removed_feed_leaves_every_filter_list() {
    let db = db();
    raw_filter(&db, 1, 1, Some(",3,57,"), &[("title", "contains", "x")], &[("delete", None)]);
    raw_filter(&db, 2, 1, Some(",57,"), &[("title", "contains", "x")], &[("delete", None)]);
    let gone: std::collections::HashSet<i64> = [57].into_iter().collect();
    snaprss_core::filters::forget_feeds(db.conn(), &gone).unwrap();
    let lists: Vec<String> = db
        .conn()
        .prepare("SELECT feeds FROM filters ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(lists, [",3,", ","], "a list that empties applies to nothing, not everything");
}
