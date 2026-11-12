use crate::{Error, Result, SessionToken};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fmt, time::Duration};
use tokio::{
    sync::mpsc,
    time::{Instant, sleep, sleep_until, timeout},
};
use tokio_tungstenite::tungstenite::{Message, protocol::WebSocketConfig};
use url::Url;

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
pub struct VoiceStateUpdate {
    pub guild_id: Option<String>,
    pub channel_id: Option<String>,
    pub self_mute: bool,
    pub self_deaf: bool,
    pub self_video: bool,
    pub self_stream: bool,
    pub connection_id: Option<String>,
    #[serde(default)]
    pub viewer_stream_keys: Vec<String>,
    #[serde(default)]
    pub p2p: bool,
}

impl VoiceStateUpdate {
    pub fn join(guild_id: Option<String>, channel_id: String) -> Self {
        Self {
            guild_id,
            channel_id: Some(channel_id),
            ..Self::default()
        }
    }

    pub fn disconnect() -> Self {
        Self::default()
    }
}

/// LiveKit credentials delivered by VOICE_SERVER_UPDATE, not a REST endpoint.
/// Debug intentionally omits the token and E2EE key.
#[derive(Clone, Deserialize)]
pub struct VoiceServerUpdate {
    #[serde(deserialize_with = "deserialize_token")]
    pub token: SessionToken,
    pub endpoint: String,
    pub connection_id: String,
    pub guild_id: Option<String>,
    pub channel_id: Option<String>,
    pub e2ee_key: Option<String>,
    #[serde(default)]
    pub ice_servers: Vec<Value>,
}

impl fmt::Debug for VoiceServerUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VoiceServerUpdate")
            .field("connection_id", &self.connection_id)
            .field("guild_id", &self.guild_id)
            .field("channel_id", &self.channel_id)
            .finish_non_exhaustive()
    }
}

fn deserialize_token<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<SessionToken, D::Error> {
    SessionToken::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResumeSession {
    pub session_id: Option<String>,
    pub sequence: Option<u64>,
}

pub enum GatewayCommand {
    VoiceState(VoiceStateUpdate),
    Presence(Value),
    Send { op: u8, data: Value },
    Close,
}

#[derive(Clone)]
pub enum GatewayEvent {
    Connecting,
    Connected,
    Reconnecting,
    Dispatch {
        name: String,
        data: Value,
        sequence: Option<u64>,
    },
    Error(String),
    Disconnected,
}

impl fmt::Debug for GatewayEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connecting => f.write_str("Connecting"),
            Self::Connected => f.write_str("Connected"),
            Self::Reconnecting => f.write_str("Reconnecting"),
            Self::Disconnected => f.write_str("Disconnected"),
            Self::Error(error) => f.debug_tuple("Error").field(error).finish(),
            Self::Dispatch { name, sequence, .. } => f
                .debug_struct("Dispatch")
                .field("name", name)
                .field("sequence", sequence)
                .finish_non_exhaustive(),
        }
    }
}

pub struct Gateway {
    url: Url,
    token: SessionToken,
    resume: ResumeSession,
    properties: Value,
    api_version: u32,
}

impl Gateway {
    pub fn new(url: Url, token: SessionToken) -> Self {
        Self {
            url,
            token,
            resume: ResumeSession::default(),
            api_version: 1,
            properties: json!({"os":std::env::consts::OS,"browser":"Fluxer Native","device":"desktop",
                "locale":"en-US","user_agent":concat!("Fluxer-Native/",env!("CARGO_PKG_VERSION")),
                "browser_version":"0","os_version":"","build_version":env!("CARGO_PKG_VERSION"),
                "desktop_app_version":env!("CARGO_PKG_VERSION"),"desktop_arch":std::env::consts::ARCH,
                "desktop_os":std::env::consts::OS,"e2ee_capable":false}),
        }
    }

    pub fn with_resume(mut self, session: ResumeSession) -> Self {
        self.resume = session;
        self
    }
    pub fn with_api_version(mut self, version: u32) -> Self {
        self.api_version = version.max(1);
        self
    }
    pub fn with_properties(mut self, properties: Value) -> Self {
        self.properties = properties;
        self
    }
    /// Advertise only after the caller has an E2EE-capable native media backend.
    pub fn with_e2ee_capable(mut self, capable: bool) -> Self {
        self.properties["e2ee_capable"] = capable.into();
        self
    }
    pub fn resume_session(&self) -> &ResumeSession {
        &self.resume
    }

    fn connect_url(&self) -> Result<Url> {
        let mut url = crate::instance::endpoint(self.url.as_str(), true)?;
        url.query_pairs_mut()
            .append_pair("v", &self.api_version.to_string())
            .append_pair("encoding", "json")
            .append_pair("compress", "none");
        Ok(url)
    }

