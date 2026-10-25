use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
};

use windows::{
    core::*,
    Foundation::TypedEventHandler,
    UI::Composition::CompositionScopedBatch,
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC, SND_NODEFAULT},
        Storage::FileSystem::*,
        System::{
            DataExchange::COPYDATASTRUCT,
            LibraryLoader::GetModuleHandleW,
            SystemInformation::GetTickCount64,
            WinRT::{RoInitialize, RO_INIT_MULTITHREADED},
            Ole::{IDropTarget, OleInitialize, RegisterDragDrop, RevokeDragDrop},
            Registry::*,
            Threading::INFINITE,
        },
        UI::{Accessibility::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
    },
};

use crate::{
    audio, bluetooth,
    claude::{self, Limits, Session, State},
    fullscreen,
    island::{self, Hit, Island, Line, Tab, View, TOP},
    headset::{self, Battery, HeadsetLink},
    liked, log,
    media::{Command, Media, MediaLink},
    notices,
    pages::{GREEN, RED},
    preferences::{self, Preferences},
    settings::{self, Settings},
    shelf::{self, DropTarget, Shelf},
    pages::{ORANGE, TEXT},
    text::{self, tr},
    timer, usage,
};

const LEAVE_MS: u32 = 400;
const PEEK_MS: u32 = 2500;

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
const WM_NOTICE: u32 = WM_APP + 11; // a new Windows notification to preview
const WM_BLUETOOTH: u32 = WM_APP + 12; // LPARAM: a Box<bluetooth::Device> the sound now goes to
const NOTICE_MS: u32 = 4000;
const SHELF_COPYDATA: usize = 0x5348_454C; // "SHEL": WM_COPYDATA from `island.exe --shelf`
const WM_HEADSET: u32 = WM_APP + 9;
const DBT_DEVNODES_CHANGED: usize = 0x0007; // a device was added or removed (any top-level window gets it)
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
const BUBBLE_TIMER: usize = 7; // while the Claude bubble is grown
const CLOCK_TIMER: usize = 8; // at each new minute, while the closed pill shows the clock
const METER_TIMER: usize = 9; // the equalizer's steps, while music plays on the closed pill
const METER_MS: u32 = 100;
const TIMER_TICK: usize = 10; // each second the timer shows, while it runs
const ALARM_MS: u32 = 6000; // the "time's up" peek
const SHOT_DELAY_MS: u32 = 400;
// The shelf's two lists.
const FILES: usize = 0;
const SCREENSHOTS: usize = 1;
const TRAY_ID: u32 = 1;
const ID_AUTOSTART: usize = 1;
const ID_SETTINGS: usize = 2;
const ID_EXIT: usize = 3;
const ID_LIKED: usize = 4;
const ID_QUIET: usize = 5; // do not disturb
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
    media: Option<MediaLink>,
    notices: Option<notices::Notices>,
    now_playing: Option<Media>,
    peek_tab: Tab, // the tab a peek opens to
    volume: Option<audio::Volume>,
    last_volume: Option<(u32, bool)>, // shown or current level and mute
    metering: bool,                   // the equalizer follows the output's level now
    timer: timer::Timer,
    shelves: [Shelf; 2], // FILES dropped on the island, SCREENSHOTS taken
    list: usize,         // which of them the shelf page shows
    dropping: bool,      // files are being dragged over the island
    shots_since: std::time::SystemTime, // the island's start: only screenshots after it show
    shots_seen: HashSet<std::path::PathBuf>, // screenshots already shown once, kept or not
    headset: Option<HeadsetLink>,
    battery: Option<Battery>, // the headset's, as shown
    liked: Vec<liked::Song>,                 // newest first
    preferences: Option<Preferences>,        // the settings window, while open
    region: RECT,                            // the clickable part of the window, as last set
}

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

