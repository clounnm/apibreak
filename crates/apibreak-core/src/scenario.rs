//! Failure scenarios: the model, matching them against requests, and keeping
//! them in sync with `scenarios.yaml`.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};

use crate::error::{Error, Result};

/// Lets editors finish their write-and-rename before the file is read.
const RELOAD_DEBOUNCE: Duration = Duration::from_millis(100);

/// Delay applied by a new delay scenario unless configured otherwise.
pub const DEFAULT_DELAY: Duration = Duration::from_secs(3);

/// A method plus a path pattern where `:name` matches one segment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Endpoint {
    pub method: String,
    pub path: String,
}

impl Endpoint {
    pub fn new(method: &str, path: &str) -> Self {
        let trimmed = path.trim_end_matches('/');
        let path = if trimmed.is_empty() { "/" } else { trimmed };
        Self {
            method: method.to_ascii_uppercase(),
            path: path.to_owned(),
        }
    }

    pub fn matches(&self, method: &str, path: &str) -> bool {
        if !self.method.eq_ignore_ascii_case(method) {
            return false;
        }
        let mut pattern = segments(&self.path);
        let mut actual = segments(path);
        loop {
            match (pattern.next(), actual.next()) {
                (None, None) => return true,
                (Some(expected), Some(segment)) => {
                    if !expected.starts_with(':') && expected != segment {
                        return false;
                    }
                }
                _ => return false,
            }
        }
    }

    /// Literal segments in the pattern: the more, the more specific.
    fn specificity(&self) -> usize {
        segments(&self.path)
            .filter(|segment| !segment.starts_with(':'))
            .count()
    }
}

fn segments(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|segment| !segment.is_empty())
}

impl fmt::Display for Endpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} {}", self.method, self.path)
    }
}

impl FromStr for Endpoint {
    type Err = Error;

    fn from_str(text: &str) -> Result<Self> {
        let invalid = || Error::InvalidScenario(format!("bad match `{text}`, want `GET /path`"));
        let (method, path) = text
            .trim()
            .split_once(char::is_whitespace)
            .ok_or_else(invalid)?;
        let path = path.trim();
        if !path.starts_with('/') || method.is_empty() {
            return Err(invalid());
        }
        Ok(Self::new(method, path))
    }
}

/// What a scenario does to a matching request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Answer with this status and a small JSON error.
    Status(u16),
    /// Answer `[]` if the endpoint returns arrays, otherwise 204.
    Empty,
    /// Wait, then continue as if this scenario were absent.
    Delay(Duration),
    /// Close the connection without answering.
    Offline,
    /// Answer from this mock file, relative to the configuration directory.
    Mock(String),
}

impl Action {
    pub fn label(&self) -> String {
        match self {
            Self::Status(status) => status.to_string(),
            Self::Empty => "empty".to_owned(),
            Self::Delay(delay) => format!("delay {}", format_duration(*delay)),
            Self::Offline => "offline".to_owned(),
            Self::Mock(_) => "mock".to_owned(),
        }
    }
}

/// An endpoint with the action applied to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawScenario", into = "RawScenario")]
pub struct Scenario {
    pub endpoint: Endpoint,
    pub action: Action,
}

/// The YAML shape of a scenario, kept flat as in the documented example.
#[derive(Debug, Serialize, Deserialize)]
struct RawScenario {
    #[serde(rename = "match")]
    matches: String,
    action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delay: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file: Option<String>,
}

impl TryFrom<RawScenario> for Scenario {
    type Error = Error;

    fn try_from(raw: RawScenario) -> Result<Self> {
        let endpoint = raw.matches.parse()?;
        let missing =
            |field: &str| Error::InvalidScenario(format!("`{}` needs `{field}`", raw.matches));
        let action = match raw.action.as_str() {
            "status" => {
                let status = raw.status.ok_or_else(|| missing("status"))?;
                if !(100..=599).contains(&status) {
                    return Err(Error::InvalidScenario(format!(
                        "`{}` has invalid status {status}",
                        raw.matches
                    )));
                }
                Action::Status(status)
            }
            "empty" => Action::Empty,
            "delay" => {
                let delay = raw.delay.as_deref().ok_or_else(|| missing("delay"))?;
                Action::Delay(parse_duration(delay)?)
            }
            "offline" => Action::Offline,
            "mock" => Action::Mock(raw.file.clone().ok_or_else(|| missing("file"))?),
            other => {
                return Err(Error::InvalidScenario(format!(
                    "unknown action `{other}` for `{}`",
                    raw.matches
                )))
            }
        };
        Ok(Self { endpoint, action })
    }
}

