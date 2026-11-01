//! Core of apibreak: a local reverse proxy that records traffic and can
//! inject failures. Nothing here knows about the terminal UI.

pub mod body;
mod cors;
mod error;
pub mod mock;
pub mod proxy;
pub mod scenario;
pub mod store;

pub use error::{Error, Result};
pub use mock::{Mock, MockStore};
pub use proxy::{Proxy, ProxyConfig};
pub use scenario::{Action, Endpoint, Scenario, ScenarioHandle, ScenarioSet};
pub use store::{Exchange, Outcome, Store, StoreEvent};
