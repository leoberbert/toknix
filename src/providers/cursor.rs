use crate::error::{Failure, ProviderResult};
use crate::http::{self, execute};
use crate::jsonutil::{float_opt, non_empty_str};
use crate::providers::codex::jwt_payload;
use crate::providers::MemoryCache;
use crate::settings::Settings;
#[cfg(any(target_os = "windows", test))]
use aes_gcm::aead::Aead;
#[cfg(any(target_os = "windows", test))]
use aes_gcm::{Aes256Gcm, KeyInit};
use base64::Engine;
use reqwest::Client;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(not(target_os = "windows"))]
use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
#[cfg(not(target_os = "windows"))]
use cbc::Decryptor;
#[cfg(not(target_os = "windows"))]
use pbkdf2::pbkdf2_hmac;
#[cfg(not(target_os = "windows"))]
use sha1::Sha1;
#[cfg(not(target_os = "windows"))]
type Aes128CbcDec = Decryptor<aes::Aes128>;

const PERIOD_PATH: &str = "/aiserver.v1.DashboardService/GetCurrentPeriodUsage";
const PLAN_PATH: &str = "/aiserver.v1.DashboardService/GetPlanInfo";
const TOKEN_PATH: &str = "/oauth/token";
const CLIENT_ID: &str = "KbZUR41cY7W6zRSdpSUJ7I7mLYBKOCmB";
const ACCESS_TOKEN_KEY: &str = "cursorAuth/accessToken";
const REFRESH_TOKEN_KEY: &str = "cursorAuth/refreshToken";
const MEMBERSHIP_KEY: &str = "cursorAuth/stripeMembershipType";
const LOGIN_HINT: &str = "Sessão do Cursor expirada; abra o Cursor e faça login";
const SAFE_STORAGE_PREFIX: &str = "cursor-safe-storage:v1:";
const ENCRYPTED_TOKEN: &str = "Não foi possível decifrar o token local do Cursor";

#[derive(Debug, Clone)]
struct CursorSession {
    access_token: String,
    refresh_token: Option<String>,
    plan: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CursorUsage {
    pub plan: Option<String>,
    pub total_percent_used: f64,
    pub api_percent_used: Option<f64>,
    pub spent_cents: i64,
    pub limit_cents: Option<i64>,
    pub cycle_end: Option<i64>,
}

pub async fn load(
    settings: &Settings,
    client: &Client,
    cache: &MemoryCache,
) -> ProviderResult<CursorUsage> {
    let database = settings.cursor_state_db.clone();
    let session = tokio::task::spawn_blocking(move || read_session(&database))
        .await
        .map_err(|error| Failure::new(format!("Falha ao ler a sessão do Cursor: {error}")))??;
    let mut token = cache
        .cached_cursor_token()
        .unwrap_or_else(|| session.access_token.clone());
    let mut refreshed = false;
    if cache.cached_cursor_token().is_none()
        && session.refresh_token.is_some()
        && jwt_expired(&token, None)
    {
        token = refresh_access_token(
            client,
            session.refresh_token.as_deref().unwrap_or_default(),
            &settings.cursor_base_url,
        )
        .await?;
        cache.store_cursor_token(token.clone());
        refreshed = true;
    }
    let period = match post_json(client, &settings.cursor_base_url, PERIOD_PATH, &token).await {
        Err(error) if error.is_auth() && !refreshed && session.refresh_token.is_some() => {
            token = refresh_access_token(
                client,
                session.refresh_token.as_deref().unwrap_or_default(),
                &settings.cursor_base_url,
            )
            .await?;
            cache.store_cursor_token(token.clone());
            post_json(client, &settings.cursor_base_url, PERIOD_PATH, &token).await?
        }
        other => other?,
    };
    let mut plan = session.plan;
    if let Ok(info) = post_json(client, &settings.cursor_base_url, PLAN_PATH, &token).await {
        if let Some(name) = parse_plan_name(&info) {
            plan = Some(name);
        }
    }
    parse_period(&period, plan)
}

fn read_session(state_db: &Path) -> ProviderResult<CursorSession> {
    if !state_db.is_file() {
        return Err(Failure::new(format!(
            "Banco do Cursor não encontrado em {}",
            state_db.display()
        )));
    }
    let connection = Connection::open_with_flags(state_db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| {
            Failure::new(format!(
                "Não foi possível abrir {}: {error}",
                state_db.display()
            ))
        })?;
    let mut statement = connection
        .prepare("SELECT key, value FROM ItemTable WHERE key IN (?1, ?2, ?3)")
        .map_err(|error| Failure::new(format!("Banco do Cursor ilegível: {error}")))?;
    let rows = statement
        .query_map(
            [ACCESS_TOKEN_KEY, REFRESH_TOKEN_KEY, MEMBERSHIP_KEY],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, rusqlite::types::Value>(1)?,
                ))
            },
        )
        .map_err(|error| Failure::new(format!("Banco do Cursor ilegível: {error}")))?;
    let mut access_token = None;
    let mut refresh_token = None;
    let mut plan = None;
    for row in rows {
        let (key, value) =
            row.map_err(|error| Failure::new(format!("Banco do Cursor ilegível: {error}")))?;
        let text = token_text(&value);
        match key.as_str() {
            ACCESS_TOKEN_KEY => access_token = text,
            REFRESH_TOKEN_KEY => refresh_token = text,
            MEMBERSHIP_KEY => plan = text.as_deref().and_then(plan_label),
            _ => {}
        }
    }
    let access_token = reveal_token(&access_token.ok_or_else(|| Failure::new(LOGIN_HINT))?)?;
    let refresh_token = refresh_token.and_then(|token| reveal_token(&token).ok());
    Ok(CursorSession {
        access_token,
        refresh_token,
        plan,
    })
}

