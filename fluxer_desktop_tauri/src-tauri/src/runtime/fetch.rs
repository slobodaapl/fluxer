// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bounded requests to the selected instance, ported from SelectedInstanceFetch.ts.
//!
//! Failures are only ever reported as discovery attempt reasons, so they carry the TS message.

use std::time::Duration;

use bytes::Bytes;
use hyper::{HeaderMap, Method, StatusCode};
use tokio_util::sync::CancellationToken;

use crate::net::bounded_body::BodyBound;
use crate::net::outbound::{
    DEFAULT_REQUEST_BODY_LIMIT, OriginRequest, OriginTrust, OutboundHttp, RequestBody,
};

/// At most 1 MiB in at most 1024 chunks.
const INSTANCE_RESPONSE: BodyBound = BodyBound {
    description: "Instance response",
    max_bytes: 1024 * 1024,
    max_chunks: 1024,
};

pub struct InstanceResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    /// `None` for 204, 205 and 304.
    pub body: Option<Bytes>,
}

/// `DesktopSelectedInstanceClient.fetch` for a GET with `Bound` origin trust.
pub async fn fetch_instance(
    outbound: &OutboundHttp,
    url: &str,
    expected_origin: &str,
    headers: HeaderMap,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<InstanceResponse, String> {
    let response = outbound
        .request(OriginRequest {
            expected_origin: expected_origin.to_owned(),
            url: url.to_owned(),
            method: Method::GET,
            headers,
            body: RequestBody::Empty,
            body_limit: DEFAULT_REQUEST_BODY_LIMIT,
            timeout,
            origin_trust: OriginTrust::Bound,
            cancel: cancel.clone(),
        })
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status;
    if status.is_redirection() {
        return Err(format!(
            "Instance request to {url} was answered with an HTTP {} redirect, which is never followed",
            status.as_u16()
        ));
    }
    let body = if has_no_body(status) {
        None
    } else {
        let declared = INSTANCE_RESPONSE.content_length(&response.headers)?;
        Some(INSTANCE_RESPONSE.read(response.body, declared).await?)
    };
    Ok(InstanceResponse {
        status,
        headers: response.headers,
        body,
    })
}

fn has_no_body(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT | StatusCode::NOT_MODIFIED
    )
}
