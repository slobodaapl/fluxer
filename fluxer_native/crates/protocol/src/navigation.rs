// SPDX-License-Identifier: AGPL-3.0-or-later
//! SearchUtils, ChannelPinsCommands and MessageCommands wire contracts.
//! UI scope/permissions/mature-content gates and indexing polling are controller
//! policy; these operations never synthesize alternate routes or extra requests.
use crate::{Client, Error, Result, http::segment};
use reqwest::{Method, header, redirect::Policy};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug)]
pub struct SearchResponse {
    pub status: u16,
    /// Unmodified wire body, including channels, threads and members.
    pub body: Value,
    /// Response hint only. Existing indexing poll policy does not consume it.
    pub retry_after: Option<Duration>,
}
impl SearchResponse {
    pub fn is_indexing(&self) -> bool {
        self.body.get("indexing").and_then(Value::as_bool) == Some(true)
    }
}

/// ChannelSearchPolling.ts: 1500 ms * 1.25^count, capped at 5000 ms.
pub fn channel_search_poll_interval(poll_count: u32) -> Duration {
    Duration::from_secs_f64((1.5 * 1.25_f64.powi(poll_count.min(10) as i32)).min(5.0))
}
fn api_client() -> Result<reqwest::Client> {
    // RestTransport defaultTimeoutMs is zero. The caller's task owns request
    // cancellation; dropping these futures cancels a send, body read or delay.
    Ok(reqwest::Client::builder()
        .redirect(Policy::none())
        .build()?)
}
impl Client {
    pub async fn recent_mentions(&self,before:Option<&str>,everyone:bool,roles:bool,guilds:bool)->Result<Value> {
        let path={
            let mut query=url::form_urlencoded::Serializer::new(String::new());
            query.append_pair("everyone",if everyone{"true"}else{"false"}).append_pair("roles",if roles{"true"}else{"false"}).append_pair("guilds",if guilds{"true"}else{"false"}).append_pair("limit","50");
            if let Some(before)=before {query.append_pair("before",before);}
            format!("/users/@me/mentions?{}",query.finish())
        };
        self.request(Method::GET,&path,None).await
    }

    /// `params` already has SearchUtils.toApiParams snake_case fields, context,
    /// page, sort and scope. Do not silently remap or discard caller filters.
    pub async fn search_messages(&self, params: &Value) -> Result<SearchResponse> {
        let client = api_client()?;
        let response = self
            .upload_api_request(&client, Method::POST, "/search/messages")?
            .json(params)
            .send()
            .await?;
        let status = response.status().as_u16();
        let retry_after = response_retry_after(response.headers());
        let body = Self::parse_api_response(response)
            .await
            .map_err(|error| rate_hint(error, retry_after))?;
        // SearchUtils requires a body before checking indexing, independently
        // of whether the server used 200 or 202 for an indexing response.
        if !body.is_object() {
            return Err(Error::InvalidSearchResponse);
        }
        Ok(SearchResponse {
            status,
            body,
            retry_after,
        })
    }

    /// The cursor is a pin timestamp, never a message snowflake. Fetching does
    /// not acknowledge unread pins; ReadStates applies that policy on pane mount.
    pub async fn pins_page(&self, channel: &str, before: Option<&str>) -> Result<Value> {
        let mut path = format!("/channels/{}/messages/pins?limit=25", segment(channel));
        if let Some(before) = before.filter(|value| !value.is_empty()) {
            path.push_str(&format!("&before={}", segment(before)));
        }
        let value = self.navigation_request(Method::GET, &path).await?;
        Ok(if value.is_null() {
            json!({"items":[],"has_more":false})
        } else {
            value
        })
    }
    pub async fn pin_message(&self, channel: &str, message: &str) -> Result<()> {
        self.navigation_request(Method::PUT, &pin_path(channel, message))
            .await?;
        Ok(())
    }
    pub async fn unpin_message(&self, channel: &str, message: &str) -> Result<()> {
        self.navigation_request(Method::DELETE, &pin_path(channel, message))
            .await?;
        Ok(())
    }
    pub async fn acknowledge_pins(&self, channel: &str) -> Result<()> {
        self.navigation_request(
            Method::POST,
            &format!("/channels/{}/pins/ack", segment(channel)),
        )
        .await?;
        Ok(())
    }
    async fn navigation_request(&self, method: Method, path: &str) -> Result<Value> {
        let client = api_client()?;
        let response = self
            .upload_api_request(&client, method, path)?
            .send()
            .await?;
        let retry_after = response_retry_after(response.headers());
        Self::parse_api_response(response)
            .await
            .map_err(|error| rate_hint(error, retry_after))
    }

