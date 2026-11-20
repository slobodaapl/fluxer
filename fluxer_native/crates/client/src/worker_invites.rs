// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::InviteStatus;
use fluxer_protocol::InvitePreview;

pub(super) struct InviteEvent {
    request_id: u64,
    code: String,
    result: Result<InviteResult>,
    accepting: bool,
}

enum InviteResult {
    Preview(InvitePreview),
    Created(String),
}

pub(super) struct InviteState {
    pub rx: async_mpsc::UnboundedReceiver<InviteEvent>,
    tx: async_mpsc::UnboundedSender<InviteEvent>,
    task: Option<JoinHandle<()>>,
    request_id: u64,
    preview: Option<(String, InvitePreview)>,
    accepted: Option<InvitePreview>,
    accepting: bool,
    navigation_superseded: bool,
    pub deadline: Option<tokio::time::Instant>,
}

impl InviteState {
    pub fn new() -> Self {
        let (tx, rx) = async_mpsc::unbounded_channel();
        Self {
            rx,
            tx,
            task: None,
            request_id: 0,
            preview: None,
            accepted: None,
            accepting: false,
            navigation_superseded: false,
            deadline: None,
        }
    }

    fn clear(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.preview = None;
        self.accepted = None;
        self.accepting = false;
        self.navigation_superseded = false;
        self.deadline = None;
    }
}

fn invite_error(error: fluxer_protocol::Error, accepting: bool) -> String {
    use fluxer_protocol::Error;
    match error {
        Error::Api { code, status, .. } => match code.as_deref() {
            Some("UNKNOWN_INVITE") => "This invite is invalid, expired, or out of uses.".into(),
            Some("ACCOUNT_LIMITED") => "Your account is currently restricted from joining.".into(),
            Some("INVITES_DISABLED" | "FEATURE_TEMPORARILY_DISABLED") => {
                "Invites are temporarily disabled for this community.".into()
            }
            Some("MAX_GUILD_MEMBERS") => "This community is full.".into(),
            Some("MAX_GUILDS") => "Your account has reached its community limit.".into(),
            Some("MAX_GROUP_DM_RECIPIENTS") => "This group DM is full.".into(),
            Some("MAX_GROUP_DMS") => "Your account has reached its group DM limit.".into(),
            Some("TEMPORARY_INVITE_REQUIRES_PRESENCE") => {
                "Temporary invites need a live connection. Reconnect and try again.".into()
            }
            Some("USER_BANNED_FROM_GUILD" | "USER_IP_BANNED_FROM_GUILD") => {
                "You are banned from this community.".into()
            }
            Some("GUILD_VISIT_AGE_RESTRICTED" | "NSFW_CONTENT_AGE_RESTRICTED") => {
                "This community is for adults only.".into()
            }
            _ if status == 404 => "This invite is invalid, expired, or out of uses.".into(),
            _ if status == 403 => "Your account cannot use this invite.".into(),
            _ => format!("Fluxer could not process this invite (HTTP {status})."),
        },
        Error::RateLimited { retry_after, .. } => {
            format!("Too many invite requests. Try again in {} seconds.", retry_after.as_secs().max(1))
        }
        _ if accepting => {
            "Fluxer could not confirm whether you joined. Check the sidebar or preview the invite again.".into()
        }
        _ => "Could not load this invite. Check your connection and try again.".into(),
    }
}

impl Worker {
    pub(super) fn clear_invites(&mut self) {
        self.invites.clear();
    }

    pub(super) fn cancel_invite(&mut self, request_id: u64) {
        if self.invites.request_id == request_id && !self.invites.accepting {
            self.clear_invites();
        }
    }

