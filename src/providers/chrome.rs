use crate::error::{Failure, ProviderResult};
use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use cbc::Decryptor;
use pbkdf2::pbkdf2_hmac;
use rusqlite::{types::Value as SqlValue, Connection, OpenFlags};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

type Aes128CbcDec = Decryptor<aes::Aes128>;

const CHROME_EPOCH_OFFSET: f64 = 11_644_473_600.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromeCookie {
    pub name: String,
    pub value: String,
    pub host_key: String,
}

pub fn cookie_header(cookies: &[ChromeCookie]) -> String {
    cookies
        .iter()
        .map(|cookie| format!("{}={}", cookie.name, cookie.value))
        .collect::<Vec<_>>()
        .join("; ")
}

pub async fn decryption_keys() -> Vec<Vec<u8>> {
    let mut keys = Vec::new();
    if let Some(password) = safe_storage_password().await {
        keys.push(derive_key(&password, 1));
        keys.push(derive_key(&password, 1003));
    }
    keys.push(derive_key(b"peanuts", 1));
    keys
}

pub fn load_cookies(
    domain: &str,
    profile: Option<&str>,
    keys: &[Vec<u8>],
) -> ProviderResult<Vec<ChromeCookie>> {
    load_cookies_from(domain, profile, &default_roots(), keys)
}

pub fn load_cookies_from(
    domain: &str,
    profile: Option<&str>,
    roots: &[PathBuf],
    keys: &[Vec<u8>],
) -> ProviderResult<Vec<ChromeCookie>> {
    let database = find_cookie_database(domain, profile, roots)?;
    let rows = read_rows(&database, domain)?;
    if rows.is_empty() {
        return Err(Failure::new(format!(
            "Nenhum cookie de {domain} encontrado em {}",
            database.display()
        )));
    }
    let mut cookies = Vec::new();
    for (host_key, name, encrypted) in rows {
        let value = decrypt_cookie(&encrypted, &host_key, keys)?;
        cookies.push(ChromeCookie {
            name,
            value,
            host_key,
        });
    }
    Ok(cookies)
}

fn default_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(override_dir) = std::env::var_os("CHROME_USER_DATA_DIR") {
        roots.push(PathBuf::from(override_dir));
    }
    let home = crate::paths::home_dir();
    let mut candidates = vec![
        home.join(".config/google-chrome"),
        home.join(".config/google-chrome-beta"),
        home.join(".config/chromium"),
        home.join(".config/BraveSoftware/Brave-Browser"),
        home.join(".config/microsoft-edge"),
        home.join(".config/vivaldi"),
    ];
    candidates.extend(platform_cookie_roots(&home));
    roots.extend(candidates.into_iter().filter(|path| path.is_dir()));
    roots
}

fn platform_cookie_roots(home: &Path) -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        return vec![
            home.join("Library/Application Support/Google/Chrome"),
            home.join("Library/Application Support/Chromium"),
            home.join("Library/Application Support/BraveSoftware/Brave-Browser"),
            home.join("Library/Application Support/Microsoft Edge"),
        ];
    }
    #[cfg(target_os = "windows")]
    {
        return std::env::var_os("LOCALAPPDATA")
            .map(|local| vec![PathBuf::from(local).join("Google/Chrome/User Data")])
            .unwrap_or_default();
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = home;
        Vec::new()
    }
}

