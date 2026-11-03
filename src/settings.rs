use crate::config::{self, WidgetConfig};
use crate::envfile::{self, resolve_secret};
use crate::paths::{self, data_home, expand_path, state_home};
use chrono::NaiveDate;
use clap::Parser;
use rust_decimal::Decimal;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 4780;
pub const DEFAULT_CODEX_BASE: &str = "https://chatgpt.com/backend-api";
pub const DEFAULT_DEEPSEEK_BASE: &str = "https://api.deepseek.com";
pub const DEFAULT_OPENAI_BASE: &str = "https://api.openai.com";
pub const DEFAULT_KIRO_BASE: &str = "https://codewhisperer.us-east-1.amazonaws.com";
pub const DEFAULT_CURSOR_BASE: &str = "https://api2.cursor.sh";
pub const DEFAULT_MIMO_BASE: &str = "https://platform.xiaomimimo.com";
pub const DEFAULT_AUTH_BASE: &str = "https://auth.openai.com";

#[derive(Debug, Parser)]
#[command(
    name = "toknix",
    version,
    about = "Local web dashboard for AI token and quota usage"
)]
pub struct Cli {
    /// Codex auth file. Defaults to ~/.codex/auth.json.
    #[arg(long, env = "CODEX_AUTH_FILE")]
    pub auth_file: Option<PathBuf>,

    /// Codex usage API origin.
    #[arg(long, env = "CODEX_USAGE_BASE_URL", default_value = DEFAULT_CODEX_BASE)]
    pub base_url: String,

    /// How often the dashboard refreshes, in seconds.
    #[arg(long, default_value_t = 60)]
    pub refresh_seconds: u64,

    /// Request timeout in seconds.
    #[arg(long, default_value_t = 20)]
    pub timeout: u64,

    #[arg(long)]
    pub config_file: Option<PathBuf>,

    #[arg(long)]
    pub env_file: Option<PathBuf>,

    #[arg(long)]
    pub deepseek_api_key: Option<String>,

    #[arg(long)]
    pub deepseek_base_url: Option<String>,

    #[arg(long)]
    pub deepseek_state_file: Option<PathBuf>,

    /// DeepSeek spend that happened before monitoring started.
    #[arg(long, value_parser = parse_decimal)]
    pub deepseek_spent_baseline: Option<Decimal>,

    #[arg(long)]
    pub mimo_chrome_profile: Option<String>,

    #[arg(long)]
    pub claude_chrome_profile: Option<String>,

    #[arg(long)]
    pub openai_admin_key: Option<String>,

    /// Purchased OpenAI credits. Balance is this amount minus tracked spend.
    #[arg(long, value_parser = parse_decimal)]
    pub openai_credits_purchased: Option<Decimal>,

    /// Date (YYYY-MM-DD) from which OpenAI spend is summed for the balance.
    #[arg(long, value_parser = parse_date)]
    pub openai_credits_since: Option<NaiveDate>,

    #[arg(long)]
    pub kiro_token_db: Option<PathBuf>,

    #[arg(long)]
    pub cursor_state_db: Option<PathBuf>,

    /// Address to bind. Defaults to loopback.
    #[arg(long, default_value = "127.0.0.1", value_parser = parse_host)]
    pub host: IpAddr,

    /// Preferred port. The next free port is used when this one is taken.
    #[arg(long, default_value_t = DEFAULT_PORT)]
    pub port: u16,

    /// Do not open a browser.
    #[arg(long)]
    pub no_open: bool,

    /// Print one text summary and exit.
    #[arg(long)]
    pub summary: bool,

    /// Print one JSON report and exit.
    #[arg(long)]
    pub json: bool,
}

#[derive(Clone)]
pub struct Settings {
    pub auth_file: PathBuf,
    pub codex_base_url: String,
    pub auth_base_url: String,
    pub deepseek_api_key: Option<String>,
    pub deepseek_base_url: String,
    pub deepseek_state_file: PathBuf,
    pub deepseek_spent_baseline: Option<Decimal>,
    pub mimo_chrome_profile: Option<String>,
    pub mimo_base_url: String,
    pub claude_chrome_profile: Option<String>,
    pub kiro_token_db: PathBuf,
    pub kiro_base_url: String,
    pub cursor_state_db: PathBuf,
    pub cursor_base_url: String,
    pub openai_admin_key: Option<String>,
    pub openai_credits_purchased: Option<Decimal>,
    pub openai_credits_since: Option<NaiveDate>,
    pub openai_base_url: String,
    pub config_path: PathBuf,
    pub env_file: PathBuf,
    pub timeout: Duration,
}

impl Settings {
    pub fn from_cli(cli: &Cli) -> Self {
        let env_file = cli
            .env_file
            .as_ref()
            .map(|path| expand_path(path))
            .unwrap_or_else(envfile::default_env_file);
        let config_path = cli
            .config_file
            .as_ref()
            .map(|path| expand_path(path))
            .unwrap_or_else(config::default_config_path);
        let config = config::load_config(&config_path);
        Self::assemble(cli, env_file, config_path, config)
    }

