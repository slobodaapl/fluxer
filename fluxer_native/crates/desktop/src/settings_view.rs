// SPDX-License-Identifier: AGPL-3.0-or-later
//! Source-style settings navigation, separate from ordinary channel navigation.
use super::*;
use crate::controls::{Icon, icon_button, quiet_button};
use fluxer_client::CommerceView;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Section {
    Profile,
    AccountSessions,
    Connections,
    Commerce(CommerceView),
    Appearance,
    Privacy,
    Accessibility,
    MessagesMedia,
    TrustedDomains,
    Keybinds,
    Voice,
    Notifications,
    StreamerMode,
    Desktop,
}

pub(super) struct SessionRevocation {
    target: String,
    title: String,
    account: Snowflake,
    instance: String,
    request: Option<u64>,
    challenge: Option<fluxer_client::SudoChallenge>,
    error: Option<String>,
}

pub(super) struct ProfileEdit {
    name: Entity<TextInput>,
    bio: Entity<TextInput>,
    pronouns: Entity<TextInput>,
    avatar: Option<Arc<crate::avatar_crop::AvatarImage>>,
    crop_revision: u64,
    crop_running: bool,
    crop: crate::avatar_crop::Crop,
    cropped: Option<crate::avatar_crop::CroppedAvatar>,
    avatar_loading: bool,
    avatar_generation: u64,
    avatar_pending: bool,
    avatar_feedback: Option<String>,
    banner: Option<Arc<crate::avatar_crop::AvatarImage>>,
    banner_crop_revision: u64,
    banner_crop_running: bool,
    banner_crop: crate::avatar_crop::Crop,
    banner_cropped: Option<crate::avatar_crop::CroppedAvatar>,
    banner_loading: bool,
    banner_generation: u64,
    banner_pending: bool,
    banner_feedback: Option<String>,
    pub pending: bool,
    pub feedback: Option<String>,
}
impl ProfileEdit {
    pub fn reset(&mut self, cx: &mut Context<Fluxer>) {
        for field in [&self.name, &self.bio, &self.pronouns] {
            field.update(cx, |input, cx| input.set(String::new(), cx));
        }
        self.avatar_generation = self.avatar_generation.wrapping_add(1);
        self.crop_revision = self.crop_revision.wrapping_add(1);
        self.avatar_loading = false;
        self.avatar = None;
        self.cropped = None;
        self.avatar_pending = false;
        self.avatar_feedback = None;
        self.banner_generation = self.banner_generation.wrapping_add(1);
        self.banner_crop_revision = self.banner_crop_revision.wrapping_add(1);
        self.banner_loading = false;
        self.banner = None;
        self.banner_cropped = None;
        self.banner_pending = false;
        self.banner_feedback = None;
        self.pending = false;
        self.feedback = None;
    }
    pub fn avatar_status(&mut self, status: fluxer_client::RelationshipMutationStatus) {
        use fluxer_client::RelationshipMutationStatus as Status;
        self.avatar_pending = matches!(status, Status::Pending);
        self.avatar_feedback = Some(match status {
            Status::Pending => "Saving avatar…".into(),
            Status::Complete => {
                self.avatar = None;
                self.cropped = None;
                "Avatar saved".into()
            }
            Status::Failed(message) => message,
        });
    }
    pub fn banner_status(&mut self, status: fluxer_client::RelationshipMutationStatus) {
        use fluxer_client::RelationshipMutationStatus as Status;
        self.banner_pending = matches!(status, Status::Pending);
        self.banner_feedback = Some(match status {
            Status::Pending => "Saving banner…".into(),
            Status::Complete => {
                self.banner = None;
                self.banner_cropped = None;
                "Banner saved".into()
            }
            Status::Failed(message) => message,
        });
    }
    pub fn new(cx: &mut Context<Fluxer>) -> Self {
        Self {
            name: cx.new(|cx| TextInput::new("Display name", cx)),
            bio: cx.new(|cx| {
                let mut input = TextInput::new("About me", cx);
                input.multiline = true;
                input
            }),
            pronouns: cx.new(|cx| TextInput::new("Pronouns", cx)),
            avatar: None,
            crop_revision: 0,
            crop_running: false,
            crop: Default::default(),
            cropped: None,
            avatar_loading: false,
            avatar_generation: 0,
            avatar_pending: false,
            avatar_feedback: None,
            banner: None,
            banner_crop_revision: 0,
            banner_crop_running: false,
            banner_crop: Default::default(),
            banner_cropped: None,
            banner_loading: false,
            banner_generation: 0,
            banner_pending: false,
            banner_feedback: None,
            pending: false,
            feedback: None,
        }
    }
}

impl Fluxer {
    pub(super) fn open_settings(
        &mut self,
        section: Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pane == Pane::Settings
            && self.settings_section == Section::Profile
            && section != Section::Profile
        {
            self.clear_avatar_editor(cx);
            self.clear_banner_editor(cx);
        }
        // Recording belongs to the visible Keybinds page, never a hidden page
        // reached through another section or navigation shortcut.
        self.interface.recording = None;
        if self.pane != Pane::Settings {
            self.settings_return = self.pane;
        }
        if section == Section::Profile
            && (self.pane != Pane::Settings || self.settings_section != Section::Profile)
        {
            if let Some(user) = &self.state.current_user {
                self.profile_edit.name.update(cx, |input, cx| {
                    input.set(user.global_name.clone().unwrap_or_default(), cx)
                });
                self.profile_edit.bio.update(cx, |input, cx| {
                    input.set(
                        user.extra
                            .get("bio")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_owned(),
                        cx,
                    )
                });
                self.profile_edit.pronouns.update(cx, |input, cx| {
                    input.set(
                        user.extra
                            .get("pronouns")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_owned(),
                        cx,
                    )
                });
            }
            self.profile_edit.feedback = None;
        }
        if self.settings_section == Section::AccountSessions && section != Section::AccountSessions
        {
            self.clear_session_revocation(cx);
        }
        self.commerce.clear();
        if section != Section::Connections {
            self.connection_unlink = None;
        }
        self.settings_section = section;
        self.pane = Pane::Settings;
        if section == Section::Connections && !self.demo {
            self.fetch_connections(None, cx);
        }
        if section == Section::AccountSessions && !self.demo {
            let request_id = fluxer_client::next_pending_message_id();
            self.account_sessions_request = Some(request_id);
            self.account_sessions = None;
            self.command(Command::FetchAccountSessions { request_id }, cx);
        }
        if let Section::Commerce(view) = section {
            self.load_commerce(view, cx);
        }
        if section == Section::Voice && !self.demo {
            self.command(Command::RefreshDevices, cx);
        }
        self.focus.focus(window);
        cx.notify();
    }

