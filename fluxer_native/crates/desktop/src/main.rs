// SPDX-License-Identifier: AGPL-3.0-or-later
mod input;

use fluxer_client::{Command, Controller, Event, VoiceStatus};
use fluxer_core::{ChannelType, ClientState, Snowflake};
use fluxer_media::{
    AudioApplication, AudioMixSettings, AudioSettings, DeviceInventory, InputMode, MediaEvent,
    ParticipantAudioSettings, ScreenShareOptions, ScreenSource, SharedAudioSelection,
    SharedAudioSettings,
};
use gpui::{prelude::*, *};
use input::TextInput;
use std::{collections::BTreeMap, sync::Arc, time::Duration};

actions!(fluxer, [Submit, Quit]);

const BG: u32 = 0x1d1b26;
const PANEL: u32 = 0x252330;
const RAIL: u32 = 0x16141e;
const ACCENT: u32 = 0x8873ee;
const TEXT: u32 = 0xece9f6;
const MUTED: u32 = 0xa19bb3;

#[derive(Clone, Copy, PartialEq)]
enum Pane {
    Chat,
    Settings,
    Screens,
    Search,
}

fn button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_md()
        .bg(rgb(0x353142))
        .cursor_pointer()
        .text_size(px(13.))
        .child(label.into())
        .hover(|s| s.bg(rgb(0x484054)))
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

