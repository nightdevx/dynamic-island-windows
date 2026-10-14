use std::path::PathBuf;

use crate::text::{tr, Language};

/// User settings from `%APPDATA%\DynamicIsland\settings.ini`, applied as soon as the file is saved.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub expand_on_hover: bool,
    pub hover_delay_ms: u32,
    /// Lowercase exe names: hide whenever one of these is in front.
    pub hide_exe: Vec<String>,
    /// Lowercase exe names: stay visible even when one of these goes fullscreen.
    pub show_exe: Vec<String>,
    /// Moving bars while music plays; they cost the compositor a frame per refresh.
    pub equalizer: bool,
    /// New screenshots go to the shelf, with a peek.
    pub screenshots: bool,
    /// The microphone/camera dot, and a peek when one starts.
    pub privacy_dot: bool,
    /// A soft glow in the art's color around the pill while music plays.
    pub glow: bool,
    /// A peek with the new song when one starts.
    pub song_peek: bool,
    /// A peek with the level when the system volume changes.
    pub volume_peek: bool,
    /// A peek when a Claude session starts waiting on the user.
    pub claude_peek: bool,
    /// A preview of each new Windows notification.
    pub notifications: bool,
    /// The island's own: it opens by itself for nothing (Windows' setting is not consulted).
    pub do_not_disturb: bool,
    pub language: Language,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            expand_on_hover: true,
            hover_delay_ms: 300,
            hide_exe: Vec::new(),
            show_exe: Vec::new(),
            equalizer: true,
            screenshots: true,
            privacy_dot: true,
            glow: true,
            song_peek: true,
            volume_peek: true,
            claude_peek: true,
            notifications: true,
            do_not_disturb: false,
            language: Language::Auto,
        }
    }
}

/// The settings file for `s`: every key with what it does, in the island's language. The
/// settings window writes it, and people can edit it by hand too.
pub fn file_text(s: &Settings) -> String {
    let entries: &[(&str, &str, &str, String)] = &[
        (
            "Dil: auto (Windows'un dili), tr ya da en.",
            "Language: auto (Windows' own), tr or en.",
            "language",
            s.language.code().into(),
        ),
        ("Fare üzerine gelince açılsın mı? false: sadece tıklayınca açılır.", "Open when the mouse rests on it? false: only on a click.", "expand_on_hover", s.expand_on_hover.to_string()),
        (
            "Açılmadan önce farenin island üzerinde durması gereken süre (ms, 0-5000).",
            "How long the mouse rests on the island before it opens (ms, 0-5000).",
            "hover_delay_ms",
            s.hover_delay_ms.to_string(),
        ),
        (
            "Island tam ekran uygulamalarda ve oyunlarda kendiliğinden gizlenir.\n# Bu programlar öndeyken tam ekran olmasa da gizlensin (virgülle ayırın), örn: valorant.exe, obs64.exe",
            "The island hides by itself in fullscreen apps and games.\n# Also hide it while these programs are in front, fullscreen or not (comma separated), e.g. valorant.exe, obs64.exe",
            "hide_exe",
            s.hide_exe.join(", "),
        ),
        ("Bu programlar tam ekrana geçse bile island görünür kalsın.", "Keep the island visible even when these programs go fullscreen.", "show_exe", s.show_exe.join(", ")),
        (
            "Müzik çalarken kapalı island'daki hareketli çubuklar. false: daha az GPU, sabit çubuklar.",
            "Moving bars on the closed island while music plays. false: less GPU, still bars.",
            "equalizer",
            s.equalizer.to_string(),
        ),
        ("Müzik çalarken island'ın kenarında kapak renginde hafif bir parıltı.", "A soft glow in the cover's color around the island while music plays.", "glow", s.glow.to_string()),
        ("Yeni şarkı başlayınca island kısa süre açılıp şarkıyı göstersin mi?", "Show the new song for a moment when one starts?", "song_peek", s.song_peek.to_string()),
        ("Sistem sesi değişince island seviyeyi göstersin mi?", "Show the level when the system volume changes?", "volume_peek", s.volume_peek.to_string()),
        ("Mikrofon ya da kamera kullanılırken island'da renkli nokta yansın mı?", "Light a colored dot while the microphone or camera is in use?", "privacy_dot", s.privacy_dot.to_string()),
        (
            "Bir Claude oturumu izin isteyince ya da cevabı bitince island kısa süre açılsın mı?",
            "Open the island for a moment when a Claude session asks for permission or finishes an answer?",
            "claude_peek",
            s.claude_peek.to_string(),
        ),
        (
            "Bilgisayara gelen bildirimler island'da kısa bir önizlemeyle görünsün mü?",
            "Show each new notification on the island in a short preview?",
            "notifications",
            s.notifications.to_string(),
        ),
        (
            "Rahatsız etmeyin: true iken island hiçbir şey için kendiliğinden açılmaz (Windows'un ayarından bağımsız).",
            "Do not disturb: while true the island opens by itself for nothing (apart from Windows' own setting).",
            "do_not_disturb",
            s.do_not_disturb.to_string(),
        ),
        (
            "Yeni ekran görüntüleri (Win + PrtScn, Ekran Alıntısı Aracı) Raf'a eklensin ve kısa bir önizleme açılsın mı?",
            "Put new screenshots (Win + PrtScn, Snipping Tool) on the shelf, with a short preview?",
            "screenshots",
            s.screenshots.to_string(),
        ),
    ];
    let mut text = tr(
        "# Dynamic Island ayarları. Ayarlar penceresi bu dosyayı yazar; elle değiştirip kaydettiğinizde de hemen uygulanır.\n",
        "# Dynamic Island settings. The settings window writes this file; edits saved by hand apply at once too.\n",
    )
    .to_string();
    for (turkish, english, key, value) in entries {
        text += &format!("\n# {}\n{key}={value}\n", tr(turkish, english));
    }
    text
}

