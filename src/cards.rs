use crate::money::{format_credits, format_money, token_text};
use crate::providers::claude::{ClaudeUsage, ClaudeWindow};
use crate::providers::codex::{CodexUsage, UsageWindow};
use crate::providers::cursor::CursorUsage;
use crate::providers::deepseek::{DeepSeekBalance, SpendSnapshot};
use crate::providers::kiro::KiroUsage;
use crate::providers::mimo::MiMoSnapshot;
use crate::providers::openai::OpenAICosts;
use chrono::{DateTime, Local};
use rust_decimal::Decimal;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Card {
    pub id: String,
    pub title: String,
    pub loading: bool,
    pub available: bool,
    /// loading, ready, setup, session, or error.
    pub state: String,
    pub error: Option<String>,
    pub primary_label: String,
    pub primary_value: String,
    pub primary_percent: Option<i32>,
    pub secondary_label: String,
    pub secondary_value: String,
    pub secondary_percent: Option<i32>,
    pub detail: String,
    pub footnote: String,
}

pub fn loading_cards() -> Vec<Card> {
    vec![
        loading("codex", "Codex", "Janela de 5 horas", "Janela semanal"),
        loading(
            "claude",
            "Claude Code",
            "Janela de 5 horas",
            "Janela semanal",
        ),
        loading("cursor", "Cursor", "Uso do plano", "Uso da API"),
        loading("kiro", "Kiro", "Créditos restantes", "Créditos usados"),
        loading("deepseek", "DeepSeek", "Saldo", "Gasto monitorado"),
        loading("mimo", "MiMo", "Saldo", "Gasto total"),
        loading("openai", "OpenAI", "Gasto no mês", "Gasto hoje"),
    ]
}

pub fn error_card(id: &str, title: &str, primary: &str, secondary: &str, message: &str) -> Card {
    let notice = explain(message);
    Card {
        id: id.to_string(),
        title: title.to_string(),
        loading: false,
        available: false,
        state: notice.state.to_string(),
        error: (!notice.technical.is_empty()).then_some(notice.technical),
        primary_label: primary.to_string(),
        primary_value: "—".to_string(),
        primary_percent: None,
        secondary_label: secondary.to_string(),
        secondary_value: "—".to_string(),
        secondary_percent: None,
        detail: notice.summary,
        footnote: notice.action,
    }
}

pub fn build_codex_card(usage: &CodexUsage) -> Card {
    let plan = usage.plan_type.as_deref().map(capitalize);
    Card {
        id: "codex".to_string(),
        title: "Codex".to_string(),
        loading: false,
        state: "ready".to_string(),
        available: usage.short_window.is_some() || usage.weekly.is_some(),
        error: None,
        primary_label: window_label(
            usage
                .short_window
                .as_ref()
                .and_then(|window| window.window_seconds),
        ),
        primary_value: window_text(usage.short_window.as_ref()),
        primary_percent: usage.short_window.as_ref().and_then(window_percent),
        secondary_label: "Janela semanal".to_string(),
        secondary_value: window_text(usage.weekly.as_ref()),
        secondary_percent: usage.weekly.as_ref().and_then(window_percent),
        detail: plan
            .map(|plan| format!("Assinatura {plan}"))
            .unwrap_or_else(|| "Limites de uso do Codex".to_string()),
        footnote: codex_resets(usage),
    }
}

pub fn build_claude_card(usage: &ClaudeUsage) -> Card {
    Card {
        id: "claude".to_string(),
        title: "Claude Code".to_string(),
        loading: false,
        state: "ready".to_string(),
        available: usage.five_hour.is_some() || usage.seven_day.is_some(),
        error: None,
        primary_label: "Janela de 5 horas".to_string(),
        primary_value: claude_text(usage.five_hour.as_ref()),
        primary_percent: usage.five_hour.as_ref().map(claude_percent),
        secondary_label: "Janela semanal".to_string(),
        secondary_value: claude_text(usage.seven_day.as_ref()),
        secondary_percent: usage.seven_day.as_ref().map(claude_percent),
        detail: usage
            .plan
            .as_deref()
            .map(|plan| format!("Assinatura {plan}"))
            .unwrap_or_else(|| "Assinatura Claude Code".to_string()),
        footnote: claude_resets(usage),
    }
}

