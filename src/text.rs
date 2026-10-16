//! The language the island speaks: Turkish or English, as the settings say, or Windows' own
//! for "auto". Each text sits where it is used, in both languages: `tr("Müzik", "Music")`.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Language {
    #[default]
    Auto,
    Turkish,
    English,
}

impl Language {
    /// As the settings file writes it.
    pub fn code(self) -> &'static str {
        match self {
            Language::Auto => "auto",
            Language::Turkish => "tr",
            Language::English => "en",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        [Language::Auto, Language::Turkish, Language::English].into_iter().find(|l| l.code() == code)
    }
}

// Read from every thread (the media thread names players). Tests run side by side, so each
// test thread has a language of its own.
#[cfg(not(test))]
static ENGLISH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(test)]
thread_local! {
    static ENGLISH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn set(language: Language) {
    let english = match language {
        // Turkish only for a Turkish Windows; everyone else reads English better.
        Language::Auto => (unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() } & 0x3FF) != 0x1F, // LANG_TURKISH
        Language::Turkish => false,
        Language::English => true,
    };
    #[cfg(not(test))]
    ENGLISH.store(english, std::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    ENGLISH.with(|e| e.set(english));
}

pub fn english() -> bool {
    #[cfg(not(test))]
    return ENGLISH.load(std::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    return ENGLISH.with(std::cell::Cell::get);
}

/// The time as Windows shows it to the user ("16:42"), and the day in the island's language
/// ("16 Ekim Perşembe", "Thursday, October 16").
pub fn clock() -> (String, String) {
    use windows::{
        core::{w, PCWSTR},
        Win32::Globalization::{GetDateFormatEx, GetTimeFormatEx, ENUM_DATE_FORMATS_FLAGS, TIME_NOSECONDS},
    };
    let (mut time, mut day) = ([0u16; 32], [0u16; 64]);
    let (locale, format) = tr((w!("tr-TR"), w!("d MMMM dddd")), (w!("en-US"), w!("dddd, MMMM d")));
    // The lengths count the closing 0; 0 means it failed.
    let text = |buffer: &[u16], n: i32| String::from_utf16_lossy(&buffer[..(n.max(1) - 1) as usize]);
    unsafe {
        let n = GetTimeFormatEx(PCWSTR::null(), TIME_NOSECONDS, None, PCWSTR::null(), Some(&mut time));
        let m = GetDateFormatEx(locale, ENUM_DATE_FORMATS_FLAGS(0), None, format, Some(&mut day), PCWSTR::null());
        (text(&time, n), text(&day, m))
    }
}

/// `turkish` or `english`, whichever the island speaks.
pub fn tr<T>(turkish: T, english: T) -> T {
    if self::english() {
        english
    } else {
        turkish
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_reads() {
        set(Language::Turkish);
        let (time, day) = clock();
        assert!(time.len() >= 4 && time.chars().any(|c| c.is_ascii_digit()), "{time}");
        let days = ["Pazartesi", "Salı", "Çarşamba", "Perşembe", "Cuma", "Cumartesi", "Pazar"];
        assert!(days.iter().any(|d| day.ends_with(d)), "{day}");
        set(Language::English);
        assert!(clock().1.contains(", "), "{}", clock().1);
    }
}
