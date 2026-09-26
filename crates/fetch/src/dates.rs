//! Publication dates as feeds actually write them.
//!
//! The RFC 2822/3339/1123 repairs follow feed-rs's own lenient parser (MIT),
//! which is not public and so cannot be wrapped. On top of them: the formats
//! Chinese feeds use, and "CST" read as China Standard Time in a Chinese feed.
//! feed-rs reads CST as US Central time, fourteen hours off, which put those
//! articles at the top of every list as if they came from the future.

use std::sync::OnceLock;

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Utc};
use regex::Regex;

struct Fix(Regex, &'static str);

fn fixes(list: &'static OnceLock<Vec<Fix>>, build: fn() -> Vec<Fix>) -> &'static [Fix] {
    list.get_or_init(build)
}

fn apply(s: &str, fixes: &[Fix]) -> String {
    let mut out = s.trim().to_string();
    for Fix(re, sub) in fixes {
        out = re.replace(&out, *sub).to_string();
    }
    out
}

fn rfc2822(s: &str) -> Option<DateTime<Utc>> {
    static F: OnceLock<Vec<Fix>> = OnceLock::new();
    let f = fixes(&F, || {
        vec![
            Fix(Regex::new("(UTC|-0000$)").unwrap(), "+0000"),
            Fix(Regex::new("(Sun|Mon|Tue|Wed|Thu|Fri|Sat)[a-z]*, ").unwrap(), ""),
            Fix(
                Regex::new("(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)[a-z]*").unwrap(),
                "$1",
            ),
            Fix(Regex::new(" ([0-9]):").unwrap(), " 0${1}:"),
        ]
    });
    DateTime::parse_from_rfc2822(&apply(s, f)).ok().map(|d| d.with_timezone(&Utc))
}

fn rfc3339(s: &str) -> Option<DateTime<Utc>> {
    static F: OnceLock<Vec<Fix>> = OnceLock::new();
    let f = fixes(&F, || {
        vec![
            Fix(Regex::new(r"(\+|-)(\d{2})(\d{2})$").unwrap(), "${1}${2}:${3}"),
        ]
    });
    let s = apply(s, f);
    DateTime::parse_from_rfc3339(&s)
        .or_else(|_| DateTime::parse_from_rfc3339(&s.replacen(' ', "T", 1)))
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn rfc1123(s: &str) -> Option<DateTime<Utc>> {
    static F: OnceLock<Vec<Fix>> = OnceLock::new();
    let f = fixes(&F, || {
        vec![
            Fix(Regex::new(" Z$").unwrap(), " +0000"),
            Fix(Regex::new("^[[:alpha:]]{3}, ").unwrap(), ""),
        ]
    });
    DateTime::parse_from_str(&apply(s, f), "%d %b %Y %H:%M:%S %z")
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

/// Dates with no zone at all ("2024-01-15 10:30:00", "2024/01/15"), read in
/// `zone`.
fn naive(s: &str, zone: FixedOffset) -> Option<DateTime<Utc>> {
    let s = s.trim();
    for fmt in [
        "%Y-%m-%d %H:%M:%S",
        "%Y/%m/%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y/%m/%d %H:%M",
    ] {
        if let Ok(n) = NaiveDateTime::parse_from_str(s, fmt) {
            return zone.from_local_datetime(&n).single().map(|d| d.with_timezone(&Utc));
        }
    }
    for fmt in ["%Y-%m-%d", "%Y/%m/%d"] {
        if let Ok(d) = NaiveDate::parse_from_str(s, fmt) {
            let n = d.and_hms_opt(0, 0, 0)?;
            return zone.from_local_datetime(&n).single().map(|d| d.with_timezone(&Utc));
        }
    }
    None
}

/// Parse a feed timestamp. `chinese` says the feed is in Chinese, which
/// decides what "CST" and a date with no zone mean.
pub fn parse(original: &str, chinese: bool) -> Option<DateTime<Utc>> {
    static GMT: OnceLock<Regex> = OnceLock::new();
    let gmt = GMT.get_or_init(|| Regex::new(r"(?i)\b(?:GMT|UTC)\s*([+-])(\d{1,2})(?::?(\d{2}))?\s*$").unwrap());
    let mut s = original.trim().to_string();
    // "GMT+8", "UTC+08:00" -> "+0800"
    if let Some(c) = gmt.captures(&s) {
        let offset = format!(
            "{}{:02}{}",
            &c[1],
            c[2].parse::<u32>().unwrap_or(0),
            c.get(3).map_or("00", |m| m.as_str())
        );
        s = format!("{}{}", &s[..c.get(0).unwrap().start()], offset);
    }
    if chinese && s.ends_with(" CST") {
        s = format!("{} +0800", &s[..s.len() - 4]);
    }
    let zone = if chinese {
        FixedOffset::east_opt(8 * 3600).unwrap()
    } else {
        FixedOffset::east_opt(0).unwrap()
    };
    // "24:30" is half past midnight at the end of the day, which no parser
    // accepts. Read it as 00:30 and move to the next day; reading it as 00:30
    // alone put the article a day early.
    static H24: OnceLock<Regex> = OnceLock::new();
    let h24 = H24.get_or_init(|| Regex::new(r"([ T])24:(\d{2})").unwrap());
    let next_day = h24.is_match(&s);
    if next_day {
        s = h24.replace(&s, "${1}00:${2}").to_string();
    }
    // A bare date ("2024-01-15") is left to `naive`, which reads it in the
    // feed's zone. The feed-rs repair that made it midnight UTC ran first and
    // put a Chinese feed's articles eight hours late.
    rfc3339(&s)
        .or_else(|| rfc2822(&s))
        .or_else(|| rfc1123(&s))
        .or_else(|| naive(&s, zone))
        .map(|d| if next_day { d + chrono::Duration::days(1) } else { d })
}

/// Whether a feed is Chinese: its declared language, or failing that, its
/// text.
pub fn looks_chinese(xml: &str) -> bool {
    let head: String = xml.chars().take(64 * 1024).collect();
    let lower = head.to_ascii_lowercase();
    if lower.contains("<language>zh") || lower.contains("xml:lang=\"zh") || lower.contains("xml:lang='zh") {
        return true;
    }
    let han = head.chars().filter(|&c| ('\u{4e00}'..='\u{9fff}').contains(&c)).count();
    han > 50
}
