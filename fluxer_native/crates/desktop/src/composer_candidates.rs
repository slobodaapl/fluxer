// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use fluxer_core::Permissions;
use input::composer_completion::{Choice, Kind, rank};
impl Fluxer {
    pub(super) fn schedule_composer_demo(&self, window: &Window, cx: &mut Context<Self>) {
        if !self.demo || !std::env::args().any(|arg| arg == "--demo") {
            return;
        }
        let Ok(mode) = std::env::var("FLUXER_COMPOSER_DEMO") else {
            return;
        };
        if !matches!(
            mode.as_str(),
            "mention" | "channel" | "emoji" | "slowmode" | "multiline"
        ) {
            return;
        }
        let handle = window.window_handle();
        cx.spawn(async move |entity, cx| {
            Timer::after(Duration::from_millis(800)).await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = entity.update(cx, |view, cx| {
                    if mode == "slowmode" {
                        if let Some(id) = view.state.selected_channel {
                            if let Some(channel) = view.state.channels.get_mut(&id) {
                                channel.rate_limit_per_user = 15;
                            }
                            // Restrict the synthetic current account only, leaving fixture targets visible.
                            if let Some(user) = view.state.current_user.as_ref() {
                                if let Some(guild) = view
                                    .state
                                    .channels
                                    .get(&id)
                                    .and_then(|c| c.guild_id)
                                    .and_then(|id| view.state.guilds.get_mut(&id))
                                {
                                    guild.owner_id = Snowflake(999);
                                    for role in &mut guild.roles {
                                        role.permissions = fluxer_core::Permissions::VIEW_CHANNEL
                                            | fluxer_core::Permissions::SEND_MESSAGES;
                                    }
                                    if let Some(member) =
                                        view.state.members.get_mut(&(guild.id, user.id))
                                    {
                                        member.roles.clear();
                                    }
                                }
                            }
                            view.slowmode.sent(id);
                        }
                    }
                    let text = match mode.as_str() {
                        "channel" => "Meet in #",
                        "emoji" => "Looks good :sm",
                        "multiline" => "First line\nUnicode 雪 🌈\nHello @",
                        "slowmode" => "This draft stays safe while slowmode counts down.",
                        _ => "Hello @",
                    };
                    view.composer
                        .update(cx, |input, cx| input.set(text.into(), cx));
                    view.composer.focus_handle(cx).focus(window);
                    view.refresh_composer_completion(cx);
                    cx.notify();
                });
            });
        })
        .detach();
    }

    pub(super) fn refresh_composer_completion(&mut self, cx: &mut Context<Self>) {
        let trigger = self.composer.read(cx).completion_trigger();
        let mut choices = Vec::new();
        if let (Some(trigger), Some(channel)) = (
            trigger,
            self.state
                .selected_channel
                .and_then(|id| self.state.channels.get(&id)),
        ) {
            match trigger.kind {
                Kind::Mention => {
                    if let Some(guild) = channel.guild_id {
                        for ((guild_id, _), member) in &self.state.members {
                            if *guild_id != guild
                                || !self
                                    .state
                                    .permissions_for(channel.id, member.user.id)
                                    .contains(Permissions::VIEW_CHANNEL)
                            {
                                continue;
                            }
                            let label =
                                member.nick.as_deref().unwrap_or(member.user.display_name());
                            if rank(label, &trigger.query).is_some()
                                || rank(&member.user.username, &trigger.query).is_some()
                            {
                                choices.push(Choice {
                                    label: label.into(),
                                    detail: format!("@{}", member.user.username),
                                    glyph: "@".into(),
                                    insert: format!("<@{}>", member.user.id),
                                    user: Some(member.user.clone()),
                                    media: self.media_endpoint.clone(),
                                    image: None,
                                    status: Some(completion_status(&self.state, member.user.id)),
                                });
                            }
                        }
                        if let Some(guild) = self.state.guilds.get(&guild) {
                            for role in &guild.roles {
                                let permitted = role.mentionable
                                    || self.state.current_user.as_ref().is_some_and(|user| {
                                        self.state
                                            .permissions_for(channel.id, user.id)
                                            .contains(Permissions::MENTION_EVERYONE)
                                    });
                                if role.id == guild.id
                                    || !permitted
                                    || rank(&role.name, &trigger.query).is_none()
                                {
                                    continue;
                                }
                                choices.push(Choice {
                                    label: role.name.clone(),
                                    detail: "Role".into(),
                                    glyph: "◉".into(),
                                    insert: format!("<@&{}>", role.id),
                                    user: None,
                                    media: None,
                                    image: None,
                                    status: None,
                                });
                            }
                        }
                    } else {
                        for user in &channel.recipients {
                            if rank(user.display_name(), &trigger.query).is_some()
                                || rank(&user.username, &trigger.query).is_some()
                            {
                                choices.push(Choice {
                                    label: user.display_name().into(),
                                    detail: format!("@{}", user.username),
                                    glyph: "@".into(),
                                    insert: format!("<@{}>", user.id),
                                    user: Some(user.clone()),
                                    media: self.media_endpoint.clone(),
                                    image: None,
                                    status: Some(completion_status(&self.state, user.id)),
                                });
                            }
                        }
                    }
                }
                Kind::Channel => {
                    for candidate in self.state.channels.values() {
                        if !candidate.kind.is_message_channel()
                            || candidate.guild_id != channel.guild_id
                            || candidate.guild_id.is_none()
                            || rank(&candidate.name, &trigger.query).is_none()
                        {
                            continue;
                        }
                        if !self.state.current_user.as_ref().is_some_and(|user| {
                            self.state
                                .permissions_for(candidate.id, user.id)
                                .contains(Permissions::VIEW_CHANNEL)
                        }) {
                            continue;
                        }
                        choices.push(Choice {
                            label: candidate.name.clone(),
                            detail: "Channel".into(),
                            glyph: "#".into(),
                            insert: format!("<#{}>", candidate.id),
                            user: None,
                            media: None,
                            image: None,
                            status: None,
                        });
                    }
                }
                Kind::Emoji => {
                    for emoji in chat_helpers::EmojiCatalog::new(&self.state, Some(channel.id))
                        .search(&trigger.query, "all")
                        .into_iter()
                        .take(30)
                    {
                        choices.push(Choice {
                            label: format!(":{}:", emoji.name),
                            detail: emoji.label.clone(),
                            glyph: if emoji.id.is_none() {
                                emoji.insert.clone()
                            } else {
                                "☺".into()
                            },
                            insert: emoji.insert.clone(),
                            user: None,
                            media: None,
                            status: None,
                            image: emoji.id.and_then(|id| {
                                self.media_endpoint.as_ref().map(|base| {
                                    format!(
                                        "{}/emojis/{id}.webp?size=48",
                                        base.trim_end_matches('/')
                                    )
                                })
                            }),
                        });
                    }
                }
            }
            if trigger.kind != Kind::Emoji {
                choices.sort_by_key(|choice| {
                    (
                        rank(&choice.label, &trigger.query).unwrap_or(3),
                        choice.label.to_lowercase(),
                    )
                });
            }
            choices.truncate(30);
        }
        self.composer
            .update(cx, |input, cx| input.set_completion_choices(choices, cx));
    }
}

fn completion_status(state: &fluxer_core::ClientState, user: fluxer_core::Snowflake) -> u32 {
    use fluxer_core::PresenceStatus;
    match state
        .presences
        .get(&user)
        .map(|p| p.status)
        .unwrap_or_default()
    {
        PresenceStatus::Online => 0x43b581,
        PresenceStatus::Idle => 0xfaa61a,
        PresenceStatus::Dnd => 0xf04747,
        _ => 0x747f8d,
    }
}
