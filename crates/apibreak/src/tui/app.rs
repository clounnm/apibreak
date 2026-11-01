//! UI state. It holds its own copy of the exchanges, fed from the store's
//! event channel, so the render loop never touches shared locks.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use apibreak_core::{
    Action as Effect, Endpoint, Exchange, Mock, MockStore, Scenario, ScenarioHandle, ScenarioSet,
    Store, StoreEvent,
};
use ratatui::text::Line;

use super::keymap::{Action, Break};
use super::view;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Details,
}

/// Feedback shown in the footer until the next key press.
pub struct Message {
    pub text: String,
    pub is_error: bool,
}

/// The rendered details of one exchange; rebuilt only when it changes.
struct DetailCache {
    exchange: Arc<Exchange>,
    lines: Arc<Vec<Line<'static>>>,
}

pub struct App {
    store: Arc<Store>,
    scenarios: ScenarioHandle,
    mocks: MockStore,
    default_delay: Duration,
    /// Active scenarios as last seen on the watch channel.
    pub active: Arc<ScenarioSet>,
    pub title: String,
    entries: VecDeque<Arc<Exchange>>,
    selected_id: Option<u64>,
    /// Keeps the selection on the newest request as new ones arrive.
    pub follow: bool,
    pub focus: Focus,
    pub mode: Mode,
    pub filter: String,
    pub paused: bool,
    pub detail_scroll: u16,
    /// Rows of the details panel, set while drawing, for clamping the scroll.
    pub detail_max_scroll: u16,
    pub should_quit: bool,
    pub message: Option<Message>,
    /// A mock waiting to be opened in the editor by the event loop.
    pub pending_edit: Option<String>,
    cache: Option<DetailCache>,
}

impl App {
    pub fn new(
        store: Arc<Store>,
        scenarios: ScenarioHandle,
        mocks: MockStore,
        default_delay: Duration,
        title: String,
    ) -> Self {
        Self {
            store,
            active: scenarios.current(),
            scenarios,
            mocks,
            default_delay,
            title,
            entries: VecDeque::new(),
            selected_id: None,
            follow: true,
            focus: Focus::List,
            mode: Mode::Normal,
            filter: String::new(),
            paused: false,
            detail_scroll: 0,
            detail_max_scroll: 0,
            should_quit: false,
            message: None,
            pending_edit: None,
            cache: None,
        }
    }

    pub fn apply_event(&mut self, event: StoreEvent) {
        match event {
            StoreEvent::Cleared => self.reset_entries(),
            StoreEvent::Upsert(exchange) => self.upsert(exchange),
        }
    }

    /// Recovers after the event channel dropped messages.
    pub fn resync(&mut self, entries: Vec<Arc<Exchange>>) {
        if !self.paused {
            self.entries = entries.into();
        }
    }

    fn reset_entries(&mut self) {
        self.entries.clear();
        self.selected_id = None;
        self.follow = true;
        self.detail_scroll = 0;
    }

    fn upsert(&mut self, exchange: Arc<Exchange>) {
        if let Some(slot) = self.entries.iter_mut().rev().find(|e| e.id == exchange.id) {
            *slot = exchange;
        } else if !self.paused {
            if self.entries.len() == self.store.capacity() {
                self.entries.pop_front();
            }
            self.entries.push_back(exchange);
        }
    }

    /// Exchanges passing the filter, oldest first.
    pub fn visible(&self) -> Vec<&Arc<Exchange>> {
        let needle = self.filter.to_lowercase();
        self.entries
            .iter()
            .filter(|entry| needle.is_empty() || entry.uri.to_lowercase().contains(&needle))
            .collect()
    }

    pub fn selected_index(&self, visible: &[&Arc<Exchange>]) -> Option<usize> {
        let last = visible.len().checked_sub(1)?;
        if self.follow {
            return Some(last);
        }
        let selected = self.selected_id?;
        // A selection that was evicted or filtered out lands on the oldest row.
        Some(visible.iter().position(|e| e.id == selected).unwrap_or(0))
    }

