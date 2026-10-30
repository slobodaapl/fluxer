// SPDX-License-Identifier: AGPL-3.0-or-later

//! Renderer document ownership, the Tauri counterpart of RendererDocumentOwner.ts.
//!
//! Electron identified a document by its WebContents and WebFrameMain objects. Tauri commands
//! only see the calling webview, so the bridge mints a 256-bit token when its main-frame
//! initialization script runs (once per document) and sends it with privileged calls. The
//! registry binds that token to the webview's current navigation generation:
//!
//! - a main-frame navigation start clears the token, bumps the generation, marks the slot
//!   loading and fires every watcher with `Navigated`;
//! - the next document registers its own token once, and only while the slot is empty;
//! - destroying the webview fires `Destroyed`.
//!
//! Residual gap against Electron: between navigation start and commit the old document still
//! runs and could take the empty slot. Only the trusted local app can reach these commands
//! (Tauri admits IPC from the local origin alone), so the token only has to tell the app's own
//! documents apart.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::error::BridgeError;
use crate::local_app::url::is_renderer_document_url;
use crate::window::MAIN_WINDOW_LABEL;

const TOKEN_HEX_LENGTH: usize = 64;
const CAPTURE_ERROR: &str = "RendererDocumentCaptureError";
const INACTIVE_ERROR: &str = "InactiveRendererDocumentOwnerError";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvalidationReason {
    Navigated,
    Destroyed,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct DocumentOwner {
    label: String,
    token: String,
    generation: u64,
}

type Watcher = Box<dyn FnOnce(InvalidationReason) + Send>;

#[derive(Default)]
struct DocumentSlot {
    token: Option<String>,
    generation: u64,
    loading: bool,
    next_watcher: u64,
    watchers: Vec<(u64, Watcher)>,
}

/// Handle that unregisters its watcher when dropped, like `watcher.dispose()` in Electron.
pub struct WatchGuard {
    registry: Arc<DocumentRegistry>,
    label: String,
    generation: u64,
    id: u64,
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        self.registry
            .remove_watcher(&self.label, self.generation, self.id);
    }
}

#[derive(Default)]
pub struct DocumentRegistry {
    slots: Mutex<HashMap<String, DocumentSlot>>,
}

/// What a command knows about its caller: the invoking webview and the token its bridge sent.
pub struct Caller {
    label: String,
    url: String,
    token: String,
}

impl Caller {
    pub fn new(webview: &tauri::Webview, token: String) -> Self {
        Self {
            label: webview.label().to_owned(),
            url: webview.url().map(|url| url.to_string()).unwrap_or_default(),
            token,
        }
    }

    #[cfg(test)]
    pub fn for_test(label: &str, url: &str, token: &str) -> Self {
        Self {
            label: label.to_owned(),
            url: url.to_owned(),
            token: token.to_owned(),
        }
    }
}

impl DocumentRegistry {
    /// First call of a document's bridge. Fails if the slot already has a token.
    pub fn register(&self, label: &str, token: &str) -> Result<(), BridgeError> {
        if !is_token(token) {
            return Err(BridgeError::new(
                CAPTURE_ERROR,
                "Renderer document token is malformed",
            ));
        }
        let mut slots = self.lock();
        let slot = slots.entry(label.to_owned()).or_default();
        if slot.token.is_some() {
            return Err(BridgeError::new(
                CAPTURE_ERROR,
                "Renderer document already registered for this navigation",
            ));
        }
        slot.token = Some(token.to_owned());
        Ok(())
    }

    /// `capture` in RendererDocumentOwner.ts: the privileged main-window document, current and
    /// not loading.
    pub fn capture(&self, caller: &Caller, context: &str) -> Result<DocumentOwner, BridgeError> {
        self.capture_with(caller, context, true)
    }

    /// `requireExecutingCurrentMainFrameForRead`: as `capture`, but a loading page is accepted.
    pub fn capture_for_read(
        &self,
        caller: &Caller,
        context: &str,
    ) -> Result<DocumentOwner, BridgeError> {
        self.capture_with(caller, context, false)
    }

    fn capture_with(
        &self,
        caller: &Caller,
        context: &str,
        require_loaded: bool,
    ) -> Result<DocumentOwner, BridgeError> {
        let refused = || {
            BridgeError::new(
                CAPTURE_ERROR,
                format!("{context} requires the current privileged main-frame renderer document"),
            )
        };
        if caller.label != MAIN_WINDOW_LABEL || !is_renderer_document_url(&caller.url) {
            return Err(refused());
        }
        let slots = self.lock();
        let slot = slots.get(&caller.label).ok_or_else(refused)?;
        if slot.token.as_deref() != Some(caller.token.as_str()) || (require_loaded && slot.loading)
        {
            return Err(refused());
        }
        Ok(DocumentOwner {
            label: caller.label.clone(),
            token: caller.token.clone(),
            generation: slot.generation,
        })
    }

