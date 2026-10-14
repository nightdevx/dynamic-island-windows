//! When the island gets out of the way: games and other fullscreen apps. A topmost window
//! over a fullscreen game also costs the game its independent flip, so we hide, not fade.

use windows::{
    core::PWSTR,
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::Threading::*,
        UI::{Shell::*, WindowsAndMessaging::*},
    },
};

use crate::settings::Settings;

/// Shell windows that cover the monitor without being fullscreen apps.
const SHELL_CLASSES: [&str; 4] = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];

pub fn is_fullscreen(window: RECT, monitor: RECT, class: &str, maximized_with_caption: bool) -> bool {
    let covers = window.left <= monitor.left
        && window.top <= monitor.top
        && window.right >= monitor.right
        && window.bottom >= monitor.bottom;
    // A maximized window covers the monitor too when the taskbar auto-hides.
    covers && !maximized_with_caption && !SHELL_CLASSES.contains(&class)
}

/// Whether the island, shown on `island`'s monitor, should hide right now.
pub fn should_hide(island: HWND, settings: &Settings) -> bool {
    // Exclusive-mode D3D and presentation mode; the API does not say which monitor.
    if let Ok(state) = unsafe { SHQueryUserNotificationState() } {
        if state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE {
            return true;
        }
    }
    let foreground = unsafe { GetForegroundWindow() };
    if foreground.is_invalid() || foreground == island {
        return false;
    }
    if !settings.hide_exe.is_empty() || !settings.show_exe.is_empty() {
        let exe = exe_name(foreground);
        if settings.hide_exe.contains(&exe) {
            return true;
        }
        if settings.show_exe.contains(&exe) {
            return false;
        }
    }
    unsafe {
        let monitor = MonitorFromWindow(island, MONITOR_DEFAULTTOPRIMARY);
        if MonitorFromWindow(foreground, MONITOR_DEFAULTTONULL) != monitor {
            return false;
        }
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let mut rect = RECT::default();
        if !GetMonitorInfoW(monitor, &mut info).as_bool() || GetWindowRect(foreground, &mut rect).is_err() {
            return false;
        }
        let caption = WS_CAPTION.0;
        let maximized_with_caption =
            IsZoomed(foreground).as_bool() && GetWindowLongW(foreground, GWL_STYLE) as u32 & caption == caption;
        is_fullscreen(rect, info.rcMonitor, &class_name(foreground), maximized_with_caption)
    }
}

pub fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut buffer) } as usize;
    String::from_utf16_lossy(&buffer[..len])
}

/// Lowercase file name of the process owning `hwnd`, or "" when it cannot be read.
pub fn exe_name(hwnd: HWND) -> String {
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buffer = [0u16; 1024];
        let mut len = buffer.len() as u32;
        let read = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        if read.is_err() {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buffer[..len as usize]);
        path.rsplit('\\').next().unwrap_or_default().to_lowercase()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cargo test --release -- --ignored --nocapture`: cost of one check on this machine.
    #[test]
    #[ignore]
    fn cost_of_a_check() {
        let settings = Settings::default();
        let start = std::time::Instant::now();
        for _ in 0..1000 {
            should_hide(HWND::default(), &settings);
        }
        let quns = std::time::Instant::now();
        for _ in 0..1000 {
            let _ = unsafe { SHQueryUserNotificationState() };
        }
        println!("should_hide {:?}/call, SHQueryUserNotificationState {:?}/call", start.elapsed() / 1000, quns.elapsed() / 1000);
    }

    const MONITOR: RECT = RECT { left: 0, top: 0, right: 2560, bottom: 1600 };

    #[test]
    fn fullscreen_rules() {
        // Borderless game or F11 browser: exactly the monitor.
        assert!(is_fullscreen(MONITOR, MONITOR, "UnityWndClass", false));
        // Maximized window with a visible taskbar: stops above the taskbar.
        let maximized = RECT { left: -8, top: -8, right: 2568, bottom: 1548 };
        assert!(!is_fullscreen(maximized, MONITOR, "Chrome_WidgetWin_1", true));
        // Maximized with an auto-hidden taskbar: covers the monitor but has a caption.
        let covering = RECT { left: -8, top: -8, right: 2568, bottom: 1608 };
        assert!(!is_fullscreen(covering, MONITOR, "Chrome_WidgetWin_1", true));
        // The desktop covers the monitor too.
        assert!(!is_fullscreen(MONITOR, MONITOR, "WorkerW", false));
        // A window on part of the screen.
        let half = RECT { left: 0, top: 0, right: 1280, bottom: 1600 };
        assert!(!is_fullscreen(half, MONITOR, "Notepad", false));
    }
}
