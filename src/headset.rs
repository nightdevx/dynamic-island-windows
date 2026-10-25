//! A headset's battery, as the closed pill, the tab bar and the peeks show it. Only the
//! Barracuda build (`--features headset`) watches a dongle for it (`barracuda.rs`); in the
//! normal build the link stays quiet and nothing about a headset shows.

use std::sync::{mpsc, Arc, Mutex};

use windows::Win32::Foundation::HWND;

use crate::text::tr;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

pub struct HeadsetLink {
    rescan: mpsc::Sender<()>,
    current: Arc<Mutex<Option<Battery>>>,
}

impl HeadsetLink {
    /// Starts the headset thread; it posts `message` to `hwnd` when the battery changes or
    /// the headset goes away.
    pub fn start(hwnd: HWND, message: u32) -> Self {
        let (rescan, devices_changed) = mpsc::channel();
        let current = Arc::new(Mutex::new(None));
        #[cfg(feature = "headset")]
        watch(devices_changed, current.clone(), hwnd, message);
        #[cfg(not(feature = "headset"))]
        let _ = (devices_changed, hwnd, message); // no dongle to watch in this build
        Self { rescan, current }
    }

    /// A device came or went: look for the dongle again if it was missing.
    pub fn rescan(&self) {
        let _ = self.rescan.send(());
    }

    pub fn current(&self) -> Option<Battery> {
        *self.current.lock().ok()?
    }
}

/// The headset thread: finds the dongle and listens to it until it goes; while it is missing,
/// waits for the window to report a device change.
#[cfg(feature = "headset")]
fn watch(devices_changed: mpsc::Receiver<()>, shared: Arc<Mutex<Option<Battery>>>, hwnd: HWND, message: u32) {
    use std::{sync::mpsc::RecvTimeoutError, time::Duration};
    use windows::Win32::{
        Foundation::{LPARAM, WPARAM},
        UI::WindowsAndMessaging::PostMessageW,
    };
    let window = hwnd.0 as isize;
    std::thread::spawn(move || {
        let publish = |battery: Option<Battery>| {
            let Ok(mut slot) = shared.lock() else { return };
            if *slot != battery {
                crate::log(format!("headset: {battery:?}"));
                *slot = battery;
                unsafe {
                    let _ = PostMessageW(Some(HWND(window as _)), message, WPARAM(0), LPARAM(0));
                }
            }
        };
        loop {
            while devices_changed.try_recv().is_ok() {}
            let found = crate::barracuda::Dongle::open();
            let present = found.is_some();
            if let Some(mut dongle) = found {
                crate::log("headset: dongle found");
                dongle.listen(&publish);
            }
            publish(None);
            // Missing: wait for a device change. Present but failing: try again in a minute.
            let waited = if present {
                devices_changed.recv_timeout(Duration::from_secs(60))
            } else {
                devices_changed.recv().map_err(|_| RecvTimeoutError::Disconnected)
            };
            if waited == Err(RecvTimeoutError::Disconnected) {
                return;
            }
        }
    });
}

/// What is worth a peek when the battery goes from `old` to `new`: the cable plugged in, or
/// the level falling under 20% or 10% (once each on the way down; a headset that comes on
/// low says so too). The text, and whether it is about charging.
pub fn news(old: Option<Battery>, new: Battery) -> Option<(String, bool)> {
    if new.charging && old.is_some_and(|o| !o.charging) {
        let n = new.percent;
        return Some((tr(format!("Kulaklık şarj oluyor · %{n}"), format!("Headset charging · {n}%")), true));
    }
    let crossed = |limit: u32| {
        !new.charging && (new.percent as u32) < limit && old.is_none_or(|o| o.charging || o.percent as u32 >= limit)
    };
    [10, crate::pages::LOW_BATTERY]
        .into_iter()
        .any(crossed)
        .then(|| (tr(format!("Kulaklık pili azaldı · %{}", new.percent), format!("Headset battery low · {}%", new.percent)), false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peeks_on_plugging_in_and_running_low() {
        let b = |percent, charging| Battery { percent, charging };
        assert_eq!(news(Some(b(53, false)), b(53, true)), Some(("Kulaklık şarj oluyor · %53".into(), true)));
        assert_eq!(news(None, b(53, true)), None); // already on the cable at startup
        assert_eq!(news(Some(b(21, false)), b(19, false)), Some(("Kulaklık pili azaldı · %19".into(), false)));
        assert_eq!(news(Some(b(19, false)), b(18, false)), None); // said once
        assert!(news(Some(b(11, false)), b(9, false)).is_some()); // and again under 10
        assert!(news(None, b(15, false)).is_some()); // comes on low
        assert!(news(Some(b(19, true)), b(19, false)).is_some()); // pulled off the cable while low
        assert_eq!(news(Some(b(60, false)), b(59, false)), None);
    }
}
