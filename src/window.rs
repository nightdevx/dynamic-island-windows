use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
};

use windows::{
    core::*,
    Foundation::TypedEventHandler,
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        Storage::FileSystem::*,
        System::{
            DataExchange::COPYDATASTRUCT,
            LibraryLoader::GetModuleHandleW,
            Ole::{IDropTarget, OleInitialize, RegisterDragDrop, RevokeDragDrop},
            Registry::*,
            Threading::INFINITE,
        },
        UI::{Accessibility::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
    },
};

use crate::{
    audio,
    claude::{self, Limits, Session, State},
    fullscreen,
    island::{self, Hit, Island, Line, Tab, View, TOP},
    log,
    media::{Command, Media, MediaLink},
    privacy,
    settings::{self, Settings},
    shelf::{self, DropTarget, Shelf},
    usage,
};

const LEAVE_MS: u32 = 400;
const PEEK_MS: u32 = 2500;
// Privacy dot colors, as on iPhone.
const MICROPHONE: u32 = 0xFFFF9F0A;
const CAMERA: u32 = 0xFF30D158;

// Defined in Win32_UI_Controls; not worth a whole feature for two numbers.
const WM_MOUSEHOVER: u32 = 0x02A1;
const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_ANIM_DONE: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;
const WM_SHELL_EVENT: u32 = WM_APP + 3;
const WM_APPBAR: u32 = WM_APP + 4;
const WM_MEDIA: u32 = WM_APP + 5;
const WM_DEVICE_LOST: u32 = WM_APP + 6;
const WM_VOLUME: u32 = WM_APP + 7;
const WM_AUDIO_DEVICE: u32 = WM_APP + 8;
const WM_DRAG_FILES: u32 = WM_APP + 10; // WPARAM: 1 files came over, 0 they left, 2 dropped
const SHELF_COPYDATA: usize = 0x5348_454C; // "SHEL": WM_COPYDATA from `island.exe --shelf`
const VOLUME_MS: u32 = 1500;
const NIN_KEYSELECT: u32 = NIN_SELECT | 1; // NINF_KEY
// All timers are one-shot: the window procedure kills each one when it fires.
const LEAVE_TIMER: usize = 1;
const RAISE_TIMER: usize = 2;
const RAISE_DELAY_MS: u32 = 100;
const PEEK_TIMER: usize = 3;
const MINUTE_TIMER: usize = 4; // only while the Claude tab is on screen: reset countdowns
const SECOND_TIMER: usize = 5; // only while the music page is on screen and playing: elapsed time
const SHOT_TIMER: usize = 6; // after the screenshots folder changes
const SHOT_DELAY_MS: u32 = 400;
// The shelf's two lists.
const FILES: usize = 0;
const SCREENSHOTS: usize = 1;
const TRAY_ID: u32 = 1;
const ID_AUTOSTART: usize = 1;
const ID_SETTINGS: usize = 2;
const ID_EXIT: usize = 3;
const ID_PLAY_PAUSE: usize = 10;
const ID_NEXT: usize = 11;
const ID_PREVIOUS: usize = 12;
const ID_NEXT_SESSION: usize = 13;

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("DynamicIsland");

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    // Read by the WinEvent callback, which can run while APP is borrowed (e.g. inside a menu loop).
    static WINDOW: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
}

struct App {
    hwnd: HWND,
    scale: f32,
    view: View,
    generation: usize, // drops completions of animations that were interrupted
    tracking: bool,
    hidden: bool, // a fullscreen app or game is in front
    island: Option<Island>, // None only while being rebuilt
    settings: Settings,
    taskbar_created: u32,
    explorer_hooks: Vec<HWINEVENTHOOK>,
    sessions: Vec<Session>,
    limits: Limits,
    states: HashMap<String, State>, // last state seen per session, to spot transitions
    seen: HashSet<String>,          // sessions waiting on the user that the user has already noticed
    compact_line: Option<CompactLine>, // what the closed island shows now; redrawn only on change
    media: Option<MediaLink>,
    now_playing: Option<Media>,
    peek_tab: Tab, // the tab a peek opens to
    privacy: privacy::InUse,
    volume: Option<audio::Volume>,
    last_volume: Option<(u32, bool)>, // shown or current level and mute
    shelves: [Shelf; 2], // FILES dropped on the island, SCREENSHOTS taken
    list: usize,         // which of them the shelf page shows
    dropping: bool,      // files are being dragged over the island
    shots_since: std::time::SystemTime, // the island's start: only screenshots after it show
    shots_seen: HashSet<std::path::PathBuf>, // screenshots already shown once, kept or not
}

/// Status, what it is about, 5-hour limit left and its color, color of the Claude mark.
type CompactLine = (String, String, Option<(f32, u32)>, u32);

/// `island.exe --shelf <files>`: puts the files on the running island's shelf. Explorer's
/// "Send to → Dynamic Island" runs this with the selected files.
pub fn send_to_shelf(paths: Vec<String>) {
    let text = paths.join("\n");
    let data = COPYDATASTRUCT { dwData: SHELF_COPYDATA, cbData: text.len() as u32, lpData: text.as_ptr() as *mut _ };
    unsafe {
        match FindWindowW(w!("NightdijitalIsland"), None) {
            Ok(island) => {
                SendMessageW(island, WM_COPYDATA, Some(WPARAM(0)), Some(LPARAM(&data as *const _ as isize)));
            }
            Err(_) => log("shelf: the island is not running"),
        }
    }
}

