use crate::cards::{
    build_claude_card, build_codex_card, build_cursor_card, build_deepseek_card, build_kiro_card,
    build_mimo_card, build_openai_card, error_card, loading_cards, Card,
};
use crate::providers::claude;
use crate::providers::codex;
use crate::providers::cursor;
use crate::providers::deepseek;
use crate::providers::kiro;
use crate::providers::mimo;
use crate::providers::openai;
use crate::providers::MemoryCache;
use crate::settings::Settings;
use chrono::Utc;
use reqwest::Client;
use serde::Serialize;
use tokio::sync::OnceCell;

pub const SERVICE_NAME: &str = "toknix";

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub service: &'static str,
    pub generated_at: Option<String>,
    pub version: u64,
    pub cards: Vec<Card>,
}

impl Snapshot {
    pub fn loading() -> Self {
        Self {
            service: SERVICE_NAME,
            generated_at: None,
            version: 0,
            cards: loading_cards(),
        }
    }
}

#[derive(Serialize)]
pub struct StatsResponse {
    pub service: &'static str,
    pub generated_at: Option<String>,
    pub version: u64,
    pub refreshing: bool,
    pub cards: Vec<Card>,
}

impl StatsResponse {
    pub fn from_snapshot(snapshot: &Snapshot, refreshing: bool) -> Self {
        Self {
            service: snapshot.service,
            generated_at: snapshot.generated_at.clone(),
            version: snapshot.version,
            refreshing,
            cards: snapshot.cards.clone(),
        }
    }
}

pub async fn collect_all(settings: &Settings, client: &Client, cache: &MemoryCache) -> Snapshot {
    let keys = OnceCell::new();
    let results = tokio::join!(
        codex_card(settings, client),
        claude_card(settings, client, &keys),
        cursor_card(settings, client, cache),
        kiro_card(settings, client),
        deepseek_card(settings, client),
        mimo_card(settings, client, &keys),
        openai_card(settings, client),
    );
    Snapshot {
        service: SERVICE_NAME,
        generated_at: Some(Utc::now().to_rfc3339()),
        version: 1,
        cards: vec![
            results.0, results.1, results.2, results.3, results.4, results.5, results.6,
        ],
    }
}

pub async fn codex_card(settings: &Settings, client: &Client) -> Card {
    match codex::load(settings, client).await {
        Ok(usage) => build_codex_card(&usage),
        Err(error) => error_card(
            "codex",
            "Codex",
            "Janela de 5 horas",
            "Janela semanal",
            error.message(),
        ),
    }
}

pub async fn claude_card(
    settings: &Settings,
    client: &Client,
    keys: &OnceCell<Vec<Vec<u8>>>,
) -> Card {
    match claude::load(settings, client, keys).await {
        Ok(usage) => build_claude_card(&usage),
        Err(error) => error_card(
            "claude",
            "Claude Code",
            "Janela de 5 horas",
            "Janela semanal",
            error.message(),
        ),
    }
}

pub async fn cursor_card(settings: &Settings, client: &Client, cache: &MemoryCache) -> Card {
    match cursor::load(settings, client, cache).await {
        Ok(usage) => build_cursor_card(&usage),
        Err(error) => error_card(
            "cursor",
            "Cursor",
            "Uso do plano",
            "Uso da API",
            error.message(),
        ),
    }
}

pub async fn kiro_card(settings: &Settings, client: &Client) -> Card {
    match kiro::load(settings, client).await {
        Ok(usage) => build_kiro_card(&usage),
        Err(error) => error_card(
            "kiro",
            "Kiro",
            "Créditos restantes",
            "Créditos usados",
            error.message(),
        ),
    }
}

pub async fn deepseek_card(settings: &Settings, client: &Client) -> Card {
    match deepseek::load(settings, client).await {
        Ok(report) => build_deepseek_card(
            &report.balance,
            &report.spend,
            settings.deepseek_spent_baseline,
        ),
        Err(error) => error_card(
            "deepseek",
            "DeepSeek",
            "Saldo",
            "Gasto monitorado",
            error.message(),
        ),
    }
}

pub async fn mimo_card(
    settings: &Settings,
    client: &Client,
    keys: &OnceCell<Vec<Vec<u8>>>,
) -> Card {
    match mimo::load(settings, client, keys).await {
        Ok(snapshot) => build_mimo_card(&snapshot),
        Err(error) => error_card("mimo", "MiMo", "Saldo", "Gasto total", error.message()),
    }
}

pub async fn openai_card(settings: &Settings, client: &Client) -> Card {
    match openai::load(settings, client).await {
        Ok(costs) => build_openai_card(&costs, settings.openai_credits_purchased),
        Err(error) => error_card(
            "openai",
            "OpenAI",
            "Gasto no mês",
            "Gasto hoje",
            error.message(),
        ),
    }
}

pub fn print_summary(snapshot: &Snapshot) {
    for card in &snapshot.cards {
        println!("{}", card.title);
        if matches!(card.state.as_str(), "setup" | "session" | "error") {
            println!("  {}", card.detail);
            if !card.footnote.is_empty() {
                println!("  {}", card.footnote);
            }
            if let Some(error) = &card.error {
                println!("  {error}");
            }
            println!();
            continue;
        }
        println!("  {}: {}", card.primary_label, card.primary_value);
        println!("  {}: {}", card.secondary_label, card.secondary_value);
        if !card.detail.is_empty() {
            println!("  {}", card.detail);
        }
        if !card.footnote.is_empty() {
            println!("  {}", card.footnote);
        }
        println!();
    }
}
