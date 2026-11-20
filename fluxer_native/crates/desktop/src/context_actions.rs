// SPDX-License-Identifier: AGPL-3.0-or-later
//! Source-backed actions; playback changes never become server moderation.
use super::*;
use crate::context_menu::{
    ContextMenu, ContextMenuCallback, ContextMenuEvent, ContextMenuItem as Item,
};
use crate::controls::Icon;
use crate::navigation_actions::NavigationPage;
use crate::stream_layout::StreamLayout;
use std::{cell::Cell, rc::Rc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Target {
    InboxFilters,
    Member { guild: Snowflake, user: Snowflake },
    Stream(String),
    Media(crate::media_actions::MediaReference),
    User { channel: Snowflake, user: Snowflake },
    Connection(String),
    SelfVoice(Snowflake),
    Channel(Snowflake),
    Community(Snowflake),
}
#[derive(Default, Clone, Copy)]
pub(super) struct UserAudioOverride {
    pub(super) muted: Option<bool>,
    pub(super) gain: Option<f32>,
}
impl UserAudioOverride {
    pub fn apply(self, mut settings: ParticipantAudioSettings) -> ParticipantAudioSettings {
        if let Some(muted) = self.muted {
            settings.muted |= muted;
        }
        if let Some(gain) = self.gain {
            settings.gain = (settings.gain * gain).clamp(0., 2.);
        }
        settings
    }
}
pub(super) struct ContextUi {
    pub menu: Entity<ContextMenu>,
    pub kick_confirm: Option<(Snowflake, Snowflake, String, Option<Option<u64>>)>,
    pub channel_delete_confirm: Option<(Snowflake, String, bool)>,
    pub kick_focus: FocusHandle,
    pub kick_previous: Option<FocusHandle>,
    pub kick_selected: bool,
    pub kick_armed: bool,
    pub page: NavigationPage,
    pub anchor: Point<Pixels>,
    target: Option<Target>,
    generation: u64,
    pub user_audio: BTreeMap<Snowflake, UserAudioOverride>,
    pub connection_audio: BTreeMap<String, ParticipantAudioSettings>,
    window: Option<AnyWindowHandle>,
}
impl ContextUi {
    pub fn new(cx: &mut Context<Fluxer>) -> Self {
        Self {
            menu: cx.new(ContextMenu::new),
            kick_confirm: None,
            channel_delete_confirm: None,
            kick_focus: cx.focus_handle(),
            kick_previous: None,
            kick_selected: false,
            kick_armed: false,
            page: NavigationPage::Main,
            anchor: Point::default(),
            target: None,
            generation: 0,
            user_audio: BTreeMap::new(),
            connection_audio: BTreeMap::new(),
            window: None,
        }
    }
}

pub(super) fn trigger(
    view: Stateful<Div>,
    target: Target,
    cx: &mut Context<Fluxer>,
) -> Stateful<Div> {
    let bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
    let paint_bounds = bounds.clone();
    let pointer_target = target.clone();
    view.relative()
        .focusable()
        .tab_stop(true)
        .child(
            canvas(move |b, _, _| paint_bounds.set(b), |_, _, _, _| {})
                .absolute()
                .size_full(),
        )
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                cx.stop_propagation();
                this.open_context_menu(pointer_target.clone(), event.position, window, cx);
            }),
        )
        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
            if (event.keystroke.key == "f10" && event.keystroke.modifiers.shift)
                || matches!(event.keystroke.key.as_str(), "menu" | "contextmenu")
            {
                cx.stop_propagation();
                if !event.is_held {
                    let b = bounds.get();
                    this.open_context_menu(
                        target.clone(),
                        point(b.left() + px(8.), b.top() + px(28.)),
                        window,
                        cx,
                    );
                }
            }
        }))
}
pub(super) fn action(id: &str, label: &str, icon: Icon) -> Item {
    Item::Action {
        id: id.into(),
        label: label.into(),
        icon: Some(icon),
        enabled: true,
        danger: false,
    }
}
fn toggle(id: &str, label: &str, checked: bool) -> Item {
    Item::Toggle {
        id: id.into(),
        label: label.into(),
        checked,
        enabled: true,
    }
}
fn volume(id: &str, label: &str, gain: f32) -> Item {
    Item::Slider {
        id: id.into(),
        label: label.into(),
        value: gain * 100.,
        min: 0.,
        max: 200.,
        step: 1.,
    }
}
fn event_id(event: &ContextMenuEvent) -> Option<&str> {
    match event {
        ContextMenuEvent::Action(id)
        | ContextMenuEvent::Toggle(id, _)
        | ContextMenuEvent::Slider(id, _) => Some(id),
        _ => None,
    }
}
fn enabled(items: &[Item], event: &ContextMenuEvent) -> bool {
    items.iter().any(|item| match (item, event) {
        (Item::Action { id, enabled, .. }, ContextMenuEvent::Action(wanted))
        | (Item::Toggle { id, enabled, .. }, ContextMenuEvent::Toggle(wanted, _)) => {
            id == wanted && *enabled
        }
        (Item::Slider { id, .. }, ContextMenuEvent::Slider(wanted, value)) => {
            id == wanted && value.is_finite()
        }
        _ => false,
    })
}
#[derive(Clone, Copy)]
enum AudioChange {
    Mute(bool),
    Gain(f32),
}
impl AudioChange {
    fn apply(self, mut settings: ParticipantAudioSettings) -> ParticipantAudioSettings {
        match self {
            Self::Mute(muted) => settings.muted = muted,
            Self::Gain(gain) if gain.is_finite() => settings.gain = gain.clamp(0., 2.),
            _ => (),
        }
        settings
    }
    fn remember(self, preference: &mut UserAudioOverride) {
        match self {
            Self::Mute(muted) => preference.muted = Some(muted),
            Self::Gain(gain) if gain.is_finite() => preference.gain = Some(gain.clamp(0., 2.)),
            _ => (),
        }
    }
}
fn audio_identities<'a>(
    participants: impl Iterator<Item = &'a String>,
    user: Snowflake,
) -> Vec<String> {
    participants
        .filter(|identity| crate::stream_view::sdk_identity_user(identity) == Some(user))
        .cloned()
        .collect()
}