pub fn build_cursor_card(usage: &CursorUsage) -> Card {
    let total_remaining = remaining_percent(usage.total_percent_used);
    let api_remaining = usage.api_percent_used.map(remaining_percent);
    Card {
        id: "cursor".to_string(),
        title: "Cursor".to_string(),
        loading: false,
        state: "ready".to_string(),
        available: total_remaining > 0,
        error: None,
        primary_label: "Uso do plano".to_string(),
        primary_value: percent_text(Some(total_remaining)),
        primary_percent: Some(total_remaining),
        secondary_label: "Uso da API".to_string(),
        secondary_value: percent_text(api_remaining),
        secondary_percent: api_remaining,
        detail: usage
            .plan
            .as_deref()
            .map(|plan| format!("Plano {plan}"))
            .unwrap_or_else(|| "Uso do Cursor".to_string()),
        footnote: cursor_footnote(usage),
    }
}

pub fn build_kiro_card(usage: &KiroUsage) -> Card {
    let percent = usage.percent_remaining();
    let mut parts = Vec::new();
    if let Some(text) = format_local(usage.next_reset, "%d/%m/%Y") {
        parts.push(format!("Renova em {text}"));
    }
    if usage.overage_enabled {
        parts.push(format!(
            "excedente {} de {} · {}",
            format_credits(usage.overage_used),
            format_credits(usage.overage_cap),
            format_money(usage.overage_charges, &usage.currency)
        ));
    } else {
        parts.push("excedente desativado".to_string());
    }
    Card {
        id: "kiro".to_string(),
        title: "Kiro".to_string(),
        loading: false,
        state: "ready".to_string(),
        available: percent > 0,
        error: None,
        primary_label: "Créditos restantes".to_string(),
        primary_value: format!("{percent}% restantes"),
        primary_percent: Some(percent),
        secondary_label: "Créditos usados".to_string(),
        secondary_value: format!(
            "{} de {}",
            format_credits(usage.used),
            format_credits(usage.limit)
        ),
        secondary_percent: None,
        detail: usage
            .plan
            .as_deref()
            .map(|plan| format!("Plano {plan}"))
            .unwrap_or_else(|| "Assinatura Kiro".to_string()),
        footnote: parts.join(" · "),
    }
}

pub fn build_deepseek_card(
    balance: &DeepSeekBalance,
    spend: &SpendSnapshot,
    baseline: Option<Decimal>,
) -> Card {
    let spent_total = spend.spent + baseline.unwrap_or(Decimal::ZERO);
    let footnote = if !balance.is_available {
        "Conta sem saldo disponível para uso".to_string()
    } else if let Some(text) = format_local(spend.since, "%d/%m/%Y %H:%M") {
        format!("Monitorado desde {text}")
    } else {
        "Monitorando…".to_string()
    };
    Card {
        id: "deepseek".to_string(),
        title: "DeepSeek".to_string(),
        loading: false,
        state: "ready".to_string(),
        available: balance.is_available,
        error: None,
        primary_label: "Saldo".to_string(),
        primary_value: format_money(balance.total, &balance.currency),
        primary_percent: None,
        secondary_label: if baseline.is_some() {
            "Gasto total".to_string()
        } else {
            "Gasto monitorado".to_string()
        },
        secondary_value: format_money(spent_total, &balance.currency),
        secondary_percent: None,
        detail: format!(
            "Concedido {} · Recarregado {}",
            format_money(balance.granted, &balance.currency),
            format_money(balance.topped_up, &balance.currency)
        ),
        footnote,
    }
}

pub fn build_mimo_card(snapshot: &MiMoSnapshot) -> Card {
    let month = format!(
        "Mês {}",
        format_money(snapshot.month_cost, &snapshot.currency)
    );
    let detail = match snapshot.recharged {
        Some(recharged) => format!(
            "Recarregado {} · {month}",
            format_money(recharged, &snapshot.currency)
        ),
        None => month,
    };
    Card {
        id: "mimo".to_string(),
        title: "MiMo".to_string(),
        loading: false,
        state: "ready".to_string(),
        available: snapshot.balance > Decimal::ZERO,
        error: None,
        primary_label: "Saldo".to_string(),
        primary_value: format_money(snapshot.balance, &snapshot.currency),
        primary_percent: None,
        secondary_label: "Gasto total".to_string(),
        secondary_value: format_money(snapshot.total_cost, &snapshot.currency),
        secondary_percent: None,
        detail,
        footnote: format!("Sessão do Chrome · {}", token_text(snapshot.total_tokens)),
    }
}

