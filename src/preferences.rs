//! The settings window: a sidebar of sections and, beside it, grouped rows of switches and
//! lists, in the manner of Apple's settings. Drawn like the island, into one composition
//! surface that is redrawn when something on it changes; it exists only while it is open.

use std::time::Instant;

use windows::{
    core::*,
    UI::Composition::{CompositionDrawingSurface, Compositor, Desktop::DesktopWindowTarget, SpriteVisual},
    Win32::{
        Foundation::*,
        Graphics::{Dwm::*, Gdi::*},
        System::{LibraryLoader::GetModuleHandleW, Registry::*, WinRT::Composition::ICompositorDesktopInterop},
        UI::{
            HiDpi::*,
            Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT, VIRTUAL_KEY, VK_DOWN, VK_ESCAPE, VK_UP},
            WindowsAndMessaging::*,
        },
    },
};

use crate::{
    liked::Song,
    pages::LIKED,
    render::{Align, Canvas, Painter},
    settings::Settings,
    text::{tr, Language},
};

type Rect = (f32, f32, f32, f32);

/// The window's inside, DIPs.
const SIZE: (f32, f32) = (760.0, 540.0);
const SIDEBAR: f32 = 220.0;
const LEFT: f32 = SIDEBAR + 28.0; // the cards' left edge
const WIDTH: f32 = SIZE.0 - LEFT - 28.0;
const BODY: f32 = 64.0; // where the part under the page title starts; it scrolls
/// The app picker, over the dimmed window: a title, rows of apps, a cancel button.
const SHEET: Rect = (SIZE.0 / 2.0 - 210.0, 48.0, 420.0, SIZE.1 - 96.0);
const SHEET_ROWS_Y: f32 = 72.0;
const SHEET_ROW: f32 = 46.0;
const SHEET_FITS: usize = ((SHEET.3 - SHEET_ROWS_Y - 64.0) / SHEET_ROW) as usize;
const FLIP_MS: f32 = 180.0; // a switch sliding over
const FLIP_TIMER: usize = 1;
const WM_DEVICE_LOST: u32 = WM_APP + 1;
const WM_MOUSELEAVE: u32 = 0x02A3; // in Win32_UI_Controls, like the island's
const CLASS: PCWSTR = w!("NightdijitalIslandSettings");
const APP: char = '\u{ECAA}'; // AppIconDefault, before an app's name
/// The hover delays offered, ms.
const DELAYS: [(u32, &str, &str); 4] = [(0, "Anında", "Instant"), (150, "Kısa", "Short"), (300, "Normal", "Normal"), (700, "Uzun", "Long")];

struct Theme {
    sidebar: u32,
    content: u32,
    card: u32,
    line: u32,
    text: u32,
    dim: u32,
    hover: u32,
    accent: u32,
    on: u32,       // a switch that is on
    off: u32,      // and off
    segments: u32, // the segmented control's track
    segment: u32,  // its chosen segment
    shade: u32,    // over the window while the picker is open
}

const DARK: Theme = Theme {
    sidebar: 0xFF252527,
    content: 0xFF1C1C1E,
    card: 0xFF2C2C2E,
    line: 0xFF3A3A3C,
    text: 0xFFF5F5F7,
    dim: 0xFF98989D,
    hover: 0x14FFFFFF,
    accent: 0xFF0A84FF,
    on: 0xFF30D158,
    off: 0xFF48484A,
    segments: 0xFF1C1C1E,
    segment: 0xFF5A5A5E,
    shade: 0x99000000,
};

const LIGHT: Theme = Theme {
    sidebar: 0xFFE8E8ED,
    content: 0xFFF2F2F7,
    card: 0xFFFFFFFF,
    line: 0xFFE5E5EA,
    text: 0xFF1C1C1E,
    dim: 0xFF6E6E73,
    hover: 0x0F000000,
    accent: 0xFF007AFF,
    on: 0xFF34C759,
    off: 0xFFE3E3E8,
    segments: 0xFFEEEEF0,
    segment: 0xFFFFFFFF,
    shade: 0x59000000,
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Section {
    #[default]
    General,
    Notifications,
    Fullscreen,
    Music,
    Liked,
    Claude,
    Shelf,
    About,
}

const SECTIONS: [Section; 8] = [
    Section::General,
    Section::Notifications,
    Section::Fullscreen,
    Section::Music,
    Section::Liked,
    Section::Claude,
    Section::Shelf,
    Section::About,
];

impl Section {
    fn title(self) -> &'static str {
        match self {
            Section::General => tr("Genel", "General"),
            Section::Notifications => tr("Bildirimler", "Notifications"),
            Section::Fullscreen => tr("Tam ekran", "Fullscreen"),
            Section::Music => tr("Müzik", "Music"),
            Section::Liked => tr("Beğenilenler", "Liked"),
            Section::Claude => "Claude",
            Section::Shelf => tr("Raf", "Shelf"),
            Section::About => tr("Hakkında", "About"),
        }
    }

    /// Its glyph in the sidebar and the color of the square behind it.
    fn icon(self) -> (char, u32) {
        match self {
            Section::General => ('\u{E713}', 0xFF8E8E93),    // Setting
            Section::Notifications => ('\u{EA8F}', 0xFFFF9500), // Ringer
            Section::Fullscreen => ('\u{E740}', 0xFF5E5CE6), // FullScreen
            Section::Music => ('\u{E8D6}', 0xFFFF2D55),      // Audio
            Section::Liked => ('\u{EB52}', 0xFFFF3B30),      // HeartFill
            Section::Claude => (' ', 0xFFD97757),            // Claude's mark is drawn
            Section::Shelf => ('\u{E7B8}', 0xFF0A84FF),      // Package
            Section::About => ('\u{E946}', 0xFF32ADE6),      // Info
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Switch {
    Autostart, // a registry value, not in the settings file
    Hover,
    PrivacyDot,
    Equalizer,
    Glow,
    SongPeek,
    VolumePeek,
    Claude, // Claude Code's own settings, not in the settings file
    ClaudePeek,
    Screenshots,
    Notifications,
    DoNotDisturb,
}

impl Switch {
    /// The setting this switch shows, when it lives in the settings file.
    fn get(self, s: &Settings) -> Option<bool> {
        Some(match self {
            Switch::Hover => s.expand_on_hover,
            Switch::PrivacyDot => s.privacy_dot,
            Switch::Equalizer => s.equalizer,
            Switch::Glow => s.glow,
            Switch::SongPeek => s.song_peek,
            Switch::VolumePeek => s.volume_peek,
            Switch::ClaudePeek => s.claude_peek,
            Switch::Screenshots => s.screenshots,
            Switch::Notifications => s.notifications,
            Switch::DoNotDisturb => s.do_not_disturb,
            Switch::Autostart | Switch::Claude => return None,
        })
    }

    /// Turns the setting over; false for a switch that is not in the settings file.
    pub fn flip(self, s: &mut Settings) -> bool {
        let field = match self {
            Switch::Hover => &mut s.expand_on_hover,
            Switch::PrivacyDot => &mut s.privacy_dot,
            Switch::Equalizer => &mut s.equalizer,
            Switch::Glow => &mut s.glow,
            Switch::SongPeek => &mut s.song_peek,
            Switch::VolumePeek => &mut s.volume_peek,
            Switch::ClaudePeek => &mut s.claude_peek,
            Switch::Screenshots => &mut s.screenshots,
            Switch::Notifications => &mut s.notifications,
            Switch::DoNotDisturb => &mut s.do_not_disturb,
            Switch::Autostart | Switch::Claude => return false,
        };
        *field = !*field;
        true
    }
}

/// The two lists of apps: hidden in front of, shown over even in fullscreen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum List {
    Hide,
    Show,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Link {
    Website,
    SettingsFile,
    Log,
}

/// What a click on the window lands on.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Section(Section),
    Toggle(Switch),
    Delay(u32),
    Language(Language),
    Pick(List), // opens the app picker
    Add(List, String),
    Close, // the picker
    Remove(List, usize),
    Unlike(usize),
    Search(usize),
    Open(Link),
}

/// What the window asks of the island after a message.
pub enum Event {
    Unhandled,
    Nothing,
    Redraw,
    Act(Action),
    Closed,
}

/// What the window shows.
pub struct Model<'a> {
    pub settings: &'a Settings,
    pub liked: &'a [Song],
    pub autostart: bool,
    pub claude: Option<bool>, // None: Claude Code is not installed
}

struct Picker {
    list: List,
    apps: Vec<(String, String)>, // exe, a window title
    first: usize,                // first row on screen
}

impl Picker {
    /// The apps not on the list yet.
    fn shown<'a>(&'a self, s: &Settings) -> Vec<&'a (String, String)> {
        let list = if self.list == List::Hide { &s.hide_exe } else { &s.show_exe };
        self.apps.iter().filter(|(exe, _)| !list.contains(exe)).collect()
    }
}

