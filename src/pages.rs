//! What the island draws into its surfaces: page layouts and their texts. Drawing only; the
//! composition tree that shows and animates these surfaces lives in island.rs. DIPs.

use windows::core::Result;

use crate::{
    claude::{Limits, Session, State, Window},
    shelf::{Shelf, VISIBLE},
    media::{Art, Media},
    render::{Align, Canvas},
};

pub const TEXT: u32 = 0xFFF2F2F7;
pub const DIM: u32 = 0xFF8E8E93;
const FAINT: u32 = 0x33FFFFFF;
const DISABLED: u32 = 0xFF48484A;

/// One page under the tab bar.
pub const PAGE: (f32, f32) = (380.0, 130.0);

// Music page.
pub const ART: (f32, f32, f32) = (18.0, 6.0, 56.0); // x, y, size
/// The art is decoded and drawn once at this size; every place that shows it scales it.
pub const ART_SOURCE: f32 = 80.0;
pub const BAR: (f32, f32, f32, f32) = (18.0, 76.0, 344.0, 4.0);
pub const TIMES: (f32, f32, f32, f32) = (18.0, 82.0, 344.0, 14.0);
pub const BUTTON: f32 = 34.0;
pub const BUTTONS_Y: f32 = 94.0;
/// Left edges of shuffle, previous, play/pause, next, repeat.
pub const BUTTON_X: [f32; 5] =
    [PAGE.0 / 2.0 - 123.0, PAGE.0 / 2.0 - 70.0, PAGE.0 / 2.0 - 17.0, PAGE.0 / 2.0 + 36.0, PAGE.0 / 2.0 + 89.0];

pub const PREVIOUS: char = '\u{E892}';
pub const NEXT: char = '\u{E893}';
pub const PLAY: char = '\u{E768}';
pub const PAUSE: char = '\u{E769}';
pub const SHUFFLE: char = '\u{E8B1}';
pub const REPEAT_ALL: char = '\u{E8EE}';
pub const REPEAT_ONE: char = '\u{E8ED}';
const NOTE: char = '\u{E8D6}';

/// Title, artist, player and the progress track. The moving parts (art, progress fill,
/// times, buttons) are separate visuals.
pub fn music_page(c: &Canvas, media: Option<&Media>) -> Result<()> {
    let Some(m) = media else {
        // Art and buttons are hidden then, so the message has the whole page.
        c.text("Çalan bir şey yok", (18.0, 38.0, PAGE.0 - 36.0, 24.0), 13.5, true, DIM, Align::Center)?;
        return c.text("Spotify, tarayıcı ya da başka bir oynatıcıda müzik başlatın", (18.0, 62.0, PAGE.0 - 36.0, 20.0), 11.5, false, 0xFF636366, Align::Center);
    };
    let x = ART.0 + ART.2 + 12.0;
    let w = PAGE.0 - x - 18.0;
    let title = if m.title.is_empty() { "Bilinmeyen parça" } else { &m.title };
    c.text(title, (x, ART.1, w, 22.0), 14.0, true, TEXT, Align::Left)?;
    c.text(&m.artist, (x, ART.1 + 21.0, w, 18.0), 12.5, false, DIM, Align::Left)?;
    c.text(&m.app, (x, ART.1 + 39.0, w, 16.0), 11.0, false, 0xFF636366, Align::Left)?;
    if m.duration > 0.0 {
        c.round_rect(BAR, BAR.3 / 2.0, FAINT)?;
    }
    Ok(())
}

pub fn times(c: &Canvas, media: Option<&Media>, now_ms: u64) -> Result<()> {
    let Some(m) = media.filter(|m| m.duration > 0.0) else { return Ok(()) };
    let at = m.position_at(now_ms);
    let bounds = (0.0, 0.0, TIMES.2, TIMES.3);
    c.text(&clock(at), bounds, 10.5, false, DIM, Align::Left)?;
    c.text(&format!("-{}", clock(m.duration - at)), bounds, 10.5, false, DIM, Align::Right)
}

