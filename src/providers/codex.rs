use crate::error::{Failure, ProviderResult};
use crate::http::{self, execute};
use crate::jsonutil::{float_opt, int_opt, non_empty_str, truncate};
use crate::settings::Settings;
use base64::Engine;
use chrono::{SecondsFormat, Utc};
use reqwest::Client;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const OAUTH_SCOPE: &str = "openid profile email offline_access";
pub const WEEKLY_WINDOW_SECONDS: i64 = 604_800;

#[derive(Debug, Clone, PartialEq)]
pub struct UsageWindow {
    pub used_percent: Option<f64>,
    pub reset_at: Option<i64>,
    pub window_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexUsage {
    pub short_window: Option<UsageWindow>,
    pub weekly: Option<UsageWindow>,
    pub plan_type: Option<String>,
}

pub async fn load(settings: &Settings, client: &Client) -> ProviderResult<CodexUsage> {
    let payload = fetch_with_refresh(settings, client).await?;
    Ok(parse_usage_payload(&payload, None))
}

pub fn parse_usage_payload(payload: &Value, now: Option<i64>) -> CodexUsage {
    let now_epoch = now.unwrap_or_else(|| Utc::now().timestamp());
    let plan_type =
        non_empty_str(payload.get("plan_type").unwrap_or(&Value::Null)).map(str::to_string);
    let Some(rate_limit) = payload.get("rate_limit").and_then(Value::as_object) else {
        return CodexUsage {
            short_window: None,
            weekly: None,
            plan_type,
        };
    };
    let primary = parse_window(rate_limit.get("primary_window"), now_epoch);
    let secondary = parse_window(rate_limit.get("secondary_window"), now_epoch);
    let mut short_window = None;
    let mut weekly = None;
    for window in [&primary, &secondary] {
        let Some(window) = window.clone() else {
            continue;
        };
        if window.window_seconds == Some(WEEKLY_WINDOW_SECONDS) {
            if weekly.is_none() {
                weekly = Some(window);
            }
        } else if short_window.is_none() {
            short_window = Some(window);
        }
    }
    if short_window.is_none() {
        if let Some(window) = primary.clone() {
            if window.window_seconds != Some(WEEKLY_WINDOW_SECONDS) {
                short_window = Some(window);
            }
        }
    }
    if weekly.is_none() {
        weekly = secondary;
    }
    CodexUsage {
        short_window,
        weekly,
        plan_type,
    }
}

async fn fetch_with_refresh(settings: &Settings, client: &Client) -> ProviderResult<Value> {
    let auth = load_auth(&settings.auth_file)?;
    match fetch_usage(client, &auth, &settings.codex_base_url).await {
        Ok(payload) => Ok(payload),
        Err(error) if error.is_auth() => {
            let refreshed = refresh_auth(client, &auth, settings).await?;
            fetch_usage(client, &refreshed, &settings.codex_base_url).await
        }
        Err(error) => Err(error),
    }
}

async fn fetch_usage(client: &Client, auth: &Value, base_url: &str) -> ProviderResult<Value> {
    let token = extract_access_token(auth)?;
    let account_id = extract_account_id(auth)?;
    let url = format!("{}/wham/usage", base_url.trim_end_matches('/'));
    let fetched = execute(
        client
            .get(url)
            .bearer_auth(token)
            .header("chatgpt-account-id", account_id)
            .header("Accept", "application/json"),
    )
    .await
    .map_err(|error| {
        http::transport_failure(
            error,
            "Timed out while fetching usage",
            "Network error while fetching usage",
        )
    })?;
    if !fetched.status.is_success() {
        let detail = error_detail(fetched.status, &fetched.body);
        let message = format!(
            "HTTP {} while fetching usage: {detail}",
            fetched.status.as_u16()
        );
        if fetched.status.as_u16() == 401 || fetched.status.as_u16() == 403 {
            return Err(Failure::auth(message));
        }
        return Err(Failure::new(message));
    }
    parse_object(&fetched.body, "Usage response")
}

async fn refresh_auth(client: &Client, auth: &Value, settings: &Settings) -> ProviderResult<Value> {
    let tokens = auth.get("tokens").and_then(Value::as_object);
    let refresh_token = tokens
        .and_then(|tokens| non_empty_str(tokens.get("refresh_token").unwrap_or(&Value::Null)))
        .ok_or_else(|| {
            Failure::new("Usage request was unauthorized and auth.json has no refresh_token")
        })?;
    let url = format!(
        "{}/oauth/token",
        settings.auth_base_url.trim_end_matches('/')
    );
    let fetched = execute(client.post(url).form(&[
        ("grant_type", "refresh_token"),
        ("client_id", OAUTH_CLIENT_ID),
        ("refresh_token", refresh_token),
        ("scope", OAUTH_SCOPE),
    ]))
    .await
    .map_err(|error| {
        http::transport_failure(
            error,
            "Timed out while refreshing auth",
            "Network error while refreshing auth",
        )
    })?;
    if !fetched.status.is_success() {
        let detail = error_detail(fetched.status, &fetched.body);
        return Err(Failure::new(format!(
            "Auth refresh failed with HTTP {}: {detail}",
            fetched.status.as_u16()
        )));
    }
    let data = parse_object(&fetched.body, "Auth refresh response")?;
    let access_token = token_value(&data, &["access_token", "accessToken"])
        .ok_or_else(|| Failure::new("Auth refresh response did not include required tokens"))?;
    let id_token = token_value(&data, &["id_token", "idToken"])
        .ok_or_else(|| Failure::new("Auth refresh response did not include required tokens"))?;
    let next_refresh = token_value(&data, &["refresh_token", "refreshToken"])
        .unwrap_or_else(|| refresh_token.to_string());
    let mut next_auth = auth.clone();
    let next_tokens = next_auth
        .as_object_mut()
        .ok_or_else(|| Failure::new("Auth file does not contain a JSON object"))?
        .entry("tokens")
        .or_insert_with(|| json!({}));
    let token_map = next_tokens
        .as_object_mut()
        .ok_or_else(|| Failure::new("Auth file tokens are not an object"))?;
    token_map.insert("access_token".to_string(), Value::String(access_token));
    token_map.insert("refresh_token".to_string(), Value::String(next_refresh));
    token_map.insert("id_token".to_string(), Value::String(id_token.clone()));
    if let Some(account_id) = account_id_from_id_token(&id_token) {
        token_map.insert("account_id".to_string(), Value::String(account_id));
    }
    next_auth["last_refresh"] =
        Value::String(Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true));
    write_auth(&settings.auth_file, &next_auth)?;
    Ok(next_auth)
}