    /// MessageCommands.requestChannelMessages explicitly requests two retries.
    /// All supplied cursors/limits are preserved; server validation is authoritative.
    pub async fn messages_page(
        &self,
        channel: &str,
        before: Option<&str>,
        after: Option<&str>,
        around: Option<&str>,
        limit: u8,
    ) -> Result<Value> {
        let path = history_path(channel, before, after, around, limit);
        let client = api_client()?;
        for attempt in 0..=2 {
            let response = self
                .upload_api_request(&client, Method::GET, &path)?
                .send()
                .await;
            match response {
                Err(error) if attempt < 2 && !error.is_builder() => {
                    tokio::time::sleep(history_retry_delay(attempt)).await;
                }
                Err(error) => return Err(error.into()),
                Ok(response) => {
                    let retryable = history_retry_status(response.status().as_u16());
                    let retry_after = response_retry_after(response.headers());
                    let result = Self::parse_api_response(response)
                        .await
                        .map_err(|error| rate_hint(error, retry_after));
                    // A body-read network error is a transport failure too.
                    let network = matches!(&result,Err(Error::Http(error)) if !error.is_builder());
                    if attempt < 2 && (retryable || network) {
                        tokio::time::sleep(history_retry_delay(attempt)).await;
                    } else {
                        return result.map(|body| if body.is_null() { json!([]) } else { body });
                    }
                }
            }
        }
        unreachable!()
    }
}
fn history_path(
    channel: &str,
    before: Option<&str>,
    after: Option<&str>,
    around: Option<&str>,
    limit: u8,
) -> String {
    // Undefined/null query values are omitted by RestTransport.resolveUrl.
    let mut path = format!("/channels/{}/messages?limit={limit}", segment(channel));
    for (key, value) in [("before", before), ("after", after), ("around", around)] {
        if let Some(value) = value {
            path.push_str(&format!("&{key}={}", segment(value)));
        }
    }
    path
}
fn pin_path(channel: &str, message: &str) -> String {
    format!("/channels/{}/pins/{}", segment(channel), segment(message))
}
fn history_retry_status(status: u16) -> bool {
    matches!(status, 502 | 504 | 507 | 522 | 523 | 524 | 598 | 599)
}
fn history_retry_delay(index: u32) -> Duration {
    let target = (1000.0 * 2_f64.powi(index.min(16) as i32)).min(30_000.);
    Duration::from_millis((target * (0.75 + fastrand::f64() * 0.5)).floor() as u64)
}
fn response_retry_after(headers: &header::HeaderMap) -> Option<Duration> {
    // RestTransport.recordPacing: x-ratelimit-reset-after is headerMs and takes
    // precedence over Retry-After seconds/date. Do not assume HTTP-library units.
    if let Some(ms) = headers
        .get("x-ratelimit-reset-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|ms| ms.is_finite() && *ms > 0.)
    {
        return Duration::try_from_secs_f64(ms / 1000.).ok();
    }
    let value = headers.get(header::RETRY_AFTER)?.to_str().ok()?;
    let seconds = value.parse::<f64>().ok().or_else(|| {
        let deadline = chrono::DateTime::parse_from_rfc2822(value).ok()?;
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()?
            .as_secs_f64();
        Some(deadline.timestamp_millis() as f64 / 1000. - now)
    })?;
    if !seconds.is_finite() {
        return None;
    }
    Duration::try_from_secs_f64(seconds.max(0.)).ok()
}

fn rate_hint(error: Error, hint: Option<Duration>) -> Error {
    match (error, hint) {
        (Error::RateLimited { global, .. }, Some(retry_after)) => Error::RateLimited {
            global,
            retry_after,
        },
        (error, _) => error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexing_interval_and_history_retries_match_existing_transport() {
        assert_eq!(channel_search_poll_interval(0), Duration::from_millis(1500));
        assert_eq!(channel_search_poll_interval(1), Duration::from_millis(1875));
        assert_eq!(channel_search_poll_interval(1000), Duration::from_secs(5));
        for status in [502, 504, 507, 522, 523, 524, 598, 599] {
            assert!(history_retry_status(status));
        }
        for status in [400, 401, 403, 404, 408, 425, 429, 500, 503] {
            assert!(!history_retry_status(status));
        }
        for i in 0..2 {
            let ms = history_retry_delay(i).as_millis();
            let base = 1000_u128 << i;
            assert!(ms >= base * 3 / 4 && ms <= base * 5 / 4);
        }
    }
    #[test]
    fn history_query_omits_absent_cursors_and_preserves_exact_limits() {
        assert_eq!(
            history_path("1", None, None, None, 50),
            "/channels/1/messages?limit=50"
        );
        assert_eq!(
            history_path("1", Some("2"), Some("3"), Some("4"), 100),
            "/channels/1/messages?limit=100&before=2&after=3&around=4"
        );
        assert_eq!(
            history_path("1", None, None, None, 0),
            "/channels/1/messages?limit=0"
        );
    }
    #[test]
    fn indexing_uses_body_predicate_independent_of_status() {
        for status in [200, 202] {
            assert!(
                SearchResponse {
                    status,
                    body: json!({"indexing":true}),
                    retry_after: None
                }
                .is_indexing()
            );
        }
        assert!(
            !SearchResponse {
                status: 202,
                body: json!({"messages":[]}),
                retry_after: None
            }
            .is_indexing()
        );
    }
    #[test]
    fn response_hints_preserve_header_priority_without_implicit_sleep() {
        let mut headers = header::HeaderMap::new();
        headers.insert(header::RETRY_AFTER, "3.25".parse().unwrap());
        headers.insert("x-ratelimit-reset-after", "10".parse().unwrap());
        assert_eq!(
            response_retry_after(&headers),
            Some(Duration::from_millis(10))
        );
        headers.remove("x-ratelimit-reset-after");
        headers.insert(
            header::RETRY_AFTER,
            "Thu, 01 Jan 1970 00:00:00 GMT".parse().unwrap(),
        );
        assert_eq!(response_retry_after(&headers), Some(Duration::ZERO));
    }
}
