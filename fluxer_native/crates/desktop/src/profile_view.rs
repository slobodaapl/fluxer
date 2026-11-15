// SPDX-License-Identifier: AGPL-3.0-or-later
//! Compact profile surface. Display precedence follows features/user/models/Profile.ts;
//! geometry follows UserProfileSurfaceGeometry.ts. Remote bio remains inert text.
use fluxer_client::{ProfileSnapshot, ProfileStatus};
use fluxer_core::{Member, ProfileDetails, Role, Snowflake, User};
use gpui::{prelude::*, *};

pub(super) struct ProfileOpen {
    pub user: User,
    pub guild_id: Option<Snowflake>,
    pub member: Option<Member>,
    pub roles: Vec<Role>,
    pub media_endpoint: Option<String>,
    pub community_name: Option<String>,
    pub anchor: Point<Pixels>,
    pub webhook: bool,
    pub can_message: bool,
    pub blocked: bool,
}

pub(super) struct ProfileUi {
    pub popup: Entity<ProfilePopup>,
}
impl ProfileUi {
    pub fn new(cx: &mut Context<super::Fluxer>) -> Self {
        Self {
            popup: cx.new(ProfilePopup::new),
        }
    }
    pub fn reset(&mut self, cx: &mut Context<super::Fluxer>) {
        self.popup.update(cx, |popup, cx| {
            popup.open = None;
            popup.snapshot = None;
            popup.previous_focus = None;
            popup.armed = false;
            popup.copied = false;
            cx.notify();
        });
    }
    pub fn is_open(&self, cx: &App) -> bool {
        self.popup.read(cx).is_open()
    }
}

#[derive(Clone, Debug)]
pub(super) enum ProfileAction {
    Retry {
        user_id: Snowflake,
        guild_id: Option<Snowflake>,
    },
    Message {
        user_id: Snowflake,
    },
    Closed,
}
pub(super) struct ProfilePopup {
    open: Option<ProfileOpen>,
    snapshot: Option<ProfileSnapshot>,
    focus: FocusHandle,
    previous_focus: Option<FocusHandle>,
    scroll: ScrollHandle,
    selected: usize,
    armed: bool,
    copied: bool,
    confirm_blocked_dm: bool,
}
impl EventEmitter<ProfileAction> for ProfilePopup {}
impl ProfilePopup {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            open: None,
            snapshot: None,
            focus: cx.focus_handle(),
            previous_focus: None,
            scroll: ScrollHandle::new(),
            selected: 0,
            armed: false,
            copied: false,
            confirm_blocked_dm: false,
        }
    }
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }
    pub fn close_after_action(&mut self, cx: &mut Context<Self>) {
        self.open = None;
        self.snapshot = None;
        self.previous_focus = None;
        cx.emit(ProfileAction::Closed);
        cx.notify();
    }
    pub fn current_scope(&self) -> Option<(Snowflake, Option<Snowflake>)> {
        self.open.as_ref().map(|open| (open.user.id, open.guild_id))
    }
    pub fn open(&mut self, config: ProfileOpen, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_open() {
            self.previous_focus = window.focused(cx);
        }
        self.open = Some(config);
        self.snapshot = None;
        self.scroll = ScrollHandle::new();
        self.selected = 0;
        self.armed = false;
        self.copied = false;
        self.confirm_blocked_dm = false;
        window.focus(&self.focus);
        cx.notify();
    }
    pub fn apply_snapshot(&mut self, snapshot: ProfileSnapshot, cx: &mut Context<Self>) {
        if self.open.as_ref().is_some_and(|open| {
            !open.webhook && open.user.id == snapshot.user_id && open.guild_id == snapshot.guild_id
        }) {
            if !matches!(snapshot.status, ProfileStatus::Failed(_)) {
                self.selected = 0;
            }
            self.armed = false;
            self.snapshot = Some(snapshot);
            cx.notify();
        }
    }
    pub fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open.take().is_none() {
            return;
        }
        self.snapshot = None;
        if let Some(focus) = self.previous_focus.take() {
            window.focus(&focus);
        }
        cx.emit(ProfileAction::Closed);
        cx.notify();
    }
    fn failed(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| matches!(snapshot.status, ProfileStatus::Failed(_)))
    }
    fn activate(&mut self, retry: bool, cx: &mut Context<Self>) {
        let Some(open) = &self.open else {
            return;
        };
        if retry {
            cx.emit(ProfileAction::Retry {
                user_id: open.user.id,
                guild_id: open.guild_id,
            });
        } else {
            cx.write_to_clipboard(ClipboardItem::new_string(open.user.id.to_string()));
            self.copied = true;
            cx.notify();
        }
    }
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => self.dismiss(window, cx),
            "tab" => {
                self.selected = if self.failed() {
                    1 - self.selected.min(1)
                } else {
                    0
                };
                self.armed = false;
                cx.notify();
            }
            "enter" | "space" => {
                if !event.is_held {
                    self.armed = true;
                }
            }
            "up" => {
                self.scroll
                    .set_offset(point(px(0.), self.scroll.offset().y + px(32.)));
                cx.notify();
            }
            "down" => {
                self.scroll
                    .set_offset(point(px(0.), self.scroll.offset().y - px(32.)));
                cx.notify();
            }
            _ => {}
        }
        cx.stop_propagation();
    }
    fn key_up(&mut self, event: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if matches!(event.keystroke.key.as_str(), "enter" | "space") && self.armed {
            self.armed = false;
            self.activate(self.selected == 1 && self.failed(), cx);
        }
        cx.stop_propagation();
    }
}

