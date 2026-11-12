//! Native Fluxer REST and gateway transport. Wire responses remain JSON so newly
//! introduced server fields can be consumed without upgrading this crate.
mod attachments;
mod error;
mod gateway;
mod http;
mod installation;
mod instance;

pub use error::{Error, Result};
pub use gateway::{
    Gateway, GatewayCommand, GatewayEvent, ResumeSession, VoiceServerUpdate, VoiceStateUpdate,
};
pub use http::{
    AuthSession, Client, DesktopHandoffPoll, DesktopHandoffSession, DesktopHandoffStatus,
    HandoffPollSecret, LoginIdentifier, LoginOutcome, SessionToken,
};
pub use instance::Instance;
pub use reqwest::Method;
pub use url::Url;

pub use attachments::{
    UploadAttachment, UploadCancellation, UploadPhase, UploadProgress, UploadProgressCallback,
};
