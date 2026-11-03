use crate::http;
use crate::providers::MemoryCache;
use crate::server::{self, ServerOptions};
use crate::settings::{Cli, Settings};
use crate::snapshot::{self, print_summary, StatsResponse};
use clap::Parser;
use std::time::Duration;

pub async fn run() -> Result<(), String> {
    let mut args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "stats") {
        args.remove(1);
    }
    let cli = Cli::parse_from(args);
    if cli.summary && cli.json {
        return Err("use apenas um entre --summary e --json".into());
    }
    if cli.refresh_seconds < 5 {
        return Err("o intervalo de atualização precisa ser de pelo menos 5 segundos".into());
    }
    let settings = Settings::from_cli(&cli);
    if cli.summary || cli.json {
        let client = http::client(settings.timeout);
        let snapshot = snapshot::collect_all(&settings, &client, &MemoryCache::default()).await;
        if cli.json {
            let report = StatsResponse::from_snapshot(&snapshot, false);
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
            );
        } else {
            print_summary(&snapshot);
        }
        return Ok(());
    }
    server::serve(
        settings,
        ServerOptions {
            host: cli.host,
            port: cli.port,
            open_browser: !cli.no_open,
            refresh_every: Duration::from_secs(cli.refresh_seconds),
        },
    )
    .await
}