fn find_cookie_database(
    domain: &str,
    profile: Option<&str>,
    roots: &[PathBuf],
) -> ProviderResult<PathBuf> {
    let mut candidates = Vec::new();
    for root in roots {
        candidates.extend(cookie_files(root, profile));
    }
    if candidates.is_empty() {
        return Err(Failure::new(format!(
            "Banco de cookies do Chrome não encontrado; abra o Chrome e faça login em https://{domain}"
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
        let score = (count, modified);
        if best
            .as_ref()
            .is_none_or(|current| score > (current.0, current.1))
        {
            best = Some((count, modified, database));
        }
    }
    best.map(|(_, _, path)| path).ok_or_else(|| {
        Failure::new(format!(
            "Nenhum perfil do Chrome tem cookies de {domain}; faça login no console"
        ))
    })
}

fn cookie_files(root: &Path, profile: Option<&str>) -> Vec<PathBuf> {
    if let Some(profile) = profile {
        let path = root.join(profile).join("Cookies");
        return if path.is_file() {
            vec![path]
        } else {
            Vec::new()
        };
    }
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path().join("Cookies"))
        .filter(|path| path.is_file())
        .collect()
}

fn count_rows(database: &Path, domain: &str) -> ProviderResult<i64> {
    let (_directory, connection) = open_copied(database)?;
    let hosts = matching_hosts(domain);
    let placeholders = vec!["?"; hosts.len()].join(", ");
    let sql = format!(
        "SELECT COUNT(*) FROM cookies WHERE host_key IN ({placeholders}) AND (expires_utc = 0 OR expires_utc > ?)"
    );
    let mut params: Vec<SqlValue> = hosts.into_iter().map(SqlValue::Text).collect();
    params.push(SqlValue::Integer(chrome_now()));
    let count: i64 = connection
        .query_row(&sql, rusqlite::params_from_iter(params), |row| row.get(0))
        .map_err(|error| Failure::new(format!("Banco de cookies ilegível: {error}")))?;
    Ok(count)
}

fn read_rows(database: &Path, domain: &str) -> ProviderResult<Vec<(String, String, Vec<u8>)>> {
    let (_directory, connection) = open_copied(database)?;
    let hosts = matching_hosts(domain);
    let placeholders = vec!["?"; hosts.len()].join(", ");
    let sql = format!(
        "SELECT host_key, name, encrypted_value FROM cookies WHERE host_key IN ({placeholders}) AND (expires_utc = 0 OR expires_utc > ?) ORDER BY host_key, name"
    );
    let mut params: Vec<SqlValue> = hosts.into_iter().map(SqlValue::Text).collect();
    params.push(SqlValue::Integer(chrome_now()));
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| Failure::new(format!("Banco de cookies ilegível: {error}")))?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(params), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        })
        .map_err(|error| Failure::new(format!("Banco de cookies ilegível: {error}")))?;
    let mut cookies = Vec::new();
    for row in rows {
        let (host, name, value) =
            row.map_err(|error| Failure::new(format!("Banco de cookies ilegível: {error}")))?;
        if !value.is_empty() {
            cookies.push((host, name, value));
        }
    }
    Ok(cookies)
}

fn open_copied(source: &Path) -> ProviderResult<(tempfile::TempDir, Connection)> {
    let directory = tempfile::tempdir()
        .map_err(|error| Failure::new(format!("Não foi possível copiar os cookies: {error}")))?;
    let target = directory.path().join("Cookies");
    fs::copy(source, &target)
        .map_err(|error| Failure::new(format!("Não foi possível copiar os cookies: {error}")))?;
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = source.as_os_str().to_owned();
        sidecar.push(suffix);
        let sidecar = PathBuf::from(sidecar);
        if sidecar.is_file() {
            let mut copied = target.as_os_str().to_owned();
            copied.push(suffix);
            let _ = fs::copy(sidecar, copied);
        }
    }
    let connection = Connection::open_with_flags(&target, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| Failure::new(format!("Banco de cookies ilegível: {error}")))?;
    Ok((directory, connection))
}

pub fn matching_hosts(domain: &str) -> Vec<String> {
    let mut hosts = BTreeSet::new();
    hosts.insert(domain.to_string());
    hosts.insert(format!(".{domain}"));
    let parts: Vec<&str> = domain.split('.').collect();
    if parts.len() > 2 {
        let parent = parts[parts.len() - 2..].join(".");
        hosts.insert(format!(".{parent}"));
        hosts.insert(parent);
    }
    hosts.into_iter().collect()
}

fn chrome_now() -> i64 {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0);
    ((seconds + CHROME_EPOCH_OFFSET) * 1_000_000.0) as i64
}