struct Fluxer {
    controller: Option<Controller>,
    state: ClientState,
    demo: bool,
    pane: Pane,
    selected_guild: Option<Snowflake>,
    composer: Entity<TextInput>,
    instance: Entity<TextInput>,
    identifier: Entity<TextInput>,
    password: Entity<TextInput>,
    search: Entity<TextInput>,
    mfa: Entity<TextInput>,
    mfa_ticket: Option<String>,
    error: Option<String>,
    status: String,
    voice_status: String,
    devices: DeviceInventory,
    selected_input: Option<String>,
    selected_output: Option<String>,
    sources: Vec<ScreenSource>,
    share_fps: u32,
    share_size: (u32, u32),
    dsp: AudioSettings,
    sharing: bool,
    editing: Option<Snowflake>,
    video: BTreeMap<String, Arc<RenderImage>>,
    mixer: AudioMixSettings,
    participants: BTreeMap<String, (String, ParticipantAudioSettings)>,
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
    fn new(demo: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let controller = Controller::spawn();
        let error = controller.as_ref().err().cloned();
        let controller = controller.ok();
        let instance = cx.new(|cx| TextInput::new("Instance URL", cx));
        instance.update(cx, |v, cx| v.set("https://fluxer.app".into(), cx));
        let password = cx.new(|cx| {
            let mut v = TextInput::new("Password", cx);
            v.secret = true;
            v
        });
        let view = Self {
            controller,
            state: ClientState::default(),
            demo,
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
            error,
            status: "Sign in to your Fluxer instance".into(),
            voice_status: "Disconnected".into(),
            devices: DeviceInventory::default(),
            selected_input: None,
            selected_output: None,
            sources: Vec::new(),
            share_fps: 30,
            share_size: (1920, 1080),
            dsp: AudioSettings::default(),
            sharing: false,
            editing: None,
            video: BTreeMap::new(),
            mixer: AudioMixSettings::default(),
            participants: BTreeMap::new(),
            applications: Vec::new(),
            shared_audio: false,
            shared_audio_settings: SharedAudioSettings::default(),
            testing_microphone: false,
            levels: (0., 0.),
            pending_message: None,
            timeline: ScrollHandle::new(),
            last_typing: std::time::Instant::now() - Duration::from_secs(10),
        };
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
            }
        })
        .detach();
        if demo && let Some(c) = view.controller.as_ref() {
            let _ = c.send(Command::Demo);
        }
        cx.spawn(async move |entity, cx| {
            loop {
                Timer::after(Duration::from_millis(33)).await;
                if entity.update(cx, |view, cx| view.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
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

    fn poll(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        for _ in 0..128 {
            let Some(event) = self.controller.as_ref().and_then(|c| c.try_recv()) else {
                break;
            };
            changed = true;
            match event {
                Event::State(state) => {
                    let old = self.state.selected_channel;
                    self.state = *state;
                    self.sharing = self.state.voice.screen_share;
                    if self.selected_guild.is_none() {
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
                Event::Error(error) => {
                    self.error = Some(error);
                    self.pending_message = None;
                }
                Event::Status(status) => self.status = status,
                Event::Demo(demo) => self.demo = demo,
                Event::MfaRequired { ticket, .. } => {
                    self.mfa_ticket = Some(ticket);
                    self.status = "Enter your authenticator code".into();
                }
                Event::Devices(devices) => self.devices = devices,
                Event::ScreenSources(sources) => self.sources = sources,
                Event::VoiceStatus(status) => {
                    self.voice_status = match status {
                        VoiceStatus::Disconnected => {
                            self.video.clear();
                            self.participants.clear();
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
                        VoiceStatus::Reconnecting => "Reconnecting voice…".into(),
                    }
                }
                Event::AudioApplications(apps) => self.applications = apps,
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
                Event::Video(frame) => {
                    if let Some(buffer) = image::RgbaImage::from_raw(
                        frame.width,
                        frame.height,
                        frame.rgba.as_ref().clone(),
                    ) {
                        // GPUI's image renderer consumes BGRA pixels.
                        let mut buffer = buffer;
                        for p in buffer.pixels_mut() {
                            p.0.swap(0, 2);
                        }
                        let image =
                            RenderImage::new(smallvec::smallvec![image::Frame::new(buffer)]);
                        self.video.insert(frame.track_id, Arc::new(image));
                    }
                }
                Event::Media(event) => match event {
                    MediaEvent::ScreenShareStarted => self.sharing = true,
                    MediaEvent::ScreenShareStopped => self.sharing = false,
                    MediaEvent::VideoEnded { track_id } => {
                        self.video.remove(&track_id);
                    }
                    MediaEvent::Disconnected => {
                        self.sharing = false;
                        self.video.clear();
                        self.participants.clear();
                        self.shared_audio = false;
                    }
                    MediaEvent::ParticipantJoined { identity, name } => {
                        self.participants
                            .insert(identity, (name, ParticipantAudioSettings::default()));
                    }
                    MediaEvent::ParticipantLeft { identity } => {
                        self.participants.remove(&identity);
                    }
                    MediaEvent::AudioLevels { input, output } => self.levels = (input, output),
                    MediaEvent::Error(error) => {
                        self.error = Some(error);
                        self.pending_message = None;
                    }
                    _ => {}
                },
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
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
        if let Some(ticket) = self.mfa_ticket.clone() {
            self.command(
                Command::LoginMfa {
                    ticket,
                    code: self.mfa.read(cx).content.to_string(),
                },
                cx,
            );
        } else {
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
        self.command(
            Command::SetDraft(self.composer.read(cx).content.to_string()),
            cx,
        );
        self.command(Command::SelectChannel { channel_id: id }, cx);
        self.pane = Pane::Chat;
    }

    fn login_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
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
            .child(muted(
                "Native Rust + GPUI client · credentials stay in memory",
            ))
            .child(
                div()
                    .w(px(380.))
                    .flex()
                    .flex_col()
                    .gap_3()
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
                        let guild = channel.guild_id;
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
                        let label = if voice {
                            format!("◉  {}", channel.name)
                        } else {
                            format!("#  {}", channel.name)
                        };
                        button(("channel", id.0), label)
                            .bg(rgb(if self.state.selected_channel == Some(id) {
                                0x3b344f
                            } else {
                                PANEL
                            }))
                            .when(self.state.is_unread(id), |v| {
                                v.text_color(rgb(TEXT)).font_weight(FontWeight::SEMIBOLD)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if voice {
                                    this.command(
                                        Command::JoinVoice {
                                            guild_id: guild,
                                            channel_id: id,
                                        },
                                        cx,
                                    );
                                } else {
                                    this.select(id, cx);
                                }
                            }))
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
                            this.mfa_ticket = None;
                            this.pending_message = None;
                            this.editing = None;
                            this.video.clear();
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

    fn chat(&self, cx: &mut Context<Self>) -> AnyElement {
        let channel = self
            .state
            .selected_channel
            .and_then(|id| self.state.channels.get(&id));
        let title = channel
            .map(|c| format!("# {}", c.name))
            .unwrap_or("Choose a channel".into());
        let selected = self.state.selected_channel;
        let messages = selected
            .map(|id| self.state.channel_messages(id).cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(60.))
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
            .when(!self.video.is_empty(), |v| {
                v.child(
                    div()
                        .flex()
                        .gap_3()
                        .p_3()
                        .children(self.video.values().map(|image| {
                            img(image.clone())
                                .w(px(420.))
                                .h(px(236.))
                                .object_fit(ObjectFit::Contain)
                        })),
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
                    .p_4()
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
                    .child(muted(
                        "Enter to send · Shift+Enter for a new line · paste multiline text",
                    )),
            )
            .into_any_element()
    }

    fn settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let dsp = self.dsp.clone();
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
                button("refresh-devices", "Refresh audio devices").on_click(
                    cx.listener(|this, _, _, cx| this.command(Command::RefreshDevices, cx)),
                ),
            )
            .child(self.audio_controls(cx))
            .child(heading("Input device"))
            .children(self.devices.inputs.iter().map(|device| {
                let id = device.id.clone();
                button(
                    SharedString::from(format!("input-{id}")),
                    device.label.clone(),
                )
                .when(self.selected_input.as_ref() == Some(&id), |v| {
                    v.bg(rgb(0x6650bd))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected_input = Some(id.clone());
                    this.command(Command::SetInputDevice(id.clone()), cx);
                }))
            }))
            .child(heading("Output device"))
            .children(self.devices.outputs.iter().map(|device| {
                let id = device.id.clone();
                button(
                    SharedString::from(format!("output-{id}")),
                    device.label.clone(),
                )
                .when(self.selected_output.as_ref() == Some(&id), |v| {
                    v.bg(rgb(0x6650bd))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected_output = Some(id.clone());
                    this.command(Command::SetOutputDevice(id.clone()), cx);
                }))
            }))
            .child(
                button(
                    "aec",
                    format!("Echo cancellation: {}", dsp.echo_cancellation),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.dsp.echo_cancellation = !this.dsp.echo_cancellation;
                    this.command(Command::SetDsp(this.dsp.clone()), cx);
                })),
            )
            .child(
                button(
                    "ns",
                    format!("Noise suppression: {}", dsp.noise_suppression),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.dsp.noise_suppression = !this.dsp.noise_suppression;
                    this.command(Command::SetDsp(this.dsp.clone()), cx);
                })),
            )
            .child(
                button("agc", format!("Automatic gain: {}", dsp.auto_gain_control)).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.dsp.auto_gain_control = !this.dsp.auto_gain_control;
                        this.command(Command::SetDsp(this.dsp.clone()), cx);
                    }),
                ),
            )
            .child(
                button("back-chat", "Back to chat").on_click(cx.listener(|this, _, _, cx| {
                    this.pane = Pane::Chat;
                    cx.notify();
                })),
            )
            .into_any_element()
    }

    fn audio_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let mode = self.mixer.input_mode;
        div()
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
            .child(
                div().flex().gap_2().children(
                    [
                        (InputMode::OpenMic, "Open microphone"),
                        (InputMode::VoiceActivity, "Voice activity"),
                        (InputMode::PushToTalk, "Push to talk"),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(i, (value, label))| {
                        button(("mode", i), label)
                            .when(mode == value, |v| v.bg(rgb(0x6650bd)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.mixer.input_mode = value;
                                this.command(Command::SetMixer(this.mixer.clone()), cx);
                            }))
                    }),
                ),
            )
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
        div().flex().flex_col().gap_3().child(heading("Share audio"))
            .child(muted(if self.shared_audio { "Audio sharing is active" } else { "No audio is being shared" }))
            .child(muted("Audio sharing uses PipeWire on Linux. Fluxer's own audio is excluded to prevent feedback."))
            .child(button("share-system-audio","Share system audio").on_click(cx.listener(|this,_,_,cx|{this.command(Command::StartSharedAudio(SharedAudioSelection::System),cx);})))
            .children(self.applications.iter().enumerate().map(|(i,app)|{let pid=app.process_id.clone();button(("share-app",i),format!("Share {} audio",app.label)).on_click(cx.listener(move |this,_,_,cx|{this.command(Command::StartSharedAudio(SharedAudioSelection::Application{process_id:pid.clone()}),cx);}))}))
            .child(button("refresh-apps","Refresh audio applications").on_click(cx.listener(|this,_,_,cx|this.command(Command::RefreshAudioApplications,cx))))
            .child(div().flex().gap_3().items_center().child(muted(format!("Shared audio {:.0}%",self.shared_audio_settings.gain*100.)))
                .child(button("share-gain-down","−").on_click(cx.listener(|this,_,_,cx|{this.shared_audio_settings.gain=(this.shared_audio_settings.gain-0.1).max(0.);this.command(Command::SetSharedAudio(this.shared_audio_settings),cx);})))
                .child(button("share-gain-up","+").on_click(cx.listener(|this,_,_,cx|{this.shared_audio_settings.gain=(this.shared_audio_settings.gain+0.1).min(2.);this.command(Command::SetSharedAudio(this.shared_audio_settings),cx);})))
                .child(button("share-audio-mute",if self.shared_audio_settings.muted{"Unmute shared audio"}else{"Mute shared audio"}).on_click(cx.listener(|this,_,_,cx|{this.shared_audio_settings.muted = !this.shared_audio_settings.muted;this.command(Command::SetSharedAudio(this.shared_audio_settings),cx);}))))
            .child(button("stop-shared-audio","Stop sharing audio").on_click(cx.listener(|this,_,_,cx|{this.command(Command::StopSharedAudio,cx);})))
            .into_any_element()
    }

    fn screens(&self, cx: &mut Context<Self>) -> AnyElement {
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
            .child(muted(
                "Select a display or window. Your desktop may open a system permission picker.",
            ))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(muted("Frame rate"))
                    .children([15_u32, 30, 60].into_iter().map(|fps| {
                        button(("share-fps", fps), format!("{fps} fps"))
                            .when(self.share_fps == fps, |v| v.bg(rgb(0x6650bd)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.share_fps = fps;
                                cx.notify();
                            }))
                    })),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(muted("Maximum resolution"))
                    .children(
                        [(1280_u32, 720_u32), (1920, 1080), (2560, 1440)]
                            .into_iter()
                            .map(|resolution| {
                                button(("share-size", resolution.1), format!("{}p", resolution.1))
                                    .when(self.share_size == resolution, |v| v.bg(rgb(0x6650bd)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.share_size = resolution;
                                        cx.notify();
                                    }))
                            }),
                    ),
            )
            .children(self.sources.iter().enumerate().map(|(index, source)| {
                let source = source.clone();
                button(("source", index), source.label.clone()).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.command(
                            Command::StartShare(ScreenShareOptions {
                                source: source.clone(),
                                fps: this.share_fps,
                                max_width: this.share_size.0,
                                max_height: this.share_size.1,
                                max_bitrate: if this.share_size.1 > 1080 {
                                    8_000_000
                                } else if this.share_fps > 30 {
                                    6_000_000
                                } else {
                                    4_000_000
                                },
                            }),
                            cx,
                        );
                        this.pane = Pane::Chat;
                    },
                ))
            }))
            .child(button("refresh-sources", "Refresh sources").on_click(
                cx.listener(|this, _, _, cx| this.command(Command::RefreshScreenSources, cx)),
            ))
            .child(self.shared_audio_controls(cx))
            .child(button("cancel-share", "Back to chat").on_click(cx.listener(
                |this, _, _, cx| {
                    this.pane = Pane::Chat;
                    cx.notify();
                },
            )))
            .into_any_element()
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
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = if self.state.current_user.is_none() {
            self.login_view(cx)
        } else {
            let body = match self.pane {
                Pane::Chat => self.chat(cx),
                Pane::Settings => self.settings(cx),
                Pane::Screens => self.screens(cx),
                Pane::Search => self.search(cx),
            };
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(self.rail(cx))
                .child(self.channels(cx))
                .child(body)
                .into_any_element()
        };
        div().size_full().flex().flex_col().bg(rgb(BG)).text_color(rgb(TEXT)).font_family("Inter").text_size(px(14.))
            .on_action(cx.listener(Self::submit))
            .on_key_down(cx.listener(|this,event:&KeyDownEvent,_,cx|{
                if event.keystroke.key=="f8" && !event.is_held && this.mixer.input_mode==InputMode::PushToTalk {this.command(Command::SetPushToTalkPressed(true),cx);}
            }))
            .on_key_up(cx.listener(|this,event:&KeyUpEvent,_,cx|{
                if event.keystroke.key=="f8" {this.command(Command::SetPushToTalkPressed(false),cx);}
            }))
            .when(self.demo,|v|v.child(div().px_4().py_1().bg(rgb(0x4c3b22)).text_size(px(12.)).child("OFFLINE DEMO · Local sample messages · Calls require a connected account")))
            .when(self.error.is_some(),|v|v.child(div().p_3().bg(rgb(0x613342)).flex().items_center().justify_between().child(self.error.clone().unwrap_or_default()).child(button("dismiss-error","Dismiss").on_click(cx.listener(|this,_,_,cx|{this.error=None;cx.notify();})))))
            .child(content)
    }
}

fn main() {
    let demo = std::env::args().any(|v| v == "--demo");
    Application::new().run(move |cx: &mut App| {
        input::init(cx);
        cx.bind_keys([
            KeyBinding::new("enter", Submit, Some("TextInput")),
            KeyBinding::new("ctrl-q", Quit, None),
            KeyBinding::new("cmd-q", Quit, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Fluxer · Native".into()),
                    ..Default::default()
                }),
                window_min_size: Some(size(px(880.), px(600.))),
                app_id: Some("app.fluxer.Native".into()),
                ..Default::default()
            },
            |window, cx| {
                window.set_window_title("Fluxer · Native");
                cx.new(|cx| Fluxer::new(demo, window, cx))
            },
        )
        .expect("open Fluxer window");
        cx.activate(true);
    });
}
