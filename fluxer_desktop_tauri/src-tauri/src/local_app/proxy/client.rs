// SPDX-License-Identifier: AGPL-3.0-or-later

//! The upstream exchange of a proxied request, `DesktopLocalAppProxyClient` in
//! LocalAppProxyClient.ts.
//!
//! Differs from Electron: Tauri's responder takes a complete body, so the upstream body is read
//! to the end and decoded before the renderer sees any of it, capped at the route's
//! `response_body_limit` both as received and as decoded, and held against the shared
//! [`BufferBudget`] while buffered.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use tauri::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::budget::{BudgetError, BudgetWait, BufferBudget, Reservation};
use super::decode::{ContentEncoding, DecodeError, content_encoding, decode};
use super::policy::request_has_body;
use super::upload_progress::UploadProgressHub;
use crate::net::outbound::{
    OriginRequest, OriginResponse, OriginTrust, OutboundError, OutboundHttp, RequestBody,
};

const DEADLINE: Duration = Duration::from_secs(5 * 60);
const REQUEST_BODY_LIMIT: u64 = 100 * 1024 * 1024;
/// `/api` answers are JSON.
pub const API_RESPONSE_BODY_LIMIT: usize = 32 * 1024 * 1024;
pub const RESOURCE_RESPONSE_BODY_LIMIT: usize = super::budget::RESPONSE_BUFFER_BUDGET;
pub const API_ACCEPT_ENCODING: &str = "gzip, br";
/// How long a remote resource GET or HEAD may wait for response headers.
pub const RESOURCE_RESPONSE_HEADERS_DEADLINE: Duration = Duration::from_secs(30);
const NULL_BODY_STATUSES: [u16; 3] = [204, 205, 304];

pub struct ProxyFetch {
    pub target: String,
    pub method: Method,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    pub accept_encoding: Option<&'static str>,
    pub upload_id: Option<String>,
    pub upload_total: Option<u64>,
    /// Applies to requests without a body only, like the TS client.
    pub response_headers_deadline: Option<Duration>,
    /// Caps the body both as received and as decoded.
    pub response_body_limit: usize,
}

pub struct ProxyResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    /// The budget `body` holds until it reaches the webview.
    pub held: Reservation,
}

#[derive(Debug)]
pub enum ProxyError {
    Outbound(OutboundError),
    Status(u16),
    Decode(DecodeError),
    BodyTooLarge(usize),
    Budget(BudgetError),
    DecoderTask(String),
}

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Outbound(error) => write!(f, "{error}"),
            Self::Status(status) => write!(
                f,
                "Local app proxy upstream answered with an unusable HTTP status: {status}"
            ),
            Self::Decode(DecodeError::Unsupported(encoding)) => write!(
                f,
                "Local app proxy upstream answered with an unsupported content encoding: {encoding}"
            ),
            Self::Decode(DecodeError::Invalid(error)) => {
                write!(
                    f,
                    "Local app proxy upstream body could not be decoded: {error}"
                )
            }
            Self::BodyTooLarge(limit) => {
                write!(f, "Local app proxy upstream body exceeds {limit} bytes")
            }
            Self::Decode(DecodeError::TooLarge) => write!(
                f,
                "Local app proxy upstream body exceeds its size limit once decoded"
            ),
            Self::Budget(error) | Self::Decode(DecodeError::Budget(error)) => {
                write!(f, "Local app proxy {error}")
            }
            Self::DecoderTask(error) => write!(f, "Local app proxy decoder failed: {error}"),
        }
    }
}