impl From<Scenario> for RawScenario {
    fn from(scenario: Scenario) -> Self {
        let mut raw = Self {
            matches: scenario.endpoint.to_string(),
            action: String::new(),
            status: None,
            delay: None,
            file: None,
        };
        raw.action = match scenario.action {
            Action::Status(status) => {
                raw.status = Some(status);
                "status"
            }
            Action::Empty => "empty",
            Action::Delay(delay) => {
                raw.delay = Some(format_duration(delay));
                "delay"
            }
            Action::Offline => "offline",
            Action::Mock(file) => {
                raw.file = Some(file);
                "mock"
            }
        }
        .to_owned();
        raw
    }
}

/// Parses `500ms`, `3s`, `1.5s` or `2m`.
pub fn parse_duration(text: &str) -> Result<Duration> {
    let invalid = || Error::InvalidScenario(format!("bad duration `{text}`, want 500ms, 3s or 2m"));
    let text = text.trim();
    let (number, scale) = if let Some(number) = text.strip_suffix("ms") {
        (number, 0.001)
    } else if let Some(number) = text.strip_suffix('s') {
        (number, 1.0)
    } else if let Some(number) = text.strip_suffix('m') {
        (number, 60.0)
    } else {
        return Err(invalid());
    };
    let value: f64 = number.trim().parse().map_err(|_| invalid())?;
    Duration::try_from_secs_f64(value * scale).map_err(|_| invalid())
}

pub fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis.is_multiple_of(1000) {
        format!("{}s", millis / 1000)
    } else {
        format!("{millis}ms")
    }
}

/// What to do with one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub delay: Option<Duration>,
    /// The scenario deciding the response; `None` forwards to the backend.
    pub response: Option<Scenario>,
    /// Text for the request list.
    pub label: String,
}

/// All active scenarios.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScenarioSet {
    scenarios: Vec<Scenario>,
}

impl ScenarioSet {
    pub fn new(scenarios: Vec<Scenario>) -> Self {
        Self { scenarios }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Scenario> {
        self.scenarios.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.scenarios.is_empty()
    }

    /// Picks the most specific match for the response and for the delay
    /// separately, because a delay combines with any other scenario.
    pub fn plan(&self, method: &str, path: &str) -> Option<Plan> {
        let best = |want_delay: bool| {
            self.scenarios
                .iter()
                .filter(|s| matches!(s.action, Action::Delay(_)) == want_delay)
                .filter(|s| s.endpoint.matches(method, path))
                .max_by_key(|s| s.endpoint.specificity())
        };
        let delay = best(true);
        let response = best(false);
        let label = [delay, response]
            .into_iter()
            .flatten()
            .map(|s| s.action.label())
            .collect::<Vec<_>>()
            .join(" + ");
        if label.is_empty() {
            return None;
        }
        Some(Plan {
            delay: delay.and_then(|s| match s.action {
                Action::Delay(delay) => Some(delay),
                _ => None,
            }),
            response: response.cloned(),
            label,
        })
    }

    /// Switches a scenario on, or off if it is already on. A new scenario
    /// replaces the endpoint's previous one, except that delays are tracked
    /// on their own and combine with the rest.
    pub fn toggle(&mut self, scenario: Scenario) {
        let is_delay = |action: &Action| matches!(action, Action::Delay(_));
        let was_on = self.scenarios.contains(&scenario);
        self.scenarios.retain(|existing| {
            existing.endpoint != scenario.endpoint
                || is_delay(&existing.action) != is_delay(&scenario.action)
        });
        if !was_on {
            self.scenarios.push(scenario);
        }
    }

    /// Removes every scenario on `endpoint`.
    pub fn clear(&mut self, endpoint: &Endpoint) {
        self.scenarios
            .retain(|existing| existing.endpoint != *endpoint);
    }

    /// The endpoint key presses on this request should act on: the pattern
    /// already covering it, or else one with ids turned into `:id`.
    pub fn endpoint_for(&self, method: &str, path: &str) -> Endpoint {
        self.scenarios
            .iter()
            .filter(|s| s.endpoint.matches(method, path))
            .max_by_key(|s| s.endpoint.specificity())
            .map_or_else(
                || Endpoint::new(method, &template_path(path)),
                |s| s.endpoint.clone(),
            )
    }

    /// One text per active endpoint, as shown in the status line.
    pub fn summary(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let endpoints = self.scenarios.iter().map(|s| &s.endpoint);
        endpoints
            .filter(|endpoint| seen.insert(*endpoint))
            .map(|endpoint| {
                let labels: Vec<String> = self
                    .scenarios
                    .iter()
                    .filter(|s| s.endpoint == *endpoint)
                    .map(|s| s.action.label())
                    .collect();
                format!("{endpoint} → {}", labels.join(" + "))
            })
            .collect()
    }
}

/// Replaces segments that look like ids with `:id`, so that breaking
/// `/api/users/7` breaks every user rather than just that one.
pub fn template_path(path: &str) -> String {
    let templated: Vec<&str> = segments(path)
        .map(|segment| if is_id(segment) { ":id" } else { segment })
        .collect();
    format!("/{}", templated.join("/"))
}

fn is_id(segment: &str) -> bool {
    let digits = segment.bytes().all(|byte| byte.is_ascii_digit());
    let hex = segment.bytes().all(|byte| byte.is_ascii_hexdigit());
    let uuid = segment.len() == 36
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-');
    digits || (hex && segment.len() >= 16) || uuid
}

/// Live scenarios shared by the proxy and its controls. Built on a `watch`
/// channel: readers never block, and followers see every change.
#[derive(Clone)]
pub struct ScenarioHandle {
    sender: Arc<watch::Sender<Arc<ScenarioSet>>>,
}

impl ScenarioHandle {
    pub fn new(set: ScenarioSet) -> Self {
        Self {
            sender: Arc::new(watch::Sender::new(Arc::new(set))),
        }
    }