pub fn button(c: &Canvas, glyph: char, enabled: bool) -> Result<()> {
    let size = if glyph == PLAY || glyph == PAUSE { 20.0 } else { 15.0 };
    c.icon(glyph, (0.0, 0.0, BUTTON, BUTTON), size, if enabled { TEXT } else { DISABLED })
}

// Shelf page: two lists (files dropped here, new screenshots) picked with the segments on
// top, a row of up to five tiles, and a line of status and actions under them.
const TILE: (f32, f32) = (68.0, 72.0);
pub const SHELF_ICON: f32 = 44.0;
const DROP: u32 = 0xFF0A84FF;
const SELECTED: u32 = 0x400A84FF;
const ROW_Y: f32 = 28.0;
/// The two lists' segments, top left: files, screenshots.
pub const SEGMENTS: [(f32, f32, f32, f32); 2] = [(14.0, 0.0, 96.0, 22.0), (114.0, 0.0, 150.0, 22.0)];
/// "Tümünü seç", top right.
pub const SELECT_ALL: (f32, f32, f32, f32) = (PAGE.0 - 112.0, 0.0, 98.0, 22.0);
/// "Temizle" (or "Kaldır" for a selection), bottom right.
pub const SHELF_CLEAR: (f32, f32, f32, f32) = (PAGE.0 - 90.0, PAGE.1 - 24.0, 76.0, 22.0);

/// Where tile `slot` of `shown` on screen sits: a row centered on the page.
pub fn shelf_tile(slot: usize, shown: usize) -> (f32, f32, f32, f32) {
    let left = (PAGE.0 - shown as f32 * TILE.0) / 2.0;
    (left + slot as f32 * TILE.0, ROW_Y, TILE.0, TILE.1)
}

/// The shelf page showing `list` (0 files, 1 screenshots); `counts` are both lists' sizes.
/// `dropping` while files are dragged over the island.
pub fn shelf_page(c: &Canvas, shelf: &Shelf, list: usize, counts: [usize; 2], dropping: bool) -> Result<()> {
    for (i, (label, count)) in [("Dosyalar", counts[0]), ("Ekran görüntüleri", counts[1])].into_iter().enumerate() {
        let bounds = SEGMENTS[i];
        if i == list {
            c.round_rect(bounds, bounds.3 / 2.0, FAINT)?;
        }
        let text = if count > 0 { format!("{label} {count}") } else { label.to_string() };
        c.text(&text, bounds, 11.5, true, if i == list { TEXT } else { DIM }, Align::Center)?;
    }
    if dropping {
        c.outline((10.0, ROW_Y - 4.0, PAGE.0 - 20.0, PAGE.1 - ROW_Y), 14.0, 1.5, DROP, true)?;
    }
    if shelf.items.is_empty() {
        let (glyph, title, hint) = match (list, dropping) {
            (1, false) => ('\u{E722}', "Ekran görüntüsü yok", "Win + PrtScn ya da Ekran Alıntısı Aracı ile alınanlar burada görünür"),
            (_, true) => ('\u{E896}', "Bırakın", "Rafta beklerler; sonra istediğiniz yere sürükleyin"),
            _ => ('\u{E896}', "Dosyaları buraya sürükleyin", "Rafta beklerler; sonra istediğiniz yere sürükleyin"),
        };
        c.icon(glyph, (0.0, ROW_Y + 6.0, PAGE.0, 30.0), 22.0, if dropping { DROP } else { DIM })?; // Camera / Download
        c.text(title, (18.0, ROW_Y + 40.0, PAGE.0 - 36.0, 22.0), 13.5, true, if dropping { TEXT } else { DIM }, Align::Center)?;
        return c.text(hint, (18.0, ROW_Y + 62.0, PAGE.0 - 36.0, 18.0), 11.0, false, 0xFF636366, Align::Center);
    }
    let shown = shelf.visible().count();
    let icon_at = |(x, y, w, _): (f32, f32, f32, f32)| (x + (w - SHELF_ICON) / 2.0, y, SHELF_ICON, SHELF_ICON);
    for (slot, (_, item)) in shelf.visible().enumerate() {
        let tile = shelf_tile(slot, shown);
        if item.selected {
            c.round_rect((tile.0 + 2.0, tile.1 - 4.0, tile.2 - 4.0, tile.3 + 4.0), 10.0, SELECTED)?;
        }
        let icon = icon_at(tile);
        match item.icon.as_ref().and_then(Option::as_ref) {
            Some(art) => c.image(&art.pixels, art.size, icon)?,
            None => c.icon('\u{E8A5}', icon, 28.0, DIM)?, // Document
        }
        c.text(&item.name, (tile.0 + 3.0, tile.1 + SHELF_ICON + 6.0, tile.2 - 6.0, 16.0), 10.5, false, TEXT, Align::Center)?;
        if item.selected {
            let (cx, cy) = (icon.0 + icon.2 - 3.0, icon.1 + 3.0);
            c.circle((cx, cy), 7.5, DROP)?;
            c.icon('\u{E73E}', (cx - 7.5, cy - 7.5, 15.0, 15.0), 9.0, TEXT)?; // CheckMark
        }
    }
    let (selected, total) = (shelf.selected(), shelf.items.len());
    let status = if selected > 0 {
        format!("{selected} seçili · sürükleyip bırakın")
    } else if total > VISIBLE {
        format!("{}–{} / {total} · tekerlekle kaydırın", shelf.offset + 1, shelf.offset + shown)
    } else {
        "Tıkla: seç · Çift tıkla: aç · Sürükle: bırak".to_string()
    };
    c.text(&status, (18.0, SHELF_CLEAR.1, 268.0, SHELF_CLEAR.3), 11.0, false, 0xFF636366, Align::Left)?;
    c.text(if selected == total { "Seçimi kaldır" } else { "Tümünü seç" }, SELECT_ALL, 11.5, true, DIM, Align::Right)?;
    c.text(if selected > 0 { "Kaldır" } else { "Temizle" }, SHELF_CLEAR, 11.5, true, DIM, Align::Right)
}