    fn authentication_payload(&self) -> Value {
        if let (Some(session_id), Some(sequence)) = (&self.resume.session_id, self.resume.sequence)
        {
            json!({"op":6,"d":{"token":self.token.expose_secret(),"session_id":session_id,"seq":sequence}})
        } else {
            json!({"op":2,"d":{"token":self.token.expose_secret(),"properties":self.properties,"flags":14,
                "presence":{"status":"online","afk":false,"mobile":false}}})
        }
    }

    /// Run until Close, command sender drop, event receiver drop, or terminal
    /// authentication failure. Reconnects use capped exponential backoff and
    /// resume the last dispatch successfully delivered to the consumer.
    pub async fn run(
        mut self,
        events: mpsc::Sender<GatewayEvent>,
        mut commands: mpsc::Receiver<GatewayCommand>,
    ) -> Result<()> {
        let url = self.connect_url()?;
        let mut failures = 0u32;
        loop {
            if events.is_closed() || commands.is_closed() {
                break;
            }
            if events
                .send(if failures == 0 {
                    GatewayEvent::Connecting
                } else {
                    GatewayEvent::Reconnecting
                })
                .await
                .is_err()
            {
                break;
            }
            let config = WebSocketConfig::default()
                .max_message_size(Some(16 * 1024 * 1024))
                .max_frame_size(Some(16 * 1024 * 1024));
            let connect = timeout(
                Duration::from_secs(15),
                tokio_tungstenite::connect_async_with_config(url.as_str(), Some(config), false),
            );
            let socket = tokio::select! {
                _ = events.closed() => break,
                command = commands.recv() => {
                    if matches!(command, None | Some(GatewayCommand::Close)) { break; }
                    // Do not replay commands issued while disconnected.
                    continue;
                },
                result = connect => match result {
                    Ok(Ok((socket, _))) => socket,
                    _ => {
                        if events.send(GatewayEvent::Error("gateway connection failed".into())).await.is_err() { break; }
                        failures = failures.saturating_add(1);
                        if wait_retry(failures, &events, &mut commands).await { break; }
                        continue;
                    }
                }
            };
            let (mut writer, mut reader) = socket.split();
            let mut heartbeat_interval = None;
            let mut next_heartbeat = Instant::now() + Duration::from_secs(10);
            let mut awaiting_ack = false;
            let mut authenticated = false;
            let mut terminal = false;
            let mut stop = false;
            loop {
                tokio::select! {
                    _ = events.closed() => { stop = true; break; },
                    _ = sleep_until(next_heartbeat) => {
                        let Some(interval) = heartbeat_interval else { break; };
                        if awaiting_ack { break; }
                        let heartbeat = json!({"op":1,"d":self.resume.sequence});
                        if send(&mut writer, heartbeat).await.is_err() { break; }
                        awaiting_ack = true;
                        next_heartbeat = Instant::now() + interval;
                    },
                    command = commands.recv() => {
                        let payload = match command {
                            Some(GatewayCommand::VoiceState(state)) if authenticated => json!({"op":4,"d":state}),
                            Some(GatewayCommand::Presence(presence)) if authenticated => json!({"op":3,"d":presence}),
                            Some(GatewayCommand::Send { op, data }) if authenticated => {
                                // Authentication, heartbeat and lifecycle opcodes are owned by this connection.
                                if matches!(op, 0..=2 | 6..=7 | 9..=12) { continue; }
                                let payload = json!({"op":op,"d":data});
                                if op == 13 && payload.to_string().len() > 16000 {
                                    let _ = events.send(GatewayEvent::Error("voice signal exceeds wire limit".into())).await;
                                    continue;
                                }
                                payload
                            },
                            None | Some(GatewayCommand::Close) => { stop = true; break; },
                            _ => continue,
                        };
                        if send(&mut writer, payload).await.is_err() { break; }
                    },
                    message = reader.next() => {
                        let payload = match message {
                            Some(Ok(Message::Text(text))) => match serde_json::from_str::<Payload>(&text) { Ok(p) => p, Err(_) => break },
                            Some(Ok(Message::Binary(bytes))) => match serde_json::from_slice::<Payload>(&bytes) { Ok(p) => p, Err(_) => break },
                            Some(Ok(Message::Ping(bytes))) => { if writer.send(Message::Pong(bytes)).await.is_err() { break; } continue; },
                            Some(Ok(Message::Close(frame))) => {
                                if let Some(frame) = frame {
                                    let code: u16 = frame.code.into();
                                    if matches!(code, 4007 | 4009) { self.resume = ResumeSession::default(); }
                                    if matches!(code, 4004 | 4010..=4012) { terminal = true; }
                                }
                                break;
                            },
                            Some(Ok(_)) => continue,
                            _ => break,
                        };
                        match payload.op {
                            10 => {
                                let Some(ms) = payload.d.get("heartbeat_interval").and_then(Value::as_u64).filter(|v| *v > 0 && *v <= 300_000) else { break; };
                                if heartbeat_interval.is_some() { break; }
                                heartbeat_interval = Some(Duration::from_millis(ms));
                                next_heartbeat = Instant::now() + Duration::from_millis(ms / 2);
                                if send(&mut writer, self.authentication_payload()).await.is_err() { break; }
                            },
                            0 => {
                                let Some(name) = payload.t else { break; };
                                // A resumed connection can repeat its last delivered
                                // dispatch. In particular, reactions are deltas and
                                // must not be applied twice.
                                if let (Some(previous), Some(sequence)) = (self.resume.sequence, payload.s)
                                    && sequence <= previous && name != "READY" {
                                    continue;
                                }
                                if name == "READY" {
                                    let Some(session_id) = payload.d.get("session_id").and_then(Value::as_str) else { break; };
                                    self.resume.session_id = Some(session_id.to_owned());
                                }
                                let ready = name == "READY" || name == "RESUMED";
                                let event = GatewayEvent::Dispatch { name, data: payload.d, sequence: payload.s };
                                // A slow consumer reconnects and resumes from the previous delivered sequence.
                                if !matches!(timeout(Duration::from_secs(2), events.send(event)).await, Ok(Ok(()))) { break; }
                                if let Some(sequence) = payload.s { self.resume.sequence = Some(sequence); }
                                if ready {
                                    authenticated = true;
                                    failures = 0;
                                    if events.send(GatewayEvent::Connected).await.is_err() { stop = true; break; }
                                }
                            },
                            1 => {
                                if send(&mut writer, json!({"op":1,"d":self.resume.sequence})).await.is_err() { break; }
                                awaiting_ack = true;
                            },
                            7 => break,
                            9 => { if payload.d != Value::Bool(true) { self.resume = ResumeSession::default(); } break; },
                            11 => awaiting_ack = false,
                            12 => { let _ = events.send(GatewayEvent::Error("gateway rejected a request".into())).await; },
                            _ => {},
                        }
                    }
                }
            }
            let _ = timeout(Duration::from_secs(1), writer.close()).await;
            if stop {
                break;
            }
            if terminal {
                let _ = events.send(GatewayEvent::Disconnected).await;
                return Err(Error::Gateway(
                    "authentication or API version rejected".into(),
                ));
            }
            failures = failures.saturating_add(1);
            if wait_retry(failures, &events, &mut commands).await {
                break;
            }
        }
        let _ = events.send(GatewayEvent::Disconnected).await;
        Ok(())
    }
}

