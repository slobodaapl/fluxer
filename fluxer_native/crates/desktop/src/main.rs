// SPDX-License-Identifier: AGPL-3.0-or-later
mod attachment_composer;
mod audio_view;
mod avatar_demo;
mod avatar_images;
mod call_view;
mod chat_helpers;
mod chat_view;
mod controls;
mod dropdown;
mod emoji_picker;
mod emoji_sprites;
mod input;
mod message_actions;
mod message_content;
mod message_demo;
mod message_grouping;
mod message_links;
mod message_markup;
mod rich_message;
mod search_browser;
mod stream_view;
mod video_perf;

use attachment_composer::AttachmentUi;
use call_view::{StreamFrame, Viewer};
use dropdown::{Dropdown, Selected};
use emoji_picker::EmojiPickerState;
use fluxer_client::{Command, Controller, Event, VoiceStatus};
use fluxer_core::{
    ChannelType, ClientState, Message, Snowflake, VoiceParticipant, can_send_message,
};
use fluxer_media::{
    AudioApplication, AudioMixSettings, AudioSettings, InputMode, MediaEvent,
    ParticipantAudioSettings, ScreenShareOptions, ScreenSource, ScreenSourceKind,
    SharedAudioSelection, SharedAudioSettings,
};
use gpui::{prelude::*, *};
use input::TextInput;
use message_actions::MessageActions;
use message_content::MessageContent;
use message_links::MessageLinks;
use search_browser::{BrowserTab, SearchUi};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use video_perf::{AnimatedFixture, VideoStats};

actions!(
    fluxer,
    [
        Submit,
        Quit,
        OpenSettings,
        OpenSearch,
        StopWatching,
        PickerLeft,
        PickerRight,
        PickerUp,
        PickerDown,
        PickerConfirm,
        PickerDismiss
    ]
);

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
        .focusable()
        .tab_stop(true)
        .text_size(px(13.))
        .hover(|style| style.bg(rgb(0x484054)))
        .focus(|style| style.bg(rgb(0x484054)))
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

#[derive(Clone, Copy, PartialEq)]
enum EmojiTarget {
    Composer,
    Reaction(Snowflake),
}

