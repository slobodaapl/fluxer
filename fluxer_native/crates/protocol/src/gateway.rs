use crate::{Error, Result, SessionToken};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fmt, time::Duration};
use tokio::{
    sync::mpsc,
    time::{Instant, sleep_until, timeout},
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
    #[serde(default)]
    pub is_mobile: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latitude: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub longitude: Option<String>,
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
    AuthenticationFailed,
    Disconnected,
}

impl fmt::Debug for GatewayEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connecting => f.write_str("Connecting"),
            Self::Connected => f.write_str("Connected"),
            Self::Reconnecting => f.write_str("Reconnecting"),
            Self::Disconnected => f.write_str("Disconnected"),
            Self::AuthenticationFailed => f.write_str("AuthenticationFailed"),
            Self::Error(error) => f.debug_tuple("Error").field(error).finish(),
            Self::Dispatch { name, sequence, .. } => f
                .debug_struct("Dispatch")
                .field("name", name)
                .field("sequence", sequence)
                .finish_non_exhaustive(),
        }
    }
}

// Use the same CCtx/DCtx implementation as the Electron renderer's WASM
// codec. In particular each message flushes, rather than finishing, its frame.
#[path = "../../../../fluxer_app/rust/libfluxcore/src/zstd_stream.rs"]
mod zstd_stream;

const HELLO_TIMEOUT: Duration = Duration::from_secs(20);
const ACK_TIMEOUT: Duration = Duration::from_secs(15);
const RESUME_TIMEOUT: Duration = Duration::from_secs(15);
const RESUME_WINDOW: Duration = Duration::from_secs(180);
const MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
const IDENTIFY_FLAGS: u32 = 2 | 4 | 8;

pub struct Gateway {
    url: Url,
    token: SessionToken,
    resume: ResumeSession,
    properties: Value,
    presence: Option<Value>,
    initial_guild_id: Option<String>,
    api_version: u32,
    compression: bool,
    last_activity: Option<Instant>,
}