    pub(super) fn clear_session_revocation(&mut self, cx: &mut Context<Self>) {
        if self.session_revocation.take().is_some() {
            self.command(Command::CancelAccountSessionRevocation, cx);
        }
        self.session_revocation_secret
            .update(cx, |input, cx| input.set(String::new(), cx));
    }

    pub(super) fn handle_session_revocation_result(
        &mut self,
        request_id: u64,
        result: Result<fluxer_protocol::SessionRevocationOutcome, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.session_revocation.as_mut() else {
            return;
        };
        if pending.request != Some(request_id) {
            return;
        }
        pending.request = None;
        match result {
            Ok(fluxer_protocol::SessionRevocationOutcome::Revoked) => {
                self.session_revocation = None;
                let request_id = fluxer_client::next_pending_message_id();
                self.account_sessions_request = Some(request_id);
                self.command(Command::FetchAccountSessions { request_id }, cx);
            }
            Ok(fluxer_protocol::SessionRevocationOutcome::Challenge(challenge)) => {
                pending.challenge = Some(challenge)
            }
            Err(error) => pending.error = Some(error),
        }
    }

    fn submit_session_revocation(&mut self, proof: Option<bool>, cx: &mut Context<Self>) {
        let Some(pending) = self.session_revocation.as_mut() else {
            return;
        };
        if pending.request.is_some()
            || pending
                .error
                .as_deref()
                .is_some_and(|e| e != "Verification failed. Try again.")
        {
            return;
        }
        pending.error = None;
        let proof = proof.map(|totp| {
            (
                totp,
                self.session_revocation_secret.read(cx).content.to_string(),
            )
        });
        if proof.as_ref().is_some_and(|(_, value)| value.is_empty()) {
            return;
        }
        self.session_revocation_secret
            .update(cx, |input, cx| input.set(String::new(), cx));
        let request_id = fluxer_client::next_pending_message_id();
        pending.request = Some(request_id);
        let command = Command::RevokeAccountSession {
            request_id,
            account_id: pending.account,
            instance: pending.instance.clone(),
            session_id_hash: pending.target.clone(),
            proof,
        };
        self.command(command, cx);
    }

    fn fetch_connections(&mut self, update: Option<(String, String, i64)>, cx: &mut Context<Self>) {
        let request_id = fluxer_client::next_pending_message_id();
        self.connections_request = Some(request_id);
        self.connection_unlink = None;
        self.connections_error = None;
        self.command(
            Command::FetchConnections {
                request_id,
                update,
                delete: None,
            },
            cx,
        );
    }

