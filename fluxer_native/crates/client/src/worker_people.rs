//! Relationship writes from RelationshipCommands.ts; gateway events remain authoritative.
use super::*;
use crate::{RelationshipAction, RelationshipMutationStatus};
use fluxer_protocol::Method;

struct Job {
    generation: u64,
    task: JoinHandle<()>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(super) struct KickEvent {
    guild: Snowflake,
    user: Snowflake,
    generation: u64,
    result: Result<()>,
    timeout: bool,
}

struct RoleJob {
    job: Job,
    original_roles: Vec<Snowflake>,
}

pub(super) struct PeopleEvent {
    role: Option<(Snowflake, Snowflake, bool)>,
    user: Snowflake,
    generation: u64,
    result: Result<()>,
}
pub(super) struct PeopleState {
    next: u64,
    jobs: BTreeMap<Snowflake, Job>,
    role_jobs: BTreeMap<(Snowflake, Snowflake), RoleJob>,
    tx: async_mpsc::UnboundedSender<PeopleEvent>,
    pub rx: async_mpsc::UnboundedReceiver<PeopleEvent>,
    kicks: BTreeMap<(Snowflake, Snowflake), Job>,
    kick_tx: async_mpsc::UnboundedSender<KickEvent>,
    pub kick_rx: async_mpsc::UnboundedReceiver<KickEvent>,
}
impl PeopleState {
    pub fn new() -> Self {
        let (tx, rx) = async_mpsc::unbounded_channel();
        let (kick_tx, kick_rx) = async_mpsc::unbounded_channel();
        Self {
            kicks: BTreeMap::new(),
            kick_tx,
            kick_rx,
            next: 0,
            jobs: BTreeMap::new(),
            role_jobs: BTreeMap::new(),
            tx,
            rx,
        }
    }
}
impl Worker {
    pub(super) fn clear_people(&mut self) {
        self.people.next = self.people.next.wrapping_add(1);
        self.people.jobs.clear();
        self.people.role_jobs.clear();
        self.people.kicks.clear();
    }
    pub(super) fn kick_member(&mut self, guild: Snowflake, user: Snowflake) -> Result<()> {
        let actor = self.state.current_user.as_ref().ok_or("Not signed in")?.id;
        let guild_data = self
            .state
            .guilds
            .get(&guild)
            .ok_or("Community no longer available")?;
        let actor_member = self
            .state
            .members
            .get(&(guild, actor))
            .ok_or("Your member information is unavailable")?;
        let target = self
            .state
            .members
            .get(&(guild, user))
            .ok_or("Member no longer available")?;
        if !fluxer_core::moderation_capabilities(guild_data, actor_member, target).kick {
            return Err("You cannot kick this member".into());
        }
        if self.people.kicks.contains_key(&(guild, user)) {
            return Err("A kick is already pending".into());
        }
        if self.demo {
            return Err("Kicks are disabled in the offline demo".into());
        }
        let client = self.client.clone().ok_or("Not signed in")?;
        self.people.next = self.people.next.wrapping_add(1);
        let generation = self.people.next;
        let tx = self.people.kick_tx.clone();
        let task = tokio::spawn(async move {
            let result = match tokio::time::timeout(
                Duration::from_secs(15),
                client.kick_member(&guild.to_string(), &user.to_string()),
            )
            .await
            {
                Ok(result) => result.map_err(error),
                Err(_) => Err("Kick timed out".into()),
            };
            let _ = tx.send(KickEvent {
                guild,
                user,
                generation,
                result,
                timeout: false,
            });
        });
        self.people
            .kicks
            .insert((guild, user), Job { generation, task });
        self.emit(Event::Status("Kicking member…".into()));
        Ok(())
    }
    pub(super) fn timeout_member(
        &mut self,
        guild: Snowflake,
        user: Snowflake,
        duration: Option<u64>,
    ) -> Result<()> {
        if duration.is_some_and(|d| ![60, 300, 600, 3600, 86400, 604800].contains(&d)) {
            return Err("Invalid timeout duration".into());
        }
        let actor = self.state.current_user.as_ref().ok_or("Not signed in")?.id;
        let guild_data = self
            .state
            .guilds
            .get(&guild)
            .ok_or("Community no longer available")?;
        let actor_member = self
            .state
            .members
            .get(&(guild, actor))
            .ok_or("Your member information is unavailable")?;
        let target = self
            .state
            .members
            .get(&(guild, user))
            .ok_or("Member no longer available")?;
        if !fluxer_core::moderation_capabilities(guild_data, actor_member, target).timeout {
            return Err("You cannot time out this member".into());
        }
        if self.people.kicks.contains_key(&(guild, user)) {
            return Err("A moderation action is already pending".into());
        }
        if self.demo {
            return Err("Timeouts are disabled in the offline demo".into());
        }
        let client = self.client.clone().ok_or("Not signed in")?;
        self.people.next = self.people.next.wrapping_add(1);
        let generation = self.people.next;
        let tx = self.people.kick_tx.clone();
        let task = tokio::spawn(async move {
            let until = duration.map(|seconds| {
                (chrono::Utc::now() + chrono::Duration::seconds(seconds as i64))
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            });
            let result = match tokio::time::timeout(
                Duration::from_secs(15),
                client.set_member_timeout(&guild.to_string(), &user.to_string(), until.as_deref()),
            )
            .await
            {
                Ok(result) => result.map(|_| ()).map_err(error),
                Err(_) => Err("Timeout update timed out".into()),
            };
            let _ = tx.send(KickEvent {
                guild,
                user,
                generation,
                result,
                timeout: true,
            });
        });
        self.people
            .kicks
            .insert((guild, user), Job { generation, task });
        self.emit(Event::Status("Updating member timeout…".into()));
        Ok(())
    }
    pub(super) fn kick_event(&mut self, event: KickEvent) {
        if self
            .people
            .kicks
            .get(&(event.guild, event.user))
            .is_none_or(|job| job.generation != event.generation)
        {
            return;
        }
        self.people.kicks.remove(&(event.guild, event.user));
        let action = if event.timeout {
            "update timeout"
        } else {
            "kick member"
        };
        match event.result {
            Ok(()) => self.emit(Event::Status(
                if event.timeout {
                    "Member timeout updated"
                } else {
                    "Member kicked"
                }
                .into(),
            )),
            Err(message) => self.emit(Event::Error(format!("Could not {action}: {message}"))),
        }
    }
    pub(super) fn change_relationship(
        &mut self,
        user: Snowflake,
        action: RelationshipAction,
    ) -> Result<()> {
        if self
            .state
            .current_user
            .as_ref()
            .is_none_or(|current| current.id == user)
        {
            return Err("Cannot change your own relationship".into());
        }
        if self.people.jobs.contains_key(&user) {
            return Err("A relationship update is already pending".into());
        }
        let kind = self.state.relationships.get(&user).map(|r| r.kind);
        let valid = match action {
            RelationshipAction::Send => kind.is_none(),
            RelationshipAction::Accept => kind == Some(3),
            RelationshipAction::Remove => kind.is_some(),
            RelationshipAction::Block => kind != Some(2),
        };
        if !valid {
            return Err("The relationship has changed".into());
        }
        if self.demo {
            return Err("Relationship changes are disabled in the offline demo".into());
        }
        let client = self.client.clone().ok_or("Not signed in")?;
        let (method, body) = match action {
            RelationshipAction::Send => (Method::POST, Some(json!({}))),
            RelationshipAction::Accept => (Method::PUT, None),
            RelationshipAction::Remove => (Method::DELETE, None),
            RelationshipAction::Block => (Method::PUT, Some(json!({"type":2}))),
        };
        self.people.next = self.people.next.wrapping_add(1);
        let generation = self.people.next;
        let tx = self.people.tx.clone();
        let task = tokio::spawn(async move {
            let path = format!("/users/@me/relationships/{user}");
            let result = match tokio::time::timeout(
                Duration::from_secs(15),
                client.request(method, &path, body.as_ref()),
            )
            .await
            {
                Ok(result) => result.map(|_| ()).map_err(error),
                Err(_) => Err("Relationship update timed out".into()),
            };
            let _ = tx.send(PeopleEvent {
                role: None,
                user,
                generation,
                result,
            });
        });
        self.people.jobs.insert(user, Job { generation, task });
        self.emit(Event::RelationshipMutation {
            user_id: user,
            status: RelationshipMutationStatus::Pending,
        });
        Ok(())
    }
    pub(super) fn set_member_role(
        &mut self,
        guild: Snowflake,
        user: Snowflake,
        role: Snowflake,
        add: bool,
    ) -> Result<()> {
        let allowed = self
            .state
            .guilds
            .get(&guild)
            .and_then(|g| {
                let current = self.state.current_user.as_ref()?;
                let actor = self.state.members.get(&(guild, current.id))?;
                let role = g.roles.iter().find(|r| r.id == role)?;
                self.state.members.get(&(guild, user))?;
                Some(fluxer_core::can_assign_role(g, actor, role))
            })
            .unwrap_or(false);
        if !allowed {
            return Err("You cannot manage this role".into());
        }
        if self.demo {
            return Err("Role changes are disabled in the offline demo".into());
        }
        if self.people.role_jobs.contains_key(&(guild, user)) {
            return Err("A role update is already pending".into());
        }
        let client = self.client.clone().ok_or("Not signed in")?;
        let original_roles = self.state.members[&(guild, user)].roles.clone();
        self.people.next = self.people.next.wrapping_add(1);
        let generation = self.people.next;
        let tx = self.people.tx.clone();
        let task = tokio::spawn(async move {
            let path = format!("/guilds/{guild}/members/{user}/roles/{role}");
            let result = match tokio::time::timeout(
                Duration::from_secs(15),
                client.request(if add { Method::PUT } else { Method::DELETE }, &path, None),
            )
            .await
            {
                Ok(result) => result.map(|_| ()).map_err(error),
                Err(_) => Err("Role update timed out".into()),
            };
            let _ = tx.send(PeopleEvent {
                role: Some((guild, role, add)),
                user,
                generation,
                result,
            });
        });
        self.people.role_jobs.insert(
            (guild, user),
            RoleJob {
                job: Job { generation, task },
                original_roles,
            },
        );
        Ok(())
    }
    pub(super) fn people_event(&mut self, event: PeopleEvent) {
        if let Some((guild, role, add)) = event.role {
            let key = (guild, event.user);
            if self
                .people
                .role_jobs
                .get(&key)
                .is_none_or(|job| job.job.generation != event.generation)
            {
                return;
            }
            let job = self.people.role_jobs.remove(&key).unwrap();
            if event.result.is_ok()
                && let Some(member) = self.state.members.get_mut(&key)
            {
                project_role_if_unchanged(&mut member.roles, &job.original_roles, role, add);
            }
            self.emit(Event::MemberRoleMutation {
                guild_id: guild,
                user_id: event.user,
                role_id: role,
                add,
                error: event.result.err(),
            });
            return;
        }

        if self
            .people
            .jobs
            .get(&event.user)
            .is_none_or(|job| job.generation != event.generation)
        {
            return;
        }
        self.people.jobs.remove(&event.user);
        let status = match event.result {
            Ok(()) => RelationshipMutationStatus::Complete,
            Err(message) => RelationshipMutationStatus::Failed(message),
        };
        self.emit(Event::RelationshipMutation {
            user_id: event.user,
            status,
        });
    }
}

// Gateway member state may have changed while the request was in flight.
// Only project the response over the exact role list it was based on.
fn project_role_if_unchanged(
    roles: &mut Vec<Snowflake>,
    original: &[Snowflake],
    role: Snowflake,
    add: bool,
) {
    if roles != original {
        return;
    }
    roles.retain(|id| *id != role);
    if add {
        roles.push(role);
    }
}

#[cfg(test)]
mod role_projection_tests {
    use super::*;

    #[test]
    fn response_does_not_overwrite_newer_member_roles() {
        let original = vec![1.into()];
        let mut unchanged = original.clone();
        project_role_if_unchanged(&mut unchanged, &original, 2.into(), true);
        assert_eq!(unchanged, vec![1.into(), 2.into()]);

        let mut updated = vec![1.into(), 3.into()];
        project_role_if_unchanged(&mut updated, &original, 2.into(), true);
        assert_eq!(updated, vec![1.into(), 3.into()]);
    }
}
