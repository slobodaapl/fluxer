// SPDX-License-Identifier: AGPL-3.0-or-later
//! Application glue for the independently focused profile popout.
use super::*;
use crate::profile_view::ProfileOpen;
use fluxer_core::User;
impl Fluxer {
    pub(super) fn open_user_profile(
        &mut self,
        user: User,
        guild: Option<Snowflake>,
        webhook: bool,
        anchor: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_user_profile_at(
            user,
            guild,
            webhook,
            Bounds::new(anchor, size(px(0.), px(0.))),
            window,
            cx,
        );
    }
    pub(super) fn open_user_profile_at(
        &mut self,
        user: User,
        guild: Option<Snowflake>,
        webhook: bool,
        trigger: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let anchor = point(trigger.right(), trigger.top());
        if self.state.current_user.is_none()
            || self.session_restoring
            || self.image_viewer.is_open()
            || self.playback.modal.is_open()
            || self.message_links.is_open()
            || self.search_ui.confirm.is_some()
        {
            return;
        }
        let guild = guild.filter(|id| self.state.guilds.contains_key(id));
        let member = (!webhook)
            .then(|| guild.and_then(|guild| self.state.members.get(&(guild, user.id)).cloned()))
            .flatten();
        let roles = guild
            .and_then(|guild| self.state.guilds.get(&guild))
            .map(|guild| guild.roles.clone())
            .unwrap_or_default();
        let manageable_roles = guild
            .and_then(|id| {
                let g = self.state.guilds.get(&id)?;
                let actor = self
                    .state
                    .current_user
                    .as_ref()
                    .and_then(|u| self.state.members.get(&(id, u.id)))?;
                member.as_ref()?;
                Some(
                    g.roles
                        .iter()
                        .filter(|r| fluxer_core::can_assign_role(g, actor, r))
                        .map(|r| r.id)
                        .collect(),
                )
            })
            .unwrap_or_default();
        let community_name = guild
            .and_then(|guild| self.state.guilds.get(&guild))
            .map(|guild| guild.name.clone());
        let user_id = user.id;
        if self.demo && std::env::var_os("FLUXER_PROFILE_DEMO").is_some() {
            eprintln!("PROFILE_DEMO open={user_id} webhook={webhook}");
        }
        if self.role_mention.read(cx).is_open() {
            self.role_mention
                .update(cx, |popup, cx| popup.dismiss(window, cx));
        }
        self.reset_context_menu(cx);
        self.dismiss_message_actions(window, cx);
        self.emoji_target = None;
        if !self.demo {
            self.command(Command::SetPushToTalkPressed(false), cx);
        }
        let open = ProfileOpen {
            user,
            guild_id: guild,
            member,
            roles,
            manageable_roles,
            media_endpoint: self.media_endpoint.clone(),
            community_name,
            anchor,
            webhook,
            can_message: !webhook
                && self
                    .state
                    .current_user
                    .as_ref()
                    .is_some_and(|current| current.id != user_id),
            can_relationship: !self.demo
                && !webhook
                && self
                    .state
                    .current_user
                    .as_ref()
                    .is_some_and(|current| current.id != user_id),
            friend_action: if !self.demo
                && !webhook
                && self
                    .state
                    .current_user
                    .as_ref()
                    .is_some_and(|current| current.id != user_id)
            {
                match self.state.relationships.get(&user_id).map(|r| r.kind) {
                    None => Some(fluxer_client::RelationshipAction::Send),
                    Some(3) => Some(fluxer_client::RelationshipAction::Accept),
                    _ => None,
                }
            } else {
                None
            },
            blocked: self
                .state
                .relationships
                .get(&user_id)
                .is_some_and(|relationship| relationship.is_blocked()),
        };
        self.profile_ui.popup.update(cx, |popup, cx| {
            popup.open(open, window, cx);
            popup.set_trigger_bounds(Some(trigger));
        });
        if !webhook {
            self.command(
                Command::LoadUserProfile {
                    user_id,
                    guild_id: guild,
                    force: false,
                },
                cx,
            );
        }
        cx.notify();
    }
    pub(super) fn dismiss_user_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.profile_ui
            .popup
            .update(cx, |popup, cx| popup.dismiss(window, cx));
    }
}
