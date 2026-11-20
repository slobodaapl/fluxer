// SPDX-License-Identifier: AGPL-3.0-or-later
//! Source parity: AttachmentUploadPlan, ChunkedAttachmentUploader and
//! MessageQueue. Storage requests have no account/instance/default headers.
use crate::{Client, Error, Result, http::segment};
use futures_util::{StreamExt, TryStreamExt, stream};
use reqwest::{Method, header, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt},
    sync::watch,
};
const CHUNK: usize = 64 * 1024;
const FALLBACK_LIMIT: u64 = 25 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct UploadAttachment {
    pub id: String,
    pub path: PathBuf,
    pub filename: String,
    pub title: String,
    pub content_type: String,
    pub description: Option<String>,
    pub flags: Option<u32>,
    pub duration: Option<u64>,
    pub waveform: Option<String>,
}
impl UploadAttachment {
    pub fn new(
        id: impl Into<String>,
        path: impl Into<PathBuf>,
        filename: impl Into<String>,
        content_type: impl Into<String>,
    ) -> Self {
        let filename = filename.into();
        Self {
            id: id.into(),
            path: path.into(),
            title: filename.clone(),
            filename,
            content_type: content_type.into(),
            description: None,
            flags: None,
            duration: None,
            waveform: None,
        }
    }
    fn metadata(&self) -> Value {
        let mut value = json!({"id":self.id,"filename":self.filename,"title":self.title});
        if let Some(v) = &self.description {
            value["description"] = v.clone().into();
        }
        if let Some(v) = self.flags {
            value["flags"] = v.into();
        }
        if let Some(v) = self.duration {
            value["duration"] = v.into();
        }
        if let Some(v) = &self.waveform {
            value["waveform"] = v.clone().into();
        }
        value
    }
}
/// One file from a shared planning request. Capability URLs remain private;
/// each handle can be transferred and completed using an independent token.
#[derive(Clone)]
pub struct PlannedAttachment {
    file: PreparedFile,
    plan: Option<Plan>,
    channel: String,
    instance_api: url::Url,
}
impl PlannedAttachment {
    pub fn id(&self) -> &str {
        &self.file.attachment.id
    }
}
impl std::fmt::Debug for PlannedAttachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlannedAttachment")
            .field("id", &self.id())
            .field("file_size", &self.file.size)
            .field("has_plan", &self.plan.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct UploadCancellation {
    cancelled: watch::Sender<bool>,
}
impl Default for UploadCancellation {
    fn default() -> Self {
        Self::new()
    }
}
impl UploadCancellation {
    pub fn new() -> Self {
        let (cancelled, _) = watch::channel(false);
        Self { cancelled }
    }
    pub fn cancel(&self) {
        self.cancelled.send_replace(true);
    }
    pub fn is_cancelled(&self) -> bool {
        *self.cancelled.borrow()
    }
    pub async fn cancelled(&self) {
        let mut state = self.cancelled.subscribe();
        let _ = state.wait_for(|cancelled| *cancelled).await;
    }
    fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(Error::UploadCancelled)
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UploadPhase {
    Planning,
    Uploading,
    Completing,
    Sending,
    Complete,
}
#[derive(Clone, Debug)]
pub struct UploadProgress {
    pub phase: UploadPhase,
    pub uploaded_bytes: u64,
    pub total_bytes: u64,
    pub completed_files: u32,
    pub total_files: u32,
}
pub type UploadProgressCallback = Arc<dyn Fn(UploadProgress) + Send + Sync>;
struct Progress {
    monotonic: bool,
    state: Mutex<ProgressState>,
    callback: UploadProgressCallback,
}
struct ProgressState {
    phase: UploadPhase,
    loaded: Vec<u64>,
    sizes: Vec<u64>,
    finished: Vec<bool>,
}
impl Progress {
    fn new(files: &[PreparedFile], callback: UploadProgressCallback) -> Self {
        Self {
            monotonic: true,
            state: Mutex::new(ProgressState {
                phase: UploadPhase::Planning,
                loaded: vec![0; files.len()],
                sizes: files.iter().map(|f| f.size).collect(),
                finished: vec![false; files.len()],
            }),
            callback,
        }
    }
    fn finish_file(&self, index: usize) {
        let size = if let Ok(mut state) = self.state.lock() {
            state.finished[index] = true;
            state.sizes[index]
        } else {
            return;
        };
        self.report(None, Some((index, size)), false);
    }
    fn finish_all(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.finished.fill(true);
            state.loaded = state.sizes.clone();
        }
    }
    fn report(&self, phase: Option<UploadPhase>, file: Option<(usize, u64)>, reset: bool) {
        if let Ok(mut state) = self.state.lock() {
            if reset {
                state.loaded.fill(0);
                state.finished.fill(false);
            }
            if let Some(phase) = phase {
                state.phase = phase;
            }
            if let Some((index, loaded)) = file {
                let loaded = loaded.min(state.sizes[index]);
                state.loaded[index] = if self.monotonic {
                    state.loaded[index].max(loaded)
                } else {
                    loaded
                };
            }
            (self.callback)(UploadProgress {
                phase: state.phase,
                uploaded_bytes: state.loaded.iter().sum(),
                total_bytes: state.sizes.iter().sum(),
                completed_files: state.finished.iter().filter(|done| **done).count() as u32,
                total_files: state.sizes.len() as u32,
            });
        }
    }
}
#[derive(Clone)]
struct PreparedFile {
    attachment: UploadAttachment,
    size: u64,
    modified: Option<SystemTime>,
}
#[derive(Clone, Deserialize)]
#[serde(tag = "upload_mode")]
enum Plan {
    #[serde(rename = "singlepart")]
    Single {
        id: Value,
        filename: String,
        upload_filename: String,
        upload_url: String,
        file_size: u64,
        content_type: String,
    },
    #[serde(rename = "multipart")]
    Multipart {
        id: Value,
        filename: String,
        upload_filename: String,
        upload_id: String,
        part_size: u64,
        parts: Vec<Part>,
        file_size: u64,
        content_type: String,
    },
}
#[derive(Clone, Deserialize)]
struct Part {
    part_number: u32,
    upload_url: String,
}
#[derive(Serialize)]
struct Completion {
    upload_filename: String,
    upload_id: String,
}
impl Plan {
    fn id(&self) -> Result<String> {
        let id = match self {
            Self::Single { id, .. } | Self::Multipart { id, .. } => id,
        };
        if let Some(s) = id.as_str() {
            Ok(s.into())
        } else if let Some(n) = id.as_i64() {
            Ok(n.to_string())
        } else {
            Err(Error::InvalidUpload("invalid plan attachment ID"))
        }
    }
    fn upload_filename(&self) -> &str {
        match self {
            Self::Single {
                upload_filename, ..
            }
            | Self::Multipart {
                upload_filename, ..
            } => upload_filename,
        }
    }
    fn content_type(&self) -> &str {
        match self {
            Self::Single { content_type, .. } | Self::Multipart { content_type, .. } => {
                content_type
            }
        }
    }
    fn file_size(&self) -> u64 {
        match self {
            Self::Single { file_size, .. } | Self::Multipart { file_size, .. } => *file_size,
        }
    }
    fn filename(&self) -> &str {
        match self {
            Self::Single { filename, .. } | Self::Multipart { filename, .. } => filename,
        }
    }
}
async fn cancellable<T>(
    cancel: &UploadCancellation,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! {biased; _=cancel.cancelled()=>Err(Error::UploadCancelled), result=future=>result}
}
struct UploadTransport {
    api: reqwest::Client,
    storage: reqwest::Client,
}
fn upload_transport() -> Result<UploadTransport> {
    Ok(UploadTransport {
        api: reqwest::Client::builder()
            .redirect(Policy::none())
            .build()?,
        storage: storage_client()?,
    })
}
fn storage_client() -> Result<reqwest::Client> {
    // No default headers, cookies, Authorization, Origin, installation metadata,
    // compression or redirect handling. Presigned URLs are capability URLs.
    Ok(reqwest::Client::builder()
        .redirect(Policy::none())
        .no_gzip()
        .no_brotli()
        .build()?)
}
async fn prepare(
    files: &[UploadAttachment],
    cancel: &UploadCancellation,
) -> Result<Vec<PreparedFile>> {
    if files.is_empty() || files.len() > 10 {
        return Err(Error::InvalidUpload("expected 1–10 attachments"));
    }
    let mut ids = std::collections::HashSet::new();
    let mut prepared = Vec::with_capacity(files.len());
    for attachment in files {
        cancel.check()?;
        if attachment.id.parse::<i32>().is_err() || !ids.insert(attachment.id.clone()) {
            return Err(Error::InvalidUpload(
                "attachment IDs must be unique Int32 strings",
            ));
        }
        if attachment.filename.is_empty() {
            return Err(Error::InvalidUpload("empty filename"));
        }
        let mut attachment = attachment.clone();
        if attachment.content_type.is_empty() {
            attachment.content_type = "application/octet-stream".into();
        }
        header::HeaderValue::from_str(&attachment.content_type)
            .map_err(|_| Error::InvalidUpload("invalid content type"))?;
        let metadata = tokio::fs::metadata(&attachment.path)
            .await
            .map_err(Error::AttachmentIo)?;
        if !metadata.is_file() {
            return Err(Error::InvalidUpload("attachment is not a regular file"));
        }
        prepared.push(PreparedFile {
            attachment,
            size: metadata.len(),
            modified: metadata.modified().ok(),
        });
    }
    prepared.iter().try_fold(0_u64, |total, file| {
        total
            .checked_add(file.size)
            .ok_or(Error::InvalidUpload("attachment sizes overflow"))
    })?;
    Ok(prepared)
}
fn storage_url(value: &str) -> Result<url::Url> {
    let url = url::Url::parse(value).map_err(|_| Error::InvalidUpload("invalid storage URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::InvalidUpload(
            "storage URL must be credential-free HTTP(S)",
        ));
    }
    Ok(url)
}
async fn file_body(
    file: PreparedFile,
    offset: u64,
    length: u64,
    on_bytes: Arc<dyn Fn(u64) + Send + Sync>,
    cancel: UploadCancellation,
) -> Result<reqwest::Body> {
    cancel.check()?;
    let mut input = File::open(&file.attachment.path)
        .await
        .map_err(Error::AttachmentIo)?;
    let metadata = input.metadata().await.map_err(Error::AttachmentIo)?;
    if metadata.len() != file.size || metadata.modified().ok() != file.modified {
        return Err(Error::InvalidUpload("attachment changed after selection"));
    }
    input
        .seek(std::io::SeekFrom::Start(offset))
        .await
        .map_err(Error::AttachmentIo)?;
    let chunks = stream::try_unfold(
        (input, length, 0_u64),
        move |(mut input, remaining, uploaded)| {
            let on_bytes = on_bytes.clone();
            let cancel = cancel.clone();
            async move {
                if remaining == 0 {
                    return Ok(None);
                }
                if cancel.is_cancelled() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "attachment upload cancelled",
                    ));
                }
                let mut chunk = vec![0; remaining.min(CHUNK as u64) as usize];
                let read = input.read(&mut chunk).await?;
                if read == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "attachment changed during upload",
                    ));
                }
                chunk.truncate(read);
                let uploaded = uploaded + read as u64;
                on_bytes(uploaded);
                Ok(Some((chunk, (input, remaining - read as u64, uploaded))))
            }
        },
    );
    Ok(reqwest::Body::wrap_stream(chunks))
}
impl Client {
    pub fn supports_presigned_attachment_uploads(&self) -> bool {
        self.instance()
            .features
            .get("presigned_attachment_uploads")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
    /// Reuse the same reply/mention/nonce shape as normal native message sends.
    pub fn build_message_body(
        content: &str,
        nonce: Option<&str>,
        reference: Option<&fluxer_core::MessageReference>,
        mention_author: bool,
    ) -> Value {
        crate::http::message_body(content, nonce, reference, mention_author)
    }
    pub async fn send_message_body(&self, channel: &str, body: Value) -> Result<Value> {
        // RestTransport defaultTimeoutMs is0, including large upload/message
        // requests. Do not inherit the ordinary REST client's30second cap.
        let transport = upload_transport()?;
        let request = self
            .upload_api_request(
                &transport.api,
                Method::POST,
                &format!("/channels/{}/messages", segment(channel)),
            )?
            .json(&body);
        Self::parse_api_response(request.send().await?).await
    }
    /// Request all plans for one staged batch, exactly once. The controller
    /// retains the shared cancellation token until its last batch member leaves.
    /// A missing response ID fails only that file when its handle is transferred.
    pub async fn plan_attachments(
        &self,
        channel: &str,
        files: &[UploadAttachment],
        cancel: &UploadCancellation,
        callback: UploadProgressCallback,
    ) -> Result<Vec<PlannedAttachment>> {
        if !self.supports_presigned_attachment_uploads() {
            return Err(Error::InvalidUpload(
                "presigned attachment uploads are disabled",
            ));
        }
        let files = prepare(files, cancel).await?;
        let progress = Arc::new(Progress::new(&files, callback));
        let transport = upload_transport()?;
        let plans = self
            .request_attachment_plans(channel, &files, &transport.api, cancel, &progress)
            .await?;
        let indexed = plans
            .into_iter()
            .map(|plan| Ok((plan.id()?, plan)))
            .collect::<Result<HashMap<_, _>>>()?;
        Ok(files
            .into_iter()
            .map(|file| PlannedAttachment {
                plan: indexed.get(&file.attachment.id).cloned(),
                file,
                channel: channel.into(),
                instance_api: self.instance().api.clone(),
            })
            .collect())
    }
    /// Transfer and, for multipart, complete one staged file. No message or
    /// fallback is sent. Callers may run siblings concurrently and cancel each
    /// independently after the single shared planning request has resolved.
    pub async fn upload_planned_attachment(
        &self,
        planned: &PlannedAttachment,
        cancel: &UploadCancellation,
        callback: UploadProgressCallback,
    ) -> Result<Value> {
        cancel.check()?;
        if planned.instance_api != self.instance().api {
            return Err(Error::InvalidUpload(
                "upload plan belongs to a different instance",
            ));
        }
        let plan = planned.plan.as_ref().ok_or(Error::AttachmentPlanMissing)?;
        validate_plan(plan, &planned.file)?;
        let files = std::slice::from_ref(&planned.file);
        let mut progress = Progress::new(files, callback);
        // Textarea upload reports part retry resets directly. Full-message
        // AttachmentUploadPlan uses a monotonic outer per-file maximum instead.
        progress.monotonic = false;
        let progress = Arc::new(progress);
        let transport = upload_transport()?;
        let mut values = self
            .transfer_plans(
                &planned.channel,
                files,
                std::slice::from_ref(plan),
                &transport,
                cancel,
                &progress,
            )
            .await?;
        progress.report(Some(UploadPhase::Complete), None, false);
        Ok(values.remove(0))
    }

    /// Background preupload never falls back to posting a message. The caller
    /// alone decides whether the user's privacy preference permits this work.
    pub async fn preupload_attachments(
        &self,
        channel: &str,
        files: &[UploadAttachment],
        cancel: &UploadCancellation,
        progress: UploadProgressCallback,
    ) -> Result<Vec<Value>> {
        if !self
            .instance()
            .features
            .get("presigned_attachment_uploads")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Err(Error::InvalidUpload(
                "presigned attachment uploads are disabled",
            ));
        }
        let files = prepare(files, cancel).await?;
        let progress = Arc::new(Progress::new(&files, progress));
        let transport = upload_transport()?;
        let plans = self
            .attachment_plans(channel, &files, &transport.api, cancel, &progress)
            .await?;
        let metadata = self
            .transfer_plans(channel, &files, &plans, &transport, cancel, &progress)
            .await?;
        progress.report(Some(UploadPhase::Complete), None, false);
        Ok(metadata)
    }
    pub async fn send_message_with_attachments(
        &self,
        channel: &str,
        mut message: Value,
        files: &[UploadAttachment],
        cancel: &UploadCancellation,
        progress: UploadProgressCallback,
    ) -> Result<Value> {
        self.send_with_attachments(channel, message, files, cancel, progress, false)
            .await
    }

