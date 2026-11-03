use crate::paths::{env_path, home_dir};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn default_env_file() -> PathBuf {
    env_path("TOKNIX_ENV_FILE")
        .or_else(|| env_path("CODEX_WIDGET_ENV_FILE"))
        .unwrap_or_else(|| home_dir().join(".env"))
}

pub fn load_env_file(path: &Path) -> HashMap<String, String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return HashMap::new();
    };
    parse_env(&content)
}

pub fn parse_env(content: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    for raw_line in content.lines() {
        let mut line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("export ") {
            line = rest.trim();
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if !is_identifier(name) {
            continue;
        }
        values.insert(name.to_string(), unquote(value.trim()));
    }
    values
}

pub fn resolve_secret(names: &[&str], env_file: &Path) -> Option<String> {
    for name in names {
        if let Ok(value) = std::env::var(name) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    resolve_from_file(names, &load_env_file(env_file))
}

#[cfg(test)]
pub fn resolve_from_maps(
    names: &[&str],
    environment: &HashMap<String, String>,
    file_values: &HashMap<String, String>,
) -> Option<String> {
    for name in names {
        if let Some(value) = environment.get(*name) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    resolve_from_file(names, file_values)
}

fn resolve_from_file(names: &[&str], file_values: &HashMap<String, String>) -> Option<String> {
    for name in names {
        if let Some(value) = file_values.get(*name) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[0] == bytes[bytes.len() - 1]
    {
        return value[1..value.len() - 1].to_string();
    }
    value.to_string()
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parses_shell_style_assignments() {
        let content = [
            "# openai",
            "OPENAI_ADMIN_API_KEY=sk-admin-plain",
            "QUOTED=\"sk-quoted\"",
            "SINGLE='sk-single'",
            "export EXPORTED=sk-exported",
            "export A B C",
            "SPACED = sk-spaced ",
            "",
            "OVERRIDDEN=first",
            "OVERRIDDEN=last",
            "not an assignment",
        ]
        .join("\n");
        let values = parse_env(&content);
        assert_eq!(values["OPENAI_ADMIN_API_KEY"], "sk-admin-plain");
        assert_eq!(values["QUOTED"], "sk-quoted");
        assert_eq!(values["SINGLE"], "sk-single");
        assert_eq!(values["EXPORTED"], "sk-exported");
        assert_eq!(values["SPACED"], "sk-spaced");
        assert_eq!(values["OVERRIDDEN"], "last");
        assert!(!values.contains_key("not an assignment"));
    }

    #[test]
    fn missing_file_yields_no_values() {
        let path = std::env::temp_dir().join("toknix-missing-env-file");
        assert!(load_env_file(&path).is_empty());
    }

    #[test]
    fn process_environment_wins_and_name_order_is_honored() {
        let file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file.as_file(), "OPENAI_ADMIN_API_KEY=from-file").unwrap();
        let values = load_env_file(file.path());
        let mut environment = HashMap::new();
        environment.insert("OPENAI_ADMIN_API_KEY".to_string(), "from-env".to_string());
        assert_eq!(
            resolve_from_maps(&["OPENAI_ADMIN_API_KEY"], &environment, &values).as_deref(),
            Some("from-env")
        );
        let empty = HashMap::new();
        assert_eq!(
            resolve_from_maps(
                &["OPENAI_ADMIN_KEY", "OPENAI_ADMIN_API_KEY"],
                &empty,
                &values
            )
            .as_deref(),
            Some("from-file")
        );
        assert_eq!(resolve_from_maps(&["ABSENT_NAME"], &empty, &values), None);
    }
}