#[derive(Deserialize)]
struct Payload {
    op: u8,
    #[serde(default)]
    d: Value,
    s: Option<u64>,
    t: Option<String>,
}

async fn send<S>(writer: &mut S, payload: Value) -> Result<()>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    timeout(
        Duration::from_secs(5),
        writer.send(Message::Text(payload.to_string().into())),
    )
    .await
    .map_err(|_| Error::Gateway("socket send timed out".into()))??;
    Ok(())
}

async fn wait_retry(
    failures: u32,
    events: &mpsc::Sender<GatewayEvent>,
    commands: &mut mpsc::Receiver<GatewayCommand>,
) -> bool {
    let delay = Duration::from_millis((500u64 << failures.min(6)).min(30_000));
    tokio::select! {
        _ = sleep(delay) => false,
        _ = events.closed() => true,
        command = commands.recv() => matches!(command, None | Some(GatewayCommand::Close)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentication_switches_from_identify_to_resume() {
        let mut gateway = Gateway::new(
            Url::parse("wss://chat.example/gateway").unwrap(),
            SessionToken::new("secret").unwrap(),
        );
        assert_eq!(gateway.authentication_payload()["op"], 2);
        gateway.resume = ResumeSession {
            session_id: Some("session".into()),
            sequence: Some(42),
        };
        let payload = gateway.authentication_payload();
        assert_eq!(payload["op"], 6);
        assert_eq!(payload["d"]["seq"], 42);
        assert_eq!(
            gateway.connect_url().unwrap().query(),
            Some("v=1&encoding=json&compress=none")
        );
    }

    #[test]
    fn voice_credentials_and_dispatch_debug_are_redacted() {
        let payload = json!({"token":"voice-secret","endpoint":"wss://voice.example","connection_id":"c","e2ee_key":"e2ee-secret"});
        let voice: VoiceServerUpdate = serde_json::from_value(payload.clone()).unwrap();
        assert!(!format!("{voice:?}").contains("secret"));
        let event = GatewayEvent::Dispatch {
            name: "VOICE_SERVER_UPDATE".into(),
            data: payload,
            sequence: Some(1),
        };
        assert!(!format!("{event:?}").contains("secret"));
        assert_eq!(
            serde_json::to_value(VoiceStateUpdate::join(
                Some("guild".into()),
                "channel".into()
            ))
            .unwrap()["connection_id"],
            Value::Null
        );
    }
}
