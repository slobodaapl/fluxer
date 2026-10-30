// SPDX-License-Identifier: AGPL-3.0-or-later

//! Upload progress of proxied request bodies, the subscriber half of LocalAppProxyClient.ts and
//! the `desktop-local-app-upload:subscribe` handler of DesktopRuntimeConfigIpc.ts.
//!
//! Electron counted bytes as Chromium streamed the body in. Here the body arrives buffered, so
//! progress counts the 64 KiB chunks handed to the upstream connection.

use std::collections::HashSet;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::Stream;
use serde::Serialize;
use tauri::{State, Webview};

use crate::documents::{Caller, DocumentRegistry};
use crate::local_app::LocalApp;
use crate::net::outbound::RequestBody;

/// `LOCAL_APP_UPLOAD_PROGRESS_CHANNELS.progress`.
pub const UPLOAD_PROGRESS_EVENT: &str = "desktop-local-app-upload:progress";
const SUBSCRIPTION_CONTEXT: &str = "Local app upload progress subscription";
const CHUNK_BYTES: usize = 64 * 1024;
const PROGRESS_MIN_BYTES: u64 = 64 * 1024;
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(100);

/// `DesktopLocalAppUploadProgress`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadProgress {
    pub upload_id: String,
    pub loaded: u64,
    pub total: Option<u64>,
    pub done: bool,
    pub failed: bool,
}

/// Delivers one progress event to one webview.
pub type EmitUploadProgress = Box<dyn Fn(&str, &UploadProgress) + Send + Sync>;

/// The webviews subscribed to upload progress, and how events reach them.
pub struct UploadProgressHub {
    subscribers: Mutex<HashSet<String>>,
    emit: EmitUploadProgress,
}

impl UploadProgressHub {
    pub fn new(emit: EmitUploadProgress) -> Self {
        Self {
            subscribers: Mutex::default(),
            emit,
        }
    }

    /// Adds `webview_label` when `caller` is the current privileged renderer document.
    pub fn subscribe(&self, documents: &DocumentRegistry, caller: &Caller, webview_label: &str) {
        if let Err(error) = documents.capture(caller, SUBSCRIPTION_CONTEXT) {
            tracing::warn!(%error, "Rejected a local app upload progress subscription");
            return;
        }
        self.subscribers().insert(webview_label.to_owned());
    }

    /// `clearLocalAppUploadProgressSubscribers`.
    pub fn clear(&self) {
        self.subscribers().clear();
    }

    /// `emitLocalAppUploadFailure`.
    pub fn failure(&self, upload_id: Option<&str>) {
        let Some(upload_id) = upload_id else {
            return;
        };
        self.emit(&UploadProgress {
            upload_id: upload_id.to_owned(),
            loaded: 0,
            total: None,
            done: true,
            failed: true,
        });
    }

    /// `instrumentedRequestBody` over a buffered body.
    pub fn instrumented_body(
        self: &Arc<Self>,
        upload_id: String,
        total: Option<u64>,
        body: Bytes,
    ) -> RequestBody {
        RequestBody::Stream(Box::pin(ProgressStream {
            hub: self.clone(),
            upload_id,
            total,
            remaining: body,
            loaded: 0,
            last_emitted_bytes: 0,
            last_emitted_at: Instant::now(),
            flushed: false,
        }))
    }

    fn emit(&self, progress: &UploadProgress) {
        let labels: Vec<String> = self.subscribers().iter().cloned().collect();
        for label in labels {
            (self.emit)(&label, progress);
        }
    }

    fn subscribers(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

struct ProgressStream {
    hub: Arc<UploadProgressHub>,
    upload_id: String,
    total: Option<u64>,
    remaining: Bytes,
    loaded: u64,
    last_emitted_bytes: u64,
    last_emitted_at: Instant,
    flushed: bool,
}

impl ProgressStream {
    fn progress(&self, done: bool) -> UploadProgress {
        UploadProgress {
            upload_id: self.upload_id.clone(),
            loaded: self.loaded,
            total: self.total,
            done,
            failed: false,
        }
    }
}

impl Stream for ProgressStream {
    type Item = Result<Bytes, io::Error>;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.remaining.is_empty() {
            if !this.flushed {
                this.flushed = true;
                this.hub.emit(&this.progress(true));
            }
            return Poll::Ready(None);
        }
        let chunk = this
            .remaining
            .split_to(CHUNK_BYTES.min(this.remaining.len()));
        this.loaded += chunk.len() as u64;
        let now = Instant::now();
        if this.loaded - this.last_emitted_bytes >= PROGRESS_MIN_BYTES
            || now.duration_since(this.last_emitted_at) >= PROGRESS_MIN_INTERVAL
        {
            this.last_emitted_bytes = this.loaded;
            this.last_emitted_at = now;
            this.hub.emit(&this.progress(false));
        }
        Poll::Ready(Some(Ok(chunk)))
    }
}

/// `desktop-local-app-upload:subscribe`, fire-and-forget like `ipcMain.on`.
#[tauri::command]
pub fn local_app_upload_subscribe(
    webview: Webview,
    local_app: State<'_, Arc<LocalApp>>,
    documents: State<'_, Arc<DocumentRegistry>>,
    document_token: String,
) {
    let caller = Caller::new(&webview, document_token);
    local_app
        .upload_progress()
        .subscribe(&documents, &caller, webview.label());
}

#[cfg(test)]
pub mod test_support {
    use super::*;

