//! Attachment I/O and HTTP run outside the serialized gateway/media worker.
use super::*;
use crate::attachments::{
    self, AttachmentInput, AttachmentStatus, PreparedAttachment, StagedAttachment,
};
use fluxer_protocol::{PlannedAttachment, UploadAttachment, UploadCancellation, UploadProgress};
use std::path::PathBuf;

pub(super) struct Stage {
    input: AttachmentInput,
    pub dto: StagedAttachment,
    revision: u64,
    prepared: Option<Arc<PreparedAttachment>>,
    uploaded: Option<Value>,
    job: Option<Job>,
    last_send_nonce: Option<String>,
    batch: u64,
    planning: Option<u64>,
    upload_started: Option<tokio::time::Instant>,
}
struct Job {
    task: JoinHandle<()>,
    cancel: Option<UploadCancellation>,
}
impl Drop for Job {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        self.task.abort();
    }
}
struct PendingSend {
    token: u64,
    nonce: String,
    ids: Vec<u64>,
    job: Option<Job>,
}
struct PreuploadPlan {
    members: Vec<(u64, u64)>,
    _job: Job,
}
#[derive(Clone, PartialEq)]
struct Fingerprint {
    channel: Snowflake,
    content: String,
    reference: Option<fluxer_core::MessageReference>,
    mention_author: bool,
    files: Vec<(u64, bool)>,
}
struct Attempt {
    fingerprint: Fingerprint,
}

pub(super) enum AttachmentEvent {
    Planned {
        epoch: u64,
        channel: Snowflake,
        batch: u64,
        members: Vec<(u64, u64)>,
        result: Result<Vec<PlannedAttachment>>,
    },
    Prepared {
        epoch: u64,
        channel: Snowflake,
        id: u64,
        revision: u64,
        result: Result<Arc<PreparedAttachment>>,
    },
    Uploaded {
        epoch: u64,
        channel: Snowflake,
        id: u64,
        revision: u64,
        result: Result<Value>,
    },
    Progress {
        epoch: u64,
        channel: Snowflake,
        id: Option<u64>,
        revision: u64,
        progress: f32,
    },
    Sent {
        epoch: u64,
        channel: Snowflake,
        token: u64,
        nonce: String,
        result: Result<Box<Message>>,
    },
}
pub(super) struct AttachmentState {
    epoch: u64,
    next_id: u64,
    next_send: u64,
    next_batch: u64,
    channels: BTreeMap<Snowflake, Vec<Stage>>,
    sends: BTreeMap<Snowflake, PendingSend>,
    attempts: BTreeMap<String, Attempt>,
    last_attempt: BTreeMap<Snowflake, String>,
    plans: BTreeMap<u64, PreuploadPlan>,
    tx: async_mpsc::UnboundedSender<AttachmentEvent>,
    pub rx: async_mpsc::UnboundedReceiver<AttachmentEvent>,
    preparation_slots: Arc<tokio::sync::Semaphore>,
}
impl AttachmentState {
    pub fn new() -> Self {
        let (tx, rx) = async_mpsc::unbounded_channel();
        Self {
            epoch: 1,
            next_id: 0,
            next_send: 0,
            next_batch: 0,
            channels: BTreeMap::new(),
            sends: BTreeMap::new(),
            attempts: BTreeMap::new(),
            last_attempt: BTreeMap::new(),
            plans: BTreeMap::new(),
            tx,
            rx,
            preparation_slots: Arc::new(tokio::sync::Semaphore::new(4)),
        }
    }
    fn stage(&mut self, channel: Snowflake, id: u64) -> Option<&mut Stage> {
        self.channels
            .get_mut(&channel)?
            .iter_mut()
            .find(|stage| stage.dto.id == id)
    }
}

