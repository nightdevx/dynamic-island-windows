//! Songs liked with the heart on the music page, newest first, in
//! `%APPDATA%\DynamicIsland\liked.tsv`: one song a line, its fields separated by tabs.

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Song {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub app: String,
    pub added: String, // local time, "2026-10-14 21:30"
}

fn path() -> Option<PathBuf> {
    Some(crate::settings::dir()?.join("liked.tsv"))
}

pub fn load() -> Vec<Song> {
    path().and_then(|p| std::fs::read_to_string(p).ok()).map_or_else(Vec::new, |text| parse(&text))
}

pub fn save(songs: &[Song]) {
    let Some(path) = path() else { return };
    let written = std::fs::create_dir_all(path.parent().unwrap()).and_then(|_| std::fs::write(&path, print(songs)));
    if let Err(e) = written {
        crate::log(format!("liked: cannot write {}: {e}", path.display()));
    }
}

/// Where the song with this title and artist is in the list.
pub fn find(songs: &[Song], title: &str, artist: &str) -> Option<usize> {
    songs.iter().position(|s| s.title.eq_ignore_ascii_case(title) && s.artist.eq_ignore_ascii_case(artist))
}

/// A search for the song on YouTube Music.
pub fn search_url(song: &Song) -> String {
    let mut url = "https://music.youtube.com/search?q=".to_string();
    for byte in format!("{} {}", song.title, song.artist).trim().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => url.push(byte as char),
            b' ' => url.push('+'),
            _ => url += &format!("%{byte:02X}"),
        }
    }
    url
}

/// Now, as `Song::added` shows it.
pub fn now() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}:{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute)
}

fn parse(text: &str) -> Vec<Song> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut fields = line.split('\t').map(str::to_string);
            let mut next = || fields.next().unwrap_or_default();
            let (added, title, artist, album, app) = (next(), next(), next(), next(), next());
            Song { title, artist, album, app, added }
        })
        .collect()
}

fn print(songs: &[Song]) -> String {
    // A tab or a line break inside a field would split it.
    let clean = |field: &str| field.replace(['\t', '\r', '\n'], " ");
    let mut text = crate::text::tr(
        "# Beğenilen şarkılar: eklenme zamanı, şarkı, sanatçı, albüm, oynatıcı (sekmeyle ayrılmış)\n",
        "# Liked songs: added, title, artist, album, player (tab separated)\n",
    )
    .to_string();
    for s in songs {
        let fields = [&s.added, &s.title, &s.artist, &s.album, &s.app].map(|f| clean(f));
        text += &(fields.join("\t") + "\n");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_read_back_and_songs_are_found() {
        let songs = vec![
            Song { title: "Gece\tYarısı".into(), artist: "Sezen Aksu".into(), album: String::new(), app: "Brave".into(), added: "2026-10-14 21:30".into() },
            Song { title: "Blue".into(), artist: "Eiffel 65".into(), album: "Europop".into(), app: "Spotify".into(), added: "2026-10-13 09:05".into() },
        ];
        let back = parse(&print(&songs));
        assert_eq!(back[0].title, "Gece Yarısı"); // the tab inside the title became a space
        assert_eq!(back[1], songs[1]);
        assert_eq!(find(&back, "BLUE", "eiffel 65"), Some(1));
        assert_eq!(find(&back, "Blue", "Someone else"), None);
        assert!(parse("").is_empty());
        assert_eq!(search_url(&back[0]), "https://music.youtube.com/search?q=Gece+Yar%C4%B1s%C4%B1+Sezen+Aksu");
        assert_eq!(search_url(&back[1]), "https://music.youtube.com/search?q=Blue+Eiffel+65");
    }
}
