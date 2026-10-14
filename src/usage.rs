//! Live plan limits from the endpoint Claude Code itself fills `/usage` from
//! (undocumented; the user chose it over the status line, which the VS Code extension never
//! runs). A background thread fetches and writes `limits.txt` like the status line does; the
//! island picks the file up through its folder watch. The OAuth token is read from Claude
//! Code's credentials for each request, sent only to api.anthropic.com, never kept or logged.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use windows::{
    core::*,
    Data::Json::JsonObject,
    Win32::{Foundation::E_FAIL, Networking::WinHttp::*},
};

use crate::claude::{self, Limits, Window};

const MIN_INTERVAL_MS: u64 = 3 * 60_000;
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static LAST_MS: AtomicU64 = AtomicU64::new(0);

/// Fetches in the background unless a fetch ran in the last 3 minutes or is still running.
/// Called on Claude activity and when the Claude tab opens, never on a timer.
pub fn refresh() {
    let now = claude::now_ms();
    if now.saturating_sub(LAST_MS.load(Ordering::Relaxed)) < MIN_INTERVAL_MS || IN_FLIGHT.swap(true, Ordering::AcqRel) {
        return;
    }
    LAST_MS.store(now, Ordering::Relaxed);
    std::thread::spawn(|| {
        if let Err(e) = fetch() {
            crate::log(format!("usage: {}", e.message()));
        }
        IN_FLIGHT.store(false, Ordering::Release);
    });
}

fn fetch() -> Result<()> {
    let token = access_token()?;
    let headers = format!("Authorization: Bearer {token}\r\nanthropic-beta: oauth-2025-04-20\r\nAccept: application/json\r\n");
    let body = https_get("api.anthropic.com", "/api/oauth/usage", &headers)?;
    let limits = parse_usage(&body, claude::now_ms() / 1000)?;
    let dir = claude::dir().ok_or_else(|| Error::new(E_FAIL, "no LOCALAPPDATA"))?;
    claude::save_limits(&dir.join("limits.txt"), &limits)
}

fn access_token() -> Result<String> {
    let home = std::env::var_os("USERPROFILE").ok_or_else(|| Error::new(E_FAIL, "no USERPROFILE"))?;
    let path = std::path::Path::new(&home).join(".claude").join(".credentials.json");
    let text = std::fs::read_to_string(path).map_err(|e| Error::new(E_FAIL, format!("credentials: {e}")))?;
    let oauth = JsonObject::Parse(&HSTRING::from(text))?.GetNamedObject(h!("claudeAiOauth"))?;
    // Claude Code refreshes the token while it runs; an expired one waits for that.
    if (oauth.GetNamedNumber(h!("expiresAt")).unwrap_or(0.0) as u64) < claude::now_ms() + 30_000 {
        return Err(Error::new(E_FAIL, "token expired; Claude Code refreshes it on its next use"));
    }
    Ok(oauth.GetNamedString(h!("accessToken"))?.to_string())
}

/// Closes a WinHTTP handle when dropped.
struct Handle(*mut core::ffi::c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn https_get(host: &str, path: &str, headers: &str) -> Result<String> {
    unsafe {
        let open = |raw: *mut core::ffi::c_void| if raw.is_null() { Err(Error::from_thread()) } else { Ok(Handle(raw)) };
        let session = open(WinHttpOpen(
            w!("DynamicIsland/0.1"),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ))?;
        WinHttpSetTimeouts(session.0, 5_000, 5_000, 5_000, 10_000)?;
        let connection = open(WinHttpConnect(session.0, &HSTRING::from(host), INTERNET_DEFAULT_HTTPS_PORT, 0))?;
        let request = open(WinHttpOpenRequest(
            connection.0,
            w!("GET"),
            &HSTRING::from(path),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ))?;
        let headers: Vec<u16> = headers.encode_utf16().collect();
        WinHttpSendRequest(request.0, Some(&headers), None, 0, 0, 0)?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())?;

        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut _),
            &mut size,
            std::ptr::null_mut(),
        )?;

        let mut body = Vec::new();
        loop {
            let mut available = 0u32;
            WinHttpQueryDataAvailable(request.0, &mut available)?;
            if available == 0 || body.len() > 1 << 20 {
                break;
            }
            let start = body.len();
            body.resize(start + available as usize, 0);
            let mut read = 0u32;
            WinHttpReadData(request.0, body[start..].as_mut_ptr() as *mut _, available, &mut read)?;
            body.truncate(start + read as usize);
        }
        if status != 200 {
            // Only the status: the body could echo request details.
            return Err(Error::new(E_FAIL, format!("HTTP {status}")));
        }
        Ok(String::from_utf8_lossy(&body).into_owned())
    }
}