#[derive(Default)]
struct State {
    section: Section,
    scroll: f32,
    hover: Option<Action>,
    picker: Option<Picker>,
    dark: bool,
}

/// One row of a group.
enum Row<'a> {
    Switch(Switch, &'a str, &'a str, Option<bool>), // label, detail, on (None: cannot be used)
    Segments(&'a str, Vec<(&'a str, Action, bool)>, bool), // label, (name, action, chosen), can be used
    App(List, usize, &'a str),
    Add(List),
    Song(usize, &'a Song),
    Link(Link, char, &'a str, &'a str), // glyph, label, detail
    Empty(char, &'a str, &'a str),      // glyph, title, hint
}

impl Row<'_> {
    fn height(&self) -> f32 {
        match self {
            Row::Switch(_, _, detail, _) if detail.is_empty() => 44.0,
            Row::Switch(..) => 58.0,
            Row::Segments(..) => 50.0,
            Row::App(..) | Row::Add(_) => 42.0,
            Row::Song(..) => 54.0,
            Row::Link(..) => 46.0,
            Row::Empty(..) => 140.0,
        }
    }
}

/// Draws top to bottom and notes where each control is.
struct Form<'a, 'c> {
    c: &'a Canvas<'c>,
    t: &'static Theme,
    hover: Option<&'a Action>,
    flip: Option<(Switch, f32)>, // the switch sliding over, and how far (0..1)
    hits: Vec<(Rect, Action)>,
    view: Rect, // controls outside it cannot be clicked
    y: f32,
}

/// Paints the whole window; returns where the controls are and how tall the page is.
fn paint(c: &Canvas, state: &State, flip: Option<(Switch, f32)>, m: &Model) -> Result<(Vec<(Rect, Action)>, f32)> {
    let t = if state.dark { &DARK } else { &LIGHT };
    let window = (0.0, 0.0, SIZE.0, SIZE.1);
    let mut f = Form { c, t, hover: state.hover.as_ref(), flip, hits: Vec::new(), view: window, y: 0.0 };
    c.round_rect(window, 0.0, t.content)?;
    f.sidebar(state.section)?;
    // The page's title stays where it is; what is under it scrolls.
    c.text(state.section.title(), (LEFT, 14.0, WIDTH, 36.0), 21.0, true, t.text, Align::Left)?;
    if state.scroll > 0.5 {
        c.round_rect((SIDEBAR, BODY - 1.0, SIZE.0 - SIDEBAR, 1.0), 0.0, t.line)?;
    }
    let body = (SIDEBAR, BODY, SIZE.0 - SIDEBAR, SIZE.1 - BODY);
    f.view = body;
    f.y = BODY + 6.0 - state.scroll;
    let top = f.y;
    c.clipped(body, |_| f.page(state.section, m))?;
    let height = f.y - top;
    if let Some(picker) = &state.picker {
        f.hits.clear();
        f.view = window;
        f.picker(picker, m)?;
    }
    Ok((f.hits, height))
}

impl Form<'_, '_> {
    fn hit(&mut self, (x, y, w, h): Rect, action: Action) {
        let (vx, vy, vw, vh) = self.view;
        let (left, top) = (x.max(vx), y.max(vy));
        let (right, bottom) = ((x + w).min(vx + vw), (y + h).min(vy + vh));
        if right > left && bottom > top {
            self.hits.push(((left, top, right - left, bottom - top), action));
        }
    }

    fn hovered(&self, action: &Action) -> bool {
        self.hover == Some(action)
    }

    fn sidebar(&mut self, chosen: Section) -> Result<()> {
        let (c, t) = (self.c, self.t);
        c.round_rect((0.0, 0.0, SIDEBAR, SIZE.1), 0.0, t.sidebar)?;
        c.round_rect((SIDEBAR - 1.0, 0.0, 1.0, SIZE.1), 0.0, t.line)?;
        app_mark(c, (18.0, 16.0), 36.0)?;
        c.text("Dynamic Island", (64.0, 17.0, SIDEBAR - 76.0, 18.0), 13.5, true, t.text, Align::Left)?;
        c.text(&version(), (64.0, 35.0, SIDEBAR - 76.0, 16.0), 11.0, false, t.dim, Align::Left)?;
        for (i, section) in SECTIONS.into_iter().enumerate() {
            let y = 72.0 + i as f32 * 34.0 + if section == Section::About { 14.0 } else { 0.0 };
            let row = (10.0, y, SIDEBAR - 20.0, 30.0);
            let action = Action::Section(section);
            if section == chosen {
                c.round_rect(row, 7.0, t.accent)?;
            } else if self.hovered(&action) {
                c.round_rect(row, 7.0, t.hover)?;
            }
            let (glyph, color) = section.icon();
            let square = (20.0, y + 5.0, 20.0, 20.0);
            c.round_rect(square, 5.0, color)?;
            if section == Section::Claude {
                c.spark((30.0, y + 15.0), 7.0, 0xFFFFFFFF)?;
            } else {
                c.icon(glyph, square, 11.0, 0xFFFFFFFF)?;
            }
            let color = if section == chosen { 0xFFFFFFFF } else { t.text };
            c.text(section.title(), (50.0, y, SIDEBAR - 70.0, 30.0), 13.0, false, color, Align::Left)?;
            self.hit(row, action);
        }
        Ok(())
    }

