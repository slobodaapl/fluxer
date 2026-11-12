// SPDX-License-Identifier: AGPL-3.0-or-later
//! One active native session, stored only through an encrypted OS credential
//! service. The injectable memory store is for tests and explicit ephemeral use.
mod codec;
mod os;

use fluxer_core::Snowflake;
use fluxer_protocol::{Instance, SessionToken};
use serde_json::Value;
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use zeroize::Zeroizing;

pub use os::OsSessionStore;

#[derive(Clone)]
pub struct SavedSession {
    pub instance: Instance,
    pub token: SessionToken,
    pub user_id: Snowflake,
    pub user: Option<Value>,
    pub locale: String,
    pub selected_guild: Option<Snowflake>,
    pub selected_channel: Option<Snowflake>,
    /// An invalidated record remains available for account display, but must
    /// never be automatically authenticated until a fresh sign-in succeeds.
    pub valid: bool,
}
impl SavedSession {
    pub fn account_key(&self) -> String {
        format!(
            "{}::{}",
            self.instance.api.as_str().trim_end_matches('/'),
            self.user_id
        )
    }
}
impl fmt::Debug for SavedSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // No instance document, account profile, user ID or token in logs.
        f.debug_struct("SavedSession")
            .field("valid", &self.valid)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("The saved session is malformed or belongs to a different instance")]
    InvalidRecord,
    #[error("The operating system credential vault is unavailable or locked")]
    Unavailable,
    #[error("Secure session storage is unsupported on this platform")]
    Unsupported,
}
pub type StoreFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, StoreError>> + Send + 'a>>;
pub trait SessionStore: Send + Sync {
    fn load(&self) -> StoreFuture<'_, Option<SavedSession>>;
    fn save(&self, session: SavedSession) -> StoreFuture<'_, ()>;
    fn clear(&self) -> StoreFuture<'_, ()>;
}

/// Clones share the serialized record across recreated workers. Production does
/// not select this store when OS secure storage fails.
#[derive(Clone, Default)]
pub struct MemorySessionStore {
    record: Arc<Mutex<Option<Zeroizing<Vec<u8>>>>>,
}
impl fmt::Debug for MemorySessionStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MemorySessionStore([REDACTED])")
    }
}
impl SessionStore for MemorySessionStore {
    fn load(&self) -> StoreFuture<'_, Option<SavedSession>> {
        Box::pin(async move {
            let record = self.record.lock().map_err(|_| StoreError::Unavailable)?;
            record
                .as_ref()
                .map(|value| codec::decode(value))
                .transpose()
        })
    }
    fn save(&self, session: SavedSession) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            let value = codec::encode(&session)?;
            *self.record.lock().map_err(|_| StoreError::Unavailable)? = Some(value);
            Ok(())
        })
    }
    fn clear(&self) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            self.record
                .lock()
                .map_err(|_| StoreError::Unavailable)?
                .take();
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests;