/// Creates the island window and runs the message loop until it is destroyed.
pub fn run() -> Result<()> {
    let hwnd = create_window()?;
    WINDOW.with(|w| w.set(hwnd));
    let mut app = App {
        hwnd,
        scale: 0.0,
        view: View::Compact,
        generation: 0,
        tracking: false,
        hidden: false,
        island: None,
        settings: settings::load(),
        taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
        explorer_hooks: Vec::new(),
        sessions: Vec::new(),
        limits: Limits::default(),
        states: HashMap::new(),
        seen: HashSet::new(),
        compact_line: None,
        media: None,
        now_playing: None,
        peek_tab: Tab::Claude,
        privacy: privacy::in_use(), // what was in use before we started is not news
        volume: None,
        last_volume: None,
        shelves: Default::default(),
        list: FILES,
        dropping: false,
        shots_since: std::time::SystemTime::now(),
        shots_seen: HashSet::new(),
    };
    // Files dragged onto the island land on the shelf.
    let dropped = unsafe {
        OleInitialize(None).and_then(|_| RegisterDragDrop(hwnd, &IDropTarget::from(DropTarget::new(hwnd, WM_DRAG_FILES))))
    };
    if let Err(e) = dropped {
        log(format!("drop target: {e}"));
    }
    app.volume = audio::Volume::new(hwnd, WM_VOLUME, WM_AUDIO_DEVICE).map_err(|e| log(format!("volume: {e}"))).ok();
    app.follow_audio();
    app.place(false)?;
    app.reload_claude(false); // what was already waiting before we started is not news
    usage::refresh();
    app.media = Some(MediaLink::start(hwnd, WM_MEDIA, (crate::pages::ART_SOURCE * app.scale).round() as u32));
    app.watch_explorer();
    appbar(hwnd, ABM_NEW);
    app.hidden = fullscreen::should_hide(hwnd, &app.settings);
    unsafe {
        // Foreground changes: a fullscreen app may have come or gone, and another topmost
        // window activated after us would cover the island.
        let hook = SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(on_win_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
        if hook.is_invalid() {
            log("foreground hook failed");
        }
        tray(hwnd, NIM_ADD);
    }
    if !app.hidden {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    message_loop()
}

/// What to do when something watched changes.
type OnChange = fn(&mut App);
/// The handle that gets signaled, what re-arms it, what to do.
type Watch = (HANDLE, Box<dyn Fn()>, OnChange);

/// Sleeps until there is input or something watched changes (settings and Claude Code state
/// folders, the microphone/camera records); nothing runs while idle.
fn message_loop() -> Result<()> {
    let folders: [(Option<std::path::PathBuf>, OnChange); 3] = [
        (settings::dir(), App::reload_settings),
        (claude::dir(), |app| app.reload_claude(true)),
        (shelf::screenshots_dir(), App::screenshots_changed),
    ];
    let mut watches: Vec<Watch> = folders
        .into_iter()
        .filter_map(|(dir, action)| {
            let dir = dir?;
            std::fs::create_dir_all(&dir).ok()?;
            let filter = FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME;
            let handle = unsafe { FindFirstChangeNotificationW(&HSTRING::from(dir.as_path()), false, filter) };
            let handle = handle.map_err(|e| log(format!("cannot watch {}: {e}", dir.display()))).ok()?;
            let rearm: Box<dyn Fn()> = Box::new(move || unsafe {
                let _ = FindNextChangeNotification(handle);
            });
            Some((handle, rearm, action))
        })
        .collect();
    if let Some(watch) = privacy::Watch::new() {
        watches.push((watch.event(), Box::new(move || watch.arm()), App::reload_privacy));
    }
    let handles: Vec<HANDLE> = watches.iter().map(|(h, _, _)| *h).collect();
    let mut msg = MSG::default();
    loop {
        let woke = unsafe { MsgWaitForMultipleObjectsEx(Some(&handles), INFINITE, QS_ALLINPUT, MWMO_INPUTAVAILABLE) };
        if let Some((_, rearm, action)) = watches.get(woke.0.wrapping_sub(WAIT_OBJECT_0.0) as usize) {
            rearm();
            APP.with(|cell| {
                if let Some(app) = cell.borrow_mut().as_mut() {
                    action(app);
                }
            });
        }
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if msg.message == WM_QUIT {
                return Ok(());
            }
            unsafe { DispatchMessageW(&msg) };
        }
    }
}

fn create_window() -> Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class = w!("NightdijitalIsland");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            style: CS_DBLCLKS, // double click opens a shelf item
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(Error::from_thread());
        }
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_NOREDIRECTIONBITMAP,
            class,
            w!("Island"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
}

