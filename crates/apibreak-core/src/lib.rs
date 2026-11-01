//! Core of apibreak: a local reverse proxy that records traffic and can
//! inject failures. Nothing here knows about the terminal UI.

mod cors;
mod error;
pub mod proxy;

pub use error::{Error, Result};
pub use proxy::{Proxy, ProxyConfig};