pub async fn fetch(
    outbound: &OutboundHttp,
    uploads: &Arc<UploadProgressHub>,
    budget: &BufferBudget,
    cancel: CancellationToken,
    fetch: ProxyFetch,
) -> Result<ProxyResponse, ProxyError> {
    let wait = BudgetWait {
        deadline: Instant::now() + DEADLINE,
        cancel: cancel.clone(),
    };
    let target = Url::parse(&fetch.target)
        .map_err(|_| ProxyError::Outbound(OutboundError::InvalidTarget))?;
    let has_body = request_has_body(&fetch.method);
    let body = if !has_body || fetch.body.is_empty() {
        RequestBody::Empty
    } else if let Some(upload_id) = fetch.upload_id {
        uploads.instrumented_body(upload_id, fetch.upload_total, Bytes::from(fetch.body))
    } else {
        RequestBody::Bytes(Bytes::from(fetch.body))
    };
    let mut headers = fetch.headers;
    if let Some(accept_encoding) = fetch.accept_encoding {
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static(accept_encoding),
        );
    }
    let method = fetch.method;
    let pending = outbound.request(OriginRequest {
        expected_origin: target.origin().ascii_serialization(),
        url: target.into(),
        method: method.clone(),
        headers,
        body,
        body_limit: REQUEST_BODY_LIMIT,
        timeout: DEADLINE,
        origin_trust: OriginTrust::Registered,
        cancel: cancel.clone(),
    });
    let response = match fetch.response_headers_deadline.filter(|_| !has_body) {
        None => pending.await,
        Some(deadline) => {
            tokio::pin!(pending);
            tokio::select! {
                response = &mut pending => response,
                () = tokio::time::sleep(deadline) => {
                    cancel.cancel();
                    pending.await
                }
            }
        }
    }
    .map_err(ProxyError::Outbound)?;
    read_response(response, &method, fetch.response_body_limit, budget, wait).await
}

/// `buildProxyResponse`, reading and decoding the whole body. The raw body's budget returns once
/// the decoded body holds its own.
async fn read_response(
    response: OriginResponse,
    method: &Method,
    limit: usize,
    budget: &BufferBudget,
    wait: BudgetWait,
) -> Result<ProxyResponse, ProxyError> {
    let status = response.status;
    if !(200..=599).contains(&status.as_u16()) {
        return Err(ProxyError::Status(status.as_u16()));
    }
    if method == Method::HEAD || NULL_BODY_STATUSES.contains(&status.as_u16()) {
        return Ok(ProxyResponse {
            status,
            headers: response.headers,
            body: Vec::new(),
            held: Reservation::default(),
        });
    }
    let encoding = content_encoding(&response.headers).map_err(ProxyError::Decode)?;
    let length = content_length(&response.headers);
    let (raw, held) = read_body(response.body, length, limit, budget, &wait).await?;
    let (body, held) = match encoding {
        None => (raw, held),
        Some(encoding) => decode_held(encoding, raw, held, limit, budget.clone()).await?,
    };
    Ok(ProxyResponse {
        status,
        headers: response.headers,
        body,
        held,
    })
}

