use super::*;
use crate::RelationshipMutationStatus as Status;
use base64::{Engine, engine::general_purpose::STANDARD};

struct Job {
    generation: u64,
    avatar: bool,
    account: Snowflake,
    task: JoinHandle<()>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(super) struct OwnProfileEvent {
    generation: u64,
    result: Result<Value>,
}
pub(super) struct OwnProfileState {
    next: u64,
    job: Option<Job>,
    tx: async_mpsc::UnboundedSender<OwnProfileEvent>,
    pub rx: async_mpsc::UnboundedReceiver<OwnProfileEvent>,
}
impl OwnProfileState {
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
fn patch(global_name: String, bio: String, pronouns: String) -> Value {
    json!({
        "global_name": if global_name.is_empty() { None } else { Some(global_name) },
        "bio": if bio.is_empty() { None } else { Some(bio) },
        "pronouns": if pronouns.is_empty() { None } else { Some(pronouns) },
    })
}
impl Worker {
    pub(super) fn clear_own_profile(&mut self) {
        self.own_profile.next = self.own_profile.next.wrapping_add(1);
        if let Some(job) = self.own_profile.job.take() {
            self.own_profile_status(job.avatar, Status::Failed("Profile save cancelled".into()));
        }
    }
    pub(super) fn cancel_own_avatar(&mut self) {
        if self.own_profile.job.as_ref().is_some_and(|job| job.avatar) {
            self.clear_own_profile();
        }
    }
    pub(super) fn update_own_profile(
        &mut self,
        name: String,
        bio: String,
        pronouns: String,
    ) -> Result<()> {
        self.update_own_profile_patch(patch(name, bio, pronouns), false)
    }
    pub(super) fn update_own_avatar(&mut self, png: Option<Vec<u8>>) -> Result<()> {
        if png.as_ref().is_some_and(|data| {
            data.len() > 10 * 1024 * 1024 || !data.starts_with(b"\x89PNG\r\n\x1a\n")
        }) {
            return Err("Invalid or oversized avatar image".into());
        }
        let avatar = png.map(|data| format!("data:image/png;base64,{}", STANDARD.encode(data)));
        self.update_own_profile_patch(json!({"avatar": avatar}), true)
    }
    fn own_profile_status(&self, avatar: bool, status: Status) {
        self.emit(if avatar {
            Event::OwnAvatarMutation(status)
        } else {
            Event::OwnProfileMutation(status)
        });
    }
    fn update_own_profile_patch(&mut self, patch: Value, avatar: bool) -> Result<()> {
        let current = self.state.current_user.clone().ok_or("Sign in first")?;
        if self.own_profile.job.is_some() {
            return Err("A profile save is already pending".into());
        }
        if self.demo {
            self.project_own_profile(current.id, patch)?;
            self.own_profile_status(avatar, Status::Complete);
            return Ok(());
        }
        let client = self.client.clone().ok_or("Not signed in")?;
        self.own_profile.next = self.own_profile.next.wrapping_add(1);
        let generation = self.own_profile.next;
        let tx = self.own_profile.tx.clone();
        let task = tokio::spawn(async move {
            let result = match tokio::time::timeout(
                Duration::from_secs(15),
                client.request(reqwest::Method::PATCH, "/users/@me", Some(&patch)),
            )
            .await
            {
                Ok(result) => result.map_err(error),
                Err(_) => Err("Profile update timed out".into()),
            };
            let _ = tx.send(OwnProfileEvent { generation, result });
        });
        self.own_profile.job = Some(Job {
            generation,
            avatar,
            account: current.id,
            task,
        });
        self.own_profile_status(avatar, Status::Pending);
        Ok(())
    }
    pub(super) fn own_profile_event(&mut self, event: OwnProfileEvent) {
        let Some(job) = self.own_profile.job.as_ref() else {
            return;
        };
        if job.generation != event.generation {
            return;
        }
        let account = job.account;
        let avatar = job.avatar;
        self.own_profile.job.take();
        let result = event
            .result
            .and_then(|response| self.project_own_profile(account, response));
        self.own_profile_status(
            avatar,
            match result {
                Ok(()) => Status::Complete,
                Err(message) => Status::Failed(message),
            },
        );
    }
    fn project_own_profile(&mut self, account: Snowflake, response: Value) -> Result<()> {
        let current = self
            .state
            .current_user
            .as_ref()
            .filter(|user| user.id == account)
            .ok_or("Profile account changed")?;
        let mut merged = serde_json::to_value(current).map_err(error)?;
        let updates = response.as_object().ok_or("Invalid profile response")?;
        merged.as_object_mut().unwrap().extend(updates.clone());
        let user: User = serde_json::from_value(merged).map_err(error)?;
        if user.id != account {
            return Err("Profile response user does not match account".into());
        }
        self.state.users.insert(user.id, user.clone());
        self.state.current_user = Some(user);
        self.invalidate_profiles();
        self.snapshot();
        Ok(())
    }
}
