// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::controls::quiet_button;
use fluxer_core::Permissions;

impl Fluxer {
    pub(super) fn threads_by_parent(&self) -> BTreeMap<Snowflake, Vec<&fluxer_core::Channel>> {
        let (Some(guild), Some(user)) = (self.selected_guild, &self.state.current_user) else {
            return BTreeMap::new();
        };
        if !self.state.thread_guilds.contains(&guild) {
            return BTreeMap::new();
        }
        let required = Permissions::VIEW_CHANNEL | Permissions::READ_MESSAGE_HISTORY;
        let mut grouped: BTreeMap<Snowflake, Vec<_>> = BTreeMap::new();
        for thread in self.state.channels.values().filter(|thread| {
            thread.guild_id == Some(guild)
                && thread.kind.is_thread()
                && (!thread.thread_archived() || self.state.selected_channel == Some(thread.id))
                && self
                    .state
                    .permissions_for(thread.id, user.id)
                    .contains(required)
        }) {
            if let Some(parent) = thread.parent_id {
                grouped.entry(parent).or_default().push(thread);
            }
        }
        for threads in grouped.values_mut() {
            threads.sort_by_key(|thread| {
                std::cmp::Reverse(thread.last_message_id.unwrap_or(thread.id))
            });
        }
        grouped
    }

    pub(super) fn sidebar_threads(
        &self,
        threads: &[&fluxer_core::Channel],
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        threads
            .iter()
            .map(|thread| {
                let id = thread.id;
                let label = format!("↳  {}", thread.name);
                button_base(("thread", id.0))
                    .ml_4()
                    .bg(rgb(if self.state.selected_channel == Some(id) {
                        0x3b344f
                    } else {
                        PANEL
                    }))
                    .when(
                        self.state.is_unread(id) && !self.channel_notifications_muted(thread),
                        |view| view.text_color(rgb(TEXT)).font_weight(FontWeight::SEMIBOLD),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .text_ellipsis()
                            .child(emoji_sprites::unicode_text(&label, 13., false, cx)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.select(id, cx)))
                    .into_any_element()
            })
            .collect()
    }

    pub(super) fn open_thread_form(
        &mut self,
        parent: Snowflake,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.creating_thread.is_some() {
            return;
        }
        if self.thread_form != Some(parent) {
            self.thread_name
                .update(cx, |input, cx| input.set(String::new(), cx));
        }
        self.thread_form = Some(parent);
        self.thread_name.focus_handle(cx).focus(window);
        cx.notify();
    }

    pub(super) fn render_thread_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let creating = self.creating_thread.is_some();
        div()
            .px_5()
            .py_2()
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_1().min_w_0().child(self.thread_name.clone()))
            .when(!creating, |view| {
                view.child(
                    quiet_button("create-thread", "Create")
                        .on_click(cx.listener(|this, _, _, cx| this.submit_public_thread(cx))),
                )
                .child(
                    quiet_button("cancel-thread", "Cancel").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.thread_form = None;
                            cx.notify();
                        },
                    )),
                )
            })
            .when(creating, |view| view.child("Creating thread…"))
    }

    pub(super) fn submit_public_thread(&mut self, cx: &mut Context<Self>) {
        let Some(parent_id) = self.thread_form.filter(|parent| {
            Some(*parent) == self.state.selected_channel && self.creating_thread.is_none()
        }) else {
            return;
        };
        let name = self.thread_name.read(cx).content.trim().to_owned();
        if !(1..=100).contains(&name.encode_utf16().count()) {
            self.error = Some("Use a thread name between 1 and 100 characters".into());
            cx.notify();
            return;
        }
        let command = Command::CreatePublicThread { parent_id, name };
        let result = self
            .controller
            .as_ref()
            .ok_or_else(|| "The native client worker is unavailable".to_owned())
            .and_then(|controller| controller.send(command));
        match result {
            Ok(()) => {
                self.error = None;
                self.creating_thread = Some(parent_id);
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    pub(super) fn finish_thread_creation(
        &mut self,
        parent: Snowflake,
        result: Result<Snowflake, String>,
        cx: &mut Context<Self>,
    ) {
        if self.creating_thread != Some(parent) {
            return;
        }
        self.creating_thread = None;
        match result {
            Ok(thread) => {
                self.thread_form = None;
                if self.state.selected_channel == Some(parent)
                    && self
                        .state
                        .channels
                        .get(&parent)
                        .and_then(|channel| channel.guild_id)
                        == self.selected_guild
                    && self.pane == Pane::Chat
                {
                    self.select(thread, cx);
                }
            }
            Err(error) => self.error = Some(error),
        }
    }
}
