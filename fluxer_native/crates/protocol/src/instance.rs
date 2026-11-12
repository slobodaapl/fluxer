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
    pub web_app: Url,
    pub api_version: u64,
    pub features: Value,
    pub document: Value,
}

impl Instance {
    /// Credential-free discovery with the desktop candidate order and at most
    /// one follow of an explicitly declared API origin. This is not an HTTP
    /// redirect: the declared endpoint must serve its own valid discovery.
    pub async fn discover(input: &str) -> Result<Self> {
        let client = Client::builder()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(15))
            .build()?;
        let mut selected = input.to_owned();
        for followed in [false, true] {
            let mut declared = None;
            let mut last_error = String::from("no discovery candidates");
            for candidate in discovery_candidates(&selected)? {
                let attempt = async {
                    let document = fetch_document(&client, &candidate).await?;
                    // DesktopRuntimeDiscovery.ts:204-212 remembers the first
                    // mismatch but still tries every candidate on this origin.
                    if let Some(endpoint) = declared_elsewhere(&candidate, &document)? {
                        return Ok(Err(endpoint));
                    }
                    Self::from_document(&candidate, document).map(Ok)
                }
                .await;
                match attempt {
                    Ok(Ok(instance)) => return Ok(instance),
                    Ok(Err(endpoint)) => {
                        last_error = "declared API origin differs from discovery origin".into();
                        declared.get_or_insert(endpoint);
                    }
                    Err(error) => last_error = error.to_string(),
                }
            }
            if !followed && let Some(endpoint) = declared {
                selected = endpoint.to_string();
                continue;
            }
            return Err(Error::Discovery(last_error));
        }
        unreachable!("second discovery attempt returns instead of following")
    }

    pub fn from_document(served_by: &Url, document: Value) -> Result<Self> {
        validate_generation(&document)?;
        if declared_elsewhere(served_by, &document)?.is_some() {
            return Err(Error::Discovery(
                "declared API origin differs from discovery origin".into(),
            ));
        }
        let endpoints = document
            .get("endpoints")
            .and_then(Value::as_object)
            .ok_or_else(|| Error::Discovery("missing endpoints".into()))?;
        // ClientRuntimeDiscovery.ts:82-98 requires every runtime endpoint below;
        // optional docs/upload_relay values do not get inferred from API paths.
        for name in [
            "api",
            "api_client",
            "api_public",
            "media",
            "static_cdn",
            "marketing",
            "admin",
            "invite",
            "gift",
        ] {
            required_endpoint(endpoints, name, false)?;
        }
        let api = required_endpoint(endpoints, "api_client", false)?;
        let gateway = required_endpoint(endpoints, "gateway", true)?;
        let web_app = required_endpoint(endpoints, "webapp", false)?;
        if !web_app.path().trim_end_matches('/').is_empty() {
            return Err(Error::Discovery(
                "endpoints.webapp must be an HTTP origin without a path".into(),
            ));
        }
        // projectClientRuntimeDiscovery requires this value: a missing version
        // never implies v1. Match JavaScript's positive safe integer bound.
        let api_version = document
            .get("api_code_version")
            .and_then(Value::as_f64)
            .filter(|value| {
                value.is_finite()
                    && value.fract() == 0.0
                    && *value >= 1.0
                    && *value <= 9_007_199_254_740_991.0
            })
            .map(|value| value as u64)
            .ok_or_else(|| Error::Discovery("missing or invalid api_code_version".into()))?;
        Ok(Self {
            api,
            gateway,
            web_app,
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
            // This explicit local-test helper configures a same-origin web app.
            // Discovery never substitutes the API for a missing webapp endpoint.
            web_app: endpoint(api, false)?,
            api_version: 1,
            features: Value::Null,
            document: Value::Null,
        })
    }
}

