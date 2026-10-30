// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod csp;
pub mod files;
pub mod index_html;
pub mod mime;
pub mod proxy;
pub mod static_metadata;
pub mod url;

use std::path::PathBuf;
use std::sync::Arc;
use tauri::http::{Request, Response};

use self::files::{IndexContext, LocalAppFiles};
use self::proxy::Proxy;
use self::proxy::upload_progress::{EmitUploadProgress, UploadProgressHub};
use crate::config::DesktopConfig;
use crate::runtime::RuntimeConfig;

pub use self::url::SCHEME;

/// Serves the `fluxer-app` scheme: renderer files and the `/api` and `/proxy` routes.
pub struct LocalApp {
    files: LocalAppFiles,
    config: Arc<DesktopConfig>,
    runtime: Arc<RuntimeConfig>,
    proxy: Proxy,
}

impl LocalApp {
    /// `emit_upload_progress` delivers an upload progress event to one subscribed webview.
    pub fn new(
        renderer_root: PathBuf,
        config: Arc<DesktopConfig>,
        runtime: Arc<RuntimeConfig>,
        emit_upload_progress: EmitUploadProgress,
    ) -> Self {
        let uploads = Arc::new(UploadProgressHub::new(emit_upload_progress));
        Self {
            files: LocalAppFiles::new(renderer_root),
            config,
            proxy: Proxy::new(runtime.clone(), uploads),
            runtime,
        }
    }

    pub fn upload_progress(&self) -> &UploadProgressHub {
        self.proxy.uploads()
    }

    /// The popout webviews the `/api` and `/proxy` routes admit besides the main window.
    pub fn authorized_webviews(&self) -> &proxy::AuthorizedWebviews {
        self.proxy.webviews()
    }

    /// The renderer files, for notification icons on the local app origin.
    #[cfg(not(target_os = "macos"))]
    pub fn files(&self) -> &LocalAppFiles {
        &self.files
    }

    /// `webview_label` is the webview that issued the request.
    pub async fn handle(
        &self,
        webview_label: &str,
        request: Request<Vec<u8>>,
    ) -> Response<Vec<u8>> {
        if let Some(route) = proxy::route(&request) {
            return self.proxy.handle(webview_label, request, route).await;
        }
        if let Some(response) = crate::event_sink::page_response(webview_label, &request) {
            return response;
        }
        files::handle_file_request(&self.files, &self.index_context(), &request).await
    }

    fn index_context(&self) -> IndexContext {
        IndexContext {
            preboot_theme: self.config.preboot_theme(),
            product_name: self.config.launch_product_name(),
            cleartext_instance_origins: self.runtime.outbound().registered_cleartext_origins(),
        }
    }
}
