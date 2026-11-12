// SPDX-License-Identifier: AGPL-3.0-or-later
#[cfg(target_os = "linux")]
use crate::codec;
use crate::{SavedSession, SessionStore, StoreError, StoreFuture};

/// One application-scoped encrypted Secret Service item. Construction itself
/// does not access or unlock the vault. Operations never fall back to plaintext.
#[derive(Clone)]
pub struct OsSessionStore {
    #[cfg(target_os = "linux")]
    application: String,
}
impl Default for OsSessionStore {
    fn default() -> Self {
        Self::new()
    }
}
impl OsSessionStore {
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            application: "app.fluxer.Native".into(),
        }
    }
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn synthetic(application: String) -> Self {
        Self { application }
    }
    #[cfg(target_os = "linux")]
    fn attributes(&self) -> std::collections::HashMap<&str, &str> {
        std::collections::HashMap::from([
            ("application", self.application.as_str()),
            ("kind", "active-session"),
        ])
    }
    #[cfg(target_os = "linux")]
    async fn collection(&self) -> Result<oo7::dbus::Collection<'static>, StoreError> {
        // Keyring::new / Service::new can fall back to an unencrypted D-Bus
        // session. Request encrypted negotiation explicitly and fail closed.
        let service = oo7::dbus::Service::encrypted()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let collection = service
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
                let collection = self.collection().await?;
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
                let secret = item.secret().await.map_err(|_| StoreError::Unavailable)?;
                codec::decode(secret.as_bytes()).map(Some)
            }
            #[cfg(not(target_os = "linux"))]
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
            #[cfg(not(target_os = "linux"))]
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
            #[cfg(not(target_os = "linux"))]
            {
                Err(StoreError::Unsupported)
            }
        })
    }
}