impl App {
    /// Top center of the primary monitor at that monitor's DPI. The composition tree is
    /// rebuilt when the scale changes, since its text is rasterized for one scale.
    /// `rebuild` forces a new composition tree, for when the drawn surfaces lost their content.
    fn place(&mut self, rebuild: bool) -> Result<()> {
        let (r, scale) = unsafe {
            let monitor = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            GetMonitorInfoW(monitor, &mut info).ok()?;
            let (mut dpi, mut _dpi_y) = (0, 0);
            GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut _dpi_y)?;
            (info.rcMonitor, dpi as f32 / 96.0)
        };
        let (w, h) = island::window_size(scale);
        unsafe { SetWindowPos(self.hwnd, Some(HWND_TOPMOST), r.left + (r.right - r.left - w) / 2, r.top, w, h, SWP_NOACTIVATE)? };
        if rebuild || scale != self.scale || self.island.is_none() {
            // A window takes one DesktopWindowTarget: drop the old tree before building the new one.
            self.island = None;
            self.scale = scale;
            self.view = View::Compact;
            self.generation += 1;
            self.island = Some(Island::new(self.hwnd, scale, WM_DEVICE_LOST)?);
            if let Some(island) = self.island.as_mut() {
                let media = self.now_playing.as_ref();
                island.set_media(media, claude::now_ms())?;
                island.set_art(media.and_then(|m| m.art.as_deref()), false)?;
            }
            self.compact_line = None;
            self.show_claude_state(false);
            self.update_live();
            self.show_privacy();
            log(format!("placed at scale {scale} on {}x{}", r.right - r.left, r.bottom - r.top));
        }
        self.set_region(self.pill_size());
        Ok(())
    }

    fn reload_settings(&mut self) {
        let new = settings::load();
        if new != self.settings {
            log(format!("settings: {new:?}"));
            self.settings = new;
            self.refresh(); // the exe lists may change what counts as "hide"
            self.update_live();
        }
    }

    /// A Claude session waits on the user and they have not looked yet.
    fn claude_needs_user(&self) -> bool {
        self.sessions
            .iter()
            .any(|s| s.state == State::Permission || (s.state == State::Done && !self.seen.contains(&s.id)))
    }

    /// Starts or stops what moves on its own: the equalizer (music playing, island closed
    /// and on screen), the progress fill and the clock (music page on screen and playing).
    /// Each costs frames or wakeups, so none runs where it cannot be seen.
    fn update_live(&mut self) {
        let playing = self.now_playing.as_ref().is_some_and(|m| m.playing);
        let line = if playing && !self.claude_needs_user() { Line::Music } else { Line::Claude };
        let equalizer = line == Line::Music && self.view == View::Compact && !self.hidden && self.settings.equalizer && animations_enabled();
        let now = claude::now_ms();
        if let Some(island) = self.island.as_mut() {
            let updated = island.set_line(line, equalizer).and_then(|_| island.set_progress(self.now_playing.as_ref(), now));
            if let Err(e) = updated {
                log(format!("live parts failed: {e}"));
            }
        }
        self.arm_clock();
    }

    fn music_page_visible(&self) -> bool {
        self.view == View::Expanded && self.tab() == Some(Tab::Music) && !self.hidden
    }

    /// The elapsed time ticks once a second, only while someone can see it.
    fn arm_clock(&self) {
        let playing = self.now_playing.as_ref().is_some_and(|m| m.playing);
        unsafe {
            if self.music_page_visible() && playing {
                SetTimer(Some(self.hwnd), SECOND_TIMER, 1020 - (claude::now_ms() % 1000) as u32, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), SECOND_TIMER);
            }
        }
    }

    /// Reads the Claude Code state files. A session that starts waiting on the user (a
    /// permission prompt, a finished answer) lights the dot and peeks, unless its window is
    /// already in front or `notify` is off.
    fn reload_claude(&mut self, notify: bool) {
        let (sessions, limits) = claude::load();
        let front = foreground_title().to_lowercase();
        let mut news: Option<&Session> = None;
        for session in &sessions {
            if self.states.get(&session.id) == Some(&session.state) {
                continue;
            }
            usage::refresh(); // Claude is in use, so the limits are moving (at most every 3 min)
            self.seen.remove(&session.id);
            if matches!(session.state, State::Permission | State::Done) {
                let in_front = !session.project.is_empty() && front.contains(&session.project.to_lowercase());
                if in_front || !notify {
                    self.seen.insert(session.id.clone());
                } else if news.is_none_or(|n| n.state != State::Permission) {
                    news = Some(session);
                }
            }
        }
        self.states = sessions.iter().map(|s| (s.id.clone(), s.state)).collect();
        self.seen.retain(|id| self.states.contains_key(id));
        let peek = news.map(|s| {
            let name = if s.project.is_empty() { "Claude" } else { &s.project };
            (format!("{name} · {}", s.state.label()), s.state.color(), s.state == State::Permission)
        });
        self.sessions = sessions;
        self.limits = limits;

        if self.view == View::Expanded && self.tab() == Some(Tab::Claude) {
            self.viewing_claude();
        } else {
            self.show_claude_state(peek.as_ref().is_some_and(|p| p.2));
        }
        self.update_live();
        if let Some((text, color, _)) = peek {
            self.peek(Tab::Claude, PEEK_MS, |island| island.draw_peek(&text, Some(color)));
        }
    }

    /// Opens the island a little for news, then closes it again after `ms`. Not over
    /// fullscreen apps and not when the user has it open already; newer news replaces a peek.
    /// A click on the peek opens `tab`.
    fn peek(&mut self, tab: Tab, ms: u32, draw: impl FnOnce(&Island) -> Result<()>) {
        if self.hidden || self.view == View::Expanded {
            return;
        }
        let drawn = self.island.as_ref().map_or(Ok(()), draw);
        self.peek_tab = tab;
        match drawn.and_then(|_| self.set_view(View::Peek)) {
            Ok(()) => unsafe {
                SetTimer(Some(self.hwnd), PEEK_TIMER, ms, None);
            },
            Err(e) => log(format!("peek failed: {e}")),
        }
    }

    /// Listens to the default output device, after startup or a switch of device; its
    /// current level is not news.
    fn follow_audio(&mut self) {
        let Some(volume) = self.volume.as_mut() else { return };
        self.last_volume = volume.follow_default().map_err(|e| log(format!("volume device: {e}"))).ok();
    }

    /// The system volume changed: a short peek with the level.
    fn show_volume(&mut self, level: u32, muted: bool) {
        // Balance and per-channel changes notify too, with the same master level.
        if self.last_volume == Some((level, muted)) {
            return;
        }
        self.last_volume = Some((level, muted));
        self.peek(Tab::Music, VOLUME_MS, |island| island.draw_volume(level, muted));
    }

    /// Reads which apps use the microphone and camera; one that just started peeks.
    fn reload_privacy(&mut self) {
        let now = privacy::in_use();
        if now == self.privacy {
            return;
        }
        log(format!("privacy: {now:?}"));
        let started = |new: &Option<String>, old: &Option<String>| new.clone().filter(|_| old.is_none());
        let news = started(&now.camera, &self.privacy.camera)
            .map(|app| (format!("Kamera kullanımda · {app}"), CAMERA))
            .or_else(|| started(&now.microphone, &self.privacy.microphone).map(|app| (format!("Mikrofon kullanımda · {app}"), MICROPHONE)));
        self.privacy = now;
        self.show_privacy();
        if let Some((text, color)) = news {
            let tab = self.tab().unwrap_or(Tab::Music);
            self.peek(tab, PEEK_MS, |island| island.draw_peek(&text, Some(color)));
        }
    }

    /// The privacy dot: green for the camera (which usually comes with the microphone),
    /// orange for the microphone alone. The closed pill grows to make room for it.
    fn show_privacy(&mut self) {
        let color = if self.privacy.camera.is_some() {
            Some(CAMERA)
        } else {
            self.privacy.microphone.as_ref().map(|_| MICROPHONE)
        };
        let before = self.pill_size();
        let Some(island) = self.island.as_mut() else { return };
        if let Err(e) = island.set_privacy(color) {
            log(format!("privacy dot failed: {e}"));
        }
        if self.view == View::Compact {
            if let Err(e) = self.reshape(before) {
                log(format!("privacy resize failed: {e}"));
            }
        }
    }

    /// The closed island's line: what Claude is doing (or waiting for) and the 5-hour limit.
    /// `pulse` beats the mark for news that needs the user.
    fn show_claude_state(&mut self, pulse: bool) {
        let (status, detail, mark) = claude::summary(&self.sessions, |s| !self.seen.contains(&s.id));
        // ponytail: a limit that resets while nothing happens shows 100 only at the next event
        let line = (status, detail, claude::five_hour_used(&self.limits, claude::now_ms() / 1000), mark);
        let Some(island) = &self.island else { return };
        if self.compact_line.as_ref() != Some(&line) {
            if let Err(e) = island.draw_compact(&line.0, &line.1, line.2, line.3) {
                log(format!("compact line failed: {e}"));
            }
            self.compact_line = Some(line);
        }
        if pulse {
            if let Err(e) = island.pulse_mark() {
                log(format!("pulse failed: {e}"));
            }
        }
    }

    /// The Claude tab is on screen: draw it fresh, count what it shows as noticed, and keep
    /// the reset countdowns current to the minute.
    fn viewing_claude(&mut self) {
        usage::refresh();
        let attention = self.sessions.iter().filter(|s| matches!(s.state, State::Permission | State::Done));
        self.seen.extend(attention.map(|s| s.id.clone()));
        self.show_claude_state(false);
        if let Some(Err(e)) = self.island.as_ref().map(|i| i.draw_claude(&self.sessions, &self.limits, claude::now_ms() / 1000)) {
            log(format!("claude page failed: {e}"));
        }
        let to_next_minute = 60_000 - (claude::now_ms() % 60_000) as u32 + 50;
        unsafe { SetTimer(Some(self.hwnd), MINUTE_TIMER, to_next_minute, None) };
    }

    fn tab(&self) -> Option<Tab> {
        self.island.as_ref().map(Island::tab)
    }

    fn set_tab(&mut self, tab: Tab) -> Result<()> {
        if let Some(island) = self.island.as_mut() {
            island.set_tab(tab)?;
        }
        if tab == Tab::Claude && self.view == View::Expanded {
            self.reload_claude(true);
        } else {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), MINUTE_TIMER);
            }
            if tab == Tab::Shelf {
                self.draw_shelf();
            }
            self.update_live();
        }
        Ok(())
    }

    /// Draws the shelf page if it is on screen (thumbnails are made only for what shows).
    fn draw_shelf(&mut self) {
        if self.view != View::Expanded || self.tab() != Some(Tab::Shelf) {
            return;
        }
        let list = &mut self.shelves[self.list];
        list.prune();
        list.load_icons((crate::pages::SHELF_ICON * self.scale).round() as u32);
        let counts = [self.shelves[0].items.len(), self.shelves[1].items.len()];
        let drawn = self.island.as_mut().map(|island| island.draw_shelf(&self.shelves[self.list], self.list, counts, self.dropping));
        if let Some(Err(e)) = drawn {
            log(format!("shelf draw failed: {e}"));
        }
    }

    /// Files dragged over the island (`what` as in WM_DRAG_FILES): it opens on the shelf to
    /// take them, and closes again when they are taken elsewhere.
    fn files_dragged(&mut self, what: usize) {
        unsafe {
            let _ = KillTimer(Some(self.hwnd), LEAVE_TIMER);
        }
        self.dropping = what == 1;
        match what {
            1 => {
                self.list = FILES;
                let opened = self.island.as_mut().map_or(Ok(()), |island| island.set_tab(Tab::Shelf)).and_then(|_| self.set_view(View::Expanded));
                if let Err(e) = opened {
                    log(format!("open for a drop failed: {e}"));
                }
            }
            2 => {
                let files = shelf::take_dropped();
                log(format!("shelf: {} file(s) dropped", files.len()));
                self.shelves[FILES].add(files, false);
            }
            _ => unsafe {
                SetTimer(Some(self.hwnd), LEAVE_TIMER, LEAVE_MS, None);
            },
        }
        self.draw_shelf();
    }

    /// The screenshots folder changed; it is read once the burst of changes is over (a new
    /// file is written in steps).
    fn screenshots_changed(&mut self) {
        if self.settings.screenshots {
            unsafe { SetTimer(Some(self.hwnd), SHOT_TIMER, SHOT_DELAY_MS, None) };
        }
    }

    /// Screenshots taken since the island started go to the front of their list, with a peek
    /// when the shelf is not open. Each comes once: one taken off the list stays off.
    fn add_screenshots(&mut self) {
        let Some(dir) = shelf::screenshots_dir() else { return };
        let mut new = shelf::screenshots_since(&dir, self.shots_since);
        new.retain(|(path, _)| self.shots_seen.insert(path.clone()));
        let added = self.shelves[SCREENSHOTS].add_named(new, true);
        if added == 0 {
            return;
        }
        log(format!("shelf: {added} new screenshot(s)"));
        self.list = SCREENSHOTS;
        if self.view == View::Expanded && self.tab() == Some(Tab::Shelf) {
            self.draw_shelf();
        } else {
            let text = if added == 1 { "Ekran görüntüsü rafta".to_string() } else { format!("{added} ekran görüntüsü rafta") };
            self.peek(Tab::Shelf, PEEK_MS, |island| island.draw_peek(&text, Some(0xFF0A84FF)));
        }
    }

    /// The shelf item under a press, if the shelf page is open there: its index, what a drag
    /// from it takes, and the point in window pixels.
    fn shelf_item_at(&self, lparam: LPARAM) -> Option<(usize, Vec<std::path::PathBuf>, POINT)> {
        if self.view != View::Expanded || self.tab() != Some(Tab::Shelf) {
            return None;
        }
        let (x, y) = ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32);
        let Some(Hit::ShelfItem(i)) = self.island.as_ref()?.hit(x, y) else { return None };
        let dragged = self.shelves[self.list].dragged(i);
        (!dragged.is_empty()).then_some((i, dragged, POINT { x, y }))
    }

    /// Clicks outside this rectangle fall through to the window below.
    fn set_region(&self, (w, h): (f32, f32)) {
        let s = self.scale;
        let (window_w, _) = island::window_size(s);
        let pad = 6.0 * s;
        let rw = w * s + 2.0 * pad;
        let left = ((window_w as f32 - rw) / 2.0).floor() as i32;
        unsafe {
            let rgn = CreateRectRgn(left, 0, left + rw.ceil() as i32, ((TOP + h) * s + pad).ceil() as i32);
            // The system owns the region after this call.
            SetWindowRgn(self.hwnd, Some(rgn), true);
        }
    }

    /// The snap-layout divider (explorer's "GhostDivider") is a topmost window that appears
    /// when the cursor nears the line between two snapped windows, right where the island sits.
    /// Only explorer's events are hooked, so other apps never wake us.
    fn watch_explorer(&mut self) {
        for hook in self.explorer_hooks.drain(..) {
            unsafe {
                let _ = UnhookWinEvent(hook);
            }
        }
        let mut pid = 0;
        unsafe {
            let Ok(taskbar) = FindWindowW(w!("Shell_TrayWnd"), None) else { return };
            GetWindowThreadProcessId(taskbar, Some(&mut pid));
        }
        if pid == 0 {
            return;
        }
        for event in [EVENT_OBJECT_SHOW, EVENT_OBJECT_UNCLOAKED] {
            let hook = unsafe { SetWinEventHook(event, event, None, Some(on_win_event), pid, 0, WINEVENT_OUTOFCONTEXT) };
            if hook.is_invalid() {
                log(format!("explorer hook {event:#x} failed"));
            } else {
                self.explorer_hooks.push(hook);
            }
        }
    }

    /// Hides behind fullscreen apps and games, shows again after them, and stays above
    /// other topmost windows while visible.
    fn refresh(&mut self) {
        let hide = fullscreen::should_hide(self.hwnd, &self.settings);
        if hide != self.hidden {
            self.hidden = hide;
            unsafe {
                let _ = ShowWindow(self.hwnd, if hide { SW_HIDE } else { SW_SHOWNOACTIVATE });
            }
            if hide {
                self.tracking = false;
                unsafe {
                    for timer in [LEAVE_TIMER, PEEK_TIMER, MINUTE_TIMER] {
                        let _ = KillTimer(Some(self.hwnd), timer);
                    }
                }
                if let Err(e) = self.set_view(View::Compact) {
                    log(format!("collapse on hide failed: {e}"));
                }
            }
            self.update_live();
            log(if hide { "hidden" } else { "shown" });
        }
        if !self.hidden {
            raise(self.hwnd);
        }
    }

    /// The pill's size in the current view, DIPs.
    fn pill_size(&self) -> (f32, f32) {
        self.island.as_ref().map_or(self.view.size(), |island| island.size_of(self.view))
    }

    fn set_view(&mut self, view: View) -> Result<()> {
        if self.view == view || self.island.is_none() {
            return Ok(());
        }
        let before = self.pill_size();
        self.view = view;
        if view != View::Expanded {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), MINUTE_TIMER);
            }
        }
        self.reshape(before)?;
        if view == View::Expanded && self.tab() == Some(Tab::Claude) {
            self.reload_claude(true);
        } else {
            self.draw_shelf(); // when it is the page that opens
            self.update_live();
        }
        Ok(())
    }

    /// Springs the pill from `before` to the current view's size. The hit area grows before
    /// the pill does and shrinks only after it has finished shrinking.
    fn reshape(&mut self, before: (f32, f32)) -> Result<()> {
        let after = self.pill_size();
        let area = |(w, h): (f32, f32)| w * h;
        let grows = area(after) > area(before);
        self.generation += 1;
        if grows {
            self.set_region(after);
        }
        let Some(island) = self.island.as_mut() else { return Ok(()) };
        let batch = island.animate(self.view)?;
        if !grows {
            let (hwnd, generation) = (self.hwnd.0 as isize, self.generation);
            batch.Completed(&TypedEventHandler::new(move |_, _| {
                unsafe { PostMessageW(Some(HWND(hwnd as _)), WM_ANIM_DONE, WPARAM(generation), LPARAM(0))? };
                Ok(())
            }))?;
        }
        Ok(())
    }

    fn media_command(&self, command: Command) {
        if let Some(media) = &self.media {
            media.send(command);
        }
    }

    /// The media thread has a new snapshot: redraw what shows it, and peek at a new song.
    fn media_changed(&mut self) {
        let media = self.media.as_ref().and_then(MediaLink::current);
        let old = self.now_playing.take();
        let new_track = match (&old, &media) {
            (Some(a), Some(b)) => !a.same_track(b),
            (None, None) => false,
            _ => true,
        };
        let art_changed = match (old.as_ref().and_then(|m| m.art.as_ref()), media.as_ref().and_then(|m| m.art.as_ref())) {
            (Some(a), Some(b)) => !std::sync::Arc::ptr_eq(a, b),
            (None, None) => false,
            _ => true,
        };
        let playing_changed = old.as_ref().map(|m| m.playing) != media.as_ref().map(|m| m.playing);
        if new_track || playing_changed || art_changed {
            log(match &media {
                Some(m) => format!(
                    "media: {} · {} — {} [{}] {:.0}/{:.0} s, art {}, {} session(s)",
                    m.app,
                    m.title,
                    m.artist,
                    if m.playing { "playing" } else { "paused" },
                    m.position,
                    m.duration,
                    m.art.as_ref().map_or("none".into(), |a| format!("{} px", a.size)),
                    m.sessions
                ),
                None => "media: none".into(),
            });
        }
        // Players report the position often while playing; that alone redraws nothing but the
        // clock and the progress, and only while the music page is open.
        let shown = |m: &Media| (m.can_previous, m.can_toggle, m.can_next, m.duration > 0.0, m.shuffle, m.repeat);
        let redraw = match (&old, &media) {
            (Some(a), Some(b)) => new_track || playing_changed || shown(a) != shown(b),
            (None, None) => false,
            _ => true,
        };
        let page_visible = self.music_page_visible();
        if let Some(island) = self.island.as_mut() {
            let now = claude::now_ms();
            let mut drawn = if redraw {
                island.set_media(media.as_ref(), now)
            } else if page_visible {
                island.draw_times(media.as_ref(), now)
            } else {
                Ok(())
            };
            if art_changed {
                let art = media.as_ref().and_then(|m| m.art.as_deref());
                drawn = drawn.and_then(|_| island.set_art(art, new_track && old.is_some()));
            }
            if let Err(e) = drawn {
                log(format!("media draw failed: {e}"));
            }
        }
        self.now_playing = media;
        self.update_live();

        let song = self.now_playing.as_ref().filter(|m| new_track && m.playing && !m.title.is_empty());
        if let Some(text) = song.map(|m| if m.artist.is_empty() { m.title.clone() } else { format!("{} — {}", m.title, m.artist) }) {
            if !self.claude_needs_user() {
                self.peek(Tab::Music, PEEK_MS, |island| island.draw_peek(&text, None));
            }
        }
    }

    /// Hover or click on a peek opens the tab the peek was about.
    fn expand(&mut self) -> Result<()> {
        if self.view == View::Peek {
            let tab = self.peek_tab;
            if let Some(island) = self.island.as_mut() {
                island.set_tab(tab)?;
            }
        }
        self.set_view(View::Expanded)
    }

    fn click(&mut self, hit: Hit) -> Result<()> {
        if let Some(island) = &self.island {
            island.press(hit)?;
        }
        match hit {
            Hit::Tab(tab) => self.set_tab(tab)?,
            Hit::Previous => self.media_command(Command::Previous),
            Hit::PlayPause => self.media_command(Command::TogglePlay),
            Hit::Next => self.media_command(Command::Next),
            Hit::Shuffle => {
                if let Some(on) = self.now_playing.as_ref().and_then(|m| m.shuffle) {
                    self.media_command(Command::Shuffle(!on));
                }
            }
            Hit::Repeat => {
                use windows::Media::MediaPlaybackAutoRepeatMode as Mode;
                if let Some(mode) = self.now_playing.as_ref().and_then(|m| m.repeat) {
                    let next = match mode {
                        Mode::None => Mode::List,
                        Mode::List => Mode::Track,
                        _ => Mode::None,
                    };
                    self.media_command(Command::Repeat(next));
                }
            }
            Hit::Seek(fraction) => {
                if let Some(m) = self.now_playing.as_ref().filter(|m| m.can_seek && m.duration > 0.0) {
                    self.media_command(Command::Seek(fraction * m.duration));
                }
            }
            Hit::Session(row) => {
                if let Some(session) = crate::pages::session_rows(&self.sessions).get(row) {
                    if !focus_session(session) {
                        log(format!("no window found for session {}", session.project));
                    }
                }
            }
            Hit::ShelfItem(_) => {} // pressed items are dragged or selected before APP is borrowed
            Hit::Segment(list) => {
                self.list = list;
                self.draw_shelf();
            }
            Hit::SelectAll => {
                self.shelves[self.list].select_all();
                self.draw_shelf();
            }
            Hit::ClearShelf => {
                self.shelves[self.list].clear();
                self.draw_shelf();
            }
        }
        Ok(())
    }

    /// A click on a shelf item (no drag) selects it or takes it out of the selection.
    fn toggle_shelf_item(&mut self, i: usize) {
        if let Some(item) = self.shelves[self.list].items.get_mut(i) {
            item.selected = !item.selected;
            self.draw_shelf();
        }
    }

    fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Result<Option<LRESULT>> {
        match msg {
            WM_MOUSEACTIVATE => return Ok(Some(LRESULT(MA_NOACTIVATE as isize))),
            WM_MOUSEMOVE => {
                if !self.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: if self.settings.expand_on_hover { TME_LEAVE | TME_HOVER } else { TME_LEAVE },
                        hwndTrack: self.hwnd,
                        dwHoverTime: self.settings.hover_delay_ms,
                    };
                    unsafe { TrackMouseEvent(&mut tme)? };
                    self.tracking = true;
                }
                unsafe { KillTimer(Some(self.hwnd), LEAVE_TIMER).ok() };
            }
            WM_MOUSEHOVER => self.expand()?,
            // A fast second click comes as a double click (the class asks for them, for the
            // shelf); anywhere else it is just another click.
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let (x, y) = ((lparam.0 & 0xFFFF) as i16 as i32, ((lparam.0 >> 16) & 0xFFFF) as i16 as i32);
                let hit = self.island.as_ref().and_then(|i| i.hit(x, y));
                match (self.view, hit) {
                    (View::Expanded, Some(hit)) => self.click(hit)?,
                    (View::Expanded, None) => {}
                    _ => self.expand()?,
                }
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                unsafe { SetTimer(Some(self.hwnd), LEAVE_TIMER, LEAVE_MS, None) };
            }
            WM_TIMER if wparam.0 == SHOT_TIMER => self.add_screenshots(),
            WM_TIMER if wparam.0 == LEAVE_TIMER => self.set_view(View::Compact)?,
            WM_TIMER if wparam.0 == PEEK_TIMER && self.view == View::Peek => self.set_view(View::Compact)?,
            WM_TIMER if wparam.0 == MINUTE_TIMER && self.view == View::Expanded => self.reload_claude(true),
            WM_TIMER if wparam.0 == SECOND_TIMER => {
                if let Some(island) = &self.island {
                    let (media, now) = (self.now_playing.as_ref(), claude::now_ms());
                    island.draw_times(media, now)?;
                    island.set_progress(media, now)?;
                }
                self.arm_clock();
            }
            WM_ANIM_DONE if wparam.0 == self.generation => self.set_region(self.pill_size()),
            WM_SHELL_EVENT => {
                self.refresh();
                // The newly active window brings itself to the top, and a fullscreen window
                // finishes resizing, only after the event reaches us: look again shortly.
                unsafe { SetTimer(Some(self.hwnd), RAISE_TIMER, RAISE_DELAY_MS, None) };
            }
            WM_APPBAR if wparam.0 == ABN_FULLSCREENAPP as usize => {
                self.refresh();
                unsafe { SetTimer(Some(self.hwnd), RAISE_TIMER, RAISE_DELAY_MS, None) };
            }
            WM_TIMER if wparam.0 == RAISE_TIMER => self.refresh(),
            WM_DPICHANGED | WM_DISPLAYCHANGE => {
                self.place(false)?;
                self.refresh();
            }
            WM_DEVICE_LOST => {
                log("rendering device replaced: rebuilding");
                self.place(true)?;
                self.refresh();
            }
            // Back from sleep or hibernation: the display, the players and the limits may all
            // have changed while we were away.
            WM_POWERBROADCAST if matches!(wparam.0 as u32, PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND) => {
                log("resumed");
                self.place(false)?;
                self.refresh();
                self.media_command(Command::Refresh);
                self.follow_audio();
                self.reload_privacy();
                self.reload_claude(false);
                usage::refresh();
                return Ok(Some(LRESULT(1)));
            }
            WM_MEDIA => self.media_changed(),
            WM_VOLUME => self.show_volume(wparam.0 as u32, lparam.0 != 0),
            WM_DRAG_FILES => self.files_dragged(wparam.0),
            // Files sent with `island.exe --shelf` (Explorer's "Send to").
            WM_COPYDATA => {
                let data = unsafe { (lparam.0 as *const COPYDATASTRUCT).as_ref() };
                let Some(data) = data.filter(|d| d.dwData == SHELF_COPYDATA && !d.lpData.is_null()) else { return Ok(None) };
                let bytes = unsafe { std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize) };
                let paths: Vec<std::path::PathBuf> = String::from_utf8_lossy(bytes).lines().filter(|l| !l.is_empty()).map(Into::into).collect();
                let added = self.shelves[FILES].add(paths, false);
                log(format!("shelf: {added} file(s) sent"));
                self.list = FILES;
                if self.view == View::Expanded && self.tab() == Some(Tab::Shelf) {
                    self.draw_shelf();
                } else if added > 0 {
                    let text = format!("{added} dosya rafta");
                    self.peek(Tab::Shelf, PEEK_MS, |island| island.draw_peek(&text, Some(0xFF0A84FF)));
                }
                return Ok(Some(LRESULT(1)));
            }
            WM_AUDIO_DEVICE => self.follow_audio(),
            // On the open shelf the wheel scrolls its row; elsewhere it switches players.
            WM_MOUSEWHEEL if self.view == View::Expanded && self.tab() == Some(Tab::Shelf) => {
                let delta = (wparam.0 >> 16) as u16 as i16;
                self.shelves[self.list].scroll(if delta < 0 { 1 } else { -1 });
                self.draw_shelf();
            }
            WM_MOUSEWHEEL => self.media_command(Command::NextSession),
            WM_COMMAND => match wparam.0 & 0xFFFF {
                ID_PLAY_PAUSE => self.media_command(Command::TogglePlay),
                ID_NEXT => self.media_command(Command::Next),
                ID_PREVIOUS => self.media_command(Command::Previous),
                ID_NEXT_SESSION => self.media_command(Command::NextSession),
                ID_AUTOSTART => {
                    let on = !autostart_enabled();
                    set_autostart(on)?;
                    log(format!("autostart {on}"));
                }
                ID_SETTINGS => open_settings(self.hwnd),
                ID_EXIT => unsafe { DestroyWindow(self.hwnd)? },
                _ => return Ok(None),
            },
            _ if msg == self.taskbar_created => {
                // Explorer restarted: tray icon and appbar registration are gone, and
                // explorer has a new process id.
                tray(self.hwnd, NIM_ADD);
                appbar(self.hwnd, ABM_REMOVE);
                appbar(self.hwnd, ABM_NEW);
                self.watch_explorer();
            }
            _ => return Ok(None),
        }
        Ok(Some(LRESULT(0)))
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // Handled without borrowing APP: the menu runs a modal loop that dispatches our messages.
    let tray_event = (lparam.0 & 0xFFFF) as u32;
    if msg == WM_RBUTTONUP || (msg == WM_TRAY && matches!(tray_event, WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT)) {
        show_menu(hwnd);
        return LRESULT(0);
    }
    // Before APP: a timer that fires while APP is busy must still stop, or it would wake us forever.
    if msg == WM_TIMER {
        unsafe {
            let _ = KillTimer(Some(hwnd), wparam.0);
        }
    }
    if msg == WM_DESTROY {
        tray(hwnd, NIM_DELETE);
        appbar(hwnd, ABM_REMOVE);
        unsafe {
            let _ = RevokeDragDrop(hwnd);
            PostQuitMessage(0);
        }
        return LRESULT(0);
    }
    // An item on the shelf: dragging it out runs a modal loop that dispatches our messages,
    // so it happens with APP free.
    if msg == WM_LBUTTONDOWN || msg == WM_LBUTTONDBLCLK {
        let pressed = APP.with(|cell| cell.try_borrow().ok()?.as_ref()?.shelf_item_at(lparam));
        if let Some((index, dragged, point)) = pressed {
            press_shelf_item(hwnd, index, &dragged, point, msg == WM_LBUTTONDBLCLK);
            return LRESULT(0);
        }
    }
    // try_borrow: calls made while handling a message can re-enter here synchronously.
    let handled = APP.with(|cell| {
        let mut slot = cell.try_borrow_mut().ok()?;
        match slot.as_mut()?.handle(msg, wparam, lparam) {
            Ok(result) => result,
            Err(e) => {
                log(format!("message {msg:#x} failed: {e}"));
                None
            }
        }
    });
    handled.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) })
}