    pub fn selected(&self) -> Option<Arc<Exchange>> {
        let visible = self.visible();
        let index = self.selected_index(&visible)?;
        Some(visible[index].clone())
    }

    /// Detail lines for the selection, reusing the last render when unchanged.
    pub fn detail_lines(&mut self) -> Arc<Vec<Line<'static>>> {
        let Some(exchange) = self.selected() else {
            self.cache = None;
            return Arc::default();
        };
        if let Some(cache) = &self.cache {
            if Arc::ptr_eq(&cache.exchange, &exchange) {
                return cache.lines.clone();
            }
        }
        let lines = Arc::new(view::detail_lines(&exchange));
        self.cache = Some(DetailCache {
            exchange,
            lines: lines.clone(),
        });
        lines
    }

    pub fn handle(&mut self, action: Action) {
        self.message = None;
        match action {
            Action::Quit => self.should_quit = true,
            Action::Down => self.move_by(1),
            Action::Up => self.move_by(-1),
            Action::Top => self.jump(false),
            Action::Bottom => self.jump(true),
            Action::SwitchPane => {
                self.focus = match self.focus {
                    Focus::List => Focus::Details,
                    Focus::Details => Focus::List,
                };
            }
            Action::StartFilter => self.mode = Mode::Filter,
            Action::FilterChar(character) => {
                self.filter.push(character);
                self.detail_scroll = 0;
            }
            Action::FilterBackspace => {
                self.filter.pop();
            }
            Action::FilterApply => self.mode = Mode::Normal,
            Action::FilterCancel => {
                self.filter.clear();
                self.mode = Mode::Normal;
            }
            Action::Clear => {
                self.store.clear();
                self.reset_entries();
            }
            Action::TogglePause => self.paused = !self.paused,
            Action::ToggleHelp => self.mode = Mode::Help,
            Action::CloseHelp => self.mode = Mode::Normal,
            Action::Break(kind) => self.toggle_scenario(kind),
            Action::ClearScenarios => {
                if let Some((_, endpoint)) = self.selection() {
                    self.scenarios.update(|set| set.clear(&endpoint));
                }
            }
            Action::Record => self.record(),
            Action::EditMock => self.edit_mock(),
            Action::ToggleMock => self.toggle_mock(),
        }
    }

    #[cfg(test)]
    pub fn scenarios_for_test(&self) -> Arc<ScenarioSet> {
        self.scenarios.current()
    }

    /// The selected exchange and the endpoint keys act on.
    fn selection(&self) -> Option<(Arc<Exchange>, Endpoint)> {
        let exchange = self.selected()?;
        let current = self.scenarios.current();
        let endpoint = current.endpoint_for(&exchange.method, &exchange.path);
        Some((exchange, endpoint))
    }

    fn inform(&mut self, text: String) {
        self.message = Some(Message {
            text,
            is_error: false,
        });
    }

    fn fail(&mut self, text: String) {
        self.message = Some(Message {
            text,
            is_error: true,
        });
    }

    fn record(&mut self) {
        let Some((exchange, endpoint)) = self.selection() else {
            return;
        };
        let saved =
            Mock::from_exchange(&exchange).and_then(|mock| self.mocks.save(&endpoint, &mock));
        match saved {
            Ok(file) => self.inform(format!("saved {file}, press m to serve it")),
            Err(error) => self.fail(error.to_string()),
        }
    }

    fn edit_mock(&mut self) {
        let Some((_, endpoint)) = self.selection() else {
            return;
        };
        let file = MockStore::file_for(&endpoint);
        if self.mocks.exists(&file) {
            self.pending_edit = Some(file);
        } else {
            self.fail(format!("no mock for {endpoint}, press r to record one"));
        }
    }

    fn toggle_mock(&mut self) {
        let Some((_, endpoint)) = self.selection() else {
            return;
        };
        let file = MockStore::file_for(&endpoint);
        if !self.mocks.exists(&file) {
            self.fail(format!("no mock for {endpoint}, press r to record one"));
            return;
        }
        let action = Effect::Mock(file);
        self.scenarios
            .update(|set| set.toggle(Scenario { endpoint, action }));
    }

    /// The path to hand to the editor for a mock file.
    pub fn mock_path(&self, file: &str) -> Option<PathBuf> {
        self.mocks.resolve(file).ok()
    }

    /// Reports on a mock after the editor closes. Changes need no reloading
    /// because the proxy reads mock files on every request.
    pub fn after_edit(&mut self, file: &str, result: Result<(), String>) {
        match result.and_then(|()| self.mocks.validate(file).map_err(|error| error.to_string())) {
            Ok(()) => self.inform(format!("{file} updated")),
            Err(error) => self.fail(error),
        }
    }

    fn toggle_scenario(&mut self, kind: Break) {
        let Some((_, endpoint)) = self.selection() else {
            return;
        };
        let action = match kind {
            Break::Status500 => Effect::Status(500),
            Break::Status401 => Effect::Status(401),
            Break::Status404 => Effect::Status(404),
            Break::Empty => Effect::Empty,
            Break::Delay => Effect::Delay(self.default_delay),
            Break::Offline => Effect::Offline,
        };
        self.scenarios
            .update(|set| set.toggle(Scenario { endpoint, action }));
    }

    fn move_by(&mut self, delta: i32) {
        if self.focus == Focus::Details {
            self.detail_scroll = self
                .detail_scroll
                .saturating_add_signed(delta as i16)
                .min(self.detail_max_scroll);
            return;
        }
        let visible = self.visible();
        let Some(current) = self.selected_index(&visible) else {
            return;
        };
        let target = current
            .saturating_add_signed(delta as isize)
            .min(visible.len() - 1);
        let follow = target == visible.len() - 1;
        let id = visible[target].id;
        self.select(id, follow);
    }

    fn jump(&mut self, bottom: bool) {
        if self.focus == Focus::Details {
            self.detail_scroll = if bottom { self.detail_max_scroll } else { 0 };
            return;
        }
        let visible = self.visible();
        let target = if bottom {
            visible.last()
        } else {
            visible.first()
        };
        if let Some(id) = target.map(|entry| entry.id) {
            self.select(id, bottom);
        }
    }

    fn select(&mut self, id: u64, follow: bool) {
        self.selected_id = Some(id);
        self.follow = follow;
        self.detail_scroll = 0;
    }
}

