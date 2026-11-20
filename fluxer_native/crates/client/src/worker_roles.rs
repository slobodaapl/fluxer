//! Role writes are projected only by gateway events, never by HTTP responses.
use super::*;
use fluxer_protocol::Method;

struct Job {
    token: u64,
    task: JoinHandle<()>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.task.abort();
    }
}
pub(super) struct RoleEvent {
    epoch: u64,
    token: u64,
    guild: Snowflake,
    result: Result<()>,
}
pub(super) struct RoleState {
    epoch: u64,
    next: u64,
    job: Option<Job>,
    tx: async_mpsc::UnboundedSender<RoleEvent>,
    pub rx: async_mpsc::UnboundedReceiver<RoleEvent>,
}
impl RoleState {
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
    pub(super) fn clear_roles(&mut self) {
        self.roles.epoch = self.roles.epoch.wrapping_add(1);
        self.roles.job = None;
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn manage_role(
        &mut self,
        guild: Snowflake,
        role: Option<Snowflake>,
        original: Option<fluxer_core::Role>,
        name: String,
        color: u32,
        permissions: u64,
        hoist: bool,
        mentionable: bool,
        delete: bool,
    ) {
        let failure = |this: &Self, message: &str| {
            this.emit(Event::RoleMutation {
                guild_id: guild,
                error: Some(message.into()),
            })
        };
        let Some(actor) = self.state.current_user.as_ref().map(|u| u.id) else {
            return failure(self, "Not signed in");
        };
        let Some(g) = self.state.guilds.get(&guild) else {
            return failure(self, "Community no longer available");
        };
        let Some(member) = self.state.members.get(&(guild, actor)) else {
            return failure(self, "Your member information is unavailable");
        };
        if !self
            .state
            .guild_permissions_for(guild, actor)
            .contains(Permissions::MANAGE_ROLES)
        {
            return failure(self, "You need Manage Roles permission");
        }
        if let Some(id) = role {
            let Some(target) = g.roles.iter().find(|r| r.id == id) else {
                return failure(self, "Role no longer available");
            };
            if original.as_ref() != Some(target) {
                return failure(self, "This role changed. Reload it before saving");
            }
            if (id == guild && delete)
                || (id != guild && !fluxer_core::can_assign_role(g, member, target))
                || (id == guild
                    && actor != g.owner_id
                    && !g
                        .roles
                        .iter()
                        .filter(|r| member.roles.contains(&r.id))
                        .any(|r| {
                            fluxer_core::role_is_higher(
                                r.position,
                                r.id,
                                target.position,
                                target.id,
                            )
                        }))
            {
                return failure(self, "You cannot manage this role");
            }
        } else if delete {
            return failure(self, "Role no longer available");
        }
        let name = name.trim();
        if !delete && (name.is_empty() || name.chars().count() > 100 || color > 0xffffff) {
            return failure(
                self,
                "Role name must be 1–100 characters and color must be valid",
            );
        }
        if self.demo {
            return failure(self, "Role management is unavailable in the offline demo");
        }
        if self.roles.job.is_some() {
            return failure(self, "A role update is already pending");
        }
        let Some(client) = self.client.clone() else {
            return failure(self, "Not signed in");
        };
        let (method, path, body) = if delete {
            (
                Method::DELETE,
                format!("/guilds/{guild}/roles/{}", role.unwrap()),
                None,
            )
        } else if let Some(id) = role {
            let original = original.as_ref().expect("validated role snapshot");
            let mut patch = serde_json::Map::new();
            if name != original.name {
                patch.insert("name".into(), json!(name));
            }
            if color != original.color {
                patch.insert("color".into(), json!(color));
            }
            if permissions != original.permissions.0 {
                patch.insert("permissions".into(), json!(permissions.to_string()));
            }
            if hoist != original.hoist {
                patch.insert("hoist".into(), json!(hoist));
            }
            if mentionable != original.mentionable {
                patch.insert("mentionable".into(), json!(mentionable));
            }
            if patch.is_empty() {
                self.emit(Event::RoleMutation {
                    guild_id: guild,
                    error: None,
                });
                return;
            }
            (
                Method::PATCH,
                format!("/guilds/{guild}/roles/{id}"),
                Some(Value::Object(patch)),
            )
        } else {
            (
                Method::POST,
                format!("/guilds/{guild}/roles"),
                Some(json!({"name":name,"color":color,"permissions":permissions.to_string()})),
            )
        };
        self.roles.next = self.roles.next.wrapping_add(1);
        let token = self.roles.next;
        let epoch = self.roles.epoch;
        let tx = self.roles.tx.clone();
        let task = tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(15),
                client.request(method, &path, body.as_ref()),
            )
            .await
            .map_err(|_| "Role update timed out".to_owned())
            .and_then(|r| r.map(|_| ()).map_err(error));
            let _ = tx.send(RoleEvent {
                epoch,
                token,
                guild,
                result,
            });
        });
        self.roles.job = Some(Job { token, task });
    }
    pub(super) fn role_event(&mut self, event: RoleEvent) {
        if event.epoch != self.roles.epoch
            || !self
                .roles
                .job
                .as_ref()
                .is_some_and(|j| j.token == event.token)
        {
            return;
        }
        self.roles.job = None;
        self.emit(Event::RoleMutation {
            guild_id: event.guild,
            error: event.result.err(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cleared_account_discards_role_response() {
        let (tx, rx) = mpsc::channel();
        let mut worker = Worker::new(tx, Arc::new(Mutex::new(BTreeMap::new())));
        let epoch = worker.roles.epoch;
        worker.roles.job = Some(Job {
            token: 1,
            task: tokio::spawn(std::future::pending()),
        });
        worker.clear_roles();
        worker.role_event(RoleEvent {
            epoch,
            token: 1,
            guild: 42.into(),
            result: Ok(()),
        });
        assert!(rx.try_recv().is_err());
    }
}
