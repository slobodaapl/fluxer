//! Account-scoped status/settings updates, following UserSettings.saveSettings
//! and LocalPresence/GatewaySessionPool in the classic client.
use super::*;
use crate::RelationshipMutationStatus;
struct Job {
    generation: u64,
    task: JoinHandle<()>,
    previous: BTreeMap<String, Option<Value>>,
    patch: Value,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(super) struct PresenceEvent {
    generation: u64,
    result: Result<()>,
}
pub(super) struct PresenceState {
    next: u64,
    job: Option<Job>,
    tx: async_mpsc::UnboundedSender<PresenceEvent>,
    pub rx: async_mpsc::UnboundedReceiver<PresenceEvent>,
}
impl PresenceState {
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
    pub(super) fn clear_presence(&mut self) {
        self.presence.next = self.presence.next.wrapping_add(1);
        self.presence.job = None;
    }
    pub(super) fn presence_payload(&self) -> Option<Value> {
        let settings = self.state.user_settings.as_object()?;
        let status = settings
            .get("status")
            .and_then(Value::as_str)
            .filter(|s| matches!(*s, "online" | "idle" | "dnd" | "invisible"))
            .unwrap_or("online");
        let custom = settings.get("custom_status").and_then(Value::as_object).map(|s| {
            let mut result=json!({"text":s.get("text").cloned().unwrap_or(Value::Null),"expires_at":s.get("expires_at").cloned().unwrap_or(Value::Null),"emoji_id":s.get("emoji_id").cloned().unwrap_or(Value::Null),"emoji_name":s.get("emoji_name").cloned().unwrap_or(Value::Null)});
            if let Some(animated)=s.get("emoji_animated").filter(|v|v.is_boolean()){result["emoji_animated"]=animated.clone();} result
        }).unwrap_or(Value::Null);
        Some(json!({"status":status,"afk":false,"mobile":false,"custom_status":custom}))
    }
    pub(super) async fn publish_presence(&self) {
        if let (Some(tx), Some(payload)) = (&self.gateway_commands, self.presence_payload()) {
            let _ = tx.send(GatewayCommand::Presence(payload)).await;
        }
    }
    pub(super) async fn set_presence_status(&mut self, status: String) -> Result<()> {
        if !matches!(status.as_str(), "online" | "idle" | "dnd" | "invisible") {
            return Err("Invalid status".into());
        }
        self.update_presence_settings(
            json!({"status":status,"status_resets_at":null,"status_resets_to":null}),
        )
        .await
    }
    pub(super) async fn clear_custom_status(&mut self) -> Result<()> {
        self.update_presence_settings(json!({"custom_status":null}))
            .await
    }
    pub(super) async fn set_custom_status(&mut self, text: String) -> Result<()> {
        let text = text.trim();
        if text.encode_utf16().count() > 128 {
            return Err("Custom status is too long".into());
        }
        let prior = self
            .state
            .user_settings
            .get("custom_status")
            .and_then(Value::as_object);
        let emoji_id = prior
            .and_then(|v| v.get("emoji_id"))
            .filter(|v| !v.is_null())
            .cloned();
        let emoji_name = prior
            .and_then(|v| v.get("emoji_name"))
            .filter(|v| !v.is_null())
            .cloned();
        let custom = if text.is_empty() && emoji_id.is_none() && emoji_name.is_none() {
            Value::Null
        } else {
            let mut value = json!({"text":if text.is_empty(){Value::Null}else{json!(text)},"expires_at":prior.and_then(|v|v.get("expires_at")).cloned().unwrap_or(Value::Null)});
            if let Some(id) = emoji_id {
                value["emoji_id"] = id;
            } else if let Some(name) = emoji_name {
                value["emoji_name"] = name;
            }
            value
        };
        self.update_presence_settings(json!({"custom_status":custom}))
            .await
    }
    async fn update_presence_settings(&mut self, patch: Value) -> Result<()> {
        if self.state.current_user.is_none() {
            return Err("Sign in first".into());
        }
        if self.demo {
            let settings = self
                .state
                .user_settings
                .as_object_mut()
                .ok_or("Settings are still loading")?;
            if let Some(updates) = patch.as_object() {
                settings.extend(updates.clone());
            }
            self.snapshot();
            self.emit(Event::PresenceMutation(
                RelationshipMutationStatus::Complete,
            ));
            return Ok(());
        }
        if self.presence.job.is_some() {
            return Err("A status update is already pending".into());
        }
        let client = self.client.clone().ok_or("Not signed in")?;
        let Some(updates) = patch.as_object() else {
            return Err("Invalid settings".into());
        };
        let before_presence = self.presence_payload();
        let settings = self
            .state
            .user_settings
            .as_object_mut()
            .ok_or("Settings are still loading")?;
        let mut previous = BTreeMap::new();
        for (key, value) in updates {
            previous.insert(key.clone(), settings.get(key).cloned());
            settings.insert(key.clone(), value.clone());
        }
        self.presence.next = self.presence.next.wrapping_add(1);
        let generation = self.presence.next;
        if self.presence_payload() != before_presence {
            self.publish_presence().await;
        }
        self.snapshot();
        self.emit(Event::PresenceMutation(RelationshipMutationStatus::Pending));
        let tx = self.presence.tx.clone();
        let request = patch.clone();
        let task = tokio::spawn(async move {
            let result = match tokio::time::timeout(
                Duration::from_secs(15),
                client.update_settings(&request),
            )
            .await
            {
                Ok(result) => result.map(|_| ()).map_err(error),
                Err(_) => Err("Status update timed out".into()),
            };
            let _ = tx.send(PresenceEvent { generation, result });
        });
        self.presence.job = Some(Job {
            generation,
            task,
            previous,
            patch,
        });
        Ok(())
    }
    pub(super) async fn presence_event(&mut self, event: PresenceEvent) {
        if self
            .presence
            .job
            .as_ref()
            .is_none_or(|job| job.generation != event.generation)
        {
            return;
        }
        let job = self.presence.job.take().unwrap();
        match event.result {
            Ok(()) => self.emit(Event::PresenceMutation(
                RelationshipMutationStatus::Complete,
            )),
            Err(message) => {
                let before_presence = self.presence_payload();
                if let Some(settings) = self.state.user_settings.as_object_mut() {
                    for (key, previous) in &job.previous {
                        if settings.get(key) == job.patch.get(key) {
                            match previous {
                                Some(value) => {
                                    settings.insert(key.clone(), value.clone());
                                }
                                None => {
                                    settings.remove(key);
                                }
                            }
                        }
                    }
                }
                if self.presence_payload() != before_presence {
                    self.publish_presence().await;
                }
                self.snapshot();
                self.emit(Event::PresenceMutation(RelationshipMutationStatus::Failed(
                    message,
                )));
            }
        }
    }
}
