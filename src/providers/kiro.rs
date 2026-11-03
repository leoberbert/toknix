use crate::error::{Failure, ProviderResult};
use crate::http::{self, execute};
use crate::jsonutil::{decimal, decimal_opt, int_opt, non_empty_str};
use crate::settings::Settings;
use chrono::{DateTime, Utc};
use reqwest::Client;
use rusqlite::{Connection, OpenFlags};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::Value;
use std::path::Path;

const TOKEN_KEY: &str = "kirocli:odic:token";
const TARGET: &str = "AmazonCodeWhispererService.GetUsageLimits";
const LOGIN_HINT: &str = "Sessão do Kiro expirada, rode `kiro-cli login`";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KiroUsage {
    pub plan: Option<String>,
    pub resource: String,
    pub used: Decimal,
    pub limit: Decimal,
    pub overage_enabled: bool,
    pub overage_used: Decimal,
    pub overage_cap: Decimal,
    pub overage_charges: Decimal,
    pub currency: String,
    pub next_reset: Option<i64>,
}

impl KiroUsage {
    pub fn percent_remaining(&self) -> i32 {
        if self.limit <= Decimal::ZERO {
            return 0;
        }
        let remaining = (self.limit - self.used) / self.limit * Decimal::ONE_HUNDRED;
        remaining.trunc().to_i64().unwrap_or(0).clamp(0, 100) as i32
    }
}

pub async fn load(settings: &Settings, client: &Client) -> ProviderResult<KiroUsage> {
    let database = settings.kiro_token_db.clone();
    let token = tokio::task::spawn_blocking(move || read_token(&database))
        .await
        .map_err(|error| Failure::new(format!("Falha ao ler o token do Kiro: {error}")))??;
    fetch_usage(client, &token, &settings.kiro_base_url).await
}

pub fn read_token(token_db: &Path) -> ProviderResult<String> {
    if !token_db.exists() {
        return Err(Failure::new(format!(
            "Banco do Kiro CLI não encontrado em {}",
            token_db.display()
        )));
    }
    let connection = Connection::open_with_flags(token_db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| {
            Failure::new(format!(
                "Não foi possível abrir {}: {error}",
                token_db.display()
            ))
        })?;
    let mut statement = connection
        .prepare("SELECT value FROM auth_kv WHERE key = ?1")
        .map_err(|error| Failure::new(format!("Banco do Kiro CLI ilegível: {error}")))?;
    let value: String = statement
        .query_row([TOKEN_KEY], |row| row.get(0))
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Failure::new(LOGIN_HINT),
            other => Failure::new(format!("Banco do Kiro CLI ilegível: {other}")),
        })?;
    let token: Value = serde_json::from_str(&value)
        .map_err(|_| Failure::new("Token do Kiro CLI não é JSON válido"))?;
    if !token.is_object() {
        return Err(Failure::new("Token do Kiro CLI em formato inesperado"));
    }
    if let Some(expires_at) = expiry(token.get("expires_at")) {
        if expires_at <= Utc::now() {
            return Err(Failure::new(LOGIN_HINT));
        }
    }
    non_empty_str(token.get("access_token").unwrap_or(&Value::Null))
        .map(str::to_string)
        .ok_or_else(|| Failure::new(LOGIN_HINT))
}

pub async fn fetch_usage(
    client: &Client,
    access_token: &str,
    base_url: &str,
) -> ProviderResult<KiroUsage> {
    let url = format!("{}/", base_url.trim_end_matches('/'));
    let fetched = execute(
        client
            .post(url)
            .body("{}")
            .bearer_auth(access_token)
            .header("Content-Type", "application/x-amz-json-1.0")
            .header("X-Amz-Target", TARGET)
            .header("Accept", "application/json"),
    )
    .await
    .map_err(|error| {
        http::transport_failure(
            error,
            "Falha ao consultar o uso do Kiro",
            "Falha ao consultar o uso do Kiro",
        )
    })?;
    if fetched.status.as_u16() == 401 || fetched.status.as_u16() == 403 {
        return Err(Failure::new(LOGIN_HINT));
    }
    if !fetched.status.is_success() {
        return Err(Failure::new(format!(
            "Uso do Kiro indisponível (HTTP {})",
            fetched.status.as_u16()
        )));
    }
    let payload: Value = serde_json::from_str(&fetched.body)
        .map_err(|_| Failure::new("Resposta de uso do Kiro não é JSON válido"))?;
    if !payload.is_object() {
        return Err(Failure::new(
            "Resposta de uso do Kiro em formato inesperado",
        ));
    }
    parse_usage(&payload)
}