/// The closed island's two lines: a bold title and a dim line under it, `w` wide.
pub fn two_lines(c: &Canvas, title: &str, detail: &str, w: f32, h: f32) -> Result<()> {
    if detail.is_empty() {
        return c.text(title, (0.0, 0.0, w, h), 12.5, true, TEXT, Align::Left);
    }
    let top = h / 2.0 - 14.5;
    c.text(title, (0.0, top, w, 16.5), 12.0, true, TEXT, Align::Left)?;
    c.text(detail, (0.0, top + 15.5, w, 13.5), 10.0, false, DIM, Align::Left)
}

/// Diameter of the 5-hour limit ring.
pub const RING: f32 = 22.0;

/// How much of the 5-hour limit is used: a ring that fills, the percent inside.
pub fn limit_ring(c: &Canvas, center: (f32, f32), used: f32, color: u32) -> Result<()> {
    c.ring(center, RING / 2.0 - 1.5, 2.2, used / 100.0, color, FAINT)?;
    let box_ = (center.0 - RING / 2.0, center.1 - RING / 2.0, RING, RING);
    c.text(&format!("{used:.0}"), box_, if used >= 99.5 { 7.5 } else { 8.5 }, true, TEXT, Align::Center)
}

/// The volume peek in a `(w, h)` line: speaker, level bar, number.
pub fn volume(c: &Canvas, (w, h): (f32, f32), level: u32, muted: bool) -> Result<()> {
    let glyph = match level {
        _ if muted => '\u{E74F}', // Mute
        0 => '\u{E992}',
        1..=33 => '\u{E993}',
        34..=66 => '\u{E994}',
        _ => '\u{E995}',
    };
    let color = if muted { DIM } else { TEXT };
    c.icon(glyph, (12.0, 0.0, 24.0, h), 15.0, color)?;
    let bar = (44.0, h / 2.0 - 2.0, w - 44.0 - 50.0, 4.0);
    c.round_rect(bar, 2.0, FAINT)?;
    if level > 0 {
        c.round_rect((bar.0, bar.1, (bar.2 * level.min(100) as f32 / 100.0).max(bar.3), bar.3), 2.0, color)?;
    }
    c.text(&level.to_string(), (w - 46.0, 0.0, 30.0, h), 12.5, true, color, Align::Right)
}

