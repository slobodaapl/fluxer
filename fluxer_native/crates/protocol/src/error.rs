use serde_json::Value;
use std::time::Duration;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid invite response")]
    InvalidInviteResponse,
    #[error("message search returned no valid response body")]
    InvalidSearchResponse,
    #[error("upload plan omitted an attachment ID")]
    AttachmentPlanMissing,
    #[error("unable to finalize multipart attachment upload: {0}")]
    AttachmentCompletion(Box<Error>),
    #[error("attachment upload cancelled")]
    UploadCancelled,
    #[error("invalid attachment upload: {0}")]
    InvalidUpload(&'static str),
    #[error("unable to read attachment")]
    AttachmentIo(#[source] std::io::Error),
    #[error("storage upload failed (HTTP {status})")]
    StorageUpload { status: u16 },
    #[error("presigned attachment upload failed and multipart message fallback exceeds 25 MiB")]
    UploadFallbackUnavailable,
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