unsafe extern "system" fn on_win_event(_: HWINEVENTHOOK, event: u32, hwnd: HWND, object: i32, child: i32, _: u32, _: u32) {
    if event != EVENT_SYSTEM_FOREGROUND
        && (object != OBJID_WINDOW.0 || child != CHILDID_SELF as i32 || fullscreen::class_name(hwnd) != "GhostDivider")
    {
        return;
    }
    // Posted, so it is handled from the message loop with APP free.
    unsafe {
        let _ = PostMessageW(Some(WINDOW.with(Cell::get)), WM_SHELL_EVENT, WPARAM(0), LPARAM(0));
    }
}

/// Registers as an appbar without taking any space, only to receive ABN_FULLSCREENAPP:
/// the notification the taskbar uses to get out of the way of fullscreen apps.
fn appbar(hwnd: HWND, message: u32) {
    let mut data = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: hwnd,
        uCallbackMessage: WM_APPBAR,
        ..Default::default()
    };
    unsafe { SHAppBarMessage(message, &mut data) };
}

/// Windows' "Animation effects" setting; off means the user asked for less motion.
fn animations_enabled() -> bool {
    let mut enabled = BOOL(1);
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut enabled as *mut BOOL as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    enabled.as_bool()
}

fn foreground_title() -> String {
    let mut buffer = [0u16; 256];
    let len = unsafe { GetWindowTextW(GetForegroundWindow(), &mut buffer) } as usize;
    String::from_utf16_lossy(&buffer[..len])
}

