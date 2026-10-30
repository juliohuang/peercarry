//! Shared UI language selection. Reads only the local preference and never creates config.
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LanguagePreference {
    #[default]
    System,
    Zh,
    En,
}

impl LanguagePreference {
    pub fn resolve(self, system: Language) -> Language {
        match self {
            Self::System => system,
            Self::Zh => Language::Zh,
            Self::En => Language::En,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Zh,
    En,
}

impl Language {
    pub fn current() -> Self {
        current_details().language
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::Zh => "zh",
            Self::En => "en",
        }
    }

    pub fn from_tag(tag: &str) -> Self {
        let tag = tag.trim().split(['.', '@']).next().unwrap_or("");
        let primary = tag.split(['-', '_']).next().unwrap_or("");
        if primary.eq_ignore_ascii_case("zh") {
            Self::Zh
        } else {
            Self::En
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LanguageDetails {
    pub preference: LanguagePreference,
    pub system_language: Language,
    pub language: Language,
}

pub fn current_details() -> LanguageDetails {
    details_for(
        read_preference(&crate::paths::config_path()),
        system_language(),
    )
}

pub fn details_for(preference: LanguagePreference, system_language: Language) -> LanguageDetails {
    LanguageDetails {
        preference,
        system_language,
        language: preference.resolve(system_language),
    }
}

fn read_preference(path: &Path) -> LanguagePreference {
    #[derive(Default, Deserialize)]
    #[serde(default)]
    struct PreferenceDocument {
        ui: crate::config::UiConfig,
    }
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| toml::from_str::<PreferenceDocument>(&raw).ok())
        .map(|document| document.ui.language)
        .unwrap_or_default()
}

pub fn system_language() -> Language {
    // Diagnostic override applies only when the user chooses Follow system.
    if let Some(tag) = std::env::var("PEERCARRY_LANG")
        .ok()
        .filter(|tag| !tag.trim().is_empty())
    {
        return Language::from_tag(&tag);
    }
    #[cfg(target_os = "linux")]
    return from_linux_env(
        linux_env("LANGUAGE").as_deref(),
        linux_env("LC_ALL").as_deref(),
        linux_env("LC_MESSAGES").as_deref(),
        linux_env("LANG").as_deref(),
    );
    #[cfg(target_os = "macos")]
    if let Some(tag) = macos_locale() {
        return Language::from_tag(&tag);
    }
    #[cfg(target_os = "windows")]
    if let Some(tag) = windows_locale() {
        return Language::from_tag(&tag);
    }
    #[cfg(not(target_os = "linux"))]
    Language::En
}

#[cfg(any(target_os = "linux", test))]
fn from_linux_env(
    language: Option<&str>,
    all: Option<&str>,
    messages: Option<&str>,
    lang: Option<&str>,
) -> Language {
    let locale = all.or(messages).or(lang);
    if locale.is_some_and(|value| {
        let base = value.trim().split(['.', '@']).next().unwrap_or("");
        matches!(base.to_ascii_uppercase().as_str(), "C" | "POSIX")
    }) {
        return Language::En;
    }
    if let Some(preferred) = language
        .filter(|value| !value.trim().is_empty())
        .and_then(|value| value.split(':').next())
        .filter(|value| !value.trim().is_empty())
    {
        return Language::from_tag(preferred);
    }
    locale.map(Language::from_tag).unwrap_or(Language::En)
}

#[cfg(target_os = "linux")]
fn linux_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[cfg(target_os = "macos")]
fn macos_locale() -> Option<String> {
    use objc2_foundation::NSLocale;
    let languages = NSLocale::preferredLanguages();
    if languages.count() == 0 {
        None
    } else {
        Some(languages.objectAtIndex(0).to_string())
    }
}

#[cfg(target_os = "windows")]
fn windows_locale() -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Globalization::{
        GetUserDefaultUILanguage, GetUserPreferredUILanguages, LCIDToLocaleName, MUI_LANGUAGE_NAME,
    };
    unsafe {
        let mut count = 0;
        let mut length = 0;
        if GetUserPreferredUILanguages(MUI_LANGUAGE_NAME, &mut count, None, &mut length).is_ok()
            && length > 1
        {
            let mut buffer = vec![0u16; length as usize];
            if GetUserPreferredUILanguages(
                MUI_LANGUAGE_NAME,
                &mut count,
                Some(PWSTR(buffer.as_mut_ptr())),
                &mut length,
            )
            .is_ok()
            {
                let first = buffer.split(|unit| *unit == 0).next()?;
                if !first.is_empty() {
                    return String::from_utf16(first).ok();
                }
            }
        }
        let mut locale = [0u16; 85];
        let size = LCIDToLocaleName(GetUserDefaultUILanguage() as u32, Some(&mut locale), 0);
        if size > 1 {
            String::from_utf16(&locale[..size as usize - 1]).ok()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_tags_use_chinese_or_english_fallback() {
        for tag in ["zh", "zh_CN", "zh-Hant", "ZH_hans_CN.UTF-8"] {
            assert_eq!(Language::from_tag(tag), Language::Zh);
        }
        for tag in ["", "en-US", "ja-JP", "fr-FR", "C", "zhang_CN"] {
            assert_eq!(Language::from_tag(tag), Language::En);
        }
        assert_eq!(from_linux_env(None, None, None, None), Language::En);
        assert_eq!(
            from_linux_env(Some("ja_JP:zh_CN"), None, Some("en_US"), Some("zh_CN")),
            Language::En
        );
        assert_eq!(
            from_linux_env(Some("zh_CN"), Some("C.UTF-8"), None, None),
            Language::En
        );
        assert_eq!(
            from_linux_env(Some("zh_CN:en_US"), Some("en_US"), None, None),
            Language::Zh
        );
        assert_eq!(
            from_linux_env(None, None, Some("zh_Hans_CN.UTF-8"), Some("en_US")),
            Language::Zh
        );
    }

    #[test]
    fn manual_preference_overrides_system_and_missing_preferences_do_not_write_files() {
        assert_eq!(
            details_for(LanguagePreference::Zh, Language::En).language,
            Language::Zh
        );
        assert_eq!(
            details_for(LanguagePreference::En, Language::Zh).language,
            Language::En
        );
        assert_eq!(
            details_for(LanguagePreference::System, Language::En).language,
            Language::En
        );
        let root =
            std::env::temp_dir().join(format!("peercarry-language-{}", uuid::Uuid::new_v4()));
        let path = root.join("config.toml");
        assert_eq!(read_preference(&path), LanguagePreference::System);
        assert!(!root.exists());
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            &path,
            "[ui]\nlanguage = 'zh'\n[private]\nvalue = 'synthetic'\n",
        )
        .unwrap();
        assert_eq!(read_preference(&path), LanguagePreference::Zh);
        std::fs::write(&path, "[ui]\nlanguage = 'en'\n").unwrap();
        assert_eq!(read_preference(&path), LanguagePreference::En);
        assert!(
            !serde_json::to_string(&details_for(read_preference(&path), Language::Zh))
                .unwrap()
                .contains("synthetic")
        );
        assert_eq!(
            toml::from_str::<crate::config::Config>("")
                .unwrap()
                .ui
                .language,
            LanguagePreference::System
        );
        assert!(toml::from_str::<crate::config::Config>("[ui]\nlanguage='unsupported'").is_err());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_returns_a_display_language() {
        let locale = windows_locale().expect("Windows should expose a UI language");
        assert!(!locale.is_empty() && !locale.contains('\0'));
    }
}
