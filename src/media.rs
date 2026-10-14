//! What plays, from the Windows media sessions (the controls in the volume flyout: Spotify,
//! browsers, Media Player, ...). All session work runs on one background thread: it waits
//! for session events or commands, reads the current state, and posts a message to the
//! window, which then takes the snapshot. Nothing polls.

use std::{
    sync::{
        mpsc::{self, Receiver, RecvTimeoutError, Sender},
        Arc, Mutex,
    },
    time::Duration,
};

use windows::{
    core::*,
    Foundation::TypedEventHandler,
    Media::{Control::*, MediaPlaybackAutoRepeatMode},
    Storage::Streams::IRandomAccessStreamReference,
    Win32::{
        Foundation::{HWND, LPARAM, WPARAM},
        Graphics::Imaging::*,
        System::{
            Com::{CoCreateInstance, IStream, CLSCTX_INPROC_SERVER},
            WinRT::{CreateStreamOverRandomAccessStream, RoInitialize, RO_INIT_MULTITHREADED},
        },
        UI::WindowsAndMessaging::PostMessageW,
    },
};

use crate::claude::now_ms;

pub enum Command {
    Refresh,
    TogglePlay,
    Next,
    Previous,
    Seek(f64), // seconds
    NextSession,
    Shuffle(bool),
    Repeat(MediaPlaybackAutoRepeatMode),
}

/// Album art as premultiplied BGRA, square.
pub struct Art {
    pub size: u32,
    pub pixels: Vec<u8>,
}

#[derive(Clone)]
pub struct Media {
    pub app: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub playing: bool,
    pub position: f64, // seconds, at `updated_ms`
    pub duration: f64,
    pub updated_ms: u64,
    pub rate: f64,
    pub can_toggle: bool,
    pub can_next: bool,
    pub can_previous: bool,
    pub can_seek: bool,
    pub shuffle: Option<bool>, // None: the player has no shuffle
    pub repeat: Option<MediaPlaybackAutoRepeatMode>,
    pub sessions: u32,
    pub art: Option<Arc<Art>>,
}

impl Media {
    /// Where playback is now. Some players (Spotify) report the position rarely, so it is
    /// carried forward from the last report instead of asking again.
    pub fn position_at(&self, now_ms: u64) -> f64 {
        if !self.playing {
            return self.position;
        }
        let elapsed = now_ms.saturating_sub(self.updated_ms) as f64 / 1000.0;
        let position = self.position + elapsed * self.rate;
        if self.duration > 0.0 {
            position.min(self.duration)
        } else {
            position
        }
    }

    pub fn same_track(&self, other: &Media) -> bool {
        (&self.app, &self.title, &self.artist, &self.album) == (&other.app, &other.title, &other.artist, &other.album)
    }
}

pub struct MediaLink {
    commands: Sender<Command>,
    snapshot: Arc<Mutex<Option<Media>>>,
}

impl MediaLink {
    /// Starts the media thread; it posts `message` to `hwnd` whenever the snapshot changes.
    /// `art_px` is the size the art is decoded to.
    pub fn start(hwnd: HWND, message: u32, art_px: u32) -> Self {
        let (commands, inbox) = mpsc::channel();
        let snapshot = Arc::new(Mutex::new(None));
        let (events, shared, window) = (commands.clone(), snapshot.clone(), hwnd.0 as isize);
        std::thread::spawn(move || {
            // The media service can fail to answer (at logon, after a crash of its host):
            // start over after a pause that grows, until the window goes away.
            let mut pause = Duration::from_secs(5);
            while let Err(e) = run(&events, &inbox, &shared, window, message, art_px) {
                crate::log(format!("media: {e}; trying again in {} s", pause.as_secs()));
                std::thread::sleep(pause);
                pause = (pause * 2).min(Duration::from_secs(60));
            }
        });
        Self { commands, snapshot }
    }

    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    pub fn current(&self) -> Option<Media> {
        self.snapshot.lock().ok()?.clone()
    }
}

/// Event tokens of the session we listen to.
struct Watched {
    session: GlobalSystemMediaTransportControlsSession,
    tokens: [i64; 3],
}

impl Watched {
    fn new(session: GlobalSystemMediaTransportControlsSession, events: &Sender<Command>) -> Result<Self> {
        let tokens = [
            session.MediaPropertiesChanged(&ping(events))?,
            session.PlaybackInfoChanged(&ping(events))?,
            session.TimelinePropertiesChanged(&ping(events))?,
        ];
        Ok(Self { session, tokens })
    }
}

