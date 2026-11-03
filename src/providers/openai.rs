use crate::error::{Failure, ProviderResult};
use crate::http::{self, execute};
use crate::jsonutil::{decimal, int_opt};
use crate::settings::Settings;
use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveTime};
use reqwest::Client;
use rust_decimal::Decimal;
use serde_json::Value;

const MAX_BUCKETS: i64 = 180;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAICosts {
    pub currency: String,
    pub today: Decimal,
    pub month: Decimal,
    pub tracked: Decimal,
    pub tracked_since: i64,
}

pub async fn load(settings: &Settings, client: &Client) -> ProviderResult<OpenAICosts> {
    let admin_key = settings.openai_admin_key.as_deref().filter(|value| !value.is_empty()).ok_or_else(|| {
        Failure::new(format!(
            "Admin key da OpenAI não configurada (--openai-admin-key, OPENAI_ADMIN_API_KEY em {} ou openai_admin_key no config)",
            settings.env_file.display()
        ))
    })?;
    load_costs(
        client,
        admin_key,
        &settings.openai_base_url,
        settings.openai_credits_since,
        None,
    )
    .await
}

pub async fn load_costs(
    client: &Client,
    admin_key: &str,
    base_url: &str,
    credits_since: Option<NaiveDate>,
    now: Option<i64>,
) -> ProviderResult<OpenAICosts> {
    let now_epoch = now.unwrap_or_else(|| chrono::Utc::now().timestamp());
    let moment = DateTime::from_timestamp(now_epoch, 0)
        .ok_or_else(|| Failure::new("Relógio do sistema inválido"))?
        .with_timezone(&Local);
    let today_start = local_midnight(moment.date_naive())?;
    let month_start = local_midnight(
        NaiveDate::from_ymd_opt(moment.year(), moment.month(), 1)
            .ok_or_else(|| Failure::new("Data do mês atual inválida"))?,
    )?;
    let tracked_since = match credits_since {
        Some(date) => local_midnight(date)?,
        None => month_start,
    };
    let buckets =
        fetch_buckets(client, admin_key, base_url, tracked_since.min(month_start)).await?;
    Ok(summarize(&buckets, today_start, month_start, tracked_since))
}

pub fn summarize(
    buckets: &[Value],
    today_start: i64,
    month_start: i64,
    tracked_since: i64,
) -> OpenAICosts {
    let mut currency = "USD".to_string();
    let mut today = Decimal::ZERO;
    let mut month = Decimal::ZERO;
    let mut tracked = Decimal::ZERO;
    for bucket in buckets {
        let Some(start) = bucket.get("start_time").and_then(int_opt) else {
            continue;
        };
        let Some(results) = bucket.get("results").and_then(Value::as_array) else {
            continue;
        };
        let mut amount = Decimal::ZERO;
        for result in results {
            let Some(raw) = result.get("amount").filter(|value| value.is_object()) else {
                continue;
            };
            amount += decimal(raw.get("value").unwrap_or(&Value::Null));
            if let Some(code) = raw
                .get("currency")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                currency = code.to_ascii_uppercase();
            }
        }
        if start >= tracked_since {
            tracked += amount;
        }
        if start >= month_start {
            month += amount;
        }
        if start >= today_start {
            today += amount;
        }
    }
    OpenAICosts {
        currency,
        today,
        month,
        tracked,
        tracked_since,
    }
}