    pub(super) fn create_invite(&mut self, request_id: u64, channel_id: Snowflake) {
        if self.invites.accepting {
            return;
        }
        self.clear_invites();
        self.invites.request_id = request_id;
        let allowed = self.state.current_user.as_ref().is_some_and(|user| {
            self.state.channels.get(&channel_id).is_some_and(|channel| {
                channel.guild_id.is_some()
                    && matches!(
                        channel.kind,
                        ChannelType::Text
                            | ChannelType::Voice
                            | ChannelType::Announcement
                            | ChannelType::Forum
                            | ChannelType::Media
                    )
                    && self
                        .state
                        .permissions_for(channel_id, user.id)
                        .contains(Permissions::VIEW_CHANNEL | Permissions::CREATE_INSTANT_INVITE)
            })
        });
        if !allowed || self.demo {
            self.invite_status(InviteStatus::Failed(
                "You cannot create an invite for this channel.".into(),
            ));
            return;
        }
        let Ok(client) = self.client() else {
            self.invite_status(InviteStatus::Failed("Sign in to create an invite.".into()));
            return;
        };
        let tx = self.invites.tx.clone();
        self.invites.task = Some(tokio::spawn(async move {
            let result = match tokio::time::timeout(
                Duration::from_secs(12),
                client.create_channel_invite(channel_id),
            )
            .await
            {
                Ok(Ok(url)) => Ok(InviteResult::Created(url)),
                Ok(Err(_)) => {
                    Err("Could not create an invite. Check your permissions and connection.".into())
                }
                Err(_) => Err("Invite creation timed out. Try again.".into()),
            };
            let _ = tx.send(InviteEvent {
                request_id,
                code: String::new(),
                result,
                accepting: false,
            });
        }));
    }

    pub(super) fn preview_invite(&mut self, request_id: u64, input: String) {
        if self.invites.accepting {
            self.emit(Event::Invite {
                request_id,
                status: InviteStatus::Failed("An invite is already being accepted.".into()),
            });
            return;
        }
        self.clear_invites();
        self.invites.request_id = request_id;
        let result = self.client().and_then(|client| {
            if self.demo || self.state.current_user.is_none() {
                return Err("Sign in to use an invite.".into());
            }
            let code = client
                .resolve_invite_code(&input)
                .ok_or("Enter an invite code or an invite link for your current instance.")?;
            Ok((client, code))
        });
        match result {
            Ok((client, code)) => self.start_invite_request(client, code, false),
            Err(message) => self.invite_status(InviteStatus::Failed(message)),
        }
    }

    pub(super) async fn accept_invite(&mut self, request_id: u64) {
        if self.invites.request_id != request_id || self.invites.accepting {
            return;
        }
        let Some((code, preview)) = self.invites.preview.take() else {
            self.invite_status(InviteStatus::Failed("Preview this invite first.".into()));
            return;
        };
        self.invites.accepting = true;
        if preview.guild.as_ref().is_some_and(|guild| {
            self.state.guilds.contains_key(&guild.id)
                && self
                    .state
                    .current_user
                    .as_ref()
                    .is_some_and(|user| self.state.members.contains_key(&(guild.id, user.id)))
        }) {
            // InviteCommands.tsx transitions directly for an existing member.
            self.on_invite_accepted(preview).await;
        } else if let Ok(client) = self.client() {
            self.start_invite_request(client, code, true);
        } else {
            self.clear_invites();
            self.invite_status(InviteStatus::Failed("Sign in to use an invite.".into()));
        }
    }

    fn start_invite_request(&mut self, client: Client, code: String, accepting: bool) {
        let tx = self.invites.tx.clone();
        let request_id = self.invites.request_id;
        self.invites.task = Some(tokio::spawn(async move {
            let request = async {
                if accepting {
                    client.accept_invite(&code).await
                } else {
                    client.preview_invite(&code).await
                }
            };
            let result = match tokio::time::timeout(Duration::from_secs(12), request).await {
                Ok(result) => result
                    .map(InviteResult::Preview)
                    .map_err(|error| invite_error(error, accepting)),
                Err(_) if accepting => Err(
                    "The request timed out. Check your sidebar to see whether you joined.".into(),
                ),
                Err(_) => Err("Invite preview timed out. Try again.".into()),
            };
            let _ = tx.send(InviteEvent {
                request_id,
                code,
                result,
                accepting,
            });
        }));
    }

