// SPDX-License-Identifier: AGPL-3.0-or-later
mod call_view;
mod dropdown;
mod input;
mod stream_view;
mod video_perf;

use call_view::{StreamFrame, Viewer};
use dropdown::{Dropdown, Selected};
use fluxer_client::{Command, Controller, Event, VoiceStatus};
use fluxer_core::{ChannelType, ClientState, Snowflake, VoiceParticipant};
use fluxer_media::{
    AudioApplication, AudioMixSettings, AudioSettings, InputMode, MediaEvent,
    ParticipantAudioSettings, ScreenShareOptions, ScreenSource, ScreenSourceKind,
    SharedAudioSelection, SharedAudioSettings,
};
use gpui::{prelude::*, *};
use input::TextInput;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use video_perf::{AnimatedFixture, VideoStats};

actions!(fluxer, [Submit, Quit, OpenSettings, StopWatching]);

const BG: u32 = 0x1d1b26;
const PANEL: u32 = 0x252330;
const RAIL: u32 = 0x16141e;
const ACCENT: u32 = 0x8873ee;
const TEXT: u32 = 0xece9f6;
const MUTED: u32 = 0xa19bb3;

#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Chat,
    Call,
    Settings,
    Screens,
    Search,
}

struct IpAuthorization {
    email: String,
    started: std::time::Instant,
    resend_available_in: u64,
    resend_used: bool,
    polling_failed: bool,
}

struct BrowserLogin {
    code: String,
    expires_at: String,
    browser_url: String,
    polling_failed: bool,
}

impl IpAuthorization {
    fn remaining(&self) -> u64 {
        self.resend_available_in
            .saturating_sub(self.started.elapsed().as_secs())
    }
}

fn button_base(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_md()
        .bg(rgb(0x353142))
        .cursor_pointer()
        .text_size(px(13.))
        .hover(|style| style.bg(rgb(0x484054)))
}
fn button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    button_base(id).child(label.into())
}

fn voice_avatar(name: &str, user: Snowflake, speaking: bool, size: f32) -> AnyElement {
    let initials = name
        .split_whitespace()
        .take(2)
        .filter_map(|part| part.chars().next())
        .flat_map(char::to_uppercase)
        .collect::<String>();
    let palette = [0x7360b4, 0x436d86, 0x8a5f80, 0x4c7a68, 0x836747];
    div()
        .size(px(size))
        .flex_shrink_0()
        .rounded_full()
        .border_2()
        .border_color(if speaking { rgb(0x4fc58a) } else { rgb(PANEL) })
        .bg(rgb(palette[(user.0 % palette.len() as u64) as usize]))
        .text_color(rgb(TEXT))
        .text_size(px(if size < 30. { 10. } else { 20. }))
        .font_weight(FontWeight::SEMIBOLD)
        .flex()
        .items_center()
        .justify_center()
        .child(if initials.is_empty() {
            "?".into()
        } else {
            initials
        })
        .into_any_element()
}
fn voice_badges(participant: &VoiceParticipant, compact: bool) -> AnyElement {
    let muted = participant.self_mute || participant.mute || participant.suppress;
    let deafened = participant.self_deaf || participant.deaf;
    let badge = |label: &'static str, color: u32| {
        div()
            .px_1()
            .rounded_sm()
            .text_size(px(if compact { 9. } else { 10. }))
            .text_color(rgb(color))
            .bg(rgb(0x292633))
            .child(label)
    };
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .flex_shrink_0()
        .gap_1()
        .when(muted, |view| {
            view.child(badge(
                if participant.mute && !compact {
                    "Server muted"
                } else if participant.suppress && !compact {
                    "No speak"
                } else if compact {
                    "Mic off"
                } else {
                    "Muted"
                },
                if participant.mute || participant.suppress {
                    0xe8919e
                } else {
                    MUTED
                },
            ))
        })
        .when(deafened, |view| {
            view.child(badge(
                if participant.deaf && !compact {
                    "Server deafened"
                } else {
                    "Deaf"
                },
                if participant.deaf { 0xe8919e } else { MUTED },
            ))
        })
        .when(participant.self_video, |view| {
            view.child(badge(if compact { "Cam" } else { "Camera" }, 0xb5a8ee))
        })
        .when(participant.self_stream, |view| {
            view.child(badge("LIVE", 0x66d4a0))
        })
        .into_any_element()
}

fn heading(text: impl Into<SharedString>) -> Div {
    div()
        .text_color(rgb(TEXT))
        .text_size(px(18.))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text.into())
}
fn muted(text: impl Into<SharedString>) -> Div {
    div()
        .text_color(rgb(MUTED))
        .text_size(px(12.))
        .child(text.into())
}

fn picker(
    label: &'static str,
    options: &[(&str, &str)],
    selected: Option<&str>,
    window: &mut Window,
    cx: &mut Context<Fluxer>,
) -> Entity<Dropdown> {
    let options = options
        .iter()
        .map(|(id, label)| ((*id).into(), (*label).into()))
        .collect();
    cx.new(|cx| Dropdown::new(label, options, selected.map(String::from), window, cx))
}

fn screen_source_id(source: &ScreenSource) -> String {
    format!("{:?}:{}", source.kind, source.id)
}

struct Fluxer {
    focus: FocusHandle,
    controller: Option<Controller>,
    state: ClientState,
    demo: bool,
    session_restoring: bool,
    session_restore_failed: bool,
    session_context_loaded: bool,
    pane: Pane,
    selected_guild: Option<Snowflake>,
    composer: Entity<TextInput>,
    instance: Entity<TextInput>,
    identifier: Entity<TextInput>,
    password: Entity<TextInput>,
    search: Entity<TextInput>,
    mfa: Entity<TextInput>,
    mfa_ticket: Option<String>,
    ip_authorization: Option<IpAuthorization>,
    last_ip_countdown: u64,
    browser_login: Option<BrowserLogin>,
    error: Option<String>,
    status: String,
    voice_status: String,
    input_picker: Entity<Dropdown>,
    output_picker: Entity<Dropdown>,
    mode_picker: Entity<Dropdown>,
    fps_picker: Entity<Dropdown>,
    resolution_picker: Entity<Dropdown>,
    screen_picker: Entity<Dropdown>,
    share_audio_picker: Entity<Dropdown>,
    sources: Vec<ScreenSource>,
    share_fps: u32,
    share_size: (u32, u32),
    dsp: AudioSettings,
    sharing: bool,
    editing: Option<Snowflake>,
    viewer: Viewer<Arc<RenderImage>>,
    demo_stream: bool,
    fixture_generation: u64,
    video_stats: VideoStats,
    animation: Option<AnimatedFixture>,
    stream_audio: BTreeMap<String, ParticipantAudioSettings>,
    mixer: AudioMixSettings,
    participants: BTreeMap<String, (String, ParticipantAudioSettings)>,
    active_speakers: BTreeSet<String>,
    available_streams: BTreeSet<String>,
    applications: Vec<AudioApplication>,
    shared_audio: bool,
    shared_audio_settings: SharedAudioSettings,
    testing_microphone: bool,
    levels: (f32, f32),
    pending_message: Option<(Snowflake, String)>,
    timeline: ScrollHandle,
    last_typing: std::time::Instant,
}