pub fn load_auth(path: &Path) -> ProviderResult<Value> {
    let text = fs::read_to_string(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Failure::new(format!("Auth file not found: {}", path.display()))
        } else {
            Failure::new(format!("Invalid auth file: {}", path.display()))
        }
    })?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|_| Failure::new(format!("Invalid auth file: {}", path.display())))?;
    if !value.is_object() {
        return Err(Failure::new("Auth file does not contain a JSON object"));
    }
    Ok(value)
}

pub fn extract_access_token(auth: &Value) -> ProviderResult<String> {
    let tokens = auth.get("tokens").and_then(Value::as_object);
    if let Some(tokens) = tokens {
        for key in ["access_token", "accessToken"] {
            if let Some(value) = non_empty_str(tokens.get(key).unwrap_or(&Value::Null)) {
                return Ok(value.to_string());
            }
        }
    }
    Err(Failure::new(
        "Could not find tokens.access_token in auth.json",
    ))
}

pub fn extract_account_id(auth: &Value) -> ProviderResult<String> {
    let tokens = auth.get("tokens").and_then(Value::as_object);
    if let Some(tokens) = tokens {
        for key in ["account_id", "accountId"] {
            if let Some(value) = non_empty_str(tokens.get(key).unwrap_or(&Value::Null)) {
                return Ok(value.to_string());
            }
        }
        if let Some(id_token) = non_empty_str(
            tokens
                .get("id_token")
                .or_else(|| tokens.get("idToken"))
                .unwrap_or(&Value::Null),
        ) {
            if let Some(account_id) = account_id_from_id_token(id_token) {
                return Ok(account_id);
            }
        }
    }
    Err(Failure::new(
        "Could not find chatgpt-account-id in auth.json",
    ))
}

