// SPDX-License-Identifier: AGPL-3.0-or-later
//! TrustedDomain.ts, DeepLinkUtils.ts and ExternalLinkWarningModal.tsx policy.
//! External links use authoritative account trust, with an opt-in host action.
use super::*;
use crate::{
    controls::quiet_button,
    message_actions::{ConfirmationChoice, ConfirmationKeys},
    message_markup::MessageTarget,
};
use fluxer_core::Permissions;
use reqwest::Url;

const BUILTIN_TRUST: &[&str] = &[
    "fluxer.app",
    "*.fluxer.app",
    "fluxer.com",
    "*.fluxer.com",
    "fluxer.dev",
    "*.fluxer.dev",
    "fluxerstatus.com",
    "*.fluxerstatus.com",
    "fluxer.gg",
    "fluxer.gift",
];
const OFFICIAL_INTERNAL_HOSTS: &[&str] = &[
    "web.fluxer.app",
    "web.canary.fluxer.app",
    "fluxer.com",
    "canary.fluxer.com",
    "fluxer.app",
    "canary.fluxer.app",
];

pub(super) struct MessageLinks {
    destination: Option<Url>,
    text: Option<Entity<TextInput>>,
    focus: FocusHandle,
    cancel: FocusHandle,
    visit: FocusHandle,
    trust: FocusHandle,
    trust_selected: bool,
    trust_pending: Option<AnyWindowHandle>,
    trust_error: Option<SharedString>,
    previous_focus: Option<FocusHandle>,
    keys: ConfirmationKeys,
    policy: InstanceLinkPolicy,
}
impl MessageLinks {
    pub fn new(cx: &mut Context<Fluxer>) -> Self {
        Self {
            destination: None,
            text: None,
            focus: cx.focus_handle(),
            cancel: cx.focus_handle().tab_stop(true),
            visit: cx.focus_handle().tab_stop(true),
            trust: cx.focus_handle().tab_stop(true),
            trust_selected: false,
            trust_pending: None,
            trust_error: None,
            previous_focus: None,
            keys: ConfirmationKeys::default(),
            policy: InstanceLinkPolicy::default(),
        }
    }
    pub fn is_open(&self) -> bool {
        self.destination.is_some()
    }
    pub fn input_context_menu_open(&self, cx: &App) -> bool {
        self.text
            .as_ref()
            .is_some_and(|text| text.read(cx).context_menu_open(cx))
    }
    pub fn reset_input_context_menu(&mut self, cx: &mut App) {
        if let Some(text) = &self.text {
            text.update(cx, |input, cx| input.reset_context_menu(cx));
        }
        self.keys.clear();
    }
    pub fn clear_prompt(&mut self) {
        self.destination = None;
        self.text = None;
        self.previous_focus = None;
        self.trust_selected = false;
        self.trust_pending = None;
        self.trust_error = None;
        self.keys.clear();
    }
    pub fn reset(&mut self) {
        self.clear_prompt();
        self.policy = InstanceLinkPolicy::default();
    }
    pub fn configure_instance(&mut self, document: &serde_json::Value) {
        self.policy = instance_policy(document);
    }
    pub fn channel_link(&self, guild: Option<Snowflake>, channel: Snowflake) -> Option<String> {
        channel_url(self.policy.web_app.as_ref()?, guild, channel)
    }
    pub fn message_link(
        &self,
        guild: Option<Snowflake>,
        channel: Snowflake,
        message: Snowflake,
    ) -> Option<String> {
        message_url(self.policy.web_app.as_ref()?, guild, channel, message)
    }
}

fn channel_url(webapp: &Url, guild: Option<Snowflake>, channel: Snowflake) -> Option<String> {
    let base = safe_url(webapp.as_str())?;
    let scope = guild
        .map(|id| id.to_string())
        .unwrap_or_else(|| "@me".into());
    base.join(&format!("/channels/{scope}/{channel}"))
        .ok()
        .map(Into::into)
}

fn message_url(
    webapp: &Url,
    guild: Option<Snowflake>,
    channel: Snowflake,
    message: Snowflake,
) -> Option<String> {
    if channel == Snowflake(0) || message == Snowflake(0) {
        return None;
    }
    Some(format!(
        "{}/{}",
        channel_url(webapp, guild, channel)?,
        message
    ))
}

