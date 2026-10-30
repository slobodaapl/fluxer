// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `desktop-runtime-config:*` channels of DesktopRuntimeConfigIpc.ts as Tauri commands. The
//! bridge sends each request object as `request`; a missing one reads as `null` and is refused
//! as malformed, like an `undefined` IPC argument was.

use std::sync::Arc;

use serde_json::Value;
use tauri::{State, Webview};

use super::RuntimeConfig;
use super::plans::DesktopRuntimePlan;
use super::transactions::{
    DesktopRuntimeAbort, DesktopRuntimeCommit, DesktopRuntimeDeactivation,
    DesktopRuntimeFinalization, DesktopRuntimePreparation, DesktopRuntimeRollback,
};
use crate::documents::Caller;
use crate::error::BridgeError;

type Runtime<'a> = State<'a, Arc<RuntimeConfig>>;
type Reply<T> = Result<T, BridgeError>;

fn arg(request: Option<Value>) -> Value {
    request.unwrap_or_default()
}

#[tauri::command]
pub async fn desktop_runtime_config_initial_input(
    webview: Webview,
    runtime: Runtime<'_>,
    document_token: String,
) -> Reply<Option<String>> {
    runtime.initial_input(&Caller::new(&webview, document_token))
}

#[tauri::command]
pub async fn desktop_runtime_config_resolve(
    webview: Webview,
    runtime: Runtime<'_>,
    document_token: String,
    request: Option<Value>,
) -> Reply<DesktopRuntimePlan> {
    let caller = Caller::new(&webview, document_token);
    runtime.resolve(&caller, &arg(request)).await
}

#[tauri::command]
pub async fn desktop_runtime_config_cancel_resolution(
    webview: Webview,
    runtime: Runtime<'_>,
    document_token: String,
    request: Option<Value>,
) -> Reply<()> {
    runtime.cancel_resolution(&Caller::new(&webview, document_token), &arg(request))
}

#[tauri::command]
pub async fn desktop_runtime_config_prepare(
    webview: Webview,
    runtime: Runtime<'_>,
    document_token: String,
    request: Option<Value>,
) -> Reply<DesktopRuntimePreparation> {
    let caller = Caller::new(&webview, document_token);
    runtime.prepare(&caller, &arg(request)).await
}

#[tauri::command]
pub async fn desktop_runtime_config_commit(
    webview: Webview,
    runtime: Runtime<'_>,
    document_token: String,
    request: Option<Value>,
) -> Reply<DesktopRuntimeCommit> {
    runtime.commit(&Caller::new(&webview, document_token), &arg(request))
}

#[tauri::command]
pub async fn desktop_runtime_config_abort(
    webview: Webview,
    runtime: Runtime<'_>,
    document_token: String,
    request: Option<Value>,
) -> Reply<DesktopRuntimeAbort> {
    runtime.abort(&Caller::new(&webview, document_token), &arg(request))
}

#[tauri::command]
pub async fn desktop_runtime_config_finalize(
    webview: Webview,
    runtime: Runtime<'_>,
    document_token: String,
    request: Option<Value>,
) -> Reply<DesktopRuntimeFinalization> {
    runtime.finalize(&Caller::new(&webview, document_token), &arg(request))
}

#[tauri::command]
pub async fn desktop_runtime_config_rollback(
    webview: Webview,
    runtime: Runtime<'_>,
    document_token: String,
    request: Option<Value>,
) -> Reply<DesktopRuntimeRollback> {
    runtime.rollback(&Caller::new(&webview, document_token), &arg(request))
}

#[tauri::command]
pub async fn desktop_runtime_config_deactivate(
    webview: Webview,
    runtime: Runtime<'_>,
    document_token: String,
    request: Option<Value>,
) -> Reply<DesktopRuntimeDeactivation> {
    runtime.deactivate(&Caller::new(&webview, document_token), &arg(request))
}