fn account_id_from_id_token(id_token: &str) -> Option<String> {
    let claims = jwt_payload(id_token)?;
    if let Some(auth_claims) = claims
        .get("https://api.openai.com/auth")
        .and_then(Value::as_object)
    {
        if let Some(value) = non_empty_str(
            auth_claims
                .get("chatgpt_account_id")
                .unwrap_or(&Value::Null),
        ) {
            return Some(value.to_string());
        }
    }
    non_empty_str(claims.get("chatgpt_account_id").unwrap_or(&Value::Null)).map(str::to_string)
}

pub fn jwt_payload(token: &str) -> Option<Value> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    if payload.is_empty() {
        return None;
    }
    let mut padded = payload.to_string();
    while padded.len() % 4 != 0 {
        padded.push('=');
    }
    let bytes = base64::engine::general_purpose::URL_SAFE
        .decode(padded)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_auth(path: &Path, auth: &Value) -> ProviderResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Failure::new(format!(
                "Could not update auth file {}: {error}",
                path.display()
            ))
        })?;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("auth.json");
    let temporary = path.with_file_name(format!("{name}.tmp"));
    let body = serde_json::to_vec_pretty(auth)
        .map_err(|_| Failure::new("Could not serialize refreshed auth"))?;
    fs::write(&temporary, body).map_err(|error| {
        Failure::new(format!(
            "Could not update auth file {}: {error}",
            path.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600)).map_err(|error| {
            Failure::new(format!(
                "Could not protect auth file {}: {error}",
                path.display()
            ))
        })?;
    }
    fs::rename(&temporary, path).map_err(|error| {
        Failure::new(format!(
            "Could not update auth file {}: {error}",
            path.display()
        ))
    })?;
    Ok(())
}

fn parse_window(raw: Option<&Value>, now: i64) -> Option<UsageWindow> {
    let raw = raw?.as_object()?;
    let mut reset_at = raw.get("reset_at").and_then(int_opt);
    if reset_at.is_none() {
        if let Some(reset_after) = raw.get("reset_after_seconds").and_then(int_opt) {
            reset_at = Some(now + reset_after);
        }
    }
    Some(UsageWindow {
        used_percent: raw.get("used_percent").and_then(float_opt),
        reset_at,
        window_seconds: raw.get("limit_window_seconds").and_then(int_opt),
    })
}

fn token_value(data: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| non_empty_str(data.get(*key).unwrap_or(&Value::Null)).map(str::to_string))
}

fn parse_object(body: &str, what: &str) -> ProviderResult<Value> {
    let value: Value = serde_json::from_str(body)
        .map_err(|_| Failure::new(format!("{what} is not valid JSON")))?;
    if !value.is_object() {
        return Err(Failure::new(format!(
            "{what} does not contain a JSON object"
        )));
    }
    Ok(value)
}