impl Fluxer {
    fn new(demo: bool, demo_stream: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let controller = Controller::spawn();
        let error = controller.as_ref().err().cloned();
        let controller = controller.ok();
        let session_restoring = !demo && controller.is_some();
        let instance = cx.new(|cx| TextInput::new("Instance URL", cx));
        instance.update(cx, |v, cx| v.set("https://web.fluxer.app/api".into(), cx));
        let password = cx.new(|cx| {
            let mut v = TextInput::new("Password", cx);
            v.secret = true;
            v
        });
        let mut view = Self {
            focus: cx.focus_handle(),
            controller,
            state: ClientState::default(),
            demo,
            session_restoring,
            session_restore_failed: false,
            session_context_loaded: false,
            pane: Pane::Chat,
            selected_guild: None,
            composer: cx.new(|cx| {
                let mut input = TextInput::new("Message this channel…", cx);
                input.multiline = true;
                input
            }),
            instance,
            identifier: cx.new(|cx| TextInput::new("Email or login", cx)),
            password,
            search: cx.new(|cx| TextInput::new("Search loaded messages…", cx)),
            mfa: cx.new(|cx| TextInput::new("Authentication code", cx)),
            mfa_ticket: None,
            ip_authorization: None,
            last_ip_countdown: 0,
            browser_login: None,
            error,
            status: if session_restoring {
                "Checking saved session…".into()
            } else {
                "Sign in to your Fluxer instance".into()
            },
            voice_status: "Disconnected".into(),
            input_picker: picker("Input device", &[], Some("input:default"), window, cx),
            output_picker: picker("Output device", &[], Some("output:default"), window, cx),
            mode_picker: picker(
                "Input mode",
                &[
                    ("open", "Open microphone"),
                    ("activity", "Voice activity"),
                    ("ptt", "Push to talk"),
                ],
                Some("open"),
                window,
                cx,
            ),
            fps_picker: picker(
                "Frame rate",
                &[("15", "15 fps"), ("30", "30 fps"), ("60", "60 fps")],
                Some("30"),
                window,
                cx,
            ),
            resolution_picker: picker(
                "Maximum resolution",
                &[("720", "720p"), ("1080", "1080p"), ("1440", "1440p")],
                Some("1080"),
                window,
                cx,
            ),
            screen_picker: picker("Display or window", &[], None, window, cx),
            share_audio_picker: picker(
                "Audio source",
                &[("system", "System audio")],
                Some("system"),
                window,
                cx,
            ),
            sources: Vec::new(),
            share_fps: 30,
            share_size: (1920, 1080),
            dsp: AudioSettings::default(),
            sharing: false,
            editing: None,
            viewer: Viewer::default(),
            demo_stream,
            fixture_generation: 1,
            video_stats: VideoStats::new(),
            animation: None,
            stream_audio: BTreeMap::new(),
            mixer: AudioMixSettings::default(),
            participants: BTreeMap::new(),
            active_speakers: BTreeSet::new(),
            available_streams: BTreeSet::new(),
            applications: Vec::new(),
            shared_audio: false,
            shared_audio_settings: SharedAudioSettings::default(),
            testing_microphone: false,
            levels: (0., 0.),
            pending_message: None,
            timeline: ScrollHandle::new(),
            last_typing: std::time::Instant::now() - Duration::from_secs(10),
        };
        window.focus(&view.focus);
        cx.subscribe(&view.input_picker, |this, _, selected: &Selected, cx| {
            this.command(Command::SetInputDevice(selected.0.clone()), cx);
        })
        .detach();
        cx.subscribe(&view.output_picker, |this, _, selected: &Selected, cx| {
            this.command(Command::SetOutputDevice(selected.0.clone()), cx);
        })
        .detach();
        cx.subscribe(&view.mode_picker, |this, _, selected: &Selected, cx| {
            this.mixer.input_mode = match selected.0.as_str() {
                "activity" => InputMode::VoiceActivity,
                "ptt" => InputMode::PushToTalk,
                _ => InputMode::OpenMic,
            };
            this.command(Command::SetMixer(this.mixer.clone()), cx);
        })
        .detach();
        cx.subscribe(&view.fps_picker, |this, _, selected: &Selected, cx| {
            if let Ok(fps) = selected.0.parse() {
                this.share_fps = fps;
            }
            cx.notify();
        })
        .detach();
        cx.subscribe(
            &view.resolution_picker,
            |this, _, selected: &Selected, cx| {
                this.share_size = match selected.0.as_str() {
                    "720" => (1280, 720),
                    "1440" => (2560, 1440),
                    _ => (1920, 1080),
                };
                cx.notify();
            },
        )
        .detach();
        for dropdown in [&view.screen_picker, &view.share_audio_picker] {
            cx.subscribe(dropdown, |_, _, _: &Selected, cx| cx.notify())
                .detach();
        }
        cx.observe(&view.search, |_, _, cx| cx.notify()).detach();
        cx.observe(&view.composer, |this, _, cx| {
            let content = this.composer.read(cx).content.to_string();
            if this.state.selected_channel.is_some() {
                this.command(Command::SetDraft(content), cx);
            }
            if !this.demo
                && this.state.current_user.is_some()
                && this.last_typing.elapsed() > Duration::from_secs(8)
            {
                this.last_typing = std::time::Instant::now();
                this.command(Command::Typing, cx);
            }
        })
        .detach();
        cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() {
                this.command(Command::SetPushToTalkPressed(false), cx);
                for picker in [
                    &this.input_picker,
                    &this.output_picker,
                    &this.mode_picker,
                    &this.fps_picker,
                    &this.resolution_picker,
                    &this.screen_picker,
                    &this.share_audio_picker,
                ] {
                    picker.update(cx, |picker, cx| picker.dismiss(cx));
                }
            }
        })
        .detach();
        if let Some(controller) = view.controller.as_ref()
            && let Err(error) = controller.send(if demo {
                Command::Demo
            } else {
                Command::RestoreSession
            })
        {
            view.error = Some(error);
            view.session_restoring = false;
            view.session_restore_failed = !demo;
        }
        cx.spawn(async move |entity, cx| {
            let mut cadence = Duration::from_millis(33);
            loop {
                Timer::after(cadence).await;
                match entity.update(cx, |view, cx| {
                    view.poll(cx);
                    !view.viewer.desired.is_empty()
                }) {
                    Ok(true) => cadence = Duration::from_millis(16),
                    Ok(false) => cadence = Duration::from_millis(100),
                    Err(_) => break,
                }
            }
        })
        .detach();
        if demo_stream && std::env::args().any(|arg| arg == "--demo-stream-animated") {
            view.animation = Some(AnimatedFixture::new());
            view.schedule_perf_stop(window, cx);
        }
        view
    }

    fn command(&mut self, command: Command, cx: &mut Context<Self>) {
        if let Some(controller) = &self.controller {
            if let Err(error) = controller.send(command) {
                self.error = Some(error);
            }
        } else {
            self.error = Some("The native client worker is unavailable".into());
        }
        cx.notify();
    }

    fn retire_image(&self, image: Arc<RenderImage>, cx: &mut Context<Self>) {
        let stats = self.video_stats.clone();
        cx.defer(move |cx| {
            stats.image_evicted(image.id.0);
            cx.drop_image(image, None);
        });
    }
    fn retire_stream_image(&self, identity: &str, cx: &mut Context<Self>) {
        if let Some(frame) = self.viewer.frames.get(identity) {
            self.retire_image(frame.image.clone(), cx);
        }
    }
    fn clear_viewer(&mut self, cx: &mut Context<Self>) {
        for frame in self.viewer.frames.values() {
            self.retire_image(frame.image.clone(), cx);
        }
        self.viewer.clear();
    }
    fn reset_viewer_transport(&mut self, cx: &mut Context<Self>) {
        for frame in self.viewer.frames.values() {
            self.retire_image(frame.image.clone(), cx);
        }
        self.viewer.transport_reset();
    }
    fn display_video(&mut self, frame: fluxer_media::VideoFrame, cx: &mut Context<Self>) {
        if !self
            .viewer
            .accepts(&frame.participant, frame.watch_generation)
        {
            return;
        }
        if let Some(buffer) = image::RgbaImage::from_raw(
            frame.width,
            frame.height,
            Arc::try_unwrap(frame.bgra).unwrap_or_else(|shared| shared.as_ref().clone()),
        ) {
            let image = RenderImage::new(smallvec::smallvec![image::Frame::new(buffer)]);
            self.retire_stream_image(&frame.participant, cx);
            self.video_stats.accepted();
            self.video_stats.image_added(image.id.0);
            self.viewer.receive(
                frame.participant,
                StreamFrame {
                    track_id: frame.track_id,
                    watch_generation: frame.watch_generation,
                    width: frame.width,
                    height: frame.height,
                    image: Arc::new(image),
                },
            );
        }
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let poll_started = std::time::Instant::now();
        let mut changed = false;
        for _ in 0..128 {
            let Some(event) = self.controller.as_ref().and_then(|c| c.try_recv_control()) else {
                break;
            };
            changed = true;
            match event {
                Event::State(state) => {
                    let old = self.state.selected_channel;
                    self.state = *state;
                    self.sharing = self.state.voice.screen_share;
                    if self.state.current_user.is_none() && self.state.guilds.is_empty() {
                        // A restore can publish an empty transition before its
                        // validated snapshot. Preserve its explicit context;
                        // logout and ordinary session resets clear stale UI.
                        if !self.session_restoring {
                            self.selected_guild = None;
                            self.session_context_loaded = false;
                        }
                    } else if self.selected_guild.is_none() && !self.session_context_loaded {
                        self.selected_guild = self.state.guilds.keys().next().copied();
                    }
                    if old != self.state.selected_channel {
                        let draft = self
                            .state
                            .selected_channel
                            .and_then(|id| self.state.drafts.get(&id))
                            .cloned()
                            .unwrap_or_default();
                        self.composer.update(cx, |input, cx| input.set(draft, cx));
                        self.editing = None;
                    }
                    if self.state.current_user.is_some() {
                        self.mfa_ticket = None;
                        self.password.update(cx, |v, cx| v.set(String::new(), cx));
                    }
                }
                Event::SessionRestoring(restoring) => {
                    self.session_restoring = restoring;
                    if restoring {
                        self.session_restore_failed = false;
                        self.mfa_ticket = None;
                        self.ip_authorization = None;
                        self.browser_login = None;
                    }
                }
                Event::SessionRestoreFailed => {
                    self.session_restoring = false;
                    self.session_restore_failed = true;
                    self.status = "Saved sign-in is unavailable. Retry or sign in below.".into();
                }
                Event::SessionContext {
                    instance,
                    selected_guild,
                } => {
                    self.instance
                        .update(cx, |input, cx| input.set(instance, cx));
                    self.selected_guild = selected_guild;
                    self.session_context_loaded = true;
                    self.pane = Pane::Chat;
                }
                Event::Error(error) => {
                    self.error = Some(error);
                    self.pending_message = None;
                }
                Event::Status(status) => self.status = status,
                Event::Demo(demo) => {
                    self.demo = demo;
                    if demo {
                        self.session_restoring = false;
                        self.session_restore_failed = false;
                        self.session_context_loaded = false;
                        if self.demo_stream {
                            self.install_stream_fixture();
                            if self.animation.is_some() {
                                self.retire_stream_image("preview-1", cx);
                                self.viewer.stop("preview-1");
                            }
                        }
                    }
                }
                Event::MfaRequired { ticket, .. } => {
                    self.mfa_ticket = Some(ticket);
                    self.status = "Enter your authenticator code".into();
                }
                Event::IpAuthorizationRequired {
                    email,
                    resend_available_in,
                } => {
                    self.mfa_ticket = None;
                    self.password.update(cx, |v, cx| v.set(String::new(), cx));
                    self.ip_authorization = Some(IpAuthorization {
                        email,
                        started: std::time::Instant::now(),
                        resend_available_in,
                        resend_used: false,
                        polling_failed: false,
                    });
                    self.status = "Waiting for email authorization…".into();
                }
                Event::IpAuthorizationPollingFailed => {
                    if let Some(challenge) = &mut self.ip_authorization {
                        challenge.polling_failed = true;
                    }
                    self.status = "Connection lost while checking authorization".into();
                }
                Event::IpAuthorizationPollingResumed => {
                    if let Some(challenge) = &mut self.ip_authorization {
                        challenge.polling_failed = false;
                    }
                    self.error = None;
                    self.status = "Waiting for email authorization…".into();
                }
                Event::IpAuthorizationResent {
                    resend_available_in,
                } => {
                    if let Some(challenge) = &mut self.ip_authorization {
                        challenge.resend_used = true;
                        challenge.started = std::time::Instant::now();
                        challenge.resend_available_in = resend_available_in;
                    }
                }
                Event::IpAuthorizationCleared => self.ip_authorization = None,
                Event::BrowserLoginRequired {
                    code,
                    expires_at,
                    browser_url,
                } => {
                    self.password.update(cx, |v, cx| v.set(String::new(), cx));
                    self.mfa_ticket = None;
                    self.browser_login = Some(BrowserLogin {
                        code,
                        expires_at,
                        browser_url,
                        polling_failed: false,
                    });
                    self.status = "Waiting for browser sign-in…".into();
                }
                Event::BrowserLoginPollingFailed => {
                    if let Some(handoff) = &mut self.browser_login {
                        handoff.polling_failed = true;
                    }
                    self.status = "Connection lost while checking browser sign-in".into();
                }
                Event::BrowserLoginPollingResumed => {
                    if let Some(handoff) = &mut self.browser_login {
                        handoff.polling_failed = false;
                    }
                    self.error = None;
                    self.status = "Waiting for browser sign-in…".into();
                }
                Event::BrowserLoginCleared => self.browser_login = None,
                // Commands update desired viewing immediately. Delayed worker
                // snapshots must never re-add a locally stopped stream.
                Event::WatchedStreams(_) => {}
                Event::Devices(devices) => {
                    self.input_picker.update(cx, |picker, cx| {
                        picker.set_options(
                            devices
                                .inputs
                                .iter()
                                .map(|d| (d.id.clone(), d.label.clone()))
                                .collect(),
                            cx,
                        )
                    });
                    self.output_picker.update(cx, |picker, cx| {
                        picker.set_options(
                            devices
                                .outputs
                                .iter()
                                .map(|d| (d.id.clone(), d.label.clone()))
                                .collect(),
                            cx,
                        )
                    });
                }
                Event::ScreenSources(sources) => {
                    self.screen_picker.update(cx, |picker, cx| {
                        picker.set_options(
                            sources
                                .iter()
                                .filter(|source| source.kind != ScreenSourceKind::SystemPicker)
                                .map(|s| (screen_source_id(s), s.label.clone()))
                                .collect(),
                            cx,
                        )
                    });
                    self.sources = sources;
                }
                Event::VoiceStatus(status) => {
                    self.voice_status = match status {
                        VoiceStatus::Disconnected => {
                            self.clear_viewer(cx);
                            if self.pane == Pane::Call {
                                self.pane = Pane::Chat;
                            }
                            self.participants.clear();
                            self.active_speakers.clear();
                            self.available_streams.clear();
                            self.shared_audio = false;
                            self.sharing = false;
                            "Disconnected".into()
                        }
                        VoiceStatus::Connecting { channel_id } => {
                            format!("Connecting to {}…", self.channel_name(channel_id))
                        }
                        VoiceStatus::Connected { channel_id } => {
                            format!("Voice connected · {}", self.channel_name(channel_id))
                        }
                        VoiceStatus::Reconnecting => {
                            self.active_speakers.clear();
                            self.reset_viewer_transport(cx);
                            "Reconnecting voice…".into()
                        }
                    }
                }
                Event::AudioApplications(apps) => {
                    let options = std::iter::once(("system".into(), "System audio".into()))
                        .chain(
                            apps.iter()
                                .map(|app| (format!("app:{}", app.process_id), app.label.clone())),
                        )
                        .collect();
                    self.share_audio_picker
                        .update(cx, |picker, cx| picker.set_options(options, cx));
                    self.applications = apps;
                }
                Event::SharedAudioStatus(active) => self.shared_audio = active,
                Event::MessageSent {
                    channel_id,
                    content,
                }
                | Event::MessageEdited {
                    channel_id,
                    content,
                    ..
                } => {
                    self.pending_message = None;
                    if self.state.selected_channel == Some(channel_id)
                        && self.composer.read(cx).content.as_ref() == content
                    {
                        self.composer.update(cx, |v, cx| v.set(String::new(), cx));
                        self.editing = None;
                    }
                    self.timeline.scroll_to_bottom();
                }
                Event::Video(_) => unreachable!("control queue cannot contain video frames"),
                Event::Media(event) => match event {
                    MediaEvent::ScreenShareStarted => self.sharing = true,
                    MediaEvent::ScreenShareStopped => self.sharing = false,
                    MediaEvent::VideoEnded {
                        track_id,
                        watch_generation,
                        ..
                    } => {
                        for frame in self.viewer.frames.values().filter(|frame| {
                            frame.track_id == track_id && frame.watch_generation == watch_generation
                        }) {
                            self.retire_image(frame.image.clone(), cx);
                        }
                        self.viewer.end_track(&track_id, watch_generation);
                    }
                    MediaEvent::Disconnected => {
                        self.active_speakers.clear();
                        self.sharing = false;
                        self.clear_viewer(cx);
                        if self.pane == Pane::Call {
                            self.pane = Pane::Chat;
                        }
                        self.participants.clear();
                        self.available_streams.clear();
                        self.shared_audio = false;
                    }
                    MediaEvent::Reconnecting => {
                        self.active_speakers.clear();
                        self.sharing = false;
                        self.reset_viewer_transport(cx);
                        self.participants.clear();
                        self.available_streams.clear();
                        self.shared_audio = false;
                    }
                    MediaEvent::ParticipantJoined { identity, name } => {
                        self.participants
                            .insert(identity, (name, ParticipantAudioSettings::default()));
                    }
                    MediaEvent::ParticipantLeft { identity } => {
                        self.active_speakers.remove(&identity);
                        self.participants.remove(&identity);
                    }
                    MediaEvent::ScreenStreamAvailable {
                        identity,
                        available,
                    } => {
                        if available {
                            self.available_streams.insert(identity);
                        } else {
                            self.available_streams.remove(&identity);
                            self.retire_stream_image(&identity, cx);
                            self.viewer.unavailable(&identity);
                        }
                    }
                    MediaEvent::Speakers(identities) => {
                        self.active_speakers = identities.into_iter().collect()
                    }
                    MediaEvent::AudioLevels { input, output } => self.levels = (input, output),
                    MediaEvent::Error(error) => {
                        if !self.viewer.fail_pending(error.clone()) {
                            self.error = Some(error);
                        }
                        self.pending_message = None;
                    }
                    _ => {}
                },
            }
        }
        let frames = self
            .controller
            .as_ref()
            .map(Controller::drain_frames)
            .unwrap_or_default();
        for frame in frames {
            self.display_video(frame, cx);
            changed = true;
        }
        if let Some(frame) = self
            .animation
            .as_ref()
            .and_then(|animation| animation.take_frame())
        {
            self.display_video(frame, cx);
            changed = true;
        }
        self.video_stats
            .poll(poll_started.elapsed(), self.viewer.frames.len());
        let countdown = self
            .ip_authorization
            .as_ref()
            .map_or(0, IpAuthorization::remaining);
        if countdown != self.last_ip_countdown {
            self.last_ip_countdown = countdown;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        if self.session_restoring {
            return;
        }
        if self.state.current_user.is_none() {
            self.login(cx);
            return;
        }
        if self.pane != Pane::Chat || self.pending_message.is_some() {
            return;
        }
        let content = self.composer.read(cx).content.to_string();
        if content.trim().is_empty() {
            return;
        }
        if let Some(channel) = self.state.selected_channel {
            self.pending_message = Some((channel, content.clone()));
        }
        if let Some(message_id) = self.editing {
            self.command(
                Command::EditMessage {
                    message_id,
                    content,
                },
                cx,
            );
        } else {
            self.command(Command::SendMessage { content }, cx);
        }
    }

    fn channel_name(&self, id: Snowflake) -> String {
        self.state
            .channels
            .get(&id)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "voice channel".into())
    }

    fn login(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        if self.session_restoring || self.ip_authorization.is_some() || self.browser_login.is_some()
        {
            return;
        }
        if let Some(ticket) = self.mfa_ticket.clone() {
            self.command(
                Command::LoginMfa {
                    ticket,
                    code: self.mfa.read(cx).content.to_string(),
                },
                cx,
            );
        } else {
            self.selected_guild = None;
            self.session_context_loaded = false;
            self.session_restore_failed = false;
            self.command(
                Command::Login {
                    instance: self.instance.read(cx).content.to_string(),
                    identifier: self.identifier.read(cx).content.to_string(),
                    password: self.password.read(cx).content.to_string(),
                },
                cx,
            );
        }
    }

    fn select(&mut self, id: Snowflake, cx: &mut Context<Self>) {
        if self.state.selected_channel.is_some() {
            self.command(
                Command::SetDraft(self.composer.read(cx).content.to_string()),
                cx,
            );
        }
        self.command(Command::SelectChannel { channel_id: id }, cx);
        self.pane = Pane::Chat;
    }

    fn restore_session(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        self.session_restoring = true;
        self.session_restore_failed = false;
        self.status = "Checking saved session…".into();
        let result = self
            .controller
            .as_ref()
            .ok_or_else(|| "The native client worker is unavailable".to_owned())
            .and_then(|controller| controller.send(Command::RestoreSession));
        if let Err(error) = result {
            self.error = Some(error);
            self.session_restoring = false;
            self.session_restore_failed = true;
        }
        cx.notify();
    }

    fn restoring_view(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .size_full()
            .gap_4()
            .child(
                div()
                    .text_color(rgb(ACCENT))
                    .text_size(px(36.))
                    .font_weight(FontWeight::BOLD)
                    .child("fluxer"),
            )
            .child(heading("Signing you in…"))
            .child(muted("Restoring your saved session"))
            .child(muted(self.status.clone()))
            .child(
                button("cancel-restore", "Back to sign in").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.session_restoring = false;
                        this.session_restore_failed = true;
                        this.status = "Automatic sign-in canceled".into();
                        this.command(Command::CancelLogin, cx);
                    },
                )),
            )
            .into_any_element()
    }

    fn login_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(handoff) = &self.browser_login {
            let code = handoff.code.clone();
            let url = handoff.browser_url.clone();
            return div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .size_full()
                .gap_4()
                .child(heading("Sign in with your browser"))
                .child(muted(
                    "Open your browser, sign in, then enter this code to link your account.",
                ))
                .child(heading(handoff.code.clone()))
                .child(muted(format!("Expires {}", handoff.expires_at)))
                .child(
                    button("copy-handoff", "Copy code").on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(code.clone()))
                    }),
                )
                .child(
                    button("open-browser", "Open browser")
                        .bg(rgb(0x6f56d7))
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                )
                .when(handoff.polling_failed, |v| {
                    v.child(button("retry-browser", "Retry").on_click(
                        cx.listener(|this, _, _, cx| this.command(Command::RetryBrowserLogin, cx)),
                    ))
                })
                .child(
                    button("cancel-browser", "Back to sign in").on_click(
                        cx.listener(|this, _, _, cx| this.command(Command::CancelLogin, cx)),
                    ),
                )
                .child(muted(self.status.clone()))
                .into_any_element();
        }
        if let Some(challenge) = &self.ip_authorization {
            let remaining = challenge.remaining();
            return div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .size_full()
                .gap_4()
                .child(heading(if challenge.polling_failed {
                    "Connection lost"
                } else {
                    "Check your email"
                }))
                .child(muted(format!(
                    "Check your inbox at {} for the authorization link.",
                    challenge.email
                )))
                .child(muted(
                    "Sign-in will continue here after you authorize the email link.",
                ))
                .when(challenge.polling_failed, |v| {
                    v.child(
                        button("retry-ip", "Retry").on_click(cx.listener(|this, _, _, cx| {
                            this.command(Command::RetryIpAuthorization, cx)
                        })),
                    )
                })
                .when(!challenge.resend_used && remaining == 0, |v| {
                    v.child(button("resend-ip", "Resend email").on_click(cx.listener(
                        |this, _, _, cx| this.command(Command::ResendIpAuthorization, cx),
                    )))
                })
                .when(!challenge.resend_used && remaining > 0, |v| {
                    v.child(muted(format!("Resend available in {remaining}s")))
                })
                .when(challenge.resend_used, |v| {
                    v.child(muted("Authorization email resent"))
                })
                .child(button("cancel-ip", "Back to sign in").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.mfa_ticket = None;
                        this.command(Command::CancelLogin, cx);
                    },
                )))
                .child(muted(self.status.clone()))
                .into_any_element();
        }
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .size_full()
            .gap_4()
            .child(
                div()
                    .text_color(rgb(ACCENT))
                    .text_size(px(36.))
                    .font_weight(FontWeight::BOLD)
                    .child("fluxer"),
            )
            .child(heading("Welcome back"))
            .child(muted("Sign in to your Fluxer account"))
            .child(
                div()
                    .w(px(380.))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .when(self.session_restore_failed, |view| {
                        view.child(
                            button("retry-session-restore", "Retry saved sign-in")
                                .on_click(cx.listener(|this, _, _, cx| this.restore_session(cx))),
                        )
                    })
                    .child(self.instance.clone())
                    .child(self.identifier.clone())
                    .child(self.password.clone())
                    .when(self.mfa_ticket.is_some(), |v| v.child(self.mfa.clone()))
                    .child(
                        button("login", "Sign in")
                            .bg(rgb(0x6f56d7))
                            .on_click(cx.listener(|this, _, _, cx| this.login(cx))),
                    )
                    .child(
                        button("browser-login", "Sign in with browser").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.error = None;
                                this.selected_guild = None;
                                this.session_context_loaded = false;
                                this.session_restore_failed = false;
                                this.command(
                                    Command::BrowserLogin {
                                        instance: this.instance.read(cx).content.to_string(),
                                        identifier: this.identifier.read(cx).content.to_string(),
                                    },
                                    cx,
                                );
                            },
                        )),
                    )
                    .child(
                        button("demo", "Explore offline demo").on_click(
                            cx.listener(|this, _, _, cx| this.command(Command::Demo, cx)),
                        ),
                    ),
            )
            .child(muted(self.status.clone()))
            .into_any_element()
    }

    fn rail(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w(px(72.))
            .h_full()
            .bg(rgb(RAIL))
            .flex()
            .flex_col()
            .items_center()
            .py_3()
            .gap_3()
            .child(
                button("home", "F")
                    .size(px(44.))
                    .bg(rgb(ACCENT))
                    .text_size(px(24.))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.selected_guild = None;
                        cx.notify();
                    })),
            )
            .children(self.state.guilds.values().map(|guild| {
                let id = guild.id;
                let label = guild.name.chars().take(2).collect::<String>();
                button(("guild", id.0), label)
                    .size(px(44.))
                    .when(self.selected_guild == Some(id), |v| v.bg(rgb(0x6650bd)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_guild = Some(id);
                        this.command(Command::SelectGuild { guild_id: id }, cx);
                    }))
            }))
            .child(div().flex_1())
            .child(
                button("settings", "⚙").on_click(cx.listener(|this, _, _, cx| {
                    this.pane = Pane::Settings;
                    this.command(Command::RefreshDevices, cx);
                })),
            )
            .into_any_element()
    }

    fn channels(&self, cx: &mut Context<Self>) -> AnyElement {
        let name = self
            .selected_guild
            .and_then(|id| self.state.guilds.get(&id))
            .map(|g| g.name.clone())
            .unwrap_or("Direct messages".into());
        let mut channels = self
            .state
            .channels
            .values()
            .filter(|c| c.guild_id == self.selected_guild)
            .collect::<Vec<_>>();
        channels.sort_by_key(|c| (c.position, c.id));
        div()
            .w(px(230.))
            .h_full()
            .bg(rgb(PANEL))
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(60.))
                    .px_4()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(rgb(RAIL))
                    .child(heading(name)),
            )
            .child(
                div()
                    .id("channels")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(channels.into_iter().map(|channel| {
                        let id = channel.id;
                        let voice = channel.kind.is_voice();
                        if channel.kind == ChannelType::Category {
                            return div()
                                .px_3()
                                .pt_4()
                                .pb_1()
                                .text_color(rgb(MUTED))
                                .text_size(px(11.))
                                .child(channel.name.to_uppercase())
                                .into_any_element();
                        }
                        let label = format!("{}  {}", if voice { "◉" } else { "#" }, channel.name);
                        let roster = if voice {
                            self.voice_sidebar_participants(id)
                        } else {
                            Vec::new()
                        };
                        let count = if voice {
                            self.state.voice_participants(id).len()
                        } else {
                            0
                        };
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                button_base(("channel", id.0))
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .bg(rgb(if self.state.selected_channel == Some(id) {
                                        0x3b344f
                                    } else {
                                        PANEL
                                    }))
                                    .when(self.state.is_unread(id), |view| {
                                        view.text_color(rgb(TEXT)).font_weight(FontWeight::SEMIBOLD)
                                    })
                                    .child(div().flex_1().min_w_0().text_ellipsis().child(label))
                                    .when(voice, |view| {
                                        view.child(
                                            div()
                                                .px_1()
                                                .rounded_md()
                                                .text_size(px(11.))
                                                .bg(rgb(0x34303f))
                                                .text_color(rgb(MUTED))
                                                .child(count.to_string()),
                                        )
                                    })
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| this.select(id, cx)),
                                    ),
                            )
                            .when(!roster.is_empty(), |view| {
                                view.child(div().ml_5().mb_1().flex().flex_col().gap_1().children(
                                    roster.into_iter().map(|(participant, speaking)| {
                                        div()
                                            .id(SharedString::from(format!(
                                                "voice-row-{}-{}",
                                                id, participant.user_id
                                            )))
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .px_2()
                                            .py_1()
                                            .rounded_md()
                                            .bg(rgb(if speaking { 0x302e3d } else { PANEL }))
                                            .child(voice_avatar(
                                                &participant.display_name,
                                                participant.user_id,
                                                speaking,
                                                24.,
                                            ))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .text_size(px(12.))
                                                    .text_color(rgb(
                                                        if speaking || participant.is_current_user {
                                                            TEXT
                                                        } else {
                                                            MUTED
                                                        },
                                                    ))
                                                    .text_ellipsis()
                                                    .child(format!(
                                                        "{}{}",
                                                        participant.display_name,
                                                        if participant.is_current_user {
                                                            " (you)"
                                                        } else {
                                                            ""
                                                        }
                                                    )),
                                            )
                                            .when(participant.connection_count > 1, |view| {
                                                view.child(
                                                    div()
                                                        .text_size(px(10.))
                                                        .text_color(rgb(MUTED))
                                                        .child(format!(
                                                            "×{}",
                                                            participant.connection_count
                                                        )),
                                                )
                                            })
                                            .child(voice_badges(&participant, true))
                                    }),
                                ))
                            })
                            .into_any_element()
                    })),
            )
            .child(self.voice_bar(cx))
            .child(
                div()
                    .p_3()
                    .bg(rgb(0x201e2a))
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        self.state
                            .current_user
                            .as_ref()
                            .map(|u| u.display_name().to_string())
                            .unwrap_or_default(),
                    )
                    .child(muted(if self.demo {
                        "Offline demo · no calls"
                    } else {
                        "Connected account"
                    }))
                    .child(
                        button("logout", "Sign out").on_click(cx.listener(|this, _, _, cx| {
                            this.command(Command::Logout, cx);
                            this.selected_guild = None;
                            this.session_context_loaded = false;
                            this.session_restore_failed = false;
                            this.session_restoring = false;
                            this.mfa_ticket = None;
                            this.pending_message = None;
                            this.editing = None;
                            this.clear_viewer(cx);
                        })),
                    ),
            )
            .into_any_element()
    }

    fn voice_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .p_3()
            .bg(rgb(0x292438))
            .flex()
            .flex_col()
            .gap_2()
            .child(muted(self.voice_status.clone()))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button(
                            "mute",
                            if self.state.voice.muted {
                                "Unmute"
                            } else {
                                "Mute"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.command(Command::SetMute(!this.state.voice.muted), cx)
                        })),
                    )
                    .child(
                        button(
                            "deaf",
                            if self.state.voice.deafened {
                                "Undeafen"
                            } else {
                                "Deafen"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.command(Command::SetDeaf(!this.state.voice.deafened), cx)
                        })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button("share", if self.sharing { "Stop share" } else { "Share" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.sharing {
                                    this.command(Command::StopShare, cx);
                                } else {
                                    this.pane = Pane::Screens;
                                    this.command(Command::RefreshScreenSources, cx);
                                    this.command(Command::RefreshAudioApplications, cx);
                                }
                            })),
                    )
                    .child(button("leave", "Leave").on_click(
                        cx.listener(|this, _, _, cx| this.command(Command::LeaveVoice, cx)),
                    )),
            )
            .into_any_element()
    }

    fn participant_speaking(&self, participant: &VoiceParticipant, channel: Snowflake) -> bool {
        self.state.voice.channel_id == Some(channel)
            && !participant.self_mute
            && !participant.mute
            && !participant.suppress
            && !participant.self_deaf
            && !participant.deaf
            && participant
                .livekit_identity
                .as_ref()
                .is_some_and(|identity| self.active_speakers.contains(identity))
    }

    fn voice_sidebar_participants(&self, channel: Snowflake) -> Vec<(VoiceParticipant, bool)> {
        // VoiceParticipantsList groups multiple connections by account; self
        // mute/deaf aggregates across all devices, while live/camera/speaking
        // flags are shown when any connection has them.
        let mut grouped = Vec::<(VoiceParticipant, bool)>::new();
        for participant in self.state.voice_participants(channel) {
            let speaking = self.participant_speaking(&participant, channel);
            if let Some((entry, any_speaking)) = grouped
                .iter_mut()
                .find(|(entry, _)| entry.user_id == participant.user_id)
            {
                entry.self_mute &= participant.self_mute;
                entry.self_deaf &= participant.self_deaf;
                entry.mute |= participant.mute;
                entry.deaf |= participant.deaf;
                entry.suppress |= participant.suppress;
                entry.self_video |= participant.self_video;
                entry.self_stream |= participant.self_stream;
                *any_speaking |= speaking;
            } else {
                grouped.push((participant, speaking));
            }
        }
        grouped
    }

    fn voice_participant_pane(&self, channel: Snowflake, cx: &mut Context<Self>) -> AnyElement {
        let participants = self.state.voice_participants(channel);
        let count = participants.len();
        let connected = self.state.voice.channel_id == Some(channel);
        let guild = self
            .state
            .channels
            .get(&channel)
            .and_then(|channel| channel.guild_id);
        div()
            .flex_shrink_0()
            .bg(rgb(0x211e2b))
            .border_b_1()
            .border_color(rgb(0x363140))
            .flex()
            .flex_col()
            .child(
                div()
                    .px_5()
                    .py_3()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Voice participants"),
                            )
                            .child(
                                div()
                                    .px_2()
                                    .rounded_md()
                                    .bg(rgb(0x393248))
                                    .text_size(px(12.))
                                    .child(count.to_string()),
                            ),
                    )
                    .child(
                        button(
                            ("viewed-voice-call", channel.0),
                            if connected {
                                "Leave voice"
                            } else {
                                "Join voice"
                            },
                        )
                        .bg(rgb(if connected { 0x613342 } else { 0x6f56d7 }))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if connected {
                                this.command(Command::LeaveVoice, cx);
                            } else {
                                this.command(
                                    Command::JoinVoice {
                                        guild_id: guild,
                                        channel_id: channel,
                                    },
                                    cx,
                                );
                            }
                        })),
                    ),
            )
            .child(
                div()
                    .id(("voice-participants", channel.0))
                    .max_h(px(250.))
                    .overflow_y_scroll()
                    .px_5()
                    .pb_4()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .when(participants.is_empty(), |view| {
                        view.child(
                            div()
                                .py_5()
                                .text_color(rgb(MUTED))
                                .child("No one is in this voice channel."),
                        )
                    })
                    .children(
                        participants
                            .into_iter()
                            .enumerate()
                            .map(|(index, participant)| {
                                let speaking = self.participant_speaking(&participant, channel);
                                // Availability is filtered by the worker's current call
                                // and local connection, including own-account other devices.
                                let stream_identity =
                                    participant.livekit_identity.clone().filter(|identity| {
                                        participant.self_stream
                                            && self.state.voice.channel_id == Some(channel)
                                            && self.available_streams.contains(identity)
                                    });
                                div()
                                    .id(("voice-participant-card", index))
                                    .w(px(172.))
                                    .min_h(px(134.))
                                    .p_3()
                                    .rounded_lg()
                                    .border_2()
                                    .border_color(rgb(if speaking { 0x4fc58a } else { 0x373140 }))
                                    .bg(rgb(0x2b2638))
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .justify_center()
                                    .gap_2()
                                    .child(voice_avatar(
                                        &participant.display_name,
                                        participant.user_id,
                                        speaking,
                                        48.,
                                    ))
                                    .child(
                                        div()
                                            .w_full()
                                            .min_w_0()
                                            .text_center()
                                            .text_size(px(13.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_ellipsis()
                                            .child(format!(
                                                "{}{}",
                                                participant.display_name,
                                                if participant.is_current_user {
                                                    " (you)"
                                                } else {
                                                    ""
                                                }
                                            )),
                                    )
                                    .child(voice_badges(&participant, false))
                                    .when_some(stream_identity, |view, identity| {
                                        let watching = self.viewer.desired.contains(&identity);
                                        view.child(
                                            button(
                                                ("watch-participant-stream", index),
                                                if watching {
                                                    "Stop watching"
                                                } else {
                                                    "Watch stream"
                                                },
                                            )
                                            .when(watching, |view| view.bg(rgb(0x6650bd)))
                                            .on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.watch_stream(
                                                        identity.clone(),
                                                        !watching,
                                                        cx,
                                                    );
                                                }),
                                            ),
                                        )
                                    })
                                    .when(speaking, |view| {
                                        view.child(
                                            div()
                                                .text_size(px(10.))
                                                .text_color(rgb(0x66d4a0))
                                                .child("Speaking"),
                                        )
                                    })
                            }),
                    ),
            )
            .into_any_element()
    }

    fn chat(&self, cx: &mut Context<Self>) -> AnyElement {
        let channel = self
            .state
            .selected_channel
            .and_then(|id| self.state.channels.get(&id));
        let voice_channel = channel
            .filter(|channel| channel.kind.is_voice())
            .map(|channel| channel.id);
        let title = channel
            .map(|c| format!("{} {}", if c.kind.is_voice() { "◉" } else { "#" }, c.name))
            .unwrap_or("Choose a channel".into());
        let selected = self.state.selected_channel;
        let messages = selected
            .map(|id| self.state.channel_messages(id).cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .flex()
            .flex_col()
            .when(self.pane == Pane::Call, |view| view.overflow_hidden())
            .child(
                div()
                    .h(px(if self.pane == Pane::Call { 38. } else { 60. }))
                    .flex_shrink_0()
                    .px_5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(PANEL))
                    .child(heading(title))
                    .child(
                        button("search", "Search").on_click(cx.listener(|this, _, _, cx| {
                            this.pane = Pane::Search;
                            cx.notify();
                        })),
                    ),
            )
            .when(
                self.pane == Pane::Chat && self.viewer.focused.is_some(),
                |view| {
                    view.child(div().px_5().py_2().bg(rgb(0x30283f)).child(
                        button("return-to-stream", "Return to stream viewer").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.pane = Pane::Call;
                                cx.notify();
                            }),
                        ),
                    ))
                },
            )
            .when_some(
                voice_channel.filter(|_| self.pane != Pane::Call),
                |view, channel| view.child(self.voice_participant_pane(channel, cx)),
            )
            .when(
                self.pane != Pane::Call && !self.available_streams.is_empty(),
                |v| {
                    v.child(
                        div().flex().flex_wrap().gap_3().p_3().children(
                            self.available_streams
                                .iter()
                                .enumerate()
                                .map(|(index, identity)| {
                                    let watching = self.viewer.desired.contains(identity);
                                    let name = self
                                        .participants
                                        .get(identity)
                                        .map(|(name, _)| name.clone())
                                        .filter(|name| !name.is_empty())
                                        .or_else(|| {
                                            self.state.voice.channel_id.and_then(|channel| {
                                                self.state
                                                    .voice_participants(channel)
                                                    .into_iter()
                                                    .find(|participant| {
                                                        participant.livekit_identity.as_ref()
                                                            == Some(identity)
                                                    })
                                                    .map(|participant| participant.display_name)
                                            })
                                        })
                                        .unwrap_or_else(|| identity.clone());
                                    let identity = identity.clone();
                                    button(
                                        ("watch-stream", index),
                                        format!(
                                            "{} {name}'s screen",
                                            if watching { "Stop watching" } else { "Watch" }
                                        ),
                                    )
                                    .when(watching, |v| v.bg(rgb(0x6650bd)))
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.watch_stream(identity.clone(), !watching, cx)
                                        },
                                    ))
                                }),
                        ),
                    )
                },
            )
            .when(voice_channel.is_some(), |view| {
                view.child(
                    div()
                        .px_5()
                        .pt_3()
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(MUTED))
                        .child("CHANNEL CHAT"),
                )
            })
            .child(
                div()
                    .id("timeline")
                    .track_scroll(&self.timeline)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .when(self.state.selected_channel.is_some() && !self.demo, |v| {
                        v.child(button("older-messages", "Load older messages").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.command(Command::LoadOlderMessages, cx)
                            }),
                        ))
                    })
                    .when(messages.is_empty(), |v| {
                        v.child(muted("This conversation is ready. Send the first message."))
                    })
                    .children(messages.into_iter().map(|message| {
                        let id = message.id;
                        let content = message.content.clone();
                        let copy_content = content.clone();
                        let own = self
                            .state
                            .current_user
                            .as_ref()
                            .is_some_and(|u| u.id == message.author.id);
                        div()
                            .id(("message", id.0))
                            .flex()
                            .gap_3()
                            .child(
                                div()
                                    .size(px(34.))
                                    .rounded_full()
                                    .bg(rgb(0x514174))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        message
                                            .author
                                            .display_name()
                                            .chars()
                                            .take(1)
                                            .collect::<String>(),
                                    ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .flex()
                                            .gap_3()
                                            .items_center()
                                            .child(
                                                div().font_weight(FontWeight::SEMIBOLD).child(
                                                    message.author.display_name().to_string(),
                                                ),
                                            )
                                            .child(muted(if message.edited_timestamp.is_some() {
                                                "edited".into()
                                            } else {
                                                message.timestamp.clone()
                                            })),
                                    )
                                    .child(div().text_size(px(14.)).child(message.content))
                                    .child(
                                        div()
                                            .flex()
                                            .gap_2()
                                            .child(button(("react", id.0), "👍").on_click(
                                                cx.listener(move |this, _, _, cx| {
                                                    this.command(
                                                        Command::React {
                                                            message_id: id,
                                                            emoji: "👍".into(),
                                                        },
                                                        cx,
                                                    )
                                                }),
                                            ))
                                            .child(button(("copy", id.0), "Copy").on_click(
                                                cx.listener(move |_, _, _, cx| {
                                                    cx.write_to_clipboard(
                                                        ClipboardItem::new_string(
                                                            copy_content.clone(),
                                                        ),
                                                    );
                                                }),
                                            ))
                                            .children(message.reactions.iter().map(|r| {
                                                muted(format!("{} ×{}", r.emoji.name, r.count))
                                            }))
                                            .when(own, |v| {
                                                v.child(button(("edit", id.0), "Edit").on_click(
                                                    cx.listener(move |this, _, window, cx| {
                                                        this.editing = Some(id);
                                                        this.composer.update(cx, |v, cx| {
                                                            v.set(content.clone(), cx)
                                                        });
                                                        this.composer
                                                            .focus_handle(cx)
                                                            .focus(window);
                                                        cx.notify();
                                                    }),
                                                ))
                                                .child(button(("delete", id.0), "Delete").on_click(
                                                    cx.listener(move |this, _, _, cx| {
                                                        this.command(
                                                            Command::DeleteMessage {
                                                                message_id: id,
                                                            },
                                                            cx,
                                                        )
                                                    }),
                                                ))
                                            }),
                                    ),
                            )
                    })),
            )
            .child(
                div()
                    .p(if self.pane == Pane::Call {
                        px(8.)
                    } else {
                        px(16.)
                    })
                    .flex_shrink_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(self.editing.is_some(), |v| {
                        v.child(muted("Editing message · press Enter to save"))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(div().flex_1().child(self.composer.clone()))
                            .child(
                                button(
                                    "send",
                                    if self.pending_message.is_some() {
                                        "Sending…"
                                    } else {
                                        "Send"
                                    },
                                )
                                .bg(rgb(0x6f56d7))
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.submit(&Submit, window, cx),
                                )),
                            ),
                    )
                    .when(self.pane != Pane::Call, |view| {
                        view.child(muted(
                            "Enter to send · Shift+Enter for a new line · paste multiline text",
                        ))
                    }),
            )
            .into_any_element()
    }

    fn settings(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("settings-body")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(heading("Voice & audio"))
            .child(muted(
                "Choose devices and configure microphone processing. Changes apply immediately.",
            ))
            .child(
                button("refresh-devices", "Refresh audio devices")
                    .max_w(px(180.))
                    .on_click(
                        cx.listener(|this, _, _, cx| this.command(Command::RefreshDevices, cx)),
                    ),
            )
            .child(self.input_picker.clone())
            .child(self.output_picker.clone())
            .child(self.audio_controls(cx))
            .child(heading("Microphone processing"))
            .children(
                [
                    ("aec", "Echo cancellation", self.dsp.echo_cancellation),
                    ("ns", "Noise suppression", self.dsp.noise_suppression),
                    ("agc", "Automatic gain", self.dsp.auto_gain_control),
                ]
                .into_iter()
                .map(|(id, label, enabled)| {
                    div()
                        .w_full()
                        .max_w(px(520.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(label)
                        .child(
                            button(id, if enabled { "On" } else { "Off" })
                                .w(px(64.))
                                .text_center()
                                .when(enabled, |view| view.bg(rgb(0x6650bd)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    match id {
                                        "aec" => {
                                            this.dsp.echo_cancellation = !this.dsp.echo_cancellation
                                        }
                                        "ns" => {
                                            this.dsp.noise_suppression = !this.dsp.noise_suppression
                                        }
                                        _ => {
                                            this.dsp.auto_gain_control = !this.dsp.auto_gain_control
                                        }
                                    }
                                    this.command(Command::SetDsp(this.dsp.clone()), cx);
                                })),
                        )
                }),
            )
            .child(
                button("back-chat", "Back to chat")
                    .max_w(px(180.))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.pane = Pane::Chat;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    fn audio_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.mixer.input_mode;
        div()
            .w_full()
            .max_w(px(520.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(heading("Levels & transmission"))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .items_center()
                    .child(muted(format!(
                        "Microphone {}%",
                        (self.mixer.input_gain * 100.).round()
                    )))
                    .child(
                        button("input-down", "−").on_click(cx.listener(|this, _, _, cx| {
                            this.mixer.input_gain = (this.mixer.input_gain - 0.1).max(0.);
                            this.command(Command::SetMixer(this.mixer.clone()), cx);
                        })),
                    )
                    .child(
                        button("input-up", "+").on_click(cx.listener(|this, _, _, cx| {
                            this.mixer.input_gain = (this.mixer.input_gain + 0.1).min(2.);
                            this.command(Command::SetMixer(this.mixer.clone()), cx);
                        })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap_3()
                    .items_center()
                    .child(muted(format!(
                        "Output {}%",
                        (self.mixer.output_gain * 100.).round()
                    )))
                    .child(
                        button("output-down", "−").on_click(cx.listener(|this, _, _, cx| {
                            this.mixer.output_gain = (this.mixer.output_gain - 0.1).max(0.);
                            this.command(Command::SetMixer(this.mixer.clone()), cx);
                        })),
                    )
                    .child(
                        button("output-up", "+").on_click(cx.listener(|this, _, _, cx| {
                            this.mixer.output_gain = (this.mixer.output_gain + 0.1).min(2.);
                            this.command(Command::SetMixer(this.mixer.clone()), cx);
                        })),
                    ),
            )
            .child(self.mode_picker.clone())
            .when(mode == InputMode::VoiceActivity, |v| {
                v.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(muted(format!(
                            "Activation threshold: {:.0} dB",
                            self.mixer.voice_activation_threshold_db
                        )))
                        .child(button("threshold-down", "−5 dB").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.mixer.voice_activation_threshold_db =
                                    (this.mixer.voice_activation_threshold_db - 5.).max(-96.);
                                this.command(Command::SetMixer(this.mixer.clone()), cx);
                            },
                        )))
                        .child(button("threshold-up", "+5 dB").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.mixer.voice_activation_threshold_db =
                                    (this.mixer.voice_activation_threshold_db + 5.).min(0.);
                                this.command(Command::SetMixer(this.mixer.clone()), cx);
                            },
                        ))),
                )
            })
            .when(mode == InputMode::PushToTalk, |v| {
                v.child(muted(
                    "Hold F8 while this window is active, or hold the Talk button.",
                ))
                .child(
                    button("ptt-held", "Hold to talk")
                        .bg(rgb(0x6650bd))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.command(Command::SetPushToTalkPressed(true), cx)
                            }),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.command(Command::SetPushToTalkPressed(false), cx)
                            }),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.command(Command::SetPushToTalkPressed(false), cx)
                            }),
                        ),
                )
            })
            .child(
                button(
                    "mic-test",
                    if self.testing_microphone {
                        "Stop microphone test"
                    } else {
                        "Test microphone (meter only)"
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.testing_microphone = !this.testing_microphone;
                    this.command(
                        if this.testing_microphone {
                            Command::StartMicrophoneTest
                        } else {
                            Command::StopMicrophoneTest
                        },
                        cx,
                    );
                })),
            )
            .child(muted(format!(
                "Input level {:.0}% · Output level {:.0}%",
                self.levels.0 * 100.,
                self.levels.1 * 100.
            )))
            .child(
                div()
                    .h(px(6.))
                    .w_full()
                    .rounded_md()
                    .bg(rgb(0x353142))
                    .child(
                        div()
                            .h_full()
                            .w(relative(self.levels.0.clamp(0., 1.)))
                            .rounded_md()
                            .bg(rgb(0x6bc99d)),
                    ),
            )
            .when(!self.participants.is_empty(), |v| {
                v.child(heading("Participant volume"))
            })
            .children(
                self.participants
                    .iter()
                    .map(|(identity, (name, settings))| {
                        let id = identity.clone();
                        let down = id.clone();
                        let up = id.clone();
                        div()
                            .flex()
                            .gap_3()
                            .items_center()
                            .child(format!(
                                "{} · {:.0}%",
                                if name.is_empty() { identity } else { name },
                                settings.gain * 100.
                            ))
                            .child(
                                button(SharedString::from(format!("participant-down-{id}")), "−")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some((_, settings)) =
                                            this.participants.get_mut(&down)
                                        {
                                            settings.gain = (settings.gain - 0.1).max(0.);
                                            let settings = *settings;
                                            this.command(
                                                Command::SetParticipantAudio {
                                                    identity: down.clone(),
                                                    settings,
                                                },
                                                cx,
                                            );
                                        }
                                    })),
                            )
                            .child(
                                button(SharedString::from(format!("participant-up-{id}")), "+")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some((_, settings)) = this.participants.get_mut(&up)
                                        {
                                            settings.gain = (settings.gain + 0.1).min(2.);
                                            let settings = *settings;
                                            this.command(
                                                Command::SetParticipantAudio {
                                                    identity: up.clone(),
                                                    settings,
                                                },
                                                cx,
                                            );
                                        }
                                    })),
                            )
                            .child(
                                button(
                                    SharedString::from(format!("participant-mute-{id}")),
                                    if settings.muted { "Unmute" } else { "Mute" },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if let Some((_, settings)) = this.participants.get_mut(&id)
                                        {
                                            settings.muted = !settings.muted;
                                            let settings = *settings;
                                            this.command(
                                                Command::SetParticipantAudio {
                                                    identity: id.clone(),
                                                    settings,
                                                },
                                                cx,
                                            );
                                        }
                                    },
                                )),
                            )
                    }),
            )
            .into_any_element()
    }

    fn shared_audio_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let selection = self.share_audio_picker.read(cx).selected().and_then(|id| {
            if id == "system" {
                Some(SharedAudioSelection::System)
            } else {
                id.strip_prefix("app:")
                    .filter(|pid| self.applications.iter().any(|app| app.process_id == *pid))
                    .map(|pid| SharedAudioSelection::Application {
                        process_id: pid.into(),
                    })
            }
        });
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(heading("Share audio"))
            .child(muted(if self.shared_audio {
                "Audio sharing is active"
            } else {
                "No audio is being shared"
            }))
            .child(self.share_audio_picker.clone())
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .when_some(selection, |view, selection| {
                        view.child(button("start-shared-audio", "Share audio").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.command(Command::StartSharedAudio(selection.clone()), cx);
                            }),
                        ))
                    })
                    .child(
                        button("refresh-apps", "Refresh applications").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.command(Command::RefreshAudioApplications, cx);
                            },
                        )),
                    )
                    .when(self.shared_audio, |view| {
                        view.child(button("stop-shared-audio", "Stop sharing audio").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.command(Command::StopSharedAudio, cx);
                            }),
                        ))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_3()
                    .items_center()
                    .child(muted(format!(
                        "Shared audio {:.0}%",
                        self.shared_audio_settings.gain * 100.
                    )))
                    .child(button("share-gain-down", "−").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.shared_audio_settings.gain =
                                (this.shared_audio_settings.gain - 0.1).max(0.);
                            this.command(Command::SetSharedAudio(this.shared_audio_settings), cx);
                        },
                    )))
                    .child(
                        button("share-gain-up", "+").on_click(cx.listener(|this, _, _, cx| {
                            this.shared_audio_settings.gain =
                                (this.shared_audio_settings.gain + 0.1).min(2.);
                            this.command(Command::SetSharedAudio(this.shared_audio_settings), cx);
                        })),
                    )
                    .child(
                        button(
                            "share-audio-mute",
                            if self.shared_audio_settings.muted {
                                "Unmute shared audio"
                            } else {
                                "Mute shared audio"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.shared_audio_settings.muted = !this.shared_audio_settings.muted;
                            this.command(Command::SetSharedAudio(this.shared_audio_settings), cx);
                        })),
                    ),
            )
            .into_any_element()
    }

    fn screens(&self, cx: &mut Context<Self>) -> AnyElement {
        let system_picker = self
            .sources
            .iter()
            .find(|source| source.kind == ScreenSourceKind::SystemPicker)
            .cloned();
        let selected = self
            .screen_picker
            .read(cx)
            .selected()
            .and_then(|id| {
                self.sources.iter().find(|source| {
                    source.kind != ScreenSourceKind::SystemPicker && screen_source_id(source) == id
                })
            })
            .cloned();
        div()
            .id("screen-sources")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(heading("Share your screen"))
            .child(muted(if system_picker.is_some() {
                "Choose your quality, then select a display or window in the system dialog."
            } else {
                "Choose a display or window and your sharing quality."
            }))
            .when(system_picker.is_none(), |view| {
                view.child(self.screen_picker.clone())
            })
            .child(self.fps_picker.clone())
            .child(self.resolution_picker.clone())
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .when_some(system_picker, |view, source| {
                        view.child(
                            button("choose-screen-source", "Choose display or window…")
                                .bg(rgb(0x6f56d7))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.start_screen_share(source.clone(), cx);
                                })),
                        )
                    })
                    .when_some(selected, |view, source| {
                        view.child(
                            button("start-screen-share", "Share screen")
                                .bg(rgb(0x6f56d7))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.start_screen_share(source.clone(), cx);
                                })),
                        )
                    })
                    .child(
                        button("refresh-sources", "Refresh sources").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.command(Command::RefreshScreenSources, cx);
                            },
                        )),
                    ),
            )
            .child(self.shared_audio_controls(cx))
            .child(button("cancel-share", "Back to chat").on_click(cx.listener(
                |this, _, _, cx| {
                    this.pane = Pane::Chat;
                    cx.notify();
                },
            )))
            .into_any_element()
    }

    fn start_screen_share(&mut self, source: ScreenSource, cx: &mut Context<Self>) {
        self.command(
            Command::StartShare(ScreenShareOptions {
                source,
                fps: self.share_fps,
                max_width: self.share_size.0,
                max_height: self.share_size.1,
                max_bitrate: if self.share_size.1 > 1080 {
                    8_000_000
                } else if self.share_fps > 30 {
                    6_000_000
                } else {
                    4_000_000
                },
            }),
            cx,
        );
        // The first captured frame, rather than this request, marks sharing active.
        self.pane = Pane::Chat;
    }

    fn search(&self, cx: &mut Context<Self>) -> AnyElement {
        let query = self.search.read(cx).content.to_lowercase();
        let results = self
            .state
            .messages
            .values()
            .flat_map(|m| m.values())
            .filter(|m| !query.is_empty() && m.content.to_lowercase().contains(&query))
            .take(100)
            .collect::<Vec<_>>();
        div()
            .id("search-results")
            .flex_1()
            .h_full()
            .min_w_0()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(heading("Search loaded messages"))
            .child(self.search.clone())
            .child(muted(
                "Search currently loaded history. Server-wide search is pending.",
            ))
            .children(results.into_iter().map(|message| {
                let id = message.channel_id;
                button(
                    ("result", message.id.0),
                    format!("{}: {}", message.author.display_name(), message.content),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.select(id, cx)))
            }))
            .child(button("close-search", "Back to chat").on_click(cx.listener(
                |this, _, _, cx| {
                    this.pane = Pane::Chat;
                    cx.notify();
                },
            )))
            .into_any_element()
    }
}