async fn fetch_buckets(
    client: &Client,
    admin_key: &str,
    base_url: &str,
    start_time: i64,
) -> ProviderResult<Vec<Value>> {
    let mut buckets = Vec::new();
    let mut page: Option<String> = None;
    loop {
        let mut request = client
            .get(format!(
                "{}/v1/organization/costs",
                base_url.trim_end_matches('/')
            ))
            .bearer_auth(admin_key)
            .header("Accept", "application/json")
            .query(&[
                ("start_time", start_time.to_string()),
                ("bucket_width", "1d".to_string()),
                ("limit", MAX_BUCKETS.to_string()),
            ]);
        if let Some(page) = &page {
            request = request.query(&[("page", page)]);
        }
        let fetched = execute(request).await.map_err(|error| {
            http::transport_failure(
                error,
                "Tempo esgotado ao consultar custos da OpenAI",
                "Erro de rede ao consultar custos da OpenAI",
            )
        })?;
        if fetched.status.as_u16() == 401 || fetched.status.as_u16() == 403 {
            return Err(Failure::new(
                "Chave da OpenAI sem permissão de custos; use uma Admin key com escopo api.usage.read",
            ));
        }
        if !fetched.status.is_success() {
            return Err(Failure::new(format!(
                "HTTP {} ao consultar custos da OpenAI",
                fetched.status.as_u16()
            )));
        }
        let payload: Value = serde_json::from_str(&fetched.body)
            .map_err(|_| Failure::new("Resposta de custos da OpenAI não é JSON válido"))?;
        if !payload.is_object() {
            return Err(Failure::new(
                "Resposta de custos da OpenAI não contém um objeto JSON",
            ));
        }
        let data = payload
            .get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| Failure::new("Resposta de custos da OpenAI sem data"))?;
        buckets.extend(data.iter().filter(|item| item.is_object()).cloned());
        page = if payload.get("has_more") == Some(&Value::Bool(true)) {
            payload
                .get("next_page")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        } else {
            None
        };
        if page.is_none() {
            return Ok(buckets);
        }
    }
}

fn local_midnight(date: NaiveDate) -> ProviderResult<i64> {
    let naive = date.and_time(NaiveTime::from_hms_opt(0, 0, 0).expect("midnight"));
    match naive.and_local_timezone(Local) {
        chrono::LocalResult::Single(value) => Ok(value.timestamp()),
        chrono::LocalResult::Ambiguous(earliest, _) => Ok(earliest.timestamp()),
        chrono::LocalResult::None => {
            let shifted = date.and_time(NaiveTime::from_hms_opt(1, 0, 0).expect("one"));
            match shifted.and_local_timezone(Local) {
                chrono::LocalResult::Single(value) | chrono::LocalResult::Ambiguous(value, _) => {
                    Ok(value.timestamp())
                }
                chrono::LocalResult::None => Err(Failure::new(
                    "Não foi possível calcular o início do dia local",
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::build_openai_card;
    use rust_decimal::Decimal;
    use serde_json::json;
    use std::str::FromStr;

    fn bucket(start: i64, value: &str) -> Value {
        json!({
            "start_time": start,
            "results": [{"amount": {"value": value, "currency": "usd"}}]
        })
    }

    #[test]
    fn splits_today_month_and_tracked_windows() {
        let costs = summarize(
            &[
                bucket(500, "7.00"),
                bucket(1_000, "1.50"),
                bucket(5_000, "2.25"),
                bucket(9_000, "0.75"),
            ],
            9_000,
            5_000,
            1_000,
        );
        assert_eq!(costs.today, Decimal::from_str("0.75").unwrap());
        assert_eq!(costs.month, Decimal::from_str("3.00").unwrap());
        assert_eq!(costs.tracked, Decimal::from_str("4.50").unwrap());
        assert_eq!(costs.currency, "USD");
    }

    #[test]
    fn builds_balance_and_spend_cards() {
        let costs = summarize(
            &[bucket(1_000, "1.50"), bucket(9_000, "0.75")],
            9_000,
            1_000,
            1_000,
        );
        let with_credits = build_openai_card(&costs, Some(Decimal::from(20)));
        assert_eq!(with_credits.primary_label, "Saldo");
        assert_eq!(with_credits.primary_value, "US$ 17,75");
        assert_eq!(with_credits.secondary_value, "US$ 2,25");
        assert!(with_credits.detail.contains("Hoje US$ 0,75"));
        assert!(with_credits.available);

        let spend_only = build_openai_card(&costs, None);
        assert_eq!(spend_only.primary_label, "Gasto no mês");
        assert_eq!(spend_only.primary_value, "US$ 2,25");
        assert_eq!(spend_only.secondary_value, "US$ 0,75");

        let exhausted = build_openai_card(&costs, Some(Decimal::from(1)));
        assert_eq!(exhausted.primary_value, "US$ -1,25");
        assert!(!exhausted.available);
    }
}