/// Reads the 5-hour and weekly windows. Newer responses list them in `limits`
/// (`session`, `weekly_all`); older ones have `five_hour` / `seven_day` objects.
fn parse_usage(body: &str, now: u64) -> Result<Limits> {
    let json = JsonObject::Parse(&HSTRING::from(body))?;
    let mut limits = Limits { updated: now, ..Limits::default() };
    if let Ok(list) = json.GetNamedArray(h!("limits")) {
        for i in 0..list.Size()? {
            let Ok(item) = list.GetObjectAt(i) else { continue };
            let window = || {
                Some(Window {
                    used_percentage: item.GetNamedNumber(h!("percent")).ok()?,
                    resets_at: parse_time(&item.GetNamedString(h!("resets_at")).ok()?.to_string())?,
                })
            };
            match item.GetNamedString(h!("kind")).map(|k| k.to_string()).as_deref() {
                Ok("session") => limits.five_hour = window(),
                Ok("weekly_all") => limits.seven_day = window(),
                _ => {}
            }
        }
    }
    let flat = |key: &HSTRING| {
        let w = json.GetNamedObject(key).ok()?;
        Some(Window {
            used_percentage: w.GetNamedNumber(h!("utilization")).ok()?,
            resets_at: parse_time(&w.GetNamedString(h!("resets_at")).ok()?.to_string())?,
        })
    };
    limits.five_hour = limits.five_hour.or_else(|| flat(h!("five_hour")));
    limits.seven_day = limits.seven_day.or_else(|| flat(h!("seven_day")));
    if limits.five_hour.is_none() && limits.seven_day.is_none() {
        return Err(Error::new(E_FAIL, "no limits in the response"));
    }
    Ok(limits)
}

/// "2026-10-13T11:50:00.483930+00:00" to unix seconds.
fn parse_time(text: &str) -> Option<u64> {
    let (date, time) = text.split_once('T')?;
    let mut ymd = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, d) = (ymd.next()??, ymd.next()??, ymd.next()??);
    let mut hms = time.get(..8)?.split(':').map(|p| p.parse::<i64>().ok());
    let (hh, mm, ss) = (hms.next()??, hms.next()??, hms.next()??);
    let zone = &time[8..];
    let offset = match zone.find(['+', '-']) {
        Some(i) => {
            let sign = if zone.as_bytes()[i] == b'-' { -1 } else { 1 };
            let mut parts = zone[i + 1..].split(':').map(|p| p.parse::<i64>().ok());
            sign * (parts.next()?? * 3600 + parts.next().flatten().unwrap_or(0) * 60)
        }
        None => 0, // "Z" or no zone: UTC
    };
    // Days since 1970-01-01 (Howard Hinnant's days_from_civil).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss - offset).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iso_times() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2026-10-13T11:50:00.483930+00:00"), Some(1_791_892_200));
        assert_eq!(parse_time("2026-10-16T07:00:00+03:00"), Some(1_792_123_200));
        assert_eq!(parse_time("2000-02-29T23:59:59Z"), Some(951_868_799));
        assert_eq!(parse_time("yesterday"), None);
    }

    #[test]
    fn parses_both_response_shapes() {
        let listed = r#"{"five_hour":null,"limits":[
            {"kind":"session","percent":7,"resets_at":"2026-10-13T11:50:00+00:00"},
            {"kind":"weekly_all","percent":37,"resets_at":"2026-10-16T04:00:00+00:00"},
            {"kind":"weekly_scoped","percent":1,"resets_at":"2026-10-16T04:00:00+00:00"}]}"#;
        let limits = parse_usage(listed, 5).unwrap();
        assert_eq!(limits.five_hour, Some(Window { used_percentage: 7.0, resets_at: 1_791_892_200 }));
        assert_eq!(limits.seven_day.map(|w| w.used_percentage), Some(37.0));
        assert_eq!(limits.updated, 5);

        let flat = r#"{"five_hour":{"utilization":42,"resets_at":"2026-10-13T11:50:00+00:00"},"seven_day":null}"#;
        let limits = parse_usage(flat, 0).unwrap();
        assert_eq!(limits.five_hour.map(|w| w.used_percentage), Some(42.0));
        assert_eq!(limits.seven_day, None);

        assert!(parse_usage(r#"{"limits":[]}"#, 0).is_err());
    }
}
