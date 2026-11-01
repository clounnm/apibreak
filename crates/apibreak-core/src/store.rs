//! Ring buffer of recent exchanges, plus a broadcast of every change.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Local};
use tokio::sync::broadcast;

/// Default number of exchanges kept in memory.
pub const DEFAULT_CAPACITY: usize = 500;
/// Bodies are captured up to this size; the wire traffic is never limited.
pub const CAPTURE_LIMIT: usize = 2 * 1024 * 1024;
const EVENT_BACKLOG: usize = 4096;

pub type Headers = Vec<(String, String)>;

/// A body as captured from the wire, possibly cut at [`CAPTURE_LIMIT`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Captured {
    pub bytes: Bytes,
    /// True if the body on the wire was longer than `bytes`.
    pub truncated: bool,
}

impl Captured {
    pub fn new(bytes: Bytes) -> Self {
        if bytes.len() > CAPTURE_LIMIT {
            Self {
                bytes: bytes.slice(..CAPTURE_LIMIT),
                truncated: true,
            }
        } else {
            Self {
                bytes,
                truncated: false,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseRecord {
    pub status: u16,
    pub headers: Headers,
    pub body: Captured,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pending,
    Response(ResponseRecord),
    /// No response reached the client; the text says why.
    Failed(String),
}

/// One request and, once finished, its response.
#[derive(Debug, Clone)]
pub struct Exchange {
    pub id: u64,
    pub started_at: DateTime<Local>,
    pub method: String,
    /// Path without the query string.
    pub path: String,
    /// Path and query as the client sent them.
    pub uri: String,
    pub request_headers: Headers,
    pub request_body: Captured,
    pub outcome: Outcome,
    pub duration: Option<Duration>,
    /// Label of the scenario that shaped the response, if any.
    pub scenario: Option<String>,
}

/// The parts of an [`Exchange`] known when the request arrives.
#[derive(Debug, Clone)]
pub struct NewExchange {
    pub method: String,
    pub path: String,
    pub uri: String,
    pub request_headers: Headers,
    pub request_body: Captured,
    pub scenario: Option<String>,
}

#[derive(Debug, Clone)]
pub enum StoreEvent {
    /// An exchange was added, or the one with this id was updated.
    Upsert(Arc<Exchange>),
    Cleared,
}

/// Shared store of recent exchanges. Readers follow [`Store::subscribe`] and
/// keep their own copy, so a UI never takes this lock on its render path.
pub struct Store {
    entries: Mutex<VecDeque<Arc<Exchange>>>,
    capacity: usize,
    next_id: AtomicU64,
    events: broadcast::Sender<StoreEvent>,
}

impl Store {
    /// Creates a store holding at most `capacity` exchanges (at least one).
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            entries: Mutex::new(VecDeque::with_capacity(capacity)),
            capacity,
            next_id: AtomicU64::new(1),
            events: broadcast::channel(EVENT_BACKLOG).0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn subscribe(&self) -> broadcast::Receiver<StoreEvent> {
        self.events.subscribe()
    }

    /// Records a new in-flight exchange and returns its id.
    pub fn begin(&self, new: NewExchange) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let exchange = Arc::new(Exchange {
            id,
            started_at: Local::now(),
            method: new.method,
            path: new.path,
            uri: new.uri,
            request_headers: new.request_headers,
            request_body: new.request_body,
            outcome: Outcome::Pending,
            duration: None,
            scenario: new.scenario,
        });
        let mut entries = self.lock();
        if entries.len() == self.capacity {
            entries.pop_front();
        }
        entries.push_back(exchange.clone());
        self.publish(StoreEvent::Upsert(exchange));
        id
    }

    /// Sets the outcome. Exchanges already evicted or cleared are ignored.
    pub fn finish(&self, id: u64, outcome: Outcome, duration: Duration) {
        let mut entries = self.lock();
        // Ids are assigned in order, so the deque is sorted by id.
        let Ok(index) = entries.binary_search_by_key(&id, |entry| entry.id) else {
            return;
        };
        let mut updated = Exchange::clone(&entries[index]);
        updated.outcome = outcome;
        updated.duration = Some(duration);
        let updated = Arc::new(updated);
        entries[index] = updated.clone();
        self.publish(StoreEvent::Upsert(updated));
    }

    pub fn clear(&self) {
        let mut entries = self.lock();
        entries.clear();
        self.publish(StoreEvent::Cleared);
    }

    /// All exchanges, oldest first.
    pub fn snapshot(&self) -> Vec<Arc<Exchange>> {
        self.lock().iter().cloned().collect()
    }

    /// The newest exchange satisfying `predicate`.
    pub fn latest(&self, predicate: impl Fn(&Exchange) -> bool) -> Option<Arc<Exchange>> {
        self.lock()
            .iter()
            .rev()
            .find(|entry| predicate(entry))
            .cloned()
    }

    fn lock(&self) -> MutexGuard<'_, VecDeque<Arc<Exchange>>> {
        // The deque is consistent after every statement, so poison is harmless.
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn publish(&self, event: StoreEvent) {
        // Sending fails only when nobody subscribes, as in tests.
        let _ = self.events.send(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new(path: &str) -> NewExchange {
        NewExchange {
            method: "GET".into(),
            path: path.into(),
            uri: path.into(),
            request_headers: Vec::new(),
            request_body: Captured::default(),
            scenario: None,
        }
    }

    fn paths(store: &Store) -> Vec<String> {
        store
            .snapshot()
            .iter()
            .map(|entry| entry.path.clone())
            .collect()
    }

    #[test]
    fn ring_buffer_evicts_oldest() {
        let store = Store::new(2);
        for path in ["/a", "/b", "/c"] {
            store.begin(new(path));
        }
        assert_eq!(paths(&store), ["/b", "/c"]);
    }

    #[test]
    fn zero_capacity_still_holds_one() {
        let store = Store::new(0);
        store.begin(new("/a"));
        store.begin(new("/b"));
        assert_eq!(paths(&store), ["/b"]);
    }

    #[test]
    fn finish_updates_entry_and_ignores_evicted_ids() {
        let store = Store::new(1);
        let evicted = store.begin(new("/a"));
        let live = store.begin(new("/b"));

        store.finish(evicted, Outcome::Failed("late".into()), Duration::ZERO);
        store.finish(
            live,
            Outcome::Failed("boom".into()),
            Duration::from_millis(5),
        );

        let entry = store.snapshot().remove(0);
        assert_eq!(entry.outcome, Outcome::Failed("boom".into()));
        assert_eq!(entry.duration, Some(Duration::from_millis(5)));
    }

    #[test]
    fn events_follow_changes() {
        let store = Store::new(4);
        let mut events = store.subscribe();
        let id = store.begin(new("/a"));
        store.finish(id, Outcome::Failed("x".into()), Duration::ZERO);
        store.clear();

        assert!(
            matches!(events.try_recv(), Ok(StoreEvent::Upsert(e)) if e.outcome == Outcome::Pending)
        );
        assert!(
            matches!(events.try_recv(), Ok(StoreEvent::Upsert(e)) if e.outcome != Outcome::Pending)
        );
        assert!(matches!(events.try_recv(), Ok(StoreEvent::Cleared)));
    }

    #[test]
    fn capture_cuts_long_bodies() {
        let captured = Captured::new(Bytes::from(vec![0u8; CAPTURE_LIMIT + 1]));
        assert!(captured.truncated);
        assert_eq!(captured.bytes.len(), CAPTURE_LIMIT);
    }

    #[test]
    fn latest_finds_newest_match() {
        let store = Store::new(4);
        store.begin(new("/a"));
        let newest = store.begin(new("/a"));
        store.begin(new("/b"));
        let found = store.latest(|entry| entry.path == "/a").expect("match");
        assert_eq!(found.id, newest);
    }
}
