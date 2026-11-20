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
fn scoped_hook<'a>(
    hooks: &'a [fluxer_protocol::Webhook],
    channel: Snowflake,
    id: &str,
) -> Option<&'a fluxer_protocol::Webhook> {
    hooks
        .iter()
        .find(|hook| hook.id == id && hook.channel_id == channel.to_string())
}

impl Worker {
    pub(super) fn webhook_list_request(
        &mut self,
        request: u64,
        channel: Snowflake,
        action: Option<WebhookAction>,
    ) -> Result<()> {
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
            let future = async {
                if let Some(action) = action {
                    match action {
                        WebhookAction::Create { name } => {
                            client
                                .create_webhook(&channel.to_string(), &name)
                                .await
                                .map_err(error)?;
                        }
                        WebhookAction::Rename { id, name } => {
                            let hooks = client
                                .channel_webhooks(&channel.to_string())
                                .await
                                .map_err(error)?;
                            let hook = scoped_hook(&hooks, channel, &id)
                                .ok_or("Webhook is no longer in this channel")?;
                            client
                                .update_webhook(&id, &name, hook.avatar.as_deref())
                                .await
                                .map_err(error)?;
                        }
                        WebhookAction::Delete { id } => {
                            let hooks = client
                                .channel_webhooks(&channel.to_string())
                                .await
                                .map_err(error)?;
                            if scoped_hook(&hooks, channel, &id).is_none() {
                                return Err("Webhook is no longer in this channel".into());
                            }
                            client.delete_webhook(&id).await.map_err(error)?;
                        }
                    }
                    client
                        .channel_webhooks(&channel.to_string())
                        .await
                        .map_err(|e| format!("Change succeeded, but refresh failed: {}", error(e)))
                } else {
                    client
                        .channel_webhooks(&channel.to_string())
                        .await
                        .map_err(error)
                }
            };
            let result = tokio::time::timeout(Duration::from_secs(15), future).await
                .unwrap_or_else(|_| Err("Request timed out. Refresh the list before trying again; the change may have succeeded.".into()));
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mutations_only_target_the_requested_channel() {
        let hooks: Vec<fluxer_protocol::Webhook> = serde_json::from_value(json!([
            {"id":"1", "guild_id":"3", "channel_id":"4", "name":"hook", "avatar":"keep", "type":1, "token":"synthetic-secret"},
            {"id":"2", "guild_id":"3", "channel_id":"5", "name":"other", "avatar":null, "type":1}
        ])).unwrap();
        assert_eq!(
            scoped_hook(&hooks, Snowflake(4), "1")
                .unwrap()
                .avatar
                .as_deref(),
            Some("keep")
        );
        assert!(scoped_hook(&hooks, Snowflake(4), "2").is_none());
        assert!(scoped_hook(&hooks, Snowflake(4), "missing").is_none());
        assert!(
            !format!("{:?}", scoped_hook(&hooks, Snowflake(4), "1")).contains("synthetic-secret")
        );
    }
}