    fn page(&mut self, section: Section, m: &Model) -> Result<()> {
        let s = m.settings;
        match section {
            Section::General => {
                let languages = [(tr("Otomatik", "Automatic"), Language::Auto), ("Türkçe", Language::Turkish), ("English", Language::English)];
                let languages = languages.into_iter().map(|(name, l)| (name, Action::Language(l), s.language == l)).collect();
                let autostart = (tr("Başlangıçta çalıştır", "Start with Windows"), tr("Windows açılınca island da açılır", "The island opens when Windows starts"));
                self.group(
                    "",
                    &[Row::Switch(Switch::Autostart, autostart.0, autostart.1, Some(m.autostart)), Row::Segments(tr("Dil", "Language"), languages, true)],
                    "",
                )?;
                let delays = DELAYS.into_iter().map(|(ms, turkish, english)| (tr(turkish, english), Action::Delay(ms), ms == s.hover_delay_ms)).collect();
                let hover = (tr("Üzerine gelince aç", "Open on hover"), tr("Kapalıyken island'a tıklayarak açarsınız", "When off, click the island to open it"));
                self.group(
                    tr("Açılma", "Opening"),
                    &[
                        Row::Switch(Switch::Hover, hover.0, hover.1, Switch::Hover.get(s)),
                        Row::Segments(tr("Bekleme süresi", "Delay"), delays, s.expand_on_hover),
                    ],
                    "",
                )?;
                let label = tr("Mikrofon ve kamera göstergesi", "Microphone and camera indicator");
                let detail = tr("Kullanılırken island'da turuncu ya da yeşil bir nokta yanar", "An orange or green dot lights up on the island while in use");
                self.group(tr("Gizlilik", "Privacy"), &[Row::Switch(Switch::PrivacyDot, label, detail, Switch::PrivacyDot.get(s))], "")
            }
            Section::Notifications => {
                let previews = (
                    tr("Bildirim önizlemeleri", "Notification previews"),
                    tr("Bilgisayara gelen bildirimler island'da kısa süre görünür", "New notifications show on the island for a moment"),
                );
                let quiet = (
                    tr("Rahatsız etmeyin", "Do not disturb"),
                    tr("Island hiçbir şey için kendiliğinden açılmaz", "The island opens by itself for nothing"),
                );
                self.group(
                    "",
                    &[
                        Row::Switch(Switch::Notifications, previews.0, previews.1, Switch::Notifications.get(s)),
                        Row::Switch(Switch::DoNotDisturb, quiet.0, quiet.1, Switch::DoNotDisturb.get(s)),
                    ],
                    tr(
                        "Rahatsız etmeyin, Windows'un ayarından ayrıdır; tepsi menüsünden de açılıp kapanır. Bildirimler iki saniyede bir okunur; island gizliyken ya da rahatsız etmeyin açıkken okunmaz.",
                        "Do not disturb is apart from Windows' own and can be switched from the tray menu too. Notifications are read every two seconds, never while the island is hidden or not to be disturbed.",
                    ),
                )
            }
            Section::Fullscreen => {
                self.note(tr(
                    "Island tam ekran uygulamalarda ve oyunlarda kendiliğinden gizlenir. Kendiliğinden anlaşılmayan durumlar için buraya uygulama ekleyin.",
                    "The island hides by itself in fullscreen apps and games. Add apps here for the cases it cannot tell on its own.",
                ))?;
                let lists = [
                    (
                        List::Hide,
                        tr("Her zaman gizle", "Always hide"),
                        tr("Bu uygulamalar öndeyken island gizlenir, tam ekran olmasalar da.", "The island hides while these apps are in front, fullscreen or not."),
                        &s.hide_exe,
                    ),
                    (
                        List::Show,
                        tr("Her zaman göster", "Always show"),
                        tr("Bu uygulamalar tam ekrana geçse de island görünür kalır.", "The island stays visible even when these apps go fullscreen."),
                        &s.show_exe,
                    ),
                ];
                for (list, header, footer, names) in lists {
                    let mut rows: Vec<Row> = names.iter().enumerate().map(|(i, name)| Row::App(list, i, name.as_str())).collect();
                    rows.push(Row::Add(list));
                    self.group(header, &rows, footer)?;
                }
                Ok(())
            }
            Section::Music => {
                self.group(
                    tr("Kapalı island", "Closed island"),
                    &[
                        Row::Switch(Switch::Equalizer, tr("Ekolayzır", "Equalizer"), tr("Müzik çalarken hareket eden çubuklar", "Moving bars while music plays"), Switch::Equalizer.get(s)),
                        Row::Switch(
                            Switch::Glow,
                            tr("Kapak rengi parıltısı", "Cover color glow"),
                            tr("Island'ın kenarı albüm kapağının renginde parlar", "The island's edge glows in the album cover's color"),
                            Switch::Glow.get(s),
                        ),
                    ],
                    "",
                )?;
                self.group(
                    tr("Kısa gösterimler", "Peeks"),
                    &[
                        Row::Switch(
                            Switch::SongPeek,
                            tr("Yeni şarkı", "New song"),
                            tr("Şarkı değişince island kısa süre açılır", "The island opens for a moment when the song changes"),
                            Switch::SongPeek.get(s),
                        ),
                        Row::Switch(
                            Switch::VolumePeek,
                            tr("Ses seviyesi", "Volume"),
                            tr("Sistem sesi değişince seviye görünür", "Shows the level when the system volume changes"),
                            Switch::VolumePeek.get(s),
                        ),
                    ],
                    tr(
                        "Kalbe dokunduğunuz şarkılar Beğenilenler'de toplanır. Island'ın üzerinde fare tekerleği oynatıcılar arasında geçer.",
                        "Songs you heart are collected under Liked. The mouse wheel over the island moves between players.",
                    ),
                )
            }
            Section::Liked => {
                if m.liked.is_empty() {
                    let hint = tr("Müzik çalarken island'ın müzik sayfasındaki kalbe dokunun", "While music plays, tap the heart on the island's music page");
                    return self.group("", &[Row::Empty('\u{EB51}', tr("Henüz beğenilen şarkı yok", "No liked songs yet"), hint)], "");
                }
                let rows: Vec<Row> = m.liked.iter().enumerate().map(|(i, song)| Row::Song(i, song)).collect();
                let n = m.liked.len();
                let header = tr(format!("{n} şarkı"), if n == 1 { "1 song".into() } else { format!("{n} songs") });
                self.group(&header, &rows, tr("Büyüteç şarkıyı YouTube Music'te arar.", "The magnifier looks the song up on YouTube Music."))
            }
            Section::Claude => {
                let detail = match m.claude {
                    None => tr("Claude Code bu bilgisayarda kurulu değil", "Claude Code is not installed on this computer"),
                    Some(_) => tr("Oturumlar ve kullanım limitleri island'da görünür", "Sessions and usage limits show on the island"),
                };
                let notify = tr("İzin isteyince ya da cevap bitince island kısa süre açılır", "Opens the island when Claude needs you or is done");
                self.group(
                    "",
                    &[
                        Row::Switch(Switch::Claude, tr("Claude Code bağlantısı", "Claude Code connection"), detail, m.claude),
                        Row::Switch(Switch::ClaudePeek, tr("Bildirimler", "Notifications"), notify, Switch::ClaudePeek.get(s)),
                    ],
                    tr(
                        "Bağlantı, Claude Code ayarlarına island'ın kancalarını ve durum satırını ekler; kapatınca geri alır.",
                        "The connection adds the island's hooks and status line to Claude Code's settings; turning it off takes them out.",
                    ),
                )
            }
            Section::Shelf => self.group(
                "",
                &[Row::Switch(
                    Switch::Screenshots,
                    tr("Ekran görüntüleri", "Screenshots"),
                    tr("Yeni ekran görüntüleri Raf'a eklenir, kısa bir önizleme açılır", "New screenshots go on the shelf, with a short preview"),
                    Switch::Screenshots.get(s),
                )],
                tr(
                    "Dosyaları island'a sürükleyin ya da Dosya Gezgini'nde sağ tıklayıp Gönder › Dynamic Island'ı seçin. Raf dosyaların kopyasını değil yerini tutar.",
                    "Drag files onto the island, or right-click them in File Explorer and pick Send to › Dynamic Island. The shelf keeps where files are, not copies.",
                ),
            ),
            Section::About => {
                let (c, t) = (self.c, self.t);
                app_mark(c, (LEFT + WIDTH / 2.0 - 36.0, self.y + 8.0), 72.0)?;
                c.text("Dynamic Island", (LEFT, self.y + 90.0, WIDTH, 24.0), 17.0, true, t.text, Align::Center)?;
                c.text(&format!("{} · nightdijital", version()), (LEFT, self.y + 114.0, WIDTH, 18.0), 12.0, false, t.dim, Align::Center)?;
                self.y += 152.0;
                self.group(
                    "",
                    &[
                        Row::Link(Link::Website, '\u{E774}', tr("Web sitesi", "Website"), "nightdijital.com"), // Globe
                        Row::Link(Link::SettingsFile, '\u{E8A5}', tr("Ayarlar dosyası", "Settings file"), tr("Elle düzenlemek için", "To edit by hand")), // Document
                        Row::Link(Link::Log, '\u{E9D9}', tr("Günlük", "Log"), tr("Bir sorunu bildirirken işe yarar", "Useful when reporting a problem")), // Diagnostic
                    ],
                    "",
                )
            }
        }
    }

