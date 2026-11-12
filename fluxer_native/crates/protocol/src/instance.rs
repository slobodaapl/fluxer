use crate::{Error, Result};
use reqwest::{Client, redirect::Policy};
use serde_json::Value;
use std::time::Duration;
use url::Url;

const DISCOVERY_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Instance {
    pub api: Url,
    pub gateway: Url,
    pub api_version: u32,
    pub features: Value,
    pub document: Value,
}

impl Instance {
    /// Discover without credentials or redirects. Both API declarations must
    /// match the serving origin, as required by the existing web client.
    pub async fn discover(input: &str) -> Result<Self> {
        let candidates = discovery_candidates(input)?;
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(15))
            .build()?;
        let mut last_error = String::from("no discovery candidates");
        for candidate in candidates {
            let attempt = async {
                let response = client
                    .get(candidate.clone())
                    .header("Accept", "application/json")
                    .send()
                    .await?;
                if !response.status().is_success() {
                    return Err(Error::Discovery(format!("HTTP {}", response.status())));
                }
                let content_type = response
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                let essence = content_type
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                if essence != "application/json" && !essence.ends_with("+json") {
                    return Err(Error::Discovery("discovery response is not JSON".into()));
                }
                let body = bounded_body(response, DISCOVERY_LIMIT).await?;
                Self::from_document(&candidate, serde_json::from_slice(&body)?)
            }
            .await;
            match attempt {
                Ok(instance) => return Ok(instance),
                Err(error @ Error::UnsupportedGeneration(_)) => return Err(error),
                Err(error) => last_error = error.to_string(),
            }
        }
        Err(Error::Discovery(last_error))
    }

    pub fn from_document(served_by: &Url, document: Value) -> Result<Self> {
        if let Some(codename) = document.get("codename").and_then(Value::as_str) {
            if codename != "tungsten" {
                return Err(Error::UnsupportedGeneration(codename.into()));
            }
        }
        let endpoints = document
            .get("endpoints")
            .ok_or_else(|| Error::Discovery("missing endpoints".into()))?;
        let api = endpoint(
            endpoints.get("api").and_then(Value::as_str).unwrap_or(""),
            false,
        )?;
        let client_api = endpoints
            .get("api_client")
            .and_then(Value::as_str)
            .map(|value| endpoint(value, false))
            .transpose()?;
        if api.origin() != served_by.origin()
            || client_api
                .as_ref()
                .is_some_and(|url| url.origin() != served_by.origin())
        {
            return Err(Error::Discovery(
                "declared API origin differs from discovery origin".into(),
            ));
        }
        let gateway = endpoint(
            endpoints
                .get("gateway")
                .and_then(Value::as_str)
                .unwrap_or(""),
            true,
        )?;
        let api_version = match document.get("api_code_version") {
            None | Some(Value::Null) => 1,
            Some(value) => value
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())
                .filter(|v| *v > 0)
                .ok_or_else(|| Error::Discovery("invalid api_code_version".into()))?,
        };
        Ok(Self {
            api: client_api.unwrap_or(api),
            gateway,
            api_version,
            features: document.get("features").cloned().unwrap_or_default(),
            document,
        })
    }

    /// Explicit endpoints for local development and test servers.
    pub fn from_endpoints(api: &str, gateway: &str) -> Result<Self> {
        Ok(Self {
            api: endpoint(api, false)?,
            gateway: endpoint(gateway, true)?,
            api_version: 1,
            features: Value::Null,
            document: Value::Null,
        })
    }
}

pub(crate) fn endpoint(value: &str, socket: bool) -> Result<Url> {
    let mut url = Url::parse(value.trim())
        .map_err(|_| Error::InvalidEndpoint("expected an absolute URL".into()))?;
    let allowed = if socket {
        ["ws", "wss"]
    } else {
        ["http", "https"]
    };
    if !allowed.contains(&url.scheme())
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::InvalidEndpoint(
            "unsupported scheme, credentials, query or fragment".into(),
        ));
    }
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&path);
    Ok(url)
}

pub(crate) fn discovery_candidates(input: &str) -> Result<Vec<Url>> {
    let base = endpoint(input, false)?;
    let mut paths = vec!["/.well-known/fluxer".to_owned()];
    let carried = base.path().trim_end_matches('/');
    if !carried.is_empty() {
        paths.push(format!("{carried}/.well-known/fluxer"));
    }
    paths.push("/api/.well-known/fluxer".into());
    let mut candidates = Vec::new();
    for path in paths {
        let mut candidate = base.clone();
        candidate.set_path(&path);
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    Ok(candidates)
}

pub(crate) async fn bounded_body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|size| size > limit as u64)
    {
        return Err(Error::ResponseTooLarge(limit));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(Error::ResponseTooLarge(limit));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn candidate_paths_are_ordered_and_deduplicated() {
        let paths: Vec<_> = discovery_candidates("https://chat.example/api/")
            .unwrap()
            .into_iter()
            .map(|u| u.path().to_owned())
            .collect();
        assert_eq!(paths, ["/.well-known/fluxer", "/api/.well-known/fluxer"]);
    }

    #[test]
    fn discovery_rejects_cross_origin_and_unknown_generation() {
        let origin = Url::parse("https://chat.example/.well-known/fluxer").unwrap();
        let document = json!({"endpoints":{"api":"https://evil.example/api","gateway":"wss://chat.example/gateway"}});
        assert!(matches!(
            Instance::from_document(&origin, document),
            Err(Error::Discovery(_))
        ));
        assert!(matches!(
            Instance::from_document(&origin, json!({"codename":"new-generation"})),
            Err(Error::UnsupportedGeneration(_))
        ));
    }

    #[test]
    fn credentials_and_queries_cannot_be_endpoint_configuration() {
        for url in [
            "https://user:secret@chat.example",
            "https://chat.example?token=secret",
            "file:///tmp/socket",
        ] {
            assert!(endpoint(url, false).is_err());
        }
    }
}