    fn assemble(cli: &Cli, env_file: PathBuf, config_path: PathBuf, config: WidgetConfig) -> Self {
        let secret = |names: &[&str]| resolve_secret(names, &env_file);
        let deepseek_api_key = nonempty(cli.deepseek_api_key.as_deref())
            .or_else(|| secret(&["DEEPSEEK_API_KEY"]))
            .or(config.deepseek_api_key);
        let deepseek_base_url = nonempty(cli.deepseek_base_url.as_deref())
            .or_else(|| secret(&["DEEPSEEK_BASE_URL"]))
            .or(config.deepseek_base_url)
            .unwrap_or_else(|| DEFAULT_DEEPSEEK_BASE.to_string());
        let deepseek_state_file = cli
            .deepseek_state_file
            .as_ref()
            .map(|path| expand_path(path))
            .or(config.deepseek_state_file)
            .unwrap_or_else(default_deepseek_state_file);
        let openai_admin_key = nonempty(cli.openai_admin_key.as_deref())
            .or_else(|| secret(&["OPENAI_ADMIN_API_KEY", "OPENAI_ADMIN_KEY"]))
            .or(config.openai_admin_key);
        Self {
            auth_file: cli
                .auth_file
                .as_ref()
                .map(|path| expand_path(path))
                .unwrap_or_else(default_auth_file),
            codex_base_url: cli.base_url.trim().trim_end_matches('/').to_string(),
            auth_base_url: DEFAULT_AUTH_BASE.to_string(),
            deepseek_api_key,
            deepseek_base_url,
            deepseek_state_file,
            deepseek_spent_baseline: cli
                .deepseek_spent_baseline
                .or(config.deepseek_spent_baseline),
            mimo_chrome_profile: nonempty(cli.mimo_chrome_profile.as_deref())
                .or(config.mimo_chrome_profile),
            mimo_base_url: DEFAULT_MIMO_BASE.to_string(),
            claude_chrome_profile: nonempty(cli.claude_chrome_profile.as_deref())
                .or(config.claude_chrome_profile),
            kiro_token_db: cli
                .kiro_token_db
                .as_ref()
                .map(|path| expand_path(path))
                .unwrap_or_else(default_kiro_db),
            kiro_base_url: DEFAULT_KIRO_BASE.to_string(),
            cursor_state_db: cli
                .cursor_state_db
                .as_ref()
                .map(|path| expand_path(path))
                .unwrap_or_else(default_cursor_db),
            cursor_base_url: DEFAULT_CURSOR_BASE.to_string(),
            openai_admin_key,
            openai_credits_purchased: cli
                .openai_credits_purchased
                .or(config.openai_credits_purchased),
            openai_credits_since: cli.openai_credits_since.or(config.openai_credits_since),
            openai_base_url: DEFAULT_OPENAI_BASE.to_string(),
            config_path,
            env_file,
            timeout: Duration::from_secs(cli.timeout.max(1)),
        }
    }
}

pub fn default_auth_file() -> PathBuf {
    paths::home_dir().join(".codex").join("auth.json")
}

pub fn default_deepseek_state_file() -> PathBuf {
    prefer_existing(
        state_home().join("toknix").join("deepseek-spend.json"),
        state_home()
            .join("codex-widget")
            .join("deepseek-spend.json"),
    )
}

pub fn default_kiro_db() -> PathBuf {
    data_home().join("kiro-cli").join("data.sqlite3")
}

pub fn default_cursor_db() -> PathBuf {
    cursor_user_dir()
        .join("User")
        .join("globalStorage")
        .join("state.vscdb")
}

fn cursor_user_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        appdata_dir().join("Cursor")
    }
    #[cfg(target_os = "macos")]
    {
        paths::home_dir()
            .join("Library")
            .join("Application Support")
            .join("Cursor")
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        paths::config_home().join("Cursor")
    }
}

#[cfg(target_os = "windows")]
fn appdata_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| paths::home_dir().join("AppData").join("Roaming"))
}

fn prefer_existing(current: PathBuf, legacy: PathBuf) -> PathBuf {
    if current.is_file() {
        current
    } else if legacy.is_file() {
        legacy
    } else {
        current
    }
}

fn nonempty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn parse_decimal(value: &str) -> Result<Decimal, String> {
    value
        .parse()
        .map_err(|_| format!("valor decimal inválido: {value}"))
}

fn parse_date(value: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| format!("data inválida (use AAAA-MM-DD): {value}"))
}

fn parse_host(value: &str) -> Result<IpAddr, String> {
    if value.eq_ignore_ascii_case("localhost") {
        return Ok(IpAddr::from([127, 0, 0, 1]));
    }
    value.parse().map_err(|_| format!("host inválido: {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_state_db_uses_the_platform_user_data_dir() {
        let path = default_cursor_db();
        let text = path.to_string_lossy().replace('\\', "/");
        assert!(
            text.ends_with("/Cursor/User/globalStorage/state.vscdb"),
            "{text}"
        );
        #[cfg(target_os = "windows")]
        {
            let expected = appdata_dir()
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb");
            assert_eq!(path, expected);
        }
        #[cfg(target_os = "macos")]
        {
            assert!(
                text.contains("/Library/Application Support/Cursor/"),
                "{text}"
            );
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            let expected = paths::config_home()
                .join("Cursor")
                .join("User")
                .join("globalStorage")
                .join("state.vscdb");
            assert_eq!(path, expected);
        }
    }
}
