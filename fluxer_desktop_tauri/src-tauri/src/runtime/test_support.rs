// SPDX-License-Identifier: AGPL-3.0-or-later

//! Shared fixtures for the runtime tests: discovery documents, a loopback HTTP responder,
//! in-memory markers and a loaded renderer document.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::discovery::RuntimeDiscovery;
use super::markers::Markers;
use super::plans::{LocalAppRuntimePlan, RuntimeEndpoints, RuntimePlans, trusted_http_origins};
use crate::documents::{Caller, DocumentRegistry};
use crate::local_app::url::origin;
use crate::net::outbound::OutboundHttp;
use crate::window::MAIN_WINDOW_LABEL;

pub const TOKEN_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
pub const TOKEN_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// `SELF_HOSTED_API_TOPOLOGY_DOCUMENT` from packages/instance_bootstrap's DiscoveryFixtures.ts,
/// with every endpoint under `base` (`http://127.0.0.1:<port>`).
pub fn document(base: &str) -> Value {
    let gateway = base.replacen("http://", "ws://", 1);
    json!({
        "api_code_version": 1,
        "endpoints": {
            "api": format!("{base}/api"),
            "api_client": format!("{base}/api"),
            "api_public": format!("{base}/api"),
            "gateway": format!("{gateway}/gateway"),
            "media": format!("{base}/media"),
            "static_cdn": base,
            "marketing": format!("{base}/marketing"),
            "admin": format!("{base}/admin"),
            "invite": format!("{base}/invite"),
            "gift": format!("{base}/gift"),
            "webapp": base
        },
        "captcha": {"provider": "none"},
        "features": {
            "voice_enabled": false,
            "stripe_enabled": false,
            "self_hosted": true,
            "presigned_attachment_uploads": false,
            "emails_enabled": false
        },
        "gif": {"provider": "klipy", "display_name": "Klipy", "attribution_required": false},
        "sso": {
            "enabled": false,
            "enforced": false,
            "display_name": null,
            "button_label": null,
            "redirect_uri": format!("{base}/sso/callback")
        },
        "registration": {"mode": "closed", "admin_registration_urls_enabled": false},
        "community": {"single_community": true, "single_community_guild_id": "1", "direct_messages_disabled": false},
        "services": {"gif_enabled": false, "youtube_enabled": false, "bluesky_enabled": false},
        "limits": {"version": 1, "traitDefinitions": [], "rules": []},
        "push": {"public_vapid_key": null},
        "app_public": {
            "branding": {
                "product_name": "Example Chat",
                "icon_url": null,
                "symbol_url": null,
                "logo_url": null,
                "wordmark_url": null,
                "favicon_url": null,
                "theme_color": null
            },
            "setup": {"configured": true, "admin_url": format!("{base}/admin")},
            "legal": {"terms_url": null, "privacy_url": null},
            "registration": {"collect_date_of_birth": false}
        }
    })
}

pub fn outbound() -> Arc<OutboundHttp> {
    Arc::new(OutboundHttp::new(
        crate::net::trust::client_config().unwrap(),
    ))
}

/// Markers in a map, counting reads.
#[derive(Default)]
pub struct MemoryMarkers {
    pub values: Mutex<HashMap<String, String>>,
    pub reads: AtomicUsize,
}

impl MemoryMarkers {
    pub fn markers(self: &Arc<Self>) -> Markers {
        Markers::over(
            self.clone(),
            |store, key| async move {
                store.reads.fetch_add(1, Ordering::SeqCst);
                Ok(store.values.lock().unwrap().get(&key).cloned())
            },
            |store, key, value| async move {
                store.values.lock().unwrap().insert(key, value);
                Ok(())
            },
        )
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.values.lock().unwrap().get(key).cloned()
    }

