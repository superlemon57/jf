//! Interface language: English (default) or French, set by `language` in config.json.
//!
//! Each string is written in both languages where it is used: `t("Play", "Lecture")`.

use std::sync::OnceLock;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    English,
    French,
}

impl Language {
    /// `"fr"` (or `"fr-FR"`, `"french"`…) selects French; anything else is English.
    pub fn parse(code: &str) -> Self {
        if code.trim().to_ascii_lowercase().starts_with("fr") {
            Self::French
        } else {
            Self::English
        }
    }
}

static LANGUAGE: OnceLock<Language> = OnceLock::new();

/// Sets the interface language once, at startup.
pub fn set(language: Language) {
    let _ = LANGUAGE.set(language);
}

pub fn language() -> Language {
    LANGUAGE.get().copied().unwrap_or_default()
}

/// The text in the interface language.
pub fn t(english: &'static str, french: &'static str) -> &'static str {
    match language() {
        Language::English => english,
        Language::French => french,
    }
}