/// Shuffle or repeat: bright with a dot under it when on, dim when off.
pub fn toggle(c: &Canvas, glyph: char, on: bool) -> Result<()> {
    c.icon(glyph, (0.0, -3.0, BUTTON, BUTTON), 14.0, if on { TEXT } else { DIM })?;
    if on {
        c.circle((BUTTON / 2.0, BUTTON - 7.0), 1.5, TEXT)?;
    }
    Ok(())
}

pub fn art(c: &Canvas, art: Option<&Art>) -> Result<()> {
    let bounds = (0.0, 0.0, ART_SOURCE, ART_SOURCE);
    // A backdrop, so art with transparent parts (site logos) does not melt into the black pill.
    c.round_rect(bounds, 0.0, 0xFF2C2C2E)?;
    match art.filter(|a| !blank(a)) {
        Some(a) => c.image(&a.pixels, a.size, bounds),
        None => c.icon(NOTE, bounds, ART_SOURCE * 0.36, DIM),
    }
}

/// Art that shows nothing: all but invisible or all black (some sites hand players such a
/// thumbnail). The placeholder note says more.
pub fn blank(art: &Art) -> bool {
    let pixels = art.pixels.as_chunks::<4>().0;
    let visible = pixels.iter().filter(|p| p[3] > 32 && p[..3].iter().any(|&c| c > 16)).count();
    visible * 20 < pixels.len() // less than 5% of the art has anything to see
}

/// Claude's page: plan limits on top, open sessions below (the ones that need the user first).
pub fn claude_page(c: &Canvas, sessions: &[Session], limits: &Limits, now: u64) -> Result<()> {
    const X: f32 = 18.0;
    const RIGHT: f32 = PAGE.0 - 18.0;
    if limits.five_hour.is_none() && limits.seven_day.is_none() {
        c.text("Limit bilgisi yok", (X, 0.0, 200.0, 18.0), 12.0, true, TEXT, Align::Left)?;
        c.text("Claude Code'da oturum açılınca gelir", (X, 18.0, RIGHT - X, 18.0), 11.5, false, DIM, Align::Left)?;
    } else {
        // Label, bar filled by what is used, "%23 kullanıldı", time to the reset.
        for (row, (label, window)) in [("5 saat", limits.five_hour), ("Haftalık", limits.seven_day)].into_iter().enumerate() {
            let y = row as f32 * 20.0;
            let limit = limit_text(window, limits.updated, now);
            c.text(label, (X, y, 56.0, 18.0), 12.0, true, TEXT, Align::Left)?;
            let (bar_x, bar_w) = (X + 58.0, 84.0);
            c.round_rect((bar_x, y + 6.0, bar_w, 6.0), 3.0, FAINT)?;
            if limit.used > 0.0 {
                let fill = crate::claude::usage_color(limit.used * 100.0);
                c.round_rect((bar_x, y + 6.0, (bar_w * limit.used).max(6.0), 6.0), 3.0, fill)?;
            }
            c.text(&limit.used_text, (bar_x + bar_w + 8.0, y, 104.0, 18.0), 11.5, false, TEXT, Align::Left)?;
            if limit.renews {
                c.icon('\u{E72C}', (RIGHT - 92.0, y, 14.0, 18.0), 9.5, DIM)?; // Refresh, before the time
            }
            c.text(&limit.when, (RIGHT - 76.0, y, 76.0, 18.0), 11.0, false, DIM, Align::Left)?;
        }
    }

    if sessions.is_empty() {
        return c.text("Açık Claude oturumu yok", (X, SESSIONS_Y, RIGHT - X, ROW_H), 12.5, false, DIM, Align::Left);
    }
    let rows = session_rows(sessions);
    for (i, session) in rows.iter().enumerate() {
        let y = SESSIONS_Y + i as f32 * ROW_H;
        c.circle((X + 4.0, y + 10.0), 3.5, session.state.color())?;
        let name = if session.project.is_empty() { "Claude" } else { &session.project };
        c.text(name, (X + 14.0, y, 200.0, 20.0), 12.5, true, TEXT, Align::Left)?;
        c.text(session.state.label(), (X + 220.0, y, RIGHT - X - 220.0, 20.0), 12.0, false, session.state.color(), Align::Right)?;
    }
    if sessions.len() > rows.len() {
        let y = SESSIONS_Y + rows.len() as f32 * ROW_H;
        c.text(&format!("+{} oturum daha", sessions.len() - rows.len()), (X + 14.0, y, 200.0, ROW_H), 12.0, false, DIM, Align::Left)?;
    }
    Ok(())
}

