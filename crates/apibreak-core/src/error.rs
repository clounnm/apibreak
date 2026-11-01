use std::path::PathBuf;

/// Errors reported by the core library.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid target URL `{0}`: expected http(s)://host[:port][/path]")]
    InvalidTarget(String),
    #[error("failed to build HTTP client: {0}")]
    Client(#[from] reqwest::Error),
    #[error("{action} {}: {source}", path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("accepting connection failed: {0}")]
    Accept(#[source] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
