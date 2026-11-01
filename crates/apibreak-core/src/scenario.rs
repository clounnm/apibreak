//! Failure scenarios: the model, and matching them against requests.

use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::error::Error;

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

    fn from_str(text: &str) -> Result<Self, Error> {
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

    fn try_from(raw: RawScenario) -> Result<Self, Error> {
        let endpoint = raw.matches.parse()?;
        let missing =
            |field: &str| Error::InvalidScenario(format!("`{}` needs `{field}`", raw.matches));
        let action = match raw.action.as_str() {
            "status" => Action::Status(raw.status.ok_or_else(|| missing("status"))?),
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
pub fn parse_duration(text: &str) -> Result<Duration, Error> {
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
}