/// Top of the session rows on the Claude page, and their height.
pub const SESSIONS_Y: f32 = 46.0;
pub const ROW_H: f32 = 20.0;

/// The sessions the Claude page lists, top to bottom: the ones that need the user first.
/// Four fit; with more, the last row says how many are left out.
pub fn session_rows(sessions: &[Session]) -> Vec<&Session> {
    const ROWS: usize = 4;
    let mut ordered: Vec<&Session> = sessions.iter().collect();
    ordered.sort_by_key(|s| match s.state {
        State::Permission => 0,
        State::Done => 1,
        State::Working => 2,
        State::Open => 3,
    });
    if ordered.len() > ROWS {
        ordered.truncate(ROWS - 1);
    }
    ordered
}

/// One limit row's figures.
#[derive(Debug, PartialEq)]
pub struct LimitText {
    pub used: f32, // share used, 0..1
    pub used_text: String,
    pub when: String, // time to the reset, or how old the figures are
    pub renews: bool, // `when` is the time to the reset
}

pub fn limit_text(window: Option<Window>, updated: u64, now: u64) -> LimitText {
    let Some(w) = window else { return LimitText { used: 0.0, used_text: "bilgi yok".into(), when: String::new(), renews: false } };
    if now >= w.resets_at {
        return LimitText { used: 0.0, used_text: "%0 kullanıldı".into(), when: "sıfırlandı".into(), renews: false };
    }
    let used = w.used_percentage.clamp(0.0, 100.0);
    let age = now.saturating_sub(updated);
    // Figures older than half an hour say so instead of a countdown that may be stale.
    let (when, renews) = if age > 30 * 60 {
        (format!("{} önce", duration_text(age)), false)
    } else {
        (duration_text(w.resets_at - now), true)
    };
    LimitText { used: (used / 100.0) as f32, used_text: format!("%{used:.0} kullanıldı"), when, renews }
}

fn duration_text(seconds: u64) -> String {
    let minutes = seconds / 60;
    match (minutes / (24 * 60), minutes / 60 % 24, minutes % 60) {
        (0, 0, 0) => "1 dk".into(),
        (0, 0, m) => format!("{m} dk"),
        (0, h, m) => format!("{h} sa {m} dk"),
        (d, h, _) => format!("{d} g {h} sa"),
    }
}