    pub async fn create_forum_post_with_attachments(
        &self,
        channel: &str,
        post: Value,
        files: &[UploadAttachment],
        cancel: &UploadCancellation,
        progress: UploadProgressCallback,
    ) -> Result<Value> {
        self.send_with_attachments(channel, post, files, cancel, progress, true)
            .await
    }

    async fn send_with_attachments(
        &self,
        channel: &str,
        mut message: Value,
        files: &[UploadAttachment],
        cancel: &UploadCancellation,
        progress: UploadProgressCallback,
        forum: bool,
    ) -> Result<Value> {
        if !message.is_object() || (forum && !message.get("message").is_some_and(Value::is_object))
        {
            return Err(Error::InvalidUpload("message body must be an object"));
        }
        let files = prepare(files, cancel).await?;
        let progress = Arc::new(Progress::new(&files, progress));
        let transport = upload_transport()?;
        let mut ready = None;
        if self
            .instance()
            .features
            .get("presigned_attachment_uploads")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            match self
                .attachment_plans(channel, &files, &transport.api, cancel, &progress)
                .await
            {
                Ok(plans) => match self
                    .transfer_plans(channel, &files, &plans, &transport, cancel, &progress)
                    .await
                {
                    Ok(uploaded) => ready = Some(uploaded),
                    Err(Error::UploadCancelled) => return Err(Error::UploadCancelled),
                    Err(failure @ Error::AttachmentCompletion(_)) => return Err(failure),
                    Err(_) => {
                        if total_size(&files) > FALLBACK_LIMIT {
                            return Err(Error::UploadFallbackUnavailable);
                        }
                    }
                },
                Err(Error::UploadCancelled) => return Err(Error::UploadCancelled),
                Err(error @ Error::AttachmentPlanMissing) => return Err(error),
                Err(error) => {
                    if endpoint_unreachable(&error) && total_size(&files) > FALLBACK_LIMIT {
                        return Err(Error::UploadFallbackUnavailable);
                    }
                }
            }
        }
        let result = if let Some(uploaded) = ready {
            if forum {
                message["message"]["attachments"] = uploaded.into();
            } else {
                message["attachments"] = uploaded.into();
            }
            progress.report(Some(UploadPhase::Sending), None, false);
            if forum {
                cancellable(cancel, self.send_forum_post_body(channel, message)).await
            } else {
                cancellable(cancel, self.send_message_body(channel, message)).await
            }
        } else {
            let metadata: Value = files
                .iter()
                .map(|file| file.attachment.metadata())
                .collect::<Vec<_>>()
                .into();
            if forum {
                message["message"]["attachments"] = metadata;
            } else {
                message["attachments"] = metadata;
            }
            progress.report(Some(UploadPhase::Uploading), None, true);
            let mut form = reqwest::multipart::Form::new()
                .text("payload_json", serde_json::to_string(&message)?);
            for (index, file) in files.iter().enumerate() {
                let progress = progress.clone();
                let bytes: Arc<dyn Fn(u64) + Send + Sync> =
                    Arc::new(move |loaded| progress.report(None, Some((index, loaded)), false));
                let body = file_body(file.clone(), 0, file.size, bytes, cancel.clone()).await?;
                let part = reqwest::multipart::Part::stream_with_length(body, file.size)
                    .file_name(file.attachment.filename.clone())
                    .mime_str(&file.attachment.content_type)
                    .map_err(|_| Error::InvalidUpload("invalid content type"))?;
                form = form.part(format!("files[{index}]"), part);
            }
            let request = self
                .upload_api_request(
                    &transport.api,
                    Method::POST,
                    &format!(
                        "/channels/{}/{}",
                        segment(channel),
                        if forum { "threads" } else { "messages" }
                    ),
                )?
                .multipart(form);
            cancellable(cancel, async {
                Self::parse_api_response(request.send().await?).await
            })
            .await
        }?;
        progress.finish_all();
        progress.report(Some(UploadPhase::Complete), None, false);
        Ok(result)
    }
    async fn request_attachment_plans(
        &self,
        channel: &str,
        files: &[PreparedFile],
        transport: &reqwest::Client,
        cancel: &UploadCancellation,
        progress: &Arc<Progress>,
    ) -> Result<Vec<Plan>> {
        progress.report(Some(UploadPhase::Planning), None, false);
        let body = json!({"attachments":files.iter().map(|file|json!({"id":file.attachment.id,"filename":file.attachment.filename,"file_size":file.size,"content_type":file.attachment.content_type})).collect::<Vec<_>>()});
        let request = self
            .upload_api_request(
                transport,
                Method::POST,
                &format!("/channels/{}/attachments", segment(channel)),
            )?
            .json(&body);
        let response = cancellable(cancel, async {
            Self::parse_api_response(request.send().await?).await
        })
        .await?;
        let plans: Vec<Plan> = serde_json::from_value(
            response
                .get("attachments")
                .cloned()
                .unwrap_or_else(|| json!([])),
        )
        .map_err(|_| Error::InvalidUpload("invalid presigned upload response"))?;
        for plan in &plans {
            validate_plan_response(plan)?;
        }
        Ok(plans)
    }
    async fn attachment_plans(
        &self,
        channel: &str,
        files: &[PreparedFile],
        transport: &reqwest::Client,
        cancel: &UploadCancellation,
        progress: &Arc<Progress>,
    ) -> Result<Vec<Plan>> {
        let plans = self
            .request_attachment_plans(channel, files, transport, cancel, progress)
            .await?;
        // Source maps response IDs instead of assuming the server's array order.
        let indexed = plans
            .iter()
            .map(|plan| Ok((plan.id()?, plan.clone())))
            .collect::<Result<HashMap<_, _>>>()?;
        let mut ordered = Vec::with_capacity(files.len());
        for file in files {
            let plan = indexed
                .get(&file.attachment.id)
                .ok_or(Error::AttachmentPlanMissing)?;
            validate_plan(plan, file)?;
            ordered.push(plan.clone());
        }
        Ok(ordered)
    }
    async fn transfer_plans(
        &self,
        channel: &str,
        files: &[PreparedFile],
        plans: &[Plan],
        transport: &UploadTransport,
        cancel: &UploadCancellation,
        progress: &Arc<Progress>,
    ) -> Result<Vec<Value>> {
        progress.report(Some(UploadPhase::Uploading), None, false);
        let mut complete = Vec::new();
        for (index, (file, plan)) in files.iter().zip(plans).enumerate() {
            match plan {
                Plan::Single { upload_url, .. } => {
                    let report = progress.clone();
                    let bytes: Arc<dyn Fn(u64) + Send + Sync> =
                        Arc::new(move |loaded| report.report(None, Some((index, loaded)), false));
                    put_file(
                        &transport.storage,
                        upload_url,
                        file.clone(),
                        0..file.size,
                        plan.content_type(),
                        bytes,
                        cancel,
                    )
                    .await
                    .map_err(|error| error.error)?;
                }
                Plan::Multipart {
                    upload_filename,
                    upload_id,
                    part_size,
                    parts,
                    ..
                } => {
                    complete.push(Completion {
                        upload_filename: upload_filename.clone(),
                        upload_id: upload_id.clone(),
                    });
                    let loaded = Arc::new(Mutex::new(vec![0_u64; parts.len()]));
                    let part_size = *part_size;
                    stream::iter(parts.clone().into_iter().enumerate().map(
                        |(part_index, part)| {
                            let loaded = loaded.clone();
                            let file = file.clone();
                            let progress = progress.clone();
                            let content_type = plan.content_type().to_owned();
                            async move {
                                let offset = part_index as u64 * part_size;
                                let length = part_size.min(file.size - offset);
                                let bytes: Arc<dyn Fn(u64) + Send + Sync> =
                                    Arc::new(move |bytes| {
                                        if let Ok(mut loaded) = loaded.lock() {
                                            loaded[part_index] = bytes;
                                            progress.report(
                                                None,
                                                Some((index, loaded.iter().sum())),
                                                false,
                                            );
                                        }
                                    });
                                for attempt in 1..=4 {
                                    match put_file(
                                        &transport.storage,
                                        &part.upload_url,
                                        file.clone(),
                                        offset..offset + length,
                                        &content_type,
                                        bytes.clone(),
                                        cancel,
                                    )
                                    .await
                                    {
                                        Ok(()) => return Ok(()),
                                        Err(failure) => {
                                            if matches!(failure.error, Error::UploadCancelled)
                                                || attempt == 4
                                                || !failure.transient
                                            {
                                                return Err(failure.error);
                                            }
                                            bytes(0);
                                            let delay = retry_delay(attempt, failure.retry_after);
                                            cancellable(cancel, async {
                                                tokio::time::sleep(delay).await;
                                                Ok(())
                                            })
                                            .await?;
                                        }
                                    }
                                }
                                unreachable!()
                            }
                        },
                    ))
                    .buffer_unordered(4)
                    .try_collect::<Vec<_>>()
                    .await?;
                }
            }
            progress.finish_file(index);
        }
        if !complete.is_empty() {
            progress.report(Some(UploadPhase::Completing), None, false);
            let body = json!({"uploads":complete});
            let request = self
                .upload_api_request(
                    &transport.api,
                    Method::POST,
                    &format!("/channels/{}/attachments/complete", segment(channel)),
                )?
                .json(&body);
            cancellable(cancel, async {
                Self::parse_api_response(request.send().await?).await
            })
            .await
            .map_err(|error| {
                if matches!(error, Error::UploadCancelled) {
                    error
                } else {
                    Error::AttachmentCompletion(Box::new(error))
                }
            })?;
        }
        Ok(files
            .iter()
            .zip(plans)
            .map(|(file, plan)| {
                let mut metadata = file.attachment.metadata();
                metadata["upload_filename"] = plan.upload_filename().into();
                metadata["file_size"] = plan.file_size().into();
                metadata["content_type"] = plan.content_type().into();
                metadata
            })
            .collect())
    }
}
fn total_size(files: &[PreparedFile]) -> u64 {
    files.iter().map(|file| file.size).sum()
}
fn endpoint_unreachable(error: &Error) -> bool {
    matches!(
        error,
        Error::Http(_)
            | Error::Api {
                status: 500..=599,
                ..
            }
    )
}
// AttachmentUploadPlan.requestPresignedAttachmentUploads validates basic
// response metadata before distributing independent textarea upload promises.
fn validate_plan_response(plan: &Plan) -> Result<()> {
    if plan.upload_filename().is_empty() || plan.filename().is_empty() {
        return Err(Error::InvalidUpload("invalid upload plan metadata"));
    }
    match plan {
        Plan::Single { upload_url, .. } if upload_url.is_empty() => {
            Err(Error::InvalidUpload("missing singlepart URL"))
        }
        Plan::Multipart {
            upload_id, parts, ..
        } if upload_id.is_empty() || parts.is_empty() => {
            Err(Error::InvalidUpload("missing multipart metadata"))
        }
        _ => Ok(()),
    }
}
fn validate_plan(plan: &Plan, file: &PreparedFile) -> Result<()> {
    if plan.upload_filename().is_empty()
        || plan.filename().is_empty()
        || plan.content_type().is_empty()
        || plan.file_size() != file.size
    {
        return Err(Error::InvalidUpload("invalid upload plan metadata"));
    }
    match plan {
        Plan::Single { upload_url, .. } => {
            storage_url(upload_url)?;
        }
        Plan::Multipart {
            part_size,
            parts,
            upload_id,
            ..
        } => {
            if *part_size == 0
                || parts.is_empty()
                || parts.len() > 10_000
                || upload_id.is_empty()
                || parts.len() as u64 != file.size.div_ceil(*part_size)
            {
                return Err(Error::InvalidUpload("invalid multipart upload plan"));
            }
            let mut numbers = std::collections::HashSet::new();
            for part in parts {
                if part.part_number == 0 || !numbers.insert(part.part_number) {
                    return Err(Error::InvalidUpload("invalid multipart part number"));
                }
                storage_url(&part.upload_url)?;
            }
        }
    }
    Ok(())
}
struct PutFailure {
    error: Error,
    transient: bool,
    retry_after: Option<Duration>,
}
async fn put_file(
    transport: &reqwest::Client,
    url: &str,
    file: PreparedFile,
    range: std::ops::Range<u64>,
    content_type: &str,
    bytes: Arc<dyn Fn(u64) + Send + Sync>,
    cancel: &UploadCancellation,
) -> std::result::Result<(), Box<PutFailure>> {
    let length = range.end - range.start;
    let request = async {
        let url = storage_url(url)?;
        let body = file_body(file, range.start, length, bytes, cancel.clone()).await?;
        cancellable(cancel, async {
            transport
                .put(url)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::CONTENT_LENGTH, length)
                .body(body)
                .send()
                .await
                .map_err(|e| Error::Http(e.without_url()))
        })
        .await
    }
    .await;
    let mut response = match request {
        Ok(response) => response,
        Err(error) => {
            let transient = matches!(&error,Error::Http(e) if !e.is_builder());
            return Err(Box::new(PutFailure {
                error,
                transient,
                retry_after: None,
            }));
        }
    };
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(());
    }
    let headers = response.headers().clone();
    let mut body = Vec::new();
    while body.len() < 64 * 1024 {
        let chunk = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Box::new(PutFailure {
                error: Error::UploadCancelled, transient: false, retry_after: None,
            })),
            chunk = response.chunk() => chunk,
        };
        match chunk {
            Ok(Some(bytes)) => {
                let take = bytes.len().min(64 * 1024 - body.len());
                body.extend_from_slice(&bytes[..take]);
            }
            _ => break,
        }
    }
    let json = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
    Err(Box::new(PutFailure {
        error: Error::StorageUpload { status },
        transient: matches!(status, 408 | 425 | 429 | 500..=599),
        retry_after: retry_hint(&json, &headers),
    }))
}
fn retry_hint(body: &Value, headers: &header::HeaderMap) -> Option<Duration> {
    fn positive(v: Option<f64>) -> Option<f64> {
        v.filter(|v| v.is_finite() && *v > 0.)
    }
    let seconds = positive(
        body.pointer("/details/retry/after_seconds")
            .and_then(Value::as_f64),
    )
    .or_else(|| positive(body.get("retry_after").and_then(Value::as_f64)))
    .or_else(|| {
        [header::RETRY_AFTER.as_str(), "x-ratelimit-reset-after"]
            .into_iter()
            .find_map(|name| {
                let value = headers.get(name)?.to_str().ok()?;
                positive(value.parse().ok()).or_else(|| {
                    chrono::DateTime::parse_from_rfc2822(value)
                        .ok()
                        .and_then(|deadline| {
                            positive(Some(
                                deadline.timestamp_millis() as f64 / 1000.
                                    - SystemTime::now()
                                        .duration_since(SystemTime::UNIX_EPOCH)
                                        .ok()?
                                        .as_secs_f64(),
                            ))
                        })
                })
            })
    })?;
    Some(Duration::from_secs_f64(seconds.min(30.)))
}
fn retry_delay(attempt: u32, hint: Option<Duration>) -> Duration {
    let base =
        (500_u64.saturating_mul(1_u64 << attempt.saturating_sub(1).min(10))).min(8000) as f64;
    let jittered = (base * (0.75 + fastrand::f64() * 0.5)).clamp(500., 8000.);
    Duration::from_secs_f64((jittered / 1000.).max(hint.unwrap_or_default().as_secs_f64()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_only_reply_omits_invisible_content_preserving_reference_nonce_and_mentions() {
        let reference = serde_json::from_value(
            json!({"channel_id":"1","message_id":"2","guild_id":"3","type":0}),
        )
        .unwrap();
        let blank = "\u{0000}\u{00ad}\u{034f}\u{061c}\u{115f}\u{1160}\u{17b4}\u{17b5}\u{180e}\u{200b}\u{200f}\u{202a}\u{202e}\u{2060}\u{2069}\u{2800}\u{3164}\u{fe00}\u{fe0f}\u{feff}\u{ffa0}\u{e0100}\u{e01ef} \t\n\u{3000}";
        for content in ["", " \r\n", blank] {
            let body = Client::build_message_body(content, Some("123"), Some(&reference), false);
            assert_eq!(
                body,
                json!({"nonce":"123","message_reference":{"channel_id":"1","message_id":"2","guild_id":"3","type":0},"allowed_mentions":{"replied_user":false}})
            );
        }
        // ECMAScript trim does not classify NEL as whitespace.
        for visible in ["hello", "\u{0085}", "\u{200b}x", "🦀"] {
            assert_eq!(
                Client::build_message_body(visible, None, None, false)["content"],
                visible
            );
        }
    }

    #[test]
    fn signed_urls_do_not_allow_implicit_credentials_or_non_http_transports() {
        for value in [
            "file:///tmp/file",
            "https://user:secret@storage.test/object",
            "https://storage.test/object#secret",
            "broken",
        ] {
            let error = storage_url(value).unwrap_err();
            assert!(!format!("{error:?}").contains("secret"));
        }
        assert!(storage_url("https://storage.test/object?signature=secret").is_ok());
    }

    #[test]
    fn retry_hints_have_source_priority_caps_and_backoff_bounds() {
        let mut headers = header::HeaderMap::new();
        headers.insert(header::RETRY_AFTER, "12".parse().unwrap());
        headers.insert("x-ratelimit-reset-after", "20".parse().unwrap());
        assert_eq!(
            retry_hint(
                &json!({"details":{"retry":{"after_seconds":7}},"retry_after":9}),
                &headers
            ),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            retry_hint(&json!({"retry_after":9}), &headers),
            Some(Duration::from_secs(9))
        );
        assert_eq!(
            retry_hint(&Value::Null, &headers),
            Some(Duration::from_secs(12))
        );
        assert_eq!(
            retry_hint(&json!({"retry_after":99999}), &headers),
            Some(Duration::from_secs(30))
        );
        for attempt in 1..10 {
            let delay = retry_delay(attempt, None);
            assert!(delay >= Duration::from_millis(500) && delay <= Duration::from_millis(8000));
        }
        assert_eq!(
            retry_delay(1, Some(Duration::from_secs(30))),
            Duration::from_secs(30)
        );
    }

    #[tokio::test]
    async fn cancellation_before_subscribe_is_observed_without_lost_wakeup() {
        let cancel = UploadCancellation::new();
        cancel.cancel();
        tokio::time::timeout(Duration::from_millis(50), cancel.cancelled())
            .await
            .unwrap();
        assert!(matches!(
            cancellable(&cancel, async { Ok(()) }).await,
            Err(Error::UploadCancelled)
        ));
    }

    #[test]
    fn zero_byte_files_only_complete_after_their_transfer_succeeds() {
        let file = PreparedFile {
            attachment: UploadAttachment::new("0", "/unused", "empty", "text/plain"),
            size: 0,
            modified: None,
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        let captured = events.clone();
        let progress = Progress::new(
            &[file],
            Arc::new(move |event| captured.lock().unwrap().push(event)),
        );
        progress.report(None, Some((0, 0)), false);
        assert_eq!(events.lock().unwrap().last().unwrap().completed_files, 0);
        progress.finish_file(0);
        assert_eq!(events.lock().unwrap().last().unwrap().completed_files, 1);
    }
    #[test]
    fn textarea_progress_reports_retry_resets_while_message_progress_stays_monotonic() {
        let file = PreparedFile {
            attachment: UploadAttachment::new("0", "/unused", "file", "text/plain"),
            size: 10,
            modified: None,
        };
        for monotonic in [false, true] {
            let events = Arc::new(Mutex::new(Vec::new()));
            let captured = events.clone();
            let mut progress = Progress::new(
                std::slice::from_ref(&file),
                Arc::new(move |event| captured.lock().unwrap().push(event)),
            );
            progress.monotonic = monotonic;
            progress.report(None, Some((0, 8)), false);
            progress.report(None, Some((0, 0)), false);
            assert_eq!(
                events.lock().unwrap().last().unwrap().uploaded_bytes,
                if monotonic { 8 } else { 0 }
            );
        }
    }
}
