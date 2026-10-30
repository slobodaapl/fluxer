// SPDX-License-Identifier: AGPL-3.0-or-later

//! Origin-scoped outbound HTTP with DNS pinning, ported from
//! `fluxer_desktop/src/main/DesktopOutboundHTTP.ts` (`request()` path) and
//! `DesktopHappyEyeballs.ts`.
//!
//! Every origin is resolved once into a binding (its pinned addresses and
//! address scope). Connections only ever use the binding's addresses, so a
//! later DNS answer cannot move an approved origin to another network.
//!
//! Routing is DIRECT only. The Electron shell asked Chromium for a proxy
//! route first; system proxy support is a separate unit and slots in before
//! `ensure_binding` in `request` and `anchor_requirement`.
//!
//! Stale keep-alive handling differs from the TS port: Node retried idempotent,
//! replayable requests once after ECONNRESET/EPIPE on a reused socket.
//! hyper-util's `retry_canceled_requests` instead retries any request that
//! hyper proves was never written to a reused connection that closed, with no
//! retry cap and regardless of method; requests that were partly or fully
//! sent are never replayed.
//!
//! Connection racing also differs: the TS agent interleaved address families
//! and started a new attempt every 250 ms under one 30 s deadline. hyper's
//! `HttpConnector` tries the first address's family in order and starts the
//! other family 250 ms later, splitting the 30 s connect timeout evenly
//! across each family's addresses.

use std::collections::{HashMap, HashSet};
use std::error::Error as StdError;
use std::fmt;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures_util::future::{BoxFuture, FutureExt, Shared};
use futures_util::{Stream, StreamExt};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Empty, Full, StreamBody};
use hyper::body::{Body, Frame, Incoming};
use hyper::{HeaderMap, Method, Request, StatusCode, Uri, header};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::connect::dns::Name;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use tokio::net::TcpStream;
use tokio::time::Sleep;
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};
use url::Url;

use super::address::{is_public_ip, parse_ip_literal};
use super::origin::{http_origin_of, normalize_http_network_origin};

const RESOLUTION_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TARGET_URL_BYTES: usize = 16 * 1024;
const MAX_REDIRECT_LOCATION_BYTES: usize = 4096;
const MAX_ORIGINS: usize = 2048;
const MAX_PENDING_RESOLUTIONS: usize = 64;
const HAPPY_EYEBALLS_ATTEMPT_DELAY: Duration = Duration::from_millis(250);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Request body cap callers should use unless they have a reason not to.
pub const DEFAULT_REQUEST_BODY_LIMIT: u64 = 1024 * 1024;

type BoxError = Box<dyn StdError + Send + Sync>;
type OutboundBody = UnsyncBoxBody<Bytes, BoxError>;
type OutboundClient = Client<HttpsConnector<TrackedConnector>, OutboundBody>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressRequirement {
    Any,
    Public,
}

fn strictest(left: AddressRequirement, right: AddressRequirement) -> AddressRequirement {
    if left == AddressRequirement::Public || right == AddressRequirement::Public {
        AddressRequirement::Public
    } else {
        AddressRequirement::Any
    }
}

/// How an unregistered origin is treated by `request`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OriginTrust {
    /// The origin must already be registered.
    Registered,
    /// The caller vouches for the origin; unregistered origins get `Any`.
    Bound,
    /// `get()`: an unregistered origin must be https and resolve to public addresses only.
    Public,
}

pub enum RequestBody {
    Empty,
    Bytes(Bytes),
    Stream(Pin<Box<dyn Stream<Item = Result<Bytes, io::Error>> + Send>>),
}

pub struct OriginRequest {
    pub expected_origin: String,
    pub url: String,
    pub method: Method,
    pub headers: HeaderMap,
    pub body: RequestBody,
    pub body_limit: u64,
    /// Covers the whole exchange, response body included.
    pub timeout: Duration,
    pub origin_trust: OriginTrust,
    pub cancel: CancellationToken,
}