/// 83.4 → "1:23", 3725 → "1:02:05".
pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// A color from the art for the equalizer and the progress bar: its average, brightened to
/// read on black. Gray art gets a neutral light instead of a muddy gray.
pub fn accent(art: &Art) -> u32 {
    let (mut sum, mut n) = ([0u64; 3], 0u64);
    for px in art.pixels.as_chunks::<4>().0 {
        for (total, channel) in sum.iter_mut().zip(px) {
            *total += *channel as u64;
        }
        n += 1;
    }
    if n == 0 {
        return TEXT;
    }
    let [b, g, r] = sum.map(|v| v as f32 / n as f32);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    if max < 8.0 || (max - min) / max < 0.18 {
        return 0xFFE5E5EA;
    }
    let lift = |v: f32| ((v * 235.0 / max).min(255.0)) as u32;
    0xFF00_0000 | lift(r) << 16 | lift(g) << 8 | lift(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_texts() {
        assert_eq!(duration_text(30), "1 dk");
        assert_eq!(duration_text(13 * 60), "13 dk");
        assert_eq!(duration_text(2 * 3600 + 13 * 60), "2 sa 13 dk");
        assert_eq!(duration_text(3 * 86400 + 4 * 3600), "3 g 4 sa");
        let w = Window { used_percentage: 42.4, resets_at: 10_000 };
        let fresh = limit_text(Some(w), 1_000, 1_000 + 60);
        assert!((fresh.used - 0.424).abs() < 1e-4);
        assert_eq!((fresh.used_text.as_str(), fresh.when.as_str(), fresh.renews), ("%42 kullanıldı", "2 sa 29 dk", true));
        let reset = limit_text(Some(w), 0, 10_000);
        assert_eq!((reset.used, reset.used_text.as_str(), reset.when.as_str()), (0.0, "%0 kullanıldı", "sıfırlandı"));
        let stale = limit_text(Some(w), 0, 3_000); // figures 50 minutes old
        assert_eq!((stale.when.as_str(), stale.renews), ("50 dk önce", false));
        assert_eq!(limit_text(None, 0, 0).used_text, "bilgi yok");
    }

    #[test]
    fn clocks() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(83.4), "1:23");
        assert_eq!(clock(3725.0), "1:02:05");
        assert_eq!(clock(-3.0), "0:00");
    }

    #[test]
    fn accents() {
        let solid = |b: u8, g: u8, r: u8| Art { size: 2, pixels: [b, g, r, 255].repeat(4) };
        // A dark blue gets brightened but stays blue.
        assert_eq!(accent(&solid(100, 20, 10)), 0xFF172FEB);
        // Gray art: a neutral light.
        assert_eq!(accent(&solid(90, 90, 92)), 0xFFE5E5EA);
        assert_eq!(accent(&Art { size: 0, pixels: vec![] }), TEXT);
    }

    #[test]
    fn blank_art() {
        let solid = |px: [u8; 4]| Art { size: 10, pixels: px.repeat(100) };
        assert!(blank(&solid([0, 0, 0, 255]))); // black
        assert!(blank(&solid([0, 0, 0, 0]))); // transparent
        assert!(!blank(&solid([40, 60, 200, 255]))); // a real color
        // A small logo on a transparent square still counts once it covers 5%.
        let mut logo = [0u8, 0, 0, 0].repeat(100);
        for p in logo.as_chunks_mut::<4>().0.iter_mut().take(10) {
            *p = [200, 200, 200, 255];
        }
        assert!(!blank(&Art { size: 10, pixels: logo }));
    }

    #[test]
    fn shelf_tiles_are_centered_and_fit() {
        for shown in 1..=VISIBLE {
            let (first, last) = (shelf_tile(0, shown), shelf_tile(shown - 1, shown));
            assert!((first.0 - (PAGE.0 - (last.0 + last.2))).abs() < 0.01, "{shown} tiles: row not centered");
            // Between the segments on top and the status line, highlight included.
            assert!(first.0 >= 18.0 && first.1 - 4.0 >= SEGMENTS[0].3 && last.1 + last.3 <= SHELF_CLEAR.1, "{shown} tiles: row does not fit");
        }
        assert!(SEGMENTS[0].0 + SEGMENTS[0].2 <= SEGMENTS[1].0 && SEGMENTS[1].0 + SEGMENTS[1].2 <= SELECT_ALL.0);
    }

    #[test]
    fn buttons_are_centered() {
        let center = |i: usize| BUTTON_X[i] + BUTTON / 2.0;
        assert_eq!(center(2), PAGE.0 / 2.0);
        for i in 1..5 {
            assert_eq!(center(i) - center(i - 1), 53.0, "button {i} is off the grid");
        }
        assert!(BUTTON_X[0] >= 18.0 && BUTTON_X[4] + BUTTON <= PAGE.0 - 18.0);
    }
}
