//! New Windows notifications, for a short preview on the island. Windows tells only packaged
//! apps when one arrives (NotificationChanged fails for the others) and writes its database
//! late, so a thread of ours reads the list every POLL, only while the island can show
//! something, and hands over the ones it has not seen.

use std::{
    collections::HashSet,
    sync::{
        mpsc::{self, Receiver, RecvTimeoutError},
        Arc, Mutex,
    },
    time::Duration,
};

use windows::{
    core::*,
    Foundation::Size,
    UI::Notifications::{KnownNotificationBindings, Management::UserNotificationListener, NotificationKinds, UserNotification},
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED},
        UI::WindowsAndMessaging::PostMessageW,
    },
};

use crate::{media::Art, text::tr};

/// How often the list is read: about 2 ms of CPU each time (measured).
const POLL: Duration = Duration::from_secs(2);
/// A notification older than this when first seen is not news (it came while the island
/// could not show it).
const FRESH_SECONDS: i64 = 10;

pub struct Notice {
    pub app: String,
    pub title: String,
    pub body: String,
    pub icon: Option<Art>,
}

pub struct Notices {
    switch: mpsc::Sender<bool>,
    latest: Arc<Mutex<Option<Notice>>>,
}

impl Notices {
    /// Starts reading, off; `message` is posted to `hwnd` when a notice is ready to take.
    /// Icons are decoded to `icon_px`.
    pub fn start(hwnd: HWND, message: u32, icon_px: u32) -> Self {
        let (switch, inbox) = mpsc::channel();
        let latest = Arc::new(Mutex::new(None));
        let (slot, window) = (latest.clone(), hwnd.0 as isize);
        std::thread::spawn(move || {
            if let Err(e) = run(&inbox, &slot, window, message, icon_px) {
                crate::log(format!("notifications: {e}"));
            }
        });
        Self { switch, latest }
    }

    /// Reads while `on`: the island is on screen and previews are wanted.
    pub fn read(&self, on: bool) {
        let _ = self.switch.send(on);
    }

    pub fn take(&self) -> Option<Notice> {
        self.latest.lock().ok()?.take()
    }
}

fn run(inbox: &Receiver<bool>, slot: &Mutex<Option<Notice>>, window: isize, message: u32, icon_px: u32) -> Result<()> {
    unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
    let listener = UserNotificationListener::Current()?;
    let mut seen: HashSet<u32> = HashSet::new();
    let mut on = false;
    loop {
        // Off, nothing wakes this thread but the switch.
        let next = if on { inbox.recv_timeout(POLL) } else { inbox.recv().map_err(|_| RecvTimeoutError::Disconnected) };
        match next {
            Ok(value) => on = value,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        if !on {
            continue;
        }
        let list = match listener.GetNotificationsAsync(NotificationKinds::Toast).and_then(|op| op.join()) {
            Ok(list) => list,
            Err(e) => {
                crate::log(format!("notifications: {e}"));
                continue;
            }
        };
        let now = now_ticks();
        let mut newest: Option<(i64, UserNotification)> = None;
        let mut ids = HashSet::new();
        for n in &list {
            let Ok(id) = n.Id() else { continue };
            ids.insert(id);
            if seen.contains(&id) {
                continue;
            }
            let created = n.CreationTime().map_or(0, |t| t.UniversalTime);
            let fresh = now - created < FRESH_SECONDS * 10_000_000;
            if fresh && newest.as_ref().is_none_or(|(t, _)| created > *t) {
                newest = Some((created, n));
            }
        }
        seen = ids; // what was dismissed is forgotten, so the set stays small
        if let Some((_, n)) = newest {
            let notice = describe(&n, icon_px);
            crate::log(format!("notification from {}", notice.app));
            if let Ok(mut latest) = slot.lock() {
                *latest = Some(notice);
            }
            unsafe {
                let _ = PostMessageW(Some(HWND(window as _)), message, WPARAM(0), LPARAM(0));
            }
        }
    }
}

/// Who sent it, what it says, and the sender's icon.
fn describe(n: &UserNotification, icon_px: u32) -> Notice {
    let display = n.AppInfo().and_then(|app| app.DisplayInfo());
    let app = display.as_ref().ok().and_then(|d| d.DisplayName().ok()).map(|n| n.to_string()).filter(|n| !n.is_empty());
    let icon = display.as_ref().ok().and_then(|d| {
        // A bigger asset than shown: its margin is cut away before it is scaled down.
        let side = 2.0 * icon_px as f32;
        let logo = d.GetLogo(Size { Width: side, Height: side }).ok()?;
        crate::media::decode_square(&logo.cast().ok()?, icon_px, true).ok()
    });
    let texts = texts(n).unwrap_or_default();
    let mut texts = texts.into_iter().filter(|t| !t.trim().is_empty());
    Notice {
        app: app.unwrap_or_else(|| tr("Bildirim", "Notification").to_string()),
        title: texts.next().unwrap_or_default(),
        body: texts.collect::<Vec<_>>().join(" "),
        icon,
    }
}

/// The texts of its toast, in order: title first.
fn texts(n: &UserNotification) -> Result<Vec<String>> {
    let visual = n.Notification()?.Visual()?;
    let binding = visual.GetBinding(&KnownNotificationBindings::ToastGeneric()?).or_else(|_| visual.Bindings()?.GetAt(0))?;
    Ok(binding.GetTextElements()?.into_iter().filter_map(|t| t.Text().ok()).map(|t| t.to_string()).collect())
}

/// Now, as WinRT counts time: 100 ns ticks since 1601.
fn now_ticks() -> i64 {
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    unix.as_nanos() as i64 / 100 + 116_444_736_000_000_000
}
