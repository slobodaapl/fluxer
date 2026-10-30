// SPDX-License-Identifier: AGPL-3.0-or-later

//! `readMessageContentLength` and `readBoundedMessage` of DesktopOutboundHTTP.ts: a response
//! body read whole, refused when its `Content-Length` header is malformed or over the cap, when
//! more bytes arrive than the cap allows, or when they arrive in more chunks than allowed.
//! Errors carry the TS messages.

use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use hyper::{HeaderMap, header};

use super::outbound::ResponseBody;
use crate::js::js_trim;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// The limits of one `readBoundedMessage` caller and the description its errors start with.
pub struct BodyBound {
    pub description: &'static str,
    pub max_bytes: u64,
    pub max_chunks: usize,
}

impl BodyBound {
    fn invalid_content_length(&self) -> String {
        format!("{} has an invalid Content-Length header", self.description)
    }

    fn byte_limit(&self) -> String {
        format!("{} exceeds {} bytes", self.description, self.max_bytes)
    }

    /// `readMessageContentLength`: absent, or one decimal safe integer.
    pub fn content_length(&self, headers: &HeaderMap) -> Result<Option<u64>, String> {
        let mut values = headers.get_all(header::CONTENT_LENGTH).iter();
        let Some(value) = values.next() else {
            return Ok(None);
        };
        if values.next().is_some() {
            return Err(self.invalid_content_length());
        }
        let digits = value
            .to_str()
            .map(js_trim)
            .map_err(|_| self.invalid_content_length())?;
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(self.invalid_content_length());
        }
        digits
            .parse::<u64>()
            .ok()
            .filter(|&length| length <= MAX_SAFE_INTEGER)
            .map(Some)
            .ok_or_else(|| self.invalid_content_length())
    }

    /// `readBoundedMessage` with `declared` from [`Self::content_length`].
    pub async fn read(&self, body: ResponseBody, declared: Option<u64>) -> Result<Bytes, String> {
        if declared.is_some_and(|length| length > self.max_bytes) {
            return Err(self.byte_limit());
        }
        let mut body = std::pin::pin!(body);
        let mut buffer = BytesMut::new();
        let mut chunks = 0;
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|error| error.to_string())?;
            // Node never emits empty `data` chunks, so they do not count against the limit.
            if chunk.is_empty() {
                continue;
            }
            chunks += 1;
            if chunks > self.max_chunks {
                return Err(format!(
                    "{} exceeds {} response chunks",
                    self.description, self.max_chunks
                ));
            }
            if (buffer.len() + chunk.len()) as u64 > self.max_bytes {
                return Err(self.byte_limit());
            }
            buffer.extend_from_slice(&chunk);
        }
        Ok(buffer.freeze())
    }
}