pub(crate) fn endpoint(value: &str, socket: bool) -> Result<Url> {
    let value = value.trim();
    if value.is_empty() || value.len() > 8192 || value.starts_with('/') {
        return Err(Error::InvalidEndpoint(
            "expected a nonempty network endpoint".into(),
        ));
    }
    // NetworkOrigin.ts:78-83 uses HTTPS for a scheme-less network authority.
    let candidate = if value.contains("://") {
        value.to_owned()
    } else {
        format!("https://{value}")
    };
    let mut url = Url::parse(&candidate)
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
        || url.port() == Some(0)
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
    let mut base = endpoint(input, false)?;
    // OfficialInstance.ts:51-65 recognizes only these /api aliases. Marketing
    // hosts and bare roots use actual discovery; they are not rewritten.
    if base.scheme() == "https"
        && base.port().is_none()
        && base.path().trim_end_matches('/') == "/api"
    {
        let target = match base.host_str() {
            Some("web.fluxer.app" | "fluxer.com") => Some("https://web.fluxer.app/api"),
            Some("web.canary.fluxer.app" | "canary.fluxer.com") => {
                Some("https://web.canary.fluxer.app/api")
            }
            _ => None,
        };
        if let Some(target) = target {
            base = Url::parse(target).expect("static official API URL");
        }
    }
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

fn validate_generation(document: &Value) -> Result<()> {
    if !document.is_object() {
        return Err(Error::Discovery(
            "discovery response is not an object".into(),
        ));
    }
    // Discovery.ts:503-515 treats an absent/empty codename as legacy and rejects
    // any nonempty unknown codename, independently of the code version.
    if let Some(codename) = document
        .get("codename")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        && codename != "tungsten"
    {
        return Err(Error::UnsupportedGeneration(codename.into()));
    }
    Ok(())
}

fn required_endpoint(
    endpoints: &serde_json::Map<String, Value>,
    name: &str,
    socket: bool,
) -> Result<Url> {
    endpoints
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Discovery(format!("missing endpoints.{name}")))
        .and_then(|value| endpoint(value, socket))
}

fn declared_elsewhere(served_by: &Url, document: &Value) -> Result<Option<Url>> {
    validate_generation(document)?;
    let endpoints = document
        .get("endpoints")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::Discovery("missing endpoints".into()))?;
    let api = required_endpoint(endpoints, "api", false)?;
    // parseInstanceDiscoveryDocument requires API and gateway before checking
    // declared origins; client_api is nullable until runtime projection.
    required_endpoint(endpoints, "gateway", true)?;
    if api.origin() != served_by.origin() {
        return Ok(Some(api));
    }
    if let Some(client_api) = endpoints
        .get("api_client")
        .and_then(Value::as_str)
        .and_then(|value| endpoint(value, false).ok())
        && client_api.origin() != served_by.origin()
    {
        return Ok(Some(client_api));
    }
    Ok(None)
}

