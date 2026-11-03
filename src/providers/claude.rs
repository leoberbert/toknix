use crate::error::{Failure, ProviderResult};
use crate::http::{self, execute};
use crate::jsonutil::{float_opt, non_empty_str};
use crate::providers::chrome;
use crate::settings::Settings;
use chrono::DateTime;
use reqwest::Client;
use serde_json::Value;
use tokio::sync::OnceCell;

const WEB_BASE_URL: &str = "https://claude.ai";
const WEB_DOMAIN: &str = "claude.ai";
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36";

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeWindow {
    pub utilization: f64,
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeUsage {
    pub five_hour: Option<ClaudeWindow>,
    pub seven_day: Option<ClaudeWindow>,
    pub plan: Option<String>,
}

pub async fn load(
    settings: &Settings,
    client: &Client,
    keys: &OnceCell<Vec<Vec<u8>>>,
) -> ProviderResult<ClaudeUsage> {
    let profile = settings.claude_chrome_profile.clone();
    let cookies = session_cookies(WEB_DOMAIN, profile, keys)
        .await
        .map_err(|error| {
            Failure::new(format!("Sessão do Claude no Chrome indisponível: {error}"))
        })?;
    fetch_web_usage(client, &chrome::cookie_header(&cookies)).await
}

pub async fn session_cookies(
    domain: &str,
    profile: Option<String>,
    keys: &OnceCell<Vec<Vec<u8>>>,
) -> ProviderResult<Vec<chrome::ChromeCookie>> {
    let key_material = keys.get_or_init(chrome::decryption_keys).await.clone();
    let domain = domain.to_string();
    tokio::task::spawn_blocking(move || {
        chrome::load_cookies(&domain, profile.as_deref(), &key_material)
    })
    .await
    .map_err(|error| Failure::new(format!("Falha ao ler os cookies do Chrome: {error}")))?
}

pub async fn fetch_web_usage(client: &Client, session: &str) -> ProviderResult<ClaudeUsage> {
    let organizations = get_json(client, "/api/organizations", session).await?;
    let Some(organizations) = organizations.as_array() else {
        return Err(Failure::new("Resposta de organizações do Claude inválida"));
    };
    let chat_org = organizations.iter().find(|org| {
        org.get("uuid").and_then(Value::as_str).is_some()
            && org
                .get("capabilities")
                .and_then(Value::as_array)
                .is_some_and(|capabilities| {
                    capabilities
                        .iter()
                        .any(|item| item.as_str() == Some("chat"))
                })
    });
    let Some(chat_org) = chat_org else {
        return Err(Failure::new(
            "Nenhuma organização de chat do Claude disponível nesta sessão",
        ));
    };
    let uuid = chat_org
        .get("uuid")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let usage = get_json(
        client,
        &format!("/api/organizations/{}/usage", encode_segment(uuid)),
        session,
    )
    .await?;
    if !usage.is_object() {
        return Err(Failure::new("Resposta de uso do Claude inválida"));
    }
    if usage.get("type").and_then(Value::as_str) == Some("error") {
        return Err(Failure::new(
            "A organização do Claude não autorizou a consulta de uso",
        ));
    }
    parse_usage(&usage, plan_label(chat_org))
}

pub fn parse_usage(payload: &Value, plan: Option<String>) -> ProviderResult<ClaudeUsage> {
    let five_hour = parse_window(payload.get("five_hour"));
    let seven_day = parse_window(payload.get("seven_day"));
    if five_hour.is_none() && seven_day.is_none() {
        return Err(Failure::new("Resposta do Claude Code sem janelas de uso"));
    }
    Ok(ClaudeUsage {
        five_hour,
        seven_day,
        plan,
    })
}

pub fn plan_label(organization: &Value) -> Option<String> {
    if let Some(tier) = organization.get("rate_limit_tier").and_then(Value::as_str) {
        if tier.contains("max_20x") {
            return Some("Max 20x".to_string());
        }
        if tier.contains("max_5x") {
            return Some("Max 5x".to_string());
        }
        if tier.contains("max") {
            return Some("Max".to_string());
        }
    }
    non_empty_str(
        organization
            .get("plan_display_name")
            .unwrap_or(&Value::Null),
    )
    .map(str::to_string)
}

async fn get_json(client: &Client, path: &str, session: &str) -> ProviderResult<Value> {
    let fetched = execute(
        client
            .get(format!("{WEB_BASE_URL}{path}"))
            .header("Cookie", session)
            .header("Accept", "application/json")
            .header("User-Agent", USER_AGENT),
    )
    .await
    .map_err(|error| {
        http::transport_failure(
            error,
            "Tempo esgotado ao consultar o Claude",
            "Erro de rede ao consultar o Claude",
        )
    })?;
    let status = fetched.status.as_u16();
    if matches!(status, 301 | 302 | 303 | 307 | 308 | 401 | 403) {
        return Err(Failure::new(
            "Sessão do Claude no Chrome expirada; abra claude.ai e faça login",
        ));
    }
    if status == 429 {
        return Err(Failure::new(
            "Consulta de uso do Claude temporariamente limitada (HTTP 429)",
        ));
    }
    if !fetched.status.is_success() {
        return Err(Failure::new(format!("HTTP {status} ao consultar o Claude")));
    }
    serde_json::from_str(&fetched.body)
        .map_err(|_| Failure::new("Resposta de uso do Claude não é JSON válido"))
}

fn parse_window(raw: Option<&Value>) -> Option<ClaudeWindow> {
    let raw = raw?.as_object()?;
    Some(ClaudeWindow {
        utilization: float_opt(raw.get("utilization")?)?,
        resets_at: iso_epoch(raw.get("resets_at")),
    })
}

fn iso_epoch(value: Option<&Value>) -> Option<i64> {
    let text = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())?;
    let normalized = text.replacen('Z', "+00:00", 1);
    DateTime::parse_from_rfc3339(&normalized)
        .ok()
        .map(|parsed| parsed.timestamp())
}