/// An event handler that only asks the media thread to read again.
fn ping<T: RuntimeType + 'static, U: RuntimeType + 'static>(events: &Sender<Command>) -> TypedEventHandler<T, U> {
    let events = events.clone();
    TypedEventHandler::new(move |_, _| {
        let _ = events.send(Command::Refresh);
        Ok(())
    })
}

impl Drop for Watched {
    fn drop(&mut self) {
        let _ = self.session.RemoveMediaPropertiesChanged(self.tokens[0]);
        let _ = self.session.RemovePlaybackInfoChanged(self.tokens[1]);
        let _ = self.session.RemoveTimelinePropertiesChanged(self.tokens[2]);
    }
}

/// Ok when the window is gone (the command channel closed); Err when the media service
/// failed and the caller should start over.
fn run(
    events: &Sender<Command>,
    inbox: &Receiver<Command>,
    snapshot: &Mutex<Option<Media>>,
    window: isize,
    message: u32,
    art_px: u32,
) -> Result<()> {
    unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
    let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.join()?;
    // Subscribed for the life of the process.
    manager.CurrentSessionChanged(&ping(events))?;
    manager.SessionsChanged(&ping(events))?;

    let mut watched: Option<Watched> = None;
    let mut chosen: Option<String> = None; // session picked with the wheel, by app id
    let mut art: Option<(String, Option<Arc<Art>>)> = None; // track key and its art
    // Look again after this long even without an event: the art is often missing right after
    // a track change, a player that is still starting answers "not ready" (bounded), and a
    // gap between two songs has to be waited out.
    const RETRY: Duration = Duration::from_millis(400);
    let mut wait: Option<Duration> = None;
    let mut failures = 0;
    // Changing songs, browsers drop their media session for a moment (or report an empty
    // title). The last song stays up through such a gap instead of "nothing playing".
    const GAP: Duration = Duration::from_millis(1500);
    let mut gap_since: Option<std::time::Instant> = None;
    let mut published: Option<Media> = None;
    let mut first = true;
    loop {
        let next = if first {
            first = false;
            Ok(Command::Refresh)
        } else if let Some(timeout) = wait {
            inbox.recv_timeout(timeout)
        } else {
            inbox.recv().map_err(|_| RecvTimeoutError::Disconnected)
        };
        wait = None;
        let mut batch = match next {
            Ok(command) => vec![command],
            Err(RecvTimeoutError::Timeout) => vec![Command::Refresh],
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        };
        // Players report one change as a burst of events spread over ~200 ms (a new file:
        // session gone, old track, new track). Wait for the burst to end so the island does
        // not flicker through half-updated states. A user's command goes at once.
        let command = |c: &Command| !matches!(c, Command::Refresh);
        let settle_until = std::time::Instant::now() + Duration::from_millis(600);
        while !batch.iter().any(command) {
            let left = settle_until.saturating_duration_since(std::time::Instant::now());
            match inbox.recv_timeout(left.min(Duration::from_millis(200))) {
                Ok(next) => batch.push(next),
                Err(_) => break,
            }
        }
        batch.extend(inbox.try_iter());

        // Nothing below may end the thread: a failed call is logged and tried again.
        let sessions: Vec<GlobalSystemMediaTransportControlsSession> = match manager.GetSessions() {
            Ok(list) => list.into_iter().collect(),
            Err(e) => {
                failures += 1;
                crate::log(format!("media sessions: {e}"));
                wait = (failures < 5).then_some(RETRY);
                continue;
            }
        };
        let count = sessions.len() as u32;
        let find = |id: &str| sessions.iter().find(|s| s.SourceAppUserModelId().is_ok_and(|a| a == *id)).cloned();
        if batch.iter().any(|c| matches!(c, Command::NextSession)) && count > 1 {
            let ids: Vec<String> = sessions.iter().filter_map(|s| s.SourceAppUserModelId().ok()).map(|a| a.to_string()).collect();
            let current = chosen.clone().or_else(|| manager.GetCurrentSession().ok()?.SourceAppUserModelId().ok().map(|a| a.to_string()));
            let index = current.and_then(|c| ids.iter().position(|id| *id == c)).map_or(0, |i| (i + 1) % ids.len());
            chosen = ids.get(index).cloned();
        }
        let session = chosen.as_deref().and_then(find).or_else(|| {
            chosen = None; // the chosen player went away: follow Windows' pick again
            manager.GetCurrentSession().ok()
        });

        // A player that cannot answer (still starting, or stuck) must not hide one that can.
        // The one shown is the one listened to and the one commands go to.
        let mut media = None;
        let mut session = session;
        if let Some(first) = session.clone() {
            let first_raw = first.as_raw();
            let others = sessions.iter().filter(|s| s.as_raw() != first_raw).cloned();
            for candidate in std::iter::once(first).chain(others) {
                match read(&candidate, count) {
                    Ok(read) => {
                        media = Some(read);
                        session = Some(candidate);
                        break;
                    }
                    Err(e) => crate::log(format!("media read: {e}")),
                }
            }
            if media.is_none() {
                failures += 1;
                wait = (failures < 5).then_some(RETRY);
                continue;
            }
        }
        failures = 0;

        let same = matches!((&watched, &session), (Some(w), Some(s)) if w.session.as_raw() == s.as_raw());
        if !same {
            watched = None;
            if let Some(s) = &session {
                match Watched::new(s.clone(), events) {
                    Ok(w) => watched = Some(w),
                    Err(e) => crate::log(format!("media events: {e}")),
                }
            }
        }
        if let Some(s) = &session {
            // The player answers with events, which bring the new state.
            for command in &batch {
                let _ = match command {
                    Command::TogglePlay => s.TryTogglePlayPauseAsync().map(|_| ()),
                    Command::Next => s.TrySkipNextAsync().map(|_| ()),
                    Command::Previous => s.TrySkipPreviousAsync().map(|_| ()),
                    Command::Seek(seconds) => s.TryChangePlaybackPositionAsync((seconds * 10_000_000.0) as i64).map(|_| ()),
                    Command::Shuffle(on) => s.TryChangeShuffleActiveAsync(*on).map(|_| ()),
                    Command::Repeat(mode) => s.TryChangeAutoRepeatModeAsync(*mode).map(|_| ()),
                    Command::Refresh | Command::NextSession => Ok(()),
                };
            }
        }

        let apps: Vec<String> = sessions.iter().filter_map(|s| s.SourceAppUserModelId().ok()).map(|a| app_name(&a.to_string())).collect();
        let gap = match (&media, &published) {
            // The player that was playing vanished: maybe only between two songs, so do not
            // jump to "nothing" or to another player yet.
            (_, Some(p)) if p.playing && !apps.contains(&p.app) => true,
            (None, Some(_)) => true,
            (Some(m), Some(p)) => m.app == p.app && m.title.is_empty() && m.artist.is_empty(),
            _ => false,
        };
        if gap {
            let left = GAP.saturating_sub(gap_since.get_or_insert_with(std::time::Instant::now).elapsed());
            if !left.is_zero() {
                wait = Some(left);
                continue;
            }
        }
        gap_since = None;

        let media = media.map(|mut media| {
            let key = format!("{}\n{}\n{}\n{}", media.app, media.title, media.artist, media.album);
            let known = art.as_ref().filter(|(k, a)| *k == key && a.is_some());
            media.art = match known {
                Some((_, a)) => a.clone(),
                None => {
                    let decoded = session
                        .as_ref()
                        .and_then(|s| s.TryGetMediaPropertiesAsync().ok()?.join().ok()?.Thumbnail().ok())
                        .and_then(|thumbnail| decode_art(&thumbnail, art_px).map_err(|e| crate::log(format!("art: {e}"))).ok())
                        .map(Arc::new);
                    // Once per track when the art is not there yet.
                    if decoded.is_none() && art.as_ref().is_none_or(|(k, _)| *k != key) {
                        wait = Some(RETRY);
                    }
                    art = Some((key, decoded.clone()));
                    decoded
                }
            };
            media
        });
        published = media.clone();
        if let Ok(mut slot) = snapshot.lock() {
            *slot = media;
        }
        unsafe {
            let _ = PostMessageW(Some(HWND(window as _)), message, WPARAM(0), LPARAM(0));
        }
    }
}

