use super::*;
pub(super) struct WebhookEvent {
    channel: Snowflake,
    request: u64,
    result: Result<Vec<fluxer_protocol::Webhook>>,
}
pub(super) struct WebhookState {
    pub rx: async_mpsc::UnboundedReceiver<WebhookEvent>,
    tx: async_mpsc::UnboundedSender<WebhookEvent>,
    job: Option<(u64, JoinHandle<()>)>,
}
impl WebhookState {
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
    pub(super) fn webhook_list_request(&mut self, request: u64, channel: Snowflake) -> Result<()> {
        let actor = self.state.current_user.as_ref().ok_or("Not signed in")?.id;
        if self.demo
            || self
                .state
                .channels
                .get(&channel)
                .and_then(|c| c.guild_id)
                .is_none()
            || !self
                .state
                .permissions_for(channel, actor)
                .contains(Permissions::MANAGE_WEBHOOKS)
        {
            return Err("You cannot manage channel webhooks".into());
        }
        let client = self.client.clone().ok_or("Not signed in")?;
        self.webhooks.clear();
        let tx = self.webhooks.tx.clone();
        let task = tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(15),
                client.channel_webhooks(&channel.to_string()),
            )
            .await
            .map_err(|_| "Webhook request timed out".to_owned())
            .and_then(|r| r.map_err(error));
            let _ = tx.send(WebhookEvent {
                channel,
                request,
                result,
            });
        });
        self.webhooks.job = Some((request, task));
        Ok(())
    }
    pub(super) fn webhook_list_event(&mut self, event: WebhookEvent) {
        if self
            .webhooks
            .job
            .as_ref()
            .is_none_or(|(request, _)| *request != event.request)
        {
            return;
        }
        self.webhooks.job = None;
        let authorized = self.state.current_user.as_ref().is_some_and(|actor| {
            self.state
                .channels
                .get(&event.channel)
                .and_then(|c| c.guild_id)
                .is_some()
                && self
                    .state
                    .permissions_for(event.channel, actor.id)
                    .contains(Permissions::MANAGE_WEBHOOKS)
        });
        let result = if authorized {
            event.result
        } else {
            Err("You cannot manage channel webhooks".into())
        };
        self.emit(Event::WebhookList {
            request_id: event.request,
            channel_id: event.channel,
            result,
        });
    }
}
