//! Platform-independent Fluxer domain models and deterministic client state.
//!
//! This crate has no network, filesystem, UI, or media dependencies. Wire models
//! follow the schemas in `packages/schema`; see README.md for migration limits.
mod attachment_policy;
mod demo;
mod id;
mod models;
mod permissions;
mod state;
mod voice;

pub use attachment_policy::*;
pub use id::Snowflake;
pub use models::*;
pub use permissions::*;
pub use state::*;
pub use voice::*;
