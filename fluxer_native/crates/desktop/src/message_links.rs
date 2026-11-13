// SPDX-License-Identifier: AGPL-3.0-or-later
//! TrustedDomain.ts, DeepLinkUtils.ts and ExternalLinkWarningModal.tsx policy.
//! Trust is read from authoritative settings; this UI does not invent a trust
//! settings write or an "always trust" checkbox without its transport.
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
            previous_focus: None,
            keys: ConfirmationKeys::default(),
            policy: InstanceLinkPolicy::default(),
        }
    }
    pub fn is_open(&self) -> bool {
        self.destination.is_some()
    }
    pub fn reset(&mut self) {
        self.destination = None;
        self.text = None;
        self.previous_focus = None;
        self.keys.clear();
        self.policy = InstanceLinkPolicy::default();
    }
    pub fn configure_instance(&mut self, document: &serde_json::Value) {
        self.policy = instance_policy(document);
    }
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
    pub(super) fn dismiss_message_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.message_links.destination = None;
        self.message_links.text = None;
        self.message_links.keys.clear();
        if let Some(previous) = self.message_links.previous_focus.take() {
            previous.focus(window);
        }
        cx.notify();
    }
    fn visit_message_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(url) = self
            .message_links
            .destination
            .as_ref()
            .and_then(|url| safe_url(url.as_str()))
        {
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
        match target {
            MessageTarget::User(_) | MessageTarget::Role(_) => {}
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
    fn follow_message_channel(
        &mut self,
        channel: Snowflake,
        scope: Option<Option<Snowflake>>,
        message: Option<Snowflake>,
        cx: &mut Context<Self>,
    ) {
        let allowed = self
            .state
            .channels
            .get(&channel)
            .zip(self.state.current_user.as_ref())
            .is_some_and(|(channel, user)| {
                channel.kind != ChannelType::Category
                    && scope.is_none_or(|scope| {
                        scope_matches(scope, channel.guild_id, channel.kind.is_private())
                    })
                    && self
                        .state
                        .permissions_for(channel.id, user.id)
                        .contains(Permissions::VIEW_CHANNEL)
            });
        if !allowed {
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
                        match event.keystroke.key.as_str() {
                            "escape" => this.dismiss_message_link(window, cx),
                            "tab" => {
                                this.message_links.keys.clear();
                                if this.message_links.visit.is_focused(window) {
                                    this.message_links.cancel.focus(window);
                                } else {
                                    this.message_links.visit.focus(window);
                                }
                            }
                            "enter" | "space" if !event.keystroke.modifiers.modified() => {
                                this.message_links.keys.press(
                                    if this.message_links.visit.is_focused(window) {
                                        ConfirmationChoice::Confirm
                                    } else {
                                        ConfirmationChoice::Cancel
                                    },
                                    event.is_held,
                                );
                            }
                            _ => return,
                        }
                        cx.stop_propagation();
                    }))
                    .capture_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| {
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
                                quiet_button("visit-external-message-link", "Visit site")
                                    .track_focus(&self.message_links.visit)
                                    .bg(rgb(0x6f56d7))
                                    .text_color(rgb(TEXT))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.visit_message_link(window, cx)
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }
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
    host.trim().to_lowercase().trim_end_matches('.').to_string()
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
        covers_host, instance_policy, internal_route, safe_url, scope_matches, trusted_host,
    };
    use fluxer_core::Snowflake;
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
