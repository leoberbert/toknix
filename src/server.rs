use crate::http;
use crate::providers::MemoryCache;
use crate::settings::Settings;
use crate::snapshot::{
    claude_card, codex_card, cursor_card, deepseek_card, kiro_card, mimo_card, openai_card,
    Snapshot, StatsResponse, SERVICE_NAME,
};
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::Router;
use futures_util::stream::{self, StreamExt};
use serde_json::Value;
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch, Mutex, OnceCell};
use tokio_stream::wrappers::BroadcastStream;

const DASHBOARD: &str = include_str!("../assets/dashboard.html");
const INTER: &[u8] = include_bytes!("../assets/inter-latin.woff2");

pub struct ServerOptions {
    pub host: IpAddr,
    pub port: u16,
    pub open_browser: bool,
    pub refresh_every: Duration,
}

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    settings: Settings,
    client: reqwest::Client,
    cache: MemoryCache,
    snapshot: Mutex<Snapshot>,
    refreshing: AtomicBool,
    events: broadcast::Sender<u64>,
    shutdown: watch::Sender<bool>,
}

impl AppState {
    pub fn new(settings: Settings) -> Self {
        let client = http::client(settings.timeout);
        let (events, _) = broadcast::channel(32);
        let (shutdown, _) = watch::channel(false);
        Self {
            inner: Arc::new(Inner {
                settings,
                client,
                cache: MemoryCache::default(),
                snapshot: Mutex::new(Snapshot::loading()),
                refreshing: AtomicBool::new(false),
                events,
                shutdown,
            }),
        }
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/", get(index))
            .route("/fonts/inter-latin.woff2", get(inter_font))
            .route("/api/stats", get(stats))
            .route("/api/status", get(stats))
            .route("/api/events", get(events))
            .route("/api/sync", post(sync_now))
            .with_state(self.clone())
    }

    pub async fn refresh(&self) {
        if self.inner.refreshing.swap(true, Ordering::AcqRel) {
            return;
        }
        let guard = RefreshGuard(&self.inner.refreshing);
        let _ = self.inner.events.send(self.version().await);
        let keys = Arc::new(OnceCell::new());
        let mut tasks = Vec::new();
        tasks.push(self.spawn_card({
            let state = self.clone();
            async move { codex_card(&state.inner.settings, &state.inner.client).await }
        }));
        tasks.push(self.spawn_card({
            let state = self.clone();
            let keys = Arc::clone(&keys);
            async move { claude_card(&state.inner.settings, &state.inner.client, &keys).await }
        }));
        tasks.push(self.spawn_card({
            let state = self.clone();
            async move {
                cursor_card(
                    &state.inner.settings,
                    &state.inner.client,
                    &state.inner.cache,
                )
                .await
            }
        }));
        tasks.push(self.spawn_card({
            let state = self.clone();
            async move { kiro_card(&state.inner.settings, &state.inner.client).await }
        }));
        tasks.push(self.spawn_card({
            let state = self.clone();
            async move { deepseek_card(&state.inner.settings, &state.inner.client).await }
        }));
        tasks.push(self.spawn_card({
            let state = self.clone();
            let keys = Arc::clone(&keys);
            async move { mimo_card(&state.inner.settings, &state.inner.client, &keys).await }
        }));
        tasks.push(self.spawn_card({
            let state = self.clone();
            async move { openai_card(&state.inner.settings, &state.inner.client).await }
        }));
        for task in tasks {
            let _ = task.await;
        }
        drop(guard);
        let _ = self.inner.events.send(self.version().await);
    }

    fn spawn_card<F>(&self, future: F) -> tokio::task::JoinHandle<()>
    where
        F: std::future::Future<Output = crate::cards::Card> + Send + 'static,
    {
        let state = self.clone();
        tokio::spawn(async move {
            let card = future.await;
            state.publish(card).await;
        })
    }

    async fn publish(&self, card: crate::cards::Card) {
        let version = {
            let mut snapshot = self.inner.snapshot.lock().await;
            if let Some(slot) = snapshot.cards.iter_mut().find(|item| item.id == card.id) {
                *slot = card;
            }
            snapshot.generated_at = Some(chrono::Utc::now().to_rfc3339());
            snapshot.version = snapshot.version.saturating_add(1);
            snapshot.version
        };
        let _ = self.inner.events.send(version);
    }