    /// Dim text over a group, as many lines as it takes.
    fn note(&mut self, text: &str) -> Result<()> {
        let h = lines(text, 12.0, WIDTH - 8.0) * 17.0;
        self.c.paragraph(text, (LEFT + 4.0, self.y, WIDTH - 8.0, h + 4.0), 12.0, self.t.dim)?;
        self.y += h + 16.0;
        Ok(())
    }

    /// A card of rows, with a header above it and a footnote under it when they are not empty.
    fn group(&mut self, header: &str, rows: &[Row], footer: &str) -> Result<()> {
        let (c, t) = (self.c, self.t);
        if !header.is_empty() {
            c.text(header, (LEFT + 4.0, self.y, WIDTH - 8.0, 18.0), 12.0, true, t.dim, Align::Left)?;
            self.y += 22.0;
        }
        let height: f32 = rows.iter().map(Row::height).sum();
        c.round_rect((LEFT, self.y, WIDTH, height), 10.0, t.card)?;
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                c.round_rect((LEFT + 16.0, self.y, WIDTH - 16.0, 0.75), 0.0, t.line)?;
            }
            self.row(row, (LEFT, self.y, WIDTH, row.height()))?;
            self.y += row.height();
        }
        self.y += 8.0;
        if !footer.is_empty() {
            let h = lines(footer, 11.5, WIDTH - 8.0) * 16.0;
            c.paragraph(footer, (LEFT + 4.0, self.y, WIDTH - 8.0, h + 4.0), 11.5, t.dim)?;
            self.y += h + 4.0;
        }
        self.y += 18.0;
        Ok(())
    }

    fn row(&mut self, row: &Row, (x, y, w, h): Rect) -> Result<()> {
        let (c, t) = (self.c, self.t);
        match *row {
            Row::Switch(switch, label, detail, on) => {
                let color = if on.is_some() { t.text } else { t.dim };
                if detail.is_empty() {
                    c.text(label, (x + 16.0, y, w - 100.0, h), 13.0, false, color, Align::Left)?;
                } else {
                    c.text(label, (x + 16.0, y + 10.0, w - 100.0, 20.0), 13.0, false, color, Align::Left)?;
                    c.text(detail, (x + 16.0, y + 30.0, w - 100.0, 17.0), 11.5, false, t.dim, Align::Left)?;
                }
                let mut at = if on == Some(true) { 1.0 } else { 0.0 };
                if let Some((_, progress)) = self.flip.filter(|(s, _)| *s == switch) {
                    let eased = progress * progress * (3.0 - 2.0 * progress);
                    at = if on == Some(true) { eased } else { 1.0 - eased };
                }
                self.switch((x + w - 16.0 - 42.0, y + (h - 25.0) / 2.0, 42.0, 25.0), at, on.is_some())?;
                if on.is_some() {
                    self.hit((x, y, w, h), Action::Toggle(switch));
                }
            }
            Row::Segments(label, ref segments, enabled) => {
                let color = if enabled { t.text } else { t.dim };
                c.text(label, (x + 16.0, y, 160.0, h), 13.0, false, color, Align::Left)?;
                const TRACK: f32 = 256.0;
                let each = TRACK / segments.len() as f32;
                let track = (x + w - 16.0 - TRACK - 4.0, y + (h - 30.0) / 2.0, TRACK + 4.0, 30.0);
                c.round_rect(track, 8.0, t.segments)?;
                for (i, (name, action, chosen)) in segments.iter().enumerate() {
                    let segment = (track.0 + 2.0 + i as f32 * each, track.1 + 2.0, each, track.3 - 4.0);
                    if *chosen {
                        c.round_rect((segment.0, segment.1 + 0.5, segment.2, segment.3), 6.5, 0x1F000000)?; // shadow
                        c.round_rect(segment, 6.5, t.segment)?;
                    }
                    c.text(name, segment, 11.5, *chosen, color, Align::Center)?;
                    if enabled {
                        self.hit(segment, action.clone());
                    }
                }
            }
            Row::App(list, index, name) => {
                c.icon(APP, (x + 14.0, y, 20.0, h), 13.0, t.dim)?;
                c.text(name, (x + 44.0, y, w - 100.0, h), 13.0, false, t.text, Align::Left)?;
                let action = Action::Remove(list, index);
                let button = (x + w - 16.0 - 26.0, y + (h - 26.0) / 2.0, 26.0, 26.0);
                let hovered = self.hovered(&action);
                c.circle((button.0 + 13.0, button.1 + 13.0), 9.5, if hovered { 0xFFFF3B30 } else { t.off })?;
                c.icon('\u{E738}', button, 9.0, if hovered { 0xFFFFFFFF } else { t.dim })?; // Remove
                self.hit(button, action);
            }
            Row::Add(list) => {
                let action = Action::Pick(list);
                if self.hovered(&action) {
                    c.round_rect((x + 4.0, y + 4.0, w - 8.0, h - 8.0), 7.0, t.hover)?;
                }
                c.icon('\u{E710}', (x + 14.0, y, 20.0, h), 12.0, t.accent)?; // Add
                c.text(tr("Uygulama ekle…", "Add app…"), (x + 44.0, y, w - 60.0, h), 13.0, false, t.accent, Align::Left)?;
                self.hit((x, y, w, h), action);
            }
            Row::Song(index, song) => {
                c.icon('\u{EB52}', (x + 14.0, y, 20.0, h), 13.0, LIKED)?; // HeartFill
                c.text(&song.title, (x + 44.0, y + 9.0, w - 130.0, 20.0), 13.0, true, t.text, Align::Left)?;
                let about: Vec<String> = [song.artist.clone(), song.app.clone(), day(&song.added)].into_iter().filter(|s| !s.is_empty()).collect();
                c.text(&about.join(" · "), (x + 44.0, y + 29.0, w - 130.0, 17.0), 11.5, false, t.dim, Align::Left)?;
                for (k, (glyph, action)) in [('\u{E721}', Action::Search(index)), ('\u{E74D}', Action::Unlike(index))].into_iter().enumerate() {
                    let button = (x + w - 12.0 - (2 - k) as f32 * 32.0, y + (h - 30.0) / 2.0, 30.0, 30.0); // Search, Delete
                    let hovered = self.hovered(&action);
                    if hovered {
                        c.round_rect(button, 7.0, t.hover)?;
                    }
                    c.icon(glyph, button, 12.5, if hovered { t.text } else { t.dim })?;
                    self.hit(button, action);
                }
            }
            Row::Link(link, glyph, label, detail) => {
                let action = Action::Open(link);
                if self.hovered(&action) {
                    c.round_rect((x + 4.0, y + 4.0, w - 8.0, h - 8.0), 7.0, t.hover)?;
                }
                c.icon(glyph, (x + 14.0, y, 20.0, h), 13.0, t.accent)?;
                c.text(label, (x + 44.0, y, 180.0, h), 13.0, false, t.text, Align::Left)?;
                c.text(detail, (x + w - 40.0 - 240.0, y, 240.0, h), 12.0, false, t.dim, Align::Right)?;
                c.icon('\u{E76C}', (x + w - 32.0, y, 16.0, h), 10.0, t.dim)?; // ChevronRight
                self.hit((x, y, w, h), action);
            }
            Row::Empty(glyph, title, hint) => {
                c.icon(glyph, (x, y + 24.0, w, 34.0), 26.0, t.dim)?;
                c.text(title, (x, y + 66.0, w, 22.0), 14.0, true, t.text, Align::Center)?;
                c.text(hint, (x, y + 90.0, w, 18.0), 12.0, false, t.dim, Align::Center)?;
            }
        }
        Ok(())
    }

    /// An on/off switch: `at` 0 off, 1 on, between while it slides.
    fn switch(&self, (x, y, w, h): Rect, at: f32, enabled: bool) -> Result<()> {
        let mut track = mix(self.t.off, self.t.on, at);
        if !enabled {
            track = track & 0x00FF_FFFF | 0x80 << 24;
        }
        self.c.round_rect((x, y, w, h), h / 2.0, track)?;
        let (cx, cy, r) = (x + h / 2.0 + at * (w - h), y + h / 2.0, h / 2.0 - 2.0);
        self.c.circle((cx, cy + 0.75), r + 0.5, 0x29000000)?; // shadow
        self.c.circle((cx, cy), r, if enabled { 0xFFFFFFFF } else { 0xB0FFFFFF })
    }

    fn picker(&mut self, picker: &Picker, m: &Model) -> Result<()> {
        let (c, t) = (self.c, self.t);
        let (x, y, w, h) = SHEET;
        c.round_rect((0.0, 0.0, SIZE.0, SIZE.1), 0.0, t.shade)?;
        c.round_rect(SHEET, 14.0, t.card)?;
        c.outline(SHEET, 14.0, 1.0, t.line, false)?;
        c.text(tr("Uygulama seçin", "Choose an app"), (x + 20.0, y + 16.0, w - 40.0, 24.0), 15.0, true, t.text, Align::Left)?;
        let hint = match picker.list {
            List::Hide => tr("Seçtiğiniz uygulama öndeyken island gizlenir", "The island hides while the app you choose is in front"),
            List::Show => tr("Seçtiğiniz uygulama tam ekrandayken de island görünür", "The island stays visible while the app you choose is fullscreen"),
        };
        c.text(hint, (x + 20.0, y + 40.0, w - 40.0, 18.0), 11.5, false, t.dim, Align::Left)?;
        let apps = picker.shown(m.settings);
        if apps.is_empty() {
            c.text(tr("Eklenebilecek açık uygulama yok", "No open apps to add"), (x, y + SHEET_ROWS_Y + 40.0, w, 20.0), 12.5, false, t.dim, Align::Center)?;
        }
        for (k, (exe, title)) in apps.iter().skip(picker.first).take(SHEET_FITS).enumerate() {
            let row = (x + 10.0, y + SHEET_ROWS_Y + k as f32 * SHEET_ROW, w - 20.0, SHEET_ROW);
            let action = Action::Add(picker.list, exe.clone());
            if self.hovered(&action) {
                c.round_rect(row, 8.0, t.hover)?;
            }
            c.icon(APP, (row.0 + 8.0, row.1, 22.0, row.3), 14.0, t.dim)?;
            c.text(exe, (row.0 + 40.0, row.1 + 6.0, row.2 - 50.0, 18.0), 13.0, true, t.text, Align::Left)?;
            c.text(title, (row.0 + 40.0, row.1 + 24.0, row.2 - 50.0, 16.0), 11.5, false, t.dim, Align::Left)?;
            self.hit(row, action);
        }
        if apps.len() > picker.first + SHEET_FITS {
            c.text(tr("Daha fazlası için kaydırın", "Scroll for more"), (x + 20.0, y + h - 48.0, 200.0, 32.0), 11.5, false, t.dim, Align::Left)?;
        }
        let cancel = (x + w - 20.0 - 96.0, y + h - 48.0, 96.0, 32.0);
        c.round_rect(cancel, 8.0, if self.hovered(&Action::Close) { t.line } else { t.segments })?;
        c.text(tr("Vazgeç", "Cancel"), cancel, 13.0, false, t.text, Align::Center)?;
        self.hit(cancel, Action::Close);
        Ok(())
    }
}