fn derive_key(password: &[u8], iterations: u32) -> Vec<u8> {
    let mut key = vec![0u8; 16];
    pbkdf2_hmac::<Sha1>(password, b"saltysalt", iterations, &mut key);
    key
}

fn decrypt_cookie(encrypted: &[u8], host_key: &str, keys: &[Vec<u8>]) -> ProviderResult<String> {
    if encrypted.len() < 3 || (&encrypted[..3] != b"v10" && &encrypted[..3] != b"v11") {
        let prefix = &encrypted[..encrypted.len().min(3)];
        return Err(Failure::new(format!(
            "Formato de cookie não suportado para {host_key} (prefixo {})",
            python_bytes(prefix)
        )));
    }
    let host_hash = Sha256::digest(host_key.as_bytes());
    let mut plaintexts = Vec::new();
    for key in keys {
        let Ok(plaintext) = decrypt_cbc(key, &encrypted[3..]) else {
            continue;
        };
        if plaintext.starts_with(host_hash.as_slice()) && plaintext.len() > host_hash.len() {
            plaintexts.insert(0, plaintext[host_hash.len()..].to_vec());
        } else {
            plaintexts.push(plaintext);
        }
    }
    for plaintext in plaintexts {
        let Ok(mut value) = String::from_utf8(plaintext) else {
            continue;
        };
        if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            value = value[1..value.len() - 1].to_string();
        }
        return Ok(value);
    }
    Err(Failure::new(format!(
        "Não foi possível decifrar os cookies de {host_key}; refaça o login no Chrome"
    )))
}

fn decrypt_cbc(key: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, ()> {
    let decryptor = Aes128CbcDec::new_from_slices(key, b"                ").map_err(|_| ())?;
    decryptor
        .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
        .map_err(|_| ())
}

fn python_bytes(bytes: &[u8]) -> String {
    let mut out = String::from("b'");
    for byte in bytes {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'\'' => out.push_str("\\'"),
            0x20..=0x7e => out.push(*byte as char),
            _ => out.push_str(&format!("\\x{byte:02x}")),
        }
    }
    out.push('\'');
    out
}

