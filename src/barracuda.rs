//! Razer Barracuda X (2022) battery, through its 2.4 GHz dongle; in the Barracuda build only
//! (`--features headset`). The dongle comes with one of two bridge chips (see `Bridge`); both
//! carry the same headset commands. The protocol is the one the razer-battery project uses
//! (barracuda.py), which credits github.com/mehdibouchami/razer-barracuda-x-battery (MIT),
//! github.com/KhromotozzDevOut/razer-barracuda-battery-tray (MIT) and github.com/leag/razer-barracuda.
//!
//! The island mostly listens. Windows gives every open handle a copy of each input report, so
//! the replies the BarracudaBattery app gets every minute reach the island too, and plugging
//! or pulling the cable sends a report on its own. Only when no level has come for `POLL`
//! does the island ask the headset itself.

use std::time::{Duration, Instant};

use windows::{
    core::*,
    Win32::{
        Devices::{DeviceAndDriverInstallation::*, HumanInterfaceDevice::*},
        Foundation::*,
        Storage::FileSystem::*,
        System::{
            Threading::{CreateEventW, CreateMutexW, ReleaseMutex, WaitForSingleObject},
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        },
    },
};

use crate::headset::Battery;

/// The dongles, by the part of their device path that names them.
const DONGLES: [(&str, Bridge); 2] = [("vid_1532&pid_0550", Bridge::YsTech), ("vid_1532&pid_0552", Bridge::Macronix)];
const VENDOR_PAGE: u16 = 0xFF00;
const FRAME: usize = 64;
// Customer-data ids (family 08).
const BATTERY: u8 = 0x21; // percent
const CABLE: u8 = 0x2A; // 00 unplugged, 01 plugged
/// Ask the headset when nobody has said the level for this long.
const POLL: Duration = Duration::from_secs(5 * 60);
/// Plugging the cable bounces for about a second; changes are published once they settle.
const SETTLE: Duration = Duration::from_millis(1500);

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

/// The chip in the dongle that bridges USB to the headset's link; each frames the headset's
/// bytes in its own reports.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Bridge {
    /// 1532:0550: `[02 80 len crc crc bytes…]` out, with a CRC-16/XMODEM over the whole report
    /// and an 'O' (taken) or 'F' (busy) to fetch after each; `[02 len bytes…]` back.
    YsTech,
    /// 1532:0552: `[01 80 len bytes…]` both ways, after an `[01 40]` hello; a frame back may
    /// be split across two reports.
    Macronix,
}

impl Bridge {
    fn report_id(self) -> u8 {
        match self {
            Bridge::YsTech => 0x02,
            Bridge::Macronix => 0x01,
        }
    }

    /// The headset's bytes in an input report; none in anything else, such as the media keys'.
    fn chunk(self, report: &[u8]) -> &[u8] {
        let start = match self {
            Bridge::YsTech => 2,
            Bridge::Macronix if report.get(1) == Some(&0x80) => 3,
            Bridge::Macronix => return &[],
        };
        if report.len() < start || report[0] != self.report_id() {
            return &[];
        }
        &report[start..(start + report[start - 1] as usize).min(report.len())]
    }
}

/// Frames out of the headset's bytes as they come: `50 49`, a 10-byte header with the
/// payload's length (LE) at 8..10, then the payload. A report can hold several frames, and a
/// frame can begin in one report and end in the next.
#[derive(Default)]
struct Stream {
    bytes: Vec<u8>,
}

impl Stream {
    fn feed(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.bytes.extend_from_slice(data);
        let mut frames = Vec::new();
        loop {
            let Some(start) = self.bytes.windows(2).position(|w| w == [0x50, 0x49]) else {
                // Keeps a header cut in half.
                let half = self.bytes.last() == Some(&0x50);
                self.bytes = if half { vec![0x50] } else { Vec::new() };
                return frames;
            };
            self.bytes.drain(..start);
            if self.bytes.len() < 10 {
                return frames;
            }
            let end = 10 + (self.bytes[8] as usize | (self.bytes[9] as usize) << 8);
            if self.bytes.len() < end {
                return frames;
            }
            frames.push(self.bytes.drain(..end).collect());
        }
    }
}

/// The lock BarracudaBattery holds while it talks to the dongle. The island takes it too, so
/// the two never talk at once; one left by a holder that died is handed over.
struct DongleLock(HANDLE);