impl Render for Fluxer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let render_started = std::time::Instant::now();
        self.video_stats.rendered(
            window,
            self.pane == Pane::Call
                && self
                    .viewer
                    .focused
                    .as_ref()
                    .is_some_and(|identity| self.viewer.frames.contains_key(identity)),
        );
        let content = if self.session_restoring {
            self.restoring_view(cx)
        } else if self.state.current_user.is_none() {
            self.login_view(cx)
        } else {
            let body = match self.pane {
                Pane::Chat => self.chat(cx),
                Pane::Call => self.call_view(window, cx),
                Pane::Settings => self.settings(cx),
                Pane::Screens => self.screens(cx),
                Pane::Search => self.search(cx),
            };
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .when(
                    !(self.pane == Pane::Call && (self.viewer.expanded || window.is_fullscreen())),
                    |view| view.child(self.rail(cx)).child(self.channels(cx)),
                )
                .child(body)
                .into_any_element()
        };
        let rendered = div().size_full().flex().flex_col().bg(rgb(BG)).text_color(rgb(TEXT)).font_family("Inter").text_size(px(14.))
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(|this, _: &StopWatching, _, cx| this.stop_focused_stream(cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.pane = Pane::Settings;
                this.command(Command::RefreshDevices, cx);
                this.input_picker.update(cx, |picker, cx| picker.focus(window, cx));
            }))
            .on_key_down(cx.listener(|this,event:&KeyDownEvent,window,cx|{
                if event.keystroke.key == "escape" && this.pane == Pane::Call {
                    if window.is_fullscreen() {
                        window.toggle_fullscreen();
                    } else if this.viewer.expanded {
                        this.viewer.expanded = false;
                        cx.notify();
                    }
                    cx.stop_propagation();
                    return;
                }
                if event.keystroke.key == "tab" {
                    if event.keystroke.modifiers.shift { window.focus_prev(); } else { window.focus_next(); }
                    cx.stop_propagation();
                }
                if event.keystroke.key=="f8" && !event.is_held && this.mixer.input_mode==InputMode::PushToTalk {this.command(Command::SetPushToTalkPressed(true),cx);}
            }))
            .on_key_up(cx.listener(|this,event:&KeyUpEvent,_,cx|{
                if event.keystroke.key=="f8" {this.command(Command::SetPushToTalkPressed(false),cx);}
            }))
            .when(self.demo,|v|v.child(div().px_4().py_1().bg(rgb(0x4c3b22)).text_size(px(12.)).child(if self.demo_stream { "OFFLINE VIEWER FIXTURE · Synthetic images and participants · No call, capture, or session restore" } else { "OFFLINE DEMO · Local sample messages · Calls require a connected account" })))
            .when(self.error.is_some(),|v|v.child(div().p_3().bg(rgb(0x613342)).flex().items_center().justify_between().child(self.error.clone().unwrap_or_default()).child(button("dismiss-error","Dismiss").on_click(cx.listener(|this,_,_,cx|{this.error=None;cx.notify();})))))
            .child(content);
        self.video_stats.render_built(render_started.elapsed());
        rendered
    }
}

fn main() {
    let demo_stream =
        std::env::args().any(|v| v == "--demo-stream" || v == "--demo-stream-animated");
    let demo = demo_stream || std::env::args().any(|v| v == "--demo");
    Application::new().run(move |cx: &mut App| {
        input::init(cx);
        cx.bind_keys([
            KeyBinding::new("enter", Submit, Some("TextInput")),
            KeyBinding::new("ctrl-q", Quit, None),
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("ctrl-,", OpenSettings, None),
            KeyBinding::new("cmd-,", OpenSettings, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
        let window_title = format!(
            "Fluxer · Native · {}",
            option_env!("FLUXER_BUILD_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
        );
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(window_title.clone().into()),
                    ..Default::default()
                }),
                window_min_size: Some(size(px(880.), px(600.))),
                app_id: Some("app.fluxer.Native".into()),
                ..Default::default()
            },
            move |window, cx| {
                window.set_window_title(&window_title);
                cx.new(|cx| Fluxer::new(demo, demo_stream, window, cx))
            },
        )
        .expect("open Fluxer window");
        cx.activate(true);
    });
}
