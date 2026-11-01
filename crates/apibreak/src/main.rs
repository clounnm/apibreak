mod tui;

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use apibreak_core::scenario::{parse_duration, DEFAULT_DELAY};
use apibreak_core::store::DEFAULT_CAPACITY;
use apibreak_core::{MockStore, Proxy, ProxyConfig, ScenarioHandle, ScenarioSet, Store};
use clap::{ArgAction, Parser};
use tokio::net::TcpListener;

const DEFAULT_PORT: u16 = 4000;
const CONFIG_DIR: &str = ".apibreak";

/// Break your API with one key: no certificates, no scripts.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Cli {
    /// Backend to proxy to, e.g. https://api.example.com
    #[arg(long)]
    target: Option<String>,
    /// Local port to listen on
    #[arg(long)]
    port: Option<u16>,
    /// Add CORS headers for localhost origins and answer preflights
    #[arg(long, default_value_t = true, action = ArgAction::Set, num_args = 0..=1,
          default_missing_value = "true", require_equals = true)]
    cors: bool,
    /// How many requests to keep in memory
    #[arg(long, default_value_t = DEFAULT_CAPACITY)]
    history: usize,
    /// Delay applied by the `d` key, e.g. 500ms, 3s, 2m
    #[arg(long, value_parser = parse_duration)]
    delay: Option<Duration>,
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    let Some(target) = cli.target else {
        bail!("no backend set: pass --target https://api.example.com");
    };
    let port = cli.port.unwrap_or(DEFAULT_PORT);
    let store = Arc::new(Store::new(cli.history));
    let config = ProxyConfig {
        target: target.clone(),
        cors: cli.cors,
        config_dir: CONFIG_DIR.into(),
    };
    let scenarios = ScenarioHandle::new(ScenarioSet::default());
    let proxy = Proxy::new(config, store.clone(), scenarios.clone())?;

    let runtime = tokio::runtime::Runtime::new().context("cannot start the runtime")?;
    let listeners = runtime.block_on(bind(port))?;
    for listener in listeners {
        runtime.spawn(proxy.clone().serve(listener));
    }
    let delay = cli.delay.unwrap_or(DEFAULT_DELAY);
    let mocks = MockStore::new(CONFIG_DIR);
    tui::run(
        store,
        scenarios,
        mocks,
        delay,
        format!(":{port} -> {target}"),
    )
}