/// A press on shelf item `index`: a drag takes `dragged` (the selection, or just the item)
/// elsewhere and off the shelf once it lands; a click selects the item or takes it out of
/// the selection; a double click opens it (and leaves the selection as it was).
fn press_shelf_item(hwnd: HWND, index: usize, dragged: &[std::path::PathBuf], mut point: POINT, double: bool) {
    let with_app = |f: &mut dyn FnMut(&mut App)| {
        APP.with(|cell| {
            if let Some(app) = cell.try_borrow_mut().ok().as_deref_mut().and_then(Option::as_mut) {
                f(app);
            }
        })
    };
    if double {
        let mut path = None;
        with_app(&mut |app| {
            app.toggle_shelf_item(index); // undo the first click's toggle
            path = app.shelves[app.list].items.get(index).map(|item| item.path.clone());
        });
        if let Some(path) = path {
            unsafe { ShellExecuteW(Some(hwnd), w!("open"), &HSTRING::from(path.as_path()), None, None, SW_SHOWNORMAL) };
        }
        return;
    }
    // DragDetect waits for the mouse; a click that did not come from a held button (pen,
    // touch, a posted message) is a click.
    let dragging = unsafe {
        let _ = ClientToScreen(hwnd, &mut point);
        GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0 && DragDetect(hwnd, point).as_bool()
    };
    if !dragging {
        with_app(&mut |app| app.toggle_shelf_item(index));
    } else if shelf::drag_out(hwnd, dragged) {
        with_app(&mut |app| {
            app.shelves[app.list].remove(dragged);
            app.draw_shelf();
        });
    }
}