fn label(text: &str, size: f32, cx: &mut App) -> AnyElement {
    div()
        .flex()
        .flex_wrap()
        .min_w_0()
        .max_w_full()
        .children(
            text.split_inclusive(char::is_whitespace)
                .map(|word| crate::emoji_sprites::unicode_text(word, size, false, cx)),
        )
        .into_any_element()
}
// AccentColorUtils.ts: effective profile accent, sampled avatar color,
// source default-avatar palette, then the application's default accent.
fn profile_accent(
    user: &User,
    global: Option<&ProfileDetails>,
    member: Option<&ProfileDetails>,
) -> u32 {
    let explicit = member
        .and_then(|p| p.accent_color)
        .or_else(|| global.and_then(|p| p.accent_color));
    if let Some(color) = explicit.or_else(|| {
        user.extra
            .get("avatar_color")
            .and_then(serde_json::Value::as_i64)
    }) {
        return color as u32 & 0xffffff;
    }
    if user.avatar.is_none() {
        return [0x4641d9, 0xf0b100, 0x00bba7, 0x2b7fff, 0xad46ff, 0x6a7282]
            [(user.id.get() % 6) as usize];
    }
    0x4641d9
}
fn text_override<'a>(global: Option<&'a str>, member: Option<&'a str>) -> &'a str {
    member.or(global).unwrap_or("")
}
fn placement(anchor: Point<Pixels>, viewport: Size<Pixels>) -> (Point<Pixels>, f32, f32) {
    let width = 300_f32.min((f32::from(viewport.width) - 16.).max(1.));
    let height = 520_f32.min((f32::from(viewport.height) - 16.).max(1.));
    let right = f32::from(anchor.x) + 12.;
    let x = if right + width + 8. <= f32::from(viewport.width) {
        right
    } else {
        f32::from(anchor.x) - width - 12.
    };
    let y = f32::from(anchor.y);
    (
        point(
            px(x.clamp(8., (f32::from(viewport.width) - width - 8.).max(8.))),
            px(y.clamp(8., (f32::from(viewport.height) - height - 8.).max(8.))),
        ),
        width,
        height,
    )
}

