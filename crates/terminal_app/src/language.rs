use std::sync::OnceLock;

use gpui::{App, SharedString};
use settings::{DisplayLanguage, SettingsFile, SettingsStore};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Locale {
    #[default]
    English,
    SimplifiedChinese,
}

impl Locale {
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::SimplifiedChinese => "zh-CN",
        }
    }

    pub(crate) fn text(self, key: &str) -> SharedString {
        self.lookup(key).unwrap_or_else(|| SharedString::from(key))
    }

    pub(crate) fn lookup(self, key: &str) -> Option<SharedString> {
        crate::_rust_i18n_try_translate(self.tag(), key)
            .map(|text| SharedString::from(text.as_ref()))
    }

    fn from_preferred_languages(languages: impl IntoIterator<Item = impl AsRef<str>>) -> Self {
        languages
            .into_iter()
            .find_map(|language| {
                let language = language.as_ref().to_ascii_lowercase();
                let mut subtags = language.split('-');
                match subtags.next()? {
                    "en" => Some(Self::English),
                    "zh" => match subtags.next() {
                        Some("hant" | "hk" | "mo" | "tw") => None,
                        _ => Some(Self::SimplifiedChinese),
                    },
                    _ => None,
                }
            })
            .unwrap_or_default()
    }
}

pub(crate) fn configured(cx: &App) -> DisplayLanguage {
    cx.try_global::<SettingsStore>()
        .and_then(|store| {
            // The language picker saves to the root user settings, even when a profile or OS override is active.
            store
                .get_content_for_file(SettingsFile::User)
                .and_then(|content| content.display_language)
                .or(store.merged_settings().display_language)
        })
        .unwrap_or_default()
}

pub(crate) fn current(cx: &App) -> Locale {
    match configured(cx) {
        DisplayLanguage::Auto => {
            static SYSTEM_LOCALE: OnceLock<Locale> = OnceLock::new();
            *SYSTEM_LOCALE
                .get_or_init(|| Locale::from_preferred_languages(sys_locale::get_locales()))
        }
        DisplayLanguage::English => Locale::English,
        DisplayLanguage::SimplifiedChinese => Locale::SimplifiedChinese,
    }
}

pub(crate) fn text(cx: &App, key: &str) -> SharedString {
    current(cx).text(key)
}

macro_rules! localized_format {
    ($locale:expr, $key:literal, $($name:ident = $argument:expr),+ $(,)?) => {
        rust_i18n::t!($key, locale = $locale.tag(), $($name = $argument),+).into_owned()
    };
}

pub(crate) use localized_format;

#[cfg(test)]
mod tests {
    use super::Locale::{self, English, SimplifiedChinese};

    #[test]
    fn catalog_formats_named_arguments_and_falls_back_to_english() {
        assert_eq!(
            super::localized_format!(SimplifiedChinese, "transfer-saved", path = "/tmp/log"),
            "已保存 /tmp/log"
        );
        assert_eq!(
            crate::_rust_i18n_try_translate("fr-FR", "settings").as_deref(),
            Some("Settings")
        );
    }

    #[test]
    fn auto_display_language_uses_supported_system_preferences() {
        for (preferences, expected) in [
            (&["zh-Hans-CN", "en-US"][..], SimplifiedChinese),
            (&["en-GB", "zh-CN"], English),
            (&["ja-JP", "zh-SG"], SimplifiedChinese),
            (&["zh-hans-HK"], SimplifiedChinese),
            (&["zh-Hant-CN", "zh-TW", "zh-HK", "zh-MO"], English),
            (&["fr-FR"], English),
            (&[], English),
        ] {
            assert_eq!(Locale::from_preferred_languages(preferences), expected);
        }
    }
}