impl Worker {
    pub(super) fn clear_attachments(&mut self) {
        self.attachments.epoch = self.attachments.epoch.wrapping_add(1);
        self.attachments.sends.clear();
        self.attachments.plans.clear();
        self.attachments.channels.clear();
        self.attachments.attempts.clear();
        self.attachments.last_attempt.clear();
        self.emit(Event::AttachmentsCleared);
    }
    fn attachment_policy(&self, channel: Snowflake) -> Result<fluxer_core::AttachmentPolicy> {
        let user = self
            .state
            .current_user
            .as_ref()
            .ok_or("Sign in before attaching files")?;
        let features = self
            .state
            .channels
            .get(&channel)
            .and_then(|c| c.guild_id)
            .and_then(|id| self.state.guilds.get(&id))
            .and_then(|guild| guild.extra.get("features"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        Ok(fluxer_core::AttachmentPolicy::resolve(
            self.client
                .as_ref()
                .map(|client| &client.instance().document)
                .unwrap_or(&Value::Null),
            user,
            &features,
            &self.state.user_settings,
        ))
    }
    fn can_attach(&self, channel: Snowflake) -> Result<()> {
        let target = self.state.channels.get(&channel).ok_or("Unknown channel")?;
        let user = self
            .state
            .current_user
            .as_ref()
            .ok_or("Sign in before attaching files")?;
        let permissions = self.state.permissions_for(channel, user.id);
        if !fluxer_core::can_send_message(target.kind, permissions)
            || !permissions.contains(Permissions::ATTACH_FILES)
        {
            return Err("You cannot attach files in this channel".into());
        }
        Ok(())
    }
    pub(super) fn attachment_snapshot(&self, channel: Snowflake) {
        let attachments = self
            .attachments
            .channels
            .get(&channel)
            .into_iter()
            .flatten()
            .map(|stage| stage.dto.clone())
            .collect();
        self.emit(Event::Attachments {
            channel_id: channel,
            attachments,
        });
        if let Ok(policy) = self.attachment_policy(channel) {
            self.emit(Event::AttachmentLimits {
                channel_id: channel,
                max_files: policy.max_attachments,
                max_file_bytes: policy.max_file_size,
                preupload: policy.preupload,
            });
        }
    }
    fn refresh_attachment_policy(&mut self, channel: Snowflake) {
        let preupload = !self.demo
            && self
                .attachment_policy(channel)
                .is_ok_and(|policy| policy.preupload)
            && self
                .client
                .as_ref()
                .is_some_and(Client::supports_presigned_attachment_uploads)
            && self.can_attach(channel).is_ok();
        if !preupload {
            for stage in self
                .attachments
                .channels
                .get_mut(&channel)
                .into_iter()
                .flatten()
            {
                // Preparation produces the local immutable clipboard source and
                // preview. Only background HTTP uploads are privacy-dependent.
                if stage.prepared.is_some() {
                    if stage.job.take().is_some() || stage.planning.take().is_some() {
                        stage.revision = stage.revision.wrapping_add(1);
                    }
                    stage.planning = None;
                    stage.uploaded = None;
                    if matches!(
                        stage.dto.status,
                        AttachmentStatus::Uploading | AttachmentStatus::Ready
                    ) {
                        stage.dto.status = AttachmentStatus::Staged;
                        stage.dto.progress = 0.;
                    }
                }
            }
            self.prune_attachment_plans();
        }
    }
    fn prune_attachment_plans(&mut self) {
        let active = self
            .attachments
            .channels
            .values()
            .flatten()
            .filter_map(|stage| stage.planning)
            .collect::<BTreeSet<_>>();
        self.attachments
            .plans
            .retain(|batch, _| active.contains(batch));
    }
    pub(super) fn refresh_attachment_policies(&mut self) {
        let channels = self
            .attachments
            .channels
            .keys()
            .copied()
            .collect::<Vec<_>>();
        for channel in channels {
            self.refresh_attachment_policy(channel);
            self.attachment_snapshot(channel);
            self.advance_attachment_send(channel);
        }
    }
    pub(super) fn stage_files(&mut self, channel: Snowflake, paths: Vec<PathBuf>) -> Result<()> {
        self.stage_inputs(
            channel,
            paths.into_iter().map(AttachmentInput::Disk).collect(),
        )
    }
    pub(super) fn stage_clipboard(
        &mut self,
        channel: Snowflake,
        filename: String,
        mime_type: String,
        bytes: Arc<Vec<u8>>,
    ) -> Result<()> {
        self.stage_inputs(
            channel,
            vec![AttachmentInput::Clipboard {
                filename,
                mime_type,
                bytes,
            }],
        )
    }
    fn stage_inputs(&mut self, channel: Snowflake, inputs: Vec<AttachmentInput>) -> Result<()> {
        self.can_attach(channel)?;
        let policy = self.attachment_policy(channel)?;
        let count = self.attachments.channels.get(&channel).map_or(0, Vec::len);
        if count.saturating_add(inputs.len()) > policy.max_attachments {
            return Err(format!(
                "This account allows {} attachments per message",
                policy.max_attachments
            ));
        }
        if self.attachments.next_id.saturating_add(inputs.len() as u64) > i32::MAX as u64 {
            return Err("Attachment identifier limit reached; restart the client".into());
        }
        self.attachments.next_batch = self.attachments.next_batch.wrapping_add(1);
        let batch = self.attachments.next_batch;
        let mut preparing = Vec::with_capacity(inputs.len());
        for input in inputs {
            self.attachments.next_id += 1;
            let id = self.attachments.next_id;
            let filename = input.filename();
            self.attachments
                .channels
                .entry(channel)
                .or_default()
                .push(Stage {
                    dto: StagedAttachment {
                        id,
                        spoiler: filename.starts_with("SPOILER_"),
                        filename,
                        size: 0,
                        mime_type: "application/octet-stream".into(),
                        status: AttachmentStatus::Preparing,
                        progress: 0.,
                        error: None,
                        preview: None,
                    },
                    input,
                    revision: 0,
                    prepared: None,
                    uploaded: None,
                    job: None,
                    last_send_nonce: None,
                    batch,
                    planning: None,
                    upload_started: None,
                });
            preparing.push(id);
        }
        // Reserve the whole batch before any preparation completion can start
        // its one background plan (MessageQueue.startTextareaAttachmentUploads).
        for id in preparing {
            self.prepare_attachment(channel, id, policy.max_file_size);
        }
        self.attachment_snapshot(channel);
        Ok(())
    }
    fn prepare_attachment(&mut self, channel: Snowflake, id: u64, max_size: u64) {
        let epoch = self.attachments.epoch;
        let tx = self.attachments.tx.clone();
        let slots = self.attachments.preparation_slots.clone();
        let Some(stage) = self.attachments.stage(channel, id) else {
            return;
        };
        stage.job = None;
        stage.revision = stage.revision.wrapping_add(1);
        let revision = stage.revision;
        let input = stage.input.clone();
        stage.dto.status = AttachmentStatus::Preparing;
        stage.dto.error = None;
        stage.dto.progress = 0.;
        let cancel = UploadCancellation::new();
        let cancelling = cancel.clone();
        stage.job = Some(Job {
            cancel: Some(cancel),
            task: tokio::spawn(async move {
                let Ok(slot) = slots.acquire_owned().await else {
                    return;
                };
                let result = tokio::task::spawn_blocking(move || {
                    let _slot = slot;
                    attachments::prepare(input, max_size, revision, &cancelling)
                })
                .await
                .map_err(|_| "Attachment preparation stopped".into())
                .and_then(|result| result);
                let _ = tx.send(AttachmentEvent::Prepared {
                    epoch,
                    channel,
                    id,
                    revision,
                    result,
                });
            }),
        });
    }
    fn start_preupload(&mut self, channel: Snowflake, id: u64) {
        if self.demo {
            return;
        }
        let Ok(policy) = self.attachment_policy(channel) else {
            return;
        };
        let Some(client) = self
            .client
            .clone()
            .filter(|client| policy.preupload && client.supports_presigned_attachment_uploads())
        else {
            return;
        };
        let Some(batch) = self.attachments.stage(channel, id).map(|stage| stage.batch) else {
            return;
        };
        if self.attachments.plans.contains_key(&batch) {
            return;
        }
        let group = self
            .attachments
            .channels
            .get(&channel)
            .into_iter()
            .flatten()
            .filter(|stage| stage.batch == batch)
            .collect::<Vec<_>>();
        // A single file-picker action produces one plan, after local metadata
        // for all its files is known. Transfers and cancellation remain per file.
        if group
            .iter()
            .any(|stage| stage.job.is_some() || stage.planning.is_some())
        {
            return;
        }
        let stages = group
            .into_iter()
            .filter(|stage| {
                stage.prepared.is_some()
                    && stage.uploaded.is_none()
                    && stage.dto.status == AttachmentStatus::Staged
                    && !self
                        .attachments
                        .sends
                        .get(&channel)
                        .is_some_and(|send| send.ids.contains(&stage.dto.id))
            })
            .collect::<Vec<_>>();
        if stages.is_empty() {
            return;
        }
        let members = stages
            .iter()
            .map(|stage| (stage.dto.id, stage.revision))
            .collect::<Vec<_>>();
        let files = stages
            .iter()
            .map(|stage| upload_file(stage, stage.dto.id.to_string()))
            .collect::<Vec<_>>();
        let keepalive = stages
            .iter()
            .filter_map(|stage| stage.prepared.clone())
            .collect::<Vec<_>>();
        for (id, _) in &members {
            let stage = self
                .attachments
                .stage(channel, *id)
                .expect("selected staged file");
            stage.planning = Some(batch);
            stage.upload_started = Some(tokio::time::Instant::now());
            stage.dto.status = AttachmentStatus::Uploading;
        }
        let epoch = self.attachments.epoch;
        let tx = self.attachments.tx.clone();
        let cancel = UploadCancellation::new();
        let cancelling = cancel.clone();
        let result_members = members.clone();
        let job = Job {
            cancel: Some(cancel),
            task: tokio::spawn(async move {
                let result = async {
                    for prepared in &keepalive {
                        prepared.validate_source().await?;
                    }
                    client
                        .plan_attachments(
                            &channel.to_string(),
                            &files,
                            &cancelling,
                            Arc::new(|_| {}),
                        )
                        .await
                        .map_err(error)
                }
                .await;
                let _ = tx.send(AttachmentEvent::Planned {
                    epoch,
                    channel,
                    batch,
                    members: result_members,
                    result,
                });
            }),
        };
        self.attachments
            .plans
            .insert(batch, PreuploadPlan { members, _job: job });
    }
    fn upload_planned_stage(&mut self, channel: Snowflake, id: u64, plan: PlannedAttachment) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let epoch = self.attachments.epoch;
        let tx = self.attachments.tx.clone();
        let Some(stage) = self.attachments.stage(channel, id) else {
            return;
        };
        let Some(prepared) = stage.prepared.clone() else {
            return;
        };
        let revision = stage.revision;
        let cancel = UploadCancellation::new();
        let cancelling = cancel.clone();
        let callback = progress_callback(tx.clone(), epoch, channel, Some(id), revision);
        stage.job = Some(Job {
            cancel: Some(cancel),
            task: tokio::spawn(async move {
                let result = async {
                    prepared.validate_source().await?;
                    client
                        .upload_planned_attachment(&plan, &cancelling, callback)
                        .await
                        .map_err(error)
                }
                .await;
                let _ = tx.send(AttachmentEvent::Uploaded {
                    epoch,
                    channel,
                    id,
                    revision,
                    result,
                });
            }),
        });
    }
    pub(super) fn retry_attachment(&mut self, channel: Snowflake, id: u64) -> Result<()> {
        self.can_attach(channel)?;
        if self
            .attachments
            .sends
            .get(&channel)
            .is_some_and(|send| send.ids.contains(&id))
        {
            return Err("Cancel the queued message before retrying its attachments".into());
        }
        let policy = self.attachment_policy(channel)?;
        let stage = self
            .attachments
            .stage(channel, id)
            .ok_or("Attachment is no longer staged")?;
        stage.dto.error = None;
        stage.dto.progress = 0.;
        stage.uploaded = None;
        if stage.prepared.is_some() {
            stage.job = None;
            stage.planning = None;
            stage.revision = stage.revision.wrapping_add(1);
            stage.dto.status = AttachmentStatus::Staged;
            self.prune_attachment_plans();
            self.start_preupload(channel, id);
        } else {
            self.prepare_attachment(channel, id, policy.max_file_size);
        }
        self.attachment_snapshot(channel);
        Ok(())
    }
    pub(super) fn cancel_attachment(
        &mut self,
        channel: Snowflake,
        id: u64,
        remove: bool,
    ) -> Result<()> {
        if self
            .attachments
            .sends
            .get(&channel)
            .is_some_and(|send| send.ids.contains(&id))
        {
            self.fail_attachment_send(channel, "Attachment send cancelled".into());
        }
        let stage = self
            .attachments
            .stage(channel, id)
            .ok_or("Attachment is no longer staged")?;
        stage.job = None;
        stage.uploaded = None;
        stage.planning = None;
        stage.revision = stage.revision.wrapping_add(1);
        stage.dto.status = AttachmentStatus::Cancelled;
        stage.dto.error = None;
        if remove {
            self.attachments
                .channels
                .get_mut(&channel)
                .expect("staged channel")
                .retain(|stage| stage.dto.id != id);
        }
        self.prune_attachment_plans();
        self.attachment_snapshot(channel);
        Ok(())
    }
    pub(super) fn cancel_attachment_send(&mut self, channel: Snowflake) {
        self.fail_attachment_send(channel, "Attachment send cancelled".into());
    }
    pub(super) fn spoiler_attachment(
        &mut self,
        channel: Snowflake,
        id: u64,
        spoiler: bool,
    ) -> Result<()> {
        if self
            .attachments
            .sends
            .get(&channel)
            .is_some_and(|send| send.ids.contains(&id))
        {
            return Err("Cancel the queued message before changing an attachment".into());
        }
        let stage = self
            .attachments
            .stage(channel, id)
            .ok_or("Attachment is no longer staged")?;
        stage.dto.spoiler = spoiler;
        self.attachment_snapshot(channel);
        Ok(())
    }
    pub(super) fn request_attachment_send(
        &mut self,
        channel: Snowflake,
        content: String,
        reference: Option<fluxer_core::MessageReference>,
        mention_author: bool,
    ) -> Result<()> {
        self.refresh_attachment_policy(channel);
        let user = self.state.current_user.as_ref().ok_or("Sign in first")?;
        let target = self.state.channels.get(&channel).ok_or("Unknown channel")?;
        if !fluxer_core::can_send_message(target.kind, self.state.permissions_for(channel, user.id))
        {
            return Err("You cannot send messages in this channel".into());
        }
        if self.attachments.sends.contains_key(&channel) {
            return Err("A message is already queued in this channel".into());
        }
        let stages = self
            .attachments
            .channels
            .get(&channel)
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        if Client::build_message_body(&content, None, reference.as_ref(), mention_author)
            .get("content")
            .is_none()
            && stages.is_empty()
        {
            return Err("Message cannot be empty".into());
        }
        if !stages.is_empty() {
            self.can_attach(channel)?;
        }
        let policy = self.attachment_policy(channel)?;
        if stages.len() > policy.max_attachments
            || stages
                .iter()
                .any(|stage| stage.dto.size > policy.max_file_size)
        {
            return Err("Staged files exceed this account's upload limits".into());
        }
        let ids = stages.iter().map(|stage| stage.dto.id).collect::<Vec<_>>();
        let fingerprint = Fingerprint {
            channel,
            content,
            reference,
            mention_author,
            files: stages
                .iter()
                .map(|stage| (stage.dto.id, stage.dto.spoiler))
                .collect(),
        };
        let nonce = self
            .attachments
            .last_attempt
            .get(&channel)
            .filter(|nonce| {
                self.attachments
                    .attempts
                    .get(*nonce)
                    .is_some_and(|attempt| attempt.fingerprint == fingerprint)
            })
            .cloned()
            .unwrap_or_else(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
                    .to_string()
            });
        self.attachments
            .attempts
            .insert(nonce.clone(), Attempt { fingerprint });
        self.attachments.last_attempt.insert(channel, nonce.clone());
        self.attachments.next_send = self.attachments.next_send.wrapping_add(1);
        self.attachments.sends.insert(
            channel,
            PendingSend {
                token: self.attachments.next_send,
                nonce: nonce.clone(),
                ids: ids.clone(),
                job: None,
            },
        );
        for stage in self
            .attachments
            .channels
            .get_mut(&channel)
            .into_iter()
            .flatten()
            .filter(|stage| ids.contains(&stage.dto.id))
        {
            stage.last_send_nonce = Some(nonce.clone());
            if stage.dto.status != AttachmentStatus::Cancelled {
                stage.dto.status = AttachmentStatus::Queued;
            }
        }
        self.attachment_snapshot(channel);
        self.advance_attachment_send(channel);
        Ok(())
    }
    fn advance_attachment_send(&mut self, channel: Snowflake) {
        self.refresh_attachment_policy(channel);
        for stage in self
            .attachments
            .channels
            .get_mut(&channel)
            .into_iter()
            .flatten()
        {
            // Textarea cache TTL is five minutes; source prunes only settled
            // uploads. A queued send then obtains a fresh send-time plan.
            if stage.job.is_none()
                && stage.planning.is_none()
                && stage
                    .upload_started
                    .is_some_and(|started| started.elapsed() >= Duration::from_secs(300))
            {
                stage.uploaded = None;
            }
        }
        let Some(send) = self.attachments.sends.get(&channel) else {
            return;
        };
        if send.job.is_some() {
            return;
        }
        let stages = self
            .attachments
            .channels
            .get(&channel)
            .into_iter()
            .flatten()
            .filter(|stage| send.ids.contains(&stage.dto.id))
            .collect::<Vec<_>>();
        if stages
            .iter()
            .any(|stage| stage.job.is_some() || stage.planning.is_some())
        {
            return;
        }
        if stages.len() != send.ids.len()
            || stages.iter().any(|stage| {
                stage.prepared.is_none() || stage.dto.status == AttachmentStatus::Cancelled
            })
        {
            self.fail_attachment_send(
                channel,
                "Retry or remove attachments that could not be prepared".into(),
            );
            return;
        }
        let policy = match self.attachment_policy(channel) {
            Ok(policy) => policy,
            Err(message) => {
                self.fail_attachment_send(channel, message);
                return;
            }
        };
        if let Err(message) = if stages.is_empty() {
            Ok(())
        } else {
            self.can_attach(channel)
        } {
            self.fail_attachment_send(channel, message);
            return;
        }
        if stages.len() > policy.max_attachments
            || stages
                .iter()
                .any(|stage| stage.dto.size > policy.max_file_size)
        {
            self.fail_attachment_send(
                channel,
                "Staged files exceed this account's upload limits".into(),
            );
            return;
        }
        let nonce = send.nonce.clone();
        let token = send.token;
        let attempt = &self.attachments.attempts[&nonce];
        let fingerprint = attempt.fingerprint.clone();
        let files = stages
            .iter()
            .enumerate()
            .map(|(index, stage)| upload_file(stage, index.to_string()))
            .collect::<Vec<_>>();
        let keepalive = stages
            .iter()
            .filter_map(|stage| stage.prepared.clone())
            .collect::<Vec<_>>();
        let cached=stages.iter().enumerate().map(|(index,stage)|stage.uploaded.as_ref().map(|uploaded|{
            let mut metadata=json!({"id":index.to_string(),"filename":stage.dto.filename,"title":stage.dto.filename,"flags":if stage.dto.spoiler {8}else{0}});
            for key in ["upload_filename","file_size","content_type"] {if let Some(value)=uploaded.get(key){metadata[key]=value.clone();}}
            metadata
        })).collect::<Option<Vec<_>>>();
        let client = self.client.clone();
        if client.is_none() && !self.demo {
            self.fail_attachment_send(channel, "Sign in first".into());
            return;
        }
        let demo_user = self.state.current_user.clone();
        let demo_id = self.state.next_local_message_id();
        let epoch = self.attachments.epoch;
        let tx = self.attachments.tx.clone();
        let cancel = UploadCancellation::new();
        let cancelling = cancel.clone();
        let callback = progress_callback(tx.clone(), epoch, channel, None, token);
        for stage in self
            .attachments
            .channels
            .get_mut(&channel)
            .into_iter()
            .flatten()
            .filter(|stage| fingerprint.files.iter().any(|(id, _)| *id == stage.dto.id))
        {
            stage.dto.status = AttachmentStatus::Sending;
            stage.dto.error = None;
        }
        let sending_nonce = nonce.clone();
        let task = tokio::spawn(async move {
            let result=async {
                for prepared in &keepalive { prepared.validate_source().await?; }
                let _keepalive=keepalive;
                if let Some(client)=client {
                    let channel_wire=channel.to_string();
                    let mut body=Client::build_message_body(&fingerprint.content,Some(&sending_nonce),fingerprint.reference.as_ref(),fingerprint.mention_author);
                    let result=if files.is_empty() {
                        tokio::select! {_=cancelling.cancelled()=>Err("Attachment send cancelled".to_owned()),result=client.send_message_body(&channel_wire,body)=>result.map_err(error)}
                    }else if let Some(cached)=cached.filter(|_|policy.preupload) {
                        body["attachments"]=Value::Array(cached);
                        tokio::select! {_=cancelling.cancelled()=>Err("Attachment send cancelled".to_owned()),result=client.send_message_body(&channel_wire,body)=>result.map_err(error)}
                    } else {client.send_message_with_attachments(&channel_wire,body,&files,&cancelling,callback).await.map_err(error)}?;
                    serde_json::from_value::<Message>(result).map_err(error)
                } else {
                    let mut message=Message::text(demo_id,channel,demo_user.ok_or("No current account")?,fingerprint.content);
                    message.nonce=Some(sending_nonce.clone());message.message_reference=fingerprint.reference;
                    message.attachments=files.iter().enumerate().map(|(index,file)|fluxer_core::Attachment{id:Snowflake(index as u64+1),filename:file.filename.clone(),size:std::fs::metadata(&file.path).map(|m|m.len()).unwrap_or(0),content_type:Some(file.content_type.clone()),flags:file.flags.unwrap_or(0),..fluxer_core::Attachment::default()}).collect();
                    Ok(message)
                }
            }.await;
            let _ = tx.send(AttachmentEvent::Sent {
                epoch,
                channel,
                token,
                nonce: sending_nonce,
                result: result.map(Box::new),
            });
        });
        self.attachments
            .sends
            .get_mut(&channel)
            .expect("pending send")
            .job = Some(Job {
            task,
            cancel: Some(cancel),
        });
        self.attachment_snapshot(channel);
    }
    fn fail_attachment_send(&mut self, channel: Snowflake, message: String) {
        let Some(send) = self.attachments.sends.remove(&channel) else {
            return;
        };
        let content = self
            .attachments
            .attempts
            .get(&send.nonce)
            .map(|attempt| attempt.fingerprint.content.clone())
            .unwrap_or_default();
        for stage in self
            .attachments
            .channels
            .get_mut(&channel)
            .into_iter()
            .flatten()
            .filter(|stage| send.ids.contains(&stage.dto.id))
        {
            stage.job = None;
            stage.planning = None;
            stage.revision = stage.revision.wrapping_add(1);
            stage.dto.status = AttachmentStatus::Failed;
            stage.dto.error = Some(message.clone());
        }
        self.prune_attachment_plans();
        self.emit(Event::MessageOperationFailed {
            channel_id: channel,
            content,
            error: message,
        });
        self.attachment_snapshot(channel);
    }
    pub(super) fn reconcile_attachment_message(&mut self, message: &Message) {
        if self
            .state
            .current_user
            .as_ref()
            .is_none_or(|me| message.author.id != me.id)
        {
            return;
        }
        if let Some(nonce) = message.nonce.as_deref() {
            self.complete_attachment_send(message.channel_id, nonce, message.clone());
        }
    }
    fn complete_attachment_send(&mut self, channel: Snowflake, nonce: &str, message: Message) {
        let Some(attempt) = self.attachments.attempts.remove(nonce) else {
            return;
        };
        if attempt.fingerprint.channel != channel {
            self.attachments.attempts.insert(nonce.into(), attempt);
            return;
        }
        if self
            .attachments
            .sends
            .get(&channel)
            .is_some_and(|send| send.nonce == nonce)
        {
            self.attachments.sends.remove(&channel);
        }
        let message_id = message.id;
        self.state.insert_message(message);
        self.navigation_message_inserted(channel, message_id);
        let content = attempt.fingerprint.content;
        if self.state.drafts.get(&channel) == Some(&content) {
            self.state.set_draft(channel, String::new());
        }
        if let Some(stages) = self.attachments.channels.get_mut(&channel) {
            stages.retain(|stage| stage.last_send_nonce.as_deref() != Some(nonce));
        }
        if self
            .attachments
            .last_attempt
            .get(&channel)
            .is_some_and(|last| last == nonce)
        {
            self.attachments.last_attempt.remove(&channel);
        }
        self.attachment_snapshot(channel);
        self.snapshot();
        self.emit(Event::MessageSent {
            channel_id: channel,
            content,
        });
    }
    pub(super) fn attachment_event(&mut self, event: AttachmentEvent) {
        match event {
            AttachmentEvent::Planned {
                epoch,
                channel,
                batch,
                members,
                result,
            } => {
                if epoch != self.attachments.epoch
                    || !self
                        .attachments
                        .plans
                        .get(&batch)
                        .is_some_and(|plan| plan.members == members)
                {
                    return;
                }
                self.attachments.plans.remove(&batch);
                for (id, revision) in members {
                    let Some(stage) = self.attachments.stage(channel, id).filter(|stage| {
                        stage.revision == revision && stage.planning == Some(batch)
                    }) else {
                        continue;
                    };
                    stage.planning = None;
                    match &result {
                        Ok(plans) => {
                            if let Some(plan) = plans
                                .iter()
                                .find(|plan| plan.id() == id.to_string())
                                .cloned()
                            {
                                self.upload_planned_stage(channel, id, plan);
                            } else {
                                stage.dto.status = AttachmentStatus::Failed;
                                stage.dto.error =
                                    Some("Upload plan omitted the selected file".into());
                            }
                        }
                        Err(message) => {
                            stage.dto.status = AttachmentStatus::Failed;
                            stage.dto.error = Some(message.clone());
                        }
                    }
                }
                self.attachment_snapshot(channel);
                self.advance_attachment_send(channel);
            }
            AttachmentEvent::Prepared {
                epoch,
                channel,
                id,
                revision,
                result,
            } => {
                if epoch != self.attachments.epoch {
                    return;
                }
                let policy = self.attachment_policy(channel).ok();
                let Some(stage) = self
                    .attachments
                    .stage(channel, id)
                    .filter(|stage| stage.revision == revision)
                else {
                    return;
                };
                stage.job = None;
                match result {
                    Ok(prepared)
                        if policy.is_some_and(|policy| prepared.size <= policy.max_file_size) =>
                    {
                        stage.dto.size = prepared.size;
                        stage.dto.mime_type = prepared.mime_type.clone();
                        stage.dto.preview = prepared.preview.clone();
                        stage.prepared = Some(prepared);
                        stage.dto.status = AttachmentStatus::Staged;
                    }
                    Ok(_) => {
                        stage.dto.status = AttachmentStatus::Failed;
                        stage.dto.error = Some("File exceeds this account's upload limit".into());
                    }
                    Err(message) => {
                        stage.dto.status = AttachmentStatus::Failed;
                        stage.dto.error = Some(message);
                    }
                }
                self.start_preupload(channel, id);
                self.attachment_snapshot(channel);
                self.advance_attachment_send(channel);
            }
            AttachmentEvent::Uploaded {
                epoch,
                channel,
                id,
                revision,
                result,
            } => {
                if epoch != self.attachments.epoch {
                    return;
                }
                let Some(stage) = self
                    .attachments
                    .stage(channel, id)
                    .filter(|stage| stage.revision == revision)
                else {
                    return;
                };
                stage.job = None;
                match result {
                    Ok(uploaded) => {
                        stage.uploaded = Some(uploaded);
                        stage.dto.status = AttachmentStatus::Ready;
                        stage.dto.progress = 1.;
                    }
                    Err(message) => {
                        stage.dto.status = AttachmentStatus::Failed;
                        stage.dto.error = Some(message);
                    }
                }
                self.start_preupload(channel, id);
                self.attachment_snapshot(channel);
                self.advance_attachment_send(channel);
            }
            AttachmentEvent::Progress {
                epoch,
                channel,
                id,
                revision,
                progress,
            } => {
                if epoch != self.attachments.epoch {
                    return;
                }
                if let Some(id) = id {
                    if let Some(stage) = self
                        .attachments
                        .stage(channel, id)
                        .filter(|stage| stage.revision == revision)
                    {
                        stage.dto.progress = progress;
                    }
                } else if let Some(send) = self
                    .attachments
                    .sends
                    .get(&channel)
                    .filter(|send| send.token == revision)
                {
                    let ids = send.ids.clone();
                    for stage in self
                        .attachments
                        .channels
                        .get_mut(&channel)
                        .into_iter()
                        .flatten()
                        .filter(|stage| ids.contains(&stage.dto.id))
                    {
                        stage.dto.progress = progress;
                    }
                } else {
                    return;
                }
                self.attachment_snapshot(channel);
            }
            AttachmentEvent::Sent {
                epoch,
                channel,
                token,
                nonce,
                result,
            } => {
                if epoch != self.attachments.epoch {
                    return;
                }
                match result {
                    Ok(message) => self.complete_attachment_send(channel, &nonce, *message),
                    Err(message) => {
                        if self
                            .attachments
                            .sends
                            .get(&channel)
                            .is_some_and(|send| send.token == token)
                        {
                            self.fail_attachment_send(channel, message);
                        }
                    }
                }
            }
        }
    }
}
fn upload_file(stage: &Stage, id: String) -> UploadAttachment {
    let prepared = stage.prepared.as_ref().expect("prepared attachment");
    let mut file = UploadAttachment::new(
        id,
        prepared.path.clone(),
        stage.dto.filename.clone(),
        prepared.mime_type.clone(),
    );
    file.flags = Some(if stage.dto.spoiler { 8 } else { 0 });
    file
}
fn progress_callback(
    tx: async_mpsc::UnboundedSender<AttachmentEvent>,
    epoch: u64,
    channel: Snowflake,
    id: Option<u64>,
    revision: u64,
) -> Arc<dyn Fn(UploadProgress) + Send + Sync> {
    let last = Mutex::new(std::time::Instant::now() - Duration::from_secs(1));
    Arc::new(move |progress| {
        let ratio = if progress.total_bytes == 0 {
            0.
        } else {
            (progress.uploaded_bytes as f64 / progress.total_bytes as f64).clamp(0., 1.) as f32
        };
        let Ok(mut last) = last.lock() else { return };
        if ratio < 1. && last.elapsed() < Duration::from_millis(100) {
            return;
        }
        *last = std::time::Instant::now();
        let _ = tx.send(AttachmentEvent::Progress {
            epoch,
            channel,
            id,
            revision,
            progress: ratio,
        });
    })
}

#[cfg(test)]
#[path = "worker_attachment_tests.rs"]
mod tests;
