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

use std::io::stdout;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use apibreak_core::{MockStore, ScenarioHandle, ScenarioSet, Store};
use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{enable_raw_mode, EnterAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::{DefaultTerminal, Terminal};
use tokio::sync::broadcast::error::TryRecvError;
use tokio::sync::watch;

use app::App;

const TICK: Duration = Duration::from_millis(40);

/// Runs the UI until the user quits. `title` is shown in the header.
pub fn run(
    store: Arc<Store>,
    scenarios: ScenarioHandle,
    mocks: MockStore,
    default_delay: Duration,
    title: String,
) -> Result<()> {
    // Installs a panic hook that leaves raw mode and the alternate screen.
    let mut terminal = ratatui::try_init().context("cannot set up the terminal")?;
    let mut watcher = scenarios.subscribe();
    let app = App::new(store.clone(), scenarios, mocks, default_delay, title);
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
        if let Some(file) = app.pending_edit.take() {
            let result = match app.mock_path(&file) {
                Some(path) => edit(terminal, &path)?,
                None => Err(format!("bad mock path {file}")),
            };
            app.after_edit(&file, result);
            dirty = true;
        }
    }
    Ok(())
}

/// Lends the terminal to the editor. The inner result is the editor's own
/// failure, which belongs in the footer; the outer one means the terminal
/// could not be taken back.
fn edit(terminal: &mut DefaultTerminal, path: &Path) -> Result<std::result::Result<(), String>> {
    ratatui::try_restore().context("cannot leave the terminal to the editor")?;
    let result = run_editor(path);
    enable_raw_mode().context("cannot re-enter raw mode")?;
    execute!(stdout(), EnterAlternateScreen).context("cannot re-enter the screen")?;
    // A fresh terminal repaints everything. `Terminal::clear` would instead
    // ask for the cursor position, which fails where nothing answers.
    *terminal =
        Terminal::new(CrosstermBackend::new(stdout())).context("cannot redraw the terminal")?;
    Ok(result)
}

fn run_editor(path: &Path) -> std::result::Result<(), String> {
    let fallback = if cfg!(windows) { "notepad" } else { "vi" };
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| fallback.to_owned());
    // $EDITOR may carry arguments, as in `code --wait`.
    let mut words = editor.split_whitespace();
    let program = words.next().unwrap_or(fallback);
    match Command::new(program).args(words).arg(path).status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("{program} exited with {status}")),
        Err(error) => Err(format!("cannot run {program}: {error}")),
    }
}