    fn connections_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut body = div()
            .id("connections-body")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().text_size(px(22.)).child("Linked accounts"))
            .child(muted(
                "Choose who can see your linked accounts on your profile.",
            ));
        if self.demo {
            body = body.child(muted("Sign in to view linked accounts."));
        } else if self.connections_request.is_some() {
            body = body.child(muted("Loading linked accounts…"));
        } else if let Some(Ok(value)) = &self.connections {
            if let Some(items) = value.as_array() {
                if items.is_empty() {
                    body = body.child(muted("No linked accounts."));
                }
                for (index, item) in items.iter().enumerate() {
                    let name = item["name"].as_str().unwrap_or("Unknown account");
                    let kind = item["type"].as_str().unwrap_or("").to_owned();
                    let id = item["id"].as_str().unwrap_or("").to_owned();
                    let flags = item["visibility_flags"].as_i64().unwrap_or(0);
                    let mut row = div()
                        .p_4()
                        .bg(rgb(0x25222e))
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(format!(
                            "{} · {}{}",
                            name,
                            kind,
                            if item["verified"].as_bool() == Some(true) {
                                " · Verified"
                            } else {
                                ""
                            }
                        ));
                    for (flag, label) in [(1, "Everyone"), (2, "Friends"), (4, "Community members")]
                    {
                        let enabled = flags & flag != 0 || (flag != 1 && flags & 1 != 0);
                        let title = format!("{}: {}", label, if enabled { "On" } else { "Off" });
                        if !kind.is_empty() && !id.is_empty() && !(flag != 1 && flags & 1 != 0) {
                            let kind = kind.clone();
                            let id = id.clone();
                            row = row.child(
                                quiet_button(
                                    ("connection-visibility", index * 8 + flag as usize),
                                    title,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.fetch_connections(
                                            Some((kind.clone(), id.clone(), flags ^ flag)),
                                            cx,
                                        );
                                        cx.notify();
                                    },
                                )),
                            );
                        } else {
                            row = row.child(muted(title));
                        }
                    }
                    if !kind.is_empty() && !id.is_empty() {
                        let selected = self
                            .connection_unlink
                            .as_ref()
                            .is_some_and(|(k, i, _)| k == &kind && i == &id);
                        if selected {
                            row = row
                                .child(muted(format!("Unlink {} from your profile?", name)))
                                .child(
                                    quiet_button(
                                        ("connection-unlink-confirm", index),
                                        "Confirm unlink",
                                    )
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            if let Some((kind, id, _)) =
                                                this.connection_unlink.take()
                                            {
                                                let request_id =
                                                    fluxer_client::next_pending_message_id();
                                                this.connections_request = Some(request_id);
                                                this.connections_error = None;
                                                this.command(
                                                    Command::FetchConnections {
                                                        request_id,
                                                        update: None,
                                                        delete: Some((kind, id)),
                                                    },
                                                    cx,
                                                );
                                                cx.notify();
                                            }
                                        },
                                    )),
                                )
                                .child(
                                    quiet_button(("connection-unlink-cancel", index), "Cancel")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.connection_unlink = None;
                                            cx.notify();
                                        })),
                                );
                        } else {
                            let kind = kind.clone();
                            let id = id.clone();
                            let name = name.to_owned();
                            row = row.child(
                                quiet_button(("connection-unlink", index), "Unlink").on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.connection_unlink =
                                            Some((kind.clone(), id.clone(), name.clone()));
                                        cx.notify();
                                    }),
                                ),
                            );
                        }
                    }
                    body = body.child(row);
                }
            } else {
                body = body.child(muted("Could not read linked accounts."));
            }
        } else if let Some(Err(error)) = &self.connections {
            body = body.child(muted(error.clone()));
        }
        if let Some(error) = &self.connections_error {
            body = body.child(muted(error.clone()));
        }
        if !self.demo && self.connections_request.is_none() {
            body = body.child(quiet_button("refresh-connections", "Refresh").on_click(
                cx.listener(|this, _, _, cx| {
                    this.fetch_connections(None, cx);
                    cx.notify();
                }),
            ));
        }
        body.into_any_element()
    }

    fn account_sessions_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut body = div()
            .id("account-sessions-body")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().text_size(px(22.)).child("Devices"))
            .child(muted("Active sign-in sessions for your account"));
        if self.demo {
            body = body.child(muted("Sign in to view devices."));
        } else if self.account_sessions_request.is_some() {
            body = body.child(muted("Loading devices…"));
        } else if let Some(Ok(sessions)) = &self.account_sessions {
            if let Some(sessions) = sessions.as_array() {
                if sessions.is_empty() {
                    body = body.child(muted("No active sessions found."));
                }
                for (index, session) in sessions.iter().enumerate() {
                    let info = &session["client_info"];
                    let title = info["os"]
                        .as_str()
                        .or_else(|| info["platform"].as_str())
                        .unwrap_or("Unknown device");
                    let current = session["current"].as_bool().unwrap_or(false);
                    let details = [
                        info["browser"].as_str(),
                        session["masked_ip"]
                            .as_str()
                            .filter(|_| !self.streaming().hide_names),
                        session["approx_last_used_at"].as_str(),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                    let mut row = div()
                        .p_4()
                        .bg(rgb(0x25222e))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(format!(
                            "{}{}",
                            title,
                            if current { " (This device)" } else { "" }
                        ))
                        .child(muted(details.clone()));
                    if !current
                        && session["current"].as_bool() == Some(false)
                        && self.session_revocation.is_none()
                        && let (Some(hash), Some(account)) = (
                            session["id_hash"].as_str(),
                            self.state.current_user.as_ref(),
                        )
                    {
                        let target = hash.to_owned();
                        let label = if details.is_empty() {
                            title.to_owned()
                        } else {
                            format!("{title} · {details}")
                        };
                        let account = account.id;
                        let instance = self.instance.read(cx).content.to_string();
                        row = row.child(
                            quiet_button(("revoke-device", index), "Sign out device").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.session_revocation = Some(SessionRevocation {
                                        target: target.clone(),
                                        title: label.clone(),
                                        account,
                                        instance: instance.clone(),
                                        request: None,
                                        challenge: None,
                                        error: None,
                                    });
                                    cx.notify();
                                }),
                            ),
                        );
                    }
                    body = body.child(row);
                }
            } else {
                body = body.child(muted("Could not read the device list."));
            }
        } else {
            body = body.child(muted("Could not load devices."));
        }
        if let Some(pending) = &self.session_revocation {
            let mut dialog = div()
                .p_4()
                .bg(rgb(0x35303e))
                .flex()
                .flex_col()
                .gap_2()
                .child(if self.streaming().hide_names {
                    "Sign out this device?".to_owned()
                } else {
                    format!("Sign out {}?", pending.title)
                });
            if pending.request.is_some() {
                dialog = dialog.child(muted("Signing out device…"));
            } else if pending
                .error
                .as_deref()
                .is_some_and(|e| e != "Verification failed. Try again.")
            {
                dialog = dialog.child(muted(pending.error.clone().unwrap()));
            } else if let Some(challenge) = &pending.challenge {
                if let Some(error) = &pending.error {
                    dialog = dialog.child(muted(error.clone()));
                }
                let totp = challenge.methods.totp || challenge.methods.backup_codes;
                let password = challenge.methods.password && !challenge.has_mfa;
                if totp || password {
                    dialog = dialog
                        .child(muted(if totp {
                            "Enter an authenticator or backup code."
                        } else {
                            "Enter your password."
                        }))
                        .child(self.session_revocation_secret.clone())
                        .child(
                            quiet_button("submit-revoke-proof", "Verify and sign out").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.submit_session_revocation(Some(totp), cx)
                                }),
                            ),
                        );
                } else {
                    dialog = dialog.child(muted("This verification method is not supported in Native. Use another client to sign out this device."));
                }
            } else {
                dialog = dialog
                    .child(muted("This immediately signs the selected device out."))
                    .child(
                        quiet_button("confirm-revoke-device", "Sign out device").on_click(
                            cx.listener(|this, _, _, cx| this.submit_session_revocation(None, cx)),
                        ),
                    );
            }
            dialog = dialog.child(
                quiet_button("cancel-revoke-device", "Close")
                    .on_click(cx.listener(|this, _, _, cx| this.clear_session_revocation(cx))),
            );
            body = body.child(dialog);
        }
        if self.session_revocation.is_none() {
            body = body.child(
                quiet_button("refresh-account-sessions", "Refresh").on_click(cx.listener(
                    |this, _, window, cx| this.open_settings(Section::AccountSessions, window, cx),
                )),
            );
        }
        body.into_any_element()
    }

    pub(super) fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_section == Section::Profile {
            self.clear_avatar_editor(cx);
            self.clear_banner_editor(cx);
        }
        self.interface.recording = None;
        self.clear_session_revocation(cx);
        self.commerce.clear();
        self.connection_unlink = None;
        self.pane = self.settings_return;
        if self.pane == Pane::Chat {
            self.composer.focus_handle(cx).focus(window);
        } else {
            self.focus.focus(window);
        }
        cx.notify();
    }

    pub(super) fn retire_avatar_preview(&mut self, cx: &mut Context<Self>) {
        if let Some(cropped) = self.profile_edit.cropped.take() {
            self.retire_image(cropped.preview, cx);
        }
    }
    fn clear_avatar_editor(&mut self, cx: &mut Context<Self>) {
        self.retire_avatar_preview(cx);
        self.profile_edit.crop_revision = self.profile_edit.crop_revision.wrapping_add(1);
        self.profile_edit.avatar_generation = self.profile_edit.avatar_generation.wrapping_add(1);
        self.profile_edit.avatar_loading = false;
        self.profile_edit.avatar = None;
        self.profile_edit.cropped = None;
        self.profile_edit.avatar_feedback = None;
        if self.profile_edit.avatar_pending {
            self.command(Command::CancelOwnAvatar, cx);
        }
    }
    fn pick_avatar(&mut self, cx: &mut Context<Self>) {
        if self.profile_edit.avatar_loading || self.profile_edit.avatar_pending {
            return;
        }
        self.profile_edit.avatar_loading = true;
        self.profile_edit.avatar_generation = self.profile_edit.avatar_generation.wrapping_add(1);
        let generation = self.profile_edit.avatar_generation;
        let account = self.state.current_user.as_ref().map(|u| u.id);
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a static PNG or JPEG avatar".into()),
        });
        let executor = cx.background_executor().clone();
        cx.spawn(async move |entity, cx| {
            let picked = picker.await;
            let result = match picked {
                Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                    Some(path) => Some(
                        executor
                            .spawn(async move { crate::avatar_crop::AvatarImage::load(&path) })
                            .await,
                    ),
                    None => None,
                },
                Ok(Ok(None)) => None,
                Ok(Err(e)) => Some(Err(format!("Could not open file picker: {e}"))),
                Err(_) => Some(Err("File picker closed unexpectedly".into())),
            };
            let _ = entity.update(cx, |this, cx| {
                if this.profile_edit.avatar_generation != generation
                    || this.state.current_user.as_ref().map(|u| u.id) != account
                {
                    return;
                }
                this.profile_edit.avatar_loading = false;
                if let Some(result) = result {
                    match result {
                        Ok(image) => {
                            this.profile_edit.avatar = Some(Arc::new(image));
                            this.profile_edit.crop = Default::default();
                            this.refresh_avatar_crop(cx);
                        }
                        Err(e) => this.profile_edit.avatar_feedback = Some(e),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn refresh_avatar_crop(&mut self, cx: &mut Context<Self>) {
        self.profile_edit.crop_revision = self.profile_edit.crop_revision.wrapping_add(1);
        self.start_avatar_crop(cx);
    }
    fn start_avatar_crop(&mut self, cx: &mut Context<Self>) {
        if self.profile_edit.crop_running {
            return;
        }
        let Some(image) = self.profile_edit.avatar.clone() else {
            return;
        };
        let revision = self.profile_edit.crop_revision;
        let crop = self.profile_edit.crop;
        self.profile_edit.crop_running = true;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |entity, cx| {
            let result = executor.spawn(async move { image.crop(crop) }).await;
            let _ = entity.update(cx, |this, cx| {
                this.profile_edit.crop_running = false;
                if revision != this.profile_edit.crop_revision {
                    if let Ok(cropped) = result {
                        this.retire_image(cropped.preview, cx);
                    }
                    this.start_avatar_crop(cx);
                } else {
                    this.retire_avatar_preview(cx);
                    match result {
                        Ok(cropped) => {
                            this.profile_edit.cropped = Some(cropped);
                            this.profile_edit.avatar_feedback = None;
                        }
                        Err(error) => this.profile_edit.avatar_feedback = Some(error),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn avatar_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.profile_edit.avatar_pending
            || self.profile_edit.avatar_loading
            || self.profile_edit.pending
            || self.profile_edit.banner_pending;
        let mut body = div().mt_4().flex().flex_col().gap_2().child(
            div()
                .text_size(px(12.))
                .text_color(rgb(MUTED))
                .child("AVATAR"),
        );
        if let Some(cropped) = &self.profile_edit.cropped {
            body = body.child(
                img(cropped.preview.clone())
                    .w(px(128.))
                    .h(px(128.))
                    .object_fit(ObjectFit::Cover),
            );
        } else if let Some(user) = &self.state.current_user {
            body = body.child(crate::avatar_images::user_avatar(
                user,
                self.media_endpoint.as_deref(),
                128.,
            ));
        }
        body = body.child(muted("Choose a static PNG or JPEG, up to 10 MB."));
        body = body.child(
            div()
                .flex()
                .gap_2()
                .child(
                    quiet_button(
                        "avatar-choose",
                        if self.profile_edit.avatar_loading {
                            "Loading…"
                        } else {
                            "Choose image"
                        },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !busy {
                            this.pick_avatar(cx);
                        }
                    })),
                )
                .child(
                    quiet_button("avatar-remove", "Remove avatar").on_click(cx.listener(
                        move |this, _, _, cx| {
                            if !busy {
                                this.profile_edit.avatar_pending = true;
                                this.profile_edit.avatar_feedback = Some("Removing avatar…".into());
                                this.command(Command::UpdateOwnAvatar { png: None }, cx);
                            }
                        },
                    )),
                ),
        );
        if self.profile_edit.avatar.is_some() {
            body = body.child(muted(format!(
                "Zoom {:.1}× · horizontal {:.0}% · vertical {:.0}%",
                self.profile_edit.crop.zoom,
                self.profile_edit.crop.x * 100.,
                self.profile_edit.crop.y * 100.
            )));
            for (id, label, dz, dx, dy) in [
                ("avatar-zoom-out", "Zoom out", -0.25, 0., 0.),
                ("avatar-zoom-in", "Zoom in", 0.25, 0., 0.),
                ("avatar-left", "Move crop left", 0., -0.2, 0.),
                ("avatar-right", "Move crop right", 0., 0.2, 0.),
                ("avatar-up", "Move crop up", 0., 0., -0.2),
                ("avatar-down", "Move crop down", 0., 0., 0.2),
            ] {
                body = body.child(quiet_button(id, label).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if !busy {
                            let crop = &mut this.profile_edit.crop;
                            crop.zoom = (crop.zoom + dz).clamp(1., 5.);
                            crop.x = (crop.x + dx).clamp(-1., 1.);
                            crop.y = (crop.y + dy).clamp(-1., 1.);
                            this.refresh_avatar_crop(cx);
                            cx.notify();
                        }
                    },
                )));
            }
            body = body.child(
                quiet_button(
                    "avatar-save",
                    if self.profile_edit.crop_running {
                        "Preparing…"
                    } else {
                        "Save avatar"
                    },
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !busy
                        && !this.profile_edit.crop_running
                        && let Some(crop) = &this.profile_edit.cropped
                    {
                        let png = crop.png.clone();
                        this.profile_edit.avatar_pending = true;
                        this.profile_edit.avatar_feedback = Some("Saving avatar…".into());
                        this.command(Command::UpdateOwnAvatar { png: Some(png) }, cx);
                    }
                })),
            );
        }
        if self.profile_edit.avatar_pending {
            body = body.child(
                quiet_button("avatar-cancel", "Cancel avatar save").on_click(
                    cx.listener(|this, _, _, cx| this.command(Command::CancelOwnAvatar, cx)),
                ),
            );
        }
        if let Some(feedback) = &self.profile_edit.avatar_feedback {
            body = body.child(muted(feedback));
        }
        body.into_any_element()
    }
    pub(super) fn retire_banner_preview(&mut self, cx: &mut Context<Self>) {
        if let Some(banner_cropped) = self.profile_edit.banner_cropped.take() {
            self.retire_image(banner_cropped.preview, cx);
        }
    }
    fn clear_banner_editor(&mut self, cx: &mut Context<Self>) {
        self.retire_banner_preview(cx);
        self.profile_edit.banner_crop_revision =
            self.profile_edit.banner_crop_revision.wrapping_add(1);
        self.profile_edit.banner_generation = self.profile_edit.banner_generation.wrapping_add(1);
        self.profile_edit.banner_loading = false;
        self.profile_edit.banner = None;
        self.profile_edit.banner_cropped = None;
        self.profile_edit.banner_feedback = None;
        if self.profile_edit.banner_pending {
            self.command(Command::CancelOwnBanner, cx);
        }
    }
    fn pick_banner(&mut self, cx: &mut Context<Self>) {
        if self.profile_edit.banner_loading || self.profile_edit.banner_pending {
            return;
        }
        self.profile_edit.banner_loading = true;
        self.profile_edit.banner_generation = self.profile_edit.banner_generation.wrapping_add(1);
        let generation = self.profile_edit.banner_generation;
        let account = self.state.current_user.as_ref().map(|u| u.id);
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose a static PNG or JPEG banner".into()),
        });
        let executor = cx.background_executor().clone();
        cx.spawn(async move |entity, cx| {
            let picked = picker.await;
            let result = match picked {
                Ok(Ok(Some(paths))) => match paths.into_iter().next() {
                    Some(path) => Some(
                        executor
                            .spawn(async move { crate::avatar_crop::AvatarImage::load(&path) })
                            .await,
                    ),
                    None => None,
                },
                Ok(Ok(None)) => None,
                Ok(Err(e)) => Some(Err(format!("Could not open file picker: {e}"))),
                Err(_) => Some(Err("File picker closed unexpectedly".into())),
            };
            let _ = entity.update(cx, |this, cx| {
                if this.profile_edit.banner_generation != generation
                    || this.state.current_user.as_ref().map(|u| u.id) != account
                {
                    return;
                }
                this.profile_edit.banner_loading = false;
                if let Some(result) = result {
                    match result {
                        Ok(image) => {
                            this.profile_edit.banner = Some(Arc::new(image));
                            this.profile_edit.banner_crop = Default::default();
                            this.refresh_banner_crop(cx);
                        }
                        Err(e) => this.profile_edit.banner_feedback = Some(e),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn refresh_banner_crop(&mut self, cx: &mut Context<Self>) {
        self.profile_edit.banner_crop_revision =
            self.profile_edit.banner_crop_revision.wrapping_add(1);
        self.start_banner_crop(cx);
    }
    fn start_banner_crop(&mut self, cx: &mut Context<Self>) {
        if self.profile_edit.banner_crop_running {
            return;
        }
        let Some(image) = self.profile_edit.banner.clone() else {
            return;
        };
        let revision = self.profile_edit.banner_crop_revision;
        let crop = self.profile_edit.banner_crop;
        self.profile_edit.banner_crop_running = true;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |entity, cx| {
            let result = executor.spawn(async move { image.crop_banner(crop) }).await;
            let _ = entity.update(cx, |this, cx| {
                this.profile_edit.banner_crop_running = false;
                if revision != this.profile_edit.banner_crop_revision {
                    if let Ok(banner_cropped) = result {
                        this.retire_image(banner_cropped.preview, cx);
                    }
                    this.start_banner_crop(cx);
                } else {
                    this.retire_banner_preview(cx);
                    match result {
                        Ok(banner_cropped) => {
                            this.profile_edit.banner_cropped = Some(banner_cropped);
                            this.profile_edit.banner_feedback = None;
                        }
                        Err(error) => this.profile_edit.banner_feedback = Some(error),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn banner_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.profile_edit.banner_pending
            || self.profile_edit.banner_loading
            || self.profile_edit.pending
            || self.profile_edit.avatar_pending;
        let mut body = div().mt_4().flex().flex_col().gap_2().child(
            div()
                .text_size(px(12.))
                .text_color(rgb(MUTED))
                .child("BANNER"),
        );
        if let Some(banner_cropped) = &self.profile_edit.banner_cropped {
            body = body.child(
                img(banner_cropped.preview.clone())
                    .w(px(340.))
                    .h(px(120.))
                    .object_fit(ObjectFit::Cover),
            );
        } else if let Some(user) = &self.state.current_user {
            let banner = user.extra.get("banner").and_then(|v| v.as_str());
            let url = banner.and_then(|hash| {
                crate::avatar_images::profile_banner_url(
                    user.id,
                    None,
                    hash,
                    self.media_endpoint.as_deref(),
                )
            });
            body = body.child(
                div()
                    .w(px(340.))
                    .h(px(120.))
                    .bg(rgb(0x514665))
                    .when_some(url, |v, url| {
                        v.child(img(url).w_full().h_full().object_fit(ObjectFit::Cover))
                    }),
            );
        }
        body = body.child(muted(
            "Choose a static PNG or JPEG, up to 10 MB. Banner uploads require premium.",
        ));
        body = body.child(
            div()
                .flex()
                .gap_2()
                .child(
                    quiet_button(
                        "banner-choose",
                        if self.profile_edit.banner_loading {
                            "Loading…"
                        } else {
                            "Choose image"
                        },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !busy {
                            this.pick_banner(cx);
                        }
                    })),
                )
                .child(
                    quiet_button("banner-remove", "Remove banner").on_click(cx.listener(
                        move |this, _, _, cx| {
                            if !busy {
                                this.profile_edit.banner_pending = true;
                                this.profile_edit.banner_feedback = Some("Removing banner…".into());
                                this.command(Command::UpdateOwnBanner { png: None }, cx);
                            }
                        },
                    )),
                ),
        );
        if self.profile_edit.banner.is_some() {
            body = body.child(muted(format!(
                "Zoom {:.1}× · horizontal {:.0}% · vertical {:.0}%",
                self.profile_edit.banner_crop.zoom,
                self.profile_edit.banner_crop.x * 100.,
                self.profile_edit.banner_crop.y * 100.
            )));
            for (id, label, dz, dx, dy) in [
                ("banner-zoom-out", "Zoom out", -0.25, 0., 0.),
                ("banner-zoom-in", "Zoom in", 0.25, 0., 0.),
                ("banner-left", "Move crop left", 0., -0.2, 0.),
                ("banner-right", "Move crop right", 0., 0.2, 0.),
                ("banner-up", "Move crop up", 0., 0., -0.2),
                ("banner-down", "Move crop down", 0., 0., 0.2),
            ] {
                body = body.child(quiet_button(id, label).on_click(cx.listener(
                    move |this, _, _, cx| {
                        if !busy {
                            let crop = &mut this.profile_edit.banner_crop;
                            crop.zoom = (crop.zoom + dz).clamp(1., 5.);
                            crop.x = (crop.x + dx).clamp(-1., 1.);
                            crop.y = (crop.y + dy).clamp(-1., 1.);
                            this.refresh_banner_crop(cx);
                            cx.notify();
                        }
                    },
                )));
            }
            body = body.child(
                quiet_button(
                    "banner-save",
                    if self.profile_edit.banner_crop_running {
                        "Preparing…"
                    } else {
                        "Save banner"
                    },
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !busy
                        && !this.profile_edit.banner_crop_running
                        && let Some(crop) = &this.profile_edit.banner_cropped
                    {
                        let png = crop.png.clone();
                        this.profile_edit.banner_pending = true;
                        this.profile_edit.banner_feedback = Some("Saving banner…".into());
                        this.command(Command::UpdateOwnBanner { png: Some(png) }, cx);
                    }
                })),
            );
        }
        if self.profile_edit.banner_pending {
            body = body.child(
                quiet_button("banner-cancel", "Cancel banner save").on_click(
                    cx.listener(|this, _, _, cx| this.command(Command::CancelOwnBanner, cx)),
                ),
            );
        }
        if let Some(feedback) = &self.profile_edit.banner_feedback {
            body = body.child(muted(feedback));
        }
        body.into_any_element()
    }
    fn profile_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let field = |title: &'static str, input: Entity<TextInput>| {
            div()
                .flex()
                .flex_col()
                .gap_1()
                .mt_4()
                .child(div().text_size(px(12.)).text_color(rgb(MUTED)).child(title))
                .child(input)
        };
        let pending = self.profile_edit.pending
            || self.profile_edit.avatar_pending
            || self.profile_edit.banner_pending;
        div()
            .id("profile-settings-body")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_6()
            .child(
                div()
                    .max_w(px(620.))
                    .flex()
                    .flex_col()
                    .child(settings_heading("My profile"))
                    .child(muted("Your profile is visible to others across Fluxer."))
                    .child(self.avatar_settings(cx))
                    .child(self.banner_settings(cx))
                    .child(field("DISPLAY NAME", self.profile_edit.name.clone()))
                    .child(field("PRONOUNS", self.profile_edit.pronouns.clone()))
                    .child(field("ABOUT ME", self.profile_edit.bio.clone()))
                    .child(
                        div()
                            .mt_4()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                quiet_button(
                                    "profile-save",
                                    if pending { "Saving…" } else { "Save changes" },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if pending {
                                            return;
                                        }
                                        let global_name =
                                            this.profile_edit.name.read(cx).content.to_string();
                                        let bio =
                                            this.profile_edit.bio.read(cx).content.to_string();
                                        let pronouns =
                                            this.profile_edit.pronouns.read(cx).content.to_string();
                                        this.profile_edit.pending = true;
                                        this.profile_edit.feedback = Some("Saving…".into());
                                        this.command(
                                            Command::UpdateOwnProfile {
                                                global_name,
                                                bio,
                                                pronouns,
                                            },
                                            cx,
                                        );
                                    },
                                )),
                            )
                            .when_some(self.profile_edit.feedback.clone(), |v, text| {
                                v.child(muted(&text))
                            }),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn settings_shell(&self, cx: &mut Context<Self>) -> AnyElement {
        let content = match self.settings_section {
            Section::Profile => self.profile_settings(cx),
            Section::AccountSessions => self.account_sessions_view(cx),
            Section::Connections => self.connections_view(cx),
            Section::Commerce(view) => self.commerce_settings(view, cx),
            Section::Voice => self.voice_settings(cx),
            Section::Privacy
            | Section::Appearance
            | Section::Accessibility
            | Section::MessagesMedia
            | Section::TrustedDomains
            | Section::Keybinds
            | Section::Desktop => self.expanded_settings(cx),
            Section::StreamerMode => self.streamer_settings(cx),
            Section::Notifications => div()
                .id("notification-settings-body")
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_y_scroll()
                .p_6()
                .child(div().max_w(px(720.)).child(self.notification_settings(cx)))
                .into_any_element(),
        };
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .child(
                div()
                    .id("settings-navigation")
                    .w(px(200.))
                    .flex_shrink_0()
                    .h_full()
                    .overflow_y_scroll()
                    .bg(rgb(self.interface.base_theme().panel()))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .px_2()
                            .py_3()
                            .text_size(px(12.))
                            .text_color(rgb(MUTED))
                            .child("USER SETTINGS"),
                    )
                    .children(
                        [
                            (Section::Privacy, "settings-privacy", "Privacy & safety"),
                            (Section::Profile, "settings-profile", "My profile"),
                            (
                                Section::Connections,
                                "settings-connections",
                                "Linked accounts",
                            ),
                            (
                                Section::AccountSessions,
                                "settings-account-sessions",
                                "Devices",
                            ),
                            (
                                Section::Commerce(CommerceView::Plutonium),
                                "settings-plutonium",
                                "Plutonium",
                            ),
                            (
                                Section::Commerce(CommerceView::Gifts),
                                "settings-gifts",
                                "Gifts & codes",
                            ),
                            (Section::Appearance, "settings-appearance", "Appearance"),
                            (
                                Section::Accessibility,
                                "settings-accessibility",
                                "Accessibility",
                            ),
                            (
                                Section::MessagesMedia,
                                "settings-messages-media",
                                "Messages & media",
                            ),
                            (
                                Section::TrustedDomains,
                                "settings-trusted-domains",
                                "Trusted domains",
                            ),
                            (Section::StreamerMode, "settings-streamer", "Streamer Mode"),
                            (Section::Desktop, "settings-desktop", "Desktop"),
                            (Section::Keybinds, "settings-keybinds", "Keybinds"),
                            (Section::Voice, "settings-voice", "Voice & audio"),
                            (
                                Section::Notifications,
                                "settings-notifications",
                                "Notifications",
                            ),
                        ]
                        .into_iter()
                        .filter(|(section, _, _)| {
                            !matches!(section, Section::Commerce(_)) || self.commerce_visible()
                        })
                        .map(|(section, id, label)| {
                            button_base(id)
                                .w_full()
                                .px_2()
                                .py_2()
                                .bg(rgb(if self.settings_section == section {
                                    0x433d52
                                } else {
                                    0x25222e
                                }))
                                .child(label)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.open_settings(section, window, cx)
                                }))
                        }),
                    )
                    .child(div().flex_1())
                    .child(
                        quiet_button("settings-sign-out", "Sign out")
                            .on_click(cx.listener(|this, _, _, cx| this.logout(cx))),
                    ),
            )
            .child(content)
            .child(
                div()
                    .pt_6()
                    .pr_4()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1()
                    .child(
                        icon_button("close-settings", Icon::Close, "Close settings (Escape)")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.close_settings(window, cx)),
                            ),
                    )
                    .child(muted("ESC")),
            )
            .into_any_element()
    }
}