struct Fluxer {
    focus: FocusHandle,
    controller: Option<Controller>,
    state: ClientState,
    demo: bool,
    message_demo_started: bool,
    avatar_demo_endpoint: Option<String>,
    session_restoring: bool,
    session_restore_failed: bool,
    session_context_loaded: bool,
    pane: Pane,
    selected_guild: Option<Snowflake>,
    composer: Entity<TextInput>,
    message_actions: MessageActions,
    message_content: MessageContent,
    message_links: MessageLinks,
    message_text: BTreeMap<Snowflake, Entity<TextInput>>,
    media_endpoint: Option<String>,
    replies: BTreeMap<Snowflake, (Message, bool)>,
    pending_replies: BTreeMap<Snowflake, (Snowflake, bool)>,
    attachments: AttachmentUi,
    emoji_target: Option<EmojiTarget>,
    emoji_search: Entity<TextInput>,
    emoji_category: String,
    emoji_index: usize,
    emoji_scroll: UniformListScrollHandle,
    emoji_picker: EmojiPickerState,
    forwarding: Option<Message>,
    forward_search: Entity<TextInput>,
    forward_pending: bool,
    forward_index: usize,
    forward_scroll: ScrollHandle,
    instance: Entity<TextInput>,
    identifier: Entity<TextInput>,
    password: Entity<TextInput>,
    search: Entity<TextInput>,
    search_ui: SearchUi,
    mfa: Entity<TextInput>,
    mfa_ticket: Option<String>,
    ip_authorization: Option<IpAuthorization>,
    last_ip_countdown: u64,
    browser_login: Option<BrowserLogin>,
    error: Option<String>,
    status: String,
    voice_status: String,
    voice_connecting: bool,
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
    edit_draft: Option<String>,
    mixer: AudioMixSettings,
    participants: BTreeMap<String, (String, ParticipantAudioSettings)>,
    active_speakers: BTreeSet<String>,
    available_streams: BTreeSet<String>,
    applications: Vec<AudioApplication>,
    shared_audio: bool,
    share_audio_enabled: bool,
    shared_audio_settings: SharedAudioSettings,
    testing_microphone: bool,
    levels: (f32, f32),
    pending_messages: BTreeMap<Snowflake, String>,
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
            message_demo_started: false,
            avatar_demo_endpoint: None,
            session_restoring,
            session_restore_failed: false,
            session_context_loaded: false,
            pane: Pane::Chat,
            selected_guild: None,
            composer: cx.new(|cx| {
                let mut input = TextInput::new("Message this channel…", cx);
                input.multiline = true;
                input.attachment_paste = true;
                input
            }),
            message_actions: MessageActions::new(cx),
            message_content: MessageContent::default(),
            message_links: MessageLinks::new(cx),
            message_text: BTreeMap::new(),
            media_endpoint: None,
            replies: BTreeMap::new(),
            pending_replies: BTreeMap::new(),
            attachments: AttachmentUi::new(),
            emoji_target: None,
            emoji_search: cx.new(|cx| {
                let mut input = TextInput::new("Search emoji…", cx);
                input.chat_picker = true;
                input
            }),
            emoji_category: "people".into(),
            emoji_index: 0,
            emoji_scroll: UniformListScrollHandle::new(),
            emoji_picker: EmojiPickerState::new(),
            forwarding: None,
            forward_search: cx.new(|cx| {
                let mut input = TextInput::new("Search destinations…", cx);
                input.chat_picker = true;
                input
            }),
            forward_pending: false,
            forward_index: 0,
            forward_scroll: ScrollHandle::new(),
            instance,
            identifier: cx.new(|cx| TextInput::new("Email or login", cx)),
            password,
            search: cx.new(|cx| TextInput::new("Search messages…", cx)),
            search_ui: SearchUi::new(window, cx),
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
            voice_connecting: false,
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
            edit_draft: None,
            mixer: AudioMixSettings::default(),
            participants: BTreeMap::new(),
            active_speakers: BTreeSet::new(),
            available_streams: BTreeSet::new(),
            applications: Vec::new(),
            shared_audio: false,
            share_audio_enabled: cfg!(target_os = "linux"),
            shared_audio_settings: SharedAudioSettings::default(),
            testing_microphone: false,
            levels: (0., 0.),
            pending_messages: BTreeMap::new(),
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
        cx.subscribe(&view.search_ui.scope, |this, _, _: &Selected, cx| {
            this.change_search_scope(cx)
        })
        .detach();
        cx.subscribe(&view.search_ui.sort, |this, _, _: &Selected, cx| {
            this.change_search_sort(cx)
        })
        .detach();
        cx.observe(&view.search, |_, _, cx| cx.notify()).detach();
        cx.observe(&view.emoji_search, |this, _, cx| {
            this.refresh_emoji_results(cx);
        })
        .detach();
        cx.observe(&view.forward_search, |this, _, cx| {
            this.forward_index = 0;
            this.forward_scroll.scroll_to_item(0);
            cx.notify();
        })
        .detach();
        cx.subscribe(&view.composer, |this, _, image: &input::ImagePasted, cx| {
            this.stage_clipboard_image(image, cx);
        })
        .detach();
        cx.observe(&view.composer, |this, _, cx| {
            let content = this.composer.read(cx).content.to_string();
            if this.state.selected_channel.is_some() && this.editing.is_none() {
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
                Event::MediaEndpoint(endpoint) => self.media_endpoint = endpoint,
                Event::InstancePresentation(document) => {
                    avatar_images::configure_instance(document.as_ref());
                    if let Some(document) = document {
                        self.message_links.configure_instance(&document);
                    } else {
                        self.message_links.reset();
                    }
                }
                Event::MessageForwarded { destination } => {
                    self.forwarding = None;
                    self.forward_pending = false;
                    self.status =
                        format!("Message forwarded to {}", self.channel_name(destination));
                }
                Event::State(state) => {
                    let old = self.state.selected_channel;
                    let was_empty = old.is_none_or(|channel| {
                        self.state.channel_messages(channel).next().is_none()
                    });
                    let previous_latest = old.and_then(|channel| {
                        self.state
                            .channel_messages(channel)
                            .next_back()
                            .map(|message| message.id)
                    });
                    let near_bottom =
                        self.timeline.max_offset().height + self.timeline.offset().y <= px(48.);
                    self.state = *state;
                    self.sharing = self.state.voice.screen_share;
                    if self.state.current_user.is_none() && self.state.guilds.is_empty() {
                        // A restore can publish an empty transition before its
                        // validated snapshot. Preserve its explicit context;
                        // logout and ordinary session resets clear stale UI.
                        if !self.session_restoring {
                            self.selected_guild = None;
                            self.session_context_loaded = false;
                            self.replies.clear();
                            self.message_text.clear();
                            self.clear_browser();
                            self.message_actions.reset();
                            self.message_content.reset();
                            // Authentication may publish an empty transition after
                            // discovery. Drop private UI while retaining public routing.
                            self.message_links.clear_prompt();
                            self.media_endpoint = None;
                            self.forwarding = None;
                            self.emoji_target = None;
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
                        self.edit_draft = None;
                        self.emoji_target = None;
                        self.forwarding = None;
                    }
                    self.refresh_open_emoji_catalog(cx);
                    let latest = self.state.selected_channel.and_then(|channel| {
                        self.state
                            .channel_messages(channel)
                            .next_back()
                            .map(|message| message.id)
                    });
                    if self.search_ui.pending_center.is_none()
                        && self
                            .search_ui
                            .histories
                            .get(&self.state.selected_channel.unwrap_or_default())
                            .is_none_or(|history| history.latest)
                        && (old != self.state.selected_channel
                            || was_empty
                            || (near_bottom && previous_latest != latest))
                        && self.state.selected_channel.is_some_and(|channel| {
                            self.state.channel_messages(channel).next().is_some()
                        })
                    {
                        self.timeline.scroll_to_bottom();
                    }
                    if self.state.current_user.is_some() {
                        self.mfa_ticket = None;
                        self.password.update(cx, |v, cx| v.set(String::new(), cx));
                    }
                    if self.demo && std::env::args().any(|arg| arg == "--demo-attachments") {
                        self.install_attachment_fixture(cx);
                    }
                    self.maybe_demo_search(None, cx);
                    self.maybe_demo_messages(cx);
                    self.maybe_demo_avatars(cx);
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
                    self.forward_pending = false;
                    self.error = Some(error);
                }
                Event::Status(status) => self.status = status,
                Event::Demo(demo) => {
                    self.demo = demo;
                    if demo {
                        self.maybe_demo_search(None, cx);
                        self.maybe_demo_messages(cx);
                        if std::env::args().any(|arg| arg == "--demo-attachments") {
                            self.install_attachment_fixture(cx);
                        }
                        self.session_restoring = false;
                        self.session_restore_failed = false;
                        self.session_context_loaded = false;
                        if std::env::args().any(|arg| arg == "--demo-emoji") {
                            self.schedule_emoji_demo(cx);
                        }
                        if self.demo_stream {
                            self.install_stream_fixture(cx);
                            if self.animation.is_some() {
                                self.retire_stream_image("preview-1", cx);
                                self.viewer.stop("preview-1");
                            }
                        }
                        self.maybe_demo_avatars(cx);
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
                    self.voice_connecting = matches!(
                        status,
                        VoiceStatus::Connecting { .. } | VoiceStatus::Reconnecting
                    );
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
                Event::MessageEdited {
                    channel_id,
                    content,
                    ..
                } => {
                    if self.complete_pending_message(channel_id, &content)
                        && attachment_composer::sent_draft_can_clear(
                            self.state.selected_channel,
                            channel_id,
                            &content,
                            self.composer.read(cx).content.as_ref(),
                        )
                    {
                        self.cancel_edit(cx);
                    }
                }
                Event::MessageSent {
                    channel_id,
                    content,
                } => {
                    if self.complete_pending_message(channel_id, &content) {
                        if let Some((message, mention)) = self.pending_replies.remove(&channel_id)
                            && self.replies.get(&channel_id).is_some_and(
                                |(reply, current_mention)| {
                                    reply.id == message && *current_mention == mention
                                },
                            )
                        {
                            self.replies.remove(&channel_id);
                        }
                        if attachment_composer::sent_draft_can_clear(
                            self.state.selected_channel,
                            channel_id,
                            &content,
                            self.composer.read(cx).content.as_ref(),
                        ) {
                            self.composer
                                .update(cx, |input, cx| input.set(String::new(), cx));
                            self.editing = None;
                        }
                        if self.state.selected_channel == Some(channel_id)
                            && self
                                .search_ui
                                .histories
                                .get(&channel_id)
                                .is_none_or(|history| history.latest)
                        {
                            self.timeline.scroll_to_bottom();
                        }
                    }
                }
                Event::MessageOperationFailed {
                    channel_id,
                    content,
                    error,
                } => {
                    if self.complete_pending_message(channel_id, &content) {
                        self.pending_replies.remove(&channel_id);
                    }
                    self.error = Some(error);
                }
                Event::Attachments {
                    channel_id,
                    attachments,
                } => {
                    self.update_attachments(channel_id, attachments, cx);
                }
                Event::AttachmentLimits {
                    channel_id,
                    max_files,
                    max_file_bytes,
                    preupload,
                } => {
                    self.attachments.limits.insert(
                        channel_id,
                        attachment_composer::Limits {
                            max_files,
                            max_file_bytes,
                            preupload,
                        },
                    );
                }
                Event::AttachmentsCleared => {
                    self.clear_attachments(cx);
                    self.pending_messages.clear();
                    self.pending_replies.clear();
                }
                Event::MessageAckResult {
                    channel_id,
                    message_id,
                    error,
                } => self.on_message_ack(channel_id, message_id, error),
                Event::Search(snapshot) => self.on_search_snapshot(snapshot, cx),
                Event::Pins(snapshot) => self.on_pins_snapshot(snapshot, cx),
                Event::History(snapshot) => self.on_history_snapshot(snapshot, cx),
                Event::PinMutation {
                    channel_id,
                    message_id,
                    pinned,
                    pending,
                    error,
                } => {
                    if pending {
                        self.search_ui.mutations.insert((channel_id, message_id));
                    } else {
                        self.search_ui.mutations.remove(&(channel_id, message_id));
                    }
                    if let Some(error) = error {
                        self.search_ui.mutation_error = Some(error.clone());
                        self.error = Some(error);
                    } else if !pending {
                        self.search_ui.mutation_error = None;
                        self.confirmed_pin(channel_id, message_id, pinned);
                    }
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
                    MediaEvent::Error(error) if !self.viewer.fail_pending(error.clone()) => {
                        self.error = Some(error);
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

    fn submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        if self.message_actions.is_open() || self.message_links.is_open() {
            return;
        }
        if self.search_ui.confirm.is_some() {
            return;
        }
        if self.search_ui.open == Some(BrowserTab::Search)
            && self.search.focus_handle(cx).is_focused(window)
        {
            self.run_search(1, cx);
            return;
        }
        if self.session_restoring {
            return;
        }
        if self.state.current_user.is_none() {
            self.login(cx);
            return;
        }
        if !(self.pane == Pane::Chat || (self.pane == Pane::Call && self.viewer.chat_open))
            || !self.composer.focus_handle(cx).is_focused(window)
            || self.message_pending()
            || self.emoji_target.is_some()
            || self.forwarding.is_some()
        {
            return;
        }
        let content = self.composer.read(cx).content.to_string();
        if content.trim().is_empty() && (self.editing.is_some() || !self.has_attachments()) {
            return;
        }
        if let Some(channel) = self.state.selected_channel {
            self.pending_messages.insert(channel, content.clone());
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
            let reply = self
                .state
                .selected_channel
                .and_then(|channel| self.replies.get(&channel));
            let reply_to = reply.map(|(message, _)| message.id);
            let mention_author = reply.is_some_and(|(_, mention)| *mention);
            if let Some((channel, reply)) = self.state.selected_channel.zip(reply_to) {
                self.pending_replies
                    .insert(channel, (reply, mention_author));
            }
            self.command(
                Command::SendMessage {
                    content,
                    reply_to,
                    mention_author,
                },
                cx,
            );
        }
    }

    fn channel_name(&self, id: Snowflake) -> String {
        self.state
            .channels
            .get(&id)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "voice channel".into())
    }

    fn cancel_edit(&mut self, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            let draft = self.edit_draft.take().unwrap_or_default();
            self.composer.update(cx, |input, cx| input.set(draft, cx));
        }
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
        self.reset_ack_context(id);
        self.search_ui.return_to_latest_on_select(id);
        if self.state.selected_channel == Some(id) {
            self.pane = Pane::Chat;
            cx.notify();
            return;
        }
        self.cancel_edit(cx);
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
                controls::icon_button("home", controls::Icon::Home, "Direct messages")
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
                button_base(("guild", id.0))
                    .size(px(44.))
                    .p_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .focusable()
                    .border_1()
                    .border_color(transparent_black())
                    .focus(|style| style.border_color(rgb(ACCENT)))
                    .rounded_md()
                    .overflow_hidden()
                    .when(self.selected_guild == Some(id), |v| {
                        v.bg(rgb(0x6650bd)).border_color(rgb(ACCENT))
                    })
                    .child(avatar_images::community_icon(
                        guild,
                        self.media_endpoint.as_deref(),
                        42.,
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_guild = Some(id);
                        this.command(Command::SelectGuild { guild_id: id }, cx);
                    }))
            }))
            .child(div().flex_1())
            .into_any_element()
    }

    fn logout(&mut self, cx: &mut Context<Self>) {
        self.command(Command::Logout, cx);
        self.selected_guild = None;
        self.session_context_loaded = false;
        self.session_restore_failed = false;
        self.session_restoring = false;
        self.mfa_ticket = None;
        self.pending_messages.clear();
        self.pending_replies.clear();
        self.clear_attachments(cx);
        self.message_actions.reset();
        self.message_content.reset();
        self.message_links.reset();
        self.editing = None;
        self.clear_viewer(cx);
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
                                            .child(avatar_images::voice_avatar(
                                                &participant,
                                                channel.guild_id,
                                                self.media_endpoint.as_deref(),
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
            .into_any_element()
    }

    fn pick_emoji(
        &mut self,
        choice: &chat_helpers::EmojiChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(target) = self.emoji_target.take() {
            match target {
                EmojiTarget::Composer => self.composer.update(cx, |input, cx| {
                    input.insert_text(&choice.insert, window, cx)
                }),
                EmojiTarget::Reaction(message_id) => self.command(
                    Command::React {
                        message_id,
                        emoji: choice.reaction.clone(),
                    },
                    cx,
                ),
            }
        }
        self.composer.focus_handle(cx).focus(window);
        cx.notify();
    }

    fn forward_destinations(&self, cx: &App) -> Vec<Snowflake> {
        let query = self.forward_search.read(cx).content.to_lowercase();
        let me = self.state.current_user.as_ref().map(|user| user.id);
        self.state
            .channels
            .values()
            .filter(|channel| {
                channel.display_name().to_lowercase().contains(&query)
                    && me.is_some_and(|me| {
                        can_send_message(channel.kind, self.state.permissions_for(channel.id, me))
                    })
            })
            .map(|channel| channel.id)
            .collect()
    }

    fn pick_forward(&mut self, destination: Snowflake, cx: &mut Context<Self>) {
        if self.forward_pending {
            return;
        }
        if let Some(source) = self.forwarding.as_ref() {
            let source_channel = source.channel_id;
            let message_id = source.id;
            self.forward_pending = true;
            self.command(
                Command::ForwardMessage {
                    source_channel,
                    message_id,
                    destination,
                },
                cx,
            );
        }
    }

    fn picker_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.emoji_target.is_none() && self.forwarding.is_none() {
            return;
        }
        cx.stop_propagation();
        if key == "escape" {
            self.emoji_target = None;
            if !self.forward_pending {
                self.forwarding = None;
            }
            self.composer.focus_handle(cx).focus(window);
        } else if self.emoji_target.is_some() {
            let choices = self.emoji_picker.choices.clone();
            self.emoji_index = self.emoji_index.min(choices.len().saturating_sub(1));
            if key == "enter" {
                if let Some(choice) = choices.get(self.emoji_index) {
                    self.pick_emoji(choice, window, cx);
                }
            } else {
                self.emoji_index = match key {
                    "left" => self.emoji_index.saturating_sub(1),
                    "right" => (self.emoji_index + 1).min(choices.len().saturating_sub(1)),
                    "up" => self.emoji_index.saturating_sub(8),
                    _ => (self.emoji_index + 8).min(choices.len().saturating_sub(1)),
                };
                self.emoji_scroll.scroll_to_item(
                    self.emoji_index / emoji_picker::COLUMNS,
                    ScrollStrategy::Top,
                );
            }
        } else {
            let destinations = self.forward_destinations(cx);
            self.forward_index = self.forward_index.min(destinations.len().saturating_sub(1));
            if key == "enter" {
                if let Some(destination) = destinations.get(self.forward_index) {
                    self.pick_forward(*destination, cx);
                }
            } else {
                self.forward_index = if matches!(key, "up" | "left") {
                    self.forward_index.saturating_sub(1)
                } else {
                    (self.forward_index + 1).min(destinations.len().saturating_sub(1))
                };
                self.forward_scroll.scroll_to_item(self.forward_index);
            }
        }
        cx.notify();
    }

    fn forward_panel(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(source) = self.forwarding.as_ref() else {
            return div().into_any_element();
        };
        let destinations = self.forward_destinations(cx);
        div()
            .p_3()
            .bg(rgb(PANEL))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(heading(if self.forward_pending {
                        "Forwarding…"
                    } else {
                        "Forward message"
                    }))
                    .child(button("cancel-forward", "Cancel").on_click(cx.listener(
                        |this, _, _, cx| {
                            if !this.forward_pending {
                                this.forwarding = None;
                            }
                            cx.notify();
                        },
                    ))),
            )
            .child(muted(format!(
                "{}: {}",
                source.author.display_name(),
                chat_helpers::preview(source)
            )))
            .child(self.forward_search.clone())
            .child(muted("Choose a destination to forward this message"))
            .child(
                div()
                    .id("forward-destinations")
                    .track_scroll(&self.forward_scroll)
                    .h(px(150.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when(destinations.is_empty(), |view| {
                        view.child(muted("No writable destinations match"))
                    })
                    .children(
                        destinations
                            .into_iter()
                            .enumerate()
                            .map(|(index, destination)| {
                                let channel = &self.state.channels[&destination];
                                let community = channel
                                    .guild_id
                                    .and_then(|guild| self.state.guilds.get(&guild))
                                    .map(|guild| format!("{} / ", guild.name))
                                    .unwrap_or_default();
                                button(
                                    ("forward-destination", destination.0),
                                    format!("{community}{}", channel.display_name()),
                                )
                                .when(index == self.forward_index, |view| view.bg(rgb(0x56438b)))
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.pick_forward(destination, cx);
                                    },
                                ))
                            }),
                    ),
            )
            .into_any_element()
    }

