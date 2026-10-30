// SPDX-License-Identifier: AGPL-3.0-or-later

//! `downloadFile`: the `download-file` channel of IpcHandlers.ts over
//! fluxer_desktop/src/main/FileDownloads.ts. The user picks the destination in a save dialog,
//! then the file is fetched the way `DesktopOutboundHTTP.get()` fetched it: no cookies,
//! credentials or user agent, an origin no instance registered must use https and resolve to
//! public addresses, at most 32 requests in flight. Up to five redirects are followed, the body
//! is capped at 512 MiB, and an expected SHA-256 is checked.
//!
//! One change from Electron, which removed the destination first and streamed into it: the body
//! goes to `<destination>.part`, renamed over the destination only once it is complete and
//! verified, and removed on any failure, so a failed download leaves an existing file alone.
//!
//! The renderer suggests a file name taken from the URL, which may carry decoded separators; the
//! dialog starts in the Downloads folder with only its last component. Errors of the outbound
//! client read as this shell words them rather than Electron's.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use hyper::{HeaderMap, Method, header};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager, State, Webview};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::atomic_file::rename_with_retry;
use crate::dialog::{self, SaveDialogOptions};
use crate::documents::{Caller, DocumentRegistry};
use crate::error::BridgeError;
use crate::js::js_trim;
use crate::net::origin::http_origin_of;
use crate::net::outbound::{
    DEFAULT_REQUEST_BODY_LIMIT, OriginRequest, OriginTrust, OutboundHttp, RequestBody,
    ResponseBody, parse_redirect_target, parse_target,
};
use crate::runtime::RuntimeConfig;

const MAX_DOWNLOAD_REDIRECTS: usize = 5;
const DOWNLOAD_DEADLINE: Duration = Duration::from_secs(600);
const DOWNLOAD_MAX_BYTES: u64 = 512 * 1024 * 1024;
/// `DESKTOP_OUTBOUND_HTTP_MAX_IN_FLIGHT`.
const MAX_IN_FLIGHT: usize = 32;
const CAPACITY_MESSAGE: &str = "Too many downloads are already in progress";
const PART_SUFFIX: &str = ".part";
const CHANNEL: &str = "download-file";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DownloadError {
    /// `DownloadChecksumError`.
    Checksum(String),
    Failed(String),
}

fn failed(message: impl Into<String>) -> DownloadError {
    DownloadError::Failed(message.into())
}

/// The in-flight count `get()` capped. Each request holds a slot until its body is done.
pub(crate) struct InFlight(AtomicUsize);

/// The process-wide count, as the Electron shell's single outbound client kept it.
static GETS: InFlight = InFlight(AtomicUsize::new(0));

struct InFlightSlot<'a>(&'a InFlight);

impl Drop for InFlightSlot<'_> {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl InFlight {
    #[cfg(test)]
    fn new() -> Self {
        Self(AtomicUsize::new(0))
    }

    fn acquire(&self) -> Result<InFlightSlot<'_>, DownloadError> {
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_IN_FLIGHT).then_some(count + 1)
            })
            .map(|_| InFlightSlot(self))
            .map_err(|_| failed(CAPACITY_MESSAGE))
    }
}

/// `parseExpectedSha256`: absent, null or blank means no check.
fn parse_expected_sha256(value: Option<&Value>) -> Result<Option<String>, DownloadError> {
    let expected = match value {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(value)) => js_trim(value).to_lowercase(),
        Some(_) => return Err(failed("Download checksum is malformed")),
    };
    if expected.is_empty() {
        return Ok(None);
    }
    if expected.len() != 64
        || !expected
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(failed("Download checksum is malformed"));
    }
    Ok(Some(expected))
}

fn part_path(destination: &Path) -> PathBuf {
    let mut part = OsString::from(destination.as_os_str());
    part.push(PART_SUFFIX);
    PathBuf::from(part)
}

/// `writeCappedResponse` into `part`.
async fn write_capped(
    body: ResponseBody,
    part: &Path,
    max_bytes: u64,
    mut hash: Option<&mut Sha256>,
) -> Result<(), DownloadError> {
    let io = |error: std::io::Error| failed(error.to_string());
    let mut file = tokio::fs::File::create(part).await.map_err(io)?;
    let mut body = pin!(body);
    let mut received: u64 = 0;
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|error| failed(error.to_string()))?;
        received += chunk.len() as u64;
        if received > max_bytes {
            return Err(failed(format!("Download exceeds {max_bytes} bytes")));
        }
        if let Some(hash) = hash.as_mut() {
            hash.update(&chunk);
        }
        file.write_all(&chunk).await.map_err(io)?;
    }
    file.flush().await.map_err(io)?;
    file.sync_all().await.map_err(io)
}