/// The app's icon: a black pill on a violet square.
fn app_mark(c: &Canvas, (x, y): (f32, f32), s: f32) -> Result<()> {
    c.gradient_rect((x, y, s, s), s * 0.23, 0xFF4F46E5, 0xFF9B36EA)?;
    c.round_rect((x + 0.15 * s, y + 0.36 * s, 0.70 * s, 0.28 * s), 0.14 * s, 0xFF000000)?;
    c.round_rect((x + 0.27 * s, y + 0.4675 * s, 0.27 * s, 0.065 * s), 0.0325 * s, 0xFFF2F2F7)?;
    c.circle((x + 0.72 * s, y + 0.5 * s), 0.055 * s, 0xFF30D158)
}

/// About how many lines `text` takes at `size` in `width`: Segoe UI averages half an em a
/// character.
fn lines(text: &str, size: f32, width: f32) -> f32 {
    (text.chars().count() as f32 * size * 0.5 / width).ceil().max(1.0)
}

fn version() -> String {
    tr(format!("Sürüm {}", env!("CARGO_PKG_VERSION")), format!("Version {}", env!("CARGO_PKG_VERSION")))
}

/// The window's title.
fn title() -> HSTRING {
    tr("Dynamic Island Ayarları", "Dynamic Island Settings").into()
}

