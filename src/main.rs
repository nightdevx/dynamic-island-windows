#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod bluetooth;
mod blur;
mod claude;
mod fullscreen;
mod island;
mod json;
mod liked;
mod media;
mod notices;
mod pages;
mod player;
mod preferences;
mod render;
mod settings;
mod shelf;
mod text;
mod timer;
mod usage;
mod window;

use std::{fs::OpenOptions, io::Write, path::PathBuf};

use windows::{
    core::*,
    System::DispatcherQueueController,
    Win32::{
        Foundation::*,
        System::{SystemInformation::GetLocalTime, Threading::*, WinRT::*},
        UI::HiDpi::*,
    },
};

fn main() {
    std::panic::set_hook(Box::new(|info| log(format!("panic: {info}"))));
    // Short-lived modes started by Claude Code and the installer; no window, no single-instance lock.
    match std::env::args().nth(1).as_deref() {
        Some("--hook") => return claude::hook(),
        Some("--statusline") => {
            text::set(settings::load().language);
            return claude::statusline();
        }
        Some("--setup-claude") => return claude::setup(true),
        Some("--remove-claude") => return claude::setup(false),
        // Explorer's "Send to → Dynamic Island": hands the files to the running island.
        Some("--shelf") => return window::send_to_shelf(std::env::args().skip(2).collect()),
        _ => {}
    }
    if let Err(e) = run() {
        log(format!("fatal: {e}"));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    // One island per session; a second launch opens its settings and exits. The handle lives as long as the process.
    let _instance = unsafe { CreateMutexW(None, false, w!("Local\\com.nightdijital.dynamicisland"))? };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        window::show_running_settings();
        return Ok(());
    }
    if std::env::var_os("ISLAND_PANIC_TEST").is_some() {
        panic!("panic test");
    }
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)?;
        RoInitialize(RO_INIT_SINGLETHREADED)?;
    }
    eco_qos();
    log(format!("start {}", env!("CARGO_PKG_VERSION")));
    // Windows.UI.Composition needs a DispatcherQueue on the UI thread; it runs on our message loop.
    let _queue = dispatcher_queue()?;
    window::run()
}

fn dispatcher_queue() -> Result<DispatcherQueueController> {
    let options = DispatcherQueueOptions {
        dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
        threadType: DQTYPE_THREAD_CURRENT,
        apartmentType: DQTAT_COM_NONE,
    };
    unsafe { CreateDispatcherQueueController(options) }
}

/// EcoQoS: efficient cores and clocks for our few milliseconds of work. Animations run in
/// DWM, so they are not slowed by this.
fn eco_qos() {
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    };
    let result = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            &state as *const _ as *const _,
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };
    if let Err(e) = result {
        log(format!("EcoQoS not set: {e}"));
    }
}

pub fn log_path() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("DynamicIsland").join("island.log"))
}

/// Appends a line to `%LOCALAPPDATA%\DynamicIsland\island.log`, keeping one 256 KB backup.
pub fn log(message: impl std::fmt::Display) {
    #[cfg(debug_assertions)]
    eprintln!("{message}");
    let Some(path) = log_path() else { return };
    let dir = path.parent().unwrap();
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 256 * 1024) {
        let _ = std::fs::rename(&path, dir.join("island.old.log"));
    }
    let _ = std::fs::create_dir_all(&dir);
    let t = unsafe { GetLocalTime() };
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(
            file,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} {message}",
            t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
        );
    }
}
