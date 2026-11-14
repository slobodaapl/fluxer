use super::*;
use crate::{ProfileSnapshot, ProfileStatus};
use fluxer_core::{ProfileDetails, UserProfile};
use tokio::time::Instant;
type Key = (Snowflake, Option<Snowflake>);
struct Job {
    token: u64,
    task: JoinHandle<()>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(super) struct ProfileEvent {
    epoch: u64,
    key: Key,
    token: u64,
    result: Result<Value>,
}
pub(super) struct ProfileState {
    epoch: u64,
    next: u64,
    cache: BTreeMap<Key, (Instant, UserProfile)>,
    jobs: BTreeMap<Key, Job>,
    tx: async_mpsc::UnboundedSender<ProfileEvent>,
    pub rx: async_mpsc::UnboundedReceiver<ProfileEvent>,
}
impl ProfileState {
    pub fn new() -> Self {
        let (tx, rx) = async_mpsc::unbounded_channel();
        Self {
            epoch: 0,
            next: 0,
            cache: BTreeMap::new(),
            jobs: BTreeMap::new(),
            tx,
            rx,
        }
    }
}
impl Worker {
    pub(super) fn clear_profiles(&mut self) {
        self.profiles.epoch = self.profiles.epoch.wrapping_add(1);
        self.profiles.jobs.clear();
        self.profiles.cache.clear();
    }
    pub(super) fn invalidate_profiles(&mut self) {
        self.profiles.epoch = self.profiles.epoch.wrapping_add(1);
        let keys: Vec<_> = self.profiles.jobs.keys().copied().collect();
        self.profiles.jobs.clear();
        self.profiles.cache.clear();
        for key in keys {
            self.profile_snapshot(
                key,
                ProfileStatus::Failed("Profile changed; retry to refresh".into()),
                None,
            );
        }
    }
    fn profile_snapshot(&self, key: Key, status: ProfileStatus, profile: Option<UserProfile>) {
        self.emit(Event::UserProfile(ProfileSnapshot {
            user_id: key.0,
            guild_id: key.1,
            status,
            profile,
        }));
    }
    pub(super) fn load_user_profile(
        &mut self,
        user_id: Snowflake,
        guild_id: Option<Snowflake>,
        force: bool,
    ) {
        let key = (user_id, guild_id);
        if !force
            && let Some((at, profile)) = self.profiles.cache.get(&key)
            && at.elapsed() < Duration::from_secs(60)
        {
            self.profile_snapshot(key, ProfileStatus::Ready, Some(profile.clone()));
            return;
        }
        if !force && self.profiles.jobs.contains_key(&key) {
            self.profile_snapshot(key, ProfileStatus::Loading, None);
            return;
        }
        self.profiles.jobs.remove(&key);
        self.profile_snapshot(key, ProfileStatus::Loading, None);
        if self.demo {
            let Some(user) = self
                .state
                .users
                .get(&user_id)
                .cloned()
                .or_else(|| self.state.current_user.clone().filter(|u| u.id == user_id))
            else {
                self.profile_snapshot(key, ProfileStatus::Failed("Unknown user".into()), None);
                return;
            };
            let profile = UserProfile {
                user,
                user_profile: ProfileDetails {
                    bio: Some("Exploring Fluxer together.".into()),
                    pronouns: Some("they/them".into()),
                    accent_color: Some(0x5865f2),
                    ..Default::default()
                },
                guild_member: guild_id
                    .and_then(|id| self.state.members.get(&(id, user_id)).cloned()),
                ..Default::default()
            };
            self.profiles
                .cache
                .insert(key, (Instant::now(), profile.clone()));
            self.profile_snapshot(key, ProfileStatus::Ready, Some(profile));
            return;
        }
        let Some(client) = self.client.clone() else {
            self.profile_snapshot(key, ProfileStatus::Failed("Sign in first".into()), None);
            return;
        };
        self.profiles.next = self.profiles.next.wrapping_add(1);
        let token = self.profiles.next;
        let epoch = self.profiles.epoch;
        let tx = self.profiles.tx.clone();
        let path = format!(
            "/users/{user_id}/profile?{}with_mutual_friends=true&with_mutual_guilds=true",
            guild_id
                .map(|id| format!("guild_id={id}&"))
                .unwrap_or_default()
        );
        let task = tokio::spawn(async move {
            let result = client
                .request(reqwest::Method::GET, &path, None)
                .await
                .map_err(error);
            let _ = tx.send(ProfileEvent {
                epoch,
                key,
                token,
                result,
            });
        });
        self.profiles.jobs.insert(key, Job { token, task });
    }
    pub(super) fn profile_event(&mut self, event: ProfileEvent) {
        if event.epoch != self.profiles.epoch
            || !self
                .profiles
                .jobs
                .get(&event.key)
                .is_some_and(|job| job.token == event.token)
        {
            return;
        }
        self.profiles.jobs.remove(&event.key);
        let result = event.result.and_then(|mut wire| {
            let user = wire.get_mut("user").ok_or("Profile response has no user")?;
            let incoming_id = user
                .get("id")
                .and_then(Value::as_str)
                .ok_or("Profile response has no user id")?;
            if incoming_id != event.key.0.to_string() {
                return Err("Profile response user does not match request".into());
            }
            if let Some(existing) = self.state.users.get(&event.key.0) {
                merge_missing(user, serde_json::to_value(existing).map_err(error)?);
            }
            let user = wire["user"].clone();
            if let Some(member) = wire.get_mut("guild_member").filter(|v| v.is_object()) {
                if let Some(existing) = event
                    .key
                    .1
                    .and_then(|guild| self.state.members.get(&(guild, event.key.0)))
                {
                    merge_missing(member, serde_json::to_value(existing).map_err(error)?);
                }
                if member.get("user").is_none() {
                    member["user"] = user;
                }
            }
            serde_json::from_value::<UserProfile>(wire).map_err(error)
        });
        match result {
            Ok(profile) => {
                self.profiles
                    .cache
                    .insert(event.key, (Instant::now(), profile.clone()));
                self.profile_snapshot(event.key, ProfileStatus::Ready, Some(profile));
            }
            Err(message) => self.profile_snapshot(event.key, ProfileStatus::Failed(message), None),
        }
    }
}
fn merge_missing(target: &mut Value, defaults: Value) {
    if let (Some(target), Value::Object(defaults)) = (target.as_object_mut(), defaults) {
        for (key, value) in defaults {
            if let Some(existing) = target.get_mut(&key) {
                if existing.is_object() && value.is_object() {
                    merge_missing(existing, value);
                }
            } else {
                target.insert(key, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    #[tokio::test]
    async fn profile_fetch_is_scoped_deduplicated_and_preserves_partial_user() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut bytes = [0; 4096];
            let n = socket.read(&mut bytes).unwrap();
            let request = String::from_utf8_lossy(&bytes[..n]).to_string();
            let body = r#"{"user":{"id":"42","username":"new"},"user_profile":{"bio":"hello","banner":null,"pronouns":"they/them","accent_color":42},"timezone_offset":null,"guild_member":{"roles":["9"],"joined_at":"2020"}}"#;
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            request
        });
        let (tx, rx) = mpsc::channel();
        let mut worker = Worker::new(tx, Arc::new(Mutex::new(BTreeMap::new())));
        let instance = Instance::from_endpoints(
            &format!("http://{address}/api"),
            &format!("ws://{address}/gateway"),
        )
        .unwrap();
        worker.client = Some(Client::new(instance).unwrap());
        worker.state.users.insert(
            42.into(),
            User {
                id: 42.into(),
                username: "old".into(),
                avatar: Some("avatar".into()),
                ..Default::default()
            },
        );
        worker.load_user_profile(42.into(), Some(8.into()), false);
        worker.load_user_profile(42.into(), Some(8.into()), false);
        assert_eq!(worker.profiles.jobs.len(), 1);
        let event = tokio::time::timeout(Duration::from_secs(5), worker.profiles.rx.recv())
            .await
            .unwrap()
            .unwrap();
        worker.profile_event(event);
        let request = server.join().unwrap();
        assert!(request.starts_with("GET /api/v1/users/42/profile?guild_id=8&with_mutual_friends=true&with_mutual_guilds=true "), "{request}");
        let profile = &worker.profiles.cache[&(42.into(), Some(8.into()))].1;
        assert_eq!(profile.user.username, "new");
        assert_eq!(profile.user.avatar.as_deref(), Some("avatar"));
        assert_eq!(profile.guild_member.as_ref().unwrap().roles, vec![9.into()]);
        assert_eq!(
            profile.guild_member.as_ref().unwrap().extra["joined_at"],
            "2020"
        );
        worker.load_user_profile(42.into(), Some(8.into()), false);
        assert!(worker.profiles.jobs.is_empty());
        assert!(rx.try_iter().any(|event| matches!(
            event,
            Event::UserProfile(ProfileSnapshot {
                status: ProfileStatus::Ready,
                ..
            })
        )));
        worker.invalidate_profiles();
        assert!(worker.profiles.cache.is_empty());
    }
    #[test]
    fn merge_preserves_explicit_nulls_and_unknown_fields() {
        let mut incoming = json!({"avatar": null, "nested": {"new": 1}});
        merge_missing(&mut incoming, json!({"avatar":"old", "nested":{"old":2}}));
        assert_eq!(incoming, json!({"avatar":null,"nested":{"new":1,"old":2}}));
    }
}
