mod headless;
mod tui;

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use apibreak_core::scenario::{parse_duration, sync_with_file, DEFAULT_DELAY};
use apibreak_core::store::DEFAULT_CAPACITY;
use apibreak_core::{Config, MockStore, Proxy, ProxyConfig, ScenarioHandle, Store};
use clap::{ArgAction, Parser};
use tokio::net::TcpListener;
use tokio::task::JoinSet;

const DEFAULT_PORT: u16 = 4000;
const CONFIG_DIR: &str = ".apibreak";
const CONFIG_FILE: &str = ".apibreak/scenarios.yaml";

/// Break your API with one key: no certificates, no scripts.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Backend to proxy to, e.g. https://api.example.com. Overrides the config file
    #[arg(long)]
    target: Option<String>,
    /// Local port to listen on. Overrides the config file
    #[arg(long)]
    port: Option<u16>,
    /// Add CORS headers for localhost origins and answer preflights
    #[arg(long, default_value_t = true, action = ArgAction::Set, num_args = 0..=1,
          default_missing_value = "true", require_equals = true)]
    cors: bool,
    /// How many requests to keep in memory
    #[arg(long, default_value_t = DEFAULT_CAPACITY)]
    history: usize,
    /// Delay applied by the `d` key, e.g. 500ms, 3s, 2m. Overrides the config file
    #[arg(long, value_parser = parse_duration)]
    delay: Option<Duration>,
    /// Run without the TUI, using scenarios from the file and logging to stdout
    #[arg(long)]
    headless: bool,
}

/// Binds IPv4 loopback, and IPv6 too because `localhost` may resolve to `::1`.
async fn bind(port: u16) -> Result<Vec<TcpListener>> {
    let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let listener = TcpListener::bind(v4)
        .await
        .with_context(|| format!("cannot listen on {v4} (is another process using the port?)"))?;
    let mut listeners = vec![listener];
    if let Ok(listener) = TcpListener::bind(SocketAddr::from((Ipv6Addr::LOCALHOST, port))).await {
        listeners.push(listener);
    }
    Ok(listeners)
}

/// Works out the settings from flags, then the file, then defaults.
fn settings(cli: &Cli, file: Option<&Config>) -> Result<(String, u16, Duration)> {
    let target = cli
        .target
        .clone()
        .or_else(|| file.and_then(|config| config.target.clone()))
        .context("no backend set: pass --target https://api.example.com or set `target` in .apibreak/scenarios.yaml")?;
    let port = cli
        .port
        .or_else(|| file.and_then(|config| config.port))
        .unwrap_or(DEFAULT_PORT);
    let delay = match (cli.delay, file.and_then(|c| c.default_delay.as_deref())) {
        (Some(delay), _) => delay,
        (None, Some(text)) => parse_duration(text)?,
        (None, None) => DEFAULT_DELAY,
    };
    Ok((target, port, delay))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = Path::new(CONFIG_FILE);
    let file = Config::load(path)?;
    let (target, port, default_delay) = settings(&cli, file.as_ref())?;
    // A new file starts with the settings in use, ready to commit.
    let config = file.unwrap_or_else(|| Config {
        target: Some(target.clone()),
        port: Some(port),
        ..Config::default()
    });

    let store = Arc::new(Store::new(cli.history));
    let scenarios = ScenarioHandle::new(config.scenarios.clone());
    let proxy = Proxy::new(
        ProxyConfig {
            target: target.clone(),
            cors: cli.cors,
            config_dir: CONFIG_DIR.into(),
        },
        store.clone(),
        scenarios.clone(),
    )?;

    // Exists from the start so hot reload can watch it.
    std::fs::create_dir_all(CONFIG_DIR).with_context(|| format!("cannot create {CONFIG_DIR}"))?;
    let runtime = tokio::runtime::Runtime::new().context("cannot start the runtime")?;
    let _runtime_context = runtime.enter();
    let config_status = sync_with_file(scenarios.clone(), path.to_owned(), config)?;

    let listeners = runtime.block_on(bind(port))?;
    let mut serving = JoinSet::new();
    for listener in listeners {
        serving.spawn(proxy.clone().serve(listener));
    }

    let title = format!(":{port} -> {target}");
    if cli.headless {
        return runtime.block_on(headless::run(
            store,
            scenarios,
            config_status,
            serving,
            title,
        ));
    }
    tui::run(tui::Setup {
        store,
        scenarios,
        config_status,
        mocks: MockStore::new(CONFIG_DIR),
        default_delay,
        title,
    })
}