/// The upstream `Content-Length`; hyper has already refused a body that disagrees with it.
fn content_length(headers: &HeaderMap) -> Option<usize> {
    headers
        .get(header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

/// A body of known `length` reserves all of it before the first chunk, so it may wait for the
/// budget; a body of unknown length grows chunk by chunk and fails if the budget runs out.
async fn read_body(
    mut body: impl Stream<Item = Result<Bytes, OutboundError>> + Unpin,
    length: Option<usize>,
    limit: usize,
    budget: &BufferBudget,
    wait: &BudgetWait,
) -> Result<(Vec<u8>, Reservation), ProxyError> {
    let mut held = Reservation::default();
    if let Some(length) = length {
        if length > limit {
            return Err(ProxyError::BodyTooLarge(limit));
        }
        held.grow(budget, length, wait)
            .await
            .map_err(ProxyError::Budget)?;
    }
    let mut buffered = Vec::with_capacity(length.unwrap_or(0));
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(ProxyError::Outbound)?;
        let buffered_len = buffered.len() + chunk.len();
        if buffered_len > limit {
            return Err(ProxyError::BodyTooLarge(limit));
        }
        held.grow(budget, buffered_len.saturating_sub(held.bytes()), wait)
            .await
            .map_err(ProxyError::Budget)?;
        buffered.extend_from_slice(&chunk);
    }
    Ok((buffered, held))
}

/// Decodes off the async workers. Decoded chunks join the raw body's reservation as they are
/// produced, without waiting, so the pair has to fit the budget together; the raw share returns
/// afterwards.
async fn decode_held(
    encoding: ContentEncoding,
    raw: Vec<u8>,
    mut held: Reservation,
    limit: usize,
    budget: BufferBudget,
) -> Result<(Vec<u8>, Reservation), ProxyError> {
    tokio::task::spawn_blocking(move || {
        let decoded = decode(encoding, &raw, limit, |bytes| held.try_grow(&budget, bytes));
        held.release(raw.len());
        decoded.map(|body| (body, held))
    })
    .await
    .map_err(|error| ProxyError::DecoderTask(error.to_string()))?
    .map_err(ProxyError::Decode)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use futures_util::stream;

    use super::*;

    fn chunks(sizes: &[usize]) -> impl Stream<Item = Result<Bytes, OutboundError>> + Unpin {
        stream::iter(
            sizes
                .iter()
                .map(|&size| Ok(Bytes::from(vec![1u8; size])))
                .collect::<Vec<_>>(),
        )
    }

    fn wait() -> BudgetWait {
        BudgetWait {
            deadline: Instant::now() + Duration::from_secs(5),
            cancel: CancellationToken::new(),
        }
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[tokio::test]
    async fn bodies_are_buffered_up_to_the_limit_and_hold_their_bytes() {
        let budget = BufferBudget::new(1024);
        for length in [None, Some(7)] {
            let (body, held) = read_body(chunks(&[3, 4]), length, 7, &budget, &wait())
                .await
                .unwrap();
            assert_eq!(body.len(), 7);
            assert_eq!(held.bytes(), 7);
            assert_eq!(budget.available(), 1024 - 7);
            drop(held);
            assert_eq!(budget.available(), 1024);
        }
        assert!(matches!(
            read_body(chunks(&[3, 4, 1]), None, 7, &budget, &wait()).await,
            Err(ProxyError::BodyTooLarge(7))
        ));
        assert!(matches!(
            read_body(chunks(&[]), Some(8), 7, &budget, &wait()).await,
            Err(ProxyError::BodyTooLarge(7))
        ));
        assert_eq!(budget.available(), 1024);
    }

    #[tokio::test]
    async fn body_errors_fail_the_read_and_return_the_budget() {
        let budget = BufferBudget::new(1024);
        for length in [None, Some(100)] {
            let failing = stream::iter(vec![
                Ok(Bytes::from_static(b"ab")),
                Err(OutboundError::Timeout),
            ]);
            assert!(matches!(
                read_body(failing, length, 1024, &budget, &wait()).await,
                Err(ProxyError::Outbound(OutboundError::Timeout))
            ));
            assert_eq!(budget.available(), 1024);
        }
    }

    #[tokio::test]
    async fn bodies_larger_than_the_whole_budget_fail() {
        let budget = BufferBudget::new(8);
        assert!(matches!(
            read_body(chunks(&[4, 4, 4]), None, 1024, &budget, &wait()).await,
            Err(ProxyError::Budget(BudgetError::NeverFits))
        ));
        assert!(matches!(
            read_body(chunks(&[]), Some(9), 1024, &budget, &wait()).await,
            Err(ProxyError::Budget(BudgetError::NeverFits))
        ));
        assert_eq!(budget.available(), 8);
    }

    #[tokio::test]
    async fn known_lengths_are_reserved_before_the_first_chunk() {
        let budget = BufferBudget::new(8);
        let mut other = Reservation::default();
        other.grow(&budget, 4, &wait()).await.unwrap();
        let read = {
            let budget = budget.clone();
            tokio::spawn(async move {
                read_body(chunks(&[4, 4]), Some(8), 1024, &budget, &wait()).await
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!read.is_finished(), "the whole length waits for budget");
        drop(other);
        let (body, held) = read.await.unwrap().unwrap();
        assert_eq!(body.len(), 8);
        assert_eq!(held.bytes(), 8);
    }

    #[tokio::test]
    async fn unknown_lengths_fail_when_the_budget_runs_out_mid_body() {
        let budget = BufferBudget::new(8);
        let mut other = Reservation::default();
        other.grow(&budget, 4, &wait()).await.unwrap();
        let started = Instant::now();
        assert!(matches!(
            read_body(chunks(&[4, 4]), None, 1024, &budget, &wait()).await,
            Err(ProxyError::Budget(BudgetError::Exhausted))
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(budget.available(), 4);
    }

    #[tokio::test]
    async fn cancelling_a_read_that_waits_for_budget_returns_the_budget() {
        let budget = BufferBudget::new(8);
        let mut other = Reservation::default();
        other.grow(&budget, 8, &wait()).await.unwrap();
        let wait = wait();
        let cancel = wait.cancel.clone();
        let read = {
            let budget = budget.clone();
            tokio::spawn(
                async move { read_body(chunks(&[4, 4]), None, 1024, &budget, &wait).await },
            )
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!read.is_finished());
        cancel.cancel();
        assert!(matches!(
            read.await.unwrap(),
            Err(ProxyError::Budget(BudgetError::Aborted))
        ));
        drop(other);
        assert_eq!(budget.available(), 8);
    }

    #[tokio::test]
    async fn decoding_trades_the_raw_budget_for_the_decoded_one() {
        let budget = BufferBudget::new(1024 * 1024);
        let text = vec![b'a'; 200 * 1024];
        let raw = gzip(&text);
        let mut held = Reservation::default();
        held.grow(&budget, raw.len(), &wait()).await.unwrap();
        let (body, held) = decode_held(
            ContentEncoding::Gzip,
            raw,
            held,
            1024 * 1024,
            budget.clone(),
        )
        .await
        .unwrap();
        assert_eq!(body, text);
        assert_eq!(held.bytes(), text.len());
        assert_eq!(budget.available(), 1024 * 1024 - text.len());
    }

    #[tokio::test]
    async fn raw_and_decoded_bodies_have_to_fit_the_budget_together() {
        let text = vec![b'a'; 200 * 1024];
        let raw = gzip(&text);
        let budget = BufferBudget::new(text.len() + raw.len() - 1);
        let mut held = Reservation::default();
        held.grow(&budget, raw.len(), &wait()).await.unwrap();
        let decoded = decode_held(
            ContentEncoding::Gzip,
            raw,
            held,
            1024 * 1024,
            budget.clone(),
        )
        .await;
        assert!(matches!(
            decoded,
            Err(ProxyError::Decode(DecodeError::Budget(
                BudgetError::NeverFits
            )))
        ));
        assert_eq!(budget.available(), budget.total());
    }

    #[tokio::test]
    async fn decoded_growth_fails_when_the_budget_runs_out() {
        let text = vec![b'a'; 200 * 1024];
        let raw = gzip(&text);
        let budget = BufferBudget::new(1024 * 1024);
        let mut other = Reservation::default();
        other
            .grow(&budget, 1024 * 1024 - raw.len() - 1024, &wait())
            .await
            .unwrap();
        let mut held = Reservation::default();
        held.grow(&budget, raw.len(), &wait()).await.unwrap();
        let decoded = decode_held(
            ContentEncoding::Gzip,
            raw,
            held,
            1024 * 1024,
            budget.clone(),
        )
        .await;
        assert!(matches!(
            decoded,
            Err(ProxyError::Decode(DecodeError::Budget(
                BudgetError::Exhausted
            )))
        ));
        drop(other);
        assert_eq!(budget.available(), budget.total());
    }
}