/// "2026-10-14 21:30" → "14.10.2026", or "2026-10-14" in English.
fn day(added: &str) -> String {
    let date = added.split(' ').next().unwrap_or_default();
    match date.split('-').collect::<Vec<_>>()[..] {
        [y, m, d] => tr(format!("{d}.{m}.{y}"), date.to_string()),
        _ => added.to_string(),
    }
}

fn mix(a: u32, b: u32, t: f32) -> u32 {
    let channel = |shift: u32| {
        let (from, to) = (((a >> shift) & 0xFF) as f32, ((b >> shift) & 0xFF) as f32);
        ((from + (to - from) * t).round() as u32) << shift
    };
    channel(24) | channel(16) | channel(8) | channel(0)
}

struct Graphics {
    painter: Painter,
    surface: CompositionDrawingSurface,
    _visual: SpriteVisual,
    _target: DesktopWindowTarget,
    _compositor: Compositor,
}

pub struct Preferences {
    hwnd: HWND,
    graphics: Option<Graphics>, // None only while being rebuilt
    scale: f32,
    state: State,
    hits: Vec<(Rect, Action)>,
    max_scroll: f32,
    flip: Option<(Switch, Instant)>,
    tracking: bool,
    pub autostart: bool,
    pub claude: Option<bool>,
}