    pub fn current(&self) -> Arc<ScenarioSet> {
        self.sender.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Arc<ScenarioSet>> {
        self.sender.subscribe()
    }

    /// Applies `change`, notifying followers if it changed anything.
    pub fn update(&self, change: impl FnOnce(&mut ScenarioSet)) {
        self.sender.send_if_modified(|current| {
            let mut next = ScenarioSet::clone(current);
            change(&mut next);
            let modified = next != **current;
            if modified {
                *current = Arc::new(next);
            }
            modified
        });
    }
}

/// The contents of `scenarios.yaml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Delay for the `d` key, such as `3s`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_delay: Option<String>,
    #[serde(default)]
    pub scenarios: ScenarioSet,
}

impl Config {
    /// Reads the file. `None` means it does not exist.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(Error::Io {
                    action: "reading",
                    path: path.to_owned(),
                    source,
                })
            }
        };
        if !text.lines().any(has_content) {
            return Ok(Some(Self::default()));
        }
        serde_saphyr::from_str(&text)
            .map(Some)
            .map_err(|error| Error::InvalidConfig {
                path: path.to_owned(),
                message: error.to_string(),
            })
    }

    /// Replaces the file in one step, so a reader never sees half of it.
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = serde_saphyr::to_string(self).map_err(|error| Error::InvalidConfig {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
        let temporary = path.with_extension("yaml.tmp");
        let io_error = |source| Error::Io {
            action: "writing",
            path: path.to_owned(),
            source,
        };
        std::fs::write(&temporary, text).map_err(io_error)?;
        std::fs::rename(&temporary, path).map_err(io_error)
    }
}

fn has_content(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty() && !line.starts_with('#')
}