impl Gateway {
    pub fn new(url: Url, token: SessionToken) -> Self {
        Self {
            url,
            token,
            resume: ResumeSession::default(),
            api_version: 1,
            properties: native_properties(),
            presence: Some(json!({"status":"online","afk":false,"mobile":false})),
            initial_guild_id: None,
            compression: true,
            last_activity: None,
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
    /// Property names follow ClientInfo.getGatewayClientProperties; values must
    /// describe this native executable, not an invented Electron installation.
    pub fn with_properties(mut self, properties: Value) -> Self {
        self.properties = properties;
        self
    }
    /// UI locale can differ from the operating system locale, as in ClientInfo.
    pub fn with_locale(mut self, locale: impl Into<String>) -> Self {
        let locale = locale.into();
        if !locale.is_empty() {
            self.properties["locale"] = locale.into();
        }
        self
    }
    pub fn with_presence(mut self, presence: Value) -> Self {
        self.presence = (!presence.is_null()).then_some(presence);
        self
    }
    pub fn with_initial_guild(mut self, guild: Option<String>) -> Self {
        self.initial_guild_id = guild.filter(|id| !id.is_empty());
        self
    }
    /// Normally enabled. The existing client retries without compression only
    /// when its zstd codec fails; this override also supports transport probes.
    pub fn with_compression_enabled(mut self, enabled: bool) -> Self {
        self.compression = enabled;
        self
    }
    pub fn with_e2ee_capable(mut self, capable: bool) -> Self {
        self.properties["e2ee_capable"] = capable.into();
        self
    }
    pub fn resume_session(&self) -> &ResumeSession {
        &self.resume
    }
    fn clear_session(&mut self) {
        self.resume = ResumeSession::default();
        self.last_activity = None;
    }
    fn connect_url(&self) -> Result<Url> {
        let mut url = crate::instance::endpoint(self.url.as_str(), true)?;
        url.query_pairs_mut()
            .append_pair("v", &self.api_version.to_string())
            .append_pair("encoding", "json")
            .append_pair(
                "compress",
                if self.compression {
                    "zstd-stream"
                } else {
                    "none"
                },
            );
        if self.compression {
            url.query_pairs_mut().append_pair("stream", "1");
        }
        Ok(url)
    }
    fn authentication_payload(&self) -> Value {
        if let Some(session) = &self.resume.session_id {
            json!({"op":6,"d":{"token":self.token.expose_secret(),"session_id":session,"seq":self.resume.sequence.unwrap_or(0)}})
        } else {
            let mut data = json!({"token":self.token.expose_secret(),"properties":self.properties,"flags":IDENTIFY_FLAGS});
            if let Some(presence) = &self.presence {
                data["presence"] = presence.clone();
            }
            if let Some(guild) = &self.initial_guild_id {
                data["initial_guild_id"] = guild.clone().into();
            }
            json!({"op":2,"d":data})
        }
    }

    /// Runs one account's connection and retains the existing desktop client's
    /// zstd dictionaries for exactly one WebSocket lifetime. A new socket always
    /// gets fresh dictionaries, whether it IDENTIFYs or RESUMEs.
    pub async fn run(
        mut self,
        events: mpsc::Sender<GatewayEvent>,
        mut commands: mpsc::Receiver<GatewayCommand>,
    ) -> Result<()> {
        let mut failures = 0u32;
        let mut backoff = ReconnectBackoff::default();
        loop {
            if events.is_closed() || commands.is_closed() {
                break;
            }
            if self
                .last_activity
                .is_some_and(|at| at.elapsed() > RESUME_WINDOW)
            {
                self.clear_session();
            }
            let url = self.connect_url()?;
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
                .max_message_size(Some(MAX_PAYLOAD_BYTES))
                .max_frame_size(Some(MAX_PAYLOAD_BYTES));
            let connect = timeout(
                HELLO_TIMEOUT,
                tokio_tungstenite::connect_async_with_config(url.as_str(), Some(config), false),
            );
            let socket = tokio::select! {
                _=events.closed()=>break,
                command=commands.recv()=>{
                    match command {
                        None|Some(GatewayCommand::Close)=>break,
                        Some(GatewayCommand::Presence(presence))=>self.presence=Some(presence),
                        _=>{},
                    }
                    continue;
                },
                result=connect=>match result {
                    Ok(Ok((socket,_)))=>socket,
                    _=>{let _=events.send(GatewayEvent::Error("gateway connection failed".into())).await;
                        failures=failures.saturating_add(1);if wait_retry(backoff.next_delay(false),&events,&mut commands,&mut self.presence).await{break;}continue;},
                }
            };
            let mut codec = match WireCodec::new(self.compression) {
                Ok(codec) => codec,
                Err(_) => {
                    self.compression = false;
                    failures = failures.saturating_add(1);
                    continue;
                }
            };
            let (mut writer, mut reader) = socket.split();
            let mut heartbeat_interval = None;
            let mut hello_deadline = Some(Instant::now() + HELLO_TIMEOUT);
            let mut heartbeat_at = None;
            let mut ack_deadline = None;
            let mut resume_deadline = None;
            let mut reidentify_at = None;
            let mut invalid_session_at = None;
            let mut authenticated = false;
            let mut terminal = false;
            let mut stop = false;
            let mut fallback_compression = false;
            let mut immediate_retry = false;
            loop {
                tokio::select! {
                    _=events.closed()=>{stop=true;break;},
                    _=deadline(hello_deadline)=>break,
                    _=deadline(ack_deadline)=>{immediate_retry=true;break;},
                    _=deadline(resume_deadline)=>{self.clear_session();break;},
                    _=deadline(invalid_session_at)=>{immediate_retry=true;break;},
                    _=deadline(reidentify_at)=>{
                        reidentify_at=None;
                        if transmit(&mut writer,self.authentication_payload(),&mut codec).await.is_err(){break;}
                    },
                    _=deadline(heartbeat_at)=>{
                        let Some(interval)=heartbeat_interval else{break;};
                        // A server-requested heartbeat can come just before this
                        // tick. Keep its independent 15s ACK deadline instead of
                        // treating a short interval as a missed acknowledgement.
                        if ack_deadline.is_none(){
                            if transmit(&mut writer,json!({"op":1,"d":self.resume.sequence.unwrap_or(0)}),&mut codec).await.is_err(){break;}
                            self.last_activity=Some(Instant::now());ack_deadline=Some(Instant::now()+ACK_TIMEOUT);
                        }
                        heartbeat_at=Some(Instant::now()+heartbeat_delay(interval));
                    },
                    command=commands.recv()=>{
                        let payload=match command {
                            Some(GatewayCommand::Presence(presence))=>{self.presence=Some(presence.clone());if !authenticated{continue;}json!({"op":3,"d":presence})},
                            Some(GatewayCommand::VoiceState(state)) if authenticated=>json!({"op":4,"d":state}),
                            Some(GatewayCommand::Send{op,data}) if authenticated=>{
                                if matches!(op,0..=2|6..=7|9..=12){continue;}
                                let payload=json!({"op":op,"d":data});
                                if op==13&&payload.to_string().len()>16000{let _=events.send(GatewayEvent::Error("voice signal exceeds wire limit".into())).await;continue;}
                                payload
                            },
                            None|Some(GatewayCommand::Close)=>{stop=true;break;},
                            _=>continue,
                        };
                        if transmit(&mut writer,payload,&mut codec).await.is_err(){break;}
                    },
                    message=reader.next()=>{
                        let payload=match message {
                            Some(Ok(Message::Text(text)))=>match serde_json::from_str::<Payload>(&text){Ok(payload)=>payload,Err(_)=>{terminal=true;let _=events.send(GatewayEvent::Error("gateway sent invalid JSON".into())).await;break;}},
                            Some(Ok(Message::Binary(bytes)))=>{
                                let decoded=match codec.decode(&bytes){Ok(decoded)=>decoded,Err(_)=>{fallback_compression=self.compression;break;}};
                                if decoded.is_empty(){continue;}
                                match serde_json::from_slice::<Payload>(&decoded){Ok(payload)=>payload,Err(_)=>{terminal=true;let _=events.send(GatewayEvent::Error("gateway sent invalid JSON".into())).await;break;}}
                            },
                            Some(Ok(Message::Ping(bytes)))=>{if writer.send(Message::Pong(bytes)).await.is_err(){break;}continue;},
                            Some(Ok(Message::Close(frame)))=>{
                                if resume_deadline.is_some(){self.clear_session();}
                                if let Some(frame)=frame {
                                    let code:u16=frame.code.into();
                                    if matches!(code,4007|4009)||resume_deadline.is_some(){self.clear_session();}
                                    if code==4004{
                                        // AuthSession.ts:630-689 invalidates only
                                        // explicit authentication-failed closes.
                                        let _=events.send(GatewayEvent::AuthenticationFailed).await;
                                        terminal=true;
                                    }
                                }
                                break;
                            },
                            Some(Ok(_))=>continue,
                            _=>{if resume_deadline.is_some(){self.clear_session();}break;},
                        };
                        self.last_activity=Some(Instant::now());
                        match payload.op {
                            10=>{
                                let Some(ms)=payload.d.get("heartbeat_interval").and_then(Value::as_u64).filter(|ms|*ms>0&&*ms<=300000) else{break;};
                                if heartbeat_interval.is_some(){break;}
                                hello_deadline=None;
                                let interval=Duration::from_millis(ms);heartbeat_interval=Some(interval);heartbeat_at=Some(Instant::now()+heartbeat_delay(interval));
                                let auth=self.authentication_payload();
                                if auth["op"]==6{resume_deadline=Some(Instant::now()+RESUME_TIMEOUT);}
                                if transmit(&mut writer,auth,&mut codec).await.is_err(){break;}
                            },
                            0=>{
                                let Some(name)=payload.t else{break;};
                                if let (Some(previous),Some(sequence))=(self.resume.sequence,payload.s)&&sequence<=previous&&name!="READY"&&name!="RESUMED"{continue;}
                                if name=="READY" {
                                    let Some(session)=payload.d.get("session_id").and_then(Value::as_str) else{break;};
                                    self.resume.session_id=Some(session.to_owned());
                                }
                                let ready=name=="READY"||name=="RESUMED";
                                let resumed=name=="RESUMED";
                                let event=GatewayEvent::Dispatch{name,data:payload.d,sequence:payload.s};
                                if !matches!(timeout(Duration::from_secs(2),events.send(event)).await,Ok(Ok(()))){break;}
                                if let Some(sequence)=payload.s{self.resume.sequence=Some(sequence);}
                                if ready {
                                    authenticated=true;failures=0;backoff.attempts=0;resume_deadline=None;ack_deadline=None;
                                    if resumed&&let Some(presence)=&self.presence&&transmit(&mut writer,json!({"op":3,"d":presence}),&mut codec).await.is_err(){break;}
                                    if events.send(GatewayEvent::Connected).await.is_err(){stop=true;break;}
                                }
                            },
                            1=>{
                                if transmit(&mut writer,json!({"op":1,"d":self.resume.sequence.unwrap_or(0)}),&mut codec).await.is_err(){break;}
                                self.last_activity=Some(Instant::now());ack_deadline=Some(Instant::now()+ACK_TIMEOUT);
                                if let Some(interval)=heartbeat_interval{heartbeat_at=Some(Instant::now()+heartbeat_delay(interval));}
                            },
                            7=>{immediate_retry=true;break;},
                            9=>{
                                authenticated=false;
                                if payload.d!=Value::Bool(true)&&resume_deadline.is_some(){
                                    self.clear_session();resume_deadline=None;reidentify_at=Some(Instant::now()+Duration::from_millis(fastrand::u64(0..1000)));
                                }else{
                                    if payload.d!=Value::Bool(true){self.clear_session();}
                                    invalid_session_at=Some(Instant::now()+Duration::from_millis(2500+fastrand::u64(0..1000)));
                                }
                            },
                            11=>ack_deadline=None,
                            12=>{let code=payload.d.get("code").and_then(Value::as_str).unwrap_or("UNKNOWN_ERROR");let message=payload.d.get("message").and_then(Value::as_str).unwrap_or("gateway rejected a request");let _=events.send(GatewayEvent::Error(format!("{code}: {message}"))).await;},
                            _=>{},
                        }
                    }
                }
            }
            fallback_compression |= codec.failed && self.compression;
            if fallback_compression {
                self.compression = false;
                let _ = timeout(
                    Duration::from_secs(1),
                    writer.send(Message::Close(Some(
                        tokio_tungstenite::tungstenite::protocol::CloseFrame {
                            code: 4002.into(),
                            reason: "Retrying without compression".into(),
                        },
                    ))),
                )
                .await;
            } else {
                let _ = timeout(Duration::from_secs(1), writer.close()).await;
            }
            if stop {
                break;
            }
            if terminal {
                let _ = events.send(GatewayEvent::Disconnected).await;
                return Err(Error::Gateway(
                    "gateway authentication or payload rejected".into(),
                ));
            }
            failures = failures.saturating_add(1);
            if wait_retry(
                backoff.next_delay(immediate_retry || fallback_compression),
                &events,
                &mut commands,
                &mut self.presence,
            )
            .await
            {
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

struct WireCodec {
    encoder: Option<zstd_stream::ZstdStreamEncoder>,
    decoder: Option<zstd_stream::ZstdStreamDecoder>,
    failed: bool,
}
impl WireCodec {
    fn new(enabled: bool) -> Result<Self> {
        Ok(Self {
            failed: false,
            encoder: if enabled {
                Some(zstd_stream::ZstdStreamEncoder::new(3).map_err(codec_error)?)
            } else {
                None
            },
            decoder: if enabled {
                Some(zstd_stream::ZstdStreamDecoder::new().map_err(codec_error)?)
            } else {
                None
            },
        })
    }
    fn encode(&mut self, json: &[u8]) -> Result<Message> {
        Ok(if let Some(encoder) = &mut self.encoder {
            Message::Binary(encoder.compress_chunk(json).map_err(codec_error)?.into())
        } else {
            Message::Text(
                String::from_utf8(json.to_vec())
                    .map_err(|_| Error::Gateway("invalid outbound UTF-8".into()))?
                    .into(),
            )
        })
    }
    fn decode(&mut self, bytes: &[u8]) -> Result<Vec<u8>> {
        match &mut self.decoder {
            Some(decoder) => decoder.decompress_chunk(bytes).map_err(|error| {
                self.failed = true;
                codec_error(error)
            }),
            None => Ok(bytes.to_vec()),
        }
    }
}
fn codec_error(error: zstd_stream::ZstdError) -> Error {
    Error::Gateway(error.message().into())
}
async fn transmit<S>(writer: &mut S, payload: Value, codec: &mut WireCodec) -> Result<()>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    let message = codec.encode(payload.to_string().as_bytes())?;
    timeout(Duration::from_secs(5), writer.send(message))
        .await
        .map_err(|_| Error::Gateway("socket send timed out".into()))??;
    Ok(())
}
async fn deadline(at: Option<Instant>) {
    match at {
        Some(at) => sleep_until(at).await,
        None => std::future::pending().await,
    }
}
fn heartbeat_delay(interval: Duration) -> Duration {
    let ms = interval.as_millis() as u64;
    Duration::from_millis((ms * 8 / 10).max(1000) + fastrand::u64(0..=(ms / 20).min(1500)))
}
async fn wait_retry(
    delay: Duration,
    events: &mpsc::Sender<GatewayEvent>,
    commands: &mut mpsc::Receiver<GatewayCommand>,
    presence: &mut Option<Value>,
) -> bool {
    let until = Instant::now() + delay;
    loop {
        tokio::select! {
            _=sleep_until(until)=>return false,
            _=events.closed()=>return true,
            command=commands.recv()=>match command {None|Some(GatewayCommand::Close)=>return true,Some(GatewayCommand::Presence(value))=>*presence=Some(value),_=>{}},
        }
    }
}
#[derive(Default)]
struct ReconnectBackoff {
    attempts: u32,
    last_at: Option<Instant>,
}
impl ReconnectBackoff {
    // GatewaySocket.nextReconnectDelay and reconnectSpread, including its
    // one-second minimum between rapidly failed connection attempts.
    fn next_delay(&mut self, immediate: bool) -> Duration {
        if immediate {
            return Duration::from_millis(fastrand::u64(0..2000));
        }
        if self
            .last_at
            .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
        {
            return Duration::from_millis(1000 + fastrand::u64(0..2000));
        }
        self.last_at = Some(Instant::now());
        self.attempts = self.attempts.saturating_add(1);
        let ceiling = 1000_u64
            .saturating_mul(1_u64 << self.attempts.saturating_sub(1).min(10))
            .min(10000);
        Duration::from_millis(fastrand::u64(500..=ceiling))
    }
}
fn native_properties() -> Value {
    let locale = std::env::var("LC_ALL")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("LC_MESSAGES").ok().filter(|v| !v.is_empty()))
        .or_else(|| std::env::var("LANG").ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "en-US".into());
    let locale = locale
        .split(['.', '@'])
        .next()
        .unwrap_or("en-US")
        .replace('_', "-");
    let locale = if locale == "C" || locale == "POSIX" {
        "en-US".into()
    } else {
        locale
    };
    let os = match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "macOS",
        "windows" => "Windows",
        other => other,
    };
    json!({"os":os,"os_version":"","browser":"Fluxer Native","browser_version":env!("CARGO_PKG_VERSION"),"device":std::env::consts::ARCH,
        "system_locale":locale,"locale":locale,"user_agent":concat!("Fluxer-Native/",env!("CARGO_PKG_VERSION")),
        "build_version":option_env!("FLUXER_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION")),
        "desktop_app_version":env!("CARGO_PKG_VERSION"),"desktop_app_channel":option_env!("FLUXER_RELEASE_CHANNEL"),
        "desktop_arch":std::env::consts::ARCH,"desktop_os":std::env::consts::OS,"e2ee_capable":false})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn gateway() -> Gateway {
        Gateway::new(
            Url::parse("wss://chat.example/gateway").unwrap(),
            SessionToken::new("secret").unwrap(),
        )
    }
    #[test]
    fn desktop_identify_url_and_flags_match_source_contract() {
        // PooledGatewayConnection.ts467-487 and GatewaySocket.ts1406-1414.
        let gateway = gateway()
            .with_initial_guild(Some("123".into()))
            .with_presence(json!({"status":"idle","afk":true,"mobile":false,"custom_status":null}));
        assert_eq!(
            gateway.connect_url().unwrap().query(),
            Some("v=1&encoding=json&compress=zstd-stream&stream=1")
        );
        let payload = gateway.authentication_payload();
        assert_eq!(payload["op"], 2);
        assert_eq!(payload["d"]["flags"], 14);
        assert_eq!(payload["d"]["initial_guild_id"], "123");
        assert_eq!(payload["d"]["presence"]["status"], "idle");
        for name in [
            "os",
            "os_version",
            "browser",
            "browser_version",
            "device",
            "system_locale",
            "locale",
            "user_agent",
            "build_version",
            "desktop_app_version",
            "desktop_app_channel",
            "desktop_arch",
            "desktop_os",
            "e2ee_capable",
        ] {
            assert!(
                payload["d"]["properties"].get(name).is_some(),
                "missing {name}"
            );
        }
        let resume = gateway
            .with_resume(ResumeSession {
                session_id: Some("session".into()),
                sequence: Some(42),
            })
            .authentication_payload();
        assert_eq!(
            resume,
            json!({"op":6,"d":{"token":"secret","session_id":"session","seq":42}})
        );
    }
    #[test]
    fn bidirectional_stream_codec_reuses_the_existing_desktop_encoder() {
        let messages = [
            br#"{"op":10,"d":{"heartbeat_interval":45000}}"#.as_slice(),
            br#"{"op":0,"s":1,"t":"READY","d":{"session_id":"a"}}"#.as_slice(),
            br#"{"op":0,"s":2,"t":"MESSAGE_CREATE","d":{"content":"repeat repeat repeat"}}"#
                .as_slice(),
        ];
        let mut desktop = zstd_stream::ZstdStreamEncoder::new(3).unwrap();
        let mut native = WireCodec::new(true).unwrap();
        for message in messages {
            let bytes = desktop.compress_chunk(message).unwrap();
            assert_eq!(native.decode(&bytes).unwrap(), message);
        }
        let mut native = WireCodec::new(true).unwrap();
        let mut desktop = zstd_stream::ZstdStreamDecoder::new().unwrap();
        for message in messages {
            let Message::Binary(bytes) = native.encode(message).unwrap() else {
                panic!("expected compressed binary")
            };
            assert_eq!(desktop.decompress_chunk(&bytes).unwrap(), message);
        }
    }
    #[test]
    fn compressed_header_can_arrive_before_payload_and_new_socket_restarts_dictionary() {
        let mut encoder = zstd_stream::ZstdStreamEncoder::new(3).unwrap();
        let bytes = encoder.compress_chunk(br#"{"op":11,"d":null}"#).unwrap();
        let mut codec = WireCodec::new(true).unwrap();
        assert!(codec.decode(&bytes[..3]).unwrap().is_empty());
        assert_eq!(codec.decode(&bytes[3..]).unwrap(), br#"{"op":11,"d":null}"#);
        let mut fresh = WireCodec::new(true).unwrap();
        assert_eq!(fresh.decode(&bytes).unwrap(), br#"{"op":11,"d":null}"#);
    }
    #[test]
    fn fallback_and_timing_match_desktop_source_limits() {
        assert_eq!(
            gateway()
                .with_compression_enabled(false)
                .connect_url()
                .unwrap()
                .query(),
            Some("v=1&encoding=json&compress=none")
        );
        let delay = heartbeat_delay(Duration::from_millis(45000));
        assert!((36000..=37500).contains(&(delay.as_millis() as u64)));
        assert_eq!(ACK_TIMEOUT, Duration::from_secs(15));
        assert_eq!(HELLO_TIMEOUT, Duration::from_secs(20));
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
    }
    async fn receive(
        socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        codec: &mut WireCodec,
    ) -> Value {
        loop {
            let message = timeout(Duration::from_secs(5), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            match message {
                Message::Text(text) => return serde_json::from_str(&text).unwrap(),
                Message::Binary(bytes) => {
                    let decoded = codec.decode(&bytes).unwrap();
                    if !decoded.is_empty() {
                        return serde_json::from_slice(&decoded).unwrap();
                    }
                }
                Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await.unwrap(),
                other => panic!("unexpected gateway fixture frame {other:?}"),
            }
        }
    }
    async fn fixture_send(
        socket: &mut tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        codec: &mut WireCodec,
        value: Value,
    ) {
        socket
            .send(codec.encode(value.to_string().as_bytes()).unwrap())
            .await
            .unwrap();
    }
    async fn wait_connected(events: &mut mpsc::Receiver<GatewayEvent>) {
        timeout(Duration::from_secs(8), async {
            loop {
                match events.recv().await {
                    Some(GatewayEvent::Connected) => break,
                    None => panic!("gateway event stream closed before Connected"),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    #[allow(clippy::result_large_err)] // tungstenite fixes its handshake callback error type.
    async fn compressed_socket_identifies_resumes_and_restores_latest_presence() {
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(
                stream,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                    assert_eq!(
                        request.uri().query(),
                        Some("v=1&encoding=json&compress=zstd-stream&stream=1")
                    );
                    Ok(response)
                },
            )
            .await
            .unwrap();
            let mut codec = WireCodec::new(true).unwrap();
            fixture_send(
                &mut socket,
                &mut codec,
                json!({"op":10,"d":{"heartbeat_interval":45000}}),
            )
            .await;
            let identify = receive(&mut socket, &mut codec).await;
            assert_eq!(identify["op"], 2);
            assert_eq!(identify["d"]["flags"], 14);
            fixture_send(&mut socket, &mut codec, json!({"op":1})).await;
            assert_eq!(
                receive(&mut socket, &mut codec).await,
                json!({"op":1,"d":0})
            );
            fixture_send(&mut socket, &mut codec, json!({"op":11})).await;
            fixture_send(
                &mut socket,
                &mut codec,
                json!({"op":0,"t":"READY","s":1,"d":{"session_id":"first"}}),
            )
            .await;
            assert_eq!(
                receive(&mut socket, &mut codec).await,
                json!({"op":3,"d":{"status":"dnd","afk":false,"mobile":false}})
            );
            fixture_send(&mut socket, &mut codec, json!({"op":7})).await;
            drop(socket);
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut codec = WireCodec::new(true).unwrap();
            fixture_send(
                &mut socket,
                &mut codec,
                json!({"op":10,"d":{"heartbeat_interval":45000}}),
            )
            .await;
            assert_eq!(
                receive(&mut socket, &mut codec).await,
                json!({"op":6,"d":{"token":"secret","session_id":"first","seq":1}})
            );
            fixture_send(
                &mut socket,
                &mut codec,
                json!({"op":0,"t":"RESUMED","s":2,"d":{}}),
            )
            .await;
            assert_eq!(
                receive(&mut socket, &mut codec).await,
                json!({"op":3,"d":{"status":"dnd","afk":false,"mobile":false}})
            );
            while let Some(Ok(message)) = socket.next().await {
                if matches!(message, Message::Close(_)) {
                    break;
                }
            }
        });
        let (tx, mut events) = mpsc::channel(32);
        let (commands, rx) = mpsc::channel(16);
        let client = tokio::spawn(
            Gateway::new(
                Url::parse(&format!("ws://{address}/gateway")).unwrap(),
                SessionToken::new("secret").unwrap(),
            )
            .run(tx, rx),
        );
        wait_connected(&mut events).await;
        commands
            .send(GatewayCommand::Presence(
                json!({"status":"dnd","afk":false,"mobile":false}),
            ))
            .await
            .unwrap();
        wait_connected(&mut events).await;
        commands.send(GatewayCommand::Close).await.unwrap();
        client.await.unwrap().unwrap();
        server.await.unwrap();
    }
    #[tokio::test]
    async fn rejected_resume_identifies_on_same_compressed_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut codec = WireCodec::new(true).unwrap();
            fixture_send(
                &mut socket,
                &mut codec,
                json!({"op":10,"d":{"heartbeat_interval":45000}}),
            )
            .await;
            assert_eq!(receive(&mut socket, &mut codec).await["op"], 6);
            fixture_send(&mut socket, &mut codec, json!({"op":9,"d":false})).await;
            assert_eq!(receive(&mut socket, &mut codec).await["op"], 2);
            fixture_send(
                &mut socket,
                &mut codec,
                json!({"op":0,"t":"READY","s":1,"d":{"session_id":"new"}}),
            )
            .await;
            while let Some(Ok(message)) = socket.next().await {
                if matches!(message, Message::Close(_)) {
                    break;
                }
            }
        });
        let (tx, mut events) = mpsc::channel(32);
        let (commands, rx) = mpsc::channel(16);
        let client = tokio::spawn(
            Gateway::new(
                Url::parse(&format!("ws://{address}")).unwrap(),
                SessionToken::new("secret").unwrap(),
            )
            .with_resume(ResumeSession {
                session_id: Some("old".into()),
                sequence: Some(8),
            })
            .run(tx, rx),
        );
        wait_connected(&mut events).await;
        commands.send(GatewayCommand::Close).await.unwrap();
        client.await.unwrap().unwrap();
        server.await.unwrap();
    }
    #[tokio::test]
    #[allow(clippy::result_large_err)] // tungstenite fixes its handshake callback error type.
    async fn invalid_zstd_reconnects_with_existing_client_none_fallback() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut codec = WireCodec::new(true).unwrap();
            fixture_send(
                &mut socket,
                &mut codec,
                json!({"op":10,"d":{"heartbeat_interval":45000}}),
            )
            .await;
            assert_eq!(receive(&mut socket, &mut codec).await["op"], 2);
            socket
                .send(Message::Binary(vec![0xff; 16].into()))
                .await
                .unwrap();
            drop(socket);
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(
                stream,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                    assert_eq!(
                        request.uri().query(),
                        Some("v=1&encoding=json&compress=none")
                    );
                    Ok(response)
                },
            )
            .await
            .unwrap();
            let mut codec = WireCodec::new(false).unwrap();
            fixture_send(
                &mut socket,
                &mut codec,
                json!({"op":10,"d":{"heartbeat_interval":45000}}),
            )
            .await;
            assert_eq!(receive(&mut socket, &mut codec).await["op"], 2);
            fixture_send(
                &mut socket,
                &mut codec,
                json!({"op":0,"t":"READY","s":1,"d":{"session_id":"fallback"}}),
            )
            .await;
            while let Some(Ok(message)) = socket.next().await {
                if matches!(message, Message::Close(_)) {
                    break;
                }
            }
        });
        let (tx, mut events) = mpsc::channel(32);
        let (commands, rx) = mpsc::channel(16);
        let client = tokio::spawn(
            Gateway::new(
                Url::parse(&format!("ws://{address}")).unwrap(),
                SessionToken::new("secret").unwrap(),
            )
            .run(tx, rx),
        );
        wait_connected(&mut events).await;
        commands.send(GatewayCommand::Close).await.unwrap();
        client.await.unwrap().unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn only_close_4004_emits_authentication_failed_before_disconnect_not_malformed_json() {
        // AuthSession.ts:630-650 distinguishes explicit 4004 invalidation from
        // all ordinary connection failures; malformed wire data is not a token verdict.
        for authentication_close in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let mut codec = WireCodec::new(false).unwrap();
                fixture_send(
                    &mut socket,
                    &mut codec,
                    json!({"op":10,"d":{"heartbeat_interval":45000}}),
                )
                .await;
                assert_eq!(receive(&mut socket, &mut codec).await["op"], 2);
                if authentication_close {
                    socket.send(Message::Close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame{code:tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Library(4004),reason:"private-server-reason".into()}))).await.unwrap();
                } else {
                    socket
                        .send(Message::Text("{malformed json".into()))
                        .await
                        .unwrap();
                }
                while let Some(Ok(message)) = socket.next().await {
                    if matches!(message, Message::Close(_)) {
                        break;
                    }
                }
            });
            let (tx, mut events) = mpsc::channel(32);
            let (_commands, rx) = mpsc::channel(16);
            let client = tokio::spawn(
                Gateway::new(
                    Url::parse(&format!("ws://{address}")).unwrap(),
                    SessionToken::new("private-session-token").unwrap(),
                )
                .with_compression_enabled(false)
                .run(tx, rx),
            );
            let captured = timeout(Duration::from_secs(5), async {
                let mut captured = Vec::new();
                while let Some(event) = events.recv().await {
                    let disconnected = matches!(event, GatewayEvent::Disconnected);
                    captured.push(event);
                    if disconnected {
                        break;
                    }
                }
                captured
            })
            .await
            .unwrap();
            assert_eq!(
                captured
                    .iter()
                    .filter(|event| matches!(event, GatewayEvent::AuthenticationFailed))
                    .count(),
                usize::from(authentication_close)
            );
            assert!(matches!(captured.last(), Some(GatewayEvent::Disconnected)));
            if authentication_close {
                assert!(matches!(
                    captured.get(captured.len() - 2),
                    Some(GatewayEvent::AuthenticationFailed)
                ));
            }
            assert!(!format!("{captured:?}").contains("private-"));
            assert!(client.await.unwrap().is_err());
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn session_timeout_4009_identifies_again_without_invalidating_authentication() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut codec = WireCodec::new(false).unwrap();
            fixture_send(
                &mut socket,
                &mut codec,
                json!({"op":10,"d":{"heartbeat_interval":45000}}),
            )
            .await;
            assert_eq!(receive(&mut socket, &mut codec).await["op"], 6);
            socket.send(Message::Close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame{code:tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Library(4009),reason:"session timed out".into()}))).await.unwrap();
            drop(socket);
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut codec = WireCodec::new(false).unwrap();
            fixture_send(
                &mut socket,
                &mut codec,
                json!({"op":10,"d":{"heartbeat_interval":45000}}),
            )
            .await;
            assert_eq!(receive(&mut socket, &mut codec).await["op"], 2);
            fixture_send(
                &mut socket,
                &mut codec,
                json!({"op":0,"t":"READY","s":1,"d":{"session_id":"new-session"}}),
            )
            .await;
            while let Some(Ok(message)) = socket.next().await {
                if matches!(message, Message::Close(_)) {
                    break;
                }
            }
        });
        let (tx, mut events) = mpsc::channel(32);
        let (commands, rx) = mpsc::channel(16);
        let client = tokio::spawn(
            Gateway::new(
                Url::parse(&format!("ws://{address}")).unwrap(),
                SessionToken::new("private-session-token").unwrap(),
            )
            .with_compression_enabled(false)
            .with_resume(ResumeSession {
                session_id: Some("old-session".into()),
                sequence: Some(7),
            })
            .run(tx, rx),
        );
        timeout(Duration::from_secs(8), async {
            while let Some(event) = events.recv().await {
                assert!(!matches!(event, GatewayEvent::AuthenticationFailed));
                if matches!(event, GatewayEvent::Connected) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        commands.send(GatewayCommand::Close).await.unwrap();
        client.await.unwrap().unwrap();
        while let Some(event) = events.recv().await {
            assert!(!matches!(event, GatewayEvent::AuthenticationFailed));
        }
        server.await.unwrap();
    }
}