#[derive(Default)]
struct InstanceLinkPolicy {
    web_app: Option<Url>,
    marketing: Option<Url>,
    self_hosted: Option<bool>,
    trusted: Vec<String>,
    product_name: Option<String>,
}
fn instance_policy(document: &serde_json::Value) -> InstanceLinkPolicy {
    let endpoint = |name: &str| {
        document
            .get("endpoints")
            .and_then(|value| value.get(name))
            .and_then(|value| value.as_str())
            .and_then(safe_url)
    };
    let mut trusted = [
        "webapp",
        "marketing",
        "api_public",
        "invite",
        "gift",
        "media",
        "static_cdn",
    ]
    .into_iter()
    .filter_map(endpoint)
    .filter_map(|url| url.host_str().map(str::to_owned))
    .collect::<Vec<_>>();
    trusted.extend(
        document
            .pointer("/app_public/branding/trusted_domains")
            .and_then(|value| value.as_array())
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str())
            .filter(|pattern| *pattern != "*")
            .map(str::to_owned),
    );
    InstanceLinkPolicy {
        web_app: endpoint("webapp"),
        marketing: endpoint("marketing"),
        self_hosted: document
            .pointer("/features/self_hosted")
            .and_then(|value| value.as_bool()),
        trusted,
        product_name: document
            .pointer("/app_public/branding/product_name")
            .and_then(|value| value.as_str())
            .map(str::to_owned),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InternalRoute {
    scope: Option<Snowflake>,
    channel: Snowflake,
    message: Option<Snowflake>,
}

impl Fluxer {
    fn can_trust_message_link(&self) -> bool {
        self.state.current_user.is_some()
            && self
                .state
                .user_settings
                .get("trusted_domains")
                .and_then(|value| value.as_array())
                .is_some_and(|domains| {
                    domains.len() < 1000 && domains.iter().all(|d| d.is_string())
                })
    }

    pub(super) fn dismiss_message_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.message_links.reset_input_context_menu(cx);
        if let Some(previous) = self.message_links.previous_focus.take() {
            previous.focus(window);
        }
        self.message_links.clear_prompt();
        cx.notify();
    }

    pub(super) fn apply_message_link_trust_status(
        &mut self,
        status: &fluxer_client::RelationshipMutationStatus,
        cx: &mut Context<Self>,
    ) {
        use fluxer_client::RelationshipMutationStatus;
        if matches!(status, RelationshipMutationStatus::Pending) {
            return;
        }
        let Some(handle) = self.message_links.trust_pending.take() else {
            return;
        };
        if let RelationshipMutationStatus::Failed(error) = status {
            self.message_links.trust_error = Some(error.clone().into());
            return;
        }
        let saved_host = self
            .message_links
            .destination
            .as_ref()
            .and_then(Url::host_str)
            .is_some_and(|host| {
                self.state
                    .user_settings
                    .get("trusted_domains")
                    .and_then(|value| value.as_array())
                    .is_some_and(|domains| {
                        domains.iter().any(|domain| {
                            domain
                                .as_str()
                                .is_some_and(|value| value == "*" || value == host)
                        })
                    })
            });
        if !saved_host {
            self.message_links.trust_error = Some("The domain was not saved.".into());
            return;
        }
        let url = self.message_links.destination.take();
        let previous = self.message_links.previous_focus.take();
        self.message_links.clear_prompt();
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, cx| {
                if let Some(previous) = previous {
                    previous.focus(window);
                }
                if let Some(url) = url {
                    cx.open_url(url.as_str());
                }
            });
        });
    }

    fn visit_message_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.message_links.trust_pending.is_some() {
            return;
        }
        if let Some(url) = self
            .message_links
            .destination
            .as_ref()
            .and_then(|url| safe_url(url.as_str()))
        {
            if self.message_links.trust_selected {
                if !self.can_trust_message_link() || self.interface.server_pending {
                    self.message_links.trust_error =
                        Some("Wait for account settings to finish updating.".into());
                    cx.notify();
                    return;
                }
                let Some(controller) = &self.controller else {
                    self.message_links.trust_error =
                        Some("The native client worker is unavailable.".into());
                    cx.notify();
                    return;
                };
                let setting = fluxer_core::DisplaySetting::AddTrustedDomain(
                    url.host_str().unwrap_or_default().to_owned(),
                );
                match controller.send(Command::UpdateDisplaySetting(setting)) {
                    Ok(()) => {
                        self.message_links.trust_pending = Some(window.window_handle());
                        self.message_links.trust_error = None;
                        self.interface.server_pending = true;
                        self.interface.server_error = None;
                    }
                    Err(error) => self.message_links.trust_error = Some(error.into()),
                }
                cx.notify();
                return;
            }
            cx.open_url(url.as_str());
        }
        self.dismiss_message_link(window, cx);
    }
    pub(super) fn activate_message_target(
        &mut self,
        target: &MessageTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate_message_target_at(target, None, window, cx);
    }
    pub(super) fn activate_message_target_at(
        &mut self,
        target: &MessageTarget,
        anchor: Option<Bounds<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let trigger_bounds = anchor;
        let anchor = anchor
            .map(|bounds| point(bounds.right(), bounds.top()))
            .unwrap_or_else(|| window.mouse_position());
        match target {
            MessageTarget::Author { channel, message } => {
                if let Some(message) = self
                    .state
                    .messages
                    .get(channel)
                    .and_then(|messages| messages.get(message))
                    .cloned()
                {
                    let guild = self
                        .state
                        .channels
                        .get(channel)
                        .and_then(|channel| channel.guild_id);
                    let webhook = crate::profile_triggers::message_is_webhook(&message);
                    self.open_user_profile_at(
                        message.author,
                        guild,
                        webhook,
                        trigger_bounds.unwrap_or_else(|| Bounds::new(anchor, size(px(0.), px(0.)))),
                        window,
                        cx,
                    );
                }
            }
            MessageTarget::User(id) => {
                let user = self.profile_user(*id, &id.to_string());
                let guild = self
                    .state
                    .selected_channel
                    .and_then(|id| self.state.channels.get(&id))
                    .and_then(|channel| channel.guild_id);
                self.open_user_profile_at(
                    user,
                    guild,
                    false,
                    trigger_bounds.unwrap_or_else(|| Bounds::new(anchor, size(px(0.), px(0.)))),
                    window,
                    cx,
                );
            }
            MessageTarget::Role(_) => {}
            MessageTarget::Channel(channel) => {
                self.follow_message_channel(*channel, None, None, cx)
            }
            MessageTarget::Link(raw) => {
                let Some(url) = safe_url(raw) else {
                    self.error = Some("This link is not a valid HTTP or HTTPS address.".into());
                    cx.notify();
                    return;
                };
                let instance = safe_url(&self.instance.read(cx).content);
                if let Some(route) = internal_route(
                    &url,
                    self.message_links.policy.marketing.as_ref(),
                    self.message_links.policy.web_app.as_ref(),
                    self.message_links.policy.self_hosted,
                ) {
                    self.follow_message_channel(
                        route.channel,
                        Some(route.scope),
                        route.message,
                        cx,
                    );
                    return;
                }
                let media = self.media_endpoint.as_deref().and_then(safe_url);
                let patterns = self
                    .state
                    .user_settings
                    .get("trusted_domains")
                    .and_then(|value| value.as_array())
                    .into_iter()
                    .flatten()
                    .filter_map(|value| value.as_str())
                    .chain(self.message_links.policy.trusted.iter().map(String::as_str));
                if trusted_host(
                    url.host_str().unwrap_or_default(),
                    patterns,
                    instance.as_ref(),
                    media.as_ref(),
                    self.message_links.policy.web_app.as_ref(),
                ) {
                    cx.open_url(url.as_str());
                    return;
                }
                self.message_links.clear_prompt();
                self.message_links.previous_focus = window.focused(cx);
                self.message_links.text = Some(cx.new(|cx| TextInput::read_only(url.as_str(), cx)));
                self.message_links.destination = Some(url);
                self.message_links.keys.clear();
                // ExternalLinkWarningModal initialFocusRef is the Visit site button.
                self.message_links.visit.focus(window);
                cx.notify();
            }
        }
    }
    pub(super) fn follow_message_channel(
        &mut self,
        channel: Snowflake,
        scope: Option<Option<Snowflake>>,
        message: Option<Snowflake>,
        cx: &mut Context<Self>,
    ) {
        if !can_follow_channel(&self.state, channel, scope) {
            self.error = Some("You do not have access to the linked channel.".into());
            cx.notify();
            return;
        }
        self.selected_guild = self
            .state
            .channels
            .get(&channel)
            .and_then(|channel| channel.guild_id);
        if let Some(id) = message {
            // Navigation only consumes IDs; this is not a fabricated message
            // inserted into the cache or acknowledged before it is displayed.
            let target = Message {
                id,
                channel_id: channel,
                ..Default::default()
            };
            self.jump_message(&target, cx);
        } else {
            self.select(channel, cx);
        }
        cx.notify();
    }
    pub(super) fn message_link_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(url) = self.message_links.destination.as_ref() else {
            return div().into_any_element();
        };
        div()
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000080))
            .child(
                div()
                    .id("external-message-link-warning")
                    .track_focus(&self.message_links.focus)
                    .w(px(460.))
                    .max_w(window.viewport_size().width - px(32.))
                    .p_5()
                    .rounded_lg()
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgb(0x4c4458))
                    .shadow_lg()
                    .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        // The URL's menu is nested in this warning's focus path.
                        // Let it handle navigation and activation before the dialog.
                        if this.message_links.input_context_menu_open(cx) {
                            this.message_links.keys.clear();
                            return;
                        }
                        match event.keystroke.key.as_str() {
                            "escape" => this.dismiss_message_link(window, cx),
                            "tab" => {
                                this.message_links.keys.clear();
                                let mut handles = vec![];
                                if let Some(text) = &this.message_links.text {
                                    handles.push(text.focus_handle(cx));
                                }
                                if this.can_trust_message_link() {
                                    handles.push(this.message_links.trust.clone());
                                }
                                handles.extend([
                                    this.message_links.cancel.clone(),
                                    this.message_links.visit.clone(),
                                ]);
                                crate::overlay::cycle_focus(
                                    window,
                                    &handles,
                                    event.keystroke.modifiers.shift,
                                );
                            }
                            "enter" | "space" if !event.keystroke.modifiers.modified() => {
                                let choice = if this.message_links.trust.is_focused(window) {
                                    if !event.is_held && this.message_links.trust_pending.is_none()
                                    {
                                        this.message_links.trust_selected =
                                            !this.message_links.trust_selected;
                                        this.message_links.trust_error = None;
                                        cx.notify();
                                    }
                                    None
                                } else if this.message_links.visit.is_focused(window) {
                                    Some(ConfirmationChoice::Confirm)
                                } else if this.message_links.cancel.is_focused(window) {
                                    Some(ConfirmationChoice::Cancel)
                                } else {
                                    None
                                };
                                if let Some(choice) = choice {
                                    this.message_links.keys.press(choice, event.is_held);
                                } else {
                                    this.message_links.keys.clear();
                                }
                            }
                            _ => return,
                        }
                        cx.stop_propagation();
                    }))
                    .capture_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| {
                        if this.message_links.input_context_menu_open(cx) {
                            this.message_links.keys.clear();
                            return;
                        }
                        if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                            match this.message_links.keys.release() {
                                Some(ConfirmationChoice::Cancel) => {
                                    this.dismiss_message_link(window, cx)
                                }
                                Some(ConfirmationChoice::Confirm) => {
                                    this.visit_message_link(window, cx)
                                }
                                None => {}
                            }
                            cx.stop_propagation();
                        }
                    }))
                    .child(heading("External link warning"))
                    .child(div().mt_3().text_sm().child(format!(
                            "You are about to leave {}.",
                            self.message_links
                                .policy
                                .product_name
                                .as_deref()
                                .unwrap_or("Fluxer")
                        )))
                    .child(
                        div()
                            .mt_1()
                            .text_sm()
                            .text_color(rgb(MUTED))
                            .child("External links can be dangerous. Be careful."),
                    )
                    .child(
                        div()
                            .mt_4()
                            .text_size(px(11.))
                            .text_color(rgb(MUTED))
                            .child(format!(
                                "Destination · {}",
                                url.host_str().unwrap_or_default()
                            )),
                    )
                    .when_some(self.message_links.text.as_ref(), |view, text| {
                        view.child(
                            div()
                                .id("external-message-link-url")
                                .mt_2()
                                .p_3()
                                .rounded_md()
                                .bg(rgb(BG))
                                .max_h(px(160.))
                                .overflow_y_scroll()
                                .w_full()
                                .min_w_0()
                                .child(text.clone()),
                        )
                    })
                    .when(self.can_trust_message_link(), |view| {
                        let checked = self.message_links.trust_selected;
                        view.child(
                            div().mt_2().child(
                                quiet_button(
                                    "trust-external-message-link",
                                    format!(
                                        "{} Always trust {}",
                                        if checked { "☑" } else { "☐" },
                                        url.host_str().unwrap_or_default(),
                                    ),
                                )
                                .track_focus(&self.message_links.trust)
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        if this.message_links.trust_pending.is_none() {
                                            this.message_links.trust_selected =
                                                !this.message_links.trust_selected;
                                            this.message_links.trust_error = None;
                                            cx.notify();
                                        }
                                    },
                                )),
                            ),
                        )
                    })
                    .when_some(self.message_links.trust_error.clone(), |view, error| {
                        view.child(div().mt_2().text_sm().text_color(rgb(MUTED)).child(error))
                    })
                    .child(
                        div()
                            .mt_5()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                quiet_button("cancel-external-message-link", "Cancel")
                                    .track_focus(&self.message_links.cancel)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.dismiss_message_link(window, cx)
                                    })),
                            )
                            .child(
                                quiet_button(
                                    "visit-external-message-link",
                                    if self.message_links.trust_pending.is_some() {
                                        "Saving…"
                                    } else {
                                        "Visit site"
                                    },
                                )
                                .track_focus(&self.message_links.visit)
                                .bg(rgb(0x6f56d7))
                                .text_color(rgb(TEXT))
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.visit_message_link(window, cx),
                                )),
                            ),
                    ),
            )
            .into_any_element()
    }
}

