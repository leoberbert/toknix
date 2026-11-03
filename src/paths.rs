use std::path::{Path, PathBuf};

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn expand_user(path: &str) -> PathBuf {
    let path = path.trim();
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        return home_dir().join(rest);
    }
    PathBuf::from(path)
}

pub fn expand_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text.starts_with('~') {
        expand_user(&text)
    } else {
        path.to_path_buf()
    }
}

fn xdg_dir(var: &str, fallback: &[&str]) -> PathBuf {
    if let Some(value) = std::env::var_os(var) {
        let path = PathBuf::from(value);
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    let mut path = home_dir();
    for part in fallback {
        path.push(part);
    }
    path
}

pub fn config_home() -> PathBuf {
    xdg_dir("XDG_CONFIG_HOME", &[".config"])
}

pub fn state_home() -> PathBuf {
    xdg_dir("XDG_STATE_HOME", &[".local", "state"])
}

pub fn data_home() -> PathBuf {
    xdg_dir("XDG_DATA_HOME", &[".local", "share"])
}

pub fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name).ok().and_then(|value| {
        let value = value.trim();
        if value.is_empty() {
            None
        } else {
            Some(expand_user(value))
        }
    })
}
