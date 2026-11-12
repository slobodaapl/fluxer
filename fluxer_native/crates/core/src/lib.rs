//! Platform-independent Fluxer domain models and deterministic client state.
//!
//! This crate has no network, filesystem, UI, or media dependencies. Wire models
//! follow the schemas in `packages/schema`; see README.md for migration limits.
mod demo;
mod id;
mod models;
mod permissions;
mod state;

pub use id::Snowflake;
pub use models::*;
pub use permissions::*;
pub use state::*;
