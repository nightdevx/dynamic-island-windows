//! Razer Barracuda X (2022) battery, through its 2.4 GHz dongle (USB 1532:0550). The protocol
//! is the one the razer-battery project uses (barracuda.py), which credits
//! github.com/mehdibouchami/razer-barracuda-x-battery (MIT) and github.com/leag/razer-barracuda.
//!
//! The island mostly listens. Windows gives every open handle a copy of each input report, so
//! the replies the BarracudaBattery app gets every minute reach the island too, and plugging
//! or pulling the cable sends a report on its own. Only when no level has come for `POLL`
//! does the island ask the headset itself. The thread sleeps in a read in between, and while
//! the dongle is missing it waits for the window to report a device change.

use std::{
    sync::{
        mpsc::{self, RecvTimeoutError},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use windows::{
    core::*,
    Win32::{
        Devices::{DeviceAndDriverInstallation::*, HumanInterfaceDevice::*},
        Foundation::*,
        Storage::FileSystem::*,
        System::{
            Threading::{CreateEventW, WaitForSingleObject},
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        },
        UI::WindowsAndMessaging::PostMessageW,
    },
};

const DONGLE: &str = "vid_1532&pid_0550";
const VENDOR_PAGE: u16 = 0xFF00;
const REPORT_ID: u8 = 0x02;
const FRAME: usize = 64;
// Customer-data ids (family 08).
const BATTERY: u8 = 0x21; // percent
const CABLE: u8 = 0x2A; // 00 unplugged, 01 plugged
/// Ask the headset when nobody has said the level for this long.
const POLL: Duration = Duration::from_secs(5 * 60);
/// Plugging the cable bounces for about a second; changes are published once they settle.
const SETTLE: Duration = Duration::from_millis(1500);

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
        let (shared, window) = (current.clone(), hwnd.0 as isize);
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
                let found = Dongle::open();
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

/// What is worth a peek when the battery goes from `old` to `new`: the cable plugged in, or
/// the level falling under 20% or 10% (once each on the way down; a headset that comes on
/// low says so too). The text, and whether it is about charging.
pub fn news(old: Option<Battery>, new: Battery) -> Option<(String, bool)> {
    if new.charging && old.is_some_and(|o| !o.charging) {
        return Some((format!("Kulaklık şarj oluyor · %{}", new.percent), true));
    }
    let crossed = |limit: u32| {
        !new.charging && (new.percent as u32) < limit && old.is_none_or(|o| o.charging || o.percent as u32 >= limit)
    };
    [10, crate::pages::LOW_BATTERY]
        .into_iter()
        .any(crossed)
        .then(|| (format!("Kulaklık pili azaldı · %{}", new.percent), false))
}

/// What the headset has said, from anyone's conversation or on its own.
#[derive(Default)]
struct Heard {
    percent: Option<u8>,
    charging: bool,
    level_at: Option<Instant>, // when a level last came
    unsettled: Option<Instant>, // first change not yet published
}

impl Heard {
    /// A frame about the battery or the cable: a reply (op 01) or a report of its own (op 02),
    /// both [cmd, op, len, value...] after the 10-byte header.
    fn take(&mut self, frame: &[u8]) {
        if frame.len() < 14 || frame[2] != 0x08 || !matches!(frame[11], 0x01 | 0x02) || frame[12] < 1 {
            return;
        }
        let value = frame[13];
        let changed = match frame[10] {
            BATTERY if value <= 100 => {
                self.level_at = Some(Instant::now());
                self.percent.replace(value) != Some(value)
            }
            CABLE => std::mem::replace(&mut self.charging, value == 1) != (value == 1),
            _ => false,
        };
        if changed {
            self.unsettled.get_or_insert_with(Instant::now);
        }
    }

    fn battery(&self) -> Option<Battery> {
        self.percent.map(|percent| Battery { percent, charging: self.charging })
    }
}

struct Dongle {
    path: Vec<u16>, // NUL-terminated
    reader: HANDLE, // overlapped: every input report, ours and other apps'
    event: HANDLE,
    input_len: usize,
    output_len: usize,
    seq: u8,
}

impl Drop for Dongle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.reader);
            let _ = CloseHandle(self.event);
        }
    }
}

fn open_path(path: &[u16], flags: FILE_FLAGS_AND_ATTRIBUTES) -> Option<HANDLE> {
    unsafe {
        CreateFileW(
            PCWSTR(path.as_ptr()),
            (GENERIC_READ | GENERIC_WRITE).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            flags,
            None,
        )
    }
    .ok()
}

