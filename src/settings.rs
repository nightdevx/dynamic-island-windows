use std::path::PathBuf;

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
}

impl Default for Settings {
    fn default() -> Self {
        Self { expand_on_hover: true, hover_delay_ms: 300, hide_exe: Vec::new(), show_exe: Vec::new(), equalizer: true }
    }
}

const DEFAULT_FILE: &str = "\
# Dynamic Island ayarları. Dosyayı kaydettiğinizde hemen uygulanır.

# Fare üzerine gelince açılsın mı? false: sadece tıklayınca açılır.
expand_on_hover=true

# Açılmadan önce farenin island üzerinde durması gereken süre (ms, 0-5000).
hover_delay_ms=300

# Island tam ekran uygulamalarda ve oyunlarda kendiliğinden gizlenir.
# Bu programlar öndeyken tam ekran olmasa da gizlensin (virgülle ayırın), örn: valorant.exe, obs64.exe
hide_exe=
# Bu programlar tam ekrana geçse bile island görünür kalsın.
show_exe=

# Müzik çalarken kapalı island'daki hareketli çubuklar. false: daha az GPU, sabit çubuklar.
equalizer=true
";

fn exe_list(value: &str) -> Vec<String> {
    value.split(',').map(|name| name.trim().to_lowercase()).filter(|name| !name.is_empty()).collect()
}

pub fn dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("DynamicIsland"))
}

pub fn path() -> Option<PathBuf> {
    Some(dir()?.join("settings.ini"))
}

/// Reads the settings file, creating it with defaults on first run. Bad lines are logged
/// and fall back to their defaults; the file itself is never rewritten.
pub fn load() -> Settings {
    let Some(path) = path() else { return Settings::default() };
    if !path.exists() {
        let written = std::fs::create_dir_all(path.parent().unwrap()).and_then(|_| std::fs::write(&path, DEFAULT_FILE));
        if let Err(e) = written {
            crate::log(format!("settings: cannot create {}: {e}", path.display()));
        }
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
    fn parses_defaults_file() {
        assert_eq!(parse(DEFAULT_FILE), (Settings::default(), vec![]));
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
