use rust_decimal::Decimal;
use serde_json::Value;
use std::str::FromStr;

pub fn non_empty_str(value: &Value) -> Option<&str> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

pub fn decimal_opt(value: &Value) -> Option<Decimal> {
    match value {
        Value::Null | Value::Bool(_) => None,
        Value::Number(number) => Decimal::from_str(&number.to_string()).ok(),
        Value::String(text) => Decimal::from_str(text.trim()).ok(),
        _ => None,
    }
}

pub fn decimal(value: &Value) -> Decimal {
    decimal_opt(value).unwrap_or(Decimal::ZERO)
}

pub fn float_opt(value: &Value) -> Option<f64> {
    match value {
        Value::Bool(_) | Value::Null => None,
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

pub fn int_opt(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(_) | Value::Null => None,
        Value::Number(number) => number
            .as_i64()
            .or_else(|| number.as_f64().and_then(f64_to_i64)),
        Value::String(text) => text
            .trim()
            .parse::<i64>()
            .ok()
            .or_else(|| text.trim().parse::<f64>().ok().and_then(f64_to_i64)),
        _ => None,
    }
}

fn f64_to_i64(value: f64) -> Option<i64> {
    if value.is_finite() && value >= i64::MIN as f64 && value <= i64::MAX as f64 {
        Some(value as i64)
    } else {
        None
    }
}

pub fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
