use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;

pub fn format_money(amount: Decimal, currency: &str) -> String {
    let rounded = amount.round_dp_with_strategy(2, RoundingStrategy::MidpointNearestEven);
    let negative = rounded.is_sign_negative() && !rounded.is_zero();
    let cents = ((rounded.abs() * Decimal::ONE_HUNDRED).round_dp(0))
        .to_i64()
        .unwrap_or(0);
    let whole = cents / 100;
    let fraction = cents % 100;
    let sign = if negative { "-" } else { "" };
    format!(
        "{} {sign}{},{fraction:02}",
        currency_symbol(currency),
        format_grouped(whole)
    )
}

pub fn format_credits(value: Decimal) -> String {
    let quantized = value.round_dp_with_strategy(1, RoundingStrategy::MidpointNearestEven);
    if quantized.fract().is_zero() {
        return format_grouped(quantized.trunc().to_i64().unwrap_or(0));
    }
    let negative = quantized.is_sign_negative() && !quantized.is_zero();
    let scaled = ((quantized.abs() * Decimal::TEN).round_dp(0))
        .to_i64()
        .unwrap_or(0);
    let whole = scaled / 10;
    let fraction = scaled % 10;
    let sign = if negative { "-" } else { "" };
    format!("{sign}{},{fraction}", format_grouped(whole))
}

pub fn currency_symbol(currency: &str) -> String {
    match currency.to_ascii_uppercase().as_str() {
        "USD" => "US$".to_string(),
        "CNY" => "CN¥".to_string(),
        "EUR" => "€".to_string(),
        "BRL" => "R$".to_string(),
        other => other.to_string(),
    }
}

pub fn format_grouped(value: i64) -> String {
    let negative = value < 0;
    let digits = value.unsigned_abs().to_string();
    let mut reversed = String::new();
    for (index, character) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            reversed.push('.');
        }
        reversed.push(character);
    }
    let mut text: String = reversed.chars().rev().collect();
    if negative {
        text.insert(0, '-');
    }
    text
}

pub fn token_text(tokens: i64) -> String {
    if tokens >= 1_000_000 {
        let text = format!("{:.1}", tokens as f64 / 1_000_000.0).replace('.', ",");
        format!("{text}M tokens")
    } else {
        format!("{tokens} tokens")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;
    use std::str::FromStr;

    #[test]
    fn formats_usd_with_brazilian_separators() {
        assert_eq!(
            format_money(Decimal::from_str("1234.56").unwrap(), "USD"),
            "US$ 1.234,56"
        );
    }

    #[test]
    fn formats_other_currencies_and_negative_amounts() {
        assert_eq!(format_money(Decimal::ZERO, "CNY"), "CN¥ 0,00");
        assert_eq!(
            format_money(Decimal::from_str("9.9").unwrap(), "BRL"),
            "R$ 9,90"
        );
        assert_eq!(
            format_money(Decimal::from_str("-1.25").unwrap(), "USD"),
            "US$ -1,25"
        );
    }

    #[test]
    fn formats_credit_counts() {
        assert_eq!(
            format_credits(Decimal::from_str("176.64").unwrap()),
            "176,6"
        );
        assert_eq!(format_credits(Decimal::from(10_000)), "10.000");
        assert_eq!(format_credits(Decimal::from_str("12.5").unwrap()), "12,5");
        assert_eq!(format_credits(Decimal::from(2500)), "2.500");
    }

    #[test]
    fn formats_token_volume() {
        assert_eq!(token_text(17_570_210), "17,6M tokens");
        assert_eq!(token_text(0), "0 tokens");
    }
}