pub struct OriginResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub url: String,
    pub body: ResponseBody,
}

#[derive(Clone, Debug)]
pub enum OutboundError {
    Closed,
    InvalidOrigin,
    InvalidTarget,
    OriginNotRegistered(String),
    PublicAddressRequired,
    InsecureTransport,
    BodyLimit,
    Timeout,
    Aborted,
    Resolution(String),
    ResolutionTimeout,
    EmptyResolution,
    Capacity,
    /// No pinned address accepted a TCP connection (refused, host or network
    /// unreachable, or the connect deadline passed).
    Connect(String),
    Transport(String),
}

impl OutboundError {
    /// Whether the binding's addresses should be re-resolved on next use.
    pub fn is_unreachable(&self) -> bool {
        matches!(self, Self::Connect(_))
    }

    fn is_resolution_failure(&self) -> bool {
        matches!(self, Self::Resolution(_) | Self::ResolutionTimeout)
    }
}

impl fmt::Display for OutboundError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("outbound HTTP is closed"),
            Self::InvalidOrigin => f.write_str("origin is not a canonical http(s) origin"),
            Self::InvalidTarget => {
                f.write_str("target is not a canonical URL under its declared origin")
            }
            Self::OriginNotRegistered(origin) => {
                write!(
                    f,
                    "origin is not registered to a discovered instance: {origin}"
                )
            }
            Self::PublicAddressRequired => f.write_str(
                "a publicly reachable instance cannot authorize a privately resolving origin",
            ),
            Self::InsecureTransport => f.write_str("a publicly routable origin requires https"),
            Self::BodyLimit => f.write_str("request body exceeds its limit"),
            Self::Timeout => f.write_str("request timed out"),
            Self::Aborted => f.write_str("request was aborted by its caller"),
            Self::Resolution(error) => write!(f, "host resolution failed: {error}"),
            Self::ResolutionTimeout => write!(
                f,
                "host resolution exceeded {} ms",
                RESOLUTION_TIMEOUT.as_millis()
            ),
            Self::EmptyResolution => f.write_str("origin resolved to no usable address"),
            Self::Capacity => f.write_str("outbound HTTP origin capacity exceeded"),
            Self::Connect(error) => write!(f, "could not connect: {error}"),
            Self::Transport(error) => write!(f, "request could not be completed: {error}"),
        }
    }
}

impl StdError for OutboundError {}

pub(crate) type LookupFuture = Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send>>;

/// Host name to addresses, in resolver order. Injectable for tests.
pub(crate) trait HostLookup: Send + Sync + 'static {
    fn lookup(&self, hostname: String) -> LookupFuture;
}

/// getaddrinfo via tokio; keeps the system's address order.
struct SystemLookup;

