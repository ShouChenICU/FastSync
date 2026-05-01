use std::fmt::Display;
use std::str::FromStr;

use rust_i18n::t;

/// fastsync 支持的用户界面语言。
///
/// 该枚举只影响 CLI 帮助、文本摘要和用户可见错误，不改变 JSON 字段与同步语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    En,
    ZhCn,
}

impl Language {
    pub const DEFAULT: Self = Self::En;

    /// 返回 rust-i18n 使用的 locale 标识。
    pub fn as_locale(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhCn => "zh-CN",
        }
    }

    /// 解析 CLI 或环境变量中的语言标识。
    pub fn parse(raw: &str) -> Option<Self> {
        for candidate in raw.split(':') {
            if let Some(language) = Self::parse_one(candidate) {
                return Some(language);
            }
        }

        None
    }

    /// 从 `FASTSYNC_LANG` 或系统 locale 环境变量读取语言，非法值会被忽略。
    pub fn from_env() -> Option<Self> {
        language_from_env_var("FASTSYNC_LANG").or_else(Self::from_system_env)
    }

    /// 从系统 locale 环境变量或 Windows UI 语言读取语言。
    pub fn from_system_env() -> Option<Self> {
        ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"]
            .into_iter()
            .find_map(language_from_env_var)
            .or_else(language_from_windows_user_default_ui_language)
    }

    fn parse_one(raw: &str) -> Option<Self> {
        let normalized = normalize_locale(raw)?;

        if matches!(normalized.as_str(), "c" | "posix") || normalized.starts_with("en") {
            return Some(Self::En);
        }

        if normalized == "cn"
            || normalized == "chinese"
            || normalized == "中文"
            || normalized.starts_with("zh")
        {
            return Some(Self::ZhCn);
        }

        None
    }
}

impl FromStr for Language {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw).ok_or_else(|| format!("unsupported locale: {raw}"))
    }
}

fn language_from_env_var(name: &str) -> Option<Language> {
    std::env::var(name)
        .ok()
        .and_then(|value| Language::parse(&value))
}

fn normalize_locale(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty() {
        return None;
    }

    let without_encoding = value.split('.').next().unwrap_or(value);
    let without_modifier = without_encoding
        .split('@')
        .next()
        .unwrap_or(without_encoding);
    let normalized = without_modifier
        .trim()
        .replace('_', "-")
        .to_ascii_lowercase();

    (!normalized.is_empty()).then_some(normalized)
}

#[cfg(windows)]
fn language_from_windows_user_default_ui_language() -> Option<Language> {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }

    // SAFETY: GetUserDefaultUILanguage has no parameters and only reads the
    // current user's Windows UI language from the operating system.
    let langid = unsafe { GetUserDefaultUILanguage() };

    language_from_windows_langid(langid)
}

#[cfg(not(windows))]
fn language_from_windows_user_default_ui_language() -> Option<Language> {
    None
}

#[cfg(any(windows, test))]
fn language_from_windows_langid(langid: u16) -> Option<Language> {
    const PRIMARY_LANGUAGE_MASK: u16 = 0x03ff;
    const LANG_CHINESE: u16 = 0x04;
    const LANG_ENGLISH: u16 = 0x09;

    match langid & PRIMARY_LANGUAGE_MASK {
        LANG_CHINESE => Some(Language::ZhCn),
        LANG_ENGLISH => Some(Language::En),
        _ => None,
    }
}

/// 设置当前线程后续翻译使用的语言。
pub fn set_language(language: Language) {
    rust_i18n::set_locale(language.as_locale());
}

/// 返回当前全局语言；未知 locale 会回退到英文。
pub fn current_language() -> Language {
    Language::parse(&rust_i18n::locale()).unwrap_or(Language::DEFAULT)
}

/// 获取指定语言的简单翻译文本。
pub fn tr(language: Language, key: &str) -> String {
    t!(key, locale = language.as_locale()).to_string()
}

/// 获取当前语言的简单翻译文本。
pub fn tr_current(key: &str) -> String {
    tr(current_language(), key)
}

/// 获取带单个 `path` 变量的当前语言翻译文本。
pub fn tr_path(key: &str, path: impl Display) -> String {
    t!(
        key,
        locale = current_language().as_locale(),
        path = path.to_string()
    )
    .to_string()
}

/// 获取带 `source` 和 `target` 变量的当前语言翻译文本。
pub fn tr_source_target(key: &str, source: impl Display, target: impl Display) -> String {
    t!(
        key,
        locale = current_language().as_locale(),
        source = source.to_string(),
        target = target.to_string()
    )
    .to_string()
}

/// 获取带 `path`、`source` 和 `target` 变量的当前语言翻译文本。
pub fn tr_path_source_target(
    key: &str,
    path: impl Display,
    source: impl Display,
    target: impl Display,
) -> String {
    t!(
        key,
        locale = current_language().as_locale(),
        path = path.to_string(),
        source = source.to_string(),
        target = target.to_string()
    )
    .to_string()
}