pub fn parse_usage(payload: &Value) -> ProviderResult<KiroUsage> {
    let breakdown = credit_breakdown(payload.get("usageBreakdownList"))?;
    let limit = first_decimal(&breakdown, &["usageLimitWithPrecision", "usageLimit"]);
    if limit <= Decimal::ZERO {
        return Err(Failure::new("Limite de créditos do Kiro inválido"));
    }
    let overage = payload.get("overageConfiguration");
    let status = overage
        .and_then(|value| value.get("overageStatus"))
        .and_then(Value::as_str);
    let plan = payload
        .get("subscriptionInfo")
        .and_then(|value| value.get("subscriptionTitle"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let next_reset = breakdown
        .get("nextDateReset")
        .or_else(|| payload.get("nextDateReset"))
        .and_then(int_opt);
    Ok(KiroUsage {
        plan,
        resource: resource_name(&breakdown),
        used: first_decimal(&breakdown, &["currentUsageWithPrecision", "currentUsage"]),
        limit,
        overage_enabled: status.is_some_and(|value| value.eq_ignore_ascii_case("ENABLED")),
        overage_used: first_decimal(
            &breakdown,
            &["currentOveragesWithPrecision", "currentOverages"],
        ),
        overage_cap: first_decimal(&breakdown, &["overageCapWithPrecision", "overageCap"]),
        overage_charges: decimal(breakdown.get("overageCharges").unwrap_or(&Value::Null)),
        currency: breakdown
            .get("currency")
            .and_then(Value::as_str)
            .unwrap_or("USD")
            .to_ascii_uppercase(),
        next_reset,
    })
}

fn credit_breakdown(raw: Option<&Value>) -> ProviderResult<serde_json::Map<String, Value>> {
    let entries: Vec<&Value> = raw
        .and_then(Value::as_array)
        .map(|items| items.iter().filter(|entry| entry.is_object()).collect())
        .unwrap_or_default();
    if entries.is_empty() {
        return Err(Failure::new(
            "Resposta de uso do Kiro sem usageBreakdownList",
        ));
    }
    let chosen = entries
        .iter()
        .copied()
        .find(|entry| {
            entry
                .get("resourceType")
                .and_then(Value::as_str)
                .is_some_and(|value| value.eq_ignore_ascii_case("CREDIT"))
        })
        .unwrap_or(entries[0]);
    chosen
        .as_object()
        .cloned()
        .ok_or_else(|| Failure::new("Resposta de uso do Kiro sem usageBreakdownList"))
}

fn resource_name(breakdown: &serde_json::Map<String, Value>) -> String {
    for key in ["displayNamePlural", "displayName"] {
        if let Some(value) = breakdown.get(key).and_then(non_empty_str) {
            return value.to_string();
        }
    }
    "Credits".to_string()
}

fn first_decimal(entry: &serde_json::Map<String, Value>, keys: &[&str]) -> Decimal {
    for key in keys {
        if let Some(value) = entry.get(*key).and_then(decimal_opt) {
            return value;
        }
    }
    Decimal::ZERO
}

fn expiry(value: Option<&Value>) -> Option<DateTime<Utc>> {
    let text = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())?;
    let normalized = text.replacen('Z', "+00:00", 1);
    DateTime::parse_from_rfc3339(&normalized)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|naive| naive.and_utc())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::build_kiro_card;
    use chrono::{DateTime, Local};
    use rusqlite::Connection;
    use rust_decimal::Decimal;
    use serde_json::json;
    use std::str::FromStr;

    fn payload() -> Value {
        json!({
            "nextDateReset": 1793491200.0,
            "overageConfiguration": {"overageStatus": "DISABLED"},
            "subscriptionInfo": {"subscriptionTitle": "KIRO POWER"},
            "usageBreakdownList": [{
                "currency": "USD",
                "currentUsageWithPrecision": 176.64,
                "displayNamePlural": "Credits",
                "nextDateReset": 1793491200.0,
                "overageCapWithPrecision": 2500.0,
                "overageCharges": 0.0,
                "resourceType": "CREDIT",
                "usageLimitWithPrecision": 10000.0
            }]
        })
    }

    #[test]
    fn reads_credit_breakdown_and_builds_the_card() {
        let usage = parse_usage(&payload()).unwrap();
        assert_eq!(usage.used, Decimal::from_str("176.64").unwrap());
        assert_eq!(usage.limit, Decimal::from_str("10000.0").unwrap());
        assert_eq!(usage.plan.as_deref(), Some("KIRO POWER"));
        assert_eq!(usage.resource, "Credits");
        assert_eq!(usage.next_reset, Some(1_793_491_200));
        assert!(!usage.overage_enabled);
        assert_eq!(usage.percent_remaining(), 98);

        let mut almost = payload();
        almost["usageBreakdownList"][0]["currentUsageWithPrecision"] = json!(9999.5);
        assert_eq!(parse_usage(&almost).unwrap().percent_remaining(), 0);

        let card = build_kiro_card(&usage);
        assert_eq!(card.primary_value, "98% restantes");
        assert_eq!(card.primary_percent, Some(98));
        assert_eq!(card.secondary_value, "176,6 de 10.000");
        assert_eq!(card.detail, "Plano KIRO POWER");
        let reset = DateTime::from_timestamp(1_793_491_200, 0)
            .unwrap()
            .with_timezone(&Local);
        assert_eq!(
            card.footnote,
            format!(
                "Renova em {} · excedente desativado",
                reset.format("%d/%m/%Y")
            )
        );

        let mut enabled = payload();
        enabled["overageConfiguration"]["overageStatus"] = json!("ENABLED");
        enabled["usageBreakdownList"][0]["currentOveragesWithPrecision"] = json!(12.5);
        enabled["usageBreakdownList"][0]["overageCharges"] = json!(0.5);
        let overage = build_kiro_card(&parse_usage(&enabled).unwrap());
        assert!(overage.footnote.contains("excedente 12,5 de 2.500"));
        assert!(overage.footnote.contains("US$ 0,50"));

        let exhausted = usage_with_used("10000");
        let card = build_kiro_card(&exhausted);
        assert_eq!(card.primary_percent, Some(0));
        assert!(!card.available);
    }

    fn usage_with_used(used: &str) -> KiroUsage {
        let mut usage = parse_usage(&payload()).unwrap();
        usage.used = Decimal::from_str(used).unwrap();
        usage
    }

    #[test]
    fn rejects_empty_and_zero_limits() {
        assert!(parse_usage(&json!({"usageBreakdownList": []})).is_err());
        let mut zero = payload();
        zero["usageBreakdownList"][0]["usageLimitWithPrecision"] = json!(0.0);
        zero["usageBreakdownList"][0]["usageLimit"] = json!(0);
        assert!(parse_usage(&zero).is_err());
    }

    #[test]
    fn reads_a_token_without_rewriting_the_database() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE auth_kv (key TEXT PRIMARY KEY, value TEXT);
                 INSERT INTO auth_kv (key, value) VALUES (
                    'kirocli:odic:token',
                    '{\"access_token\":\"access-1\",\"expires_at\":\"2999-01-01T00:00:00Z\"}'
                 );",
            )
            .unwrap();
        drop(connection);
        assert_eq!(read_token(file.path()).unwrap(), "access-1");
    }
}