fn error_detail(status: reqwest::StatusCode, body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        status
            .canonical_reason()
            .unwrap_or("error without details")
            .to_string()
    } else {
        truncate(body, 500).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::build_codex_card;
    use chrono::{DateTime, Local};
    use serde_json::json;

    fn window(used_percent: f64, seconds: i64) -> Value {
        json!({
            "used_percent": used_percent,
            "limit_window_seconds": seconds,
            "reset_after_seconds": 600
        })
    }

    #[test]
    fn classifies_short_and_weekly_windows() {
        let payload = json!({
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": window(20.0, 21_600),
                "secondary_window": window(40.0, 604_800)
            }
        });
        let usage = parse_usage_payload(&payload, Some(1_000));
        let short = usage.short_window.unwrap();
        assert_eq!(short.window_seconds, Some(21_600));
        assert_eq!(short.used_percent, Some(20.0));
        assert_eq!(short.reset_at, Some(1_600));
        assert_eq!(usage.weekly.unwrap().window_seconds, Some(604_800));
        assert_eq!(usage.plan_type.as_deref(), Some("plus"));

        let five_hour = parse_usage_payload(
            &json!({
                "rate_limit": {
                    "primary_window": window(14.0, 18_000),
                    "secondary_window": window(16.0, 604_800)
                }
            }),
            Some(1_000),
        );
        assert_eq!(five_hour.short_window.unwrap().window_seconds, Some(18_000));
        assert_eq!(five_hour.weekly.unwrap().window_seconds, Some(604_800));
    }

    #[test]
    fn reads_a_short_window_from_the_secondary_slot() {
        let usage = parse_usage_payload(
            &json!({
                "rate_limit": {
                    "primary_window": window(40.0, 604_800),
                    "secondary_window": window(20.0, 21_600)
                }
            }),
            Some(1_000),
        );
        assert_eq!(usage.weekly.unwrap().used_percent, Some(40.0));
        assert_eq!(usage.short_window.unwrap().used_percent, Some(20.0));
    }

    #[test]
    fn weekly_only_and_missing_rate_limit() {
        let weekly = parse_usage_payload(
            &json!({"rate_limit": {"primary_window": window(30.0, 604_800)}}),
            Some(1_000),
        );
        assert!(weekly.short_window.is_none());
        assert_eq!(weekly.weekly.unwrap().used_percent, Some(30.0));

        let empty = parse_usage_payload(&json!({"plan_type": "pro"}), Some(1_000));
        assert!(empty.short_window.is_none());
        assert!(empty.weekly.is_none());
        assert_eq!(empty.plan_type.as_deref(), Some("pro"));
    }

    #[test]
    fn builds_the_codex_card() {
        let usage = CodexUsage {
            short_window: Some(UsageWindow {
                used_percent: Some(25.0),
                reset_at: Some(1_800_000_000),
                window_seconds: Some(18_000),
            }),
            weekly: Some(UsageWindow {
                used_percent: Some(7.0),
                reset_at: Some(1_800_050_000),
                window_seconds: Some(604_800),
            }),
            plan_type: Some("plus".to_string()),
        };
        let card = build_codex_card(&usage);
        assert_eq!(card.title, "Codex");
        assert_eq!(card.primary_label, "Janela de 5 horas");
        assert_eq!(card.primary_value, "75% restantes");
        assert_eq!(card.primary_percent, Some(75));
        assert_eq!(card.secondary_value, "93% restantes");
        assert_eq!(card.secondary_percent, Some(93));
        assert_eq!(card.detail, "Assinatura Plus");
        assert!(card.available);
        let short = DateTime::from_timestamp(1_800_000_000, 0)
            .unwrap()
            .with_timezone(&Local);
        let weekly = DateTime::from_timestamp(1_800_050_000, 0)
            .unwrap()
            .with_timezone(&Local);
        assert_eq!(
            card.footnote,
            format!(
                "5h {} · 7d {}",
                short.format("%H:%M"),
                weekly.format("%d/%m %H:%M")
            )
        );

        let six_hours = build_codex_card(&CodexUsage {
            short_window: Some(UsageWindow {
                used_percent: Some(10.0),
                reset_at: None,
                window_seconds: Some(21_600),
            }),
            weekly: None,
            plan_type: None,
        });
        assert_eq!(six_hours.primary_label, "Janela de 6 horas");
        assert_eq!(six_hours.detail, "Limites de uso do Codex");

        let missing = build_codex_card(&CodexUsage {
            short_window: None,
            weekly: None,
            plan_type: None,
        });
        assert_eq!(missing.primary_value, "Indisponível");
        assert_eq!(missing.primary_percent, None);
        assert_eq!(missing.footnote, "Redefinição indisponível");
        assert!(!missing.available);
    }

    #[test]
    fn reads_the_account_id_from_the_id_token() {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct-1"}}"#);
        let auth = json!({"tokens": {"access_token": "token", "id_token": format!("{header}.{payload}.sig")}});
        assert_eq!(extract_account_id(&auth).unwrap(), "acct-1");
    }
}
