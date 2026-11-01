//! Proxy without the TUI: one log line per finished request on stdout.

use std::sync::Arc;

use anyhow::{Context, Result};
use apibreak_core::{Exchange, Outcome, ScenarioHandle, Store, StoreEvent};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::watch;
use tokio::task::JoinSet;

/// Logs until Ctrl-C, or until the proxy stops by itself.
pub async fn run(
    store: Arc<Store>,
    scenarios: ScenarioHandle,
    mut config_status: watch::Receiver<Option<String>>,
    mut serving: JoinSet<apibreak_core::Result<()>>,
    title: String,
) -> Result<()> {
    println!("apibreak {title}");
    let mut active = scenarios.subscribe();
    print_scenarios(&active.borrow_and_update().summary());
    let mut events = store.subscribe();
    loop {
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                return result.context("cannot wait for Ctrl-C");
            }
            Some(result) = serving.join_next() => {
                return result.context("proxy task failed")?.map_err(Into::into);
            }
            event = events.recv() => match event {
                Ok(StoreEvent::Upsert(exchange)) => {
                    if exchange.outcome != Outcome::Pending {
                        println!("{}", log_line(&exchange));
                    }
                }
                Ok(StoreEvent::Cleared) => {}
                Err(RecvError::Lagged(missed)) => println!("({missed} requests not logged)"),
                Err(RecvError::Closed) => return Ok(()),
            },
            Ok(()) = active.changed() => print_scenarios(&active.borrow_and_update().summary()),
            Ok(()) = config_status.changed() => {
                if let Some(error) = config_status.borrow_and_update().as_ref() {
                    println!("{error}");
                }
            }
        }
    }
}

fn print_scenarios(summary: &[String]) {
    if summary.is_empty() {
        println!("scenarios: none");
    } else {
        println!("scenarios: {}", summary.join(" · "));
    }
}

fn log_line(exchange: &Exchange) -> String {
    let result = match &exchange.outcome {
        Outcome::Response(response) => response.status.to_string(),
        Outcome::Failed(reason) => format!("ERR ({reason})"),
        Outcome::Pending => "...".to_owned(),
    };
    let millis = exchange.duration.unwrap_or_default().as_millis();
    let scenario = exchange
        .scenario
        .as_ref()
        .map_or_else(String::new, |label| format!(" [{label}]"));
    format!(
        "{} {} {} {result} {millis}ms{scenario}",
        exchange.started_at.format("%H:%M:%S"),
        exchange.method,
        exchange.uri,
    )
}

#[cfg(test)]
mod tests {
    use apibreak_core::store::{Captured, NewExchange};

    use super::*;

    #[test]
    fn log_line_shows_failure_and_scenario() {
        let store = Store::new(1);
        let id = store.begin(NewExchange {
            method: "GET".into(),
            path: "/a".into(),
            uri: "/a?b=1".into(),
            request_headers: Vec::new(),
            request_body: Captured::default(),
            scenario: Some("offline".into()),
        });
        store.finish(id, Outcome::Failed("gone".into()), Default::default());
        let line = log_line(&store.snapshot()[0]);
        assert!(
            line.ends_with("GET /a?b=1 ERR (gone) 0ms [offline]"),
            "{line}"
        );
    }
}
