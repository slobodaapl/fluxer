// SPDX-License-Identifier: AGPL-3.0-or-later

//! Notification icons, the port of `resolveNotificationIcon` in NotificationIcon.ts: the
//! renderer's icon source decoded to RGBA.
//!
//! - `http:`/`https:` URLs are fetched through outbound HTTP the way `DesktopOutboundHTTP.get`
//!   did: an origin no instance registered must be https and resolve to public addresses only,
//!   a registered one keeps its recorded requirement. Each hop gets 10 s; up to five
//!   301/302/303/307/308 redirects are followed, each target checked again; anything but 200
//!   fails; the body is capped at 3 MiB in 4096 chunks.
//! - `data:` URLs decode when their media type is `image/png` or `image/jpeg`, as
//!   `nativeImage.createFromDataURL` required.
//! - Everything else is refused.
//!
//! A downloaded body decodes as PNG or JPEG by its signature, as `nativeImage.createFromBuffer`
//! accepted. Decoding stops at 64 MiB of RGBA (the png crate's default buffer limit; zune-jpeg
//! is held to the same budget), where Chromium set no explicit limit.
//!
//! Deviation (a hazard Electron had on Windows): a URL on the local app origin is read from the
//! renderer files and never fetched. Tauri serves that origin as `https://fluxer-app.localhost`
//! on Windows, which the http branch would otherwise download over loopback. Its `/api` and
//! `/proxy` routes are refused, and a path that falls back to `index.html` is not an icon. On
//! other platforms the origin is `fluxer-app://app`, which Electron refused outright; here its
//! files resolve too.

use std::fmt;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use hyper::{HeaderMap, Method, StatusCode, header};
use percent_encoding::percent_decode_str;
use tokio_util::sync::CancellationToken;
use url::Url;
use zune_jpeg::JpegDecoder;
use zune_jpeg::zune_core::colorspace::ColorSpace;
use zune_jpeg::zune_core::options::DecoderOptions;

use crate::local_app::files::{LocalAppFiles, Resolved, is_index_file};
use crate::local_app::url::{is_reserved_proxy_path, parse_local_app_url};
use crate::net::bounded_body::BodyBound;
use crate::net::origin::http_origin_of;
use crate::net::outbound::{
    DEFAULT_REQUEST_BODY_LIMIT, OriginRequest, OriginTrust, OutboundHttp, RequestBody,
    parse_redirect_target,
};
use crate::png_image::{self, RgbaImage};

/// `NOTIFICATION_ICON_DOWNLOAD_TIMEOUT_MS`.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10);
/// `NOTIFICATION_ICON_MAX_REDIRECTS`.
const MAX_REDIRECTS: u32 = 5;
const MAX_BYTES: u64 = 3 * 1024 * 1024;
const DOWNLOAD: BodyBound = BodyBound {
    description: "Notification icon download",
    max_bytes: MAX_BYTES,
    max_chunks: 4096,
};
const MAX_DECODED_BYTES: usize = 64 * 1024 * 1024;
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const JPEG_SIGNATURE: &[u8] = b"\xff\xd8\xff";
/// The WHATWG forgiving-base64 decode Chromium applies to `data:` URLs: padding optional,
/// trailing bits ignored (ASCII whitespace is removed beforehand).
const FORGIVING_BASE64: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// What icon sources are read through.
pub struct IconSources<'a> {
    pub outbound: &'a OutboundHttp,
    pub files: &'a LocalAppFiles,
}

#[derive(Debug)]
pub enum IconError {
    /// Not an http(s), `data:` or local app URL.
    UnsupportedSource,
    /// An `/api` or `/proxy` route of the local app origin.
    ReservedLocalPath,
    /// No renderer file, or one that could not be read.
    LocalFile(String),
    Download(String),
    /// Not a PNG or JPEG, or not one that decodes.
    Undecodable,
}

impl fmt::Display for IconError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSource => {
                formatter.write_str("Rejected non-URL notification icon source")
            }
            Self::ReservedLocalPath => {
                formatter.write_str("Rejected a local app route as a notification icon source")
            }
            Self::LocalFile(error) => {
                write!(formatter, "Notification icon file unavailable: {error}")
            }
            Self::Download(error) => formatter.write_str(error),
            Self::Undecodable => {
                formatter.write_str("Notification icon could not be decoded as PNG/JPEG")
            }
        }
    }
}

/// `describeIconSource`: enough of the source to log, without query, fragment or data.
pub fn describe(source: &str) -> String {
    if source.starts_with("data:") {
        return "data-url".to_owned();
    }
    match Url::parse(source) {
        Ok(url) => {
            let port = url
                .port()
                .map(|port| format!(":{port}"))
                .unwrap_or_default();
            format!(
                "{}://{}{port}{}",
                url.scheme(),
                url.host_str().unwrap_or_default(),
                url.path()
            )
        }
        Err(_) => std::path::Path::new(source).file_name().map_or_else(
            || "local-path".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        ),
    }
}

/// `resolveNotificationIcon` up to the decoded image.
pub async fn resolve(source: &str, sources: IconSources<'_>) -> Result<RgbaImage, IconError> {
    if let Some(url) = parse_local_app_url(source) {
        return read_local(source, &url, sources.files).await;
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        let bytes = download(sources.outbound, source)
            .await
            .map_err(IconError::Download)?;
        return decode(&bytes);
    }
    if let Some(data_url) = source.strip_prefix("data:") {
        return decode_data_url(data_url);
    }
    Err(IconError::UnsupportedSource)
}

