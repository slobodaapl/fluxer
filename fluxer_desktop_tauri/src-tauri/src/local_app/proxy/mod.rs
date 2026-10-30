// SPDX-License-Identifier: AGPL-3.0-or-later

//! The `/api` and `/proxy` routes of the local app scheme, ported from the proxy half of
//! LocalAppRequestHandler.ts. Requests the routes do not match fall through to the file handler.

mod admission;
mod api;
mod budget;
mod client;
mod decode;
mod policy;
mod resource;
pub mod upload_progress;

use std::sync::Arc;

use tauri::http::{HeaderMap, Request, Response, StatusCode, header};
use url::Url;

use self::budget::{BufferBudget, RESPONSE_BUFFER_BUDGET, Reservation};
use self::client::{ProxyError, ProxyFetch, ProxyResponse};
use self::policy::{RuntimeRoute, parse_runtime_route};
use self::upload_progress::UploadProgressHub;
use super::url::{API_PATH_PREFIX, REMOTE_PROXY_PATH_PREFIX, parse_local_app_url};
use crate::error::BridgeError;
use crate::runtime::RuntimeConfig;
use crate::runtime::plans::LocalAppRuntimePlan;

pub use self::admission::AuthorizedWebviews;
pub use self::budget::HeldBuffer;

const TEXT_CONTENT_TYPE: &str = "text/plain; charset=utf-8";
const NOT_FOUND_TEXT: &str = "Not found";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteKind {
    Api,
    RemoteResource,
}

pub struct ProxyRoute {
    pub kind: RouteKind,
    url: Url,
    runtime: RuntimeRoute,
}

/// `matchesLocalAppRoute` for both prefixes: a local app URL whose path names an instance key.
pub fn route(request: &Request<Vec<u8>>) -> Option<ProxyRoute> {
    let url = parse_local_app_url(&request.uri().to_string())?;
    let (kind, runtime) = match parse_runtime_route(url.path(), API_PATH_PREFIX) {
        Some(runtime) => (RouteKind::Api, runtime),
        None => (
            RouteKind::RemoteResource,
            parse_runtime_route(url.path(), REMOTE_PROXY_PATH_PREFIX)?,
        ),
    };
    Some(ProxyRoute { kind, url, runtime })
}

pub struct Proxy {
    runtime: Arc<RuntimeConfig>,
    uploads: Arc<UploadProgressHub>,
    budget: BufferBudget,
    webviews: AuthorizedWebviews,
}

impl Proxy {
    pub fn new(runtime: Arc<RuntimeConfig>, uploads: Arc<UploadProgressHub>) -> Self {
        Self::with_budget(runtime, uploads, BufferBudget::new(RESPONSE_BUFFER_BUDGET))
    }

    fn with_budget(
        runtime: Arc<RuntimeConfig>,
        uploads: Arc<UploadProgressHub>,
        budget: BufferBudget,
    ) -> Self {
        Self {
            runtime,
            uploads,
            budget,
            webviews: AuthorizedWebviews::default(),
        }
    }

    pub fn uploads(&self) -> &UploadProgressHub {
        &self.uploads
    }

    pub fn webviews(&self) -> &AuthorizedWebviews {
        &self.webviews
    }

    pub async fn handle(
        &self,
        webview_label: &str,
        request: Request<Vec<u8>>,
        route: ProxyRoute,
    ) -> Response<Vec<u8>> {
        let summary = RequestSummary::of(webview_label, &request);
        let authorization_required = admission::requires_authorization(
            matches!(route.kind, RouteKind::RemoteResource),
            request.method(),
        );
        if let Err(rejection) = admission::admit(
            webview_label,
            &self.webviews,
            request.headers(),
            authorization_required,
            self.runtime.documents(),
        ) {
            tracing::warn!(?rejection, request = ?summary, "Rejected a local app proxy request");
            return not_found_response(NOT_FOUND_TEXT);
        }
        tracing::debug!(request = ?summary, "Admitted a local app proxy request");
        match route.kind {
            RouteKind::Api => api::handle(self, request, route).await,
            RouteKind::RemoteResource => resource::handle(self, request, route).await,
        }
    }

    async fn plan_for_route(
        &self,
        runtime_key: &str,
    ) -> Result<Option<Arc<LocalAppRuntimePlan>>, BridgeError> {
        self.runtime.plans().plan_for_route(runtime_key).await
    }

    /// Cancelled by plan shutdown, like `AbortSignal.any([request.signal, shutdownSignal])`.
    async fn fetch(&self, fetch: ProxyFetch) -> Result<ProxyResponse, ProxyError> {
        let cancel = self.runtime.plans().shutdown_child_token();
        client::fetch(
            &self.runtime.outbound(),
            &self.uploads,
            &self.budget,
            cancel,
            fetch,
        )
        .await
    }
}

/// What the logs record of a proxy request: never the query, which can carry relay tokens.
#[derive(Debug)]
#[expect(dead_code, reason = "read through the Debug output only")]
struct RequestSummary<'a> {
    webview: &'a str,
    method: &'a str,
    path: &'a str,
    origin: Option<&'a str>,
    referer: Option<&'a str>,
    sec_fetch_site: Option<&'a str>,
    sec_fetch_mode: Option<&'a str>,
    sec_fetch_dest: Option<&'a str>,
}

impl<'a> RequestSummary<'a> {
    fn of(webview: &'a str, request: &'a Request<Vec<u8>>) -> Self {
        let headers = request.headers();
        let value = |name: &str| header_text(headers, name);
        Self {
            webview,
            method: request.method().as_str(),
            path: request.uri().path(),
            origin: value(header::ORIGIN.as_str()),
            referer: value(header::REFERER.as_str()),
            sec_fetch_site: value("sec-fetch-site"),
            sec_fetch_mode: value("sec-fetch-mode"),
            sec_fetch_dest: value("sec-fetch-dest"),
        }
    }
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .map(|value| value.to_str().unwrap_or("<non-ascii>"))
}

/// `LOCAL_APP_PROXY_TEXT_RESPONSE_HEADERS`.
fn text_response(status: StatusCode, text: &str) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, TEXT_CONTENT_TYPE)
        .body(text.as_bytes().to_vec())
        .expect("static response parts are valid")
}

/// `localAppProxyNotFoundResponse`.
fn not_found_response(text: &str) -> Response<Vec<u8>> {
    text_response(StatusCode::NOT_FOUND, text)
}

/// `LOCAL_APP_PROXY_FAILURE_HEADERS`.
fn failure_response(status: StatusCode, text: &str) -> Response<Vec<u8>> {
    let mut response = text_response(status, text);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        tauri::http::HeaderValue::from_static("no-store"),
    );
    response
}

/// The body's budget rides along until [`take_held_buffer`] takes it out.
fn proxy_response(
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
    held: Reservation,
) -> Response<Vec<u8>> {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response.extensions_mut().insert(HeldBuffer::new(held));
    response
}

/// The response buffer budget a proxied body holds. The scheme handler drops it once the
/// webview has the body.
pub fn take_held_buffer(response: &mut Response<Vec<u8>>) -> Option<HeldBuffer> {
    response.extensions_mut().remove::<HeldBuffer>()
}

#[cfg(test)]
mod tests;
