// SPDX-License-Identifier: AGPL-3.0-or-later
use super::{OsSessionStore, SavedSession, StoreError, codec};
use security_framework::{base::Error, passwords};
use security_framework_sys::base::errSecItemNotFound;
use zeroize::Zeroizing;

const ACCOUNT: &str = "active-session";
// errSecUserCanceled in Security.framework's SecBase.h.
const USER_CANCELED: i32 = -128;

fn options(application: &str) -> passwords::PasswordOptions {
    let mut options = passwords::PasswordOptions::new_generic_password(application, ACCOUNT);
    options.set_access_synchronized(Some(false));
    options
}

fn store_error(error: Error) -> StoreError {
    match error.code() {
        USER_CANCELED => StoreError::UnlockDismissed,
        _ => StoreError::Unavailable,
    }
}

pub(super) fn load(application: &str) -> Result<Option<SavedSession>, StoreError> {
    match passwords::generic_password(options(application)) {
        Ok(data) => codec::decode(&Zeroizing::new(data)).map(Some),
        Err(error) if error.code() == errSecItemNotFound => Ok(None),
        Err(error) => Err(store_error(error)),
    }
}

pub(super) fn save(application: &str, session: SavedSession) -> Result<(), StoreError> {
    let data = codec::encode(&session)?;
    passwords::set_generic_password_options(&data, options(application)).map_err(store_error)
}

pub(super) fn clear(application: &str) -> Result<(), StoreError> {
    match passwords::delete_generic_password_options(options(application)) {
        Err(error) if error.code() == errSecItemNotFound => Ok(()),
        result => result.map_err(store_error),
    }
}

impl OsSessionStore {
    pub(super) async fn keychain_operation<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&str) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        // Security.framework can prompt synchronously. Retain the lock until
        // it returns so a canceled caller cannot reorder a later save or clear.
        let lock = self.operations.clone().lock_owned().await;
        let application = self.application.clone();
        tokio::task::spawn_blocking(move || {
            let _lock = lock;
            operation(&application)
        })
        .await
        .map_err(|_| StoreError::Unavailable)?
    }
}