fn participant_target(
    channel: Option<Snowflake>,
    user: Option<Snowflake>,
    identity: Option<String>,
    current_user: Option<Snowflake>,
    remote: bool,
) -> Option<Target> {
    // Gateway account identity alone never proves which device is local.
    // Only the explicit account dock offers this device's microphone controls.
    if user.is_some() && user == current_user && remote {
        return identity.map(Target::Connection);
    }
    if let (Some(channel), Some(user)) = (channel, user) {
        Some(Target::User { channel, user })
    } else {
        identity.map(Target::Connection)
    }
}

impl Fluxer {
    fn can_manage_guild_channels(&self, id: Snowflake) -> bool {
        let Some(user) = self.state.current_user.as_ref() else {
            return false;
        };
        let Some(guild) = self.state.guilds.get(&id) else {
            return false;
        };
        guild.owner_id == user.id
            || self
                .state
                .members
                .get(&(id, user.id))
                .is_some_and(|member| {
                    fluxer_core::resolve_permissions(guild, member, &[])
                        .contains(fluxer_core::Permissions::MANAGE_CHANNELS)
                })
    }
    fn can_edit_community_overview(&self, id: Snowflake) -> bool {
        self.state.current_user.as_ref().is_some_and(|user| {
            self.state
                .guild_permissions_for(id, user.id)
                .contains(fluxer_core::Permissions::MANAGE_GUILD)
        })
    }
    pub(super) fn install_context_voice_fixture(&mut self) {
        if !self.demo_stream || std::env::var_os("FLUXER_CONTEXT_DEMO_TARGET").is_none() {
            return;
        }
        self.state.voice.join(Snowflake(14));
        self.state.voice_states.clear();
        for (user, connection, name) in [
            (101, "desktop", "Alex"),
            (101, "phone", "Alex"),
            (100, "other-device", "You on another device"),
        ] {
            let user = Snowflake(user);
            let identity = format!("user_{user}_{connection}");
            self.state.voice_states.insert(
                (user, connection.into()),
                fluxer_core::VoiceState {
                    user_id: user,
                    guild_id: Some(Snowflake(1)),
                    channel_id: Some(Snowflake(14)),
                    connection_id: Some(connection.into()),
                    ..Default::default()
                },
            );
            let base = ParticipantAudioSettings {
                gain: if connection == "phone" { 0.3 } else { 1. },
                muted: false,
            };
            self.context.connection_audio.insert(identity.clone(), base);
            self.participants.insert(identity, (name.into(), base));
        }
    }
    pub(super) fn schedule_context_demo(&self, window: &Window, cx: &mut Context<Self>) {
        if !self.demo_stream {
            return;
        }
        let Ok(kind) = std::env::var("FLUXER_CONTEXT_DEMO_TARGET") else {
            return;
        };
        let target = match kind.as_str() {
            "stream" => Target::Stream("preview-0".into()),
            "user" => Target::User {
                channel: Snowflake(14),
                user: Snowflake(101),
            },
            "connection" => Target::Connection("user_100_other-device".into()),
            "self" => Target::SelfVoice(Snowflake(14)),
            "channel" => Target::Channel(Snowflake(11)),
            "community" => Target::Community(Snowflake(1)),
            _ => return,
        };
        let handle = window.window_handle();
        cx.spawn(async move |entity, cx| {
            Timer::after(Duration::from_millis(600)).await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = entity.update(cx, |view, cx| {
                    view.install_context_voice_fixture();
                    view.focus.focus(window);
                    let size = window.viewport_size();
                    view.open_context_menu(
                        target,
                        point(size.width - px(24.), size.height - px(24.)),
                        window,
                        cx,
                    );
                });
            });
        })
        .detach();
    }
    pub(super) fn context_menu_open(&self, cx: &App) -> bool {
        self.context.menu.read(cx).is_open()
            || self.profile_ui.is_open(cx)
            || self.role_mention.read(cx).is_open()
            || self.message_links.input_context_menu_open(cx)
            || [
                &self.composer,
                &self.instance,
                &self.identifier,
                &self.password,
                &self.mfa,
                &self.search,
                &self.emoji_search,
                &self.forward_search,
            ]
            .iter()
            .any(|input| input.read(cx).context_menu_open(cx))
    }
    pub(super) fn reset_text_context_menus(&mut self, cx: &mut Context<Self>) {
        for input in [
            &self.composer,
            &self.instance,
            &self.identifier,
            &self.password,
            &self.mfa,
            &self.search,
            &self.emoji_search,
            &self.forward_search,
        ] {
            input.update(cx, |input, cx| input.reset_context_menu(cx));
        }
        self.message_links.reset_input_context_menu(cx);
    }
    pub(super) fn reset_context_menu(&mut self, cx: &mut Context<Self>) {
        self.reset_text_context_menus(cx);
        self.context.target = None;
        self.context.generation = self.context.generation.wrapping_add(1);
        let menu = self.context.menu.clone();
        if self.context.menu.read(cx).is_open()
            && let Some(window) = self.context.window
        {
            let owner = cx.entity().downgrade();
            let generation = self.context.generation;
            cx.defer(move |cx| {
                let _ = owner.update(cx, |this, cx| {
                    if this.context.generation == generation {
                        let _ = window.update(cx, |_, window, cx| {
                            menu.update(cx, |menu, cx| menu.dismiss(window, cx))
                        });
                    }
                });
            });
        } else {
            menu.update(cx, |menu, cx| menu.reset(cx));
        }
    }
    pub(super) fn voice_context_target(
        &self,
        channel: Option<Snowflake>,
        user: Option<Snowflake>,
        identity: Option<String>,
    ) -> Option<Target> {
        participant_target(
            channel,
            user,
            identity.clone(),
            self.state.current_user.as_ref().map(|me| me.id),
            identity
                .as_ref()
                .is_some_and(|id| self.participants.contains_key(id)),
        )
    }

    fn context_target_valid(&self, target: &Target) -> bool {
        match target {
            Target::InboxFilters => self.inbox_filters_available(),
            Target::Member { guild, user } => {
                self.state.members.contains_key(&(*guild, *user))
                    && self.state.guilds.contains_key(guild)
            }
            Target::Media(reference) => self.media_descriptor(reference).is_some(),
            Target::Stream(id) => {
                self.available_streams.contains(id) || self.viewer.desired.contains(id)
            }
            Target::Connection(id) => self.participants.contains_key(id),
            Target::User { channel, user } => self
                .state
                .voice_participants(*channel)
                .iter()
                .any(|p| p.user_id == *user),
            Target::SelfVoice(channel) => self.state.voice.channel_id == Some(*channel),
            Target::Channel(channel) => self.can_view_context_channel(*channel),
            Target::Community(guild) => self.state.guilds.contains_key(guild),
        }
    }
    fn can_view_context_channel(&self, channel: Snowflake) -> bool {
        self.state.current_user.as_ref().is_some_and(|user| {
            self.state.channels.contains_key(&channel)
                && self
                    .state
                    .permissions_for(channel, user.id)
                    .contains(fluxer_core::Permissions::VIEW_CHANNEL)
        })
    }
    fn can_mention_context_user(&self) -> bool {
        self.state
            .selected_channel
            .zip(self.state.current_user.as_ref())
            .is_some_and(|(id, user)| {
                self.state.channels.get(&id).is_some_and(|channel| {
                    can_send_message(channel.kind, self.state.permissions_for(id, user.id))
                })
            })
    }
    fn context_items(&self, target: &Target, window: &Window) -> Vec<Item> {
        if self.context.page != NavigationPage::Main {
            return self.navigation_page_items(target);
        }
        let mut items = Vec::new();
        match target {
            Target::InboxFilters => return self.inbox_filter_items(),
            Target::Member { guild, user } => {
                let name = self
                    .state
                    .members
                    .get(&(*guild, *user))
                    .map(|m| {
                        m.nick
                            .clone()
                            .unwrap_or_else(|| m.user.display_name().to_owned())
                    })
                    .unwrap_or_else(|| user.to_string());
                items.push(Item::Header(name));
                if self.can_timeout_member(*guild, *user) {
                    for (id, label) in [
                        ("timeout-60", "Time out for 60 seconds"),
                        ("timeout-300", "Time out for 5 minutes"),
                        ("timeout-600", "Time out for 10 minutes"),
                        ("timeout-3600", "Time out for 1 hour"),
                        ("timeout-86400", "Time out for 1 day"),
                        ("timeout-604800", "Time out for 1 week"),
                    ] {
                        items.push(Item::Action {
                            id: id.into(),
                            label: label.into(),
                            icon: None,
                            enabled: true,
                            danger: true,
                        });
                    }
                    if self
                        .state
                        .members
                        .get(&(*guild, *user))
                        .and_then(|m| m.communication_disabled_until.as_deref())
                        .and_then(|until| chrono::DateTime::parse_from_rfc3339(until).ok())
                        .is_some_and(|until| until > chrono::Utc::now())
                    {
                        items.push(Item::Action {
                            id: "timeout-clear".into(),
                            label: "Remove timeout".into(),
                            icon: None,
                            enabled: true,
                            danger: false,
                        });
                    }
                }
                if self.can_kick_member(*guild, *user) {
                    items.push(Item::Action {
                        id: "kick-member".into(),
                        label: "Kick member".into(),
                        icon: None,
                        enabled: true,
                        danger: true,
                    });
                }
            }
            Target::Media(reference) => return self.media_context_items(reference),
            Target::Stream(identity) => {
                items.push(Item::Header(self.stream_name(identity)));
                if self.viewer.desired.contains(identity) {
                    let audio = self.stream_audio.get(identity).copied().unwrap_or_default();
                    items.extend([
                        toggle("stream-mute", "Mute stream", audio.muted),
                        volume("stream-gain", "Stream volume", audio.gain),
                        action("stream-reset", "Reset volume to 100%", Icon::Refresh),
                        Item::Separator,
                        action("stream-stage", "Put on main stage", Icon::Expand),
                        action("stream-tiles", "Tile all streams", Icon::ScreenShare),
                        action(
                            "fullscreen",
                            if window.is_fullscreen() {
                                "Exit fullscreen"
                            } else {
                                "Fullscreen"
                            },
                            Icon::Fullscreen,
                        ),
                        action(
                            "stream-chat",
                            if self.viewer.chat_open {
                                "Hide chat"
                            } else {
                                "Show chat"
                            },
                            Icon::Chat,
                        ),
                        Item::Separator,
                        action("stream-stop", "Stop watching", Icon::Stop),
                    ]);
                } else {
                    items.push(action("stream-watch", "Watch stream", Icon::ScreenShare));
                }
            }
            Target::User { channel, user } => {
                let name = self
                    .state
                    .voice_participants(*channel)
                    .into_iter()
                    .find(|p| p.user_id == *user)
                    .map(|p| p.display_name)
                    .unwrap_or_else(|| user.to_string());
                items.push(Item::Header(name));
                items.push(action("view-profile", "View profile", Icon::Eye));
                let identities = audio_identities(self.participants.keys(), *user);
                if self.state.voice.channel_id == Some(*channel) && !identities.is_empty() {
                    let settings = self
                        .context
                        .user_audio
                        .get(user)
                        .copied()
                        .unwrap_or_default();
                    items.extend([
                        toggle("user-mute", "Mute for me", settings.muted.unwrap_or(false)),
                        volume("user-gain", "User volume", settings.gain.unwrap_or(1.)),
                        action("user-reset", "Reset volume to 100%", Icon::Refresh),
                        Item::Separator,
                    ]);
                }
                for identity in self.available_streams.iter().filter(|id| {
                    self.state.voice.channel_id == Some(*channel)
                        && crate::stream_view::sdk_identity_user(id) == Some(*user)
                }) {
                    let watching = self.viewer.desired.contains(identity);
                    items.push(action(
                        &format!("user-watch:{identity}"),
                        if watching {
                            "Stop watching stream"
                        } else {
                            "Watch stream"
                        },
                        Icon::ScreenShare,
                    ));
                }
                if self.can_mention_context_user() {
                    items.push(action("mention", "Mention", Icon::Reply));
                }
                items.push(action("copy-user", "Copy user ID", Icon::Copy));
            }
            Target::Connection(identity) => {
                if let Some((name, _)) = self.participants.get(identity) {
                    let audio = self
                        .context
                        .connection_audio
                        .get(identity)
                        .copied()
                        .unwrap_or_default();
                    items.extend([
                        Item::Header(name.clone()),
                        toggle("connection-mute", "Mute for me", audio.muted),
                        volume("connection-gain", "Connection volume", audio.gain),
                        action("connection-reset", "Reset volume to 100%", Icon::Refresh),
                        Item::Separator,
                        action("copy-connection", "Copy connection identity", Icon::Copy),
                    ]);
                }
            }
            Target::SelfVoice(_) => {
                items.extend([
                    Item::Header("Your voice · this device".into()),
                    toggle("self-mute", "Mute microphone", self.state.voice.muted),
                    toggle("self-deaf", "Deafen", self.state.voice.deafened),
                ]);
                if self.sharing {
                    items.push(action("stop-share", "Stop sharing", Icon::Stop));
                }
                items.extend([
                    action("voice-settings", "Voice settings", Icon::Settings),
                    Item::Separator,
                    action("disconnect", "Disconnect", Icon::PhoneOff),
                ]);
            }
            Target::Channel(id) => {
                if let Some(channel) = self.state.channels.get(id) {
                    items.push(Item::Header(channel.display_name()));
                    if channel.kind != ChannelType::Category {
                        items.push(action("channel-open", "Open chat", Icon::Chat));
                    }
                    if channel.kind.is_voice() {
                        if self.state.voice.channel_id == Some(*id) {
                            items.push(action("disconnect", "Disconnect", Icon::PhoneOff));
                        } else if self.state.current_user.as_ref().is_some_and(|user| {
                            self.state
                                .permissions_for(*id, user.id)
                                .contains(fluxer_core::Permissions::CONNECT)
                        }) {
                            items.push(action("channel-join", "Join voice", Icon::Headphones));
                        }
                    }
                    if channel.guild_id.is_some()
                        && matches!(
                            channel.kind,
                            ChannelType::Text | ChannelType::Voice | ChannelType::Category
                        )
                        && self.state.current_user.as_ref().is_some_and(|u| {
                            self.state
                                .permissions_for(*id, u.id)
                                .contains(fluxer_core::Permissions::MANAGE_CHANNELS)
                        })
                    {
                        items.push(action("channel-edit", "Rename channel", Icon::Settings));
                        items.push(action("channel-delete", "Delete channel", Icon::Settings));
                        if channel.kind == ChannelType::Category {
                            items.push(action(
                                "category-create-text",
                                "Create text channel",
                                Icon::Chat,
                            ));
                            items.push(action(
                                "category-create-voice",
                                "Create voice channel",
                                Icon::Headphones,
                            ));
                        }
                    }
                    items.extend(self.navigation_items(target));
                    items.push(Item::Separator);
                    if self
                        .message_links
                        .channel_link(channel.guild_id, *id)
                        .is_some()
                    {
                        items.push(action("copy-channel-link", "Copy channel link", Icon::Copy));
                    }
                    items.push(action("copy-channel", "Copy channel ID", Icon::Copy));
                }
            }
            Target::Community(id) => {
                let name = self
                    .state
                    .guilds
                    .get(id)
                    .map(|g| g.name.clone())
                    .unwrap_or_default();
                items.extend([
                    Item::Header(name),
                    action("community-open", "Open community", Icon::Home),
                ]);
                if self.can_edit_community_overview(*id) {
                    items.push(action(
                        "community-overview",
                        "Community Overview",
                        Icon::Settings,
                    ));
                }
                if self.can_manage_guild_channels(*id) {
                    items.push(action(
                        "community-create-text",
                        "Create text channel",
                        Icon::Chat,
                    ));
                    items.push(action(
                        "community-create-voice",
                        "Create voice channel",
                        Icon::Headphones,
                    ));
                    items.push(action(
                        "community-create-category",
                        "Create category",
                        Icon::Settings,
                    ));
                }
                items.extend(self.navigation_items(target));
                if self.state.guilds.get(id).is_some_and(|g| {
                    self.state
                        .current_user
                        .as_ref()
                        .is_some_and(|u| u.id != g.owner_id)
                }) {
                    items.push(action("community-leave", "Leave community", Icon::Back));
                }
                items.extend([
                    Item::Separator,
                    action("copy-community", "Copy community ID", Icon::Copy),
                ]);
            }
        }
        items
    }
    pub(super) fn open_context_menu(
        &mut self,
        target: Target,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_context_menu_page(target, anchor, NavigationPage::Main, window, cx);
    }
    pub(super) fn open_context_menu_page(
        &mut self,
        target: Target,
        anchor: Point<Pixels>,
        page: NavigationPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ((self.image_viewer.is_open() || self.playback.modal.is_open())
            && !matches!(&target, Target::Media(_)))
            || self.message_links.is_open()
            || self.search_ui.confirm.is_some()
            || self.context.kick_confirm.is_some()
            || self.context.channel_delete_confirm.is_some()
            || !self.context_target_valid(&target)
        {
            return;
        }
        self.reset_text_context_menus(cx);
        self.dismiss_message_actions(window, cx);
        self.emoji_target = None;
        self.command_if_live(Command::SetPushToTalkPressed(false), cx);
        self.context.page = page;
        self.context.anchor = anchor;
        let items = self.context_items(&target, window);
        self.context.generation = self.context.generation.wrapping_add(1);
        let generation = self.context.generation;
        self.context.target = Some(target.clone());
        self.context.window = Some(window.window_handle());
        let owner = cx.entity().downgrade();
        let callback: ContextMenuCallback = Rc::new(move |event, window, cx| {
            let _ = owner.update(cx, |this, cx| {
                if this.context.generation != generation
                    || this.context.target.as_ref() != Some(&target)
                {
                    return;
                }
                if event == ContextMenuEvent::Dismiss {
                    this.context.target = None;
                    return;
                }
                if !this.context_target_valid(&target)
                    || !enabled(&this.context_items(&target, window), &event)
                {
                    this.reset_context_menu(cx);
                    return;
                }
                let close = matches!(event, ContextMenuEvent::Action(_));
                this.perform_context_action(&target, event, window, cx);
                if this.context.generation != generation {
                    return;
                }
                if close {
                    this.context.target = None;
                } else {
                    let items = this.context_items(&target, window);
                    this.context
                        .menu
                        .update(cx, |menu, cx| menu.set_items(items, window, cx));
                }
                cx.notify();
            });
        });
        self.context.menu.update(cx, |menu, cx| {
            menu.open(items, anchor, callback, window, cx)
        });
        cx.notify();
    }
    pub(super) fn command_if_live(&mut self, command: Command, cx: &mut Context<Self>) {
        if !self.demo {
            self.command(command, cx);
        }
    }
    fn change_user_audio(&mut self, user: Snowflake, change: AudioChange, cx: &mut Context<Self>) {
        change.remember(self.context.user_audio.entry(user).or_default());
        let saved = self.context.user_audio[&user];
        self.command_if_live(
            Command::SetUserAudioPreference {
                user,
                preference: fluxer_client::UserAudioPreference {
                    muted: saved.muted,
                    gain: saved.gain,
                },
            },
            cx,
        );
        for identity in audio_identities(self.participants.keys(), user) {
            self.recompute_connection_audio(&identity, cx);
        }
    }
    fn change_connection_audio(
        &mut self,
        identity: &str,
        change: AudioChange,
        cx: &mut Context<Self>,
    ) {
        let base = self
            .context
            .connection_audio
            .get(identity)
            .copied()
            .unwrap_or_default();
        self.set_connection_audio(identity, change.apply(base), cx);
    }
    pub(super) fn set_connection_audio(
        &mut self,
        identity: &str,
        settings: ParticipantAudioSettings,
        cx: &mut Context<Self>,
    ) {
        if !self.participants.contains_key(identity) {
            return;
        }
        self.context
            .connection_audio
            .insert(identity.into(), settings);
        self.recompute_connection_audio(identity, cx);
    }
    fn recompute_connection_audio(&mut self, identity: &str, cx: &mut Context<Self>) {
        let base = self
            .context
            .connection_audio
            .get(identity)
            .copied()
            .unwrap_or_default();
        let settings = crate::stream_view::sdk_identity_user(identity)
            .and_then(|user| self.context.user_audio.get(&user))
            .copied()
            .unwrap_or_default()
            .apply(base);
        if let Some((_, current)) = self.participants.get_mut(identity) {
            *current = settings;
            if self.demo_stream && std::env::var_os("FLUXER_CONTEXT_DEMO_TARGET").is_some() {
                eprintln!(
                    "CONTEXT_DEMO_AUDIO identity={identity} gain={} muted={}",
                    settings.gain, settings.muted
                );
            }
            self.command_if_live(
                Command::SetParticipantAudio {
                    identity: identity.into(),
                    settings,
                },
                cx,
            );
        }
    }

    fn perform_context_action(
        &mut self,
        target: &Target,
        event: ContextMenuEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.demo_stream && std::env::var_os("FLUXER_CONTEXT_DEMO_TARGET").is_some() {
            eprintln!("CONTEXT_DEMO_EVENT target={target:?} event={event:?}");
        }
        if self.perform_navigation_action(target, &event, window, cx) {
            return;
        }
        let id = event_id(&event).unwrap_or_default();
        let muted = if let ContextMenuEvent::Toggle(_, value) = event {
            Some(value)
        } else {
            None
        };
        let gain = if let ContextMenuEvent::Slider(_, value) = event {
            Some(value / 100.)
        } else {
            None
        };
        match (target, id) {
            (Target::InboxFilters, _) => {
                if let Some(value) = muted {
                    self.update_inbox_filter(id, value, cx);
                }
            }
            (Target::Media(reference), _) => {
                self.perform_media_context_action(reference, event.clone(), window, cx)
            }
            (Target::Stream(identity), "stream-mute") => {
                if let Some(muted) = muted {
                    self.change_stream_audio(identity.clone(), |s| s.muted = muted, cx);
                }
            }
            (Target::Stream(identity), "stream-gain") => {
                if let Some(gain) = gain {
                    self.change_stream_audio(identity.clone(), |s| s.gain = gain.clamp(0., 2.), cx);
                }
            }
            (Target::Stream(identity), "stream-reset") => {
                self.change_stream_audio(identity.clone(), |s| s.gain = 1., cx)
            }
            (Target::Stream(identity), "stream-watch") => {
                self.watch_stream(identity.clone(), true, cx)
            }
            (Target::Stream(identity), "stream-stop") => {
                self.watch_stream(identity.clone(), false, cx)
            }
            (Target::Stream(identity), "stream-stage") => {
                self.viewer.focused = Some(identity.clone());
                self.viewer.layout = StreamLayout::MainStage;
                self.pane = Pane::Call;
            }
            (Target::Stream(_), "stream-tiles") => self.viewer.layout = StreamLayout::Tiled,
            (Target::Stream(_), "fullscreen") => window.toggle_fullscreen(),
            (Target::Stream(_), "stream-chat") => self.viewer.chat_open = !self.viewer.chat_open,
            (Target::Member { guild, user }, action) if action.starts_with("timeout-") => {
                let duration = match action.strip_prefix("timeout-").unwrap_or("") {
                    "clear" => Some(None),
                    value => value
                        .parse::<u64>()
                        .ok()
                        .filter(|d| [60, 300, 600, 3600, 86400, 604800].contains(d))
                        .map(Some),
                };
                if let Some(duration) = duration
                    && self.can_timeout_member(*guild, *user)
                {
                    let name = self
                        .state
                        .members
                        .get(&(*guild, *user))
                        .map(|m| {
                            m.nick
                                .clone()
                                .unwrap_or_else(|| m.user.display_name().to_owned())
                        })
                        .unwrap_or_else(|| user.to_string());
                    self.context.kick_confirm = Some((*guild, *user, name, Some(duration)));
                    self.context.kick_previous = window.focused(cx);
                    self.context.kick_selected = false;
                    self.context.kick_armed = false;
                    window.focus(&self.context.kick_focus);
                }
            }
            (Target::Member { guild, user }, "kick-member") => {
                if self.can_kick_member(*guild, *user) {
                    let name = self
                        .state
                        .members
                        .get(&(*guild, *user))
                        .map(|m| {
                            m.nick
                                .clone()
                                .unwrap_or_else(|| m.user.display_name().to_owned())
                        })
                        .unwrap_or_else(|| user.to_string());
                    self.context.kick_confirm = Some((*guild, *user, name, None));
                    self.context.kick_previous = window.focused(cx);
                    self.context.kick_selected = false;
                    self.context.kick_armed = false;
                    window.focus(&self.context.kick_focus);
                }
            }
            (Target::User { channel, user }, "view-profile") => {
                let guild = self
                    .state
                    .channels
                    .get(channel)
                    .and_then(|channel| channel.guild_id);
                self.open_user_profile(
                    self.profile_user(*user, &user.to_string()),
                    guild,
                    false,
                    window.mouse_position(),
                    window,
                    cx,
                );
            }
            (Target::User { user, .. }, "user-mute") => {
                if let Some(muted) = muted {
                    self.change_user_audio(*user, AudioChange::Mute(muted), cx);
                }
            }
            (Target::User { user, .. }, "user-gain") => {
                if let Some(gain) = gain {
                    self.change_user_audio(*user, AudioChange::Gain(gain), cx);
                }
            }
            (Target::User { user, .. }, "user-reset") => {
                self.change_user_audio(*user, AudioChange::Gain(1.), cx)
            }
            (Target::User { user, .. }, "copy-user") => {
                cx.write_to_clipboard(ClipboardItem::new_string(user.to_string()))
            }
            (Target::User { user, .. }, "mention") => {
                self.pane = Pane::Chat;
                self.composer.update(cx, |input, cx| {
                    input.insert_text(&format!("<@{user}> "), window, cx)
                });
                self.composer.focus_handle(cx).focus(window);
            }
            (Target::User { .. }, id) if id.starts_with("user-watch:") => {
                let identity = id.trim_start_matches("user-watch:").to_owned();
                let watch = !self.viewer.desired.contains(&identity);
                self.watch_stream(identity, watch, cx);
            }
            (Target::Connection(identity), "connection-mute") => {
                if let Some(muted) = muted {
                    self.change_connection_audio(identity, AudioChange::Mute(muted), cx);
                }
            }
            (Target::Connection(identity), "connection-gain") => {
                if let Some(gain) = gain {
                    self.change_connection_audio(identity, AudioChange::Gain(gain), cx);
                }
            }
            (Target::Connection(identity), "connection-reset") => {
                self.change_connection_audio(identity, AudioChange::Gain(1.), cx)
            }
            (Target::Connection(identity), "copy-connection") => {
                cx.write_to_clipboard(ClipboardItem::new_string(identity.clone()))
            }
            (Target::SelfVoice(_), "self-mute") => {
                if let Some(muted) = muted {
                    self.state.voice.set_muted(muted);
                    self.command_if_live(Command::SetMute(muted), cx);
                }
            }
            (Target::SelfVoice(_), "self-deaf") => {
                if let Some(deaf) = muted {
                    self.state.voice.set_deafened(deaf);
                    self.command_if_live(Command::SetDeaf(deaf), cx);
                }
            }
            (Target::SelfVoice(_), "stop-share") => self.command_if_live(Command::StopShare, cx),
            (Target::SelfVoice(_), "voice-settings") => {
                self.open_settings(settings_view::Section::Voice, window, cx);
            }
            (Target::SelfVoice(_) | Target::Channel(_), "disconnect") => {
                self.command_if_live(Command::LeaveVoice, cx)
            }
            (Target::Channel(channel), "channel-delete") => {
                if let Some(c) = self.state.channels.get(channel) {
                    self.context.channel_delete_confirm =
                        Some((*channel, c.name.clone(), c.kind == ChannelType::Category));
                    self.context.kick_previous = window.focused(cx);
                    self.context.kick_selected = false;
                    self.context.kick_armed = false;
                    window.focus(&self.context.kick_focus);
                }
            }
            (Target::Channel(channel), "channel-edit") => {
                if let Some(c) = self.state.channels.get(channel)
                    && let Some(guild) = c.guild_id
                {
                    self.channel_name
                        .update(cx, |input, cx| input.set(c.name.clone(), cx));
                    self.selected_guild = Some(guild);
                    self.channel_form = Some((guild, None, c.kind.code(), Some(*channel)));
                }
            }
            (Target::Channel(channel), "category-create-text" | "category-create-voice") => {
                if let Some(guild) = self.state.channels.get(channel).and_then(|c| c.guild_id) {
                    self.channel_name
                        .update(cx, |input, cx| input.set(String::new(), cx));
                    self.selected_guild = Some(guild);
                    self.channel_form = Some((
                        guild,
                        Some(*channel),
                        if id == "category-create-text" { 0 } else { 2 },
                        None,
                    ));
                }
            }
            (
                Target::Community(guild),
                "community-create-text" | "community-create-voice" | "community-create-category",
            ) => {
                self.channel_name
                    .update(cx, |input, cx| input.set(String::new(), cx));
                self.selected_guild = Some(*guild);
                self.channel_form = Some((
                    *guild,
                    None,
                    match id {
                        "community-create-text" => 0,
                        "community-create-voice" => 2,
                        _ => 4,
                    },
                    None,
                ));
            }
            (Target::Channel(channel), "channel-open") => self.select(*channel, cx),
            (Target::Channel(channel), "channel-join") => {
                let guild_id = self.state.channels.get(channel).and_then(|c| c.guild_id);
                self.command_if_live(
                    Command::JoinVoice {
                        guild_id,
                        channel_id: *channel,
                    },
                    cx,
                );
            }
            (Target::Channel(channel), "copy-channel") => {
                cx.write_to_clipboard(ClipboardItem::new_string(channel.to_string()))
            }
            (Target::Channel(channel), "copy-channel-link") => {
                if let Some(url) = self.message_links.channel_link(
                    self.state.channels.get(channel).and_then(|c| c.guild_id),
                    *channel,
                ) {
                    cx.write_to_clipboard(ClipboardItem::new_string(url));
                }
            }
            (Target::Community(guild), "copy-community") => {
                cx.write_to_clipboard(ClipboardItem::new_string(guild.to_string()))
            }
            (Target::Community(guild), "community-overview") => {
                if self.community_ui.pending || !self.can_edit_community_overview(*guild) {
                    return;
                }
                let Some(community) = self.state.guilds.get(guild) else {
                    return;
                };
                self.community_ui.window = Some(window.window_handle());
                self.community_ui.previous_focus = window.focused(cx);
                self.community_ui
                    .open_overview(*guild, community.name.clone(), cx);
                window.focus(&self.community_ui.name.focus_handle(cx));
                cx.notify();
            }
            (Target::Community(guild), "community-leave") => {
                if self.community_ui.pending {
                    return;
                }
                self.community_ui.window = Some(window.window_handle());
                self.community_ui.previous_focus = window.focused(cx);
                self.community_ui.open_leave(*guild);
                window.focus(&self.community_ui.focus);
                cx.notify();
            }
            (Target::Community(guild), "community-open") => {
                self.selected_guild = Some(*guild);
                self.command(Command::SelectGuild { guild_id: *guild }, cx);
            }
            _ => (),
        }
    }
    pub(super) fn reconcile_context_menu(&mut self, cx: &mut Context<Self>) {
        if self
            .context
            .target
            .as_ref()
            .is_some_and(|target| !self.context_target_valid(target))
        {
            self.reset_context_menu(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AudioChange, Target, UserAudioOverride, audio_identities, enabled, participant_target,
        toggle, volume,
    };
    use crate::context_menu::ContextMenuEvent;
    use fluxer_core::Snowflake;
    use fluxer_media::ParticipantAudioSettings;
    #[test]
    fn user_audio_uses_exact_ids_and_all_devices_without_touching_other_users() {
        let ids = [
            "user_1_a",
            "user_1_second_device",
            "user_10_a",
            "preview-1",
            "user_1_",
        ]
        .map(String::from);
        assert_eq!(
            audio_identities(ids.iter(), Snowflake(1)),
            vec!["user_1_a", "user_1_second_device"]
        );
    }
    #[test]
    fn gain_mute_and_new_device_overrides_preserve_independent_fields() {
        let original = ParticipantAudioSettings {
            gain: 0.35,
            muted: false,
        };
        let muted = AudioChange::Mute(true).apply(original);
        assert_eq!(muted.gain, 0.35);
        assert!(muted.muted);
        let louder = AudioChange::Gain(9.).apply(muted);
        assert_eq!(louder.gain, 2.);
        assert!(louder.muted);
        assert_eq!(AudioChange::Gain(f32::NAN).apply(louder).gain, 2.);
        let mut preference = UserAudioOverride::default();
        AudioChange::Mute(true).remember(&mut preference);
        let new = preference.apply(original);
        assert!(new.muted);
        assert_eq!(new.gain, 0.35);
        AudioChange::Gain(0.8).remember(&mut preference);
        assert!((preference.apply(original).gain - 0.28).abs() < 0.001);
    }
    #[test]
    fn stale_or_wrong_event_types_and_nonfinite_gains_are_rejected() {
        let items = vec![toggle("mute", "Mute", false), volume("gain", "Volume", 1.)];
        assert!(enabled(
            &items,
            &ContextMenuEvent::Toggle("mute".into(), true)
        ));
        assert!(!enabled(&items, &ContextMenuEvent::Action("mute".into())));
        assert!(!enabled(
            &items,
            &ContextMenuEvent::Slider("gain".into(), f32::INFINITY)
        ));
        assert!(!enabled(
            &items,
            &ContextMenuEvent::Toggle("server-mute".into(), true)
        ));
    }
    #[test]
    fn own_remote_or_unresolved_gateway_device_never_controls_this_device() {
        let channel = Some(Snowflake(14));
        let me = Some(Snowflake(100));
        assert_eq!(
            participant_target(channel, me, Some("user_100_phone".into()), me, true),
            Some(Target::Connection("user_100_phone".into()))
        );
        assert_eq!(
            participant_target(channel, me, Some("user_100_phone".into()), me, false),
            Some(Target::User {
                channel: Snowflake(14),
                user: Snowflake(100)
            })
        );
        assert_eq!(
            participant_target(channel, me, None, me, false),
            Some(Target::User {
                channel: Snowflake(14),
                user: Snowflake(100)
            })
        );
    }
    #[test]
    fn user_and_connection_gain_are_independent_and_mutes_compose() {
        let mut preference = UserAudioOverride::default();
        AudioChange::Gain(0.8).remember(&mut preference);
        let connection = ParticipantAudioSettings {
            gain: 0.3,
            muted: true,
        };
        assert!((preference.apply(connection).gain - 0.24).abs() < 0.001);
        AudioChange::Gain(0.9).remember(&mut preference);
        let effective = preference.apply(connection);
        assert!((effective.gain - 0.27).abs() < 0.001);
        assert!(effective.muted);
        assert_eq!(connection.gain, 0.3);
        AudioChange::Mute(false).remember(&mut preference);
        assert!(preference.apply(connection).muted);
    }
}