fn encode_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::build_claude_card;
    use chrono::{DateTime, Local};
    use serde_json::json;

    const RESET_5H: &str = "2026-10-22T03:49:59.549914+00:00";
    const RESET_7D: &str = "2026-10-23T03:59:59.549930+00:00";

    #[test]
    fn parses_windows_and_builds_the_card() {
        let usage = parse_usage(
            &json!({
                "five_hour": {"utilization": 2.0, "resets_at": RESET_5H},
                "seven_day": {"utilization": 3.0, "resets_at": RESET_7D}
            }),
            Some("Max 5x".to_string()),
        )
        .unwrap();
        assert_eq!(usage.five_hour.as_ref().unwrap().utilization, 2.0);
        assert_eq!(
            usage.five_hour.as_ref().unwrap().resets_at,
            Some(DateTime::parse_from_rfc3339(RESET_5H).unwrap().timestamp())
        );
        let card = build_claude_card(&usage);
        assert_eq!(card.primary_value, "98% restantes");
        assert_eq!(card.secondary_value, "97% restantes");
        assert_eq!(card.primary_percent, Some(98));
        assert_eq!(card.secondary_percent, Some(97));
        assert_eq!(card.detail, "Assinatura Max 5x");
        let reset = DateTime::from_timestamp(usage.five_hour.unwrap().resets_at.unwrap(), 0)
            .unwrap()
            .with_timezone(&Local);
        assert!(card
            .footnote
            .contains(&format!("5h {}", reset.format("%d/%m %H:%M"))));

        let missing = parse_usage(
            &json!({"seven_day": {"utilization": 100, "resets_at": RESET_7D}}),
            None,
        )
        .unwrap();
        let card = build_claude_card(&missing);
        assert_eq!(card.primary_value, "Indisponível");
        assert_eq!(card.primary_percent, None);
        assert_eq!(card.secondary_percent, Some(0));
        assert!(parse_usage(&json!({"five_hour": null, "seven_day": null}), None).is_err());
    }

    #[test]
    fn labels_known_rate_limit_tiers() {
        assert_eq!(
            plan_label(&json!({"rate_limit_tier": "default_claude_max_20x"})).as_deref(),
            Some("Max 20x")
        );
        assert_eq!(
            plan_label(&json!({"rate_limit_tier": "default_claude_max"})).as_deref(),
            Some("Max")
        );
        assert_eq!(
            plan_label(&json!({"plan_display_name": " Pro "})).as_deref(),
            Some("Pro")
        );
    }
}