    fn start_screen_share(&mut self, source: ScreenSource, cx: &mut Context<Self>) {
        let audio = if self.share_audio_enabled {
            let Some(selection) = self.selected_share_audio(cx) else {
                self.error = Some("Choose an available audio source before sharing.".into());
                cx.notify();
                return;
            };
            Some(selection)
        } else {
            None
        };
        self.command(
            Command::StartShare {
                options: ScreenShareOptions {
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
                },
                audio,
            },
            cx,
        );
        // The first captured frame, rather than this request, marks sharing active.
        self.pane = Pane::Chat;
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
                Pane::Chat => {
                    let chat = self.chat(window, cx);
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .min_h_0()
                        .flex()
                        .child(chat)
                        .when(self.search_ui.open.is_some(), |view| {
                            view.child(self.browser_pane(window, cx))
                        })
                        .into_any_element()
                }
                Pane::Call => self.call_view(window, cx),
                Pane::Settings => self.settings(cx),
                Pane::Screens => self.screens(cx),
            };
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .when(
                    !(self.pane == Pane::Call && (self.viewer.expanded || window.is_fullscreen())),
                    |view| {
                        view.child(self.rail(cx)).when(
                            !(self.pane == Pane::Chat
                                && self.search_ui.open.is_some()
                                && window.viewport_size().width < px(1100.)),
                            |view| view.child(self.channels(cx)),
                        )
                    },
                )
                .child(body)
                .into_any_element()
        };
        let rendered = div().size_full().flex().flex_col().bg(rgb(BG)).text_color(rgb(TEXT)).font_family("Fluxer Sans").text_size(px(14.))
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &PickerLeft, window, cx| this.picker_key("left", window, cx)))
            .on_action(cx.listener(|this, _: &PickerRight, window, cx| this.picker_key("right", window, cx)))
            .on_action(cx.listener(|this, _: &PickerUp, window, cx| this.picker_key("up", window, cx)))
            .on_action(cx.listener(|this, _: &PickerDown, window, cx| this.picker_key("down", window, cx)))
            .on_action(cx.listener(|this, _: &PickerConfirm, window, cx| this.picker_key("enter", window, cx)))
            .on_action(cx.listener(|this, _: &PickerDismiss, window, cx| this.picker_key("escape", window, cx)))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(|this,_:&OpenSearch,window,cx|{if this.search_ui.confirm.is_none(){this.open_search(window,cx);}}))
            .on_action(cx.listener(|this, _: &StopWatching, _, cx| this.stop_focused_stream(cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                this.pane = Pane::Settings;
                this.command(Command::RefreshDevices, cx);
                this.input_picker.update(cx, |picker, cx| picker.focus(window, cx));
            }))
            .on_key_down(cx.listener(|this,event:&KeyDownEvent,window,cx|{
                if event.keystroke.key=="escape" && this.message_links.is_open() {this.dismiss_message_link(window,cx);cx.stop_propagation();return;}
                if event.keystroke.key=="escape" && this.message_actions.is_blocking() {this.dismiss_message_actions(window,cx);cx.stop_propagation();return;}
                if this.search_ui.confirm.is_some() && event.keystroke.key=="escape" {
                    this.dismiss_pin_confirmation(window,cx);cx.stop_propagation();return;
                }
                if event.keystroke.key=="escape" && this.search_ui.open.is_some() && this.emoji_target.is_none() && this.forwarding.is_none(){this.close_browser(window,cx);cx.stop_propagation();return;}

                if event.keystroke.key == "escape" && (this.emoji_target.is_some() || this.forwarding.is_some()) {
                    this.emoji_target=None;
                    if !this.forward_pending { this.forwarding=None; }
                    this.composer.focus_handle(cx).focus(window);
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
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
            .when(self.demo,|v|v.child(div().px_4().py_1().bg(rgb(0x4c3b22)).text_size(px(12.)).child(if self.avatar_demo_endpoint.is_some() { "OFFLINE AVATAR DEMO · Local images and synthetic roster · No call or capture" } else if self.attachments.fixture { "OFFLINE ATTACHMENT DEMO · Local staging only · No files are uploaded" } else if self.demo_stream { "OFFLINE VIEWER FIXTURE · Synthetic images and participants · No call, capture, or session restore" } else { "OFFLINE DEMO · Local sample messages · Calls require a connected account" })))
            .when(self.error.is_some(),|v|v.child(div().px_3().py_1().gap_3().bg(rgb(0x613342)).flex().items_center().justify_between().child(div().flex_1().min_w_0().child(self.error.clone().unwrap_or_default())).child(controls::icon_button("dismiss-error",controls::Icon::Close,"Dismiss error").on_click(cx.listener(|this,_,_,cx|{this.error=None;cx.notify();})))))
            .child(content)
            .child(self.message_action_overlay(window,cx))
            .child(self.message_link_overlay(window,cx))
            .when(self.emoji_target.is_some(), |view| view.child(self.emoji_panel(window, cx)))
            .when(self.search_ui.confirm.is_some(),|view|view.child(self.pin_confirmation(cx)));
        self.schedule_history_center(window, cx);
        self.video_stats.render_built(render_started.elapsed());
        rendered
    }
}

fn main() {
    let demo_stream =
        std::env::args().any(|v| v == "--demo-stream" || v == "--demo-stream-animated");
    let demo = demo_stream
        || std::env::args().any(|v| {
            v == "--demo"
                || v == "--demo-emoji"
                || v == "--demo-attachments"
                || v == "--demo-search"
                || v == "--demo-messages"
                || v == "--demo-avatars"
        });
    Application::new()
        .with_assets(controls::Assets)
        .with_http_client(rich_message::http_client())
        .run(move |cx: &mut App| {
            cx.text_system()
                .add_fonts(controls::fonts())
                .expect("load bundled Fluxer fonts");
            input::init(cx);
            cx.bind_keys([
                KeyBinding::new("enter", Submit, Some("TextInput")),
                KeyBinding::new("left", PickerLeft, Some("ChatPicker")),
                KeyBinding::new("right", PickerRight, Some("ChatPicker")),
                KeyBinding::new("up", PickerUp, Some("ChatPicker")),
                KeyBinding::new("down", PickerDown, Some("ChatPicker")),
                KeyBinding::new("enter", PickerConfirm, Some("ChatPicker")),
                KeyBinding::new("escape", PickerDismiss, Some("ChatPicker")),
                KeyBinding::new("ctrl-q", Quit, None),
                KeyBinding::new("cmd-q", Quit, None),
                KeyBinding::new("ctrl-f", OpenSearch, None),
                KeyBinding::new("cmd-f", OpenSearch, None),
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