/// Brings a Claude session's window to the front: the first process up from its Claude
/// process that has a window (VS Code, Windows Terminal), and of several windows (VS Code
/// runs them all from one process) the one whose title names the project.
fn focus_session(session: &Session) -> bool {
    let mut windows: Vec<(HWND, u32, String)> = Vec::new();
    unsafe extern "system" fn collect(hwnd: HWND, list: LPARAM) -> BOOL {
        let list = unsafe { &mut *(list.0 as *mut Vec<(HWND, u32, String)>) };
        unsafe {
            let tool = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0;
            if IsWindowVisible(hwnd).as_bool() && !tool {
                let mut buffer = [0u16; 256];
                let len = GetWindowTextW(hwnd, &mut buffer) as usize;
                let mut pid = 0;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                if len > 0 {
                    list.push((hwnd, pid, String::from_utf16_lossy(&buffer[..len]).to_lowercase()));
                }
            }
        }
        TRUE
    }
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut windows as *mut _ as isize));
    }
    let project = session.project.to_lowercase();
    let names_project = |title: &String| !project.is_empty() && title.contains(&project);
    let processes = claude::processes();
    let mut pid = session.pid;
    let mut target = None;
    for _ in 0..8 {
        let mine: Vec<&(HWND, u32, String)> = windows.iter().filter(|w| pid != 0 && w.1 == pid).collect();
        if let Some(w) = mine.iter().find(|w| names_project(&w.2)).or(mine.first()) {
            target = Some(w.0);
            break;
        }
        pid = processes.get(&pid).map_or(0, |p| p.0);
    }
    let Some(hwnd) = target.or_else(|| windows.iter().find(|w| names_project(&w.2)).map(|w| w.0)) else { return false };
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd).as_bool()
    }
}