fn read(session: &GlobalSystemMediaTransportControlsSession, sessions: u32) -> Result<Media> {
    let properties = session.TryGetMediaPropertiesAsync()?.join()?;
    let info = session.GetPlaybackInfo()?;
    let controls = info.Controls()?;
    let timeline = session.GetTimelineProperties()?;
    let seconds = |span: windows::Foundation::TimeSpan| span.Duration as f64 / 10_000_000.0;
    // DateTime counts 100 ns ticks since 1601.
    let updated = timeline.LastUpdatedTime()?.UniversalTime;
    let updated_ms = if updated > 116_444_736_000_000_000 { ((updated - 116_444_736_000_000_000) / 10_000) as u64 } else { now_ms() };
    Ok(Media {
        app: app_name(&session.SourceAppUserModelId()?.to_string()),
        title: properties.Title()?.to_string(),
        artist: properties.Artist()?.to_string(),
        album: properties.AlbumTitle()?.to_string(),
        playing: info.PlaybackStatus()? == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing,
        position: seconds(timeline.Position()?) - seconds(timeline.StartTime()?),
        duration: seconds(timeline.EndTime()?) - seconds(timeline.StartTime()?),
        updated_ms,
        rate: info.PlaybackRate().and_then(|r| r.Value()).unwrap_or(1.0),
        can_toggle: controls.IsPlayPauseToggleEnabled()? || controls.IsPlayEnabled()? || controls.IsPauseEnabled()?,
        can_next: controls.IsNextEnabled()?,
        can_previous: controls.IsPreviousEnabled()?,
        can_seek: controls.IsPlaybackPositionEnabled()?,
        shuffle: controls.IsShuffleEnabled()?.then(|| info.IsShuffleActive().and_then(|v| v.Value()).unwrap_or(false)),
        repeat: controls
            .IsRepeatEnabled()?
            .then(|| info.AutoRepeatMode().and_then(|v| v.Value()).unwrap_or(MediaPlaybackAutoRepeatMode::None)),
        sessions,
        art: None,
    })
}

