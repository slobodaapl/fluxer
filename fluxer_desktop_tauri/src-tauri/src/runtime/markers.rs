// SPDX-License-Identifier: AGPL-3.0-or-later

//! Marker access for runtime discovery. The desktop app store type is private to the storage
//! module, so the shell hands its `get_marker`/`set_marker` over as closures.

use std::future::Future;
use std::sync::Arc;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;

use crate::error::BridgeError;

type MarkerRead = BoxFuture<'static, Result<Option<String>, BridgeError>>;
type MarkerWrite = BoxFuture<'static, Result<(), BridgeError>>;

pub struct Markers {
    get: Box<dyn Fn(String) -> MarkerRead + Send + Sync>,
    set: Box<dyn Fn(String, String) -> MarkerWrite + Send + Sync>,
}

impl Markers {
    /// Markers stored in `store`, read by `get(store, key)` and written by
    /// `set(store, key, value)`.
    pub fn over<S, G, GF, W, WF>(store: Arc<S>, get: G, set: W) -> Self
    where
        S: Send + Sync + 'static,
        G: Fn(Arc<S>, String) -> GF + Send + Sync + 'static,
        GF: Future<Output = Result<Option<String>, BridgeError>> + Send + 'static,
        W: Fn(Arc<S>, String, String) -> WF + Send + Sync + 'static,
        WF: Future<Output = Result<(), BridgeError>> + Send + 'static,
    {
        let read_store = store.clone();
        Self {
            get: Box::new(move |key| get(read_store.clone(), key).boxed()),
            set: Box::new(move |key, value| set(store.clone(), key, value).boxed()),
        }
    }

    pub fn get(&self, key: String) -> MarkerRead {
        (self.get)(key)
    }

    pub fn set(&self, key: String, value: String) -> MarkerWrite {
        (self.set)(key, value)
    }
}