impl Dongle {
    /// The dongle's vendor collection, the one that carries the headset protocol.
    fn open() -> Option<Self> {
        let guid = unsafe { HidD_GetHidGuid() };
        let mut len = 0;
        let flags = CM_GET_DEVICE_INTERFACE_LIST_PRESENT;
        unsafe { CM_Get_Device_Interface_List_SizeW(&mut len, &guid, None, flags) };
        let mut list = vec![0u16; len as usize];
        if unsafe { CM_Get_Device_Interface_ListW(&guid, None, &mut list, flags) } != CR_SUCCESS {
            return None;
        }
        for path in list.split(|&c| c == 0).filter(|p| !p.is_empty()) {
            if !String::from_utf16_lossy(path).to_lowercase().contains(DONGLE) {
                continue;
            }
            let path: Vec<u16> = path.iter().copied().chain([0]).collect();
            let Some(reader) = open_path(&path, FILE_FLAG_OVERLAPPED) else { continue };
            let mut caps = HIDP_CAPS::default();
            unsafe {
                let mut data = PHIDP_PREPARSED_DATA::default();
                if HidD_GetPreparsedData(reader, &mut data) {
                    let _ = HidP_GetCaps(data, &mut caps);
                    let _ = HidD_FreePreparsedData(data);
                }
            }
            let event = unsafe { CreateEventW(None, true, false, None) };
            match event {
                Ok(event) if caps.UsagePage == VENDOR_PAGE => {
                    return Some(Self {
                        path,
                        reader,
                        event,
                        input_len: (caps.InputReportByteLength as usize).max(FRAME),
                        output_len: (caps.OutputReportByteLength as usize).max(FRAME),
                        seq: 0,
                    });
                }
                Ok(event) => unsafe {
                    let _ = CloseHandle(event);
                    let _ = CloseHandle(reader);
                },
                Err(_) => unsafe {
                    let _ = CloseHandle(reader);
                },
            }
        }
        None
    }

    /// Reads until the dongle fails (pulled out). Publishes every settled change.
    fn listen(&mut self, publish: &dyn Fn(Option<Battery>)) {
        let mut heard = Heard::default();
        let mut next_ask = Instant::now(); // at once, so the level shows from the start
        loop {
            let now = Instant::now();
            let due = heard.level_at.map_or(next_ask, |at| (at + POLL).max(next_ask));
            if now >= due {
                if !self.ask(&mut heard) && heard.percent.take().is_some() {
                    // Off, out of range or in Bluetooth mode.
                    heard.unsettled.get_or_insert(now);
                }
                next_ask = now + POLL;
            } else {
                let wake = heard.unsettled.map_or(due, |at| (at + SETTLE).min(due));
                match self.next_report(wake - now) {
                    Ok(Some(report)) => frames(&report).iter().for_each(|f| heard.take(f)),
                    Ok(None) => {}
                    Err(e) => {
                        crate::log(format!("headset: {e}"));
                        return;
                    }
                }
            }
            if heard.unsettled.is_some_and(|at| at.elapsed() >= SETTLE) {
                heard.unsettled = None;
                publish(heard.battery());
            }
        }
    }

    /// The next input report, or None when `timeout` passes first. Err when the dongle is gone.
    fn next_report(&self, timeout: Duration) -> Result<Option<Vec<u8>>> {
        let mut report = vec![0u8; self.input_len];
        let mut overlapped = OVERLAPPED { hEvent: self.event, ..Default::default() };
        let mut read = 0;
        unsafe {
            if let Err(e) = ReadFile(self.reader, Some(&mut report), None, Some(&mut overlapped)) {
                if e.code() != ERROR_IO_PENDING.to_hresult() {
                    return Err(e);
                }
                if WaitForSingleObject(self.event, timeout.as_millis().min(u32::MAX as u128) as u32) == WAIT_TIMEOUT {
                    let _ = CancelIoEx(self.reader, Some(&overlapped));
                    // Waits for the cancel; a report that arrived just before it still counts.
                    if GetOverlappedResult(self.reader, &overlapped, &mut read, true).is_err() || read == 0 {
                        return Ok(None);
                    }
                    report.truncate(read as usize);
                    return Ok(Some(report));
                }
            }
            GetOverlappedResult(self.reader, &overlapped, &mut read, false)?;
        }
        report.truncate(read as usize);
        Ok(Some(report))
    }

    /// One conversation, as BarracudaBattery has it: route commands to the headset, ask the
    /// level and the cable, route them back. The answers arrive as input reports and are
    /// taken like any other. False when the headset does not answer.
    fn ask(&mut self, heard: &mut Heard) -> bool {
        let Some(writer) = open_path(&self.path, FILE_FLAGS_AND_ATTRIBUTES(0)) else { return false };
        let remote = |on: u8| [0x02, 0xE1, on];
        let routed = self.send(writer, 0x0E, &remote(1), |f, seq| f[10] == 0x0E && f[11] == 0x80 | seq, heard);
        let answered = routed.is_some_and(|f| f.get(12) == Some(&0)) && {
            let get = |cmd: u8| move |f: &[u8], _: u8| f[2] == 0x08 && f[10] == cmd && f[11] == 0x01;
            let level = self.send(writer, 0x08, &[0x03, BATTERY, 0x00, 0x00], get(BATTERY), heard);
            self.send(writer, 0x08, &[0x03, CABLE, 0x00, 0x00], get(CABLE), heard);
            self.send(writer, 0x0E, &remote(0), |f, seq| f[10] == 0x0E && f[11] == 0x80 | seq, heard);
            level.is_some()
        };
        unsafe {
            let _ = CloseHandle(writer);
        }
        answered
    }