fn reveal_token(stored: &str) -> ProviderResult<String> {
    let Some(payload) = stored.strip_prefix(SAFE_STORAGE_PREFIX) else {
        return Ok(stored.to_string());
    };
    let blob = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|_| Failure::new(ENCRYPTED_TOKEN))?;
    decrypt_safe_storage(&blob)
}

fn decrypt_safe_storage(blob: &[u8]) -> ProviderResult<String> {
    #[cfg(target_os = "windows")]
    {
        decrypt_windows_safe_storage(blob)
    }
    #[cfg(target_os = "macos")]
    {
        decrypt_macos_safe_storage(blob)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        decrypt_linux_safe_storage(blob)
    }
}

#[cfg(any(target_os = "windows", test))]
fn decrypt_v10_gcm(blob: &[u8], key: &[u8]) -> Result<Vec<u8>, ()> {
    if blob.len() < 3 + 12 + 16 || &blob[..3] != b"v10" {
        return Err(());
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| ())?;
    let nonce = aes_gcm::Nonce::try_from(&blob[3..15]).map_err(|_| ())?;
    cipher.decrypt(&nonce, &blob[15..]).map_err(|_| ())
}

#[cfg(not(target_os = "windows"))]
fn decrypt_v10_cbc(blob: &[u8], key: &[u8]) -> Result<Vec<u8>, ()> {
    if blob.len() <= 3 || &blob[..3] != b"v10" {
        return Err(());
    }
    let decryptor = Aes128CbcDec::new_from_slices(key, b"                ").map_err(|_| ())?;
    decryptor
        .decrypt_padded_vec_mut::<Pkcs7>(&blob[3..])
        .map_err(|_| ())
}

#[cfg(not(target_os = "windows"))]
fn derive_safe_storage_key(password: &[u8], iterations: u32) -> Vec<u8> {
    let mut key = vec![0u8; 16];
    pbkdf2_hmac::<Sha1>(password, b"saltysalt", iterations, &mut key);
    key
}

fn utf8_secret(bytes: Vec<u8>) -> ProviderResult<String> {
    String::from_utf8(bytes)
        .map(|text| text.trim().to_string())
        .map_err(|_| Failure::new(ENCRYPTED_TOKEN))
        .and_then(|text| {
            if text.is_empty() {
                Err(Failure::new(ENCRYPTED_TOKEN))
            } else {
                Ok(text)
            }
        })
}