/// Decodes the thumbnail, crops it to the middle square (video thumbnails are 16:9) and
/// scales it to `size` pixels, so a 640 px original is never kept around.
fn decode_art(thumbnail: &IRandomAccessStreamReference, size: u32) -> Result<Art> {
    unsafe {
        let stream = thumbnail.OpenReadAsync()?.join()?;
        let stream: IStream = CreateStreamOverRandomAccessStream(&stream)?;
        let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let frame = wic.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?.GetFrame(0)?;
        let (mut w, mut h) = (0, 0);
        frame.GetSize(&mut w, &mut h)?;
        let side = w.min(h);
        let square = WICRect { X: ((w - side) / 2) as i32, Y: ((h - side) / 2) as i32, Width: side as i32, Height: side as i32 };
        let clipper = wic.CreateBitmapClipper()?;
        clipper.Initialize(&frame, &square)?;
        let scaler = wic.CreateBitmapScaler()?;
        scaler.Initialize(&clipper, size, size, WICBitmapInterpolationModeHighQualityCubic)?;
        let converter = wic.CreateFormatConverter()?;
        converter.Initialize(&scaler, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
        let mut pixels = vec![0u8; (size * size * 4) as usize];
        converter.CopyPixels(std::ptr::null(), size * 4, &mut pixels)?;
        Ok(Art { size, pixels })
    }
}

/// "Spotify.exe", "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify", "MSEdge" → a name people know.
fn app_name(app_id: &str) -> String {
    let base = app_id.rsplit('!').next().unwrap_or(app_id);
    let base = base.rsplit(['\\', '/']).next().unwrap_or(base);
    let base = base.strip_suffix(".exe").or_else(|| base.strip_suffix(".EXE")).unwrap_or(base);
    let base = base.rsplit('.').next().unwrap_or(base);
    match base.to_lowercase().as_str() {
        "msedge" => "Edge".into(),
        "chrome" => "Chrome".into(),
        "firefox" => "Firefox".into(),
        "spotify" | "spotifymusic" => "Spotify".into(),
        "zunemusic" => "Medya Oynatıcı".into(),
        "applemusic" | "itunes" => "Apple Music".into(),
        _ => base.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_names() {
        assert_eq!(app_name("Spotify.exe"), "Spotify");
        assert_eq!(app_name("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"), "Spotify");
        assert_eq!(app_name("MSEdge"), "Edge");
        assert_eq!(app_name("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"), "Medya Oynatıcı");
        assert_eq!(app_name("C:\\Apps\\foobar2000.exe"), "foobar2000");
    }

    #[test]
    fn position_runs_on_while_playing() {
        let media = Media {
            app: String::new(),
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            playing: true,
            position: 10.0,
            duration: 200.0,
            updated_ms: 1_000,
            rate: 1.0,
            can_toggle: true,
            can_next: true,
            can_previous: true,
            can_seek: true,
            shuffle: None,
            repeat: None,
            sessions: 1,
            art: None,
        };
        assert_eq!(media.position_at(6_000), 15.0);
        assert_eq!(media.position_at(1_000_000), 200.0); // not past the end
        assert_eq!(Media { playing: false, ..media.clone() }.position_at(6_000), 10.0);
        assert_eq!(Media { rate: 2.0, ..media }.position_at(3_000), 14.0);
    }
}
