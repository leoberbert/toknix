use crate::error::{Failure, ProviderResult};
use crate::http::{self, execute};
use crate::jsonutil::{decimal, int_opt};
use crate::settings::Settings;
use reqwest::Client;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepSeekBalance {
    pub currency: String,
    pub total: Decimal,
    pub granted: Decimal,
    pub topped_up: Decimal,
    pub is_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpendSnapshot {
    pub spent: Decimal,
    pub topups: Decimal,
    pub since: Option<i64>,
    pub tracking: bool,
}

pub struct DeepSeekReport {
    pub balance: DeepSeekBalance,
    pub spend: SpendSnapshot,
}

pub async fn load(settings: &Settings, client: &Client) -> ProviderResult<DeepSeekReport> {
    let api_key = settings
        .deepseek_api_key
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            Failure::new(format!(
            "DeepSeek API key não configurada (--deepseek-api-key, DEEPSEEK_API_KEY em {} ou {})",
            settings.env_file.display(),
            settings.config_path.display()
        ))
        })?;
    let balance = fetch_balance(client, api_key, &settings.deepseek_base_url).await?;
    let spend = SpendTracker::new(settings.deepseek_state_file.clone()).record(&balance, None)?;
    Ok(DeepSeekReport { balance, spend })
}

pub async fn fetch_balance(
    client: &Client,
    api_key: &str,
    base_url: &str,
) -> ProviderResult<DeepSeekBalance> {
    let url = format!("{}/user/balance", base_url.trim_end_matches('/'));
    let fetched = execute(
        client
            .get(url)
            .bearer_auth(api_key)
            .header("Accept", "application/json"),
    )
    .await
    .map_err(|error| {
        http::transport_failure(
            error,
            "Tempo esgotado ao consultar saldo DeepSeek",
            "Erro de rede ao consultar saldo DeepSeek",
        )
    })?;
    if !fetched.status.is_success() {
        return Err(Failure::new(format!(
            "HTTP {} ao consultar saldo DeepSeek",
            fetched.status.as_u16()
        )));
    }
    let payload: Value = serde_json::from_str(&fetched.body)
        .map_err(|_| Failure::new("Resposta de saldo DeepSeek não é JSON válido"))?;
    if !payload.is_object() {
        return Err(Failure::new(
            "Resposta de saldo DeepSeek não contém um objeto JSON",
        ));
    }
    parse_balance_payload(&payload)
}

pub fn parse_balance_payload(payload: &Value) -> ProviderResult<DeepSeekBalance> {
    let infos = payload
        .get("balance_infos")
        .and_then(Value::as_array)
        .ok_or_else(|| Failure::new("Resposta de saldo DeepSeek sem balance_infos"))?;
    let entries: Vec<&Value> = infos.iter().filter(|entry| entry.is_object()).collect();
    if entries.is_empty() {
        return Err(Failure::new(
            "Resposta de saldo DeepSeek sem entradas válidas",
        ));
    }
    let chosen = entries
        .iter()
        .copied()
        .find(|entry| currency_of(entry) == "USD")
        .unwrap_or(entries[0]);
    Ok(DeepSeekBalance {
        currency: currency_of(chosen),
        total: decimal(chosen.get("total_balance").unwrap_or(&Value::Null)),
        granted: decimal(chosen.get("granted_balance").unwrap_or(&Value::Null)),
        topped_up: decimal(chosen.get("topped_up_balance").unwrap_or(&Value::Null)),
        is_available: payload.get("is_available") == Some(&Value::Bool(true)),
    })
}

pub struct SpendTracker {
    path: std::path::PathBuf,
}

impl SpendTracker {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn record(
        &self,
        balance: &DeepSeekBalance,
        now: Option<i64>,
    ) -> ProviderResult<SpendSnapshot> {
        let timestamp = now.unwrap_or_else(|| chrono::Utc::now().timestamp());
        let state = self.load();
        let mut spent = Decimal::ZERO;
        let mut topups = Decimal::ZERO;
        let mut since = timestamp;
        let mut tracking = false;
        if let Some(state) = state.filter(|state| {
            state.get("currency").and_then(Value::as_str) == Some(balance.currency.as_str())
        }) {
            spent = decimal(state.get("spent").unwrap_or(&Value::Null));
            topups = decimal(state.get("topups").unwrap_or(&Value::Null));
            since = state.get("since").and_then(int_opt).unwrap_or(timestamp);
            tracking = true;
            let delta = decimal(state.get("last_total").unwrap_or(&Value::Null)) - balance.total;
            if delta > Decimal::ZERO {
                spent += delta;
            } else if delta < Decimal::ZERO {
                topups += -delta;
            }
        }
        self.save(&json!({
            "currency": balance.currency,
            "last_total": balance.total.to_string(),
            "spent": spent.to_string(),
            "topups": topups.to_string(),
            "since": since,
        }))?;
        Ok(SpendSnapshot {
            spent,
            topups,
            since: Some(since),
            tracking,
        })
    }

    fn load(&self) -> Option<Value> {
        let text = fs::read_to_string(&self.path).ok()?;
        let value: Value = serde_json::from_str(&text).ok()?;
        value.is_object().then_some(value)
    }