#[cfg(test)]
mod tests {
    use apibreak_core::store::{Captured, NewExchange};

    use super::*;

    fn add(store: &Store, path: &str) {
        store.begin(NewExchange {
            method: "GET".into(),
            path: path.into(),
            uri: path.into(),
            request_headers: Vec::new(),
            request_body: Captured::default(),
            scenario: None,
        });
    }

    fn new_app(store: Arc<Store>) -> App {
        let scenarios = ScenarioHandle::new(ScenarioSet::default());
        let mocks = MockStore::new(std::env::temp_dir().join("apibreak-no-mocks-here"));
        App::new(
            store,
            scenarios,
            mocks,
            Duration::from_secs(3),
            String::new(),
        )
    }

    /// An app with `paths` already delivered through the event channel.
    fn app_with(paths: &[&str]) -> App {
        let store = Arc::new(Store::new(3));
        let mut events = store.subscribe();
        let mut app = new_app(store.clone());
        for path in paths {
            add(&store, path);
        }
        while let Ok(event) = events.try_recv() {
            app.apply_event(event);
        }
        app
    }

    fn selected_path(app: &App) -> String {
        app.selected().expect("selection").path.clone()
    }

    #[test]
    fn follows_newest_until_moved_and_resumes_at_bottom() {
        let mut app = app_with(&["/a", "/b", "/c"]);
        assert_eq!(selected_path(&app), "/c");

        app.handle(Action::Up);
        assert_eq!(selected_path(&app), "/b");
        assert!(!app.follow);

        app.handle(Action::Down);
        assert!(app.follow);
    }

