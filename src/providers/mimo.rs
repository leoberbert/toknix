use crate::error::{Failure, ProviderResult};
use crate::http::{self, execute};
use crate::jsonutil::{decimal, int_opt, non_empty_str};
use crate::providers::claude::session_cookies;
use crate::settings::Settings;
use reqwest::Client;
use rust_decimal::Decimal;
use serde_json::Value;
use tokio::sync::OnceCell;

const DEFAULT_DOMAIN: &str = "platform.xiaomimimo.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MiMoSnapshot {
    pub currency: String,
    pub balance: Decimal,
    pub cash: Decimal,
    pub gift: Decimal,
    pub recharged: Option<Decimal>,
    pub total_cost: Decimal,
    pub month_cost: Decimal,
    pub total_tokens: i64,
}

pub async fn load(
    settings: &Settings,
    client: &Client,
    keys: &OnceCell<Vec<Vec<u8>>>,
) -> ProviderResult<MiMoSnapshot> {
    let cookies =
        session_cookies(DEFAULT_DOMAIN, settings.mimo_chrome_profile.clone(), keys).await?;
    fetch_snapshot(
        client,
        &crate::providers::chrome::cookie_header(&cookies),
        &settings.mimo_base_url,
    )
    .await
}

pub async fn fetch_snapshot(
    client: &Client,
    cookie: &str,
    base_url: &str,
) -> ProviderResult<MiMoSnapshot> {
    let balance = get_json(client, base_url, "/api/v1/balance", cookie).await?;
    let usage = get_json(client, base_url, "/api/v1/usage", cookie).await?;
    let recharge = get_json(
        client,
        base_url,
        "/api/v1/accumulatedRechargeAmount",
        cookie,
    )
    .await
    .ok();
    parse_snapshot(&balance, &usage, recharge.as_ref())
}

pub fn parse_snapshot(
    balance: &Value,
    usage: &Value,
    recharge: Option<&Value>,
) -> ProviderResult<MiMoSnapshot> {
    let currency = non_empty_str(balance.get("currency").unwrap_or(&Value::Null))
        .unwrap_or("USD")
        .to_ascii_uppercase();
    let token_usage = usage.get("tokenUsage").and_then(Value::as_object);
    let cost_usage = usage.get("costUsage").and_then(Value::as_object);
    if token_usage.is_none() || cost_usage.is_none() {
        return Err(Failure::new(
            "Resposta de uso do MiMo sem tokenUsage/costUsage",
        ));
    }
    let token_usage = token_usage.unwrap();
    let cost_usage = cost_usage.unwrap();
    Ok(MiMoSnapshot {
        currency,
        balance: decimal(balance.get("balance").unwrap_or(&Value::Null)),
        cash: decimal(balance.get("cashBalance").unwrap_or(&Value::Null)),
        gift: decimal(balance.get("giftBalance").unwrap_or(&Value::Null)),
        recharged: recharge.filter(|value| value.is_object()).map(|value| {
            decimal(
                value
                    .get("accumulatedRechargeAmount")
                    .unwrap_or(&Value::Null),
            )
        }),
        total_cost: decimal(cost_usage.get("totalCost").unwrap_or(&Value::Null)),
        month_cost: decimal(cost_usage.get("currentMonthCost").unwrap_or(&Value::Null)),
        total_tokens: int_opt(token_usage.get("totalToken").unwrap_or(&Value::Null)).unwrap_or(0),
    })
}

async fn get_json(
    client: &Client,
    base_url: &str,
    path: &str,
    cookie: &str,
) -> ProviderResult<Value> {
    let fetched = execute(
        client
            .get(format!("{}{path}", base_url.trim_end_matches('/')))
            .header("Cookie", cookie)
            .header("Accept", "application/json"),
    )
    .await
    .map_err(|error| {
        http::transport_failure(
            error,
            "Tempo esgotado ao consultar o MiMo",
            "Erro de rede ao consultar o MiMo",
        )
    })?;
    if fetched.status.as_u16() == 401 || fetched.status.as_u16() == 403 {
        return Err(Failure::new(
            "Sessão do Chrome expirada para platform.xiaomimimo.com; faça login novamente",
        ));
    }
    if !fetched.status.is_success() {
        return Err(Failure::new(format!(
            "HTTP {} ao consultar o MiMo",
            fetched.status.as_u16()
        )));
    }
    let payload: Value = serde_json::from_str(&fetched.body)
        .map_err(|_| Failure::new("Resposta do MiMo não é JSON válido"))?;
    if !payload.is_object() {
        return Err(Failure::new("Resposta do MiMo não contém um objeto JSON"));
    }
    if let Some(code) = payload.get("code") {
        if code != &Value::Null && code.as_i64() != Some(0) && code.as_u64() != Some(0) {
            let message = non_empty_str(payload.get("message").unwrap_or(&Value::Null))
                .unwrap_or("erro desconhecido");
            return Err(Failure::new(format!("MiMo retornou erro: {message}")));
        }
    }
    payload
        .get("data")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| Failure::new(format!("Resposta do MiMo sem data em {path}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::build_mimo_card;
    use rust_decimal::Decimal;
    use serde_json::json;
    use std::str::FromStr;

    #[test]
    fn parses_balance_usage_and_optional_recharge() {
        let snapshot = parse_snapshot(
            &json!({"balance": "9.82", "cashBalance": "9.82", "giftBalance": "0.00", "currency": "USD"}),
            &json!({
                "tokenUsage": {"totalToken": 17_570_210},
                "costUsage": {"totalCost": "0.90", "currentMonthCost": "0.10"}
            }),
            Some(&json!({"accumulatedRechargeAmount": "10.00"})),
        )
        .unwrap();
        assert_eq!(snapshot.balance, Decimal::from_str("9.82").unwrap());
        assert_eq!(snapshot.recharged, Decimal::from_str("10.00").ok());
        assert_eq!(snapshot.total_tokens, 17_570_210);
        let card = build_mimo_card(&snapshot);
        assert_eq!(card.primary_value, "US$ 9,82");
        assert_eq!(card.secondary_value, "US$ 0,90");
        assert_eq!(card.detail, "Recarregado US$ 10,00 · Mês US$ 0,10");
        assert_eq!(card.footnote, "Sessão do Chrome · 17,6M tokens");
        assert!(card.available);

        let without = parse_snapshot(
            &json!({"balance": "1.00", "currency": "CNY"}),
            &json!({"tokenUsage": {"totalToken": 10}, "costUsage": {"totalCost": "0.5", "currentMonthCost": "0.5"}}),
            None,
        )
        .unwrap();
        assert!(without.recharged.is_none());
        assert_eq!(without.currency, "CNY");
        assert!(
            parse_snapshot(&json!({"balance": "1.00"}), &json!({"costUsage": {}}), None).is_err()
        );

        let empty = parse_snapshot(
            &json!({"balance": "0.00", "currency": "USD"}),
            &json!({"tokenUsage": {"totalToken": 0}, "costUsage": {"totalCost": "0", "currentMonthCost": "0"}}),
            None,
        )
        .unwrap();
        let card = build_mimo_card(&empty);
        assert!(!card.available);
        assert_eq!(card.footnote, "Sessão do Chrome · 0 tokens");
    }
}
