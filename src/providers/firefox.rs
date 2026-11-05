use crate::error::{Failure, ProviderResult};
use crate::paths;
use crate::providers::chrome::{self, ChromeCookie};
use rusqlite::types::Value as SqlValue;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const DATABASE: &str = "cookies.sqlite";

pub fn load_cookies(domain: &str) -> ProviderResult<Vec<ChromeCookie>> {
    load_cookies_from(domain, &default_roots())
}

pub fn load_cookies_from(domain: &str, roots: &[PathBuf]) -> ProviderResult<Vec<ChromeCookie>> {
    let database = find_cookie_database(domain, roots)?;
    let rows = read_rows(&database, domain)?;
    Ok(rows
        .into_iter()
        .map(|(host_key, name, value)| ChromeCookie {
            name,
            value,
            host_key,
        })
        .collect())
}

fn default_roots() -> Vec<PathBuf> {
    platform_profile_roots(&paths::home_dir())
        .into_iter()
        .filter(|path| path.is_dir())
        .collect()
}

#[cfg(target_os = "linux")]
fn platform_profile_roots(home: &Path) -> Vec<PathBuf> {
    vec![
        paths::config_home().join("mozilla/firefox"),
        home.join(".mozilla/firefox"),
        home.join("snap/firefox/common/.mozilla/firefox"),
        home.join(".var/app/org.mozilla.firefox/config/mozilla/firefox"),
        home.join(".var/app/org.mozilla.firefox/.mozilla/firefox"),
    ]
}

#[cfg(target_os = "windows")]
fn platform_profile_roots(_home: &Path) -> Vec<PathBuf> {
    std::env::var_os("APPDATA")
        .map(|appdata| vec![PathBuf::from(appdata).join("Mozilla/Firefox/Profiles")])
        .unwrap_or_default()
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn platform_profile_roots(_home: &Path) -> Vec<PathBuf> {
    Vec::new()
}

fn find_cookie_database(domain: &str, roots: &[PathBuf]) -> ProviderResult<PathBuf> {
    let candidates: Vec<PathBuf> = roots
        .iter()
        .flat_map(|root| profile_databases(root))
        .collect();
    if candidates.is_empty() {
        return Err(Failure::new(format!(
            "Banco de cookies do Firefox não encontrado; abra o Firefox e faça login em https://{domain}"
        )));
    }
    let mut best: Option<(i64, i64, PathBuf)> = None;
    for database in candidates {
        let Ok(count) = count_rows(&database, domain) else {
            continue;
        };
        if count == 0 {
            continue;
        }
        let modified = fs::metadata(&database)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        if best
            .as_ref()
            .is_none_or(|current| (count, modified) > (current.0, current.1))
        {
            best = Some((count, modified, database));
        }
    }
    best.map(|(_, _, path)| path).ok_or_else(|| {
        Failure::new(format!(
            "Nenhum perfil do Firefox tem cookies de {domain}; faça login no site"
        ))
    })
}

fn profile_databases(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path().join(DATABASE))
        .filter(|path| path.is_file())
        .collect()
}

fn count_rows(database: &Path, domain: &str) -> ProviderResult<i64> {
    let (_directory, connection) = chrome::open_copied(database)?;
    let (sql, params) = cookie_query(domain, "COUNT(*)");
    connection
        .query_row(&sql, rusqlite::params_from_iter(params), |row| row.get(0))
        .map_err(unreadable)
}

fn read_rows(database: &Path, domain: &str) -> ProviderResult<Vec<(String, String, String)>> {
    let (_directory, connection) = chrome::open_copied(database)?;
    let (sql, params) = cookie_query(domain, "host, name, value");
    let mut statement = connection.prepare(&sql).map_err(unreadable)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(params), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(unreadable)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(unreadable)
}

fn cookie_query(domain: &str, columns: &str) -> (String, Vec<SqlValue>) {
    let hosts = chrome::matching_hosts(domain);
    let placeholders = vec!["?"; hosts.len()].join(", ");
    let sql = format!(
        "SELECT {columns} FROM moz_cookies WHERE host IN ({placeholders}) AND value <> '' AND (expiry = 0 OR expiry > ?)"
    );
    let mut params: Vec<SqlValue> = hosts.into_iter().map(SqlValue::Text).collect();
    params.push(SqlValue::Integer(now_millis()));
    (sql, params)
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn unreadable(error: rusqlite::Error) -> Failure {
    Failure::new(format!("Banco de cookies do Firefox ilegível: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn write_profile(root: &Path, name: &str, rows: &[(&str, &str, &str, i64)]) {
        let profile = root.join(name);
        fs::create_dir_all(&profile).unwrap();
        let connection = Connection::open(profile.join(DATABASE)).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE moz_cookies (id INTEGER PRIMARY KEY, name TEXT, value TEXT, host TEXT, expiry INTEGER);",
            )
            .unwrap();
        for (host, cookie_name, value, expiry) in rows {
            connection
                .execute(
                    "INSERT INTO moz_cookies (name, value, host, expiry) VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![cookie_name, value, host, expiry],
                )
                .unwrap();
        }
    }

    #[test]
    fn reads_plain_cookies_for_domain_and_skips_expired_or_empty_rows() {
        let root = tempfile::tempdir().unwrap();
        let future = now_millis() + 3_600_000;
        write_profile(
            root.path(),
            "abc.default",
            &[
                (".claude.ai", "sessionKey", "secret", future),
                ("claude.ai", "lastActive", "", future),
                (".claude.ai", "old", "expired", now_millis() - 10_000),
                (".example.com", "other", "value", future),
            ],
        );

        let cookies = load_cookies_from("claude.ai", &[root.path().to_path_buf()]).unwrap();

        assert_eq!(
            cookies,
            vec![ChromeCookie {
                name: "sessionKey".to_string(),
                value: "secret".to_string(),
                host_key: ".claude.ai".to_string(),
            }]
        );
    }

    #[test]
    fn prefers_the_profile_with_more_cookies_for_the_domain() {
        let root = tempfile::tempdir().unwrap();
        let future = now_millis() + 3_600_000;
        write_profile(root.path(), "small", &[(".claude.ai", "a", "1", future)]);
        write_profile(
            root.path(),
            "big",
            &[
                (".claude.ai", "a", "2", future),
                (".claude.ai", "b", "3", future),
            ],
        );

        let cookies = load_cookies_from("claude.ai", &[root.path().to_path_buf()]).unwrap();

        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].value, "2");
    }

    #[test]
    fn reports_missing_profiles_as_firefox_error() {
        let root = tempfile::tempdir().unwrap();

        let error = load_cookies_from("claude.ai", &[root.path().to_path_buf()]).unwrap_err();

        assert!(error.to_string().contains("Firefox"));
    }
}