    async fn version(&self) -> u64 {
        self.inner.snapshot.lock().await.version
    }

    async fn response(&self) -> StatsResponse {
        let snapshot = self.inner.snapshot.lock().await.clone();
        StatsResponse::from_snapshot(&snapshot, self.inner.refreshing.load(Ordering::Acquire))
    }
}

struct RefreshGuard<'a>(&'a AtomicBool);

impl Drop for RefreshGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub async fn serve(settings: Settings, options: ServerOptions) -> Result<(), String> {
    if options.host.is_loopback() && options.port != 0 {
        if let Some(url) = running_dashboard(options.port).await {
            println!("Toknix já está em {url}");
            if options.open_browser {
                open_browser(&url);
            }
            return Ok(());
        }
    }
    let (listener, port) = bind(options.host, options.port).await?;
    let url = browser_url(options.host, port);
    println!("Toknix");
    println!("Painel em {url}");
    if !options.host.is_loopback() {
        eprintln!(
            "Aviso: o painel está escutando em {}, acessível por outras máquinas desta rede.",
            options.host
        );
    }
    println!("Ctrl+C encerra o servidor.");
    let state = AppState::new(settings);
    let refresher = state.clone();
    tokio::spawn(async move {
        loop {
            if *refresher.inner.shutdown.borrow() {
                break;
            }
            refresher.refresh().await;
            let mut stop = refresher.inner.shutdown.subscribe();
            tokio::select! {
                _ = tokio::time::sleep(options.refresh_every) => {}
                _ = stop.wait_for(|stop| *stop) => break,
            }
        }
    });
    if options.open_browser {
        let url = url.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            open_browser(&url);
        });
    }
    let stop = state.inner.shutdown.clone();
    axum::serve(listener, state.router())
        .with_graceful_shutdown(async move {
            wait_for_shutdown_signal().await;
            let _ = stop.send(true);
            eprintln!("encerrando");
            // The open SSE response keeps Hyper in graceful shutdown, and the
            // SIGINT handler swallows later Ctrl+C. The flag above ends that
            // stream; this task leaves if the server is still up.
            tokio::spawn(async {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(400)) => {}
                    _ = wait_for_shutdown_signal() => {}
                }
                std::process::exit(0);
            });
        })
        .await
        .map_err(|error| error.to_string())
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let (Ok(mut interrupt), Ok(mut terminate)) = (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
        ) {
            tokio::select! {
                _ = interrupt.recv() => {}
                _ = terminate.recv() => {}
            }
            return;
        }
        let _ = tokio::signal::ctrl_c().await;
    }
    #[cfg(windows)]
    {
        // No parent console means the program was opened from Explorer.
        // Waiting on Ctrl+C would fail immediately and stop the server.
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

async fn index() -> impl IntoResponse {
    ([(header::CACHE_CONTROL, "no-store")], Html(DASHBOARD))
}

async fn inter_font() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "font/woff2"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        INTER,
    )
}

async fn stats(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        axum::Json(state.response().await),
    )
}

async fn sync_now(State(state): State<AppState>) -> StatusCode {
    let state = state.clone();
    tokio::spawn(async move {
        state.refresh().await;
    });
    StatusCode::ACCEPTED
}