impl Preferences {
    /// Opens the window centered on the monitor with the cursor. `offscreen` puts it beyond
    /// the right edge of the desktop, without taking the focus: for background tests.
    pub fn open(proc: WNDPROC, offscreen: bool) -> Result<Self> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let icon = LoadIconW(Some(instance.into()), PCWSTR(std::ptr::without_provenance(1))).unwrap_or_default(); // build.rs's icon
            let class = WNDCLASSW {
                lpfnWndProc: proc,
                hInstance: instance.into(),
                lpszClassName: CLASS,
                hCursor: LoadCursorW(None, IDC_ARROW)?,
                hIcon: icon,
                ..Default::default()
            };
            RegisterClassW(&class); // fails harmlessly when an earlier opening registered it
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(monitor, &mut info);
            let (mut dpi, mut _dpi_y) = (96, 96);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut _dpi_y);
            let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
            let px = |v: f32| (v * dpi as f32 / 96.0).round() as i32;
            let mut frame = RECT { left: 0, top: 0, right: px(SIZE.0), bottom: px(SIZE.1) };
            AdjustWindowRectExForDpi(&mut frame, style, false, WS_EX_NOREDIRECTIONBITMAP, dpi)?;
            let (w, h) = (frame.right - frame.left, frame.bottom - frame.top);
            let work = info.rcWork;
            let (x, y) = if offscreen {
                (GetSystemMetrics(SM_XVIRTUALSCREEN) + GetSystemMetrics(SM_CXVIRTUALSCREEN) + 100, work.top)
            } else {
                (work.left + (work.right - work.left - w) / 2, work.top + (work.bottom - work.top - h) / 2)
            };
            let hwnd = CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP,
                CLASS,
                &title(),
                style,
                x,
                y,
                w,
                h,
                None,
                None,
                Some(instance.into()),
                None,
            )?;
            let mut preferences = Self {
                hwnd,
                graphics: None,
                scale: GetDpiForWindow(hwnd) as f32 / 96.0,
                state: State::default(),
                hits: Vec::new(),
                max_scroll: 0.0,
                flip: None,
                tracking: false,
                autostart: false,
                claude: None,
            };
            preferences.theme();
            preferences.graphics = Some(preferences.build()?);
            let _ = ShowWindow(hwnd, if offscreen { SW_SHOWNOACTIVATE } else { SW_SHOW });
            if !offscreen {
                let _ = SetForegroundWindow(hwnd);
            }
            Ok(preferences)
        }
    }

    /// The title in the island's language, after it changed.
    pub fn retitle(&self) {
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &title());
        }
    }

    /// Brings the open window back to the front.
    pub fn show(&self) {
        unsafe {
            if IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    pub fn show_section(&mut self, section: Section) {
        self.state.section = section;
        self.state.scroll = 0.0;
    }

    fn build(&self) -> Result<Graphics> {
        let compositor = Compositor::new()?;
        let target = unsafe { compositor.cast::<ICompositorDesktopInterop>()?.CreateDesktopWindowTarget(self.hwnd, false)? };
        let painter = Painter::new(&compositor, self.scale)?;
        painter.on_device_replaced(self.hwnd, WM_DEVICE_LOST)?;
        let surface = painter.surface(SIZE)?;
        let visual = compositor.CreateSpriteVisual()?;
        visual.SetBrush(&compositor.CreateSurfaceBrushWithSurface(&surface)?)?;
        visual.SetSize(painter.px(SIZE))?;
        target.SetRoot(&visual)?;
        Ok(Graphics { painter, surface, _visual: visual, _target: target, _compositor: compositor })
    }

    /// A new composition tree: the scale changed, or the drawn surface lost its content.
    fn rebuild(&mut self) {
        self.graphics = None; // a window takes one DesktopWindowTarget at a time
        match self.build() {
            Ok(graphics) => self.graphics = Some(graphics),
            Err(e) => crate::log(format!("settings window: {e}")),
        }
    }

    /// Follows Windows' light or dark mode, title bar included.
    fn theme(&mut self) {
        self.state.dark = !light_theme();
        let t = if self.state.dark { &DARK } else { &LIGHT };
        let [_, r, g, b] = t.content.to_be_bytes();
        let caption = u32::from_le_bytes([r, g, b, 0]); // COLORREF
        let dark = BOOL::from(self.state.dark);
        unsafe {
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const BOOL as *const _, 4);
            let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_CAPTION_COLOR, &caption as *const u32 as *const _, 4);
        }
    }

    pub fn draw(&mut self, settings: &Settings, liked: &[Song]) {
        let Some(graphics) = &self.graphics else { return };
        let model = Model { settings, liked, autostart: self.autostart, claude: self.claude };
        let flip = self.flip.map(|(switch, start)| (switch, (start.elapsed().as_secs_f32() * 1000.0 / FLIP_MS).min(1.0)));
        let mut painted = None;
        let drawn = graphics.painter.draw(&graphics.surface, |c| {
            painted = Some(paint(c, &self.state, flip, &model)?);
            Ok(())
        });
        if let Err(e) = drawn {
            crate::log(format!("settings window draw failed: {e}"));
        }
        let Some((hits, height)) = painted else { return };
        self.hits = hits;
        self.max_scroll = (height - (SIZE.1 - BODY) + 12.0).max(0.0);
        if self.state.scroll > self.max_scroll {
            // The page got shorter (a song or an app taken off).
            self.state.scroll = self.max_scroll;
            self.draw(settings, liked);
        }
    }

    /// The control under a point in DIPs.
    fn at(&self, (x, y): (f32, f32)) -> Option<Action> {
        let inside = |&(rx, ry, rw, rh): &Rect| (rx..rx + rw).contains(&x) && (ry..ry + rh).contains(&y);
        self.hits.iter().find(|(r, _)| inside(r)).map(|(_, action)| action.clone())
    }

    pub fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Event {
        let point = (
            (lparam.0 & 0xFFFF) as i16 as f32 / self.scale,
            ((lparam.0 >> 16) & 0xFFFF) as i16 as f32 / self.scale,
        );
        match msg {
            WM_MOUSEMOVE => {
                if !self.tracking {
                    let mut leave = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: self.hwnd,
                        dwHoverTime: 0,
                    };
                    self.tracking = unsafe { TrackMouseEvent(&mut leave) }.is_ok();
                }
                let hover = self.at(point);
                if hover == self.state.hover {
                    return Event::Nothing;
                }
                self.state.hover = hover;
                Event::Redraw
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                if self.state.hover.take().is_some() {
                    Event::Redraw
                } else {
                    Event::Nothing
                }
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => self.click(point),
            WM_MOUSEWHEEL => {
                let steps = -((wparam.0 >> 16) as u16 as i16 as f32) / 120.0;
                match &mut self.state.picker {
                    Some(p) => p.first = (p.first as f32 + steps.signum()).clamp(0.0, p.apps.len().saturating_sub(SHEET_FITS) as f32) as usize,
                    None => self.state.scroll = (self.state.scroll + steps * 52.0).clamp(0.0, self.max_scroll),
                }
                self.state.hover = None; // what is under the cursor moved; the next move finds it
                Event::Redraw
            }
            WM_KEYDOWN => match VIRTUAL_KEY(wparam.0 as u16) {
                VK_ESCAPE if self.state.picker.take().is_some() => Event::Redraw,
                VK_ESCAPE => {
                    // Posted: destroying the window from here would leave the island holding it.
                    unsafe {
                        let _ = PostMessageW(Some(self.hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                    Event::Nothing
                }
                key @ (VK_UP | VK_DOWN) if self.state.picker.is_none() => {
                    let i = SECTIONS.iter().position(|s| *s == self.state.section).unwrap_or(0);
                    let i = if key == VK_UP { i.saturating_sub(1) } else { (i + 1).min(SECTIONS.len() - 1) };
                    self.show_section(SECTIONS[i]);
                    Event::Redraw
                }
                _ => Event::Unhandled,
            },
            WM_TIMER if wparam.0 == FLIP_TIMER => {
                if self.flip.is_none_or(|(_, start)| start.elapsed().as_secs_f32() * 1000.0 >= FLIP_MS) {
                    self.flip = None;
                    unsafe {
                        let _ = KillTimer(Some(self.hwnd), FLIP_TIMER);
                    }
                }
                Event::Redraw
            }
            WM_DPICHANGED => {
                self.scale = (wparam.0 & 0xFFFF) as f32 / 96.0;
                if let Some(r) = unsafe { (lparam.0 as *const RECT).as_ref() } {
                    unsafe {
                        let _ = SetWindowPos(self.hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                    }
                }
                self.rebuild();
                Event::Redraw
            }
            WM_DEVICE_LOST => {
                self.rebuild();
                Event::Redraw
            }
            WM_SETTINGCHANGE => {
                self.theme(); // light or dark may have changed
                Event::Redraw
            }
            WM_DESTROY => Event::Closed,
            _ => Event::Unhandled,
        }
    }

    fn click(&mut self, (x, y): (f32, f32)) -> Event {
        let (sx, sy, sw, sh) = SHEET;
        if self.state.picker.is_some() && !((sx..sx + sw).contains(&x) && (sy..sy + sh).contains(&y)) {
            self.state.picker = None; // a click beside the sheet puts it away
            return Event::Redraw;
        }
        let Some(action) = self.at((x, y)) else { return Event::Nothing };
        match action {
            Action::Section(section) => {
                self.show_section(section);
                Event::Redraw
            }
            Action::Pick(list) => {
                self.state.picker = Some(Picker { list, apps: running_apps(), first: 0 });
                self.state.hover = None;
                Event::Redraw
            }
            Action::Close => {
                self.state.picker = None;
                Event::Redraw
            }
            Action::Toggle(switch) => {
                self.flip = Some((switch, Instant::now()));
                unsafe { SetTimer(Some(self.hwnd), FLIP_TIMER, 15, None) };
                Event::Act(action)
            }
            Action::Add(..) => {
                self.state.picker = None;
                self.state.hover = None;
                Event::Act(action)
            }
            _ => Event::Act(action),
        }
    }
}

/// Apps with a window on the desktop now: exe name and a window title, one each, by name.
fn running_apps() -> Vec<(String, String)> {
    unsafe extern "system" fn collect(hwnd: HWND, list: LPARAM) -> BOOL {
        unsafe { (*(list.0 as *mut Vec<HWND>)).push(hwnd) };
        TRUE
    }
    let mut windows: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut windows as *mut _ as isize));
    }
    let own = std::process::id();
    let mut apps: Vec<(String, String)> = Vec::new();
    for hwnd in windows {
        unsafe {
            let tool = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0;
            let owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.is_invalid());
            let mut cloaked = 0u32; // hidden by the shell: other desktops, suspended store apps
            let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut _, 4);
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if !IsWindowVisible(hwnd).as_bool() || tool || owned || cloaked != 0 || pid == own {
                continue;
            }
            let mut buffer = [0u16; 256];
            let len = GetWindowTextW(hwnd, &mut buffer) as usize;
            let exe = crate::fullscreen::exe_name(hwnd);
            // Store apps' windows belong to ApplicationFrameHost, which would stand for all of them.
            if len == 0 || exe.is_empty() || exe == "applicationframehost.exe" || apps.iter().any(|(e, _)| *e == exe) {
                continue;
            }
            apps.push((exe, String::from_utf16_lossy(&buffer[..len])));
        }
    }
    apps.sort();
    apps
}

