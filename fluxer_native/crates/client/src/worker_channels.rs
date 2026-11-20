// SPDX-License-Identifier: AGPL-3.0-or-later
//! Channel mutations run outside the worker loop and are fenced to the session.
use super::*;
use fluxer_core::Channel;

struct Job {
    generation: u64,
    account: Snowflake,
    previous: Option<Channel>,
    delete: bool,
    task: JoinHandle<()>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(super) struct ChannelEvent {
    generation: u64,
    result: Result<Value>,
}
pub(super) struct ChannelJobs {
    next: u64,
    job: Option<Job>,
    tx: async_mpsc::UnboundedSender<ChannelEvent>,
    pub rx: async_mpsc::UnboundedReceiver<ChannelEvent>,
}
impl ChannelJobs {
    pub fn new() -> Self {
        let (tx, rx) = async_mpsc::unbounded_channel();
        Self {
            next: 0,
            job: None,
            tx,
            rx,
        }
    }
}
impl Worker {
    pub(super) fn clear_channel_jobs(&mut self) {
        self.channels.next = self.channels.next.wrapping_add(1);
        self.channels.job.take();
    }
    pub(super) fn create_basic_channel(
        &mut self,
        guild_id: Snowflake,
        parent_id: Option<Snowflake>,
        kind: u16,
        name: String,
    ) -> Result<()> {
        if !matches!(kind, 0 | 2 | 4) || name.trim().is_empty() {
            return Err("Invalid channel name or type".into());
        }
        let user = self.state.current_user.as_ref().ok_or("Not signed in")?;
        let account = user.id;
        let guild = self
            .state
            .guilds
            .get(&guild_id)
            .ok_or("Community unavailable")?;
        let allowed =
            if let Some(parent) = parent_id {
                self.state.channels.get(&parent).is_some_and(|c| {
                    c.guild_id == Some(guild_id) && c.kind == ChannelType::Category
                }) && self
                    .state
                    .permissions_for(parent, account)
                    .contains(Permissions::MANAGE_CHANNELS)
            } else if guild.owner_id == account {
                true
            } else {
                self.state
                    .members
                    .get(&(guild_id, account))
                    .is_some_and(|m| {
                        fluxer_core::resolve_permissions(guild, m, &[])
                            .contains(Permissions::MANAGE_CHANNELS)
                    })
            };
        if !allowed || (kind == 4 && parent_id.is_some()) {
            return Err("You cannot manage channels here".into());
        }
        let client = self.client.clone().ok_or("Not connected")?;
        self.start_channel_job(account, None, async move {
            client
                .create_guild_channel(
                    &guild_id.to_string(),
                    name.trim(),
                    kind,
                    parent_id.map(|id| id.to_string()).as_deref(),
                )
                .await
                .map_err(error)
        })
    }
    pub(super) fn rename_basic_channel(
        &mut self,
        channel_id: Snowflake,
        name: String,
    ) -> Result<()> {
        let user = self.state.current_user.as_ref().ok_or("Not signed in")?;
        let account = user.id;
        let channel = self
            .state
            .channels
            .get(&channel_id)
            .ok_or("Channel unavailable")?;
        if channel.guild_id.is_none()
            || !matches!(
                channel.kind,
                ChannelType::Text | ChannelType::Voice | ChannelType::Category
            )
            || name.trim().is_empty()
            || !self
                .state
                .permissions_for(channel_id, account)
                .contains(Permissions::MANAGE_CHANNELS)
        {
            return Err("You cannot edit this channel".into());
        }
        let previous = channel.clone();
        let client = self.client.clone().ok_or("Not connected")?;
        self.start_channel_job(account, Some(previous), async move {
            client
                .rename_channel(&channel_id.to_string(), name.trim())
                .await
                .map_err(error)
        })
    }
    pub(super) fn delete_basic_channel(&mut self, channel_id: Snowflake) -> Result<()> {
        let account = self.state.current_user.as_ref().ok_or("Not signed in")?.id;
        let channel = self
            .state
            .channels
            .get(&channel_id)
            .ok_or("Channel unavailable")?;
        if channel.guild_id.is_none()
            || !matches!(
                channel.kind,
                ChannelType::Text | ChannelType::Voice | ChannelType::Category
            )
            || !self
                .state
                .permissions_for(channel_id, account)
                .contains(Permissions::MANAGE_CHANNELS)
        {
            return Err("You cannot delete this channel".into());
        }
        let client = self.client.clone().ok_or("Not connected")?;
        self.start_channel_job(account, None, async move {
            client
                .delete_channel(&channel_id.to_string())
                .await
                .map_err(error)
        })?;
        if let Some(job) = self.channels.job.as_mut() {
            job.delete = true;
        }
        Ok(())
    }
    fn start_channel_job(
        &mut self,
        account: Snowflake,
        previous: Option<Channel>,
        request: impl std::future::Future<Output = Result<Value>> + Send + 'static,
    ) -> Result<()> {
        if self.channels.job.is_some() {
            return Err("A channel change is already pending".into());
        }
        self.channels.next = self.channels.next.wrapping_add(1);
        let generation = self.channels.next;
        let tx = self.channels.tx.clone();
        let task = tokio::spawn(async move {
            let result = tokio::time::timeout(Duration::from_secs(15), request)
                .await
                .unwrap_or_else(|_| Err("Channel change timed out".into()));
            let _ = tx.send(ChannelEvent { generation, result });
        });
        self.channels.job = Some(Job {
            generation,
            account,
            previous,
            delete: false,
            task,
        });
        Ok(())
    }
    pub(super) fn channel_event(&mut self, event: ChannelEvent) {
        let Some(job) = self.channels.job.as_ref() else {
            return;
        };
        if job.generation != event.generation {
            return;
        }
        let account = job.account;
        let previous = job.previous.clone();
        let delete = job.delete;
        self.channels.job.take();
        if self.state.current_user.as_ref().map(|u| u.id) != Some(account) {
            return;
        }
        let result = event.result.and_then(|value| {
            if delete {
                return Ok(());
            }
            if let Some(previous) = previous {
                // Gateway events may have modified or deleted the channel during the request.
                if self.state.channels.get(&previous.id) != Some(&previous) {
                    return Ok(());
                }
                let id: Snowflake = serde_json::from_value(
                    value.get("id").cloned().ok_or("Invalid channel response")?,
                )
                .map_err(error)?;
                if id != previous.id {
                    return Err("Channel response ID mismatch".into());
                }
                self.state
                    .apply_gateway("CHANNEL_UPDATE", value)
                    .map_err(error)?;
            } else {
                let id: Snowflake = serde_json::from_value(
                    value.get("id").cloned().ok_or("Invalid channel response")?,
                )
                .map_err(error)?;
                if self.state.channels.contains_key(&id) {
                    return Ok(());
                }
                self.state
                    .apply_gateway("CHANNEL_CREATE", value)
                    .map_err(error)?;
            }
            self.snapshot();
            Ok(())
        });
        if let Err(message) = result {
            self.emit(Event::Error(message));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker() -> Worker {
        let (tx, _) = std::sync::mpsc::channel();
        Worker::new(tx, Arc::new(Mutex::new(BTreeMap::new())))
    }
    fn user() -> User {
        serde_json::from_value(json!({"id":"1", "username":"test", "discriminator":"0000"}))
            .unwrap()
    }
    fn channel() -> Channel {
        serde_json::from_value(json!({"id":"2", "guild_id":"3", "type":0, "name":"old"})).unwrap()
    }
    #[tokio::test]
    async fn gateway_update_wins_over_late_rename_response() {
        let mut worker = worker();
        worker.state.current_user = Some(user());
        let previous = channel();
        worker.state.channels.insert(previous.id, previous.clone());
        worker
            .start_channel_job(1.into(), Some(previous), async {
                Ok(json!({"id":"2", "name":"rest"}))
            })
            .unwrap();
        let event = worker.channels.rx.recv().await.unwrap();
        worker.state.channels.get_mut(&2.into()).unwrap().name = "gateway".into();
        worker.channel_event(event);
        assert_eq!(worker.state.channels[&2.into()].name, "gateway");
    }
    #[tokio::test]
    async fn reset_fences_queued_response() {
        let mut worker = worker();
        worker.state.current_user = Some(user());
        worker
            .start_channel_job(1.into(), None, async {
                Ok(json!({"id":"2", "guild_id":"3", "type":0, "name":"created"}))
            })
            .unwrap();
        let event = worker.channels.rx.recv().await.unwrap();
        worker.clear_channel_jobs();
        worker.channel_event(event);
        assert!(!worker.state.channels.contains_key(&2.into()));
    }
}