async fn fetch_document(client: &Client, candidate: &Url) -> Result<Value> {
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
    Ok(serde_json::from_slice(
        &bounded_body(response, DISCOVERY_LIMIT).await?,
    )?)
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

    #[test]
    fn official_aliases_match_official_instance_exact_path_and_host_rules() {
        for (host, expected) in [
            ("web.fluxer.app", "web.fluxer.app"),
            ("fluxer.com", "web.fluxer.app"),
            ("web.canary.fluxer.app", "web.canary.fluxer.app"),
            ("canary.fluxer.com", "web.canary.fluxer.app"),
        ] {
            let urls = discovery_candidates(&format!("https://{host}/api/")).unwrap();
            assert!(urls.iter().all(|url| url.host_str() == Some(expected)));
        }
        for input in [
            "https://fluxer.app",
            "https://api.fluxer.app/api",
            "https://fluxer.com",
            "https://canary.fluxer.app/api",
        ] {
            let source = Url::parse(input).unwrap();
            assert!(
                discovery_candidates(input)
                    .unwrap()
                    .iter()
                    .all(|url| url.origin() == source.origin())
            );
        }
    }

    // Runtime endpoint fixture mirrors the required endpoint keys from
    // packages/instance_bootstrap/src/__tests__/DiscoveryFixtures.ts and
    // ClientRuntimeDiscovery.ts:82-98. Feature projection is outside this type.
    fn runtime_document(origin: &str) -> Value {
        let mut gateway = Url::parse(origin).unwrap();
        gateway
            .set_scheme(if origin.starts_with("https:") {
                "wss"
            } else {
                "ws"
            })
            .unwrap();
        gateway.set_path("/gateway");
        json!({"api_code_version":1,"endpoints":{
            "api":format!("{origin}/api"), "api_client":format!("{origin}/api"),
            "api_public":format!("{origin}/api"), "gateway":gateway.as_str(),
            "media":format!("{origin}/media"), "static_cdn":origin,
            "marketing":format!("{origin}/marketing"), "admin":format!("{origin}/admin"),
            "invite":format!("{origin}/invite"), "gift":format!("{origin}/gift"), "webapp":origin
        }})
    }

    #[test]
    fn discovery_document_requires_declared_version_without_a_v1_fallback() {
        let origin = Url::parse("https://chat.example/.well-known/fluxer").unwrap();
        let valid = runtime_document("https://chat.example");
        assert_eq!(
            Instance::from_document(&origin, valid.clone())
                .unwrap()
                .api_version,
            1
        );
        for version in [Value::Null, json!(0), json!(-1), json!(1.5), json!("1")] {
            let mut document = valid.clone();
            document["api_code_version"] = version;
            assert!(Instance::from_document(&origin, document).is_err());
        }
        let mut absent = valid.clone();
        absent.as_object_mut().unwrap().remove("api_code_version");
        assert!(Instance::from_document(&origin, absent).is_err());
        let mut numeric = valid;
        numeric["api_code_version"] = json!(2.0);
        numeric["codename"] = json!("");
        assert_eq!(
            Instance::from_document(&origin, numeric)
                .unwrap()
                .api_version,
            2
        );
        let mut boundary = runtime_document("https://chat.example");
        boundary["api_code_version"] = json!(9_007_199_254_740_991_u64);
        assert_eq!(
            Instance::from_document(&origin, boundary.clone())
                .unwrap()
                .api_version,
            9_007_199_254_740_991
        );
        boundary["api_code_version"] = json!(9_007_199_254_740_992_u64);
        assert!(Instance::from_document(&origin, boundary).is_err());
    }

    #[test]
    fn discovery_document_validates_every_required_endpoint_and_webapp_origin() {
        let origin = Url::parse("https://chat.example/.well-known/fluxer").unwrap();
        let valid = runtime_document("https://chat.example");
        for key in [
            "api",
            "api_client",
            "api_public",
            "gateway",
            "media",
            "static_cdn",
            "marketing",
            "admin",
            "invite",
            "gift",
            "webapp",
        ] {
            let mut document = valid.clone();
            document["endpoints"].as_object_mut().unwrap().remove(key);
            assert!(Instance::from_document(&origin, document).is_err(), "{key}");
        }
        let mut path_webapp = valid.clone();
        path_webapp["endpoints"]["webapp"] = json!("https://chat.example/app");
        assert!(Instance::from_document(&origin, path_webapp).is_err());
        let mut declared = valid;
        declared["endpoints"]["api_client"] = json!("https://chat.example/client/api");
        declared["endpoints"]["webapp"] = json!("https://frontend.example");
        let instance = Instance::from_document(&origin, declared).unwrap();
        assert_eq!(instance.api.path(), "/client/api");
        assert_eq!(instance.web_app.host_str(), Some("frontend.example"));
    }

    #[test]
    fn discovery_document_referral_uses_first_mismatched_api_declaration() {
        let origin = Url::parse("https://entry.example/.well-known/fluxer").unwrap();
        let document = runtime_document("https://declared.example");
        assert_eq!(
            declared_elsewhere(&origin, &document)
                .unwrap()
                .unwrap()
                .as_str(),
            "https://declared.example/api"
        );
        let mut document = runtime_document("https://entry.example");
        document["endpoints"]["api_client"] = json!("https://client.example/client-api");
        assert_eq!(
            declared_elsewhere(&origin, &document)
                .unwrap()
                .unwrap()
                .host_str(),
            Some("client.example")
        );
    }

    #[test]
    fn discovery_document_endpoint_normalization_matches_network_origin() {
        assert_eq!(
            endpoint(" chat.example/api/// ", false).unwrap().as_str(),
            "https://chat.example/api"
        );
        for input in [
            "https://chat.example:0",
            "//chat.example",
            "/api",
            "https://chat.example/api?",
            "https://chat.example/#fragment",
        ] {
            assert!(endpoint(input, false).is_err(), "{input}");
        }
    }

    async fn server<F>(
        handler: F,
    ) -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        tokio::task::JoinHandle<()>,
    )
    where
        F: Fn(&str) -> Value + Send + Sync + 'static,
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = vec![0; 8192];
                let mut used = 0;
                loop {
                    let n = socket.read(&mut bytes[used..]).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    used += n;
                    if bytes[..used].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&bytes[..used]);
                assert!(!request.to_ascii_lowercase().contains("authorization:"));
                let path = request.split_whitespace().nth(1).unwrap();
                seen.lock().unwrap().push(path.to_owned());
                let body = handler(path).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (origin, requests, task)
    }

    #[tokio::test]
    async fn discovery_socket_follows_declared_endpoint_only_once() {
        let (third, third_requests, third_task) = server(|_| json!({})).await;
        let third_document = runtime_document(&third);
        let (second, second_requests, second_task) = server(move |_| third_document.clone()).await;
        let second_document = runtime_document(&second);
        let (first, first_requests, first_task) = server(move |_| second_document.clone()).await;
        assert!(Instance::discover(&first).await.is_err());
        assert_eq!(first_requests.lock().unwrap().len(), 2);
        assert_eq!(second_requests.lock().unwrap().len(), 2);
        assert!(third_requests.lock().unwrap().is_empty());
        for task in [first_task, second_task, third_task] {
            task.abort();
        }
    }

    #[tokio::test]
    async fn discovery_socket_revalidates_the_declared_origin() {
        let document = std::sync::Arc::new(std::sync::Mutex::new(Value::Null));
        let served = document.clone();
        let (canonical, canonical_requests, canonical_task) =
            server(move |_| served.lock().unwrap().clone()).await;
        *document.lock().unwrap() = runtime_document(&canonical);
        let canonical_document = runtime_document(&canonical);
        let (entry, entry_requests, entry_task) = server(move |_| canonical_document.clone()).await;
        let instance = Instance::discover(&entry).await.unwrap();
        assert_eq!(instance.api.as_str(), format!("{canonical}/api"));
        assert_eq!(entry_requests.lock().unwrap().len(), 2);
        assert_eq!(canonical_requests.lock().unwrap().len(), 1);
        entry_task.abort();
        canonical_task.abort();
    }

    #[tokio::test]
    async fn discovery_socket_exhausts_local_candidates_before_following() {
        let (other, other_requests, other_task) = server(|_| json!({})).await;
        let other_document = runtime_document(&other);
        let own_document = std::sync::Arc::new(std::sync::Mutex::new(Value::Null));
        let served = own_document.clone();
        let (origin, requests, task) = server(move |path| {
            if path == "/api/.well-known/fluxer" {
                served.lock().unwrap().clone()
            } else {
                other_document.clone()
            }
        })
        .await;
        *own_document.lock().unwrap() = runtime_document(&origin);
        let instance = Instance::discover(&origin).await.unwrap();
        assert_eq!(instance.api.as_str(), format!("{origin}/api"));
        assert_eq!(requests.lock().unwrap().len(), 2);
        assert!(other_requests.lock().unwrap().is_empty());
        task.abort();
        other_task.abort();
    }
}
