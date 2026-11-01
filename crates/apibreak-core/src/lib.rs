//! Core of apibreak: a local reverse proxy that records traffic and can
//! inject failures. Nothing here knows about the terminal UI.

pub mod body;
mod cors;
mod error;
pub mod proxy;
pub mod store;

pub use error::{Error, Result};
pub use proxy::{Proxy, ProxyConfig};
pub use store::{Exchange, Outcome, Store, StoreEvent};