pub fn build_openai_card(costs: &OpenAICosts, credits_purchased: Option<Decimal>) -> Card {
    let since = format_local(Some(costs.tracked_since), "%d/%m/%Y")
        .unwrap_or_else(|| "data indisponível".to_string());
    let today = format_money(costs.today, &costs.currency);
    if let Some(credits) = credits_purchased {
        let balance = credits - costs.tracked;
        return Card {
            id: "openai".to_string(),
            title: "OpenAI".to_string(),
            loading: false,
            state: "ready".to_string(),
            available: balance > Decimal::ZERO,
            error: None,
            primary_label: "Saldo".to_string(),
            primary_value: format_money(balance, &costs.currency),
            primary_percent: None,
            secondary_label: "Gasto no mês".to_string(),
            secondary_value: format_money(costs.month, &costs.currency),
            secondary_percent: None,
            detail: format!(
                "Hoje {today} · Desde {since} {}",
                format_money(costs.tracked, &costs.currency)
            ),
            footnote: format!(
                "Créditos informados {} · custos da organização",
                format_money(credits, &costs.currency)
            ),
        };
    }
    Card {
        id: "openai".to_string(),
        title: "OpenAI".to_string(),
        loading: false,
        state: "ready".to_string(),
        available: true,
        error: None,
        primary_label: "Gasto no mês".to_string(),
        primary_value: format_money(costs.month, &costs.currency),
        primary_percent: None,
        secondary_label: "Gasto hoje".to_string(),
        secondary_value: today,
        secondary_percent: None,
        detail: format!("Período desde {since}"),
        footnote: "Custos da organização (Admin key)".to_string(),
    }
}

fn loading(id: &str, title: &str, primary: &str, secondary: &str) -> Card {
    Card {
        id: id.to_string(),
        title: title.to_string(),
        loading: true,
        state: "loading".to_string(),
        available: false,
        error: None,
        primary_label: primary.to_string(),
        primary_value: "…".to_string(),
        primary_percent: None,
        secondary_label: secondary.to_string(),
        secondary_value: "…".to_string(),
        secondary_percent: None,
        detail: String::new(),
        footnote: String::new(),
    }
}

fn window_label(seconds: Option<i64>) -> String {
    if let Some(seconds) = seconds {
        if seconds > 0 && seconds % 3600 == 0 {
            let hours = seconds / 3600;
            let unit = if hours == 1 { "hora" } else { "horas" };
            return format!("Janela de {hours} {unit}");
        }
    }
    "Janela curta".to_string()
}

fn window_percent(window: &UsageWindow) -> Option<i32> {
    window.used_percent.map(remaining_percent)
}

fn window_text(window: Option<&UsageWindow>) -> String {
    percent_text(window.and_then(window_percent))
}

fn claude_percent(window: &ClaudeWindow) -> i32 {
    remaining_percent(window.utilization)
}

fn claude_text(window: Option<&ClaudeWindow>) -> String {
    percent_text(window.map(claude_percent))
}

fn remaining_percent(used: f64) -> i32 {
    round_half_even((100.0 - used).max(0.0)).clamp(0, 100)
}

fn percent_text(percent: Option<i32>) -> String {
    match percent {
        Some(percent) => format!("{percent}% restantes"),
        None => "Indisponível".to_string(),
    }
}

fn round_half_even(value: f64) -> i32 {
    if !value.is_finite() {
        return 0;
    }
    let floor = value.floor();
    let fraction = value - floor;
    let rounded = if (fraction - 0.5).abs() < 1e-10 {
        if (floor as i64).rem_euclid(2) == 0 {
            floor
        } else {
            floor + 1.0
        }
    } else {
        value.round()
    };
    rounded as i32
}

fn codex_resets(usage: &CodexUsage) -> String {
    let mut parts = Vec::new();
    if let Some(text) = usage
        .short_window
        .as_ref()
        .and_then(|window| format_local(window.reset_at, "%H:%M"))
    {
        parts.push(format!("5h {text}"));
    }
    if let Some(text) = usage
        .weekly
        .as_ref()
        .and_then(|window| format_local(window.reset_at, "%d/%m %H:%M"))
    {
        parts.push(format!("7d {text}"));
    }
    if parts.is_empty() {
        "Redefinição indisponível".to_string()
    } else {
        parts.join(" · ")
    }
}

fn claude_resets(usage: &ClaudeUsage) -> String {
    let mut parts = Vec::new();
    for (label, window) in [
        ("5h", usage.five_hour.as_ref()),
        ("7d", usage.seven_day.as_ref()),
    ] {
        if let Some(text) = window.and_then(|window| format_local(window.resets_at, "%d/%m %H:%M"))
        {
            parts.push(format!("{label} {text}"));
        }
    }
    if parts.is_empty() {
        "Redefinição indisponível".to_string()
    } else {
        parts.join(" · ")
    }
}