#[cfg(target_os = "windows")]
fn decrypt_windows_safe_storage(blob: &[u8]) -> ProviderResult<String> {
    if !blob.starts_with(b"v10") {
        return Err(Failure::new(ENCRYPTED_TOKEN));
    }
    let key = windows_os_crypt_key()?;
    utf8_secret(decrypt_v10_gcm(blob, &key).map_err(|_| Failure::new(ENCRYPTED_TOKEN))?)
}

#[cfg(target_os = "windows")]
fn windows_os_crypt_key() -> ProviderResult<Vec<u8>> {
    let path = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| crate::paths::home_dir().join("AppData").join("Roaming"))
        .join("Cursor")
        .join("Local State");
    let text = std::fs::read_to_string(&path).map_err(|_| Failure::new(ENCRYPTED_TOKEN))?;
    let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|_| Failure::new(ENCRYPTED_TOKEN))?;
    let encoded = value
        .get("os_crypt")
        .and_then(|item| item.get("encrypted_key"))
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::new(ENCRYPTED_TOKEN))?;
    let protected = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| Failure::new(ENCRYPTED_TOKEN))?;
    let blob = protected
        .strip_prefix(b"DPAPI")
        .ok_or_else(|| Failure::new(ENCRYPTED_TOKEN))?;
    let key = windows_dpapi::decrypt_data(blob, windows_dpapi::Scope::User, None)
        .map_err(|_| Failure::new(ENCRYPTED_TOKEN))?;
    if key.len() != 32 {
        return Err(Failure::new(ENCRYPTED_TOKEN));
    }
    Ok(key)
}

#[cfg(target_os = "macos")]
fn decrypt_macos_safe_storage(blob: &[u8]) -> ProviderResult<String> {
    let password = macos_safe_storage_password().ok_or_else(|| Failure::new(ENCRYPTED_TOKEN))?;
    let key = derive_safe_storage_key(&password, 1003);
    utf8_secret(decrypt_v10_cbc(blob, &key).map_err(|_| Failure::new(ENCRYPTED_TOKEN))?)
}

#[cfg(target_os = "macos")]
fn macos_safe_storage_password() -> Option<Vec<u8>> {
    let attempts: [&[&str]; 2] = [
        &[
            "find-generic-password",
            "-w",
            "-s",
            "Cursor Safe Storage",
            "-a",
            "Cursor",
        ],
        &["find-generic-password", "-w", "-s", "Cursor Safe Storage"],
    ];
    for args in attempts {
        let Ok(output) = std::process::Command::new("security").args(args).output() else {
            return None;
        };
        if !output.status.success() {
            continue;
        }
        let mut secret = output.stdout;
        while secret
            .last()
            .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
        {
            secret.pop();
        }
        if !secret.is_empty() {
            return Some(secret);
        }
    }
    None
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn decrypt_linux_safe_storage(blob: &[u8]) -> ProviderResult<String> {
    let key = derive_safe_storage_key(b"peanuts", 1);
    utf8_secret(decrypt_v10_cbc(blob, &key).map_err(|_| Failure::new(ENCRYPTED_TOKEN))?)
}

pub async fn refresh_access_token(
    client: &Client,
    refresh_token: &str,
    base_url: &str,
) -> ProviderResult<String> {
    let fetched = execute(
        client
            .post(format!("{}{TOKEN_PATH}", base_url.trim_end_matches('/')))
            .json(&serde_json::json!({
                "grant_type": "refresh_token",
                "client_id": CLIENT_ID,
                "refresh_token": refresh_token
            }))
            .header("Accept", "application/json"),
    )
    .await
    .map_err(|error| {
        http::transport_failure(
            error,
            "Falha ao consultar o uso do Cursor",
            "Falha ao consultar o uso do Cursor",
        )
    })?;
    interpret_json(fetched, true).and_then(|payload| {
        if payload.get("shouldLogout") == Some(&Value::Bool(true)) {
            return Err(Failure::new(LOGIN_HINT));
        }
        non_empty_str(payload.get("access_token").unwrap_or(&Value::Null))
            .map(str::to_string)
            .ok_or_else(|| Failure::new(LOGIN_HINT))
    })
}

pub fn parse_plan_name(payload: &Value) -> Option<String> {
    non_empty_str(
        payload
            .get("planInfo")
            .and_then(|info| info.get("planName"))
            .unwrap_or(&Value::Null),
    )
    .map(str::to_string)
}

pub fn parse_period(payload: &Value, plan: Option<String>) -> ProviderResult<CursorUsage> {
    let plan_usage = payload
        .get("planUsage")
        .and_then(Value::as_object)
        .ok_or_else(|| Failure::new("Resposta de uso do Cursor sem o plano atual"))?;
    let limit = plan_usage.get("limit").and_then(cents);
    let spent = plan_usage
        .get("totalSpend")
        .and_then(cents)
        .or_else(|| plan_usage.get("includedSpend").and_then(cents))
        .unwrap_or(0);
    let mut total = plan_usage.get("totalPercentUsed").and_then(float_opt);
    if total.is_none() {
        if let Some(limit) = limit.filter(|value| *value != 0) {
            total = Some(spent as f64 / limit as f64 * 100.0);
        }
    }
    let total =
        total.ok_or_else(|| Failure::new("Resposta de uso do Cursor sem percentual do plano"))?;
    Ok(CursorUsage {
        plan,
        total_percent_used: total,
        api_percent_used: plan_usage.get("apiPercentUsed").and_then(float_opt),
        spent_cents: spent,
        limit_cents: limit,
        cycle_end: epoch_seconds(payload.get("billingCycleEnd")),
    })
}

pub fn jwt_expired(token: &str, now: Option<f64>) -> bool {
    let Some(payload) = jwt_payload(token) else {
        return false;
    };
    let Some(exp) = payload.get("exp").and_then(float_opt) else {
        return false;
    };
    let current = now.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs_f64())
            .unwrap_or(0.0)
    });
    exp <= current + 60.0
}

