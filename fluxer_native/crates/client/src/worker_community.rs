//! Community mutations follow GuildCommands; gateway events own the roster.
use super::*;
use fluxer_protocol::Method;

#[derive(Clone, Copy)]
enum Operation {
    Create,
    Leave(Snowflake),
}
struct Job {
    token: u64,
    task: JoinHandle<()>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(super) struct CommunityEvent {
    epoch: u64,
    token: u64,
    operation: Operation,
    result: Result<Option<Snowflake>>,
}
pub(super) struct CommunityState {
    epoch: u64,
    next: u64,
    job: Option<Job>,
    tx: async_mpsc::UnboundedSender<CommunityEvent>,
    pub rx: async_mpsc::UnboundedReceiver<CommunityEvent>,
}
impl CommunityState {
    pub fn new() -> Self {
        let (tx, rx) = async_mpsc::unbounded_channel();
        Self {
            epoch: 0,
            next: 0,
            job: None,
            tx,
            rx,
        }
    }
}
impl Worker {
    pub(super) fn clear_community(&mut self) {
        self.community.epoch = self.community.epoch.wrapping_add(1);
        self.community.job = None;
    }
    fn community_error(&self, message: impl Into<String>) {
        self.emit(Event::CommunityMutation {
            created: None,
            left: None,
            error: Some(message.into()),
        });
    }
    pub(super) fn create_community(&mut self, name: String) {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 100 {
            return self.community_error("Community name must be 1–100 characters");
        }
        if self.demo {
            return self.community_error("Community creation is unavailable in the offline demo");
        }
        self.start_community(
            Operation::Create,
            Method::POST,
            "/guilds".into(),
            Some(json!({"name": name})),
        );
    }
    pub(super) fn leave_community(&mut self, guild_id: Snowflake, delete_messages: bool) {
        if self.demo {
            return self.community_error("Leaving communities is unavailable in the offline demo");
        }
        let Some(guild) = self.state.guilds.get(&guild_id) else {
            return self.community_error("Community no longer available");
        };
        if self
            .state
            .current_user
            .as_ref()
            .is_some_and(|u| u.id == guild.owner_id)
        {
            return self.community_error("Transfer ownership before leaving this community");
        }
        let path = format!(
            "/users/@me/guilds/{guild_id}{}",
            if delete_messages {
                "?delete_messages=true"
            } else {
                ""
            }
        );
        self.start_community(Operation::Leave(guild_id), Method::DELETE, path, None);
    }
    fn start_community(
        &mut self,
        operation: Operation,
        method: Method,
        path: String,
        body: Option<Value>,
    ) {
        if self.community.job.is_some() {
            return self.community_error("A community update is already pending");
        }
        let Some(client) = self.client.clone() else {
            return self.community_error("Not signed in");
        };
        self.community.next = self.community.next.wrapping_add(1);
        let token = self.community.next;
        let epoch = self.community.epoch;
        let tx = self.community.tx.clone();
        let task = tokio::spawn(async move {
            let result = match tokio::time::timeout(
                Duration::from_secs(15),
                client.request(method, &path, body.as_ref()),
            )
            .await
            {
                Ok(result) => result.map_err(error).and_then(|value| match operation {
                    Operation::Create => serde_json::from_value(
                        value.get("id").cloned().ok_or("Missing community ID")?,
                    )
                    .map(Some)
                    .map_err(error),
                    Operation::Leave(_) => Ok(None),
                }),
                Err(_) => Err("Community update timed out".into()),
            };
            let _ = tx.send(CommunityEvent {
                epoch,
                token,
                operation,
                result,
            });
        });
        self.community.job = Some(Job { token, task });
    }
    pub(super) async fn community_event(&mut self, event: CommunityEvent) {
        if event.epoch != self.community.epoch
            || !self
                .community
                .job
                .as_ref()
                .is_some_and(|job| job.token == event.token)
        {
            return;
        }
        self.community.job = None;
        let left = match event.operation {
            Operation::Leave(id) if event.result.is_ok() => Some(id),
            _ => None,
        };
        if left.is_some() && self.selected_guild == left {
            self.selected_guild = None;
            self.persist_selection().await;
        }
        self.emit(Event::CommunityMutation {
            created: event.result.as_ref().ok().copied().flatten(),
            left,
            error: event.result.err(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cleared_session_discards_queued_community_result() {
        let (tx, rx) = mpsc::channel();
        let mut worker = Worker::new(tx, Arc::new(Mutex::new(BTreeMap::new())));
        worker.selected_guild = Some(42.into());
        let epoch = worker.community.epoch;
        worker.community.job = Some(Job {
            token: 1,
            task: tokio::spawn(std::future::pending()),
        });
        worker.clear_community();
        worker
            .community_event(CommunityEvent {
                epoch,
                token: 1,
                operation: Operation::Leave(42.into()),
                result: Ok(None),
            })
            .await;
        assert_eq!(worker.selected_guild, Some(42.into()));
        assert!(rx.try_recv().is_err());
    }
}