    pub(super) async fn invite_event(&mut self, event: InviteEvent) {
        if self.invites.request_id != event.request_id || self.invites.task.take().is_none() {
            return;
        }
        match event.result {
            Ok(InviteResult::Created(url)) => self.invite_status(InviteStatus::Created(url)),
            Ok(InviteResult::Preview(preview)) if event.accepting => {
                self.on_invite_accepted(preview).await
            }
            Ok(InviteResult::Preview(preview)) => {
                self.invites.preview = Some((event.code, preview.clone()));
                self.invite_status(InviteStatus::Preview(Box::new(preview)));
            }
            Err(message) => {
                self.clear_invites();
                self.invite_status(InviteStatus::Failed(message));
            }
        }
    }

    pub(super) fn supersede_invite_navigation(&mut self) {
        if self.invites.accepted.is_some() {
            self.clear_invites();
            self.invite_status(InviteStatus::Joined);
        } else if self.invites.accepting {
            // Keep the POST alive to report its outcome, then leave selection alone.
            self.invites.navigation_superseded = true;
        }
    }

    async fn on_invite_accepted(&mut self, preview: InvitePreview) {
        if self.invites.navigation_superseded {
            self.clear_invites();
            self.invite_status(InviteStatus::Joined);
            return;
        }
        self.invites.accepted = Some(preview);
        self.invites.deadline = Some(tokio::time::Instant::now() + Duration::from_secs(12));
        self.invite_status(InviteStatus::Opening);
        self.open_accepted_invite().await;
    }

    pub(super) fn invite_timeout(&mut self) {
        self.clear_invites();
        self.invite_status(InviteStatus::Failed(
            "Invite accepted. Fluxer is still syncing the destination. Check your sidebar once it reconnects.".into(),
        ));
    }

    pub(super) async fn open_accepted_invite(&mut self) {
        let Some(invite) = &self.invites.accepted else {
            return;
        };
        let guild_id = invite.guild.as_ref().map(|guild| guild.id);
        let target = invite.channel.id;
        let channel_id = if let Some(guild_id) = guild_id {
            if !self
                .state
                .guilds
                .get(&guild_id)
                .is_some_and(|guild| !guild.unavailable)
            {
                return;
            }
            let target_channel = self.state.channels.get(&target).filter(|channel| {
                channel.guild_id == Some(guild_id) && self.require_history(target).is_ok()
            });
            target_channel.map(|channel| channel.id).or_else(|| {
                self.state
                    .guild_channels(guild_id)
                    .into_iter()
                    .find(|channel| {
                        !channel.kind.is_voice() && self.require_history(channel.id).is_ok()
                    })
                    .map(|channel| channel.id)
            })
        } else {
            if !self.state.channels.get(&target).is_some_and(|channel| {
                channel.kind == ChannelType::GroupDm && channel.guild_id.is_none()
            }) {
                return;
            }
            Some(target)
        };
        self.cancel_open_dm();
        self.clear_invites();
        if let Some(channel) = channel_id {
            if let Err(message) = self.select_channel(channel).await {
                self.snapshot();
                self.invite_status(InviteStatus::Failed(format!(
                    "Invite accepted, but the channel could not open. {message}"
                )));
                return;
            }
        } else {
            self.selected_guild = guild_id;
            self.state.selected_channel = None;
            if let Err(message) = self.sync_selected_guild().await {
                self.emit(Event::Error(message));
            }
            self.persist_selection().await;
            self.snapshot();
        }
        self.invite_status(InviteStatus::Opened { guild_id });
    }