async fn events(State(state): State<AppState>) -> impl IntoResponse {
    let current = state.version().await;
    let mut shutdown = state.inner.shutdown.subscribe();
    let first = stream::once(async move {
        Ok::<_, Infallible>(Event::default().event("status").data(current.to_string()))
    });
    let updates =
        BroadcastStream::new(state.inner.events.subscribe()).filter_map(|item| async move {
            item.ok().map(|version| {
                Ok::<_, Infallible>(Event::default().event("status").data(version.to_string()))
            })
        });
    let stream = first.chain(updates).take_until(async move {
        let _ = shutdown.wait_for(|stop| *stop).await;
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

async fn running_dashboard(port: u16) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(400))
        .build()
        .ok()?;
    let response = client
        .get(format!("http://127.0.0.1:{port}/api/status"))
        .send()
        .await
        .ok()?;
    let body: Value = response.json().await.ok()?;
    if body.get("service").and_then(Value::as_str) == Some(SERVICE_NAME) {
        Some(format!("http://127.0.0.1:{port}/"))
    } else {
        None
    }
}

async fn bind(host: IpAddr, start: u16) -> Result<(TcpListener, u16), String> {
    if start == 0 {
        let listener = TcpListener::bind((host, 0))
            .await
            .map_err(|error| error.to_string())?;
        let port = listener
            .local_addr()
            .map_err(|error| error.to_string())?
            .port();
        return Ok((listener, port));
    }
    let mut port = start;
    let mut last = String::from("porta ocupada");
    for attempt in 0..20 {
        if attempt > 0 {
            if port == u16::MAX {
                break;
            }
            port += 1;
        }
        match TcpListener::bind((host, port)).await {
            Ok(listener) => return Ok((listener, port)),
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => last = error.to_string(),
            Err(error) => return Err(error.to_string()),
        }
    }
    Err(format!(
        "não foi possível escutar a partir da porta {start}: {last}"
    ))
}

fn browser_url(host: IpAddr, port: u16) -> String {
    let host = match host {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        other => other,
    };
    match host {
        IpAddr::V6(address) => format!("http://[{address}]:{port}/"),
        IpAddr::V4(address) => format!("http://{address}:{port}/"),
    }
}

fn open_browser(url: &str) {
    if let Err(error) = open::that(url) {
        eprintln!("Não consegui abrir o navegador ({error}). Abra {url}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Cli;
    use axum::body::Body;
    use axum::http::Request;
    use clap::Parser;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn serves_the_dashboard_without_contacting_providers() {
        let cli = Cli::try_parse_from([
            "toknix",
            "--auth-file",
            "/tmp/toknix-missing-auth.json",
            "--config-file",
            "/tmp/toknix-missing-config.json",
            "--env-file",
            "/tmp/toknix-missing.env",
            "--no-open",
        ])
        .unwrap();
        let state = AppState::new(Settings::from_cli(&cli));
        let response = state
            .router()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let html = String::from_utf8(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(html.contains("Toknix"));
        assert!(html.contains("id=\"board\""));
        assert!(html.contains("/fonts/inter-latin.woff2"));

        let font = state
            .router()
            .oneshot(
                Request::builder()
                    .uri("/fonts/inter-latin.woff2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(font.status(), StatusCode::OK);
        assert_eq!(
            font.headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("font/woff2")
        );

        let response = state
            .router()
            .oneshot(
                Request::builder()
                    .uri("/api/stats")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(body["service"], "toknix");
        assert_eq!(body["refreshing"], false);
        assert_eq!(body["cards"].as_array().unwrap().len(), 7);
        assert!(body["cards"]
            .as_array()
            .unwrap()
            .iter()
            .all(|card| card["loading"] == true));
    }

    #[tokio::test]
    async fn an_open_event_stream_does_not_block_shutdown() {
        let cli = Cli::try_parse_from([
            "toknix",
            "--auth-file",
            "/tmp/toknix-missing-auth.json",
            "--config-file",
            "/tmp/toknix-missing-config.json",
            "--env-file",
            "/tmp/toknix-missing.env",
            "--no-open",
        ])
        .unwrap();
        let state = AppState::new(Settings::from_cli(&cli));
        let stop = state.inner.shutdown.clone();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (gate, shutdown) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            axum::serve(listener, state.router())
                .with_graceful_shutdown(async move {
                    let _ = shutdown.await;
                    let _ = stop.send(true);
                })
                .await
                .unwrap();
        });

        let response = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}/api/events"))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success());

        gate.send(()).unwrap();
        let finished = tokio::time::timeout(Duration::from_secs(2), server).await;
        assert!(
            finished.is_ok(),
            "o servidor continuou de pé com o stream de eventos aberto"
        );
        finished.unwrap().unwrap();
        // Keep the response alive until the server has already stopped.
        drop(response);
    }
}
