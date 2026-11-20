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
pub(super) struct PeopleEvent {
    role: Option<(Snowflake, Snowflake, bool)>,
    user: Snowflake,
    generation: u64,
    result: Result<()>,
}
pub(super) struct PeopleState {
    next: u64,
    jobs: BTreeMap<Snowflake, Job>,
    role_jobs: BTreeMap<(Snowflake, Snowflake), Job>,
    tx: async_mpsc::UnboundedSender<PeopleEvent>,
    pub rx: async_mpsc::UnboundedReceiver<PeopleEvent>,
}
impl PeopleState {
    pub fn new() -> Self {
        let (tx, rx) = async_mpsc::unbounded_channel();
        Self {
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
        self.people
            .role_jobs
            .insert((guild, user), Job { generation, task });
        Ok(())
    }
    pub(super) fn people_event(&mut self, event: PeopleEvent) {
        if let Some((guild, role, add)) = event.role {
            let key = (guild, event.user);
            if self
                .people
                .role_jobs
                .get(&key)
                .is_none_or(|job| job.generation != event.generation)
            {
                return;
            }
            self.people.role_jobs.remove(&key);
            if event.result.is_ok() {
                if let Some(member) = self.state.members.get_mut(&key) {
                    member.roles.retain(|id| *id != role);
                    if add {
                        member.roles.push(role);
                    }
                }
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
