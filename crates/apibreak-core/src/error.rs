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
    #[error("invalid scenario: {0}")]
    InvalidScenario(String),
    #[error("invalid config {}: {message}", path.display())]
    InvalidConfig { path: PathBuf, message: String },
    #[error("cannot watch config: {0}")]
    Watch(#[from] notify::Error),
    #[error("cannot record mock: {0}")]
    CannotRecord(String),
    #[error("invalid mock {file}: {source}")]
    InvalidMock {
        file: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("accepting connection failed: {0}")]
    Accept(#[source] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
