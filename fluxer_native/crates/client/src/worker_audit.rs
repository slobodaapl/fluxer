// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

pub(super) struct AuditEvent {
    guild: Snowflake,
    request: u64,
    result: Result<Value>,
}
pub(super) struct AuditState {
    pub rx: async_mpsc::UnboundedReceiver<AuditEvent>,
    tx: async_mpsc::UnboundedSender<AuditEvent>,
    job: Option<(u64, JoinHandle<()>)>,
}
impl AuditState {
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
impl Worker {
    pub(super) fn audit_request(
        &mut self,
        request: u64,
        guild: Snowflake,
        before: Option<String>,
    ) -> Result<()> {
        let actor = self.state.current_user.as_ref().ok_or("Not signed in")?.id;
        if !self.state.guilds.contains_key(&guild)
            || !self.state.members.contains_key(&(guild, actor))
            || !self
                .state
                .guild_permissions_for(guild, actor)
                .contains(Permissions::VIEW_AUDIT_LOG)
        {
            return Err("You cannot view the community audit log".into());
        }
        if self.demo {
            return Err("Audit log is unavailable in the offline demo".into());
        }
        let client = self.client.clone().ok_or("Not signed in")?;
        self.audit.clear();
        let tx = self.audit.tx.clone();
        let task = tokio::spawn(async move {
            let result = tokio::time::timeout(
                Duration::from_secs(15),
                client.guild_audit_log(&guild.to_string(), before.as_deref()),
            )
            .await
            .map_err(|_| "Audit log request timed out".to_owned())
            .and_then(|r| r.map_err(error))
            .and_then(|v| {
                if v["audit_log_entries"].is_array() && v["users"].is_array() {
                    Ok(v)
                } else {
                    Err("Invalid audit log returned by server".into())
                }
            });
            let _ = tx.send(AuditEvent {
                guild,
                request,
                result,
            });
        });
        self.audit.job = Some((request, task));
        Ok(())
    }
    pub(super) fn audit_event(&mut self, event: AuditEvent) {
        if self
            .audit
            .job
            .as_ref()
            .is_none_or(|(request, _)| *request != event.request)
        {
            return;
        }
        self.audit.job = None;
        self.emit(Event::AuditLog {
            request_id: event.request,
            guild_id: event.guild,
            result: event.result,
        });
    }
}
