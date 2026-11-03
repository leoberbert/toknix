use crate::jsonutil::decimal_opt;
use crate::paths::{config_home, env_path, expand_user};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WidgetConfig {
    pub deepseek_api_key: Option<String>,
    pub deepseek_base_url: Option<String>,
    pub deepseek_state_file: Option<PathBuf>,
    pub deepseek_spent_baseline: Option<Decimal>,
    pub mimo_chrome_profile: Option<String>,
    pub claude_chrome_profile: Option<String>,
    pub openai_admin_key: Option<String>,
    pub openai_credits_purchased: Option<Decimal>,
    pub openai_credits_since: Option<NaiveDate>,
}

pub fn default_config_path() -> PathBuf {
    resolve_config_path(
        env_path("TOKNIX_CONFIG"),
        env_path("CODEX_WIDGET_CONFIG"),
        &config_home(),
    )
}

pub fn resolve_config_path(
    toknix_override: Option<PathBuf>,
    legacy_override: Option<PathBuf>,
    config_home: &Path,
) -> PathBuf {
    if let Some(path) = toknix_override.or(legacy_override) {
        return path;
    }
    let current = config_home.join("toknix").join("config.json");
    let legacy = config_home.join("codex-widget").join("config.json");
    if current.is_file() {
        current
    } else if legacy.is_file() {
        legacy
    } else {
        current
    }
}

pub fn load_config(path: &Path) -> WidgetConfig {
    let Ok(text) = std::fs::read_to_string(path) else {
        return WidgetConfig::default();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return WidgetConfig::default();
    };
    let Some(raw) = value.as_object() else {
        return WidgetConfig::default();
    };
    WidgetConfig {
        deepseek_api_key: string_field(raw.get("deepseek_api_key")),
        deepseek_base_url: string_field(raw.get("deepseek_base_url")),
        deepseek_state_file: string_field(raw.get("deepseek_state_file"))
            .map(|text| expand_user(&text)),
        deepseek_spent_baseline: raw.get("deepseek_spent_baseline").and_then(decimal_opt),
        mimo_chrome_profile: string_field(raw.get("mimo_chrome_profile")),
        claude_chrome_profile: string_field(raw.get("claude_chrome_profile")),
        openai_admin_key: string_field(raw.get("openai_admin_key")),
        openai_credits_purchased: raw.get("openai_credits_purchased").and_then(decimal_opt),
        openai_credits_since: string_field(raw.get("openai_credits_since"))
            .and_then(|text| NaiveDate::parse_from_str(&text, "%Y-%m-%d").ok()),
    }
}

fn string_field(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn loads_known_fields_and_ignores_invalid_values() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            file.path(),
            r#"{
                "deepseek_api_key": "  sk-test  ",
                "deepseek_base_url": "https://example.test",
                "deepseek_state_file": "~/state.json",
                "deepseek_spent_baseline": "12.34",
                "mimo_chrome_profile": "Default",
                "claude_chrome_profile": "Profile 1",
                "openai_admin_key": "sk-admin",
                "openai_credits_purchased": "50.00",
                "openai_credits_since": "2026-09-01"
            }"#,
        )
        .unwrap();
        let config = load_config(file.path());
        assert_eq!(config.deepseek_api_key.as_deref(), Some("sk-test"));
        assert_eq!(
            config.deepseek_base_url.as_deref(),
            Some("https://example.test")
        );
        assert_eq!(
            config.deepseek_state_file,
            Some(expand_user("~/state.json"))
        );
        assert_eq!(
            config.deepseek_spent_baseline,
            Decimal::from_str("12.34").ok()
        );
        assert_eq!(config.mimo_chrome_profile.as_deref(), Some("Default"));
        assert_eq!(config.claude_chrome_profile.as_deref(), Some("Profile 1"));
        assert_eq!(
            config.openai_credits_since,
            NaiveDate::from_ymd_opt(2026, 9, 1)
        );
    }

    #[test]
    fn missing_corrupt_and_non_object_files_are_empty() {
        let missing = std::env::temp_dir().join("toknix-missing-config.json");
        assert_eq!(load_config(&missing), WidgetConfig::default());
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "{not json").unwrap();
        assert_eq!(load_config(file.path()), WidgetConfig::default());
        std::fs::write(file.path(), "[]").unwrap();
        assert_eq!(load_config(file.path()), WidgetConfig::default());
        std::fs::write(file.path(), r#"{"deepseek_spent_baseline": "abc"}"#).unwrap();
        assert_eq!(load_config(file.path()).deepseek_spent_baseline, None);
    }

    #[test]
    fn config_path_prefers_override_then_existing_legacy_file() {
        let root = tempfile::tempdir().unwrap();
        let override_path = root.path().join("custom.json");
        assert_eq!(
            resolve_config_path(Some(override_path.clone()), None, root.path()),
            override_path
        );
        let legacy_dir = root.path().join("codex-widget");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        let legacy = legacy_dir.join("config.json");
        std::fs::write(&legacy, "{}").unwrap();
        assert_eq!(resolve_config_path(None, None, root.path()), legacy);
        let current_dir = root.path().join("toknix");
        std::fs::create_dir_all(&current_dir).unwrap();
        let current = current_dir.join("config.json");
        std::fs::write(&current, "{}").unwrap();
        assert_eq!(resolve_config_path(None, None, root.path()), current);
    }
}