impl HostLookup for SystemLookup {
    fn lookup(&self, hostname: String) -> LookupFuture {
        Box::pin(async move {
            Ok(tokio::net::lookup_host((hostname.as_str(), 0))
                .await?
                .map(|address| address.ip())
                .collect())
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddressScope {
    NonPublic,
    Public,
}

fn address_scope(address: IpAddr) -> AddressScope {
    if is_public_ip(address) {
        AddressScope::Public
    } else {
        AddressScope::NonPublic
    }
}

#[derive(Clone, Debug)]
struct Binding {
    origin: String,
    /// Never empty; the first entry decides `scope`.
    addresses: Vec<IpAddr>,
    scope: AddressScope,
    unreachable: bool,
}

fn require_scope(binding: &Binding, requirement: AddressRequirement) -> Result<(), OutboundError> {
    if requirement == AddressRequirement::Public && binding.scope != AddressScope::Public {
        return Err(OutboundError::PublicAddressRequired);
    }
    Ok(())
}

/// Dedupes, then keeps only the non-public addresses when any exist, so a
/// mixed answer is treated as private rather than public.
fn select_pinned_addresses(candidates: impl IntoIterator<Item = IpAddr>) -> Vec<IpAddr> {
    let mut unique = Vec::new();
    for candidate in candidates {
        if !unique.contains(&candidate) {
            unique.push(candidate);
        }
    }
    if unique.iter().any(|&address| !is_public_ip(address)) {
        unique.retain(|&address| !is_public_ip(address));
    }
    unique
}

fn absolute_lookup_hostname(hostname: &str) -> String {
    if hostname.contains('.') && !hostname.ends_with('.') {
        format!("{hostname}.")
    } else {
        hostname.to_owned()
    }
}

fn normalize_lookup_hostname(hostname: &str) -> String {
    let lowered = hostname.trim().to_lowercase();
    let mut value = lowered
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(&lowered);
    if value.len() > 1 {
        value = value.strip_suffix('.').unwrap_or(value);
    }
    value.to_owned()
}

fn require_origin(value: &str) -> Result<String, OutboundError> {
    normalize_http_network_origin(value).ok_or(OutboundError::InvalidOrigin)
}

/// `parseDesktopHTTPTarget`: an http(s) URL of at most 16 KiB with a host, no credentials and
/// a port other than 0.
pub fn parse_target(value: &str) -> Option<Url> {
    if value.len() > MAX_TARGET_URL_BYTES {
        return None;
    }
    let url = Url::parse(value).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some_and(|password| !password.is_empty())
        || url.host_str().is_none_or(str::is_empty)
        || url.port() == Some(0)
    {
        return None;
    }
    Some(url)
}

/// `parseDesktopRedirectTarget`: the absolute target of a redirect's `Location`, if it is one
/// an outbound request could be sent to.
pub fn parse_redirect_target(base: &Url, location: Option<&header::HeaderValue>) -> Option<Url> {
    // Node's HTTP parser hands header values over as latin1 strings.
    let location: String = location?.as_bytes().iter().map(|&byte| char::from(byte)).collect();
    if location.is_empty() || location.len() > MAX_REDIRECT_LOCATION_BYTES {
        return None;
    }
    parse_target(base.join(&location).ok()?.as_str())
}

fn host_of(origin: &str) -> Result<String, OutboundError> {
    Url::parse(origin)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .ok_or(OutboundError::InvalidOrigin)
}

type SharedResolution = Shared<BoxFuture<'static, Result<Arc<Binding>, OutboundError>>>;

#[derive(Clone)]
struct PendingBinding {
    id: u64,
    /// Requirement only ever tightens to public while the resolution runs.
    public: Arc<AtomicBool>,
    result: SharedResolution,
}

fn pending_requirement(public: &AtomicBool) -> AddressRequirement {
    if public.load(Ordering::Acquire) {
        AddressRequirement::Public
    } else {
        AddressRequirement::Any
    }
}

#[derive(Default)]
struct State {
    closed: bool,
    bindings: HashMap<String, Arc<Binding>>,
    pending: HashMap<String, PendingBinding>,
    registered: HashMap<String, AddressRequirement>,
}

struct Inner {
    state: Mutex<State>,
    client: Mutex<Option<OutboundClient>>,
    lookup: Arc<dyn HostLookup>,
    shutdown: CancellationToken,
    next_pending_id: AtomicU64,
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn require_admission(&self) -> Result<(), OutboundError> {
        if self.state().closed {
            return Err(OutboundError::Closed);
        }
        Ok(())
    }
}

/// Per-request connect context: the pinned addresses the connector may use
/// and a count of TCP connects this request started that have not finished.
#[derive(Clone)]
struct ConnectContext {
    hostname: String,
    addresses: Arc<[IpAddr]>,
    connecting: Arc<AtomicUsize>,
}

tokio::task_local! {
    static CONNECT_CONTEXT: ConnectContext;
}

/// DNS for the connector: answers only with the request's pinned addresses,
/// and only for the request's own host name.
#[derive(Clone)]
struct PinnedResolver;

impl tower_service::Service<Name> for PinnedResolver {
    type Response = std::vec::IntoIter<SocketAddr>;
    type Error = io::Error;
    type Future = std::future::Ready<io::Result<Self::Response>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, name: Name) -> Self::Future {
        let answer = CONNECT_CONTEXT
            .try_with(|context| {
                if normalize_lookup_hostname(name.as_str())
                    != normalize_lookup_hostname(&context.hostname)
                {
                    return Err(io::Error::other("pinned lookup host name mismatch"));
                }
                Ok(context
                    .addresses
                    .iter()
                    .map(|&address| SocketAddr::new(address, 0))
                    .collect::<Vec<_>>()
                    .into_iter())
            })
            .unwrap_or_else(|_| Err(io::Error::other("lookup outside a pinned request")));
        std::future::ready(answer)
    }
}

struct ConnectingGuard(Arc<AtomicUsize>);

impl ConnectingGuard {
    fn new(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(counter)
    }
}

impl Drop for ConnectingGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// TCP connector that carries the request's connect context into the
/// resolver (also when hyper finishes a connect in a background task) and
/// counts in-progress TCP connects, so a deadline that fires mid-connect can
/// mark the binding unreachable the way the TS `releaseUnconnectedBinding` did.
#[derive(Clone)]
struct TrackedConnector {
    inner: HttpConnector<PinnedResolver>,
}

impl tower_service::Service<Uri> for TrackedConnector {
    type Response = TokioIo<TcpStream>;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, BoxError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), BoxError>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        let Ok(context) = CONNECT_CONTEXT.try_with(ConnectContext::clone) else {
            return Box::pin(std::future::ready(Err(
                "connect outside a pinned request".into()
            )));
        };
        let guard = ConnectingGuard::new(context.connecting.clone());
        let connect = self.inner.call(dst);
        Box::pin(CONNECT_CONTEXT.scope(context, async move {
            let _guard = guard;
            connect.await.map_err(Into::into)
        }))
    }
}

fn build_client(tls: &rustls::ClientConfig) -> OutboundClient {
    let mut http = HttpConnector::new_with_resolver(PinnedResolver);
    http.enforce_http(false);
    http.set_happy_eyeballs_timeout(Some(HAPPY_EYEBALLS_ATTEMPT_DELAY));
    http.set_connect_timeout(Some(CONNECT_TIMEOUT));
    // hyper-rustls refuses a config with ALPN already set and derives it from
    // the enabled protocols (h2, http/1.1), so hand it a copy without.
    let mut tls = tls.clone();
    tls.alpn_protocols.clear();
    let https = HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .wrap_connector(TrackedConnector { inner: http });
    Client::builder(TokioExecutor::new())
        .timer(TokioTimer::new())
        .pool_timer(TokioTimer::new())
        .retry_canceled_requests(true)
        .build(https)
}

fn error_chain(error: &(dyn StdError + 'static)) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

fn has_unreachable_cause(error: &(dyn StdError + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(io_error) = error.downcast_ref::<io::Error>()
            && matches!(
                io_error.kind(),
                io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::HostUnreachable
                    | io::ErrorKind::NetworkUnreachable
                    | io::ErrorKind::TimedOut
            )
        {
            return true;
        }
        current = error.source();
    }
    false
}

fn send_error(
    error: &hyper_util::client::legacy::Error,
    body_limit_hit: &AtomicBool,
) -> OutboundError {
    if body_limit_hit.load(Ordering::Acquire) {
        return OutboundError::BodyLimit;
    }
    if error.is_connect() && has_unreachable_cause(error) {
        return OutboundError::Connect(error_chain(error));
    }
    OutboundError::Transport(error_chain(error))
}

fn outbound_body(body: RequestBody, limit: u64, limit_hit: Arc<AtomicBool>) -> OutboundBody {
    match body {
        RequestBody::Empty => Empty::new().map_err(|never| match never {}).boxed_unsync(),
        RequestBody::Bytes(bytes) => Full::new(bytes)
            .map_err(|never| match never {})
            .boxed_unsync(),
        RequestBody::Stream(stream) => {
            let mut total: u64 = 0;
            let frames = stream.map(move |chunk| -> Result<Frame<Bytes>, BoxError> {
                let chunk = chunk?;
                total = total.saturating_add(chunk.len() as u64);
                if total > limit {
                    limit_hit.store(true, Ordering::Release);
                    return Err(Box::new(OutboundError::BodyLimit));
                }
                Ok(Frame::data(chunk))
            });
            StreamBody::new(frames).boxed_unsync()
        }
    }
}

/// Response body that keeps enforcing the request deadline, caller
/// cancellation and shutdown until the body ends.
pub struct ResponseBody {
    incoming: Option<Incoming>,
    deadline: Pin<Box<Sleep>>,
    cancel: Pin<Box<WaitForCancellationFutureOwned>>,
    shutdown: Pin<Box<WaitForCancellationFutureOwned>>,
}

impl Stream for ResponseBody {
    type Item = Result<Bytes, OutboundError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let Some(incoming) = this.incoming.as_mut() else {
            return Poll::Ready(None);
        };
        let terminal = if this.cancel.as_mut().poll(cx).is_ready() {
            Some(OutboundError::Aborted)
        } else if this.shutdown.as_mut().poll(cx).is_ready() {
            Some(OutboundError::Closed)
        } else if this.deadline.as_mut().poll(cx).is_ready() {
            Some(OutboundError::Timeout)
        } else {
            loop {
                match Pin::new(&mut *incoming).poll_frame(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Some(Ok(frame))) => {
                        if let Ok(data) = frame.into_data() {
                            return Poll::Ready(Some(Ok(data)));
                        }
                    }
                    Poll::Ready(Some(Err(error))) => {
                        break Some(OutboundError::Transport(error_chain(&error)));
                    }
                    Poll::Ready(None) => break None,
                }
            }
        };
        this.incoming = None;
        Poll::Ready(terminal.map(Err))
    }
}

#[derive(Clone)]
pub struct OutboundHttp {
    inner: Arc<Inner>,
}

impl OutboundHttp {
    pub fn new(tls: Arc<rustls::ClientConfig>) -> Self {
        Self::with_lookup(tls, Arc::new(SystemLookup))
    }