    pub type Recorded = Arc<Mutex<Vec<(String, UploadProgress)>>>;

    /// An emitter that records every `(webview label, progress)` it is handed.
    pub fn recorder() -> (EmitUploadProgress, Recorded) {
        let events = Recorded::default();
        let sink = events.clone();
        let emit: EmitUploadProgress = Box::new(move |label, progress| {
            sink.lock()
                .unwrap()
                .push((label.to_owned(), progress.clone()));
        });
        (emit, events)
    }
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;

    use super::test_support::recorder;
    use super::*;
    use crate::runtime::test_support::{TOKEN_A, TOKEN_B, caller, loaded_documents};
    use crate::window::MAIN_WINDOW_LABEL;

    fn progress(loaded: u64, total: Option<u64>, done: bool) -> UploadProgress {
        UploadProgress {
            upload_id: "up-1".to_owned(),
            loaded,
            total,
            done,
            failed: false,
        }
    }

    fn recording_hub() -> (Arc<UploadProgressHub>, super::test_support::Recorded) {
        let (emit, events) = recorder();
        (Arc::new(UploadProgressHub::new(emit)), events)
    }

    #[test]
    fn only_the_current_document_subscribes() {
        let (hub, events) = recording_hub();
        let documents = loaded_documents();
        hub.subscribe(&documents, &caller(TOKEN_B), MAIN_WINDOW_LABEL);
        hub.failure(Some("up-1"));
        assert!(events.lock().unwrap().is_empty());

        hub.subscribe(&documents, &caller(TOKEN_A), MAIN_WINDOW_LABEL);
        hub.subscribe(&documents, &caller(TOKEN_A), MAIN_WINDOW_LABEL);
        hub.failure(Some("up-1"));
        hub.failure(None);
        let failure = UploadProgress {
            upload_id: "up-1".to_owned(),
            loaded: 0,
            total: None,
            done: true,
            failed: true,
        };
        assert_eq!(
            *events.lock().unwrap(),
            vec![(MAIN_WINDOW_LABEL.to_owned(), failure)]
        );

        hub.clear();
        hub.failure(Some("up-1"));
        assert_eq!(events.lock().unwrap().len(), 1);
    }

    #[test]
    fn progress_serialises_as_the_renderer_contract() {
        let value = serde_json::to_value(progress(5, None, false)).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"uploadId": "up-1", "loaded": 5, "total": null, "done": false, "failed": false})
        );
    }

    #[tokio::test]
    async fn the_body_streams_in_chunks_with_progress_then_done() {
        let (hub, events) = recording_hub();
        hub.subscribers().insert(MAIN_WINDOW_LABEL.to_owned());
        let body = Bytes::from(vec![7u8; 2 * CHUNK_BYTES]);
        let RequestBody::Stream(mut stream) =
            hub.instrumented_body("up-1".to_owned(), Some(131_072), body.clone())
        else {
            panic!("instrumented bodies stream");
        };
        let mut sent = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.unwrap();
            assert!(chunk.len() <= CHUNK_BYTES);
            sent.extend_from_slice(&chunk);
        }
        assert!(stream.next().await.is_none());
        assert_eq!(sent, body);
        let emitted: Vec<UploadProgress> = events
            .lock()
            .unwrap()
            .iter()
            .map(|(_, progress)| progress.clone())
            .collect();
        assert_eq!(
            emitted,
            vec![
                progress(65_536, Some(131_072), false),
                progress(131_072, Some(131_072), false),
                progress(131_072, Some(131_072), true),
            ]
        );
    }
}