impl DongleLock {
    fn take() -> Option<Self> {
        let mutex = unsafe { CreateMutexW(None, false, w!("Local\\BarracudaBattery.Dongle")) }.ok()?;
        match unsafe { WaitForSingleObject(mutex, 15_000) } {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Some(Self(mutex)),
            _ => {
                unsafe {
                    let _ = CloseHandle(mutex);
                }
                None
            }
        }
    }
}

impl Drop for DongleLock {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.0);
            let _ = CloseHandle(self.0);
        }
    }
}

pub struct Dongle {
    bridge: Bridge,
    path: Vec<u16>, // NUL-terminated
    reader: HANDLE, // overlapped: every input report, ours and other apps'
    event: HANDLE,
    input_len: usize,
    output_len: usize,
    stream: Stream,
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
    pub fn open() -> Option<Self> {
        let guid = unsafe { HidD_GetHidGuid() };
        let mut len = 0;
        let flags = CM_GET_DEVICE_INTERFACE_LIST_PRESENT;
        unsafe { CM_Get_Device_Interface_List_SizeW(&mut len, &guid, None, flags) };
        let mut list = vec![0u16; len as usize];
        if unsafe { CM_Get_Device_Interface_ListW(&guid, None, &mut list, flags) } != CR_SUCCESS {
            return None;
        }
        for path in list.split(|&c| c == 0).filter(|p| !p.is_empty()) {
            let name = String::from_utf16_lossy(path).to_lowercase();
            let Some(&(_, bridge)) = DONGLES.iter().find(|(id, _)| name.contains(id)) else { continue };
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
                    crate::log(format!("headset: dongle with the {bridge:?} bridge"));
                    return Some(Self {
                        bridge,
                        path,
                        reader,
                        event,
                        input_len: (caps.InputReportByteLength as usize).max(FRAME),
                        output_len: (caps.OutputReportByteLength as usize).max(FRAME),
                        stream: Stream::default(),
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
    pub fn listen(&mut self, publish: &dyn Fn(Option<Battery>)) {
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
                    Ok(Some(report)) => {
                        self.frames(&report).iter().for_each(|f| heard.take(f));
                    }
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

    /// The frames that `report` completes.
    fn frames(&mut self, report: &[u8]) -> Vec<Vec<u8>> {
        self.stream.feed(self.bridge.chunk(report))
    }

    /// One conversation, as BarracudaBattery has it: route commands to the headset, ask the
    /// level and the cable, route them back. The answers arrive as input reports and are
    /// taken like any other. False when the headset does not answer.
    fn ask(&mut self, heard: &mut Heard) -> bool {
        let Some(_lock) = DongleLock::take() else { return false };
        let Some(writer) = open_path(&self.path, FILE_FLAGS_AND_ATTRIBUTES(0)) else { return false };
        if self.bridge == Bridge::Macronix {
            self.hello(writer, heard);
        }
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

    /// The Macronix bridge's hello, as its Windows tool says it first ("app mode"). Drivers
    /// elsewhere work without it, so an unanswered hello is no failure.
    fn hello(&mut self, writer: HANDLE, heard: &mut Heard) {
        let mut hello = vec![0u8; self.output_len];
        hello[..2].copy_from_slice(&[0x01, 0x40]);
        if unsafe { WriteFile(writer, Some(&hello), None, None) }.is_err() {
            return;
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.next_report(left) {
                Ok(Some(report)) if report.starts_with(&[0x01, 0x40]) => return,
                Ok(Some(report)) => self.frames(&report).iter().for_each(|f| heard.take(f)),
                _ => return,
            }
        }
    }

    /// Hands `payload` to the dongle in its bridge's framing; false when it refuses it.
    fn deliver(&self, writer: HANDLE, payload: &[u8]) -> bool {
        let mut frame = vec![0u8; self.output_len];
        if self.bridge == Bridge::Macronix {
            frame[..3].copy_from_slice(&[0x01, 0x80, payload.len() as u8]);
            frame[3..3 + payload.len()].copy_from_slice(payload);
            return unsafe { WriteFile(writer, Some(&frame), None, None) }.is_ok();
        }
        frame[..3].copy_from_slice(&[0x02, 0x80, payload.len() as u8]);
        frame[5..5 + payload.len()].copy_from_slice(payload);
        let crc = crc16_xmodem(&frame[..FRAME]); // over the frame with the CRC bytes still zero
        frame[3..5].copy_from_slice(&crc.to_le_bytes());
        // The dongle answers 'O' when it took the command and 'F' while it is busy.
        for attempt in 0..5 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(50));
            }
            let mut status = vec![0u8; self.input_len];
            status[0] = 0x02;
            let asked = unsafe {
                WriteFile(writer, Some(&frame), None, None).is_ok()
                    && HidD_GetInputReport(writer, status.as_mut_ptr().cast(), status.len() as u32)
            };
            match (asked, status[1]) {
                (true, b'O') => return true,
                (true, b'F') => continue,
                _ => return false,
            }
        }
        false
    }

    /// Writes `50 41 group seq body...` and returns the first reply frame that `matches`.
    fn send(&mut self, writer: HANDLE, group: u8, body: &[u8], matches: impl Fn(&[u8], u8) -> bool, heard: &mut Heard) -> Option<Vec<u8>> {
        self.seq = self.seq % 127 + 1;
        let seq = self.seq;
        let payload: Vec<u8> = [0x50, 0x41, group, seq].into_iter().chain(body.iter().copied()).collect();
        if !self.deliver(writer, &payload) {
            return None;
        }
        let deadline = Instant::now() + Duration::from_millis(1500);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let report = self.next_report(left).ok()??;
            for f in self.frames(&report) {
                heard.take(&f);
                if f.len() >= 13 && matches(&f, seq) {
                    return Some(f);
                }
            }
        }
        None
    }
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
    fn crc_is_xmodem() {
        assert_eq!(crc16_xmodem(b"123456789"), 0x31C3); // the standard check value
    }

    #[test]
    fn reads_captured_reports() {
        let ys = Bridge::YsTech;
        // Captured by barracuda.py on a 0550 dongle: an event, then the battery reply (0x26 = 38%).
        let event = hex("50 49 01 c0 59 31 29 01 03 00 0e 84 00");
        let reply = hex("50 49 08 fa b8 ce 17 01 04 00 21 01 01 26");
        let report: Vec<u8> = [vec![0x02, (event.len() + reply.len()) as u8], event.clone(), reply.clone()].concat();
        let mut stream = Stream::default();
        let frames = stream.feed(ys.chunk(&report));
        assert_eq!(frames, vec![event, reply]);
        let mut heard = Heard::default();
        frames.iter().for_each(|f| heard.take(f));
        assert_eq!(heard.battery(), Some(Battery { percent: 38, charging: false }));
        // Plugging the cable: battery 53% and cable 01 in one report, sent unasked.
        let plugged = hex("02 1c 50 49 08 fc 04 2d 67 00 04 00 21 02 01 35 50 49 08 fd 04 2d 67 00 04 00 2a 02 01 01");
        stream.feed(ys.chunk(&plugged)).iter().for_each(|f| heard.take(f));
        assert_eq!(heard.battery(), Some(Battery { percent: 53, charging: true }));
        // The other bridge's framing, garbage and media-key reports carry nothing.
        assert!(Bridge::Macronix.chunk(&plugged).is_empty());
        assert!(ys.chunk(&[0x01, 0x00]).is_empty());
        assert!(Stream::default().feed(ys.chunk(&[0x02, 40, 0x50, 0x49])).is_empty());
    }

    #[test]
    fn joins_a_frame_split_across_reports() {
        // From leag's PROTOCOL.md for the 0552 dongle: a link report, here in two halves.
        let link = hex("50 49 08 f7 0d 32 69 00 04 00 20 02 01 00");
        let mac = Bridge::Macronix;
        let first: Vec<u8> = [vec![0x01, 0x80, 6], link[..6].to_vec()].concat();
        let second: Vec<u8> = [vec![0x01, 0x80, 8], link[6..].to_vec(), vec![0; 20]].concat(); // padded, as read
        let mut stream = Stream::default();
        assert!(stream.feed(mac.chunk(&first)).is_empty());
        assert_eq!(stream.feed(mac.chunk(&second)), vec![link]);
        // The battery reply the same way; a media-key report in between changes nothing.
        let reply = hex("50 49 08 fa b8 ce 17 01 04 00 21 01 01 26");
        let mut heard = Heard::default();
        assert!(stream.feed(mac.chunk(&hex("02 00 02 00 00"))).is_empty());
        for half in [&reply[..9], &reply[9..]] {
            let report: Vec<u8> = [vec![0x01, 0x80, half.len() as u8], half.to_vec()].concat();
            stream.feed(mac.chunk(&report)).iter().for_each(|f| heard.take(f));
        }
        assert_eq!(heard.battery(), Some(Battery { percent: 38, charging: false }));
    }
}
