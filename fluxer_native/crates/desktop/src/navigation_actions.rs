// SPDX-License-Identifier: AGPL-3.0-or-later
//! Read and notification actions backed by classic ReadStateCommands and
//! UserGuildSettingsCommands. Thread membership notifications use another API.
use super::*;
use crate::{
    context_actions::{Target, action},
    context_menu::{ContextMenuEvent, ContextMenuItem as Item},
    controls::Icon,
};
use fluxer_core::Permissions;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum NavigationPage {
    #[default]
    Main,
    Mute,
    Notifications,
}
const MUTE_WINDOWS: &[(u64, &str)] = &[
    (900, "For 15 minutes"),
    (1800, "For 30 minutes"),
    (3600, "For 1 hour"),
    (10800, "For 3 hours"),
    (14400, "For 4 hours"),
    (28800, "For 8 hours"),
    (86400, "For 24 hours"),
    (259200, "For 3 days"),
    (0, "Until I turn it back on"),
];
fn supports_notification_override(kind: ChannelType) -> bool {
    matches!(
        kind,
        ChannelType::Text
            | ChannelType::Announcement
            | ChannelType::Voice
            | ChannelType::Category
            | ChannelType::Dm
            | ChannelType::GroupDm
            | ChannelType::Forum
            | ChannelType::Media
    )
}
fn level_name(level: i8) -> &'static str {
    match level {
        0 => "All messages",
        1 => "Only mentions",
        2 => "Nothing",
        _ => "Default",
    }
}
fn notification_selection(explicit: i8, effective: i8, inheritable: bool) -> i8 {
    if (0..=2).contains(&explicit) {
        explicit
    } else if inheritable {
        3
    } else {
        effective
    }
}
fn notification_choice(level: u8, label: &str, selected: i8) -> Item {
    Item::Action {
        id: format!("notify-level-{level}"),
        label: if level as i8 == selected {
            format!("✓ {label}")
        } else {
            label.into()
        },
        icon: None,
        enabled: true,
        danger: false,
    }
}
impl Fluxer {
    pub(super) fn schedule_read_menu_demo(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !self.demo || !std::env::args().any(|arg| arg == "--demo") {
            return;
        }
        let Ok(kind) = std::env::var("FLUXER_READ_MENU_DEMO") else {
            return;
        };
        if !matches!(
            kind.as_str(),
            "channel" | "community" | "message" | "category" | "dm"
        ) {
            return;
        }
        self.command(
            Command::SetGuildNotificationSettings {
                guild_id: Some(Snowflake(1)),
                muted: None,
                mute_seconds: None,
                message_notifications: Some(1),
            },
            cx,
        );
        self.command(
            Command::MarkMessageUnread {
                channel_id: Snowflake(11),
                message_id: Snowflake(1001),
            },
            cx,
        );
        let handle = window.window_handle();
        cx.spawn(async move |entity, cx| {
            Timer::after(Duration::from_millis(700)).await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = entity.update(cx, |view, cx| {
                    view.focus.focus(window);
                    let size = window.viewport_size();
                    let anchor = point(size.width - px(24.), size.height - px(24.));
                    if kind == "message" {
                        if let Some(message) = view
                            .state
                            .messages
                            .get(&Snowflake(11))
                            .and_then(|messages| messages.get(&Snowflake(1001)))
                            .cloned()
                        {
                            view.open_message_menu(
                                message,
                                anchor,
                                crate::message_actions::MessageMenuContext::Timeline,
                                window,
                                cx,
                            );
                        }
                    } else {
                        let target = match kind.as_str() {
                            "community" => Target::Community(Snowflake(1)),
                            "category" => Target::Channel(Snowflake(10)),
                            "dm" => Target::Channel(Snowflake(40)),
                            _ => Target::Channel(Snowflake(11)),
                        };
                        view.open_context_menu(target, anchor, window, cx);
                    }
                });
            });
        })
        .detach();
    }
    fn notification_scope(
        &self,
        target: &Target,
    ) -> Option<(Option<Snowflake>, Option<Snowflake>)> {
        let scope = match target {
            Target::Community(guild) => (Some(*guild), None),
            Target::Channel(channel) => {
                let channel = self.state.channels.get(channel)?;
                if !supports_notification_override(channel.kind) {
                    return None;
                }
                (channel.guild_id, Some(channel.id))
            }
            _ => return None,
        };
        if scope.0.is_some_and(|id| {
            self.state.guilds.get(&id).is_none_or(|guild| {
                guild
                    .extra
                    .get("visiting")
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
            })
        }) {
            return None;
        }
        Some(scope)
    }
    fn navigation_has_unread(&self, target: &Target) -> bool {
        let unread = |id| {
            self.state.is_unread(id)
                || self
                    .state
                    .read_states
                    .get(&id)
                    .is_some_and(|read| read.mention_count > 0)
        };
        match target {
            Target::Channel(id) => {
                if self
                    .state
                    .channels
                    .get(id)
                    .is_some_and(|channel| channel.kind == ChannelType::Category)
                {
                    self.state.channels.values().any(|channel| {
                        channel.parent_id == Some(*id)
                            && channel.kind != ChannelType::Category
                            && self.can_view_navigation_channel(channel.id)
                            && unread(channel.id)
                    })
                } else {
                    self.can_view_navigation_channel(*id) && unread(*id)
                }
            }
            Target::Community(guild) => self.state.channels.values().any(|channel| {
                channel.guild_id == Some(*guild)
                    && self.can_view_navigation_channel(channel.id)
                    && unread(channel.id)
            }),
            _ => false,
        }
    }
    fn can_view_navigation_channel(&self, id: Snowflake) -> bool {
        self.state.current_user.as_ref().is_some_and(|user| {
            self.state
                .permissions_for(id, user.id)
                .contains(Permissions::VIEW_CHANNEL)
        })
    }
    pub(super) fn channel_notifications_muted(&self, channel: &fluxer_core::Channel) -> bool {
        supports_notification_override(channel.kind)
            && self
                .state
                .channel_notification_settings(
                    channel.guild_id,
                    channel.id,
                    chrono::Utc::now().timestamp_millis(),
                )
                .effective_muted
    }
    pub(super) fn navigation_items(&self, target: &Target) -> Vec<Item> {
        let mut items = Vec::new();
        let visiting = match target {
            Target::Community(id) => Some(*id),
            Target::Channel(id) => self
                .state
                .channels
                .get(id)
                .and_then(|channel| channel.guild_id),
            _ => None,
        }
        .is_some_and(|id| {
            self.state
                .guilds
                .get(&id)
                .and_then(|guild| guild.extra.get("visiting"))
                .and_then(serde_json::Value::as_bool)
                == Some(true)
        });
        if visiting {
            return items;
        }
        if self.navigation_has_unread(target) {
            items.push(action("mark-read", "Mark as read", Icon::MarkRead));
        }
        if let Some((guild, channel)) = self.notification_scope(target) {
            let now = chrono::Utc::now().timestamp_millis();
            let muted = channel
                .map(|channel| {
                    self.state
                        .channel_notification_settings(guild, channel, now)
                        .explicit_muted
                })
                .unwrap_or_else(|| self.state.guild_notification_settings(guild).is_muted(now));
            let noun = if channel.is_some() {
                "channel"
            } else {
                "community"
            };
            items.push(action(
                if muted {
                    "notify-unmute"
                } else {
                    "notify-mute"
                },
                &format!(
                    "{} {noun}{}",
                    if muted { "Unmute" } else { "Mute" },
                    if muted { "" } else { "…" }
                ),
                if muted {
                    Icon::Notifications
                } else {
                    Icon::NotificationsOff
                },
            ));
            items.push(action(
                "notify-settings",
                "Notification settings…",
                Icon::Notifications,
            ));
        }
        items
    }
    pub(super) fn navigation_page_items(&self, target: &Target) -> Vec<Item> {
        let Some((guild, channel)) = self.notification_scope(target) else {
            return Vec::new();
        };
        let mut items = vec![action("notify-back", "Back", Icon::Back), Item::Separator];
        match self.context.page {
            NavigationPage::Mute => {
                items.push(Item::Header(
                    if channel.is_some() {
                        "Mute channel"
                    } else {
                        "Mute community"
                    }
                    .into(),
                ));
                for (seconds, label) in MUTE_WINDOWS {
                    items.push(action(
                        &format!("notify-mute-{seconds}"),
                        label,
                        Icon::NotificationsOff,
                    ));
                }
            }
            NavigationPage::Notifications => {
                let now = chrono::Utc::now().timestamp_millis();
                let settings = self.state.guild_notification_settings(guild);
                let explicit = channel
                    .map(|id| {
                        self.state
                            .channel_notification_settings(guild, id, now)
                            .message_notifications
                    })
                    .unwrap_or(settings.message_notifications);
                let effective = channel
                    .map(|id| {
                        self.state
                            .channel_notification_settings(guild, id, now)
                            .effective_message_notifications
                    })
                    .unwrap_or_else(|| self.state.guild_effective_message_notifications(guild));
                let selected = notification_selection(
                    explicit,
                    effective,
                    channel.is_some() && guild.is_some(),
                );
                items.push(Item::Header("Notifications".into()));
                if let Some(id) = channel
                    && guild.is_some()
                {
                    let parent_is_category = self
                        .state
                        .channels
                        .get(&id)
                        .and_then(|c| c.parent_id)
                        .and_then(|id| self.state.channels.get(&id))
                        .is_some_and(|c| c.kind == ChannelType::Category);
                    items.push(notification_choice(
                        3,
                        if parent_is_category {
                            "Category default"
                        } else {
                            "Community default"
                        },
                        selected,
                    ));
                    if selected == 3 {
                        items.push(Item::Header(format!("Default: {}", level_name(effective))));
                    }
                }
                for level in 0..=2 {
                    items.push(notification_choice(
                        level,
                        level_name(level as i8),
                        selected,
                    ));
                }
                if channel.is_some_and(|id| {
                    self.state
                        .channel_notification_settings(guild, id, now)
                        .effective_muted
                }) || settings.is_muted(now)
                {
                    items.push(Item::Separator);
                    items.push(Item::Header("Notifications are muted".into()));
                }
            }
            NavigationPage::Main => (),
        }
        items
    }
    pub(super) fn perform_navigation_action(
        &mut self,
        target: &Target,
        event: &ContextMenuEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let ContextMenuEvent::Action(id) = event else {
            return false;
        };
        if self.demo && std::env::var_os("FLUXER_READ_MENU_DEMO").is_some() {
            eprintln!("READ_MENU_DEMO action={id}");
        }
        if id == "mark-read" && self.navigation_has_unread(target) {
            let command = match target {
                Target::Channel(channel) => Command::MarkChannelRead {
                    channel_id: *channel,
                },
                Target::Community(guild) => Command::MarkGuildRead { guild_id: *guild },
                _ => return false,
            };
            self.command(command, cx);
            return true;
        }
        let Some((guild, channel)) = self.notification_scope(target) else {
            return false;
        };
        let page = match id.as_str() {
            "notify-back" => Some(NavigationPage::Main),
            "notify-settings" => Some(NavigationPage::Notifications),
            "notify-mute" => Some(NavigationPage::Mute),
            _ => None,
        };
        if let Some(page) = page {
            self.open_context_menu_page(target.clone(), self.context.anchor, page, window, cx);
            return true;
        }
        let (muted, seconds, level) = if id == "notify-unmute" {
            (Some(false), None, None)
        } else if let Some(seconds) = id
            .strip_prefix("notify-mute-")
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|s| MUTE_WINDOWS.iter().any(|(value, _)| value == s))
        {
            (Some(true), (seconds != 0).then_some(seconds), None)
        } else if let Some(level) = id
            .strip_prefix("notify-level-")
            .and_then(|s| s.parse::<u8>().ok())
            .filter(|level| *level <= 2 || (*level == 3 && guild.is_some() && channel.is_some()))
        {
            (None, None, Some(level))
        } else {
            return false;
        };
        if let Some(channel_id) = channel {
            self.command(
                Command::SetChannelNotificationSettings {
                    guild_id: guild,
                    channel_id,
                    muted,
                    mute_seconds: seconds,
                    message_notifications: level,
                },
                cx,
            );
        } else {
            self.command(
                Command::SetGuildNotificationSettings {
                    guild_id: guild,
                    muted,
                    mute_seconds: seconds,
                    message_notifications: level,
                },
                cx,
            );
        }
        true
    }
    pub(super) fn mark_message_unread(&mut self, message: &Message, cx: &mut Context<Self>) {
        let Some(messages) = self.state.messages.get(&message.channel_id) else {
            return;
        };
        if !messages.contains_key(&message.id) {
            return;
        }
        let previous = messages
            .range(..message.id)
            .next_back()
            .map(|(id, _)| *id)
            .unwrap_or(Snowflake(((message.id.0 >> 22).saturating_sub(1)) << 22));
        if previous.0 == 0 {
            return;
        }
        if self.demo && std::env::var_os("FLUXER_READ_MENU_DEMO").is_some() {
            eprintln!("READ_MENU_DEMO action=mark-unread");
        }
        self.reset_ack_context(message.channel_id);
        // Block another paint's automatic ACK while the worker applies manual unread.
        let read = self
            .state
            .read_states
            .entry(message.channel_id)
            .or_insert_with(|| fluxer_core::ReadState {
                id: message.channel_id,
                ..Default::default()
            });
        read.manual_ack = true;
        self.command(
            Command::MarkMessageUnread {
                channel_id: message.channel_id,
                message_id: message.id,
            },
            cx,
        );
        cx.notify();
    }
}
#[cfg(test)]
mod tests {
    use super::{notification_selection, supports_notification_override};
    use fluxer_core::ChannelType;
    #[test]
    fn inherited_channel_selection_stays_distinct_from_effective_and_private_defaults() {
        assert_eq!(notification_selection(-1, 1, true), 3);
        assert_eq!(notification_selection(3, 2, true), 3);
        assert_eq!(notification_selection(0, 1, true), 0);
        assert_eq!(notification_selection(3, 0, false), 0);
        assert_eq!(notification_selection(-1, 1, false), 1);
    }
    #[test]
    fn notification_overrides_exclude_threads_links_and_unknowns() {
        for kind in [
            ChannelType::AnnouncementThread,
            ChannelType::PublicThread,
            ChannelType::PrivateThread,
            ChannelType::Link,
            ChannelType::Unknown(42),
            ChannelType::PersonalNotes,
        ] {
            assert!(!supports_notification_override(kind));
        }
        for kind in [
            ChannelType::Text,
            ChannelType::Voice,
            ChannelType::Dm,
            ChannelType::Category,
            ChannelType::Forum,
        ] {
            assert!(supports_notification_override(kind));
        }
    }
}