fn raise(hwnd: HWND) {
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOOWNERZORDER);
    }
}

fn tray(hwnd: HWND, message: NOTIFY_ICON_MESSAGE) {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
        uCallbackMessage: WM_TRAY,
        hIcon: app_icon(),
        ..Default::default()
    };
    let tip: Vec<u16> = "Dynamic Island".encode_utf16().collect();
    data.szTip[..tip.len()].copy_from_slice(&tip);
    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    unsafe {
        let _ = Shell_NotifyIconW(message, &data);
        if message == NIM_ADD {
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
        }
    }
}

/// The icon embedded by build.rs, at the small-icon size; shared, so loading it again for a
/// re-added tray icon costs nothing.
fn app_icon() -> HICON {
    unsafe {
        let loaded = GetModuleHandleW(None).and_then(|module| {
            let (w, h) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
            let id = PCWSTR(std::ptr::without_provenance(1)); // MAKEINTRESOURCE(1), the icon build.rs embeds
            LoadImageW(Some(module.into()), id, IMAGE_ICON, w, h, LR_SHARED)
        });
        match loaded {
            Ok(icon) => HICON(icon.0),
            Err(_) => LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
        }
    }
}

fn show_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let check = if autostart_enabled() { MF_CHECKED } else { MF_UNCHECKED };
        let _ = AppendMenuW(menu, MF_STRING | check, ID_AUTOSTART, w!("Başlangıçta çalıştır"));
        let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS, w!("Ayarlar dosyasını aç"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, w!("Çıkış"));
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        // Without the foreground the menu would not close when the user clicks elsewhere.
        let previous = GetForegroundWindow();
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, cursor.x, cursor.y, None, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        // Hand the keyboard back unless the user already clicked into another window.
        if GetForegroundWindow() == hwnd && !previous.is_invalid() {
            let _ = SetForegroundWindow(previous);
        }
    }
}

fn open_settings(hwnd: HWND) {
    settings::load(); // creates the file on first use
    if let Some(path) = settings::path() {
        unsafe { ShellExecuteW(Some(hwnd), w!("open"), &HSTRING::from(path.as_path()), None, None, SW_SHOWNORMAL) };
    }
}

fn autostart_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, None) }.is_ok()
}

fn set_autostart(on: bool) -> Result<()> {
    unsafe {
        if on {
            let exe = std::env::current_exe().map_err(|_| Error::from(E_FAIL))?;
            let command: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain([0]).collect();
            RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, REG_SZ.0, Some(command.as_ptr().cast()), (command.len() * 2) as u32)
                .ok()
        } else {
            RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE).ok()
        }
    }
}