pub(super) fn setting_row(title: &str, description: &str, control: impl IntoElement) -> Div {
    div()
        .w_full()
        .py_4()
        .border_b_1()
        .border_color(rgb(0x403a4c))
        .flex()
        .items_center()
        .gap_4()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title.to_owned()),
                )
                .child(
                    div()
                        .mt_1()
                        .text_size(px(13.))
                        .text_color(rgb(MUTED))
                        .child(description.to_owned()),
                ),
        )
        .child(control)
}
impl Fluxer {
    fn expanded_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::controls::{slider, toggle};
        use crate::native_preferences::Shortcut;
        use fluxer_core::DisplaySetting;
        let pending = self.interface.server_pending;
        let mut body = div().max_w(px(720.)).flex().flex_col().gap_2();
        match self.settings_section {
            Section::Privacy => {
                body = body.child(settings_heading("Privacy & safety"));
                let settings = &self.state.user_settings;
                if let Some(flags) = settings
                    .get("friend_source_flags")
                    .and_then(serde_json::Value::as_u64)
                {
                    body = body.child(muted("Choose who can send you a friend request."));
                    let everyone = flags & 4 != 0;
                    for (id, title, description, flag) in [
                        (
                            "privacy-everyone",
                            "Everyone",
                            "Anyone who finds you can send a request.",
                            4,
                        ),
                        (
                            "privacy-mutual-friends",
                            "Friends of friends",
                            "People who share a friend with you.",
                            1,
                        ),
                        (
                            "privacy-mutual-guilds",
                            "Community members",
                            "People who share a community with you.",
                            2,
                        ),
                    ] {
                        let enabled = flags & flag != 0;
                        body = body.child(setting_row(
                            title,
                            description,
                            toggle(id, enabled || (flag != 4 && everyone), title).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    if !pending && (flag == 4 || !everyone) {
                                        this.save_display_setting(
                                            DisplaySetting::FriendSource {
                                                flag,
                                                enabled: !enabled,
                                            },
                                            cx,
                                        );
                                    }
                                }),
                            ),
                        ));
                    }
                } else {
                    body = body.child(muted("Friend request settings are loading."));
                }
                for (id, key, title, bot) in [
                    (
                        "privacy-member-dms",
                        "default_guilds_restricted",
                        "Direct messages from community members",
                        false,
                    ),
                    (
                        "privacy-bot-dms",
                        "bot_default_guilds_restricted",
                        "Direct messages from bots",
                        true,
                    ),
                ] {
                    if let Some(restricted) = settings.get(key).and_then(serde_json::Value::as_bool)
                    {
                        body = body.child(setting_row(title, "Allow direct messages in newly joined communities. Existing community settings are unchanged.",
                            toggle(id, !restricted, title).on_click(cx.listener(move |this, _, _, cx| {
                                if !pending {
                                    let setting = if bot { DisplaySetting::BotDefaultGuildsRestricted(!restricted) } else { DisplaySetting::DefaultGuildsRestricted(!restricted) };
                                    this.save_display_setting(setting, cx);
                                }
                            }))));
                    }
                }
            }
            Section::Appearance => {
                let compact = fluxer_core::compact_messages(&self.state.user_settings);
                let size = self
                    .interface
                    .font_draft
                    .unwrap_or_else(|| fluxer_core::message_font_size(&self.state.user_settings));
                body =
                    body.child(settings_heading("Appearance"))
                        .child(muted("Base theme · saved on this device"))
                        .child(
                            div().flex().gap_2().mt_3().mb_4().children(
                                [
                                    (native_preferences::BaseTheme::Dark, "Dark"),
                                    (native_preferences::BaseTheme::Coal, "Coal"),
                                ]
                                .into_iter()
                                .map(|(theme, label)| {
                                    let selected = self.interface.base_theme() == theme;
                                    quiet_button(
                                        label,
                                        if selected {
                                            format!("{label} ✓")
                                        } else {
                                            label.to_owned()
                                        },
                                    )
                                    .when(selected, |button| button.bg(rgb(ACCENT)))
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.interface.set_base_theme(theme);
                                            cx.notify();
                                        },
                                    ))
                                }),
                            ),
                        )
                        .child(muted("Message presentation · saved to your account"))
                        .child(setting_row(
                            "Compact messages",
                            "Hide avatars and reduce spacing between messages.",
                            toggle("settings-compact", compact, "Compact messages").on_click(
                                cx.listener(move |this, _, _, cx| {
                                    if !pending {
                                        this.save_display_setting(
                                            DisplaySetting::Compact(!compact),
                                            cx,
                                        );
                                    }
                                }),
                            ),
                        ))
                        .child(
                            div()
                                .py_4()
                                .child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(format!("Message text size · {size:.0}px")),
                                )
                                .child(div().mt_2().child(slider(
                                    "settings-font-size",
                                    size,
                                    8.,
                                    48.,
                                    1.,
                                    "Message text size",
                                    cx.listener(|this, size: &f32, _, cx| {
                                        if !this.interface.server_pending {
                                            this.interface.font_draft = Some(*size);
                                            cx.notify();
                                        }
                                    }),
                                )))
                                .child(
                                    div()
                                        .mt_2()
                                        .flex()
                                        .justify_between()
                                        .child(muted("8px"))
                                        .child(muted("48px")),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .child(
                                    quiet_button(
                                        "settings-apply-size",
                                        if pending {
                                            "Saving…"
                                        } else {
                                            "Apply text size"
                                        },
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.save_display_setting(
                                                DisplaySetting::FontSize(size as f64),
                                                cx,
                                            );
                                        },
                                    )),
                                )
                                .when(self.interface.font_draft.is_some(), |row| {
                                    row.child(
                                        quiet_button("settings-cancel-size", "Reset preview")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.interface.font_draft = None;
                                                cx.notify();
                                            })),
                                    )
                                }),
                        )
                        .child(
                            div().mt_4().p_4().rounded_md().bg(rgb(0x25222e)).child(
                                div()
                                    .text_size(px(size))
                                    .line_height(px(size * 1.375))
                                    .child(
                                        "The quick brown fox jumps over the lazy dog.
A preview of your message text size.",
                                    ),
                            ),
                        );
            }
            Section::Accessibility => {
                let reduced = self.interface.reduced_motion();
                body=body.child(settings_heading("Accessibility")).child(muted("Motion preferences · saved on this device"))
                    .child(setting_row("Reduce motion", "Pause automatic GIF animation in chat and search. Hover or open an image to play it intentionally.",toggle("settings-reduced-motion",reduced,"Reduce motion").on_click(cx.listener(|this,_,_,cx|{this.interface.toggle_motion();cx.notify();}))))
                    .child(div().mt_4().text_color(rgb(MUTED)).child("Message text size and compact display are available under Appearance."));
            }
            Section::MessagesMedia => {
                let autoplay = media_animation::gif_autoplay(&self.state.user_settings);
                let animate_emoji =
                    crate::emoji_presentation::autoplay(&self.state.user_settings, false);
                body=body.child(settings_heading("Messages & media")).child(muted("Animation preferences · saved to your account"))
                    .child(setting_row("Automatically play GIFs", "Play animated images without hovering. Reduced motion takes priority on this device.",toggle("settings-gif-autoplay",autoplay,"Automatically play GIFs").on_click(cx.listener(move|this,_,_,cx|{if !pending{this.save_display_setting(DisplaySetting::GifAutoplay(!autoplay),cx);}}))))
                    .child(setting_row("Animate custom emoji", "Play animated custom emoji automatically. Reduced motion takes priority on this device.",toggle("settings-animate-emoji",animate_emoji,"Animate custom emoji").on_click(cx.listener(move|this,_,_,cx|{if !pending{this.save_display_setting(DisplaySetting::AnimateEmoji(!animate_emoji),cx);}}))));
            }
            Section::TrustedDomains => {
                body = body.child(settings_heading("Trusted domains")).child(muted(
                    "Save a domain from an external link warning. Remove it here to show warnings again.",
                ));
                match self
                    .state
                    .user_settings
                    .get("trusted_domains")
                    .and_then(|value| value.as_array())
                {
                    Some(values) if values.iter().all(|value| value.is_string()) => {
                        let domains: Vec<String> = values
                            .iter()
                            .filter_map(|value| value.as_str().map(str::to_owned))
                            .collect();
                        if domains.is_empty() {
                            body = body.child(muted("No trusted domains."));
                        }
                        for (index, domain) in domains.iter().enumerate() {
                            let mut remaining = domains.clone();
                            remaining.remove(index);
                            body = body.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .gap_3()
                                    .child(div().min_w_0().child(if domain == "*" {
                                        "All external domains".to_owned()
                                    } else {
                                        domain.clone()
                                    }))
                                    .child(
                                        quiet_button(("remove-trusted-domain", index), "Remove")
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                if !pending {
                                                    this.save_display_setting(
                                                        DisplaySetting::TrustedDomains(
                                                            remaining.clone(),
                                                        ),
                                                        cx,
                                                    );
                                                }
                                            })),
                                    ),
                            );
                        }
                        if domains.len() > 1 {
                            body = body.child(
                                quiet_button("clear-trusted-domains", "Clear trusted domains")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if !pending {
                                            this.save_display_setting(
                                                DisplaySetting::TrustedDomains(vec![]),
                                                cx,
                                            );
                                        }
                                    })),
                            );
                        }
                    }
                    _ => {
                        body = body.child(muted(
                            "Trusted domains are unavailable. Reload settings to manage them.",
                        ))
                    }
                }
            }
            Section::Desktop => {
                use crate::native_preferences::WindowSetting;
                let behavior = self.interface.window_behavior();
                let linux = cfg!(target_os = "linux");
                body = body
                    .child(settings_heading("Desktop"))
                    .child(muted("Window preferences · saved on this device"))
                    .child(
                        div()
                            .mt_2()
                            .p_3()
                            .rounded_md()
                            .bg(rgb(0x25222e))
                            .text_size(px(13.))
                            .text_color(rgb(MUTED))
                            .child(self.desktop_tray_status()),
                    );
                if !linux {
                    body = body.child(muted(
                        "System tray integration is currently available on Linux.",
                    ));
                }
                for (setting, id, title, description, value, available) in [
                    (
                        WindowSetting::ShowTrayIcon,
                        "settings-show-tray",
                        "Show system tray icon",
                        "Keep an icon in the system tray to reopen Fluxer.",
                        behavior.show_tray_icon,
                        linux,
                    ),
                    (
                        WindowSetting::CloseToTray,
                        "settings-close-to-tray",
                        "Close to tray",
                        "When the tray is available, closing the window keeps Fluxer running. Ctrl+Q or Quit exits the app.",
                        behavior.close_to_tray,
                        linux && behavior.show_tray_icon,
                    ),
                    (
                        WindowSetting::StartMinimized,
                        "settings-start-minimized",
                        "Start minimized",
                        "Start in the system tray when a tray host is available.",
                        behavior.start_minimized,
                        linux && behavior.show_tray_icon,
                    ),
                    (
                        WindowSetting::ColoredTrayIcon,
                        "settings-colored-tray",
                        "Use colored tray icon",
                        "Use Fluxer’s colors instead of a monochrome tray icon.",
                        behavior.colored_tray_icon,
                        linux && behavior.show_tray_icon,
                    ),
                ] {
                    let control = toggle(id, value, title)
                        .when(!available, |view| {
                            view.opacity(0.45)
                                .tab_stop(false)
                                .cursor(CursorStyle::Arrow)
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if available {
                                this.interface.set_window_behavior(setting, !value);
                                cx.notify();
                            }
                        }))
                        .on_key_down(move |event, _, cx| {
                            if available
                                && matches!(event.keystroke.key.as_str(), "enter" | "space")
                            {
                                // GPUI synthesizes on_click on key-up. Only
                                // block parent shortcuts here; mutate once in
                                // the click handler for mouse and keyboard.
                                cx.stop_propagation();
                            }
                        });
                    body = body.child(setting_row(title, description, control));
                }
                if linux && !behavior.show_tray_icon {
                    body = body.child(muted(
                        "Enable the tray icon to change the other window preferences.",
                    ));
                }
            }
            Section::Keybinds => {
                body = body.child(settings_heading("Keybinds")).child(muted(
                    "Shortcuts work while the native app is focused and are saved on this device.",
                ));
                for action in Shortcut::ALL {
                    let recording = self.interface.recording == Some(action);
                    let label = if recording {
                        "Press a shortcut…".to_owned()
                    } else {
                        self.interface.binding(action).to_owned()
                    };
                    body = body.child(setting_row(
                        action.label(),
                        if recording {
                            "Escape cancels recording."
                        } else {
                            "Click to record a new shortcut."
                        },
                        quiet_button(
                            ("settings-keybind", action.index()),
                            SharedString::from(label),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.interface.recording = Some(action);
                                this.interface.error = None;
                                this.focus.focus(window);
                                cx.notify();
                            },
                        )),
                    ));
                }
                body=body.child(div().mt_4().child(quiet_button("settings-reset-keybinds","Reset shortcuts").on_click(cx.listener(|this,_,_,cx|{this.interface.reset();cx.notify();}))))
                    .child(div().mt_2().text_size(px(13.)).text_color(rgb(MUTED)).child("F8 remains reserved for in-app push to talk. These shortcuts do not control the app globally."));
            }
            _ => {}
        }
        if matches!(
            self.settings_section,
            Section::Privacy
                | Section::Appearance
                | Section::MessagesMedia
                | Section::TrustedDomains
        ) {
            if pending {
                body = body.child(muted("Saving account preference…"));
            }
            if let Some(error) = &self.interface.server_error {
                body = body.child(div().mt_2().text_color(rgb(0xe79a9e)).child(error.clone()));
            }
        }
        if let Some(error) = &self.interface.error {
            body = body.child(div().mt_2().text_color(rgb(0xe79a9e)).child(error.clone()));
        }
        div()
            .id("expanded-settings-body")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .p_6()
            .child(body)
            .into_any_element()
    }
}

fn settings_heading(title: &str) -> Div {
    div()
        .text_size(px(24.))
        .font_weight(FontWeight::BOLD)
        .mb_2()
        .child(title.to_owned())
}
