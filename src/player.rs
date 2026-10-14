//! Brings what plays to the front: the player's window, and in a browser the tab that plays,
//! selected through UI Automation as an item of a list is (no keys are sent). On a thread of
//! its own: asking a browser for its tabs takes a moment.

use windows::{
    core::*,
    Win32::{
        Foundation::{HWND, LPARAM, TRUE},
        System::{
            Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED},
            Variant::VARIANT,
        },
        UI::{Accessibility::*, Shell::*, WindowsAndMessaging::*},
    },
};

/// The words a browser adds to the name of a tab that plays sound, in the languages the
/// island speaks.
const PLAYING: [&str; 3] = ["audio playing", "playing audio", "ses çalıyor"];

/// Brings the player of media session `app_id` to the front, on the tab playing `title`.
pub fn show(app_id: String, title: String) {
    std::thread::spawn(move || {
        if let Err(e) = bring(&app_id, &title) {
            crate::log(format!("show player: {e}"));
        }
    });
}

fn bring(app_id: &str, title: &str) -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let windows = player_windows(app_id);
    let Some(&first) = windows.first() else {
        // A store app runs in a frame of the system's: Windows brings it up by its id.
        if app_id.contains('!') {
            let manager: IApplicationActivationManager = unsafe { CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER)? };
            unsafe { manager.ActivateApplication(&HSTRING::from(app_id), None, AO_NONE)? };
        }
        return Ok(());
    };
    let automation: IUIAutomation = unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)? };
    let mut best: Option<(u32, HWND, IUIAutomationElement)> = None;
    for &hwnd in &windows {
        for (name, tab) in tabs(&automation, hwnd).unwrap_or_default() {
            let score = tab_score(&name, title);
            if score > best.as_ref().map_or(0, |b| b.0) {
                best = Some((score, hwnd, tab));
            }
        }
    }
    let hwnd = match best {
        Some((_, hwnd, tab)) => {
            let selected = unsafe { tab.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId).and_then(|p| p.Select()) };
            if let Err(e) = selected {
                crate::log(format!("show player: tab not selected: {e}"));
            }
            hwnd
        }
        None => first,
    };
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
    Ok(())
}

/// How well a tab's name fits the track: 2 when it names the track, 1 when the browser says
/// it plays sound, 0 otherwise.
fn tab_score(name: &str, title: &str) -> u32 {
    let name = name.to_lowercase();
    let title = title.trim().to_lowercase();
    if !title.is_empty() && name.contains(&title) {
        2
    } else if PLAYING.iter().any(|p| name.contains(p)) {
        1
    } else {
        0
    }
}

/// The tabs in a window, with their names.
fn tabs(automation: &IUIAutomation, hwnd: HWND) -> Result<Vec<(String, IUIAutomationElement)>> {
    unsafe {
        let window = automation.ElementFromHandle(hwnd)?;
        let condition = automation.CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(UIA_TabItemControlTypeId.0))?;
        let found = window.FindAll(TreeScope_Descendants, &condition)?;
        let mut tabs = Vec::new();
        for i in 0..found.Length()? {
            let tab = found.GetElement(i)?;
            tabs.push((tab.CurrentName()?.to_string(), tab));
        }
        Ok(tabs)
    }
}

/// The player's windows that people use, front first.
fn player_windows(app_id: &str) -> Vec<HWND> {
    unsafe extern "system" fn collect(hwnd: HWND, list: LPARAM) -> windows::core::BOOL {
        unsafe { (*(list.0 as *mut Vec<HWND>)).push(hwnd) };
        TRUE
    }
    let mut all: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut all as *mut _ as isize));
    }
    let app = app_id.to_lowercase();
    let processes = crate::claude::processes();
    all.into_iter()
        .filter(|&hwnd| unsafe {
            let tool = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0;
            let owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.is_invalid());
            let mut pid = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            IsWindowVisible(hwnd).as_bool()
                && !tool
                && !owned
                && GetWindowTextLengthW(hwnd) > 0
                && processes.get(&pid).is_some_and(|(_, exe)| crate::audio::plays_for(&app, exe))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tab_naming_the_track_wins() {
        let tab = "Everybody Dies In Their Nightmares | YouTube Music - Ses çalıyor - Bellek kullanımı - 363 MB";
        assert_eq!(tab_score(tab, "Everybody Dies In Their Nightmares"), 2);
        assert_eq!(tab_score(tab, "Another Song"), 1); // still the one making sound
        assert_eq!(tab_score("Inbox - Gmail", "Another Song"), 0);
        assert_eq!(tab_score("Lofi beats - YouTube - Audio playing", ""), 1);
    }
}