    /// Waits for the background marker write of `key`.
    pub async fn written(&self, key: &str) -> Value {
        for _ in 0..200 {
            if let Some(value) = self.get(key) {
                return serde_json::from_str(&value).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("marker {key} was never written");
    }
}

pub fn plans_with(outbound: Arc<OutboundHttp>, markers: Markers) -> Arc<RuntimePlans> {
    Arc::new(RuntimePlans::new(Arc::new(RuntimeDiscovery::new(
        outbound, markers,
    ))))
}

/// A plan whose every endpoint is on `http://127.0.0.1:<port>`.
pub fn loopback_plan(port: u16) -> LocalAppRuntimePlan {
    let base = format!("http://127.0.0.1:{port}");
    LocalAppRuntimePlan {
        instance_key: format!("{base}/api"),
        document: document(&base),
        endpoints: RuntimeEndpoints {
            api: format!("{base}/api"),
            api_public: format!("{base}/api"),
            webapp: base.clone(),
            media: format!("{base}/media"),
            static_cdn: base.clone(),
            upload_relay: None,
            gateway: format!("ws://127.0.0.1:{port}/gateway"),
        },
    }
}

/// Registers the plan's origins (IP literals, so no DNS) and caches it.
pub async fn cached_plan(plans: &RuntimePlans, port: u16) -> Arc<LocalAppRuntimePlan> {
    let plan = Arc::new(loopback_plan(port));
    plans
        .discovery()
        .outbound
        .register_anchored_origins(
            &format!("http://127.0.0.1:{port}"),
            &trusted_http_origins(&plan),
            None,
        )
        .await
        .unwrap();
    plans.cache(plan.clone()).unwrap();
    plan
}

/// The main window showing a loaded renderer document registered with `TOKEN_A`.
pub fn loaded_documents() -> Arc<DocumentRegistry> {
    let documents = Arc::new(DocumentRegistry::default());
    documents.navigation_started(MAIN_WINDOW_LABEL);
    documents.register(MAIN_WINDOW_LABEL, TOKEN_A).unwrap();
    documents.page_loaded(MAIN_WINDOW_LABEL);
    documents
}

pub fn caller(token: &str) -> Caller {
    Caller::for_test(
        MAIN_WINDOW_LABEL,
        &format!("{}/channels/@me", origin()),
        token,
    )
}

/// Replaces the main window's document with a new one registered with `token`.
pub fn navigate(documents: &DocumentRegistry, token: &str) {
    documents.navigation_started(MAIN_WINDOW_LABEL);
    documents.register(MAIN_WINDOW_LABEL, token).unwrap();
    documents.page_loaded(MAIN_WINDOW_LABEL);
}

#[derive(Clone)]
pub enum Reply {
    /// Status, headers and body; `Content-Length` is added unless a header sets it.
    Fixed(u16, Vec<(&'static str, String)>, Vec<u8>),
    /// A 200 JSON response whose body ends when the connection closes.
    CloseDelimited(Vec<u8>),
    /// Never answers.
    Hang,
}

impl Reply {
    pub fn json(value: &Value) -> Self {
        Self::Fixed(
            200,
            vec![("Content-Type", "application/json".to_owned())],
            value.to_string().into_bytes(),
        )
    }

    pub fn status(status: u16) -> Self {
        Self::Fixed(
            status,
            vec![("Content-Type", "text/plain".to_owned())],
            b"no".to_vec(),
        )
    }
}

/// A minimal HTTP/1.1 responder on an ephemeral loopback port. Unrouted paths get 404.
#[derive(Clone)]
pub struct TestServer {
    pub base: String,
    routes: Arc<Mutex<HashMap<String, Reply>>>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl TestServer {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = Self {
            base: format!("http://{}", listener.local_addr().unwrap()),
            routes: Arc::default(),
            requests: Arc::default(),
        };
        let accepting = server.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(accepting.clone().serve(stream));
            }
        });
        server
    }

    pub fn api_endpoint(&self) -> String {
        format!("{}/api", self.base)
    }

    pub fn route(&self, path: &str, reply: Reply) {
        self.routes.lock().unwrap().insert(path.to_owned(), reply);
    }

    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    async fn serve(self, mut stream: TcpStream) {
        let mut head = Vec::new();
        let mut buffer = [0u8; 1024];
        while !head.windows(4).any(|window| window == b"\r\n\r\n") {
            match stream.read(&mut buffer).await {
                Ok(0) | Err(_) => return,
                Ok(read) => head.extend_from_slice(&buffer[..read]),
            }
        }
        let head = String::from_utf8_lossy(&head).into_owned();
        let path = head
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_owned();
        self.requests.lock().unwrap().push(path.clone());
        let reply = self
            .routes
            .lock()
            .unwrap()
            .get(&path)
            .cloned()
            .unwrap_or_else(|| Reply::status(404));
        let (status, headers, body, close_delimited) = match reply {
            Reply::Hang => {
                std::future::pending::<()>().await;
                return;
            }
            Reply::Fixed(status, headers, body) => (status, headers, body, false),
            Reply::CloseDelimited(body) => (
                200,
                vec![("Content-Type", "application/json".to_owned())],
                body,
                true,
            ),
        };
        let mut response = format!("HTTP/1.1 {status} Test\r\nConnection: close\r\n");
        for (name, value) in &headers {
            response.push_str(&format!("{name}: {value}\r\n"));
        }
        let sets_length = headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("content-length"));
        if !close_delimited && !sets_length {
            response.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        response.push_str("\r\n");
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.write_all(&body).await;
        let _ = stream.shutdown().await;
    }
}