async fn read_local(
    source: &str,
    url: &Url,
    files: &LocalAppFiles,
) -> Result<RgbaImage, IconError> {
    if is_reserved_proxy_path(url.path()) {
        return Err(IconError::ReservedLocalPath);
    }
    let file = match files.resolve(source).await {
        Ok(Resolved::File(file)) if !is_index_file(&file.path) => file,
        Ok(_) => return Err(IconError::LocalFile("no such renderer file".to_owned())),
        Err(error) => return Err(IconError::LocalFile(error.to_string())),
    };
    let bytes = files
        .read(&file, Some(MAX_BYTES))
        .await
        .map_err(|error| IconError::LocalFile(error.to_string()))?;
    decode(&bytes)
}

/// `downloadToBuffer`.
async fn download(outbound: &OutboundHttp, url: &str) -> Result<bytes::Bytes, String> {
    let invalid = || "Notification icon URL must use http or https".to_owned();
    let mut target = Url::parse(url).map_err(|_| invalid())?;
    let mut redirects_remaining = MAX_REDIRECTS;
    loop {
        let origin = http_origin_of(&target).ok_or_else(invalid)?;
        let response = outbound
            .request(OriginRequest {
                expected_origin: origin,
                url: target.to_string(),
                method: Method::GET,
                headers: HeaderMap::new(),
                body: RequestBody::Empty,
                body_limit: DEFAULT_REQUEST_BODY_LIMIT,
                timeout: DOWNLOAD_TIMEOUT,
                origin_trust: OriginTrust::Public,
                cancel: CancellationToken::new(),
            })
            .await
            .map_err(|error| format!("{}: {error}", DOWNLOAD.description))?;
        let status = response.status;
        if matches!(
            status,
            StatusCode::MOVED_PERMANENTLY
                | StatusCode::FOUND
                | StatusCode::SEE_OTHER
                | StatusCode::TEMPORARY_REDIRECT
                | StatusCode::PERMANENT_REDIRECT
        ) {
            if redirects_remaining == 0 {
                return Err("Notification icon download exceeded redirect limit".to_owned());
            }
            target = parse_redirect_target(&target, response.headers.get(header::LOCATION))
                .ok_or_else(|| {
                    format!(
                        "Notification icon redirect target is unusable ({})",
                        status.as_u16()
                    )
                })?;
            redirects_remaining -= 1;
            continue;
        }
        if status != StatusCode::OK {
            return Err(format!(
                "Notification icon download failed with HTTP {}",
                status.as_u16()
            ));
        }
        let declared = DOWNLOAD.content_length(&response.headers)?;
        return DOWNLOAD.read(response.body, declared).await;
    }
}

/// `nativeImage.createFromBuffer`: PNG or JPEG, told apart by signature.
fn decode(bytes: &[u8]) -> Result<RgbaImage, IconError> {
    if bytes.starts_with(PNG_SIGNATURE) {
        decode_png(bytes)
    } else if bytes.starts_with(JPEG_SIGNATURE) {
        decode_jpeg(bytes)
    } else {
        Err(IconError::Undecodable)
    }
}

fn decode_png(bytes: &[u8]) -> Result<RgbaImage, IconError> {
    png_image::decode_any(bytes).map_err(|_| IconError::Undecodable)
}

fn decode_jpeg(bytes: &[u8]) -> Result<RgbaImage, IconError> {
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = JpegDecoder::new_with_options(std::io::Cursor::new(bytes), options);
    decoder
        .decode_headers()
        .map_err(|_| IconError::Undecodable)?;
    let (width, height) = decoder.dimensions().ok_or(IconError::Undecodable)?;
    if width.saturating_mul(height).saturating_mul(4) > MAX_DECODED_BYTES {
        return Err(IconError::Undecodable);
    }
    let pixels = decoder.decode().map_err(|_| IconError::Undecodable)?;
    Ok(RgbaImage {
        width: u32::try_from(width).map_err(|_| IconError::Undecodable)?,
        height: u32::try_from(height).map_err(|_| IconError::Undecodable)?,
        pixels,
    })
}

/// `nativeImage.createFromDataURL`: Chromium's `DataURL::Parse`, then the decoder its media
/// type names. `rest` follows `data:`.
fn decode_data_url(rest: &str) -> Result<RgbaImage, IconError> {
    let (metadata, data) = rest.split_once(',').ok_or(IconError::Undecodable)?;
    let mut parameters = metadata.split(';');
    let media_type = parameters
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let is_base64 = parameters.any(|parameter| parameter.trim().eq_ignore_ascii_case("base64"));
    let mut bytes: Vec<u8> = percent_decode_str(data).collect();
    if is_base64 {
        bytes.retain(|byte| !byte.is_ascii_whitespace());
        bytes = FORGIVING_BASE64
            .decode(&bytes)
            .map_err(|_| IconError::Undecodable)?;
    }
    match media_type.as_str() {
        "image/png" => decode_png(&bytes),
        "image/jpeg" => decode_jpeg(&bytes),
        _ => Err(IconError::Undecodable),
    }
}

#[cfg(test)]
mod tests;
