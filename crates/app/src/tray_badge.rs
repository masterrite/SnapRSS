//! The unread count on the tray icon, as QuiteRSS shows it: a small badge
//! drawn over the icon, and the number in the tooltip.

use std::sync::atomic::{AtomicI64, Ordering};

use tauri::image::Image;
use tauri::{AppHandle, Manager};

use crate::commands::AppState;

/// What the tray shows now, so an unchanged count is not redrawn. -1 until
/// the first update; -2 when the badge is switched off.
static SHOWN: AtomicI64 = AtomicI64::new(-1);

/// 3x5 digits and a plus sign, one row per 3 bits.
const GLYPHS: [(char, [u8; 5]); 11] = [
    ('0', [0b111, 0b101, 0b101, 0b101, 0b111]),
    ('1', [0b010, 0b110, 0b010, 0b010, 0b111]),
    ('2', [0b111, 0b001, 0b111, 0b100, 0b111]),
    ('3', [0b111, 0b001, 0b111, 0b001, 0b111]),
    ('4', [0b101, 0b101, 0b111, 0b001, 0b001]),
    ('5', [0b111, 0b100, 0b111, 0b001, 0b111]),
    ('6', [0b111, 0b100, 0b111, 0b101, 0b111]),
    ('7', [0b111, 0b001, 0b001, 0b001, 0b001]),
    ('8', [0b111, 0b101, 0b111, 0b101, 0b111]),
    ('9', [0b111, 0b101, 0b111, 0b001, 0b111]),
    ('+', [0b000, 0b010, 0b111, 0b010, 0b000]),
];

/// The text on the badge: the count up to 99, then "99+".
pub fn badge_text(n: i64) -> String {
    if n > 99 { "99+".into() } else { n.to_string() }
}

/// `base` (RGBA, `w` x `h`) with a red badge carrying `text` in its lower
/// right corner.
pub fn draw_badge(base: &[u8], w: u32, h: u32, text: &str) -> Vec<u8> {
    let mut px = base.to_vec();
    let (w, h) = (w as i64, h as i64);
    let scale = (h / 16).max(1);
    let glyph_w = 3 * scale;
    let gap = scale;
    let chars: Vec<char> = text.chars().collect();
    let text_w = chars.len() as i64 * glyph_w + (chars.len() as i64 - 1).max(0) * gap;
    let pad = scale;
    let bw = (text_w + 2 * pad).min(w);
    let bh = (5 * scale + 2 * pad).min(h);
    let (x0, y0) = (w - bw, h - bh);
    let mut put = |x: i64, y: i64, rgba: [u8; 4]| {
        if x >= 0 && y >= 0 && x < w && y < h {
            let i = ((y * w + x) * 4) as usize;
            px[i..i + 4].copy_from_slice(&rgba);
        }
    };
    const RED: [u8; 4] = [0xD1, 0x34, 0x38, 0xFF];
    const WHITE: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];
    for y in y0..h {
        for x in x0..w {
            // Clip the corners for a rounded look.
            let corner = (x == x0 || x == w - 1) && (y == y0 || y == h - 1);
            if !corner {
                put(x, y, RED);
            }
        }
    }
    let mut cx = x0 + pad + (bw - 2 * pad - text_w).max(0) / 2;
    let cy = y0 + pad;
    for c in chars {
        if let Some((_, rows)) = GLYPHS.iter().find(|(g, _)| *g == c) {
            for (ry, bits) in rows.iter().enumerate() {
                for rx in 0..3 {
                    if bits & (0b100 >> rx) != 0 {
                        for sy in 0..scale {
                            for sx in 0..scale {
                                put(cx + rx * scale + sx, cy + ry as i64 * scale + sy, WHITE);
                            }
                        }
                    }
                }
            }
        }
        cx += glyph_w + gap;
    }
    px
}

/// Bring the tray up to date with the unread count. Cheap when nothing
/// changed. Called whenever counts are read and after every update.
pub async fn refresh(app: &AppHandle) {
    let state = app.state::<AppState>();
    let (seq, unread, on) = {
        let db = state.db.lock().await;
        // Numbered while the count is read, so the order of the numbers is
        // the order of the counts. Two refreshes (the updater's and one after
        // marking read) could otherwise finish the wrong way round and leave
        // the older count on the tray.
        let seq = READ_SEQ.fetch_add(1, Ordering::AcqRel) + 1;
        let unread: i64 = db
            .conn()
            .query_row("SELECT COUNT(*) FROM news WHERE deleted = 0 AND read = 0", [], |r| r.get(0))
            .unwrap_or(0);
        let on = db
            .conn()
            .query_row("SELECT value FROM settings WHERE key = 'tray.show_unread'", [], |r| r.get::<_, String>(0))
            .map(|v| v != "0")
            .unwrap_or(true);
        (seq, unread, on)
    };
    // The icon is not drawn under the database lock: on Windows setting it
    // waits for the main thread, which can itself be waiting for that lock
    // while the window closes.
    let mut drawn = DRAWN_SEQ.lock().unwrap_or_else(|e| e.into_inner());
    if *drawn > seq {
        return;
    }
    *drawn = seq;
    show(app, unread, on);
}

static READ_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static DRAWN_SEQ: std::sync::Mutex<u64> = std::sync::Mutex::new(0);

fn show(app: &AppHandle, unread: i64, on: bool) {
    let want = if on { unread } else { -2 };
    if SHOWN.swap(want, Ordering::AcqRel) == want {
        return;
    }
    let Some(tray) = app.tray_by_id("main") else { return };
    let Some(icon) = app.default_window_icon() else { return };
    let tooltip = if on && unread > 0 { format!("SnapRSS · {unread} unread") } else { "SnapRSS".to_string() };
    let _ = tray.set_tooltip(Some(tooltip));
    let image = if on && unread > 0 {
        let rgba = draw_badge(icon.rgba(), icon.width(), icon.height(), &badge_text(unread));
        Image::new_owned(rgba, icon.width(), icon.height())
    } else {
        icon.clone()
    };
    let _ = tray.set_icon(Some(image));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_badge_caps_at_99_plus() {
        assert_eq!(badge_text(7), "7");
        assert_eq!(badge_text(99), "99");
        assert_eq!(badge_text(100), "99+");
    }

    #[test]
    fn the_badge_is_drawn_in_the_corner_and_nowhere_else() {
        let (w, h) = (32u32, 32u32);
        let base = vec![0x10u8; (w * h * 4) as usize];
        let out = draw_badge(&base, w, h, "12");
        let at = |x: u32, y: u32| &out[((y * w + x) * 4) as usize..((y * w + x) * 4 + 4) as usize];
        assert_eq!(at(0, 0), &[0x10, 0x10, 0x10, 0x10], "the top left is untouched");
        assert_eq!(at(w - 2, h - 2), &[0xD1, 0x34, 0x38, 0xFF], "the corner is the badge");
        let white = out.chunks(4).filter(|p| *p == [0xFF, 0xFF, 0xFF, 0xFF]).count();
        // "1" has 8 lit cells and "2" 11, each 2x2 at this size.
        assert_eq!(white, (8 + 11) * 4);
    }
}