    /// Whether `token` is the one the webview's current document registered. Loading documents
    /// pass: the renderer reaches the instance API while it boots.
    pub fn is_document_token(&self, label: &str, token: &str) -> bool {
        let slots = self.lock();
        slots
            .get(label)
            .and_then(|slot| slot.token.as_deref())
            .is_some_and(|current| constant_time_eq(current.as_bytes(), token.as_bytes()))
    }

    /// `webContents.isLoading() || !getURL()`: no page has finished loading since the last
    /// navigation started, or none has started.
    pub fn is_loading(&self, label: &str) -> bool {
        self.lock().get(label).is_none_or(|slot| slot.loading)
    }

    pub fn is_current(&self, owner: &DocumentOwner) -> bool {
        let slots = self.lock();
        slots.get(&owner.label).is_some_and(|slot| {
            slot.generation == owner.generation
                && !slot.loading
                && slot.token.as_deref() == Some(owner.token.as_str())
        })
    }

    pub fn require_current(&self, owner: &DocumentOwner, context: &str) -> Result<(), BridgeError> {
        if self.is_current(owner) {
            return Ok(());
        }
        Err(BridgeError::new(
            INACTIVE_ERROR,
            format!("{context} owner document is no longer active"),
        ))
    }

    /// `matchesEvent`: the caller is the same document as `owner`, and still current.
    pub fn matches(&self, owner: &DocumentOwner, caller: &Caller) -> bool {
        owner.label == caller.label && owner.token == caller.token && self.is_current(owner)
    }

    /// Runs `watcher` once when the owner's document goes away. Refused if it already has.
    pub fn watch(
        self: &Arc<Self>,
        owner: &DocumentOwner,
        context: &str,
        watcher: impl FnOnce(InvalidationReason) + Send + 'static,
    ) -> Result<WatchGuard, BridgeError> {
        let mut slots = self.lock();
        let slot = slots
            .get_mut(&owner.label)
            .filter(|slot| {
                slot.generation == owner.generation
                    && !slot.loading
                    && slot.token.as_deref() == Some(owner.token.as_str())
            })
            .ok_or_else(|| {
                BridgeError::new(
                    INACTIVE_ERROR,
                    format!("{context} owner document is no longer active"),
                )
            })?;
        let id = slot.next_watcher;
        slot.next_watcher += 1;
        slot.watchers.push((id, Box::new(watcher)));
        Ok(WatchGuard {
            registry: Arc::clone(self),
            label: owner.label.clone(),
            generation: owner.generation,
            id,
        })
    }

    pub fn navigation_started(&self, label: &str) {
        self.invalidate(label, InvalidationReason::Navigated, true);
    }

    pub fn page_loaded(&self, label: &str) {
        if let Some(slot) = self.lock().get_mut(label) {
            slot.loading = false;
        }
    }

    pub fn destroyed(&self, label: &str) {
        self.invalidate(label, InvalidationReason::Destroyed, false);
    }

    fn invalidate(&self, label: &str, reason: InvalidationReason, keep_slot: bool) {
        let watchers = {
            let mut slots = self.lock();
            if keep_slot {
                let slot = slots.entry(label.to_owned()).or_default();
                slot.token = None;
                slot.generation += 1;
                slot.loading = true;
                std::mem::take(&mut slot.watchers)
            } else {
                slots
                    .remove(label)
                    .map(|slot| slot.watchers)
                    .unwrap_or_default()
            }
        };
        // Watchers re-enter the registry (they release ownership), so they run unlocked.
        for (_, watcher) in watchers {
            watcher(reason);
        }
    }

