use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

use anyhow::{bail, Context, Result};
use apibreak_core::{Proxy, ProxyConfig};
use clap::{ArgAction, Parser};
use tokio::net::TcpListener;

const DEFAULT_PORT: u16 = 4000;

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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let Some(target) = cli.target else {
        bail!("no backend set: pass --target https://api.example.com");
    };
    let port = cli.port.unwrap_or(DEFAULT_PORT);
    let proxy = Proxy::new(ProxyConfig {
        target: target.clone(),
        cors: cli.cors,
    })?;

    let mut tasks = tokio::task::JoinSet::new();
    for listener in bind(port).await? {
        tasks.spawn(proxy.clone().serve(listener));
    }
    println!("apibreak listening on http://localhost:{port} -> {target}");
    tokio::select! {
        result = tokio::signal::ctrl_c() => result.context("waiting for Ctrl-C")?,
        Some(result) = tasks.join_next() => result.context("proxy task failed")??,
    }
    Ok(())
}