    #[test]
    fn top_and_bottom_jump() {
        let mut app = app_with(&["/a", "/b", "/c"]);
        app.handle(Action::Top);
        assert_eq!(selected_path(&app), "/a");
        app.handle(Action::Bottom);
        assert_eq!(selected_path(&app), "/c");
    }

    #[test]
    fn filter_matches_case_insensitively() {
        let mut app = app_with(&["/api/Users", "/api/login"]);
        app.handle(Action::StartFilter);
        app.handle(Action::FilterChar('u'));
        app.handle(Action::FilterChar('s'));
        assert_eq!(app.visible().len(), 1);
        app.handle(Action::FilterCancel);
        assert_eq!(app.visible().len(), 2);
    }

    #[test]
    fn local_copy_is_a_ring_too() {
        let app = app_with(&["/a", "/b", "/c", "/d"]);
        let visible = app.visible();
        assert_eq!(visible.len(), 3);
        assert_eq!(visible[0].path, "/b");
    }

    #[test]
    fn pause_ignores_new_requests() {
        let store = Arc::new(Store::new(3));
        let mut events = store.subscribe();
        let mut app = new_app(store.clone());
        app.handle(Action::TogglePause);
        add(&store, "/a");
        app.apply_event(events.try_recv().expect("event"));
        assert!(app.visible().is_empty());
    }

    #[test]
    fn clear_empties_ui_and_store() {
        let mut app = app_with(&["/a"]);
        app.handle(Action::Clear);
        assert!(app.visible().is_empty());
        assert!(app.selected().is_none());
        assert!(app.store.snapshot().is_empty());
    }

    #[test]
    fn details_scroll_is_clamped_and_leaves_selection_alone() {
        let mut app = app_with(&["/a", "/b"]);
        app.handle(Action::SwitchPane);
        app.detail_max_scroll = 1;
        for _ in 0..3 {
            app.handle(Action::Down);
        }
        assert_eq!(app.detail_scroll, 1);
        assert_eq!(selected_path(&app), "/b");
    }

    #[test]
    fn keys_toggle_scenarios_on_the_templated_endpoint() {
        let mut app = app_with(&["/api/users/7"]);
        app.handle(Action::Break(Break::Status500));
        app.handle(Action::Break(Break::Delay));
        assert_eq!(
            app.scenarios.current().summary(),
            ["GET /api/users/:id → 500 + delay 3s"]
        );

        app.handle(Action::Break(Break::Status500));
        assert_eq!(
            app.scenarios.current().summary(),
            ["GET /api/users/:id → delay 3s"]
        );

        app.handle(Action::ClearScenarios);
        assert!(app.scenarios.current().is_empty());
    }

    #[test]
    fn scenario_keys_without_a_selection_do_nothing() {
        let mut app = app_with(&[]);
        app.handle(Action::Break(Break::Offline));
        app.handle(Action::ClearScenarios);
        assert!(app.scenarios.current().is_empty());
    }

    #[test]
    fn mock_keys_explain_when_nothing_was_recorded() {
        let mut app = app_with(&["/a"]);
        app.handle(Action::ToggleMock);
        assert!(app.message.as_ref().is_some_and(|message| message.is_error));
        assert!(app.scenarios.current().is_empty());

        app.handle(Action::EditMock);
        assert!(app.pending_edit.is_none());

        app.handle(Action::Down);
        assert!(app.message.is_none(), "any key clears the message");
    }

    #[test]
    fn navigating_an_empty_list_is_safe() {
        let mut app = app_with(&[]);
        for action in [Action::Down, Action::Up, Action::Top, Action::Bottom] {
            app.handle(action);
        }
        assert!(app.selected().is_none());
    }
}
