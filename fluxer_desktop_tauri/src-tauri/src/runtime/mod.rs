// SPDX-License-Identifier: AGPL-3.0-or-later

//! The desktop runtime config: how the renderer finds its instance and switches between
//! instances, the port of DesktopRuntimeConfigIpc.ts and the modules behind it.

pub mod commands;
pub(crate) mod discovery;
pub(crate) mod errors;
pub(crate) mod fetch;
mod markers;
pub mod plans;
mod request;
mod resolution;
mod transactions;

use std::sync::Arc;

use serde_json::Value;

use self::discovery::RuntimeDiscovery;
pub use self::markers::Markers;
use self::plans::{DesktopRuntimePlan, RuntimePlans};
use self::resolution::ResolutionRegistry;
use self::transactions::{
    DesktopRuntimeAbort, DesktopRuntimeCommit, DesktopRuntimeDeactivation,
    DesktopRuntimeFinalization, DesktopRuntimePreparation, DesktopRuntimeRollback,
    TransactionRegistry,
};
use crate::config::launch::{instance_endpoint_override, process_args};
use crate::documents::{Caller, DocumentRegistry};
use crate::error::BridgeError;
use crate::net::outbound::OutboundHttp;

const INITIAL_INPUT_CONTEXT: &str = "Desktop runtime config initial input";

pub struct RuntimeConfig {
    documents: Arc<DocumentRegistry>,
    plans: Arc<RuntimePlans>,
    resolutions: ResolutionRegistry,
    transactions: TransactionRegistry,
}

impl RuntimeConfig {
    pub fn new(
        outbound: Arc<OutboundHttp>,
        markers: Markers,
        documents: Arc<DocumentRegistry>,
    ) -> Self {
        let discovery = Arc::new(RuntimeDiscovery::new(outbound, markers));
        let plans = Arc::new(RuntimePlans::new(discovery));
        Self {
            resolutions: ResolutionRegistry::new(documents.clone(), plans.clone()),
            transactions: TransactionRegistry::new(documents.clone(), plans.clone()),
            documents,
            plans,
        }
    }

    /// The plans the local app protocol routes `/api/<instance key>` requests by.
    pub fn documents(&self) -> &Arc<DocumentRegistry> {
        &self.documents
    }

    pub fn plans(&self) -> Arc<RuntimePlans> {
        self.plans.clone()
    }

    /// The outbound HTTP client the plans' origins are registered with.
    pub fn outbound(&self) -> Arc<OutboundHttp> {
        self.plans.discovery().outbound.clone()
    }

    /// `cleanupDesktopRuntimeConfigHandlers`: aborts every resolution and preparation and rolls
    /// back a provisional commit.
    pub fn cleanup(&self) {
        self.resolutions.cleanup();
        self.transactions.cleanup();
    }

    pub fn initial_input(&self, caller: &Caller) -> Result<Option<String>, BridgeError> {
        self.documents
            .capture_for_read(caller, INITIAL_INPUT_CONTEXT)?;
        // `desktopRuntimeInitialInput`; a debug build stands in for Electron's unpackaged app.
        instance_endpoint_override(&process_args(), cfg!(debug_assertions))
            .map_err(|message| BridgeError::new("Error", message))
    }

    pub async fn resolve(
        &self,
        caller: &Caller,
        request: &Value,
    ) -> Result<DesktopRuntimePlan, BridgeError> {
        self.resolutions.resolve(caller, request).await
    }

    pub fn cancel_resolution(&self, caller: &Caller, request: &Value) -> Result<(), BridgeError> {
        self.resolutions.cancel(caller, request)
    }

    pub async fn prepare(
        &self,
        caller: &Caller,
        request: &Value,
    ) -> Result<DesktopRuntimePreparation, BridgeError> {
        self.transactions.prepare(caller, request).await
    }

    pub fn commit(
        &self,
        caller: &Caller,
        request: &Value,
    ) -> Result<DesktopRuntimeCommit, BridgeError> {
        self.transactions.commit(caller, request)
    }

    pub fn abort(
        &self,
        caller: &Caller,
        request: &Value,
    ) -> Result<DesktopRuntimeAbort, BridgeError> {
        self.transactions.abort(caller, request)
    }

    pub fn finalize(
        &self,
        caller: &Caller,
        request: &Value,
    ) -> Result<DesktopRuntimeFinalization, BridgeError> {
        self.transactions.finalize(caller, request)
    }

    pub fn rollback(
        &self,
        caller: &Caller,
        request: &Value,
    ) -> Result<DesktopRuntimeRollback, BridgeError> {
        self.transactions.rollback(caller, request)
    }

    pub fn deactivate(
        &self,
        caller: &Caller,
        request: &Value,
    ) -> Result<DesktopRuntimeDeactivation, BridgeError> {
        self.transactions.deactivate(caller, request)
    }
}

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