    pub(crate) fn with_lookup(tls: Arc<rustls::ClientConfig>, lookup: Arc<dyn HostLookup>) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                client: Mutex::new(Some(build_client(&tls))),
                lookup,
                shutdown: CancellationToken::new(),
                next_pending_id: AtomicU64::new(0),
            }),
        }
    }

    /// Registers `origins` (and the anchor itself) under the requirement the
    /// anchor's address scope implies: a publicly resolving anchor makes every
    /// origin `Public`, otherwise `Any`. When the anchor cannot be resolved,
    /// `remembered` is used instead if given. All-or-nothing: nothing is
    /// registered if any origin is invalid, already bound outside the
    /// requirement, or the origin cap would be exceeded.
    pub async fn register_anchored_origins(
        &self,
        anchor: &str,
        origins: &[String],
        remembered: Option<AddressRequirement>,
    ) -> Result<AddressRequirement, OutboundError> {
        self.inner.require_admission()?;
        let canonical_anchor = require_origin(anchor)?;
        let mut canonical_origins = HashSet::from([canonical_anchor.clone()]);
        for origin in origins {
            canonical_origins.insert(require_origin(origin)?);
        }
        let requirement = self
            .anchor_requirement(&canonical_anchor, remembered)
            .await?;
        let mut state = self.inner.state();
        if state.closed {
            return Err(OutboundError::Closed);
        }
        let mut additions = 0;
        for origin in &canonical_origins {
            if !state.registered.contains_key(origin) {
                additions += 1;
            }
            if let Some(binding) = state.bindings.get(origin) {
                require_scope(binding, requirement)?;
            }
        }
        if state.registered.len() + additions > MAX_ORIGINS {
            return Err(OutboundError::Capacity);
        }
        for origin in canonical_origins {
            let next = state
                .registered
                .get(&origin)
                .map_or(requirement, |&current| strictest(current, requirement));
            if next == AddressRequirement::Public
                && let Some(pending) = state.pending.get(&origin)
            {
                pending.public.store(true, Ordering::Release);
            }
            state.registered.insert(origin, next);
        }
        Ok(requirement)
    }

    async fn anchor_requirement(
        &self,
        canonical_anchor: &str,
        remembered: Option<AddressRequirement>,
    ) -> Result<AddressRequirement, OutboundError> {
        match self
            .ensure_binding(canonical_anchor, AddressRequirement::Any)
            .await
        {
            Ok(anchor) if anchor.scope == AddressScope::Public => Ok(AddressRequirement::Public),
            Ok(_) => Ok(AddressRequirement::Any),
            Err(error) => match remembered {
                Some(requirement) if error.is_resolution_failure() => Ok(requirement),
                _ => Err(error),
            },
        }
    }

    /// Registered `http://` origins whose requirement is `Any`, sorted.
    pub fn registered_cleartext_origins(&self) -> Vec<String> {
        let state = self.inner.state();
        let mut origins: Vec<String> = state
            .registered
            .iter()
            .filter(|&(origin, &requirement)| {
                requirement == AddressRequirement::Any && origin.starts_with("http://")
            })
            .map(|(origin, _)| origin.clone())
            .collect();
        origins.sort();
        origins
    }

    pub fn require_registered_origins(&self, origins: &[String]) -> Result<(), OutboundError> {
        for origin in origins {
            let canonical = require_origin(origin)?;
            if !self.inner.state().registered.contains_key(&canonical) {
                return Err(OutboundError::OriginNotRegistered(canonical));
            }
        }
        Ok(())
    }

    /// Address a cleartext transport for `origin` must connect to: refuses
    /// publicly routable origins, prefers the first IPv4 address.
    pub async fn require_cleartext_transport_address(
        &self,
        origin: &str,
    ) -> Result<IpAddr, OutboundError> {
        self.inner.require_admission()?;
        let canonical = require_origin(origin)?;
        let requirement = self
            .inner
            .state()
            .registered
            .get(&canonical)
            .copied()
            .unwrap_or(AddressRequirement::Any);
        let binding = self.ensure_binding(&canonical, requirement).await?;
        if binding.scope == AddressScope::Public {
            return Err(OutboundError::InsecureTransport);
        }
        Ok(binding
            .addresses
            .iter()
            .copied()
            .find(IpAddr::is_ipv4)
            .unwrap_or(binding.addresses[0]))
    }

    pub async fn request(&self, req: OriginRequest) -> Result<OriginResponse, OutboundError> {
        self.inner.require_admission()?;
        let origin = require_origin(&req.expected_origin)?;
        let target = parse_target(&req.url)
            .filter(|target| http_origin_of(target).as_deref() == Some(origin.as_str()))
            .ok_or(OutboundError::InvalidTarget)?;
        if let RequestBody::Bytes(bytes) = &req.body
            && bytes.len() as u64 > req.body_limit
        {
            return Err(OutboundError::BodyLimit);
        }
        let requirement = self.requirement_for(&origin, req.origin_trust)?;
        // `get()` refused cleartext to a public-only origin before resolving it.
        if req.origin_trust == OriginTrust::Public
            && requirement == AddressRequirement::Public
            && target.scheme() != "https"
        {
            return Err(OutboundError::InsecureTransport);
        }
        let binding = self.ensure_binding(&origin, requirement).await?;
        if binding.scope == AddressScope::Public && target.scheme() != "https" {
            return Err(OutboundError::InsecureTransport);
        }
        self.inner.require_admission()?;
        self.issue(req, target, binding).await
    }

    pub fn cleanup(&self) {
        let mut state = self.inner.state();
        if state.closed {
            return;
        }
        state.closed = true;
        state.bindings.clear();
        state.registered.clear();
        drop(state);
        self.inner.shutdown.cancel();
        self.inner
            .client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }

    fn requirement_for(
        &self,
        origin: &str,
        trust: OriginTrust,
    ) -> Result<AddressRequirement, OutboundError> {
        if let Some(&registered) = self.inner.state().registered.get(origin) {
            return Ok(registered);
        }
        match trust {
            OriginTrust::Registered => Err(OutboundError::OriginNotRegistered(origin.to_owned())),
            OriginTrust::Bound => Ok(AddressRequirement::Any),
            OriginTrust::Public => Ok(AddressRequirement::Public),
        }
    }

    /// Returns the origin's binding, resolving it when absent or unreachable.
    /// Concurrent callers share one resolution; each re-checks the binding
    /// against the requirement as tightened while it ran.
    async fn ensure_binding(
        &self,
        origin: &str,
        requirement: AddressRequirement,
    ) -> Result<Arc<Binding>, OutboundError> {
        let (public, result) = {
            let mut state = self.inner.state();
            if state.closed {
                return Err(OutboundError::Closed);
            }
            let existing = state.bindings.get(origin).cloned();
            if let Some(binding) = existing.as_ref().filter(|binding| !binding.unreachable) {
                require_scope(binding, requirement)?;
                return Ok(binding.clone());
            }
            if let Some(pending) = state.pending.get(origin) {
                if requirement == AddressRequirement::Public {
                    pending.public.store(true, Ordering::Release);
                }
                (pending.public.clone(), pending.result.clone())
            } else {
                if state.pending.len() >= MAX_PENDING_RESOLUTIONS {
                    return Err(OutboundError::Capacity);
                }
                if existing.is_none() && state.bindings.len() + state.pending.len() >= MAX_ORIGINS {
                    return Err(OutboundError::Capacity);
                }
                let pending = self.spawn_resolution(origin, requirement);
                let shared = (pending.public.clone(), pending.result.clone());
                state.pending.insert(origin.to_owned(), pending);
                shared
            }
        };
        let binding = result.await?;
        require_scope(&binding, pending_requirement(&public))?;
        Ok(binding)
    }

    /// Runs the resolution as its own task so it completes (and clears its
    /// pending entry) even when every waiter goes away. Must be called with
    /// the state lock held so the entry is inserted before the task finishes.
    fn spawn_resolution(&self, origin: &str, requirement: AddressRequirement) -> PendingBinding {
        let id = self.inner.next_pending_id.fetch_add(1, Ordering::Relaxed);
        let public = Arc::new(AtomicBool::new(requirement == AddressRequirement::Public));
        let inner = self.inner.clone();
        let task_origin = origin.to_owned();
        let task_public = public.clone();
        let task = tokio::spawn(async move {
            let resolved = resolve_binding(&*inner.lookup, &task_origin).await;
            let mut state = inner.state();
            if state
                .pending
                .get(&task_origin)
                .is_some_and(|pending| pending.id == id)
            {
                state.pending.remove(&task_origin);
            }
            let binding = Arc::new(resolved?);
            if state.closed {
                return Err(OutboundError::Closed);
            }
            require_scope(&binding, pending_requirement(&task_public))?;
            state.bindings.insert(task_origin, binding.clone());
            Ok(binding)
        });
        let result = async move {
            task.await
                .unwrap_or_else(|error| Err(OutboundError::Transport(error.to_string())))
        }
        .boxed()
        .shared();
        PendingBinding { id, public, result }
    }

    /// Marks `binding` for re-resolution if it is still the origin's current one.
    fn mark_binding_unreachable(&self, binding: &Arc<Binding>) {
        let mut state = self.inner.state();
        if !state
            .bindings
            .get(&binding.origin)
            .is_some_and(|current| Arc::ptr_eq(current, binding))
        {
            return;
        }
        let marked = Binding {
            unreachable: true,
            ..(**binding).clone()
        };
        state
            .bindings
            .insert(binding.origin.clone(), Arc::new(marked));
    }

    async fn issue(
        &self,
        req: OriginRequest,
        target: Url,
        binding: Arc<Binding>,
    ) -> Result<OriginResponse, OutboundError> {
        let mut deadline = Box::pin(tokio::time::sleep(req.timeout));
        if req.cancel.is_cancelled() {
            return Err(OutboundError::Aborted);
        }
        let client = self
            .inner
            .client
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .ok_or(OutboundError::Closed)?;
        let uri: Uri = target
            .as_str()
            .parse()
            .map_err(|_| OutboundError::InvalidTarget)?;
        let mut headers = req.headers;
        headers.remove(header::HOST);
        let limit_hit = Arc::new(AtomicBool::new(false));
        let mut request = Request::new(outbound_body(req.body, req.body_limit, limit_hit.clone()));
        *request.method_mut() = req.method;
        *request.uri_mut() = uri;
        *request.headers_mut() = headers;
        let context = ConnectContext {
            hostname: target.host_str().unwrap_or_default().to_owned(),
            addresses: binding.addresses.clone().into(),
            connecting: Arc::new(AtomicUsize::new(0)),
        };
        let connecting = context.connecting.clone();
        let send = CONNECT_CONTEXT.scope(context, client.request(request));
        let mut cancel = Box::pin(req.cancel.cancelled_owned());
        let mut shutdown = Box::pin(self.inner.shutdown.clone().cancelled_owned());
        let interrupted = |error: OutboundError| {
            if connecting.load(Ordering::Acquire) > 0 {
                self.mark_binding_unreachable(&binding);
            }
            Err(error)
        };
        let response = tokio::select! {
            biased;
            () = &mut cancel => return interrupted(OutboundError::Aborted),
            () = &mut shutdown => return Err(OutboundError::Closed),
            () = &mut deadline => return interrupted(OutboundError::Timeout),
            result = send => result.map_err(|error| send_error(&error, &limit_hit)),
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                if error.is_unreachable() {
                    self.mark_binding_unreachable(&binding);
                }
                return Err(error);
            }
        };
        let (parts, incoming) = response.into_parts();
        Ok(OriginResponse {
            status: parts.status,
            headers: parts.headers,
            url: target.to_string(),
            body: ResponseBody {
                incoming: Some(incoming),
                deadline,
                cancel,
                shutdown,
            },
        })
    }
}

async fn resolve_binding(lookup: &dyn HostLookup, origin: &str) -> Result<Binding, OutboundError> {
    let hostname = host_of(origin)?;
    if let Some(literal) = parse_ip_literal(&hostname) {
        return Ok(Binding {
            origin: origin.to_owned(),
            addresses: vec![literal],
            scope: address_scope(literal),
            unreachable: false,
        });
    }
    let answer = tokio::time::timeout(
        RESOLUTION_TIMEOUT,
        lookup.lookup(absolute_lookup_hostname(&hostname)),
    )
    .await
    .map_err(|_| OutboundError::ResolutionTimeout)?
    .map_err(|error| OutboundError::Resolution(error.to_string()))?;
    let addresses = select_pinned_addresses(answer);
    let primary = *addresses.first().ok_or(OutboundError::EmptyResolution)?;
    Ok(Binding {
        origin: origin.to_owned(),
        addresses,
        scope: address_scope(primary),
        unreachable: false,
    })
}

#[cfg(test)]
mod tests;
