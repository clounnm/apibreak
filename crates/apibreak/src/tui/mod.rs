//! Terminal interface: a thin layer over `apibreak-core`.
//!
//! It runs on its own thread with a synchronous poll loop. That lets it hand
//! the terminal to `$EDITOR` by simply blocking, while the proxy keeps serving
//! on the tokio runtime.

mod app;
mod highlight;
mod keymap;
mod style;
mod view;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use apibreak_core::{ScenarioHandle, ScenarioSet, Store};
use crossterm::event::{self, Event};
use ratatui::DefaultTerminal;
use tokio::sync::broadcast::error::TryRecvError;
use tokio::sync::watch;

use app::App;

const TICK: Duration = Duration::from_millis(40);

/// Runs the UI until the user quits. `title` is shown in the header.
pub fn run(
    store: Arc<Store>,
    scenarios: ScenarioHandle,
    default_delay: Duration,
    title: String,
) -> Result<()> {
    // Installs a panic hook that leaves raw mode and the alternate screen.
    let mut terminal = ratatui::try_init().context("cannot set up the terminal")?;
    let mut watcher = scenarios.subscribe();
    let app = App::new(store.clone(), scenarios, default_delay, title);
    let result = event_loop(&mut terminal, app, &store, &mut watcher);
    let restored = ratatui::try_restore().context("cannot restore the terminal");
    result.and(restored)
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    mut app: App,
    store: &Store,
    scenarios: &mut watch::Receiver<Arc<ScenarioSet>>,
) -> Result<()> {
    let mut events = store.subscribe();
    app.resync(store.snapshot());
    let mut dirty = true;
    while !app.should_quit {
        loop {
            match events.try_recv() {
                Ok(event) => app.apply_event(event),
                Err(TryRecvError::Lagged(_)) => app.resync(store.snapshot()),
                Err(TryRecvError::Empty | TryRecvError::Closed) => break,
            }
            dirty = true;
        }
        if scenarios.has_changed().unwrap_or(false) {
            app.active = scenarios.borrow_and_update().clone();
            dirty = true;
        }
        if dirty {
            terminal.draw(|frame| view::draw(frame, &mut app))?;
            dirty = false;
        }
        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key) => {
                    if let Some(action) = keymap::action(app.mode, key) {
                        app.handle(action);
                        dirty = true;
                    }
                }
                Event::Resize(..) => dirty = true,
                _ => {}
            }
        }
    }
    Ok(())
}
