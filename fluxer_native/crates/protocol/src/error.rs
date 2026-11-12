use serde_json::Value;
use std::time::Duration;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("operating system randomness is unavailable for the client installation ID")]
    InstallationIdUnavailable,
    #[error("invalid endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("instance discovery failed: {0}")]
    Discovery(String),
    #[error("instance discovery changed account authority from {expected} to {actual}")]
    DiscoveryIdentityChanged { expected: String, actual: String },
    #[error("unsupported instance generation: {0}")]
    UnsupportedGeneration(String),
    #[error("response exceeded {0} byte limit")]
    ResponseTooLarge(usize),
    #[error("HTTP transport failed")]
    Http(#[from] reqwest::Error),
    #[error("invalid JSON response")]
    Json(#[from] serde_json::Error),
    #[error("API request failed (HTTP {status}, code {code:?})")]
    Api {
        status: u16,
        code: Option<String>,
        body: Value,
    },
    #[error("rate limited; retry after {retry_after:?}")]
    RateLimited { retry_after: Duration, global: bool },
    #[error("authentication response omitted token or user ID")]
    InvalidAuthResponse,
    #[error("gateway transport failed")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("gateway protocol error: {0}")]
    Gateway(String),
}