    /// Writes `50 41 group seq body...` and returns the first reply frame that `matches`.
    fn send(&mut self, writer: HANDLE, group: u8, body: &[u8], matches: impl Fn(&[u8], u8) -> bool, heard: &mut Heard) -> Option<Vec<u8>> {
        self.seq = self.seq % 127 + 1;
        let seq = self.seq;
        let mut frame = vec![0u8; self.output_len];
        let payload: Vec<u8> = [0x50, 0x41, group, seq].into_iter().chain(body.iter().copied()).collect();
        frame[..3].copy_from_slice(&[REPORT_ID, 0x80, payload.len() as u8]);
        frame[5..5 + payload.len()].copy_from_slice(&payload);
        let crc = crc16_xmodem(&frame[..FRAME]); // over the frame with the CRC bytes still zero
        frame[3..5].copy_from_slice(&crc.to_le_bytes());

        // The dongle answers 'O' when it took the command and 'F' while it is busy.
        let mut accepted = false;
        for attempt in 0..5 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(50));
            }
            let mut status = vec![0u8; self.input_len];
            status[0] = REPORT_ID;
            let asked = unsafe {
                WriteFile(writer, Some(&frame), None, None).is_ok()
                    && HidD_GetInputReport(writer, status.as_mut_ptr().cast(), status.len() as u32)
            };
            match (asked, status[1]) {
                (true, b'O') => {
                    accepted = true;
                    break;
                }
                (true, b'F') => continue,
                _ => return None,
            }
        }
        if !accepted {
            return None;
        }
        let deadline = Instant::now() + Duration::from_millis(1500);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let report = self.next_report(left).ok()??;
            for f in frames(&report) {
                heard.take(f);
                if f.len() >= 13 && matches(f, seq) {
                    return Some(f.to_vec());
                }
            }
        }
        None
    }
}

/// The frames in an input report `[02, len, frames...]`: each a 10-byte header (`50 49`,
/// payload length LE at 8..10) and its payload. One report can hold several.
fn frames(report: &[u8]) -> Vec<&[u8]> {
    let mut found = Vec::new();
    if report.len() < 2 || report[0] != REPORT_ID || report[1] as usize > report.len() - 2 {
        return found;
    }
    let (end, mut at) = (2 + report[1] as usize, 2);
    while at + 10 <= end && report[at] == 0x50 && report[at + 1] == 0x49 {
        let frame_end = at + 10 + (report[at + 8] as usize | (report[at + 9] as usize) << 8);
        if frame_end > end {
            break;
        }
        found.push(&report[at..frame_end]);
        at = frame_end;
    }
    found
}

fn crc16_xmodem(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &byte in data {
        crc ^= (byte as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        text.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

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

    #[test]
    fn crc_is_xmodem() {
        assert_eq!(crc16_xmodem(b"123456789"), 0x31C3); // the standard check value
    }

    #[test]
    fn reads_captured_reports() {
        // Captured by barracuda.py on this dongle: an event, then the battery reply (0x26 = 38%).
        let event = hex("50 49 01 c0 59 31 29 01 03 00 0e 84 00");
        let reply = hex("50 49 08 fa b8 ce 17 01 04 00 21 01 01 26");
        let report: Vec<u8> = [vec![REPORT_ID, (event.len() + reply.len()) as u8], event.clone(), reply.clone()].concat();
        assert_eq!(frames(&report), vec![&event[..], &reply[..]]);
        let mut heard = Heard::default();
        frames(&report).iter().for_each(|f| heard.take(f));
        assert_eq!(heard.battery(), Some(Battery { percent: 38, charging: false }));
        // Plugging the cable: battery 53% and cable 01 in one report, sent unasked.
        let plugged = hex("02 1c 50 49 08 fc 04 2d 67 00 04 00 21 02 01 35 50 49 08 fd 04 2d 67 00 04 00 2a 02 01 01");
        frames(&plugged).iter().for_each(|f| heard.take(f));
        assert_eq!(heard.battery(), Some(Battery { percent: 53, charging: true }));
        // Garbage and cut-off reports yield nothing.
        assert!(frames(&[REPORT_ID, 40, 0x50, 0x49]).is_empty());
        assert!(frames(&[0x01, 0x00]).is_empty());
    }
}