    fn remove_watcher(&self, label: &str, generation: u64, id: u64) {
        if let Some(slot) = self.lock().get_mut(label)
            && slot.generation == generation
        {
            slot.watchers.retain(|(candidate, _)| *candidate != id);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, DocumentSlot>> {
        self.slots.lock().expect("renderer document registry lock")
    }
}

#[tauri::command]
pub fn register_document(
    webview: tauri::Webview,
    documents: tauri::State<'_, Arc<DocumentRegistry>>,
    token: String,
) -> Result<(), BridgeError> {
    documents.register(webview.label(), &token)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

fn is_token(token: &str) -> bool {
    token.len() == TOKEN_HEX_LENGTH && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_app::url::origin;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const TOKEN_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TOKEN_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn document_url() -> String {
        format!("{}/channels/@me", origin())
    }

    fn caller(url: &str, token: &str) -> Caller {
        Caller::for_test(MAIN_WINDOW_LABEL, url, token)
    }

    fn loaded_registry(token: &str) -> Arc<DocumentRegistry> {
        let registry = Arc::new(DocumentRegistry::default());
        registry.navigation_started(MAIN_WINDOW_LABEL);
        registry.register(MAIN_WINDOW_LABEL, token).unwrap();
        registry.page_loaded(MAIN_WINDOW_LABEL);
        registry
    }

    #[test]
    fn captures_only_the_current_main_document() {
        let url = document_url();
        let registry = loaded_registry(TOKEN_A);
        let owner = registry.capture(&caller(&url, TOKEN_A), "Test").unwrap();
        assert!(registry.is_current(&owner));

        let refused = registry
            .capture(&caller(&url, TOKEN_B), "Test")
            .unwrap_err();
        assert_eq!(refused.name(), CAPTURE_ERROR);
        let popout = Caller::for_test("popout", &url, TOKEN_A);
        assert!(registry.capture(&popout, "Test").is_err());
        let api_url = format!("{}/api/x", origin());
        assert!(
            registry
                .capture(&caller(&api_url, TOKEN_A), "Test")
                .is_err()
        );
        assert!(
            registry
                .capture(&caller("https://evil.test/", TOKEN_A), "Test")
                .is_err()
        );
    }

    #[test]
    fn loading_documents_are_readable_but_not_capturable() {
        let url = document_url();
        let registry = Arc::new(DocumentRegistry::default());
        registry.navigation_started(MAIN_WINDOW_LABEL);
        registry.register(MAIN_WINDOW_LABEL, TOKEN_A).unwrap();
        assert!(registry.capture(&caller(&url, TOKEN_A), "Test").is_err());
        assert!(
            registry
                .capture_for_read(&caller(&url, TOKEN_A), "Test")
                .is_ok()
        );
    }

    #[test]
    fn one_registration_per_navigation() {
        let registry = loaded_registry(TOKEN_A);
        assert!(registry.register(MAIN_WINDOW_LABEL, TOKEN_B).is_err());
        registry.navigation_started(MAIN_WINDOW_LABEL);
        registry.register(MAIN_WINDOW_LABEL, TOKEN_B).unwrap();
        assert!(registry.register(MAIN_WINDOW_LABEL, "zz").is_err());
    }

    #[test]
    fn navigation_invalidates_owner_and_fires_watchers_once() {
        let url = document_url();
        let registry = loaded_registry(TOKEN_A);
        let owner = registry.capture(&caller(&url, TOKEN_A), "Test").unwrap();
        let fired = Arc::new(AtomicUsize::new(0));
        let counter = fired.clone();
        let _guard = registry
            .watch(&owner, "Test", move |reason| {
                assert_eq!(reason, InvalidationReason::Navigated);
                counter.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();

        registry.navigation_started(MAIN_WINDOW_LABEL);
        registry.navigation_started(MAIN_WINDOW_LABEL);
        assert_eq!(fired.load(Ordering::SeqCst), 1);
        assert!(!registry.is_current(&owner));
        assert!(!registry.matches(&owner, &caller(&url, TOKEN_A)));
        assert_eq!(
            registry.require_current(&owner, "Test").unwrap_err().name(),
            INACTIVE_ERROR
        );
        assert!(registry.watch(&owner, "Test", |_| {}).is_err());
    }

    #[test]
    fn request_tokens_must_belong_to_the_current_document() {
        let registry = DocumentRegistry::default();
        registry.navigation_started(MAIN_WINDOW_LABEL);
        registry.register(MAIN_WINDOW_LABEL, TOKEN_A).unwrap();
        assert!(registry.is_document_token(MAIN_WINDOW_LABEL, TOKEN_A));
        assert!(!registry.is_document_token(MAIN_WINDOW_LABEL, TOKEN_B));
        assert!(!registry.is_document_token("popout", TOKEN_A));
        registry.navigation_started(MAIN_WINDOW_LABEL);
        assert!(!registry.is_document_token(MAIN_WINDOW_LABEL, TOKEN_A));
    }

    #[test]
    fn dropped_guards_do_not_fire() {
        let url = document_url();
        let registry = loaded_registry(TOKEN_A);
        let owner = registry.capture(&caller(&url, TOKEN_A), "Test").unwrap();
        let fired = Arc::new(AtomicUsize::new(0));
        let counter = fired.clone();
        drop(
            registry
                .watch(&owner, "Test", move |_| {
                    counter.fetch_add(1, Ordering::SeqCst);
                })
                .unwrap(),
        );
        registry.destroyed(MAIN_WINDOW_LABEL);
        assert_eq!(fired.load(Ordering::SeqCst), 0);
    }
}