fn cursor_footnote(usage: &CursorUsage) -> String {
    let spent = Decimal::from(usage.spent_cents) / Decimal::from(100);
    let mut parts = vec![format_money(spent, "USD")];
    if let Some(limit) = usage.limit_cents {
        let limit = Decimal::from(limit) / Decimal::from(100);
        parts[0] = format!("{} de {}", parts[0], format_money(limit, "USD"));
    }
    if let Some(text) = format_local(usage.cycle_end, "%d/%m") {
        parts.push(format!("renova {text}"));
    }
    parts.join(" · ")
}

fn format_local(epoch: Option<i64>, pattern: &str) -> Option<String> {
    let local = DateTime::from_timestamp(epoch?, 0)?.with_timezone(&Local);
    Some(local.format(pattern).to_string())
}

struct Notice {
    state: &'static str,
    summary: String,
    action: String,
    technical: String,
}

fn explain(message: &str) -> Notice {
    let message = shorten_home(message.trim());
    if is_session(&message) {
        return explain_session(&message);
    }
    if message.contains("não configurad")
        || message.contains("Auth file not found")
        || message.contains("not configured")
    {
        return explain_missing_key(&message);
    }
    if message.contains("não encontrado") || message.contains("not found") {
        return explain_missing_file(&message);
    }
    Notice {
        state: "error",
        summary: "Não foi possível ler".to_string(),
        action: message,
        technical: String::new(),
    }
}

fn is_session(message: &str) -> bool {
    let lower = message.to_lowercase();
    lower.contains("sessão")
        || lower.contains("session")
        || lower.contains("faça login")
        || lower.contains("cookies")
        || lower.contains("unauthorized")
        || lower.contains("expirad")
}

fn explain_session(message: &str) -> Notice {
    let summary = if message.to_lowercase().contains("expirad") {
        "Sessão expirada"
    } else {
        "Sem sessão"
    };
    let technical = if message.to_lowercase().contains("cookie") {
        "Cookies do Chrome não encontrados".to_string()
    } else {
        String::new()
    };
    Notice {
        state: "session",
        summary: summary.to_string(),
        action: session_action(message),
        technical,
    }
}

fn session_action(message: &str) -> String {
    if message.contains("kiro-cli") {
        return "Rode kiro-cli login.".to_string();
    }
    if let Some(url) = message
        .split_whitespace()
        .find(|part| part.starts_with("http"))
    {
        return format!("Abra o Chrome ou o Firefox e entre em {}.", host_of(url));
    }
    if message.contains("Cursor") {
        return "Abra o Cursor e entre de novo.".to_string();
    }
    if message.contains("Codex")
        || message.to_lowercase().contains("unauthorized")
        || message.contains("auth.json")
    {
        return "Entre de novo no Codex.".to_string();
    }
    if let Some(host) = domain_token(message) {
        return format!("Entre de novo em {host}.");
    }
    "Entre de novo para continuar a leitura.".to_string()
}

fn explain_missing_key(message: &str) -> Notice {
    let action = if message.contains("DeepSeek") {
        "Defina a chave da API para ver o saldo."
    } else if message.contains("OpenAI") || message.contains("Admin key") {
        "Defina a admin key para ver os custos."
    } else if message.contains("Auth file") || message.contains("auth.json") {
        "Entre no Codex para gravar a autenticação."
    } else {
        "Falta uma credencial para consultar este serviço."
    };
    Notice {
        state: "setup",
        summary: "Não configurado".to_string(),
        action: action.to_string(),
        technical: key_detail(message),
    }
}

fn key_detail(message: &str) -> String {
    if let Some(start) = message.find('(') {
        if let Some(end) = message.rfind(')') {
            if end > start {
                let inner = &message[start + 1..end];
                if let Some(comma) = inner.find(", ") {
                    return inner[comma + 2..].to_string();
                }
                return inner.to_string();
            }
        }
    }
    message
        .split_once(": ")
        .map(|(_, path)| path.to_string())
        .unwrap_or_default()
}

