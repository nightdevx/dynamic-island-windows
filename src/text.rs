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

/// `turkish` or `english`, whichever the island speaks.
pub fn tr<T>(turkish: T, english: T) -> T {
    if self::english() {
        english
    } else {
        turkish
    }
}
