// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::handle::{AppStoreError, AppStoreHandle};
use napi::Task;
use napi::bindgen_prelude::{AsyncTask, Env, Error, Result, Status, ToNapiValue, TypeName};
use napi_derive::napi;
use std::path::PathBuf;

impl From<AppStoreError> for Error {
    fn from(error: AppStoreError) -> Self {
        Self::new(Status::GenericFailure, error.to_string())
    }
}

#[napi(object)]
pub struct AppStoreOptions {
    pub path: String,
    pub derivation_version: Option<String>,
}

type StoreJob<T> = Box<dyn FnOnce(&AppStoreHandle) -> std::result::Result<T, AppStoreError> + Send>;

pub struct StoreTask<T> {
    handle: AppStoreHandle,
    job: Option<StoreJob<T>>,
}

impl<T: Send + ToNapiValue + TypeName + 'static> Task for StoreTask<T> {
    type Output = T;
    type JsValue = T;

    fn compute(&mut self) -> Result<Self::Output> {
        let job = self.job.take().ok_or_else(|| {
            Error::new(
                Status::GenericFailure,
                "app store operation was already executed",
            )
        })?;
        Ok(job(&self.handle)?)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

#[napi]
pub struct AppStore {
    handle: AppStoreHandle,
}

impl AppStore {
    fn task<T: Send + ToNapiValue + TypeName + 'static>(
        &self,
        job: impl FnOnce(&AppStoreHandle) -> std::result::Result<T, AppStoreError> + Send + 'static,
    ) -> AsyncTask<StoreTask<T>> {
        AsyncTask::new(StoreTask {
            handle: self.handle.clone(),
            job: Some(Box::new(job)),
        })
    }
}

#[napi]
impl AppStore {
    #[napi(constructor)]
    pub fn new(options: AppStoreOptions) -> Result<Self> {
        let handle = AppStoreHandle::open(
            PathBuf::from(options.path),
            options.derivation_version.as_deref(),
        )?;
        Ok(Self { handle })
    }

    #[napi(getter)]
    pub fn initialization(&self) -> String {
        self.handle.initialization().to_owned()
    }

    #[napi]
    pub fn close(&self) -> Result<()> {
        Ok(self.handle.close()?)
    }

    #[napi]
    pub fn get_metadata(&self, key: String) -> AsyncTask<StoreTask<String>> {
        self.task(move |handle| handle.get_metadata(&key))
    }

    #[napi]
    pub fn set_metadata(&self, key: String, value: String) -> AsyncTask<StoreTask<()>> {
        self.task(move |handle| handle.set_metadata(&key, &value))
    }

    #[napi]
    pub fn get_all_accounts(&self) -> AsyncTask<StoreTask<String>> {
        self.task(|handle| handle.get_all_accounts())
    }

    #[napi]
    pub fn get_account(&self, storage_key: String) -> AsyncTask<StoreTask<String>> {
        self.task(move |handle| handle.get_account(&storage_key))
    }

    #[napi]
    pub fn upsert_account(&self, payload: String) -> AsyncTask<StoreTask<()>> {
        self.task(move |handle| handle.upsert_account(&payload))
    }

    #[napi]
    pub fn compare_and_swap_account(&self, payload: String) -> AsyncTask<StoreTask<bool>> {
        self.task(move |handle| handle.compare_and_swap_account(&payload))
    }

    #[napi]
    pub fn delete_account(&self, storage_key: String) -> AsyncTask<StoreTask<()>> {
        self.task(move |handle| handle.delete_account(&storage_key))
    }

    #[napi]
    pub fn import_accounts(&self, payload: String) -> AsyncTask<StoreTask<String>> {
        self.task(move |handle| handle.import_accounts(&payload))
    }

    #[napi]
    pub fn get_all_known_instances(&self) -> AsyncTask<StoreTask<String>> {
        self.task(|handle| handle.get_all_known_instances())
    }

    #[napi]
    pub fn upsert_known_instance(&self, payload: String) -> AsyncTask<StoreTask<()>> {
        self.task(move |handle| handle.upsert_known_instance(&payload))
    }

    #[napi]
    pub fn delete_known_instance(&self, key: String) -> AsyncTask<StoreTask<()>> {
        self.task(move |handle| handle.delete_known_instance(&key))
    }

    #[napi]
    pub fn get_entries(&self, store: String, scope: String) -> AsyncTask<StoreTask<String>> {
        self.task(move |handle| handle.get_entries(&store, &scope))
    }

    #[napi]
    pub fn get_entry(
        &self,
        store: String,
        scope: String,
        key: String,
    ) -> AsyncTask<StoreTask<String>> {
        self.task(move |handle| handle.get_entry(&store, &scope, &key))
    }

    #[napi]
    pub fn set_entry(&self, payload: String) -> AsyncTask<StoreTask<()>> {
        self.task(move |handle| handle.set_entry(&payload))
    }

    #[napi]
    pub fn delete_entry(
        &self,
        store: String,
        scope: String,
        key: String,
    ) -> AsyncTask<StoreTask<()>> {
        self.task(move |handle| handle.delete_entry(&store, &scope, &key))
    }

    #[napi]
    pub fn clear_scope(&self, scope: String) -> AsyncTask<StoreTask<()>> {
        self.task(move |handle| handle.clear_scope(&scope))
    }

    #[napi]
    pub fn clear_store_except(&self, payload: String) -> AsyncTask<StoreTask<()>> {
        self.task(move |handle| handle.clear_store_except(&payload))
    }

    #[napi]
    pub fn import_entries(&self, payload: String) -> AsyncTask<StoreTask<String>> {
        self.task(move |handle| handle.import_entries(&payload))
    }

    #[napi]
    pub fn prune(&self, payload: String) -> AsyncTask<StoreTask<String>> {
        self.task(move |handle| handle.prune(&payload))
    }
}