    fn invite_status(&self, status: InviteStatus) {
        self.emit(Event::Invite {
            request_id: self.invites.request_id,
            status,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn accepted_group_dm_waits_for_channel_create_before_opening() {
        let (tx, rx) = mpsc::channel();
        let mut worker = Worker::new(tx, Arc::new(Mutex::new(BTreeMap::new())));
        worker.demo = true; // Use local history while exercising the gateway reducer.
        worker.state = ClientState::demo();
        let previous_channel = worker.state.selected_channel;
        worker.invites.accepting = true;
        worker.invites.request_id = 7;
        let invite = serde_json::from_value(json!({
            "code":"Ab-C", "type":1, "member_count":2, "temporary":false, "expires_at":null,
            "channel":{"id":"420","type":3,"name":null,"recipients":[{"username":"friend"}]}
        }))
        .unwrap();

        worker.on_invite_accepted(invite).await;
        assert_eq!(worker.state.selected_channel, previous_channel);
        assert!(worker.invites.deadline.is_some());
        assert!(matches!(
            rx.recv().unwrap(),
            Event::Invite {
                request_id: 7,
                status: InviteStatus::Opening,
            }
        ));

        worker.gateway(GatewayEvent::Dispatch {
            name: "CHANNEL_CREATE".into(),
            data: json!({"id":"420","type":3,"name":null,"recipients":[{"id":"421","username":"friend"}]}),
            sequence: Some(1),
        }).await.unwrap();
        assert_eq!(worker.state.selected_channel, Some(420.into()));
        assert!(worker.invites.deadline.is_none());
        assert!(rx.try_iter().any(|event| matches!(
            event,
            Event::Invite {
                request_id: 7,
                status: InviteStatus::Opened { guild_id: None },
            }
        )));
    }

    #[tokio::test]
    async fn newer_selection_supersedes_invite_navigation_during_accept_or_gateway_wait() {
        for already_accepted in [false, true] {
            let (tx, rx) = mpsc::channel();
            let mut worker = Worker::new(tx, Arc::new(Mutex::new(BTreeMap::new())));
            worker.demo = true;
            worker.state = ClientState::demo();
            worker.invites.accepting = true;
            let invite: InvitePreview = serde_json::from_value(json!({
                "code":"Ab-C", "type":1, "member_count":2, "temporary":false,
                "channel":{"id":"420","type":3}
            }))
            .unwrap();

            if already_accepted {
                worker.on_invite_accepted(invite.clone()).await;
            }
            worker
                .command(Command::SelectGuild {
                    guild_id: 10.into(),
                })
                .await
                .unwrap();
            worker
                .state
                .apply_gateway("CHANNEL_CREATE", json!({"id":"420","type":3}))
                .unwrap();
            if already_accepted {
                worker.open_accepted_invite().await;
            } else {
                worker.on_invite_accepted(invite).await;
            }
            assert_ne!(worker.state.selected_channel, Some(420.into()));
            assert_eq!(worker.selected_guild, Some(10.into()));
            assert!(worker.invites.deadline.is_none());
            assert!(rx.try_iter().any(|event| matches!(
                event,
                Event::Invite {
                    status: InviteStatus::Joined,
                    ..
                }
            )));
        }
    }

    #[test]
    fn source_invite_errors_are_actionable() {
        let api = |code: &str, status| fluxer_protocol::Error::Api {
            code: Some(code.into()),
            status,
            body: json!({"message":"upstream"}),
        };
        assert!(invite_error(api("UNKNOWN_INVITE", 404), false).contains("expired"));
        assert!(
            invite_error(api("TEMPORARY_INVITE_REQUIRES_PRESENCE", 403), true)
                .contains("live connection")
        );
        assert!(invite_error(api("USER_IP_BANNED_FROM_GUILD", 403), true).contains("banned"));
        assert!(invite_error(api("MAX_GROUP_DM_RECIPIENTS", 400), true).contains("full"));
    }
}
