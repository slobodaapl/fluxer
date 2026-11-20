use super::*;

pub(super) struct BanEvent {
    guild: Snowflake,
    request: u64,
    result: Result<Vec<Value>>,
}
pub(super) struct BanState {
    pub rx: async_mpsc::UnboundedReceiver<BanEvent>,
    tx: async_mpsc::UnboundedSender<BanEvent>,
    job: Option<(u64, JoinHandle<()>)>,
}
impl BanState {
    pub fn new() -> Self {
        let (tx, rx) = async_mpsc::unbounded_channel();
        Self { rx, tx, job: None }
    }
    pub fn clear(&mut self) {
        if let Some((_, job)) = self.job.take() {
            job.abort();
        }
    }
}
impl Worker {
    pub(super) fn ban_list_request(
        &mut self,
        request: u64,
        guild: Snowflake,
        user: Option<Snowflake>,
    ) -> Result<()> {
        let actor = self.state.current_user.as_ref().ok_or("Not signed in")?.id;
        if !self.state.guilds.contains_key(&guild)
            || !self.state.members.contains_key(&(guild, actor))
            || !self
                .state
                .guild_permissions_for(guild, actor)
                .contains(Permissions::BAN_MEMBERS)
        {
            return Err("You cannot manage community bans".into());
        }
        if self.demo {
            return Err("Ban management is unavailable in the offline demo".into());
        }
        let client = self.client.clone().ok_or("Not signed in")?;
        self.bans.clear();
        let tx = self.bans.tx.clone();
        let task = tokio::spawn(async move {
            let future = async {
                if let Some(user) = user {
                    client
                        .unban_member(&guild.to_string(), &user.to_string())
                        .await
                        .map_err(error)?;
                }
                let value = client.guild_bans(&guild.to_string()).await.map_err(|err| {
                    if user.is_some() {
                        format!(
                            "Ban revoked, but the list could not be refreshed: {}",
                            error(err)
                        )
                    } else {
                        error(err)
                    }
                })?;
                value
                    .as_array()
                    .cloned()
                    .ok_or_else(|| "Invalid ban list returned by server".to_owned())
            };
            let result = tokio::time::timeout(Duration::from_secs(15), future)
                .await
                .unwrap_or_else(|_| Err("Ban management request timed out".into()));
            let _ = tx.send(BanEvent {
                guild,
                request,
                result,
            });
        });
        self.bans.job = Some((request, task));
        Ok(())
    }
    pub(super) fn ban_list_event(&mut self, event: BanEvent) {
        if self
            .bans
            .job
            .as_ref()
            .is_none_or(|(request, _)| *request != event.request)
        {
            return;
        }
        self.bans.job = None;
        self.emit(Event::BanList {
            request_id: event.request,
            guild_id: event.guild,
            result: event.result,
        });
    }
}