/// `downloadFileWithRedirects`: fetches `target` into `part`, following redirects.
async fn fetch_into(
    outbound: &OutboundHttp,
    gets: &InFlight,
    mut target: Url,
    part: &Path,
    max_bytes: u64,
    mut hash: Option<&mut Sha256>,
) -> Result<(), DownloadError> {
    let mut redirects = 0;
    loop {
        let slot = gets.acquire()?;
        let response = outbound
            .request(OriginRequest {
                expected_origin: http_origin_of(&target).expect("an http(s) URL has an origin"),
                url: target.to_string(),
                method: Method::GET,
                headers: HeaderMap::new(),
                body: RequestBody::Empty,
                body_limit: DEFAULT_REQUEST_BODY_LIMIT,
                timeout: DOWNLOAD_DEADLINE,
                origin_trust: OriginTrust::Public,
                cancel: CancellationToken::new(),
            })
            .await
            .map_err(|error| failed(error.to_string()))?;
        let status = response.status.as_u16();
        if (300..400).contains(&status) {
            if redirects >= MAX_DOWNLOAD_REDIRECTS {
                return Err(failed("Too many download redirects"));
            }
            let base = Url::parse(&response.url).expect("the response URL is the request URL");
            target = parse_redirect_target(&base, response.headers.get(header::LOCATION))
                .ok_or_else(|| failed(format!("HTTP {status} redirect target is unusable")))?;
            redirects += 1;
            continue;
        }
        if status == 204 || status == 205 {
            drop(response);
            drop(slot);
            return tokio::fs::write(part, [])
                .await
                .map_err(|error| failed(error.to_string()));
        }
        if !(200..300).contains(&status) {
            return Err(failed(format!("HTTP {status}")));
        }
        let written = write_capped(response.body, part, max_bytes, hash.as_deref_mut()).await;
        drop(slot);
        return written;
    }
}

/// `downloadFile(url, destination, {sha256, maxBytes})`.
pub(crate) async fn download(
    outbound: &OutboundHttp,
    gets: &InFlight,
    url: Option<&str>,
    destination: &Path,
    sha256: Option<&Value>,
    max_bytes: u64,
) -> Result<(), DownloadError> {
    let target = url
        .and_then(parse_target)
        .ok_or_else(|| failed("Download URL must use http or https"))?;
    let expected = parse_expected_sha256(sha256)?;
    let mut hash = expected.is_some().then(Sha256::new);
    let part = part_path(destination);
    let remove_part = || async {
        let _ = tokio::fs::remove_file(&part).await;
    };
    if let Err(error) = fetch_into(outbound, gets, target, &part, max_bytes, hash.as_mut()).await {
        remove_part().await;
        return Err(error);
    }
    if let (Some(expected), Some(hash)) = (expected, hash) {
        let digest = format!("{:x}", hash.finalize());
        if digest != expected {
            remove_part().await;
            return Err(DownloadError::Checksum(format!(
                "Download checksum mismatch: expected {expected}, downloaded {digest}"
            )));
        }
    }
    let (source, target) = (part.clone(), destination.to_owned());
    let published =
        tauri::async_runtime::spawn_blocking(move || rename_with_retry(&source, &target))
            .await
            .expect("renaming a download does not panic");
    if let Err(error) = published {
        remove_part().await;
        return Err(failed(error.to_string()));
    }
    Ok(())
}

/// The last component of the renderer's suggested name, if it has one.
fn suggested_file_name(default_path: &str) -> Option<String> {
    Path::new(default_path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

/// `DownloadFileResult`.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadFileResult {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    canceled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    checksum_mismatch: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[tauri::command]
pub async fn download_file(
    app: AppHandle,
    webview: Webview,
    documents: State<'_, Arc<DocumentRegistry>>,
    document_token: String,
    url: Option<Value>,
    default_path: Option<Value>,
    sha256: Option<Value>,
) -> Result<DownloadFileResult, BridgeError> {
    documents
        .capture_for_read(&Caller::new(&webview, document_token), CHANNEL)
        .map_err(|_| {
            BridgeError::new(
                "UntrustedRendererDocumentSenderError",
                format!("{CHANNEL} is only reachable from a trusted top-level renderer document"),
            )
        })?;
    let file_name = default_path
        .as_ref()
        .and_then(Value::as_str)
        .and_then(suggested_file_name);
    let folder = file_name
        .as_ref()
        .and_then(|_| app.path().download_dir().ok());
    let Some(destination) =
        dialog::show_save_dialog(&webview.window(), SaveDialogOptions { folder, file_name }).await
    else {
        return Ok(DownloadFileResult {
            canceled: Some(true),
            ..DownloadFileResult::default()
        });
    };
    let outbound = app.state::<Arc<RuntimeConfig>>().outbound();
    let downloaded = download(
        &outbound,
        &GETS,
        url.as_ref().and_then(Value::as_str),
        &destination,
        sha256.as_ref(),
        DOWNLOAD_MAX_BYTES,
    )
    .await;
    Ok(match downloaded {
        Ok(()) => DownloadFileResult {
            success: true,
            path: Some(destination.to_string_lossy().into_owned()),
            ..DownloadFileResult::default()
        },
        Err(DownloadError::Checksum(message)) => DownloadFileResult {
            checksum_mismatch: Some(true),
            error: Some(message),
            ..DownloadFileResult::default()
        },
        Err(DownloadError::Failed(message)) => DownloadFileResult {
            error: Some(message),
            ..DownloadFileResult::default()
        },
    })
}

#[cfg(test)]
mod tests;