fn exe_list(value: &str) -> Vec<String> {
    value.split(',').map(|name| name.trim().to_lowercase()).filter(|name| !name.is_empty()).collect()
}

pub fn dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("DynamicIsland"))
}

pub fn path() -> Option<PathBuf> {
    Some(dir()?.join("settings.ini"))
}

/// Writes the settings file; the island picks it up from there like a hand edit.
pub fn save(settings: &Settings) {
    let Some(path) = path() else { return };
    let written = std::fs::create_dir_all(path.parent().unwrap()).and_then(|_| std::fs::write(&path, file_text(settings)));
    if let Err(e) = written {
        crate::log(format!("settings: cannot write {}: {e}", path.display()));
    }
}

/// Reads the settings file, creating it with defaults on first run. Bad lines are logged
/// and fall back to their defaults.
pub fn load() -> Settings {
    let Some(path) = path() else { return Settings::default() };
    if !path.exists() {
        save(&Settings::default());
        return Settings::default();
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let (settings, problems) = parse(&text);
            for problem in problems {
                crate::log(format!("settings: {problem}"));
            }
            settings
        }
        Err(e) => {
            crate::log(format!("settings: cannot read {}: {e}", path.display()));
            Settings::default()
        }
    }
}

pub fn parse(text: &str) -> (Settings, Vec<String>) {
    let mut settings = Settings::default();
    let mut problems = Vec::new();
    for (number, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            problems.push(format!("line {}: expected key=value: {line}", number + 1));
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        let ok = match key {
            "expand_on_hover" => value.parse().map(|v| settings.expand_on_hover = v).is_ok(),
            "equalizer" => value.parse().map(|v| settings.equalizer = v).is_ok(),
            "screenshots" => value.parse().map(|v| settings.screenshots = v).is_ok(),
            "privacy_dot" => value.parse().map(|v| settings.privacy_dot = v).is_ok(),
            "glow" => value.parse().map(|v| settings.glow = v).is_ok(),
            "song_peek" => value.parse().map(|v| settings.song_peek = v).is_ok(),
            "volume_peek" => value.parse().map(|v| settings.volume_peek = v).is_ok(),
            "claude_peek" => value.parse().map(|v| settings.claude_peek = v).is_ok(),
            "notifications" => value.parse().map(|v| settings.notifications = v).is_ok(),
            "do_not_disturb" => value.parse().map(|v| settings.do_not_disturb = v).is_ok(),
            "language" => Language::from_code(value).map(|v| settings.language = v).is_some(),
            "hover_delay_ms" => value.parse().ok().filter(|v| *v <= 5000).map(|v| settings.hover_delay_ms = v).is_some(),
            "hide_exe" => {
                settings.hide_exe = exe_list(value);
                true
            }
            "show_exe" => {
                settings.show_exe = exe_list(value);
                true
            }
            _ => {
                problems.push(format!("line {}: unknown key {key}", number + 1));
                continue;
            }
        };
        if !ok {
            problems.push(format!("line {}: invalid value for {key}: {value}", number + 1));
        }
    }
    (settings, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_files_read_back() {
        assert_eq!(parse(&file_text(&Settings::default())), (Settings::default(), vec![]));
        let changed = Settings {
            expand_on_hover: false,
            hover_delay_ms: 700,
            hide_exe: vec!["obs64.exe".into(), "valorant.exe".into()],
            show_exe: vec!["mpv.exe".into()],
            equalizer: false,
            screenshots: false,
            privacy_dot: false,
            glow: false,
            song_peek: false,
            volume_peek: false,
            claude_peek: false,
            notifications: false,
            do_not_disturb: true,
            language: Language::English,
        };
        assert_eq!(parse(&file_text(&changed)), (changed.clone(), vec![]));
        crate::text::set(Language::English);
        assert!(file_text(&changed).starts_with("# Dynamic Island settings."));
        assert_eq!(parse(&file_text(&changed)), (changed, vec![]));
    }

    #[test]
    fn parses_values_and_reports_bad_lines() {
        let text = "\u{feff}expand_on_hover = false\nhover_delay_ms=9999\nnonsense\ncolor=red\nhide_exe= Valorant.exe, ,cs2.exe\n";
        let (settings, problems) = parse(text);
        let hide_exe = vec!["valorant.exe".to_string(), "cs2.exe".to_string()];
        assert_eq!(settings, Settings { expand_on_hover: false, hide_exe, ..Settings::default() });
        assert_eq!(problems.len(), 3, "{problems:?}");
    }
}