impl Render for ProfilePopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(open) = &self.open else {
            return div().into_any_element();
        };
        let profile = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.profile.as_ref());
        let user = profile.map(|profile| &profile.user).unwrap_or(&open.user);
        // A completed response's membership is authoritative, even when absent.
        let ready = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| matches!(snapshot.status, ProfileStatus::Ready));
        let member = if ready || profile.is_some() {
            profile.and_then(|profile| profile.guild_member.as_ref())
        } else {
            open.member.as_ref()
        };
        let global = profile.map(|profile| &profile.user_profile);
        let guild_profile = if member.is_some() {
            profile.and_then(|profile| profile.guild_member_profile.as_ref())
        } else {
            None
        };
        let name = member
            .and_then(|member| member.nick.as_deref())
            .unwrap_or(user.display_name());
        let pronouns = text_override(
            global.and_then(|p| p.pronouns.as_deref()),
            guild_profile.and_then(|p| p.pronouns.as_deref()),
        );
        let bio = text_override(
            global.and_then(|p| p.bio.as_deref()),
            guild_profile.and_then(|p| p.bio.as_deref()),
        );
        let flags = member.and_then(|member| member.profile_flags).unwrap_or(0);
        let banner = if flags & 2 != 0 {
            None
        } else {
            guild_profile
                .and_then(|p| p.banner.as_deref())
                .or_else(|| global.and_then(|p| p.banner.as_deref()))
        };
        let accent_color = profile_accent(user, global, guild_profile);
        let banner_url = banner.and_then(|hash| {
            crate::avatar_images::profile_banner_url(
                user.id,
                open.guild_id
                    .filter(|_| guild_profile.and_then(|p| p.banner.as_ref()).is_some()),
                hash,
                open.media_endpoint.as_deref(),
            )
        });
        let avatar = crate::avatar_images::profile_avatar(
            user,
            member,
            open.guild_id,
            open.media_endpoint.as_deref(),
            80.,
        );
        let (origin, width, height) = placement(open.anchor, window.viewport_size());
        let mut content = div()
            .flex()
            .flex_col()
            .gap_2()
            .px_3()
            .pb_3()
            .pt(px(34.))
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::BOLD)
                    .child(label(name, 20., cx)),
            );
        if name != user.display_name() {
            content = content.child(div().text_size(px(14.)).child(label(
                user.display_name(),
                14.,
                cx,
            )));
        }
        content = content.child(
            div()
                .text_size(px(13.))
                .text_color(rgb(0xb5bac1))
                .child(label(&user.username, 13., cx)),
        );
        if !pronouns.is_empty() {
            content = content.child(div().text_size(px(12.)).child(label(pronouns, 12., cx)));
        }
        if user.bot {
            content = content.child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(0xbcb9ff))
                    .child("BOT"),
            );
        }
        if !bio.is_empty() {
            content = content.child(div().mt_2().text_size(px(13.)).child(label(bio, 13., cx)));
        }
        if !open.webhook && user.id.get() != 0 {
            // packages/snowflake/src/Snowflake.ts: Fluxer uses the 2015 epoch.
            let millis = (user.id.get() >> 22) as i64 + 1_420_070_400_000;
            if let Some(created) = chrono::DateTime::from_timestamp_millis(millis) {
                content = content
                    .child(
                        div()
                            .mt_2()
                            .text_size(px(11.))
                            .font_weight(FontWeight::BOLD)
                            .child("FLUXER MEMBER SINCE"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(0xb5bac1))
                            .child(created.format("%b %-d, %Y").to_string()),
                    );
            }
        }
        if let Some(member) = member {
            if let Some(joined) = member
                .extra
                .get("joined_at")
                .and_then(serde_json::Value::as_str)
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            {
                content = content
                    .child(
                        div()
                            .mt_2()
                            .text_size(px(11.))
                            .font_weight(FontWeight::BOLD)
                            .child(
                                open.community_name
                                    .as_ref()
                                    .map(|name| format!("JOINED {name}"))
                                    .unwrap_or_else(|| "COMMUNITY MEMBER SINCE".into()),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(0xb5bac1))
                            .child(joined.format("%b %-d, %Y").to_string()),
                    );
            }
            let mut roles: Vec<_> = open
                .roles
                .iter()
                .filter(|role| member.roles.contains(&role.id))
                .collect();
            roles.sort_by_key(|role| std::cmp::Reverse(role.position));
            if !roles.is_empty() {
                content = content
                    .child(
                        div()
                            .mt_2()
                            .text_size(px(11.))
                            .font_weight(FontWeight::BOLD)
                            .child("ROLES"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_1()
                            .children(roles.into_iter().map(|role| {
                                div()
                                    .rounded_md()
                                    .px_2()
                                    .py_1()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .bg(rgb(0x383a40))
                                    .child(div().size(px(6.)).rounded_full().flex_shrink_0().bg(
                                        rgb(if role.color == 0 {
                                            0x99aab5
                                        } else {
                                            role.color & 0xffffff
                                        }),
                                    ))
                                    .text_size(px(12.))
                                    .child(label(&role.name, 12., cx))
                            })),
                    );
            }
        }
        if open.webhook {
            content = content.child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(0xb5bac1))
                    .child("Webhook identity"),
            );
        } else {
            match self.snapshot.as_ref().map(|snapshot| &snapshot.status) {
                Some(ProfileStatus::Ready) => {
                    if profile.is_some_and(|profile| profile.profile_limited) {
                        content = content.child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(0xb5bac1))
                                .child("This profile has limited information."),
                        );
                    }
                }
                Some(ProfileStatus::Failed(error)) => {
                    content = content
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(0xf2b8b5))
                                .child(format!("Couldn't load profile: {error}")),
                        )
                        .child(
                            div()
                                .id("profile-retry")
                                .cursor_pointer()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .bg(rgb(if self.selected == 1 {
                                    0x535064
                                } else {
                                    0x383a40
                                }))
                                .child("Retry")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.activate(true, cx);
                                })),
                        );
                }
                _ => {
                    content = content.child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(0xb5bac1))
                            .child("Loading profile…"),
                    );
                }
            }
        }
        if open.can_message {
            content = content.child(
                div()
                    .id("profile-message")
                    .cursor_pointer()
                    .mt_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgb(0x5865f2))
                    .text_size(px(12.))
                    .child(if self.confirm_blocked_dm {
                        "This user is blocked. Open DM anyway?"
                    } else if open.blocked {
                        "Open DM"
                    } else {
                        "Message"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        if let Some(open) = &this.open {
                            if open.blocked && !this.confirm_blocked_dm {
                                this.confirm_blocked_dm = true;
                                cx.notify();
                            } else {
                                cx.emit(ProfileAction::Message {
                                    user_id: open.user.id,
                                });
                            }
                        }
                    })),
            );
        }
        content = content.child(
            div()
                .id("profile-copy-id")
                .cursor_pointer()
                .mt_2()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(rgb(if self.selected == 0 {
                    0x535064
                } else {
                    0x383a40
                }))
                .text_size(px(12.))
                .child(if self.copied {
                    "Copied User ID"
                } else {
                    "Copy User ID"
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.activate(false, cx);
                })),
        );
        let header = div()
            .relative()
            .h(px(105.))
            .flex_shrink_0()
            .bg(rgb(accent_color))
            .when_some(banner_url, |view, url| {
                view.child(img(url).w_full().h(px(105.)).object_fit(ObjectFit::Cover))
            })
            .child(
                div()
                    .absolute()
                    .left(px(10.))
                    .top(px(55.))
                    .size(px(88.))
                    .rounded_full()
                    .bg(rgb(0x232428))
                    .p_1()
                    .child(avatar),
            );
        deferred(
            anchored()
                .position_mode(AnchoredPositionMode::Window)
                .position(origin)
                .anchor(Corner::TopLeft)
                .snap_to_window_with_margin(px(8.))
                .child(
                    div()
                        .id("native-profile-popup")
                        .track_focus(&self.focus)
                        .occlude()
                        .w(px(width))
                        .max_h(px(height))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .rounded_lg()
                        .border_1()
                        .border_color(rgb(0x4c4458))
                        .bg(rgb(0x232428))
                        .text_color(rgb(0xf2f3f5))
                        .shadow_lg()
                        .on_mouse_down_out(
                            cx.listener(|this, _, window, cx| this.dismiss(window, cx)),
                        )
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .capture_key_down(cx.listener(Self::key_down))
                        .capture_key_up(cx.listener(Self::key_up))
                        .child(header)
                        .child(content),
                ),
        )
        .with_priority(9)
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{placement, profile_accent, text_override};
    use fluxer_core::{ProfileDetails, User};
    use gpui::{point, px, size};
    #[test]
    fn accent_uses_profile_then_avatar_then_source_defaults_including_black() {
        let mut user = User {
            id: 1.into(),
            ..Default::default()
        };
        assert_eq!(profile_accent(&user, None, None), 0xf0b100);
        user.extra
            .insert("avatar_color".into(), serde_json::json!(0x112233));
        assert_eq!(profile_accent(&user, None, None), 0x112233);
        let global = ProfileDetails {
            accent_color: Some(0x445566),
            ..Default::default()
        };
        let member = ProfileDetails {
            accent_color: Some(0),
            ..Default::default()
        };
        assert_eq!(profile_accent(&user, Some(&global), None), 0x445566);
        assert_eq!(profile_accent(&user, Some(&global), Some(&member)), 0);
    }
    #[test]
    fn member_empty_text_overrides_global() {
        assert_eq!(text_override(Some("global"), Some("")), "");
        assert_eq!(text_override(Some("global"), None), "global");
    }
    #[test]
    fn popup_flips_and_fits_short_windows() {
        let (origin, width, height) =
            placement(point(px(790.), px(590.)), size(px(800.), px(600.)));
        assert!(origin.x + px(width) <= px(792.));
        assert!(origin.y + px(height) <= px(592.));
        let (_, width, height) = placement(point(px(50.), px(50.)), size(px(180.), px(200.)));
        assert_eq!((width, height), (164., 184.));
    }
}