/// Keeps `handle` and the file at `path` in step until the runtime stops:
/// edits to the file reach the proxy, and changes made through `handle` are
/// saved. `config` is what the file holds now; its fields other than the
/// scenarios are preserved when saving. Call from within a tokio runtime.
///
/// The returned channel carries the current problem with the file, if any.
pub fn sync_with_file(
    handle: ScenarioHandle,
    path: PathBuf,
    config: Config,
) -> Result<watch::Receiver<Option<String>>> {
    let (changes, file_events) = mpsc::unbounded_channel();
    let name = path.file_name().map(ToOwned::to_owned);
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let relevant = event.is_ok_and(|event| {
            event
                .paths
                .iter()
                .any(|changed| changed.file_name() == name.as_deref())
        });
        if relevant {
            // Fails only once the sync task is gone.
            let _ = changes.send(());
        }
    })?;
    // The directory, not the file: editors often replace the file by renaming.
    let directory = path.parent().unwrap_or(Path::new("."));
    watcher.watch(directory, RecursiveMode::NonRecursive)?;

    let (status, status_receiver) = watch::channel(None);
    let sync = FileSync {
        handle,
        path,
        config,
        status,
        file_broken: false,
    };
    tokio::spawn(sync.run(file_events, watcher));
    Ok(status_receiver)
}

struct FileSync {
    handle: ScenarioHandle,
    path: PathBuf,
    /// What is believed to be on disk.
    config: Config,
    status: watch::Sender<Option<String>>,
    file_broken: bool,
}

