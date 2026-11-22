// SPDX-License-Identifier: AGPL-3.0-or-later
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::codec;
use crate::{SavedSession, SessionStore, StoreError, StoreFuture};

#[cfg(target_os = "macos")]
mod keychain;

/// One application-scoped credential item. Construction does not access or
/// unlock the vault. Operations never fall back to plaintext.
#[derive(Clone)]
pub struct OsSessionStore {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    application: String,
    #[cfg(target_os = "macos")]
    operations: std::sync::Arc<tokio::sync::Mutex<()>>,
}
impl Default for OsSessionStore {
    fn default() -> Self {
        Self::new()
    }
}
impl OsSessionStore {
    pub fn new() -> Self {
        Self {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            application: "app.fluxer.Native".into(),
            #[cfg(target_os = "macos")]
            operations: Default::default(),
        }
    }
    #[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
    pub(crate) fn synthetic(application: String) -> Self {
        let mut store = Self::new();
        store.application = application;
        store
    }
    #[cfg(target_os = "linux")]
    fn attributes(&self) -> std::collections::HashMap<&str, &str> {
        std::collections::HashMap::from([
            ("application", self.application.as_str()),
            ("kind", "active-session"),
        ])
    }
    #[cfg(target_os = "linux")]
    async fn service() -> Result<oo7::dbus::Service<'static>, StoreError> {
        // Keyring::new / Service::new can fall back to an unencrypted D-Bus
        // session. Request encrypted negotiation explicitly and fail closed.
        oo7::dbus::Service::encrypted()
            .await
            .map_err(|_| StoreError::Unavailable)
    }
    #[cfg(target_os = "linux")]
    async fn collection(&self) -> Result<oo7::dbus::Collection<'static>, StoreError> {
        let collection = Self::service()
            .await?
            .default_collection()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        if collection
            .is_locked()
            .await
            .map_err(|_| StoreError::Unavailable)?
        {
            return Err(StoreError::Unavailable);
        }
        Ok(collection)
    }
}
impl std::fmt::Debug for OsSessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OsSessionStore")
    }
}
impl SessionStore for OsSessionStore {
    fn load(&self) -> StoreFuture<'_, Option<SavedSession>> {
        Box::pin(async move {
            #[cfg(target_os = "linux")]
            {
                // Lookup attributes are available on locked items. Request
                // the OS unlock prompt only for the matching session item.
                let service = Self::service().await?;
                let Some(collection) = service
                    .with_alias(oo7::dbus::Service::DEFAULT_COLLECTION)
                    .await
                    .map_err(|_| StoreError::Unavailable)?
                else {
                    return Ok(None);
                };
                let items = collection
                    .search_items(&self.attributes())
                    .await
                    .map_err(|_| StoreError::Unavailable)?;
                if items.len() > 1 {
                    return Err(StoreError::InvalidRecord);
                }
                let Some(item) = items.first() else {
                    return Ok(None);
                };
                if item
                    .is_locked()
                    .await
                    .map_err(|_| StoreError::Unavailable)?
                {
                    item.unlock(None).await.map_err(|error| match error {
                        oo7::dbus::Error::Dismissed => StoreError::UnlockDismissed,
                        _ => StoreError::Unavailable,
                    })?;
                }
                let secret = item.secret().await.map_err(|_| StoreError::Unavailable)?;
                codec::decode(secret.as_bytes()).map(Some)
            }
            #[cfg(target_os = "macos")]
            {
                self.keychain_operation(keychain::load).await
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                Err(StoreError::Unsupported)
            }
        })
    }
    fn save(&self, session: SavedSession) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            #[cfg(target_os = "linux")]
            {
                let encoded = codec::encode(&session)?;
                let collection = self.collection().await?;
                collection
                    .create_item(
                        "Fluxer Native active session",
                        &self.attributes(),
                        oo7::Secret::blob(encoded.as_slice()),
                        true,
                        None,
                    )
                    .await
                    .map_err(|_| StoreError::Unavailable)?;
                Ok(())
            }
            #[cfg(target_os = "macos")]
            {
                self.keychain_operation(move |application| keychain::save(application, session))
                    .await
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                let _ = session;
                Err(StoreError::Unsupported)
            }
        })
    }
    fn clear(&self) -> StoreFuture<'_, ()> {
        Box::pin(async move {
            #[cfg(target_os = "linux")]
            {
                let collection = self.collection().await?;
                let items = collection
                    .search_items(&self.attributes())
                    .await
                    .map_err(|_| StoreError::Unavailable)?;
                for item in items {
                    item.delete(None)
                        .await
                        .map_err(|_| StoreError::Unavailable)?;
                }
                Ok(())
            }
            #[cfg(target_os = "macos")]
            {
                self.keychain_operation(keychain::clear).await
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            {
                Err(StoreError::Unsupported)
            }
        })
    }
}
