// SPDX-License-Identifier: AGPL-3.0-or-later

//! The route table of DesktopAppStorageIpc.ts. Each command admits only the privileged
//! renderer document, as IpcHandlers.ts did for every app-store channel, and the Electron
//! channel names it served are the gate's context.
//!
//! Electron's gate (`requirePrivilegedRendererDocumentSender`) checked the sending frame but
//! not whether the page was still loading, and the renderer reads storage while it boots, so
//! the gate here is `capture_for_read`.
//!
//! Account writes no longer notify an account engine: the Tauri shell has none yet.

use std::sync::Arc;

use serde_json::Value;
use tauri::{State, Webview};

use super::operations::StorageEntry;
use super::service::{DesktopAppStorage, StoreStatus};
use super::validation::AccountRecord;
use crate::documents::{Caller, DocumentRegistry};
use crate::error::BridgeError;

type Documents<'a> = State<'a, Arc<DocumentRegistry>>;
type Storage<'a> = State<'a, Arc<DesktopAppStorage>>;
type Reply<T> = Result<T, BridgeError>;

fn gate(
    webview: &Webview,
    documents: &DocumentRegistry,
    token: String,
    channel: &str,
) -> Reply<()> {
    documents
        .capture_for_read(&Caller::new(webview, token), channel)
        .map(drop)
}

fn arg(value: Option<Value>) -> Value {
    value.unwrap_or_default()
}

#[tauri::command]
pub async fn desktop_accounts_get_all(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
) -> Reply<Vec<AccountRecord>> {
    gate(
        &webview,
        &documents,
        document_token,
        "desktop-accounts:get-all",
    )?;
    storage.get_all_accounts().await
}

#[tauri::command]
pub async fn desktop_accounts_get(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    storage_key: Option<Value>,
) -> Reply<Option<AccountRecord>> {
    gate(&webview, &documents, document_token, "desktop-accounts:get")?;
    storage.get_account(&arg(storage_key)).await
}

#[tauri::command]
pub async fn desktop_accounts_upsert(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    account: Option<Value>,
) -> Reply<()> {
    gate(
        &webview,
        &documents,
        document_token,
        "desktop-accounts:upsert",
    )?;
    storage.upsert_account(&arg(account)).await
}

#[tauri::command]
pub async fn desktop_accounts_compare_and_swap(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    request: Option<Value>,
) -> Reply<bool> {
    gate(
        &webview,
        &documents,
        document_token,
        "desktop-accounts:compare-and-swap",
    )?;
    storage.compare_and_swap_account(&arg(request)).await
}

#[tauri::command]
pub async fn desktop_accounts_delete(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    storage_key: Option<Value>,
) -> Reply<()> {
    gate(
        &webview,
        &documents,
        document_token,
        "desktop-accounts:delete",
    )?;
    storage.delete_account(&arg(storage_key)).await
}

#[tauri::command]
pub async fn desktop_accounts_import(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    request: Option<Value>,
) -> Reply<Value> {
    gate(
        &webview,
        &documents,
        document_token,
        "desktop-accounts:import",
    )?;
    storage.import_accounts(&arg(request)).await
}

#[tauri::command]
pub async fn desktop_accounts_prune(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    request: Option<Value>,
) -> Reply<Value> {
    gate(
        &webview,
        &documents,
        document_token,
        "desktop-accounts:prune",
    )?;
    storage.prune_accounts(&arg(request)).await
}

#[tauri::command]
pub async fn desktop_storage_get_status(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
) -> Reply<StoreStatus> {
    gate(
        &webview,
        &documents,
        document_token,
        "desktop-storage:get-status",
    )?;
    Ok(storage.status())
}

#[tauri::command]
pub async fn desktop_storage_load(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    scope: Option<Value>,
) -> Reply<Vec<StorageEntry>> {
    gate(&webview, &documents, document_token, "desktop-storage:load")?;
    storage.load_entries(&arg(scope)).await
}

#[tauri::command]
pub async fn desktop_storage_get(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    scope: Option<Value>,
    key: Option<Value>,
) -> Reply<Option<StorageEntry>> {
    gate(&webview, &documents, document_token, "desktop-storage:get")?;
    storage.get_entry(&arg(scope), &arg(key)).await
}

#[tauri::command]
pub async fn desktop_storage_set(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    scope: Option<Value>,
    key: Option<Value>,
    value: Option<Value>,
) -> Reply<()> {
    gate(&webview, &documents, document_token, "desktop-storage:set")?;
    storage.set_entry(&arg(scope), &arg(key), &arg(value)).await
}

#[tauri::command]
pub async fn desktop_storage_delete(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    scope: Option<Value>,
    key: Option<Value>,
) -> Reply<()> {
    gate(
        &webview,
        &documents,
        document_token,
        "desktop-storage:delete",
    )?;
    storage.delete_entry(&arg(scope), &arg(key)).await
}

#[tauri::command]
pub async fn desktop_storage_clear_all_for_scope(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    scope: Option<Value>,
) -> Reply<()> {
    gate(
        &webview,
        &documents,
        document_token,
        "desktop-storage:clear-all-for-scope",
    )?;
    storage.clear_all_for_scope(&arg(scope)).await
}

#[tauri::command]
pub async fn desktop_storage_clear_all_except(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    keys_to_keep: Option<Value>,
) -> Reply<()> {
    gate(
        &webview,
        &documents,
        document_token,
        "desktop-storage:clear-all-except",
    )?;
    storage.clear_all_except(&arg(keys_to_keep)).await
}

#[tauri::command]
pub async fn desktop_storage_set_many(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    writes: Option<Value>,
) -> Reply<()> {
    gate(
        &webview,
        &documents,
        document_token,
        "desktop-storage:set-many",
    )?;
    storage.set_many_entries(&arg(writes)).await
}

#[tauri::command]
pub async fn desktop_storage_import(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    request: Option<Value>,
) -> Reply<Value> {
    gate(
        &webview,
        &documents,
        document_token,
        "desktop-storage:import",
    )?;
    storage.import_entries(&arg(request)).await
}

#[tauri::command]
pub async fn desktop_storage_get_marker(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    key: Option<Value>,
) -> Reply<Option<String>> {
    gate(
        &webview,
        &documents,
        document_token,
        "desktop-storage:get-marker",
    )?;
    storage.get_marker_value(&arg(key)).await
}

#[tauri::command]
pub async fn desktop_storage_set_marker(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    key: Option<Value>,
    value: Option<Value>,
) -> Reply<()> {
    gate(
        &webview,
        &documents,
        document_token,
        "desktop-storage:set-marker",
    )?;
    storage.set_marker_value(&arg(key), &arg(value)).await
}

#[tauri::command]
pub async fn desktop_known_instances_get_all(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
) -> Reply<Vec<Value>> {
    gate(
        &webview,
        &documents,
        document_token,
        "desktop-known-instances:get-all",
    )?;
    storage.get_all_known_instances().await
}

#[tauri::command]
pub async fn desktop_known_instances_upsert(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    instance: Option<Value>,
) -> Reply<()> {
    gate(
        &webview,
        &documents,
        document_token,
        "desktop-known-instances:upsert",
    )?;
    storage.upsert_known_instance(&arg(instance)).await
}

#[tauri::command]
pub async fn desktop_known_instances_delete(
    webview: Webview,
    documents: Documents<'_>,
    storage: Storage<'_>,
    document_token: String,
    instance_key: Option<Value>,
) -> Reply<()> {
    gate(
        &webview,
        &documents,
        document_token,
        "desktop-known-instances:delete",
    )?;
    storage.delete_known_instance(&arg(instance_key)).await
}