impl FileSync {
    /// `_watcher` is held here because dropping it stops the events.
    async fn run(
        mut self,
        mut file_events: mpsc::UnboundedReceiver<()>,
        _watcher: notify::RecommendedWatcher,
    ) {
        let mut scenarios = self.handle.subscribe();
        loop {
            tokio::select! {
                event = file_events.recv() => {
                    if event.is_none() {
                        return;
                    }
                    tokio::time::sleep(RELOAD_DEBOUNCE).await;
                    while file_events.try_recv().is_ok() {}
                    self.reload();
                }
                changed = scenarios.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let current = scenarios.borrow_and_update().clone();
                    self.save(&current);
                }
            }
        }
    }

    fn reload(&mut self) {
        match Config::load(&self.path) {
            Ok(Some(config)) => {
                self.file_broken = false;
                self.report(None);
                // Events also fire for our own saves. Applying an unchanged
                // file could undo a change the handle has just made.
                if config == self.config {
                    return;
                }
                let scenarios = config.scenarios.clone();
                self.config = config;
                self.handle.update(|set| *set = scenarios);
            }
            // Deleting the file is not a request to drop what is active. The
            // next change made through the handle writes it back.
            Ok(None) => {}
            Err(error) => {
                self.file_broken = true;
                self.report(Some(error.to_string()));
            }
        }
    }

    fn save(&mut self, current: &ScenarioSet) {
        if *current == self.config.scenarios {
            return;
        }
        self.config.scenarios = current.clone();
        // Saving now would overwrite whatever the user is halfway through.
        if self.file_broken {
            return;
        }
        let problem = self
            .config
            .save(&self.path)
            .err()
            .map(|error| error.to_string());
        self.report(problem);
    }

    /// Passes on a change of status. A file saved repeatedly in a broken
    /// state stays one problem rather than a stream of identical ones.
    fn report(&self, problem: Option<String>) {
        self.status.send_if_modified(|status| {
            let changed = *status != problem;
            *status = problem;
            changed
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(text: &str) -> Endpoint {
        text.parse().expect("endpoint")
    }

    fn scenario(text: &str, action: Action) -> Scenario {
        Scenario {
            endpoint: endpoint(text),
            action,
        }
    }

    #[test]
    fn matches_literal_and_template_paths() {
        let pattern = endpoint("GET /api/users/:id");
        assert!(pattern.matches("GET", "/api/users/42"));
        assert!(pattern.matches("get", "/api/users/42/"));
        assert!(!pattern.matches("POST", "/api/users/42"));
        assert!(!pattern.matches("GET", "/api/users"));
        assert!(!pattern.matches("GET", "/api/users/42/posts"));
        assert!(endpoint("GET /").matches("GET", "/"));
        assert!(!endpoint("GET /a").matches("GET", "/b"));
    }

    #[test]
    fn parses_endpoints() {
        assert_eq!(endpoint("post  /a/").to_string(), "POST /a");
        for bad in ["GET", "GET api", "/api", ""] {
            assert!(bad.parse::<Endpoint>().is_err(), "{bad}");
        }
    }

    #[test]
    fn most_specific_scenario_wins() {
        let set = ScenarioSet::new(vec![
            scenario("GET /api/users/:id", Action::Status(500)),
            scenario("GET /api/users/me", Action::Status(401)),
        ]);
        let plan = set.plan("GET", "/api/users/me").expect("plan");
        assert_eq!(plan.response.expect("response").action, Action::Status(401));
        let plan = set.plan("GET", "/api/users/1").expect("plan");
        assert_eq!(plan.response.expect("response").action, Action::Status(500));
    }

    #[test]
    fn delay_combines_with_a_response_scenario() {
        let mut set = ScenarioSet::default();
        set.toggle(scenario("GET /a", Action::Status(500)));
        set.toggle(scenario("GET /a", Action::Delay(Duration::from_secs(2))));

        let plan = set.plan("GET", "/a").expect("plan");
        assert_eq!(plan.delay, Some(Duration::from_secs(2)));
        assert_eq!(plan.label, "delay 2s + 500");
        assert!(set.plan("GET", "/b").is_none());
    }

    #[test]
    fn toggle_replaces_then_switches_off() {
        let mut set = ScenarioSet::default();
        set.toggle(scenario("GET /a", Action::Status(500)));
        set.toggle(scenario("GET /a", Action::Offline));
        assert_eq!(set.iter().count(), 1);
        assert_eq!(set.iter().next().expect("one").action, Action::Offline);
        set.toggle(scenario("GET /a", Action::Offline));
        assert!(set.is_empty());
    }

    #[test]
    fn clear_only_touches_one_endpoint() {
        let mut set = ScenarioSet::default();
        set.toggle(scenario("GET /a", Action::Status(500)));
        set.toggle(scenario("GET /a", Action::Delay(Duration::from_secs(1))));
        set.toggle(scenario("GET /b", Action::Status(500)));
        set.clear(&endpoint("GET /a"));
        assert_eq!(set.summary(), ["GET /b → 500"]);
    }

    #[test]
    fn summary_reads_like_the_status_line() {
        let set = ScenarioSet::new(vec![
            scenario("GET /api/users", Action::Status(500)),
            scenario("POST /api/login", Action::Delay(Duration::from_secs(3))),
        ]);
        assert_eq!(
            set.summary(),
            ["GET /api/users → 500", "POST /api/login → delay 3s"]
        );
    }

    #[test]
    fn templates_id_like_segments() {
        assert_eq!(template_path("/api/users/42"), "/api/users/:id");
        assert_eq!(
            template_path("/a/550e8400-e29b-41d4-a716-446655440000/b"),
            "/a/:id/b"
        );
        assert_eq!(template_path("/a/0123456789abcdef"), "/a/:id");
        assert_eq!(template_path("/api/v2/feed"), "/api/v2/feed");
        assert_eq!(template_path("/"), "/");
    }

    #[test]
    fn endpoint_for_prefers_existing_pattern() {
        let set = ScenarioSet::new(vec![scenario("GET /files/:name", Action::Offline)]);
        assert_eq!(
            set.endpoint_for("GET", "/files/a.txt"),
            endpoint("GET /files/:name")
        );
        assert_eq!(
            set.endpoint_for("GET", "/users/7"),
            endpoint("GET /users/:id")
        );
    }

    #[test]
    fn durations_round_trip() {
        for (text, duration) in [
            ("500ms", Duration::from_millis(500)),
            ("3s", Duration::from_secs(3)),
            ("1.5s", Duration::from_millis(1500)),
            ("2m", Duration::from_secs(120)),
        ] {
            assert_eq!(parse_duration(text).expect(text), duration);
        }
        assert_eq!(format_duration(Duration::from_millis(1500)), "1500ms");
        for bad in ["3", "s", "-1s", "abc"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn handle_notifies_only_on_real_changes() {
        let handle = ScenarioHandle::new(ScenarioSet::default());
        let mut receiver = handle.subscribe();
        handle.update(|_| {});
        assert!(!receiver.has_changed().expect("open"));
        handle.update(|set| set.toggle(scenario("GET /a", Action::Offline)));
        assert!(receiver.has_changed().expect("open"));
        receiver.mark_unchanged();
        assert_eq!(handle.current().iter().count(), 1);
    }

    const EXAMPLE: &str = r#"
target: https://api.example.com
port: 4000
scenarios:
  - match: "GET /api/users"
    action: status
    status: 500
  - match: "POST /api/login"
    action: delay
    delay: 3s
  - match: "GET /api/users/:id"
    action: mock
    file: mocks/GET_api_users_id.json
  - match: "GET /api/feed"
    action: empty
  - match: "GET /api/ping"
    action: offline
"#;

    fn write_example(directory: &Path) -> PathBuf {
        let path = directory.join("scenarios.yaml");
        std::fs::write(&path, EXAMPLE).expect("write");
        path
    }

    #[test]
    fn loads_the_documented_example() {
        let directory = tempfile::tempdir().expect("tempdir");
        let config = Config::load(&write_example(directory.path()))
            .expect("load")
            .expect("exists");
        assert_eq!(config.target.as_deref(), Some("https://api.example.com"));
        assert_eq!(config.port, Some(4000));
        assert_eq!(
            config.scenarios.summary(),
            [
                "GET /api/users → 500",
                "POST /api/login → delay 3s",
                "GET /api/users/:id → mock",
                "GET /api/feed → empty",
                "GET /api/ping → offline",
            ]
        );
    }

    #[test]
    fn config_round_trips_through_the_file() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = write_example(directory.path());
        let config = Config::load(&path).expect("load").expect("exists");

        config.save(&path).expect("save");

        assert_eq!(Config::load(&path).expect("load"), Some(config));
    }

    #[test]
    fn missing_and_comment_only_files_are_fine() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("scenarios.yaml");
        assert_eq!(Config::load(&path).expect("load"), None);
        std::fs::write(&path, "# nothing yet\n\n").expect("write");
        assert_eq!(Config::load(&path).expect("load"), Some(Config::default()));
    }

    #[test]
    fn bad_files_name_the_problem() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("scenarios.yaml");
        for (text, needle) in [
            (
                "scenarios:\n  - match: \"GET /a\"\n    action: explode\n",
                "explode",
            ),
            (
                "scenarios:\n  - match: \"GET /a\"\n    action: status\n",
                "status",
            ),
            (
                "scenarios:\n  - match: \"GET /a\"\n    action: status\n    status: 9\n",
                "9",
            ),
            (
                "scenarios:\n  - match: \"nope\"\n    action: empty\n",
                "nope",
            ),
        ] {
            std::fs::write(&path, text).expect("write");
            let message = Config::load(&path).expect_err(text).to_string();
            assert!(message.contains(needle), "{message}");
        }
    }

    async fn wait_until(what: &str, condition: impl Fn() -> bool) {
        for _ in 0..300 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}");
    }

    #[tokio::test]
    async fn hot_reload_and_saving_both_ways() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = write_example(directory.path());
        let config = Config::load(&path).expect("load").expect("exists");
        let handle = ScenarioHandle::new(config.scenarios.clone());
        let status = sync_with_file(handle.clone(), path.clone(), config).expect("sync");

        // Hand edit reaches the proxy.
        std::fs::write(
            &path,
            "target: https://kept.example\nscenarios:\n  - match: \"GET /new\"\n    action: offline\n",
        )
        .expect("write");
        wait_until("reload", || {
            handle.current().summary() == ["GET /new → offline"]
        })
        .await;

        // A broken edit keeps the last good scenarios and reports the error.
        std::fs::write(&path, "scenarios: [").expect("write");
        wait_until("error", || status.borrow().is_some()).await;
        assert_eq!(handle.current().iter().count(), 1);

        // While broken, changes must not clobber the file being edited.
        handle.update(|set| set.toggle(scenario("GET /ui", Action::Status(500))));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "scenarios: ["
        );

        // Fixing the file wins over the unsaved change.
        std::fs::write(
            &path,
            "target: https://kept.example\nscenarios:\n  - match: \"GET /fixed\"\n    action: empty\n",
        )
        .expect("write");
        wait_until("recovery", || status.borrow().is_none()).await;
        wait_until("fixed", || {
            handle.current().summary() == ["GET /fixed → empty"]
        })
        .await;

        // A change made through the handle lands in the file, target intact.
        handle.update(|set| set.toggle(scenario("POST /b", Action::Status(401))));
        let on_disk = || Config::load(&path).ok().flatten().unwrap_or_default();
        wait_until("save", || on_disk().scenarios.iter().count() == 2).await;
        assert_eq!(on_disk().target.as_deref(), Some("https://kept.example"));
    }
}