/// A second start of the island opens the running one's settings window.
pub fn show_running_settings() {
    unsafe {
        let Ok(island) = FindWindowW(w!("NightdijitalIsland"), None) else { return };
        let mut pid = 0;
        GetWindowThreadProcessId(island, Some(&mut pid));
        let _ = AllowSetForegroundWindow(pid); // ours to give: the user just started us
        let _ = PostMessageW(Some(island), WM_COMMAND, WPARAM(ID_SETTINGS), LPARAM(0));
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
        media: None,
        notices: None,
        now_playing: None,
        peek_tab: Tab::Claude,
        volume: None,
        last_volume: None,
        metering: false,
        timer: timer::Timer::new(),
        shelves: Default::default(),
        list: FILES,
        dropping: false,
        shots_since: std::time::SystemTime::now(),
        shots_seen: HashSet::new(),
        headset: Some(HeadsetLink::start(hwnd, WM_HEADSET)),
        battery: None,
        liked: liked::load(),
        preferences: None,
        region: RECT::default(),
    };
    text::set(app.settings.language);
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
    app.notices = Some(notices::Notices::start(hwnd, WM_NOTICE, (crate::pages::NOTICE_ICON * app.scale).round() as u32));
    app.watch_explorer();
    appbar(hwnd, ABM_NEW);
    app.hidden = fullscreen::should_hide(hwnd, &app.settings);
    app.read_notices();
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
    let watches: Vec<Watch> = folders
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
                island.set_headset(self.battery)?;
                island.set_media(media, claude::now_ms())?;
                island.set_art(media.and_then(|m| m.art.as_deref()), false)?;
            }
            self.show_like();
            self.show_limit();
            self.show_timer(); // and the rest that lives

            log(format!("placed at scale {scale} on {}x{}", r.right - r.left, r.bottom - r.top));
        }
        self.set_region(self.pill_size());
        Ok(())
    }

    fn reload_settings(&mut self) {
        let new = settings::load();
        if new == self.settings {
            return;
        }
        log(format!("settings: {new:?}"));
        let language = new.language != self.settings.language;
        self.settings = new;
        if language {
            // Everything drawn shows text: build the island again, in the new language.
            text::set(self.settings.language);
            if let Err(e) = self.place(true) {
                log(format!("relayout for the language failed: {e}"));
            }
            self.media_command(Command::Refresh); // player names
            if let Some(window) = &self.preferences {
                window.retitle();
            }
        }
        self.refresh(); // the exe lists may change what counts as "hide"
        self.update_live();
        self.read_notices();
        self.draw_preferences();
    }

    /// Notifications are read only while a preview could show: the island is on screen,
    /// previews are on, and it may disturb.
    fn read_notices(&self) {
        if let Some(notices) = &self.notices {
            notices.read(!self.hidden && self.settings.notifications && !self.settings.do_not_disturb);
        }
    }

    /// Opens the settings window, or brings it back to the front. `offscreen` for tests.
    fn open_preferences(&mut self, offscreen: bool) {
        if let Some(window) = &self.preferences {
            window.show();
            return;
        }
        match Preferences::open(Some(preferences_proc), offscreen) {
            Ok(mut window) => {
                window.autostart = autostart_enabled();
                window.claude = claude::connected();
                log("settings window opened");
                self.preferences = Some(window);
                self.draw_preferences();
            }
            Err(e) => log(format!("settings window: {e}")),
        }
    }

    fn draw_preferences(&mut self) {
        if let Some(window) = self.preferences.as_mut() {
            window.draw(&self.settings, &self.liked);
        }
    }

    fn preferences_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match self.preferences.as_mut()?.handle(msg, wparam, lparam) {
            preferences::Event::Unhandled => return None,
            preferences::Event::Nothing => {}
            preferences::Event::Redraw => self.draw_preferences(),
            preferences::Event::Act(action) => {
                self.preference(action);
                self.draw_preferences();
            }
            preferences::Event::Closed => {
                self.preferences = None;
                log("settings window closed");
            }
        }
        Some(LRESULT(0))
    }

    /// Does what was clicked in the settings window. Settings go through the file, as if
    /// edited by hand.
    fn preference(&mut self, action: preferences::Action) {
        use preferences::{Action, Link, List, Switch};
        let mut settings = self.settings.clone();
        fn names(s: &mut Settings, list: List) -> &mut Vec<String> {
            if list == List::Hide {
                &mut s.hide_exe
            } else {
                &mut s.show_exe
            }
        }
        match action {
            Action::Toggle(Switch::Autostart) => {
                if let Err(e) = set_autostart(!autostart_enabled()) {
                    log(format!("autostart: {e}"));
                }
                if let Some(window) = self.preferences.as_mut() {
                    window.autostart = autostart_enabled();
                }
            }
            Action::Toggle(Switch::Claude) => {
                claude::setup(claude::connected() != Some(true));
                if let Some(window) = self.preferences.as_mut() {
                    window.claude = claude::connected();
                }
            }
            Action::Toggle(switch) => {
                switch.flip(&mut settings);
            }
            Action::Delay(ms) => settings.hover_delay_ms = ms,
            Action::NewsTime(seconds) => settings.claude_news_seconds = seconds,
            Action::Language(language) => settings.language = language,
            Action::Add(list, exe) => {
                let names = names(&mut settings, list);
                if !names.contains(&exe) {
                    names.push(exe);
                }
            }
            Action::Remove(list, index) => {
                let names = names(&mut settings, list);
                if index < names.len() {
                    names.remove(index);
                }
            }
            Action::Unlike(index) => {
                if index < self.liked.len() {
                    self.liked.remove(index);
                    liked::save(&self.liked);
                    self.show_like();
                }
            }
            Action::Search(index) => {
                if let Some(song) = self.liked.get(index) {
                    shell_open(self.hwnd, &HSTRING::from(liked::search_url(song)));
                }
            }
            Action::Open(Link::Website) => shell_open(self.hwnd, h!("https://nightdijital.com")),
            Action::Open(Link::SettingsFile) => open_settings(self.hwnd),
            Action::Open(Link::Log) => {
                if let Some(path) = crate::log_path() {
                    shell_open(self.hwnd, &HSTRING::from(path.as_path()));
                }
            }
            Action::Section(_) | Action::Pick(_) | Action::Close => {}
        }
        if settings != self.settings {
            text::set(settings.language); // the file's notes come in the language just picked
            settings::save(&settings);
            self.reload_settings();
        }
    }

    /// The heart on the music page, for the song playing.
    fn show_like(&mut self) {
        let song = self.now_playing.as_ref().filter(|m| !m.title.is_empty());
        let liked = song.map(|m| liked::find(&self.liked, &m.title, &m.artist).is_some());
        if let Some(Err(e)) = self.island.as_mut().map(|island| island.draw_like(liked)) {
            log(format!("heart failed: {e}"));
        }
    }

    /// The heart was pressed: the song goes on the liked list, or comes off it.
    fn toggle_like(&mut self) {
        let Some(m) = self.now_playing.as_ref().filter(|m| !m.title.is_empty()) else { return };
        match liked::find(&self.liked, &m.title, &m.artist) {
            Some(index) => {
                self.liked.remove(index);
            }
            None => {
                let song = liked::Song { title: m.title.clone(), artist: m.artist.clone(), album: m.album.clone(), app: m.app.clone(), added: liked::now() };
                self.liked.insert(0, song);
            }
        }
        log(format!("liked songs: {}", self.liked.len()));
        liked::save(&self.liked);
        self.show_like();
        self.draw_preferences();
    }

    /// Starts or stops what moves on its own: the equalizer (music playing, island closed
    /// and on screen), the progress fill and the clock (music page on screen and playing).
    /// Each costs frames or wakeups, so none runs where it cannot be seen.
    fn update_live(&mut self) {
        let playing = self.now_playing.as_ref().is_some_and(|m| m.playing);
        // A paused song stays on the closed pill too, with still bars.
        let song = self.now_playing.as_ref().is_some_and(|m| m.playing || !m.title.is_empty());
        let line = if song { Line::Music } else { Line::Idle };
        let motion = animations_enabled();
        // A running timer takes the equalizer's place.
        let equalizer = playing && !self.timer.active() && self.view == View::Compact && !self.hidden && self.settings.equalizer && motion;
        let now = claude::now_ms();
        let before = self.pill_size();
        // The bars follow how loud the output plays, or without a level meter, a wave.
        let metered = self.volume.as_ref().is_some_and(|v| v.peak().is_some());
        if let Some(island) = self.island.as_mut() {
            island.set_full_motion(motion);
            let updated = island.set_line(line, equalizer && !metered, self.settings.glow).and_then(|_| island.set_progress(self.now_playing.as_ref(), now));
            if let Err(e) = updated {
                log(format!("live parts failed: {e}"));
            }
        }
        let metering = equalizer && metered;
        if metering != self.metering {
            self.metering = metering;
            unsafe {
                if metering {
                    SetTimer(Some(self.hwnd), METER_TIMER, METER_MS, None);
                } else {
                    let _ = KillTimer(Some(self.hwnd), METER_TIMER);
                }
            }
        }
        // Music starting or stopping changes the closed pill's width.
        if self.pill_size() != before {
            if let Err(e) = self.reshape(before) {
                log(format!("line resize failed: {e}"));
            }
        } else {
            self.set_region(self.pill_size()); // the bubble may have come out or gone in
        }
        self.arm_clock();
        self.show_clock(line == Line::Idle);
    }

    /// The closed pill's clock, with no song (or a running timer in its place): drawn now and
    /// again at each new minute, only while it can be seen.
    fn show_clock(&mut self, idle: bool) {
        if !idle || self.hidden || self.view != View::Compact {
            unsafe {
                let _ = KillTimer(Some(self.hwnd), CLOCK_TIMER);
            }
            return;
        }
        let clock = if self.timer.active() {
            let paused = matches!(self.timer.state, timer::State::Paused { .. });
            let caption = if paused { tr("Duraklatıldı", "Paused") } else { tr("Zamanlayıcı", "Timer") };
            (timer::clock(self.timer.left(ticks())), caption.to_string(), ORANGE)
        } else {
            let (time, day) = text::clock();
            (time, day, TEXT)
        };
        if let Some(Err(e)) = self.island.as_mut().map(|island| island.draw_clock(clock)) {
            log(format!("clock failed: {e}"));
        }
        let to_next_minute = 60_000 - (claude::now_ms() % 60_000) as u32 + 50;
        unsafe { SetTimer(Some(self.hwnd), CLOCK_TIMER, to_next_minute, None) };
    }

    /// The timer everywhere it shows: in the closed pill (in the clock's or the equalizer's
    /// place) and on its page. While it runs it comes back each time the second shown changes.
    fn show_timer(&mut self) {
        let now = ticks();
        let time = self.timer.active().then(|| timer::clock(self.timer.left(now)));
        if let Some(Err(e)) = self.island.as_mut().map(|island| island.set_music_timer(time.as_deref())) {
            log(format!("timer failed: {e}"));
        }
        self.draw_timer_page();
        self.update_live(); // the clock's place, and the equalizer gives way
        unsafe {
            if matches!(self.timer.state, timer::State::Running { .. }) {
                let next = match self.timer.left(now) % 1000 {
                    0 => 1000,
                    part => part,
                };
                SetTimer(Some(self.hwnd), TIMER_TICK, next as u32 + 5, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_TICK);
            }
        }
    }

    fn draw_timer_page(&self) {
        if self.view != View::Expanded || self.tab() != Some(Tab::Timer) {
            return;
        }
        if let Some(Err(e)) = self.island.as_ref().map(|island| island.draw_timer(&self.timer, ticks())) {
            log(format!("timer page failed: {e}"));
        }
    }

    /// A second of the timer has gone by; at the end the alarm rings and a peek says so, even
    /// when the island should not disturb: the user set it.
    fn timer_tick(&mut self) {
        if self.timer.finished(ticks()) {
            log("timer: time's up");
            unsafe {
                let _ = PlaySoundW(w!("Notification.Looping.Alarm"), None, SND_ALIAS | SND_ASYNC | SND_NODEFAULT);
            }
            let minutes = self.timer.minutes;
            let text = tr(format!("Süre doldu · {minutes} dk"), format!("Time's up · {minutes} min"));
            self.show_peek(Tab::Timer, ALARM_MS, |island| island.draw_peek(&text, Some(ORANGE)));
        }
        self.show_timer();
    }

    /// A step of the equalizer, with the output's level now.
    fn meter_tick(&mut self) {
        if !self.metering {
            return; // a tick posted before the timer was stopped
        }
        let peak = self.volume.as_ref().and_then(audio::Volume::peak).unwrap_or(0.0);
        if let Some(Err(e)) = self.island.as_mut().map(|island| island.show_level(peak)) {
            log(format!("equalizer failed: {e}"));
        }
        unsafe { SetTimer(Some(self.hwnd), METER_TIMER, METER_MS, None) };
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
    /// permission prompt, a finished answer) grows the bubble for a while, unless its window
    /// is already in front or `notify` is off.
    fn reload_claude(&mut self, notify: bool) {
        let (sessions, limits) = claude::load();
        let front = foreground_title().to_lowercase();
        let mut news: Option<&Session> = None;
        for session in &sessions {
            if self.states.get(&session.id) == Some(&session.state) {
                continue;
            }
            usage::refresh(); // Claude is in use, so the limits are moving (at most every 3 min)
            if matches!(session.state, State::Permission | State::Done) {
                let in_front = !session.project.is_empty() && front.contains(&session.project.to_lowercase());
                if notify && !in_front && news.is_none_or(|n| n.state != State::Permission) {
                    news = Some(session);
                }
            }
        }
        self.states = sessions.iter().map(|s| (s.id.clone(), s.state)).collect();
        let news = news.filter(|_| self.settings.claude_peek && !self.settings.do_not_disturb).map(|s| {
            let about = if s.project.is_empty() { "Claude" } else { &s.project };
            (s.state.headline().to_string(), about.to_string(), s.state.color())
        });
        self.sessions = sessions;
        self.limits = limits;

        if self.view == View::Expanded && self.tab() == Some(Tab::Claude) {
            self.viewing_claude();
        } else {
            self.show_limit();
            if news.is_some() {
                self.show_news(news);
            }
        }
        self.update_live();
    }

    /// Opens the island a little for news, then closes it again after `ms`. Not over
    /// fullscreen apps and not when the user has it open already; newer news replaces a peek.
    /// A click on the peek opens `tab`.
    fn peek(&mut self, tab: Tab, ms: u32, draw: impl FnOnce(&mut Island) -> Result<()>) {
        if !self.settings.do_not_disturb {
            self.show_peek(tab, ms, draw);
        }
    }

    /// As `peek`, even while the island should not disturb: for what the user asked for.
    fn show_peek(&mut self, tab: Tab, ms: u32, draw: impl FnOnce(&mut Island) -> Result<()>) {
        if self.hidden || self.view == View::Expanded {
            return;
        }
        let before = self.pill_size();
        let drawn = self.island.as_mut().map_or(Ok(()), draw);
        self.peek_tab = tab;
        // A peek that replaces another may be taller or shorter (a notification is taller).
        let shown = drawn.and_then(|_| if self.view == View::Peek { self.reshape(before) } else { self.set_view(View::Peek) });
        match shown {
            Ok(()) => unsafe {
                SetTimer(Some(self.hwnd), PEEK_TIMER, ms, None);
            },
            Err(e) => log(format!("peek failed: {e}")),
        }
    }

    /// The headset battery changed: the tab bar shows it; plugging in and running low peek.
    fn headset_changed(&mut self) {
        let new = self.headset.as_ref().and_then(HeadsetLink::current);
        let old = std::mem::replace(&mut self.battery, new);
        if let Some(Err(e)) = self.island.as_mut().map(|island| island.set_headset(new)) {
            log(format!("headset draw failed: {e}"));
        }
        if let Some((text, charging)) = new.and_then(|now| headset::news(old, now)) {
            let color = if charging { GREEN } else { RED };
            self.peek(Tab::Music, PEEK_MS, |island| island.draw_peek(&text, Some(color)));
        }
    }

    /// Listens to the default output device, after startup or a switch of device; its
    /// current level is not news.
    fn follow_audio(&mut self) {
        let Some(volume) = self.volume.as_mut() else { return };
        self.last_volume = volume.follow_default().map_err(|e| log(format!("volume device: {e}"))).ok();
    }

    /// The sound moved to another device: when it is a Bluetooth headset, a card with its name
    /// and battery (WM_BLUETOOTH). Looked up off this thread; a headset that just connected
    /// tells its battery a moment later, so a few tries a second apart.
    fn find_headset(&self) {
        if !self.settings.bluetooth_peek {
            return;
        }
        let window = self.hwnd.0 as isize;
        std::thread::spawn(move || {
            if unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.is_err() {
                return;
            }
            for attempt in 0..4 {
                match bluetooth::output() {
                    Ok(Some(device)) if device.battery.is_some() || attempt == 3 => {
                        let device = Box::into_raw(Box::new(device));
                        if unsafe { PostMessageW(Some(HWND(window as _)), WM_BLUETOOTH, WPARAM(0), LPARAM(device as isize)) }.is_err() {
                            drop(unsafe { Box::from_raw(device) });
                        }
                        return;
                    }
                    Ok(Some(_)) => std::thread::sleep(std::time::Duration::from_secs(1)),
                    Ok(None) => return,
                    Err(e) => return log(format!("bluetooth: {e}")),
                }
            }
        });
    }

    /// The system volume changed: a short peek with the level.
    fn show_volume(&mut self, level: u32, muted: bool) {
        // Balance and per-channel changes notify too, with the same master level.
        if self.last_volume == Some((level, muted)) {
            return;
        }
        self.last_volume = Some((level, muted));
        if !self.settings.volume_peek {
            return;
        }
        self.peek(Tab::Music, VOLUME_MS, |island| island.draw_volume(level, muted));
    }

    /// The bubble's 5-hour limit ring.
    fn show_limit(&mut self) {
        // ponytail: a limit that resets while nothing happens shows 0 only at the next event
        let limit = claude::five_hour_used(&self.limits, claude::now_ms() / 1000);
        if let Some(Err(e)) = self.island.as_mut().map(|island| island.set_bubble(limit)) {
            log(format!("limit bubble failed: {e}"));
        }
    }

    /// Grows the Claude bubble with `news` (a status, what it is about, its color) for the
    /// time the settings give it, or takes it back to the limit ring. The clickable area grows
    /// before the bubble does and shrinks after it.
    fn show_news(&mut self, news: Option<(String, String, u32)>) {
        let Some(island) = self.island.as_mut() else { return };
        let grows = news.is_some();
        let shown = island.set_news(news.as_ref().map(|(status, about, color)| (status.as_str(), about.as_str(), *color)));
        let shown = shown.and_then(|batch| {
            if grows {
                self.set_region(self.pill_size());
                unsafe { SetTimer(Some(self.hwnd), BUBBLE_TIMER, self.settings.claude_news_seconds * 1000, None) };
                Ok(())
            } else {
                self.generation += 1;
                self.shrink_region_after(&batch)
            }
        });
        if let Err(e) = shown {
            log(format!("bubble news failed: {e}"));
        }
    }

    /// The Claude tab is on screen: draw it fresh, take back the bubble's news (seen now), and
    /// keep the reset countdowns current to the minute.
    fn viewing_claude(&mut self) {
        usage::refresh();
        self.show_limit();
        if self.island.as_ref().is_some_and(Island::has_news) {
            self.show_news(None);
        }
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
            self.draw_timer_page();
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
            let text = match added {
                1 => tr("Ekran görüntüsü rafta", "Screenshot on the shelf").to_string(),
                n => tr(format!("{n} ekran görüntüsü rafta"), format!("{n} screenshots on the shelf")),
            };
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
    /// The pill (and the limit bubble beside it, when out); set again only when it changes.
    fn set_region(&mut self, (w, h): (f32, f32)) {
        let s = self.scale;
        let (window_w, _) = island::window_size(s);
        let pad = 9.0 * s; // the pill's shadow and glow
        let rw = w * s + 2.0 * pad;
        let left = ((window_w as f32 - rw) / 2.0).floor() as i32;
        let bubble = self.island.as_ref().map_or(0.0, |island| island.bubble_extent()) * s;
        let rect = RECT { left, top: 0, right: left + (rw + bubble).ceil() as i32, bottom: ((TOP + h) * s + pad).ceil() as i32 };
        if rect == self.region {
            return;
        }
        self.region = rect;
        unsafe {
            let rgn = CreateRectRgn(rect.left, rect.top, rect.right, rect.bottom);
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
            self.read_notices();
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
            self.draw_timer_page();
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
            self.shrink_region_after(&batch)?;
        }
        Ok(())
    }

    /// Once `batch` has settled, the clickable area shrinks to what is on screen
    /// (WM_ANIM_DONE), unless a newer transition has started since.
    fn shrink_region_after(&self, batch: &CompositionScopedBatch) -> Result<()> {
        let (hwnd, generation) = (self.hwnd.0 as isize, self.generation);
        batch.Completed(&TypedEventHandler::new(move |_, _| {
            unsafe { PostMessageW(Some(HWND(hwnd as _)), WM_ANIM_DONE, WPARAM(generation), LPARAM(0))? };
            Ok(())
        }))?;
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
        let shown = |m: &Media| (m.can_previous, m.can_toggle, m.can_next, m.duration > 0.0, m.shuffle, m.repeat, m.sessions, m.index);
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
            if new_track && old.is_some() {
                drawn = drawn.and_then(|_| island.new_track());
            }
            if let Err(e) = drawn {
                log(format!("media draw failed: {e}"));
            }
        }
        self.now_playing = media;
        if redraw {
            self.show_like();
        }
        self.update_live();

        let song = self.now_playing.as_ref().filter(|m| new_track && m.playing && !m.title.is_empty());
        if let Some(text) = song.map(|m| if m.artist.is_empty() { m.title.clone() } else { format!("{} — {}", m.title, m.artist) }) {
            if self.settings.song_peek {
                self.peek(Tab::Music, PEEK_MS, |island| island.draw_peek(&text, None));
            }
        }
    }

    /// The closed pill grows a little under the mouse, and back when it leaves.
    fn set_hover(&mut self, on: bool) {
        let before = self.pill_size();
        let Some(island) = self.island.as_mut() else { return };
        if island.set_hover(on) && self.view != View::Expanded {
            if let Err(e) = self.reshape(before) {
                log(format!("hover failed: {e}"));
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
            Hit::Like => self.toggle_like(),
            Hit::ShowPlayer => {
                if let Some(m) = &self.now_playing {
                    crate::player::show(m.app_id.clone(), m.title.clone());
                }
            }
            Hit::NextPlayer => self.media_command(Command::NextSession),
            Hit::TimerMinutes(minutes) => {
                self.timer.set_minutes(minutes);
                self.show_timer();
            }
            Hit::TimerToggle => {
                self.timer.toggle(ticks());
                self.show_timer();
            }
            Hit::TimerReset => {
                self.timer.reset();
                self.show_timer();
            }
            Hit::Settings => self.open_preferences(false),
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
                    self.set_hover(true);
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
                    _ => {
                        // The bubble opens the island on Claude's limits.
                        if let Some(island) = self.island.as_mut().filter(|island| island.on_bubble(x, y)) {
                            island.set_tab(Tab::Claude)?;
                            self.peek_tab = Tab::Claude;
                        }
                        self.expand()?
                    }
                }
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                self.set_hover(false);
                unsafe { SetTimer(Some(self.hwnd), LEAVE_TIMER, LEAVE_MS, None) };
            }
            WM_TIMER if wparam.0 == SHOT_TIMER => self.add_screenshots(),
            WM_TIMER if wparam.0 == LEAVE_TIMER => self.set_view(View::Compact)?,
            WM_TIMER if wparam.0 == PEEK_TIMER && self.view == View::Peek => self.set_view(View::Compact)?,
            WM_TIMER if wparam.0 == BUBBLE_TIMER => self.show_news(None),
            WM_TIMER if wparam.0 == CLOCK_TIMER => self.update_live(),
            WM_TIMER if wparam.0 == METER_TIMER => self.meter_tick(),
            WM_TIMER if wparam.0 == TIMER_TICK => self.timer_tick(),
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
                self.reload_claude(false);
                usage::refresh();
                return Ok(Some(LRESULT(1)));
            }
            WM_MEDIA => self.media_changed(),
            WM_HEADSET => self.headset_changed(),
            WM_DEVICECHANGE if wparam.0 == DBT_DEVNODES_CHANGED => {
                if let Some(headset) = &self.headset {
                    headset.rescan();
                }
                return Ok(None);
            }
            WM_VOLUME => self.show_volume(wparam.0 as u32, lparam.0 != 0),
            WM_DRAG_FILES => self.files_dragged(wparam.0),
            WM_NOTICE => {
                if let Some(notice) = self.notices.as_ref().and_then(notices::Notices::take) {
                    let tab = self.tab().unwrap_or(Tab::Music);
                    self.peek(tab, NOTICE_MS, |island| island.draw_notice(&notice));
                }
            }
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
                    let text = match added {
                        1 => tr("1 dosya rafta", "1 file on the shelf").to_string(),
                        n => tr(format!("{n} dosya rafta"), format!("{n} files on the shelf")),
                    };
                    self.peek(Tab::Shelf, PEEK_MS, |island| island.draw_peek(&text, Some(0xFF0A84FF)));
                }
                return Ok(Some(LRESULT(1)));
            }
            WM_AUDIO_DEVICE => {
                self.follow_audio();
                self.update_live(); // the equalizer listens to the new device
                self.find_headset();
            }
            WM_BLUETOOTH => {
                let device = unsafe { Box::from_raw(lparam.0 as *mut bluetooth::Device) };
                log(format!("bluetooth: {device:?}"));
                self.peek(Tab::Music, NOTICE_MS, |island| island.draw_device(&device.name, device.battery));
            }
            // On the open shelf the wheel scrolls its row; elsewhere it switches players.
            WM_MOUSEWHEEL if self.view == View::Expanded && self.tab() == Some(Tab::Shelf) => {
                let delta = (wparam.0 >> 16) as u16 as i16;
                self.shelves[self.list].scroll(if delta < 0 { 1 } else { -1 });
                self.draw_shelf();
            }
            // On the timer page it sets the minutes.
            WM_MOUSEWHEEL if self.view == View::Expanded && self.tab() == Some(Tab::Timer) => {
                let delta = (wparam.0 >> 16) as u16 as i16;
                self.timer.nudge(if delta > 0 { 1 } else { -1 });
                self.show_timer();
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
                ID_SETTINGS => self.open_preferences(lparam.0 == 1), // 1: tests, off screen
                ID_QUIET => {
                    let mut settings = self.settings.clone();
                    settings.do_not_disturb = !settings.do_not_disturb;
                    settings::save(&settings);
                    self.reload_settings();
                }
                ID_LIKED => {
                    self.open_preferences(false);
                    if let Some(window) = self.preferences.as_mut() {
                        window.show_section(preferences::Section::Liked);
                    }
                    self.draw_preferences();
                }
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

/// The settings window's messages go to the island's App, which holds the window.
extern "system" fn preferences_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let handled = APP.with(|cell| cell.try_borrow_mut().ok()?.as_mut()?.preferences_message(msg, wparam, lparam));
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
        let item = |turkish: &str, english: &str| HSTRING::from(tr(turkish, english));
        let _ = AppendMenuW(menu, MF_STRING | check, ID_AUTOSTART, &item("Başlangıçta çalıştır", "Start with Windows"));
        let _ = AppendMenuW(menu, MF_STRING, ID_SETTINGS, &item("Ayarlar", "Settings"));
        let _ = AppendMenuW(menu, MF_STRING, ID_LIKED, &item("Beğenilen şarkılar", "Liked songs"));
        let quiet = if settings::load().do_not_disturb { MF_CHECKED } else { MF_UNCHECKED };
        let _ = AppendMenuW(menu, MF_STRING | quiet, ID_QUIET, &item("Rahatsız etmeyin", "Do not disturb"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, &item("Çıkış", "Quit"));
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
        shell_open(hwnd, &HSTRING::from(path.as_path()));
    }
}

/// Opens a file or a web page with what Windows opens it with.
/// Milliseconds since Windows started: the timer's clock, which a change of the time does not move.
fn ticks() -> u64 {
    unsafe { GetTickCount64() }
}

fn shell_open(hwnd: HWND, target: &HSTRING) {
    unsafe { ShellExecuteW(Some(hwnd), w!("open"), target, None, None, SW_SHOWNORMAL) };
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