    fn save(&self, state: &Value) -> ProviderResult<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| spend_write_error(&self.path, &error))?;
        }
        let name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("deepseek-spend.json");
        let temporary = self.path.with_file_name(format!("{name}.tmp"));
        let body = serde_json::to_vec_pretty(state).map_err(|error| {
            Failure::new(format!(
                "Não foi possível gravar o estado de gasto DeepSeek: {error}"
            ))
        })?;
        fs::write(&temporary, body).map_err(|error| spend_write_error(&self.path, &error))?;
        fs::rename(&temporary, &self.path)
            .map_err(|error| spend_write_error(&self.path, &error))?;
        Ok(())
    }
}

fn spend_write_error(path: &Path, error: &std::io::Error) -> Failure {
    Failure::new(format!(
        "Não foi possível gravar o estado de gasto DeepSeek em {}: {error}",
        path.display()
    ))
}

fn currency_of(entry: &Value) -> String {
    entry
        .get("currency")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| text.to_ascii_uppercase())
        .unwrap_or_else(|| "USD".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::build_deepseek_card;
    use rust_decimal::Decimal;
    use serde_json::json;
    use std::str::FromStr;

    fn balance(total: &str, currency: &str, available: bool) -> DeepSeekBalance {
        let value = Decimal::from_str(total).unwrap();
        DeepSeekBalance {
            currency: currency.to_string(),
            total: value,
            granted: Decimal::ZERO,
            topped_up: value,
            is_available: available,
        }
    }

    #[test]
    fn parses_balance_infos_and_prefers_usd() {
        let parsed = parse_balance_payload(&json!({
            "is_available": true,
            "balance_infos": [{
                "currency": "USD",
                "total_balance": "4.19",
                "granted_balance": "0.00",
                "topped_up_balance": "4.19"
            }]
        }))
        .unwrap();
        assert_eq!(parsed.currency, "USD");
        assert_eq!(parsed.total, Decimal::from_str("4.19").unwrap());
        assert!(parsed.is_available);

        let preferred = parse_balance_payload(&json!({
            "is_available": false,
            "balance_infos": [
                {"currency": "CNY", "total_balance": "30.00"},
                {"currency": "USD", "total_balance": "4.19"}
            ]
        }))
        .unwrap();
        assert_eq!(preferred.currency, "USD");
        assert!(!preferred.is_available);
        assert!(
            parse_balance_payload(&json!({"is_available": true, "balance_infos": []})).is_err()
        );
    }

    #[test]
    fn tracks_spend_from_balance_drops() {
        let path = tempfile::NamedTempFile::new().unwrap().path().to_path_buf();
        let tracker = SpendTracker::new(&path);
        let first = tracker
            .record(&balance("10.00", "USD", true), Some(1_000))
            .unwrap();
        assert_eq!(first.spent, Decimal::ZERO);
        assert!(!first.tracking);
        assert_eq!(first.since, Some(1_000));

        let drop = tracker
            .record(&balance("7.50", "USD", true), Some(2_000))
            .unwrap();
        assert_eq!(drop.spent, Decimal::from_str("2.50").unwrap());
        assert!(drop.tracking);

        let topup = SpendTracker::new(&path)
            .record(&balance("12.00", "USD", true), Some(3_000))
            .unwrap();
        assert_eq!(topup.spent, Decimal::from_str("2.50").unwrap());
        assert_eq!(topup.topups, Decimal::from_str("4.50").unwrap());

        let reset = tracker
            .record(&balance("10.00", "CNY", true), Some(4_000))
            .unwrap();
        assert_eq!(reset.spent, Decimal::ZERO);
        assert_eq!(reset.since, Some(4_000));
        assert!(!reset.tracking);
    }

    #[test]
    fn corrupt_state_is_ignored_and_cards_include_the_baseline() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "{not json").unwrap();
        let tracker = SpendTracker::new(file.path());
        let spend = tracker
            .record(&balance("10.00", "USD", true), Some(1_000))
            .unwrap();
        assert!(!spend.tracking);
        let state: Value =
            serde_json::from_str(&std::fs::read_to_string(file.path()).unwrap()).unwrap();
        assert_eq!(state["last_total"], "10.00");

        let card = build_deepseek_card(
            &balance("10.00", "USD", true),
            &spend,
            Some(Decimal::from_str("5.50").unwrap()),
        );
        assert_eq!(card.secondary_label, "Gasto total");
        assert_eq!(card.secondary_value, "US$ 5,50");

        let monitored = tracker
            .record(&balance("7.50", "USD", true), Some(2_000))
            .unwrap();
        let card = build_deepseek_card(&balance("7.50", "USD", true), &monitored, None);
        assert_eq!(card.primary_value, "US$ 7,50");
        assert_eq!(card.secondary_label, "Gasto monitorado");
        assert_eq!(card.secondary_value, "US$ 2,50");
        assert_eq!(card.detail, "Concedido US$ 0,00 · Recarregado US$ 7,50");
        assert!(card.footnote.starts_with("Monitorado desde"));
        assert!(card.available);

        let unavailable = build_deepseek_card(&balance("0.00", "USD", false), &monitored, None);
        assert_eq!(unavailable.footnote, "Conta sem saldo disponível para uso");
        assert!(!unavailable.available);
    }
}
