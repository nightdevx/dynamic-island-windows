//! Microphone and camera in use, from Windows' own privacy records (the ones behind the
//! microphone icon in the taskbar): each app that used a device has a key under
//! `CapabilityAccessManager\ConsentStore\<device>` whose `LastUsedTimeStop` is 0 while it
//! still uses it. The keys are watched with a registry notification; nothing polls.

use windows::{
    core::*,
    Win32::{
        Foundation::*,
        System::{Registry::*, Threading::CreateEventW},
    },
};

const STORE: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\CapabilityAccessManager\\ConsentStore";
const DEVICES: [&str; 2] = ["microphone", "webcam"];

/// The app using each device now, by a short name; None when the device is free.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InUse {
    pub microphone: Option<String>,
    pub camera: Option<String>,
}

pub fn in_use() -> InUse {
    let processes = crate::claude::processes();
    let running = |exe: &str| processes.values().any(|(_, name)| *name == exe);
    let [microphone, camera] = DEVICES.map(|device| {
        let mut users = Vec::new();
        let key = open(&format!("{STORE}\\{device}"))?;
        for name in subkeys(key) {
            if name == "NonPackaged" {
                // Desktop apps, named by their path with '#' for '\'. One that crashed while
                // using the device never wrote a stop time: it only counts while it runs.
                if let Some(desktop) = open(&format!("{STORE}\\{device}\\NonPackaged")) {
                    for path in subkeys(desktop) {
                        let exe = path.rsplit('#').next().unwrap_or_default();
                        let lower = exe.to_lowercase();
                        if using(desktop, &path) && running(&lower) {
                            let name = if lower.ends_with(".exe") { &exe[..exe.len() - 4] } else { exe };
                            users.push(name.to_string());
                        }
                    }
                    close(desktop);
                }
            } else if using(key, &name) {
                users.push(package_name(&name));
            }
        }
        close(key);
        users.into_iter().next()
    });
    InUse { microphone, camera }
}

/// "Microsoft.WindowsCamera_8wekyb3d8bbwe" → "WindowsCamera".
fn package_name(family: &str) -> String {
    let name = family.split('_').next().unwrap_or(family);
    name.rsplit('.').next().unwrap_or(name).to_string()
}

fn open(path: &str) -> Option<HKEY> {
    let mut key = HKEY::default();
    let opened = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), None, KEY_READ, &mut key) };
    opened.is_ok().then_some(key)
}

fn close(key: HKEY) {
    unsafe {
        let _ = RegCloseKey(key);
    }
}

fn subkeys(key: HKEY) -> Vec<String> {
    let mut names = Vec::new();
    for index in 0.. {
        let mut buffer = [0u16; 512];
        let mut len = buffer.len() as u32;
        let found = unsafe { RegEnumKeyExW(key, index, Some(PWSTR(buffer.as_mut_ptr())), &mut len, None, None, None, None) };
        if found.is_err() {
            break;
        }
        names.push(String::from_utf16_lossy(&buffer[..len as usize]));
    }
    names
}

/// Started using the device and has not stopped.
fn using(key: HKEY, subkey: &str) -> bool {
    let time = |value: PCWSTR| {
        let mut data = 0u64;
        let mut size = 8u32;
        let read = unsafe {
            RegGetValueW(key, &HSTRING::from(subkey), value, RRF_RT_REG_QWORD, None, Some(&mut data as *mut u64 as *mut _), Some(&mut size))
        };
        read.is_ok().then_some(data)
    };
    time(w!("LastUsedTimeStart")).is_some_and(|t| t != 0) && time(w!("LastUsedTimeStop")) == Some(0)
}

/// Signals an event when an app starts or stops using a device. Call `arm` after each
/// signal: a registry notification fires once.
pub struct Watch {
    keys: Vec<HKEY>,
    event: HANDLE,
}

impl Watch {
    pub fn new() -> Option<Self> {
        let keys: Vec<HKEY> = DEVICES.iter().filter_map(|device| open(&format!("{STORE}\\{device}"))).collect();
        let event = unsafe { CreateEventW(None, false, false, None) }.ok()?;
        let watch = Self { keys, event };
        watch.arm();
        Some(watch)
    }

    pub fn event(&self) -> HANDLE {
        self.event
    }

    pub fn arm(&self) {
        let filter = REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET;
        for key in &self.keys {
            if let Err(e) = unsafe { RegNotifyChangeKeyValue(*key, true, filter, Some(self.event), true) }.ok() {
                crate::log(format!("privacy watch: {e}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names() {
        assert_eq!(package_name("Microsoft.WindowsCamera_8wekyb3d8bbwe"), "WindowsCamera");
        assert_eq!(package_name("5319275A.WhatsAppDesktop_cv1g1gvanyjgm"), "WhatsAppDesktop");
    }
}
