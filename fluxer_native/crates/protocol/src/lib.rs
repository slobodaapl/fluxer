//! Native Fluxer REST and gateway transport. Wire responses remain JSON so newly
//! introduced server fields can be consumed without upgrading this crate.
mod attachments;
mod error;
mod gateway;
mod gifs;
mod http;
mod installation;
mod instance;
mod invites;
mod moderation;
mod navigation;
mod sudo;
mod webhooks;

pub use error::{Error, Result};
pub use gateway::{
    Gateway, GatewayCommand, GatewayEvent, ResumeSession, VoiceServerUpdate, VoiceStateUpdate,
};
pub use gifs::{Gif, GifCategory, GifFeed, GifProvider, GifResults, GifShare};
pub use http::{
    AuthSession, Client, DesktopHandoffPoll, DesktopHandoffSession, DesktopHandoffStatus,
    HandoffPollSecret, LoginIdentifier, LoginOutcome, SessionRevocationOutcome,
    SessionRevocationProof, SessionToken, forum_post_body, normalize_forum_content,
};
pub use instance::Instance;
pub use invites::{CommunityInvite, InvitePreview, valid_invite_code};
pub use reqwest::Method;
pub use sudo::{SudoChallenge, SudoMethods};
pub use url::Url;
pub use webhooks::Webhook;

pub use attachments::{
    PlannedAttachment, UploadAttachment, UploadCancellation, UploadPhase, UploadProgress,
    UploadProgressCallback,
};

pub use navigation::{SearchResponse, channel_search_poll_interval};