async fn safe_storage_password() -> Option<Vec<u8>> {
    #[cfg(target_os = "linux")]
    {
        tokio::time::timeout(std::time::Duration::from_secs(60), linux_safe_storage())
            .await
            .ok()
            .flatten()
    }
    #[cfg(target_os = "macos")]
    {
        return keychain_secret();
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
async fn linux_safe_storage() -> Option<Vec<u8>> {
    use secret_service::{EncryptionType, SecretService};
    let service = SecretService::connect(EncryptionType::Dh).await.ok()?;
    let collection = service.get_default_collection().await.ok()?;
    if collection.is_locked().await.unwrap_or(false) && collection.unlock().await.is_err() {
        return None;
    }
    let items = collection.get_all_items().await.ok()?;
    let mut fallback = None;
    for item in &items {
        let label = item.get_label().await.unwrap_or_default();
        let attributes = item.get_attributes().await.unwrap_or_default();
        if attributes.get("application").map(String::as_str) == Some("chrome")
            && label.ends_with("Safe Storage")
        {
            if let Ok(secret) = item.get_secret().await {
                if !secret.is_empty() {
                    return Some(secret);
                }
            }
        }
        if label == "Chrome Safe Storage" && fallback.is_none() {
            fallback = item.get_secret().await.ok();
        }
    }
    fallback.filter(|secret| !secret.is_empty())
}

#[cfg(target_os = "macos")]
fn keychain_secret() -> Option<Vec<u8>> {
    let output = std::process::Command::new("security")
        .args(["find-generic-password", "-w", "-s", "Chrome Safe Storage"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut secret = output.stdout;
    while secret
        .last()
        .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
    {
        secret.pop();
    }
    if secret.is_empty() {
        None
    } else {
        Some(secret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
    use cbc::Encryptor;

    type Aes128CbcEnc = Encryptor<aes::Aes128>;
    const HOST: &str = "platform.xiaomimimo.com";

    fn encrypt(plaintext: &[u8], key: &[u8]) -> Vec<u8> {
        let encryptor = Aes128CbcEnc::new_from_slices(key, b"                ").unwrap();
        let mut encrypted = b"v10".to_vec();
        encrypted.extend(encryptor.encrypt_padded_vec_mut::<Pkcs7>(plaintext));
        encrypted
    }

    #[test]
    fn decrypts_domain_bound_and_legacy_cookies() {
        let key = derive_key(b"test-secret", 1);
        let mut bound = Sha256::digest(HOST.as_bytes()).to_vec();
        bound.extend_from_slice(b"session-token");
        assert_eq!(
            decrypt_cookie(&encrypt(&bound, &key), HOST, std::slice::from_ref(&key)).unwrap(),
            "session-token"
        );
        assert_eq!(
            decrypt_cookie(
                &encrypt(b"legacy-value", &key),
                HOST,
                std::slice::from_ref(&key)
            )
            .unwrap(),
            "legacy-value"
        );
        let mut quoted = Sha256::digest(HOST.as_bytes()).to_vec();
        quoted.extend_from_slice(b"\"quoted-value\"");
        assert_eq!(
            decrypt_cookie(&encrypt(&quoted, &key), HOST, std::slice::from_ref(&key)).unwrap(),
            "quoted-value"
        );
        let wrong = derive_key(b"wrong", 1);
        let mut token = Sha256::digest(HOST.as_bytes()).to_vec();
        token.extend_from_slice(b"token");
        assert_eq!(
            decrypt_cookie(&encrypt(&token, &key), HOST, &[wrong, key.clone()]).unwrap(),
            "token"
        );
        assert!(decrypt_cookie(&encrypt(&token, &key), HOST, &[derive_key(b"nope", 1)]).is_err());
        assert!(decrypt_cookie(b"v99\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0", HOST, &[key]).is_err());
    }

    #[test]
    fn joins_cookies_and_matches_parent_hosts() {
        let cookies = vec![
            ChromeCookie {
                name: "a".into(),
                value: "1".into(),
                host_key: HOST.into(),
            },
            ChromeCookie {
                name: "b".into(),
                value: "2".into(),
                host_key: HOST.into(),
            },
        ];
        assert_eq!(cookie_header(&cookies), "a=1; b=2");
        let hosts = matching_hosts(HOST);
        assert!(hosts.contains(&HOST.to_string()));
        assert!(hosts.contains(&format!(".{HOST}")));
        assert!(hosts.contains(&"xiaomimimo.com".to_string()));
        assert!(hosts.contains(&".xiaomimimo.com".to_string()));
        assert_eq!(
            matching_hosts("xiaomimimo.com"),
            vec![".xiaomimimo.com".to_string(), "xiaomimimo.com".to_string()]
        );
    }

    #[test]
    fn reads_the_profile_with_the_most_live_cookies() {
        let root = tempfile::tempdir().unwrap();
        let empty = root.path().join("Default");
        let full = root.path().join("Profile 1");
        fs::create_dir_all(&empty).unwrap();
        fs::create_dir_all(&full).unwrap();
        write_cookie_db(&empty.join("Cookies"), &[]);
        let key = derive_key(b"peanuts", 1);
        let encrypted = encrypt(b"session", &key);
        write_cookie_db(
            &full.join("Cookies"),
            &[("claude.ai", "sessionKey", &encrypted)],
        );
        let cookies =
            load_cookies_from("claude.ai", None, &[root.path().to_path_buf()], &[key]).unwrap();
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].value, "session");
    }

    fn write_cookie_db(path: &Path, rows: &[(&str, &str, &[u8])]) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE cookies (
                    host_key TEXT,
                    name TEXT,
                    encrypted_value BLOB,
                    expires_utc INTEGER
                );",
            )
            .unwrap();
        for (host, name, value) in rows {
            connection
                .execute(
                    "INSERT INTO cookies (host_key, name, encrypted_value, expires_utc) VALUES (?1, ?2, ?3, 0)",
                    (host, name, value),
                )
                .unwrap();
        }
    }
}