fn light_theme() -> bool {
    let (mut value, mut size) = (1u32, 4u32);
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    !read.is_ok() || value != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(title: &str, artist: &str) -> Song {
        Song { title: title.into(), artist: artist.into(), album: String::new(), app: "Brave".into(), added: "2026-10-14 21:30".into() }
    }

    /// Every page in both themes, as PNGs in the temp folder to look at; and the controls
    /// each page should offer are where a click finds them.
    #[test]
    fn pages_draw_with_their_controls() {
        let settings = Settings { hide_exe: vec!["obs64.exe".into()], ..Settings::default() };
        let liked = [song("Gece Yarısı", "Sezen Aksu"), song("Blue (Da Ba Dee)", "Eiffel 65")];
        let model = Model { settings: &settings, liked: &liked, autostart: true, claude: Some(true) };
        let dir = std::env::temp_dir().join("island-settings");
        std::fs::create_dir_all(&dir).unwrap();
        for (dark, language) in [(true, Language::Turkish), (false, Language::Turkish), (true, Language::English), (false, Language::English)] {
            crate::text::set(language);
            for section in SECTIONS {
                let state = State { section, dark, ..State::default() };
                let mut painted = None;
                let name = format!("{section:?}-{}-{}.png", if dark { "dark" } else { "light" }, language.code());
                crate::render::to_png(SIZE, 1.5, &dir.join(name), |c| {
                    painted = Some(paint(c, &state, None, &model)?);
                    Ok(())
                })
                .unwrap();
                let (hits, height) = painted.unwrap();
                let has = |action: Action| hits.iter().any(|(_, a)| *a == action);
                assert!(SECTIONS.into_iter().all(|s| has(Action::Section(s))), "{section:?}: sidebar");
                let expected = match section {
                    Section::General => vec![
                        Action::Toggle(Switch::Autostart),
                        Action::Language(Language::English),
                        Action::Delay(300),
                        Action::Toggle(Switch::PrivacyDot),
                    ],
                    Section::Fullscreen => vec![Action::Remove(List::Hide, 0), Action::Pick(List::Hide), Action::Pick(List::Show)],
                    Section::Music => vec![Action::Toggle(Switch::Equalizer), Action::Toggle(Switch::VolumePeek)],
                    Section::Liked => vec![Action::Search(1), Action::Unlike(1)],
                    Section::Claude => vec![Action::Toggle(Switch::Claude), Action::Toggle(Switch::ClaudePeek)],
                    Section::Shelf => vec![Action::Toggle(Switch::Screenshots)],
                    Section::Notifications => vec![Action::Toggle(Switch::Notifications), Action::Toggle(Switch::DoNotDisturb)],
                    Section::About => vec![Action::Open(Link::Website), Action::Open(Link::Log)],
                };
                for action in expected {
                    assert!(has(action.clone()), "{section:?}: no {action:?}");
                }
                assert!(height > 0.0 && height < SIZE.1 - BODY, "{section:?} needs scrolling at {height}");
            }
        }
        // The picker covers the page: only its own controls answer.
        let picker = Picker { list: List::Hide, apps: vec![("obs64.exe".into(), "OBS".into()), ("mpv.exe".into(), "film.mkv".into())], first: 0 };
        let state = State { section: Section::Fullscreen, dark: true, picker: Some(picker), ..State::default() };
        let mut painted = None;
        crate::render::to_png(SIZE, 1.5, &dir.join("Picker-dark.png"), |c| {
            painted = Some(paint(c, &state, None, &model)?);
            Ok(())
        })
        .unwrap();
        let actions: Vec<Action> = painted.unwrap().0.into_iter().map(|(_, a)| a).collect();
        assert_eq!(actions, vec![Action::Add(List::Hide, "mpv.exe".into()), Action::Close], "obs64.exe is on the list already");
    }

    #[test]
    fn switches_flip_their_own_setting() {
        let all = [
            Switch::Hover,
            Switch::PrivacyDot,
            Switch::Equalizer,
            Switch::Glow,
            Switch::SongPeek,
            Switch::VolumePeek,
            Switch::ClaudePeek,
            Switch::Screenshots,
            Switch::Notifications,
        ];
        for switch in all {
            let mut s = Settings::default();
            assert!(switch.flip(&mut s));
            assert_eq!(switch.get(&s), Some(false), "{switch:?}");
            let others = all.iter().filter(|o| **o != switch).all(|o| o.get(&s) == Some(true));
            assert!(others, "{switch:?} changed another setting");
        }
        assert!(!Switch::Autostart.flip(&mut Settings::default()));
        assert_eq!(day("2026-10-14 21:30"), "14.10.2026");
        assert_eq!(mix(0xFF000000, 0xFFFFFFFF, 0.5), 0xFF808080);
    }
}