async fn post_json(
    client: &Client,
    base_url: &str,
    path: &str,
    access_token: &str,
) -> ProviderResult<Value> {
    let fetched = execute(
        client
            .post(format!("{}{path}", base_url.trim_end_matches('/')))
            .bearer_auth(access_token)
            .header("Content-Type", "application/json")
            .header("Connect-Protocol-Version", "1")
            .header("Accept", "application/json")
            .body("{}"),
    )
    .await
    .map_err(|error| {
        http::transport_failure(
            error,
            "Falha ao consultar o uso do Cursor",
            "Falha ao consultar o uso do Cursor",
        )
    })?;
    interpret_json(fetched, true)
}

fn interpret_json(fetched: crate::http::Fetched, auth_failure: bool) -> ProviderResult<Value> {
    if !fetched.status.is_success() {
        if auth_failure && (fetched.status.as_u16() == 401 || fetched.status.as_u16() == 403) {
            return Err(Failure::auth(LOGIN_HINT));
        }
        return Err(Failure::new(format!(
            "Uso do Cursor indisponível (HTTP {})",
            fetched.status.as_u16()
        )));
    }
    serde_json::from_str(&fetched.body)
        .map_err(|_| Failure::new("Resposta de uso do Cursor não é JSON válido"))
}

fn token_text(value: &rusqlite::types::Value) -> Option<String> {
    let text = match value {
        rusqlite::types::Value::Text(text) => text.clone(),
        rusqlite::types::Value::Blob(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        _ => return None,
    };
    let mut text = text.trim().to_string();
    if text.len() >= 2 && text.starts_with('"') && text.ends_with('"') {
        text = match serde_json::from_str::<String>(&text) {
            Ok(loaded) => loaded.trim().to_string(),
            Err(_) => text[1..text.len() - 1].trim().to_string(),
        };
    }
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn plan_label(value: &str) -> Option<String> {
    let label = value.replace(['_', '-'], " ");
    let label = label.split_whitespace().collect::<Vec<_>>().join(" ");
    if label.is_empty() {
        return None;
    }
    Some(
        label
            .split_whitespace()
            .map(|word| {
                let mut chars = word.chars();
                match chars.next() {
                    Some(first) => {
                        first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                    }
                    None => String::new(),
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn cents(value: &Value) -> Option<i64> {
    float_opt(value).map(|number| number.round() as i64)
}

fn epoch_seconds(value: Option<&Value>) -> Option<i64> {
    let mut number = float_opt(value?)?;
    if number <= 0.0 {
        return None;
    }
    if number > 10_000_000_000.0 {
        number /= 1000.0;
    }
    Some(number as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::build_cursor_card;
    use base64::Engine;
    use chrono::{DateTime, Local};
    use rusqlite::Connection;
    use serde_json::json;

    fn period() -> Value {
        json!({
            "billingCycleEnd": "1800000000000",
            "planUsage": {
                "totalSpend": 23222,
                "includedSpend": 23222,
                "limit": 40000,
                "apiPercentUsed": 46.444,
                "totalPercentUsed": 15.48
            }
        })
    }

    #[test]
    fn parses_period_and_builds_the_card() {
        let usage = parse_period(&period(), Some("Pro".to_string())).unwrap();
        assert!((usage.total_percent_used - 15.48).abs() < 1e-9);
        assert!((usage.api_percent_used.unwrap() - 46.444).abs() < 1e-9);
        assert_eq!(usage.spent_cents, 23222);
        assert_eq!(usage.limit_cents, Some(40000));
        assert_eq!(usage.cycle_end, Some(1_800_000_000));

        let mut derived = period();
        derived["planUsage"]
            .as_object_mut()
            .unwrap()
            .remove("totalPercentUsed");
        derived["planUsage"]
            .as_object_mut()
            .unwrap()
            .remove("apiPercentUsed");
        let derived = parse_period(&derived, None).unwrap();
        assert!((derived.total_percent_used - 58.055).abs() < 0.001);
        assert!(derived.api_percent_used.is_none());
        assert!(parse_period(&json!({}), None).is_err());

        let card = build_cursor_card(&usage);
        let renewal = DateTime::from_timestamp(1_800_000_000, 0)
            .unwrap()
            .with_timezone(&Local);
        assert_eq!(card.primary_value, "85% restantes");
        assert_eq!(card.primary_percent, Some(85));
        assert_eq!(card.secondary_value, "54% restantes");
        assert_eq!(card.secondary_percent, Some(54));
        assert_eq!(card.detail, "Plano Pro");
        assert_eq!(
            card.footnote,
            format!(
                "US$ 232,22 de US$ 400,00 · renova {}",
                renewal.format("%d/%m")
            )
        );
        assert!(card.available);
    }

    #[test]
    fn reads_quoted_session_values_and_detects_expiry() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT);
                 INSERT INTO ItemTable (key, value) VALUES
                    ('cursorAuth/accessToken', '\"access-1\"'),
                    ('cursorAuth/refreshToken', 'refresh-1'),
                    ('cursorAuth/stripeMembershipType', 'pro_plus');",
            )
            .unwrap();
        drop(connection);
        let session = read_session(file.path()).unwrap();
        assert_eq!(session.access_token, "access-1");
        assert_eq!(session.refresh_token.as_deref(), Some("refresh-1"));
        assert_eq!(session.plan.as_deref(), Some("Pro Plus"));

        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"exp":100}"#);
        assert!(jwt_expired(&format!("{header}.{payload}.sig"), Some(200.0)));
        assert!(!jwt_expired("not-a-jwt", Some(200.0)));
    }

    #[test]
    fn decrypts_a_windows_safe_storage_blob_and_keeps_plaintext_tokens() {
        assert_eq!(reveal_token("access-1").unwrap(), "access-1");
        let key = [9u8; 32];
        let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
        let nonce = aes_gcm::Nonce::try_from(b"0123456789ab".as_slice()).unwrap();
        let mut blob = b"v10".to_vec();
        blob.extend_from_slice(&nonce);
        blob.extend(cipher.encrypt(&nonce, b"access-1".as_slice()).unwrap());
        assert_eq!(decrypt_v10_gcm(&blob, &key).unwrap(), b"access-1");
        assert!(decrypt_v10_gcm(&blob, &[1u8; 32]).is_err());

        let stored = format!(
            "{SAFE_STORAGE_PREFIX}{}",
            base64::engine::general_purpose::STANDARD.encode(blob)
        );
        assert!(reveal_token(&stored).is_err());
        assert!(reveal_token("cursor-safe-storage:v1:not-base64").is_err());
    }
}