pub(super) fn can_follow_channel(
    state: &ClientState,
    channel: Snowflake,
    scope: Option<Option<Snowflake>>,
) -> bool {
    state
        .channels
        .get(&channel)
        .zip(state.current_user.as_ref())
        .is_some_and(|(channel, user)| {
            channel.kind != ChannelType::Category
                && scope.is_none_or(|scope| {
                    scope_matches(scope, channel.guild_id, channel.kind.is_private())
                })
                && state
                    .permissions_for(channel.id, user.id)
                    .contains(Permissions::VIEW_CHANNEL)
        })
}

fn safe_url(raw: &str) -> Option<Url> {
    let raw = raw.trim();
    if raw.chars().any(|character| character.is_control()) {
        return None;
    }
    let url = Url::parse(raw).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none())
    .then_some(url)
}
fn comparable(host: &str) -> String {
    let normalized = host.trim().to_lowercase();
    normalized
        .strip_suffix('.')
        .unwrap_or(&normalized)
        .to_owned()
}
fn covers_host(pattern: &str, host: &str) -> bool {
    let pattern = comparable(pattern);
    let host = comparable(host);
    if pattern == "*" {
        return true;
    }
    if let Some(base) = pattern.strip_prefix("*.") {
        host == base || host.ends_with(&format!(".{base}"))
    } else {
        host == pattern
    }
}
fn trusted_host<'a>(
    host: &str,
    custom: impl Iterator<Item = &'a str>,
    instance: Option<&Url>,
    media: Option<&Url>,
    web_app: Option<&Url>,
) -> bool {
    BUILTIN_TRUST
        .iter()
        .copied()
        .chain(custom)
        .any(|pattern| covers_host(pattern, host))
        || [instance, media, web_app].into_iter().flatten().any(|url| {
            url.host_str()
                .is_some_and(|configured| comparable(configured) == comparable(host))
        })
}
fn authority(url: &Url) -> String {
    format!(
        "{}{}",
        url.host_str().unwrap_or_default(),
        url.port()
            .map(|port| format!(":{port}"))
            .unwrap_or_default()
    )
}
fn internal_route(
    url: &Url,
    instance: Option<&Url>,
    web_app: Option<&Url>,
    self_hosted: Option<bool>,
) -> Option<InternalRoute> {
    let host = authority(url);
    let configured = [instance, web_app]
        .into_iter()
        .flatten()
        .any(|configured| authority(configured).eq_ignore_ascii_case(&host));
    if !configured
        && !(self_hosted == Some(false) && OFFICIAL_INTERNAL_HOSTS.contains(&host.as_str()))
    {
        return None;
    }
    let segments = url
        .path()
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if !(segments.len() == 3 || segments.len() == 4) || segments[0] != "channels" {
        return None;
    }
    let scope = if segments[1] == "@me" {
        None
    } else {
        Some(positive_id(segments[1])?)
    };
    let channel = positive_id(segments[2])?;
    let message = match segments.get(3) {
        Some(id) => Some(positive_id(id)?),
        None => None,
    };
    Some(InternalRoute {
        scope,
        channel,
        message,
    })
}
// Keep route IDs in the native Snowflake range; no unknown-ID REST discovery.
fn positive_id(raw: &str) -> Option<Snowflake> {
    raw.parse::<u64>().ok().filter(|id| *id > 0).map(Snowflake)
}
fn scope_matches(scope: Option<Snowflake>, guild: Option<Snowflake>, private: bool) -> bool {
    match scope {
        Some(scope) => guild == Some(scope) && !private,
        None => guild.is_none() && private,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        can_follow_channel, covers_host, instance_policy, internal_route, safe_url, scope_matches,
        trusted_host,
    };
    use fluxer_core::Snowflake;
    #[test]
    fn copied_channel_links_use_discovered_frontend_and_exact_scope() {
        let origin = reqwest::Url::parse("https://chat.example").unwrap();
        assert_eq!(
            super::channel_url(
                &origin,
                Some(fluxer_core::Snowflake(1)),
                fluxer_core::Snowflake(11)
            )
            .as_deref(),
            Some("https://chat.example/channels/1/11")
        );
        assert_eq!(
            super::channel_url(&origin, None, fluxer_core::Snowflake(22)).as_deref(),
            Some("https://chat.example/channels/@me/22")
        );
        assert!(
            super::channel_url(
                &reqwest::Url::parse("https://user:secret@chat.example").unwrap(),
                None,
                fluxer_core::Snowflake(22)
            )
            .is_none()
        );
    }
    #[test]
    fn copied_message_links_roundtrip_guild_dm_voice_and_self_hosted_routes() {
        let webapp = reqwest::Url::parse("https://self-hosted.example/app?ignored=true").unwrap();
        for guild in [Some(Snowflake(1)), None] {
            let link =
                super::message_url(&webapp, guild, Snowflake(14), Snowflake(987654321)).unwrap();
            let scope = guild
                .map(|id| id.to_string())
                .unwrap_or_else(|| "@me".into());
            assert_eq!(
                link,
                format!("https://self-hosted.example/channels/{scope}/14/987654321")
            );
            let parsed = reqwest::Url::parse(&link).unwrap();
            let route = internal_route(&parsed, None, Some(&webapp), Some(true)).unwrap();
            assert_eq!(route.scope, guild);
            assert_eq!(route.channel, Snowflake(14));
            assert_eq!(route.message, Some(Snowflake(987654321)));
        }
        assert!(super::message_url(&webapp, None, Snowflake(14), Snowflake(0)).is_none());
        assert!(
            super::message_url(
                &reqwest::Url::parse("https://user:secret@host").unwrap(),
                None,
                Snowflake(14),
                Snowflake(99)
            )
            .is_none()
        );
    }
    #[test]
    fn channel_link_actions_require_live_access_without_requiring_cached_history() {
        let mut state = fluxer_core::ClientState::demo();
        state.messages.clear();
        assert!(can_follow_channel(&state, 11.into(), None));
        assert!(can_follow_channel(&state, 40.into(), None));
        assert!(
            !can_follow_channel(&state, 10.into(), None),
            "categories are not message destinations"
        );
        assert!(!can_follow_channel(&state, 999.into(), None));
        let user = state.current_user.as_ref().unwrap().id;
        state.guilds.get_mut(&Snowflake(1)).unwrap().roles[0].permissions =
            fluxer_core::Permissions::NONE;
        assert!(
            !can_follow_channel(&state, 11.into(), None),
            "cached announcements cannot bypass revoked view permission"
        );
        state.guilds.get_mut(&Snowflake(1)).unwrap().owner_id = user;
        assert!(can_follow_channel(&state, 11.into(), None));
        state.current_user = None;
        assert!(!can_follow_channel(&state, 11.into(), None));
    }
    #[test]
    fn source_builtin_patterns_do_not_trust_lookalikes_or_extra_gg_subdomains() {
        for host in [
            "fluxer.app",
            "canary.fluxer.app",
            "a.b.fluxer.dev",
            "FLUXERSTATUS.COM.",
            "fluxer.gg",
            "fluxer.gift",
        ] {
            assert!(
                trusted_host(host, std::iter::empty(), None, None, None),
                "{host}"
            );
        }
        for host in [
            "evilfluxer.app",
            "fluxer.app.evil.test",
            "sub.fluxer.gg",
            "sub.fluxer.gift",
            "evil.test",
        ] {
            assert!(
                !trusted_host(host, std::iter::empty(), None, None, None),
                "{host}"
            );
        }
    }
    #[test]
    fn custom_and_configured_trust_are_exact_unless_explicitly_wildcarded() {
        assert!(covers_host(" *.Example.test. ", "example.test"));
        assert!(covers_host("*.example.test", "nested.example.test"));
        assert!(!covers_host("*.example.test", "notexample.test"));
        assert!(
            !covers_host("example.test", "example.test.."),
            "source removes one terminal dot, not an arbitrary suffix"
        );
        assert!(covers_host("*", "anything.test"));
        let instance = safe_url("https://chat.example:8443/").unwrap();
        assert!(trusted_host(
            "CHAT.EXAMPLE",
            std::iter::empty(),
            Some(&instance),
            None,
            None
        ));
        assert!(!trusted_host(
            "other.chat.example",
            std::iter::empty(),
            Some(&instance),
            None,
            None
        ));
    }
    #[test]
    fn unsafe_protocols_credentials_and_controls_never_open() {
        for raw in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,test",
            "https://user@example.test/",
            "https://:secret@example.test/",
            "https://exa\nmple.test/",
            "/channels/1/2",
        ] {
            assert!(safe_url(raw).is_none(), "{raw}");
        }
        assert!(safe_url(" HTTPS://example.test/a?q=1#section ").is_some());
    }
    #[test]
    fn internal_host_aliases_require_authoritative_official_mode_and_ports_match() {
        let instance = safe_url("https://self.example:8443").unwrap();
        let local = safe_url("https://self.example:8443/channels/10/20/30?ignored=1").unwrap();
        assert_eq!(
            internal_route(&local, Some(&instance), None, Some(true))
                .unwrap()
                .message,
            Some(Snowflake(30))
        );
        assert!(
            internal_route(
                &safe_url("https://self.example/channels/10/20").unwrap(),
                Some(&instance),
                None,
                Some(true)
            )
            .is_none()
        );
        let official = safe_url("https://web.fluxer.app/channels/@me/20/30").unwrap();
        assert!(internal_route(&official, Some(&instance), None, None).is_none());
        assert!(internal_route(&official, Some(&instance), None, Some(true)).is_none());
        assert!(internal_route(&official, Some(&instance), None, Some(false)).is_some());
    }
    #[test]
    fn route_shape_ids_and_channel_scope_are_guarded() {
        let base = safe_url("https://chat.example").unwrap();
        for path in [
            "/channels/10/20/30/40",
            "/channels/@me",
            "/channels/x/20",
            "/channels/0/20",
            "/channels/10/20/0",
        ] {
            assert!(
                internal_route(&base.join(path).unwrap(), Some(&base), None, None).is_none(),
                "{path}"
            );
        }
        assert!(scope_matches(
            Some(Snowflake(10)),
            Some(Snowflake(10)),
            false
        ));
        assert!(!scope_matches(
            Some(Snowflake(11)),
            Some(Snowflake(10)),
            false
        ));
        assert!(!scope_matches(None, Some(Snowflake(10)), false));
        assert!(scope_matches(None, None, true));
        assert!(!scope_matches(Some(Snowflake(10)), None, true));
    }
    #[test]
    fn discovery_policy_uses_actual_features_and_declared_hosts_without_branding_trust_all() {
        let policy = instance_policy(
            &serde_json::json!({"features":{"self_hosted":true},"endpoints":{"webapp":"https://frontend.example:8443","marketing":"https://marketing.example","static_cdn":"https://cdn.example/assets","api_client":"https://not-trusted.example/api"},"app_public":{"branding":{"product_name":"Example Chat","trusted_domains":["*","*.partners.example"]}}}),
        );
        assert_eq!(policy.self_hosted, Some(true));
        assert_eq!(policy.product_name.as_deref(), Some("Example Chat"));
        assert!(policy.trusted.contains(&"cdn.example".into()));
        assert!(policy.trusted.contains(&"*.partners.example".into()));
        assert!(!policy.trusted.contains(&"*".into()));
        assert!(!policy.trusted.contains(&"not-trusted.example".into()));
        let url = safe_url("https://frontend.example:8443/channels/10/20").unwrap();
        assert!(
            internal_route(
                &url,
                policy.marketing.as_ref(),
                policy.web_app.as_ref(),
                policy.self_hosted
            )
            .is_some()
        );
    }
}