/// 获取带 `value` 变量的当前语言翻译文本。
pub fn tr_value(key: &str, value: impl Display) -> String {
    t!(
        key,
        locale = current_language().as_locale(),
        value = value.to_string()
    )
    .to_string()
}

/// 获取带错误数量和首个错误的当前语言翻译文本。
pub fn tr_many_errors(count: usize, first: &str) -> String {
    t!(
        "error.many",
        locale = current_language().as_locale(),
        count = count,
        first = first
    )
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::{Mutex, OnceLock};

    #[test]
    fn from_str_supports_normalized_language_names() {
        assert_eq!(
            "zh_CN.UTF-8".parse::<Language>().expect("zh_CN.UTF-8"),
            Language::ZhCn
        );
        assert_eq!("C".parse::<Language>().expect("C"), Language::En);
        assert_eq!("en-GB".parse::<Language>().expect("en-GB"), Language::En);
        assert_eq!(
            "zh-Hans-CN".parse::<Language>().expect("zh-Hans-CN"),
            Language::ZhCn
        );
    }

    #[test]
    fn from_str_rejects_unknown_locale() {
        assert!("fr_FR".parse::<Language>().is_err());
    }

    #[test]
    fn from_env_prefers_fastsync_lang_over_system_locale() {
        let _guard = env_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let snapshot = snapshot_env_vars();

        set_env_var("FASTSYNC_LANG", Some("zh_CN.UTF-8"));
        set_env_var("LC_ALL", Some("en-US"));
        set_env_var("LC_MESSAGES", Some("zh-CN"));

        assert_eq!(Language::from_env(), Some(Language::ZhCn));

        restore_env_vars(snapshot);
    }

    #[test]
    fn from_system_env_honors_locale_priority() {
        let _guard = env_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let snapshot = snapshot_env_vars();

        set_env_var("FASTSYNC_LANG", None);
        set_env_var("LC_ALL", Some("zh_CN.UTF-8"));
        set_env_var("LC_MESSAGES", Some("en-US.UTF-8"));
        set_env_var("LANGUAGE", Some("en-US"));
        set_env_var("LANG", Some("zh-CN"));

        assert_eq!(Language::from_system_env(), Some(Language::ZhCn));

        restore_env_vars(snapshot);
    }

    #[test]
    #[cfg(not(windows))]
    fn from_env_returns_none_for_invalid_values() {
        let _guard = env_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let snapshot = snapshot_env_vars();

        set_env_var("FASTSYNC_LANG", Some("fr_FR"));
        set_env_var("LC_ALL", Some("de_DE"));
        set_env_var("LC_MESSAGES", Some("jp_JP"));
        set_env_var("LANGUAGE", Some("ru_RU"));
        set_env_var("LANG", Some("xx_XX"));

        assert_eq!(Language::from_env(), None);

        restore_env_vars(snapshot);
    }

    fn env_test_lock() -> &'static Mutex<()> {
        static ENV_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_TEST_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn snapshot_env_vars() -> Vec<(String, Option<String>)> {
        ["FASTSYNC_LANG", "LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"]
            .into_iter()
            .map(|name| (name.to_string(), env::var(name).ok()))
            .collect()
    }

    fn restore_env_vars(snapshot: Vec<(String, Option<String>)>) {
        for (name, value) in snapshot {
            set_env_var(&name, value.as_deref());
        }
    }

    fn set_env_var(name: &str, value: Option<&str>) {
        match value {
            Some(value) => {
                // SAFETY: These tests serialize all mutations of the locale-related
                // environment variables through env_test_lock.
                unsafe { env::set_var(name, value) };
            }
            None => {
                // SAFETY: These tests serialize all mutations of the locale-related
                // environment variables through env_test_lock.
                unsafe { env::remove_var(name) };
            }
        }
    }

    #[test]
    fn parses_common_linux_locale_names() {
        assert_eq!(Language::parse("zh_CN.UTF-8"), Some(Language::ZhCn));
        assert_eq!(Language::parse("zh-CN.UTF-8"), Some(Language::ZhCn));
        assert_eq!(Language::parse("zh_Hans_CN.UTF-8"), Some(Language::ZhCn));
        assert_eq!(Language::parse("en_US.UTF-8"), Some(Language::En));
        assert_eq!(Language::parse("C.UTF-8"), Some(Language::En));
    }

    #[test]
    fn parses_language_priority_list() {
        assert_eq!(Language::parse("fr_FR:zh_CN:en_US"), Some(Language::ZhCn));
        assert_eq!(Language::parse("fr_FR:en_US"), Some(Language::En));
    }

    #[test]
    fn maps_windows_langid_to_supported_language() {
        assert_eq!(language_from_windows_langid(0x0804), Some(Language::ZhCn));
        assert_eq!(language_from_windows_langid(0x0404), Some(Language::ZhCn));
        assert_eq!(language_from_windows_langid(0x0409), Some(Language::En));
        assert_eq!(language_from_windows_langid(0x040c), None);
    }
}
