// SPDX-License-Identifier: AGPL-3.0-or-later
//! Validated, account-scoped display preference updates, independent of presence.
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
pub(super) struct DisplaySettingsEvent {
    generation: u64,
    result: Result<()>,
}
pub(super) struct DisplaySettingsState {
    next: u64,
    job: Option<Job>,
    tx: async_mpsc::UnboundedSender<DisplaySettingsEvent>,
    pub rx: async_mpsc::UnboundedReceiver<DisplaySettingsEvent>,
}
impl DisplaySettingsState {
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
    pub(super) fn clear_display_settings(&mut self) {
        self.display_settings.next = self.display_settings.next.wrapping_add(1);
        let status = if self.display_settings.job.take().is_some() {
            RelationshipMutationStatus::Failed("The account changed while saving settings".into())
        } else {
            RelationshipMutationStatus::Complete
        };
        self.emit(Event::DisplaySettingsMutation(status));
    }
    pub(super) async fn set_display_setting(
        &mut self,
        setting: fluxer_core::DisplaySetting,
    ) -> Result<()> {
        let patch = fluxer_core::display_settings_patch(&self.state.user_settings, setting)?;
        if patch.as_object().is_some_and(serde_json::Map::is_empty) {
            self.emit(Event::DisplaySettingsMutation(
                RelationshipMutationStatus::Complete,
            ));
            return Ok(());
        }
        self.update_display_settings_settings(patch).await
    }
    async fn update_display_settings_settings(&mut self, patch: Value) -> Result<()> {
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
            self.emit(Event::DisplaySettingsMutation(
                RelationshipMutationStatus::Complete,
            ));
            return Ok(());
        }
        if self.display_settings.job.is_some() {
            return Err("A settings update is already pending".into());
        }
        let client = self.client.clone().ok_or("Not signed in")?;
        let Some(updates) = patch.as_object() else {
            return Err("Invalid settings".into());
        };
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
        self.display_settings.next = self.display_settings.next.wrapping_add(1);
        let generation = self.display_settings.next;
        self.snapshot();
        self.emit(Event::DisplaySettingsMutation(
            RelationshipMutationStatus::Pending,
        ));
        let tx = self.display_settings.tx.clone();
        let request = patch.clone();
        let task = tokio::spawn(async move {
            let result = match tokio::time::timeout(
                Duration::from_secs(15),
                client.update_settings(&request),
            )
            .await
            {
                Ok(result) => result.map(|_| ()).map_err(error),
                Err(_) => Err("Settings update timed out".into()),
            };
            let _ = tx.send(DisplaySettingsEvent { generation, result });
        });
        self.display_settings.job = Some(Job {
            generation,
            task,
            previous,
            patch,
        });
        Ok(())
    }
    pub(super) async fn display_settings_event(&mut self, event: DisplaySettingsEvent) {
        if self
            .display_settings
            .job
            .as_ref()
            .is_none_or(|job| job.generation != event.generation)
        {
            return;
        }
        let job = self.display_settings.job.take().unwrap();
        match event.result {
            Ok(()) => self.emit(Event::DisplaySettingsMutation(
                RelationshipMutationStatus::Complete,
            )),
            Err(message) => {
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
                self.snapshot();
                self.emit(Event::DisplaySettingsMutation(
                    RelationshipMutationStatus::Failed(message),
                ));
            }
        }
    }
}