fn explain_missing_file(message: &str) -> Notice {
    let action = if message.contains("Kiro") {
        "Rode kiro-cli login para criar a sessão local."
    } else if message.contains("Cursor") {
        "Abra o Cursor nesta máquina para gravar a sessão."
    } else if message.contains("Auth") || message.contains("auth") {
        "Entre no Codex para gravar a autenticação."
    } else {
        "O arquivo esperado não está nesta máquina."
    };
    let technical = message
        .rsplit_once(" em ")
        .or_else(|| message.rsplit_once(": "))
        .map(|(_, path)| path.trim().to_string())
        .filter(|path| path != message)
        .unwrap_or_default();
    Notice {
        state: "setup",
        summary: "Não configurado".to_string(),
        action: action.to_string(),
        technical,
    }
}

fn host_of(url: &str) -> &str {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches(['/', '.', ';', ','])
}

fn domain_token(message: &str) -> Option<&str> {
    message
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | ',' | ':' | '(' | ')'))
        .map(|token| token.trim_matches(|c: char| matches!(c, '.' | '`' | '/')))
        .find(|token| token.contains('.') && !token.contains('/') && !token.starts_with('~'))
}

fn shorten_home(message: &str) -> String {
    let home = crate::paths::home_dir().to_string_lossy().into_owned();
    normalize_path_text(&replace_home(message, &home))
}

fn replace_home(message: &str, home: &str) -> String {
    if home.is_empty() || home == "." {
        message.to_string()
    } else {
        message.replace(home, "~")
    }
}

fn normalize_path_text(message: &str) -> String {
    message.replace('\\', "/")
}

fn capitalize(value: &str) -> String {
    let mut chars = value.trim().chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(message: &str) -> Card {
        error_card("demo", "Demo", "A", "B", message)
    }

    #[test]
    fn missing_api_key_is_a_quiet_setup_notice() {
        let home = crate::paths::home_dir();
        let message = format!(
            "DeepSeek API key não configurada (--deepseek-api-key, DEEPSEEK_API_KEY em {} ou {})",
            home.join(".env").display(),
            home.join(".config/toknix/config.json").display()
        );
        let card = card(&message);
        assert_eq!(card.state, "setup");
        assert_eq!(card.detail, "Não configurado");
        assert_eq!(card.footnote, "Defina a chave da API para ver o saldo.");
        assert_eq!(
            card.error.as_deref(),
            Some("DEEPSEEK_API_KEY em ~/.env ou ~/.config/toknix/config.json")
        );
    }

    #[test]
    fn missing_local_database_points_at_the_next_step() {
        let home = crate::paths::home_dir();
        let message = format!(
            "Banco do Kiro CLI não encontrado em {}",
            home.join(".local/share/kiro-cli/data.sqlite3").display()
        );
        let card = card(&message);
        assert_eq!(card.state, "setup");
        assert_eq!(card.detail, "Não configurado");
        assert_eq!(
            card.footnote,
            "Rode kiro-cli login para criar a sessão local."
        );
        assert_eq!(
            card.error.as_deref(),
            Some("~/.local/share/kiro-cli/data.sqlite3")
        );
    }

    #[test]
    fn chrome_login_is_a_session_notice() {
        let card = card(
            "Sessão do Claude no Chrome indisponível: Banco de cookies do Chrome não encontrado; abra o Chrome e faça login em https://claude.ai",
        );
        assert_eq!(card.state, "session");
        assert_eq!(card.detail, "Sem sessão");
        assert_eq!(
            card.footnote,
            "Abra o Chrome ou o Firefox e entre em claude.ai."
        );
        assert_eq!(
            card.error.as_deref(),
            Some("Cookies do Chrome não encontrados")
        );
    }

    #[test]
    fn expired_cli_session_names_the_command() {
        let card = card("Sessão do Kiro expirada, rode `kiro-cli login`");
        assert_eq!(card.state, "session");
        assert_eq!(card.detail, "Sessão expirada");
        assert_eq!(card.footnote, "Rode kiro-cli login.");
        assert!(card.error.is_none());
    }

    #[test]
    fn windows_home_prefixes_use_forward_slashes() {
        let message = r"DEEPSEEK_API_KEY em C:\Users\runner\.env ou C:\Users\runner\.config\toknix\config.json";
        assert_eq!(
            normalize_path_text(&replace_home(message, r"C:\Users\runner")),
            "DEEPSEEK_API_KEY em ~/.env ou ~/.config/toknix/config.json"
        );
    }

    #[test]
    fn transport_failures_stay_errors() {
        let card = card("HTTP 500 ao consultar saldo DeepSeek");
        assert_eq!(card.state, "error");
        assert_eq!(card.detail, "Não foi possível ler");
        assert_eq!(card.footnote, "HTTP 500 ao consultar saldo DeepSeek");
        assert!(card.error.is_none());
    }
}
