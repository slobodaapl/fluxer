// SPDX-License-Identifier: AGPL-3.0-or-later
//! Composer staging is worker-owned; this module only renders bounded previews.
use super::*;
use fluxer_client::{AttachmentPreview, AttachmentStatus, StagedAttachment};
use std::{cell::RefCell, collections::BTreeMap, path::PathBuf};

#[derive(Clone, Copy)]
pub(super) struct Limits {
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub preupload: bool,
}
pub(super) struct AttachmentUi {
    pub channels: BTreeMap<Snowflake, Vec<StagedAttachment>>,
    pub limits: BTreeMap<Snowflake, Limits>,
    previews: RefCell<PreviewCache>,
    pub fixture: bool,
    pub cancelling: BTreeSet<Snowflake>,
}
impl AttachmentUi {
    pub fn new() -> Self {
        Self {
            channels: BTreeMap::new(),
            limits: BTreeMap::new(),
            previews: RefCell::new(PreviewCache::default()),
            fixture: false,
            cancelling: BTreeSet::new(),
        }
    }
}
#[derive(Default)]
struct PreviewCache {
    images: BTreeMap<(Snowflake, u64), CachedPreview>,
    clock: u64,
}
struct CachedPreview {
    revision: u64,
    image: Arc<RenderImage>,
    touched: u64,
}
impl PreviewCache {
    fn get(
        &mut self,
        channel: Snowflake,
        id: u64,
        preview: &AttachmentPreview,
    ) -> (Option<Arc<RenderImage>>, Vec<Arc<RenderImage>>) {
        self.clock += 1;
        let key = (channel, id);
        let mut retired = Vec::new();
        if let Some(cached) = self.images.get_mut(&key)
            && cached.revision == preview.revision
        {
            cached.touched = self.clock;
            return (Some(cached.image.clone()), retired);
        }
        if let Some(old) = self.images.remove(&key) {
            retired.push(old.image);
        }
        if preview.width == 0
            || preview.height == 0
            || preview.width > 256
            || preview.height > 256
            || preview.bgra.len() != preview.width as usize * preview.height as usize * 4
        {
            return (None, retired);
        }
        let Some(buffer) = image::RgbaImage::from_raw(
            preview.width,
            preview.height,
            preview.bgra.as_ref().clone(),
        ) else {
            return (None, retired);
        };
        let image = Arc::new(RenderImage::new(smallvec::smallvec![image::Frame::new(
            buffer
        )]));
        self.images.insert(
            key,
            CachedPreview {
                revision: preview.revision,
                image: image.clone(),
                touched: self.clock,
            },
        );
        if self.images.len() > 64
            && let Some(oldest) = self
                .images
                .iter()
                .min_by_key(|(_, value)| value.touched)
                .map(|(key, _)| *key)
            && let Some(old) = self.images.remove(&oldest)
        {
            retired.push(old.image);
        }
        (Some(image), retired)
    }
}

impl Fluxer {
    pub(super) fn message_pending(&self) -> bool {
        self.state
            .selected_channel
            .is_some_and(|channel| self.pending_messages.contains_key(&channel))
    }
    pub(super) fn complete_pending_message(&mut self, channel: Snowflake, content: &str) -> bool {
        let completed = complete_pending(&mut self.pending_messages, channel, content);
        if completed {
            self.attachments.cancelling.remove(&channel);
        }
        completed
    }
    pub(super) fn has_attachments(&self) -> bool {
        self.state
            .selected_channel
            .and_then(|channel| self.attachments.channels.get(&channel))
            .is_some_and(|stages| !stages.is_empty())
    }
    pub(super) fn attachment_block_reason(&self) -> Option<String> {
        if self.editing.is_some() {
            return Some("Finish editing before adding files".into());
        }
        let channel = self
            .state
            .selected_channel
            .and_then(|id| self.state.channels.get(&id));
        let Some(channel) = channel else {
            return Some("Choose a conversation first".into());
        };
        let Some(user) = &self.state.current_user else {
            return Some("Sign in to attach files".into());
        };
        let permissions = self.state.permissions_for(channel.id, user.id);
        if !can_send_message(channel.kind, permissions)
            || !permissions.contains(fluxer_core::Permissions::ATTACH_FILES)
        {
            return Some("You cannot attach files in this channel".into());
        }
        if let Some(limits) = self.attachments.limits.get(&channel.id)
            && self
                .attachments
                .channels
                .get(&channel.id)
                .map_or(0, Vec::len)
                >= limits.max_files
        {
            return Some(format!("Up to {} files per message", limits.max_files));
        }
        None
    }
    pub(super) fn prompt_attachments(&mut self, cx: &mut Context<Self>) {
        if let Some(reason) = self.attachment_block_reason() {
            self.error = Some(reason);
            cx.notify();
            return;
        }
        let channel_id = self
            .state
            .selected_channel
            .expect("permission checked channel");
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Add files to your message".into()),
        });
        cx.spawn(async move |entity, cx| {
            let result = picked.await;
            let _ = entity.update(cx, |this, cx| match result {
                Ok(Ok(Some(paths))) => this.stage_attachment_paths(channel_id, paths, cx),
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    this.error = Some(format!("Could not open the file picker: {error}"));
                    cx.notify();
                }
                Err(_) => {
                    this.error = Some("The file picker closed unexpectedly".into());
                    cx.notify();
                }
            });
        })
        .detach();
    }
    pub(super) fn stage_attachment_paths(
        &mut self,
        channel_id: Snowflake,
        paths: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if self.state.selected_channel == Some(channel_id)
            && let Some(reason) = self.attachment_block_reason()
        {
            self.error = Some(reason);
            cx.notify();
            return;
        }
        if !paths.is_empty() {
            self.command(Command::StageFiles { channel_id, paths }, cx);
        }
    }
    pub(super) fn stage_clipboard_image(
        &mut self,
        image: &input::ImagePasted,
        cx: &mut Context<Self>,
    ) {
        if let Some(reason) = self.attachment_block_reason() {
            self.error = Some(reason);
            cx.notify();
            return;
        }
        if let Some(channel_id) = self.state.selected_channel {
            self.command(
                Command::StageClipboardImage {
                    channel_id,
                    filename: format!("pasted-image.{}", image.extension),
                    mime_type: image.content_type.into(),
                    bytes: image.bytes.clone(),
                },
                cx,
            );
        }
    }
    pub(super) fn update_attachments(
        &mut self,
        channel: Snowflake,
        stages: Vec<StagedAttachment>,
        cx: &mut Context<Self>,
    ) {
        let mut cache = self.attachments.previews.borrow_mut();
        let obsolete: Vec<_> = cache
            .images
            .iter()
            .filter(|((cached_channel, id), entry)| {
                *cached_channel == channel
                    && !stages.iter().any(|stage| {
                        stage.id == *id
                            && stage
                                .preview
                                .as_ref()
                                .is_some_and(|preview| preview.revision == entry.revision)
                    })
            })
            .map(|(key, _)| *key)
            .collect();
        for key in obsolete {
            if let Some(old) = cache.images.remove(&key) {
                cx.defer(move |cx| cx.drop_image(old.image, None));
            }
        }
        drop(cache);
        if stages.is_empty() {
            self.attachments.channels.remove(&channel);
        } else {
            self.attachments.channels.insert(channel, stages);
        }
        cx.notify();
    }
    pub(super) fn clear_attachments(&mut self, cx: &mut Context<Self>) {
        self.attachments.channels.clear();
        self.attachments.limits.clear();
        self.attachments.fixture = false;
        self.attachments.cancelling.clear();
        for (_, cached) in std::mem::take(&mut self.attachments.previews.borrow_mut().images) {
            cx.defer(move |cx| cx.drop_image(cached.image, None));
        }
        cx.notify();
    }
    fn attachment_preview(
        &self,
        channel: Snowflake,
        stage: &StagedAttachment,
        cx: &mut Context<Self>,
    ) -> Option<Arc<RenderImage>> {
        let preview = stage.preview.as_ref()?;
        let (image, retired) = self
            .attachments
            .previews
            .borrow_mut()
            .get(channel, stage.id, preview);
        for image in retired {
            cx.defer(move |cx| cx.drop_image(image, None));
        }
        image
    }
    pub(super) fn attachment_strip(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(channel) = self.state.selected_channel else {
            return div().into_any_element();
        };
        let stages = self.attachments.channels.get(&channel);
        let compact = self.pane == Pane::Call;
        div()
            .flex()
            .flex_col()
            .gap_1()
            .when(self.message_pending() && self.has_attachments(), |view| {
                view.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(11.))
                                .text_color(rgb(MUTED))
                                .child("Sending attachments…"),
                        )
                        .child(
                            button(
                                "cancel-attachment-send",
                                if self.attachments.cancelling.contains(&channel) {
                                    "Cancelling…"
                                } else {
                                    "Cancel send"
                                },
                            )
                            .px_2()
                            .py_1()
                            .text_size(px(11.))
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if this.attachments.cancelling.insert(channel) {
                                        this.command(
                                            Command::CancelAttachmentSend {
                                                channel_id: channel,
                                            },
                                            cx,
                                        );
                                    }
                                },
                            )),
                        ),
                )
            })
            .when_some(
                stages.filter(|stages| !stages.is_empty()),
                |view, stages| {
                    view.child(
                        div()
                            .id("staged-attachments")
                            .h(px(if compact { 110. } else { 142. }))
                            .overflow_x_scroll()
                            .flex()
                            .gap_2()
                            .children(stages.iter().map(|stage| {
                                let id = stage.id;
                                let spoiler = stage.spoiler;
                                let active = matches!(
                                    stage.status,
                                    AttachmentStatus::Uploading
                                        | AttachmentStatus::Queued
                                        | AttachmentStatus::Sending
                                        | AttachmentStatus::Preparing
                                );
                                let frozen = matches!(
                                    stage.status,
                                    AttachmentStatus::Queued | AttachmentStatus::Sending
                                );
                                let retry = matches!(
                                    stage.status,
                                    AttachmentStatus::Failed | AttachmentStatus::Cancelled
                                );
                                let label = status_label(stage, self.attachments.fixture);
                                let preview = if spoiler {
                                    None
                                } else {
                                    self.attachment_preview(channel, stage, cx)
                                };
                                div()
                                    .id(("staged-file", id))
                                    .w(px(160.))
                                    .flex_shrink_0()
                                    .rounded_md()
                                    .overflow_hidden()
                                    .bg(rgb(0x302b3b))
                                    .border_1()
                                    .border_color(rgb(if retry { 0xa54c62 } else { 0x494053 }))
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .h(px(if compact { 28. } else { 56. }))
                                            .flex_shrink_0()
                                            .bg(rgb(0x1a1722))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .when_some(preview, |view, image| {
                                                view.child(
                                                    img(image)
                                                        .size_full()
                                                        .object_fit(ObjectFit::Contain),
                                                )
                                            })
                                            .when(stage.preview.is_none() || spoiler, |view| {
                                                view.child(
                                                    div()
                                                        .text_size(px(if compact {
                                                            12.
                                                        } else {
                                                            20.
                                                        }))
                                                        .text_color(rgb(MUTED))
                                                        .child(if spoiler {
                                                            "SPOILER"
                                                        } else {
                                                            "▤"
                                                        }),
                                                )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .px_2()
                                            .pt_1()
                                            .text_size(px(11.))
                                            .text_ellipsis()
                                            .child(stage.filename.clone()),
                                    )
                                    .child(
                                        div()
                                            .px_2()
                                            .text_size(px(10.))
                                            .text_color(rgb(MUTED))
                                            .child(format!(
                                                "{} · {label}",
                                                format_bytes(stage.size)
                                            )),
                                    )
                                    .when(
                                        matches!(
                                            stage.status,
                                            AttachmentStatus::Uploading | AttachmentStatus::Sending
                                        ),
                                        |view| {
                                            view.child(
                                                div().h(px(3.)).w_full().bg(rgb(0x50465e)).child(
                                                    div()
                                                        .h_full()
                                                        .w(relative(safe_progress(stage.progress)))
                                                        .bg(rgb(ACCENT)),
                                                ),
                                            )
                                        },
                                    )
                                    .when_some(stage.error.as_ref(), |view, error| {
                                        view.child(
                                            div()
                                                .px_2()
                                                .text_size(px(10.))
                                                .text_color(rgb(0xf0a1b5))
                                                .text_ellipsis()
                                                .child(error.clone()),
                                        )
                                    })
                                    .child(
                                        div()
                                            .mt_auto()
                                            .px_1()
                                            .pb_1()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .child(
                                                button(
                                                    ("attachment-spoiler", id),
                                                    if spoiler {
                                                        "Unmark spoiler"
                                                    } else {
                                                        "Mark spoiler"
                                                    },
                                                )
                                                .px_1()
                                                .py_0()
                                                .text_size(px(10.))
                                                .when(frozen, |view| {
                                                    view.opacity(0.4)
                                                        .cursor(CursorStyle::OperationNotAllowed)
                                                })
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    if !frozen {
                                                        this.attachment_spoiler(
                                                            channel, id, !spoiler, cx,
                                                        );
                                                    }
                                                })),
                                            )
                                            .when(retry, |view| {
                                                view.child(
                                                    button(("attachment-retry", id), "Retry")
                                                        .px_1()
                                                        .py_0()
                                                        .text_size(px(10.))
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.attachment_retry(
                                                                    channel, id, cx,
                                                                )
                                                            },
                                                        )),
                                                )
                                            })
                                            .when(active, |view| {
                                                view.child(
                                                    button(("attachment-cancel", id), "Cancel")
                                                        .px_1()
                                                        .py_0()
                                                        .text_size(px(10.))
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.attachment_cancel(
                                                                    channel, id, cx,
                                                                )
                                                            },
                                                        )),
                                                )
                                            })
                                            .child(
                                                button(("attachment-remove", id), "×")
                                                    .ml_auto()
                                                    .px_1()
                                                    .py_0()
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.attachment_remove(channel, id, cx)
                                                        },
                                                    )),
                                            ),
                                    )
                            })),
                    )
                },
            )
            .when_some(self.attachments.limits.get(&channel), |view, limits| {
                view.child(
                    div()
                        .text_size(px(10.))
                        .text_color(rgb(MUTED))
                        .child(format!(
                            "Up to {} files · {} each · {}",
                            limits.max_files,
                            format_bytes(limits.max_file_bytes),
                            if self.attachments.fixture {
                                "Offline: local staging only"
                            } else if limits.preupload {
                                "Files upload as you add them"
                            } else {
                                "Files upload when you send"
                            }
                        )),
                )
            })
            .into_any_element()
    }
    pub(super) fn attachment_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let reason = self.attachment_block_reason();
        button("add-attachment", "+ File")
            .flex_shrink_0()
            .when(reason.is_some(), |view| view.opacity(0.5))
            .on_click(cx.listener(|this, _, _, cx| this.prompt_attachments(cx)))
            .into_any_element()
    }
    fn attachment_remove(&mut self, channel: Snowflake, id: u64, cx: &mut Context<Self>) {
        self.command(
            Command::RemoveAttachment {
                channel_id: channel,
                id,
            },
            cx,
        );
    }
    fn attachment_cancel(&mut self, channel: Snowflake, id: u64, cx: &mut Context<Self>) {
        self.command(
            Command::CancelAttachment {
                channel_id: channel,
                id,
            },
            cx,
        );
    }
    fn attachment_retry(&mut self, channel: Snowflake, id: u64, cx: &mut Context<Self>) {
        self.command(
            Command::RetryAttachment {
                channel_id: channel,
                id,
            },
            cx,
        );
    }
    fn attachment_spoiler(
        &mut self,
        channel: Snowflake,
        id: u64,
        spoiler: bool,
        cx: &mut Context<Self>,
    ) {
        self.command(
            Command::SetAttachmentSpoiler {
                channel_id: channel,
                id,
                spoiler,
            },
            cx,
        );
    }
    pub(super) fn install_attachment_fixture(&mut self, cx: &mut Context<Self>) {
        self.attachments.fixture = true;
        if let Some(channel_id) = self.state.selected_channel {
            let bytes = Arc::new(
                include_bytes!("../../../../fluxer_app/src/media/images/avatars/0.png").to_vec(),
            );
            self.command(
                Command::StageClipboardImage {
                    channel_id,
                    filename: "offline-example.png".into(),
                    mime_type: "image/png".into(),
                    bytes,
                },
                cx,
            );
            let paths = std::env::args()
                .filter_map(|arg| {
                    arg.strip_prefix("--demo-attachment-file=")
                        .map(PathBuf::from)
                })
                .collect::<Vec<_>>();
            if !paths.is_empty() {
                self.command(Command::StageFiles { channel_id, paths }, cx);
            }
        }
    }
}
fn safe_progress(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0., 1.)
    } else {
        0.
    }
}
fn status_label(stage: &StagedAttachment, fixture: bool) -> String {
    let label = match stage.status {
        AttachmentStatus::Preparing => "Preparing…",
        AttachmentStatus::Staged => "Ready to send",
        AttachmentStatus::Uploading => "Uploading",
        AttachmentStatus::Ready => "Uploaded",
        AttachmentStatus::Queued => "Queued to send",
        AttachmentStatus::Sending => "Sending…",
        AttachmentStatus::Failed => "Failed",
        AttachmentStatus::Cancelled => "Cancelled",
    };
    let label = if matches!(
        stage.status,
        AttachmentStatus::Uploading | AttachmentStatus::Sending
    ) {
        format!("{label} {:.0}%", safe_progress(stage.progress) * 100.)
    } else {
        label.into()
    };
    if fixture {
        format!("Local: {label}")
    } else {
        label
    }
}
fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024. * 1024.))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.)
    } else {
        format!("{bytes} B")
    }
}

pub(super) fn sent_draft_can_clear(
    selected: Option<Snowflake>,
    channel: Snowflake,
    sent: &str,
    current: &str,
) -> bool {
    selected == Some(channel) && sent == current
}

fn complete_pending(
    pending: &mut BTreeMap<Snowflake, String>,
    channel: Snowflake,
    content: &str,
) -> bool {
    if pending
        .get(&channel)
        .is_some_and(|current| current == content)
    {
        pending.remove(&channel);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{PreviewCache, complete_pending, sent_draft_can_clear};
    use fluxer_client::AttachmentPreview;
    use fluxer_core::Snowflake;
    use std::{collections::BTreeMap, sync::Arc};
    #[test]
    fn successful_send_preserves_newly_typed_text_and_other_channel_drafts() {
        assert!(sent_draft_can_clear(
            Some(Snowflake(1)),
            Snowflake(1),
            "sent",
            "sent"
        ));
        assert!(!sent_draft_can_clear(
            Some(Snowflake(1)),
            Snowflake(1),
            "sent",
            "new draft typed during upload"
        ));
        assert!(!sent_draft_can_clear(
            Some(Snowflake(2)),
            Snowflake(1),
            "same text",
            "same text"
        ));
        assert!(sent_draft_can_clear(
            Some(Snowflake(1)),
            Snowflake(1),
            "",
            ""
        ));
        assert!(!sent_draft_can_clear(
            Some(Snowflake(1)),
            Snowflake(1),
            "",
            "next message"
        ));
    }
    #[test]
    fn acknowledgements_do_not_clear_other_channels_or_newer_pending_content() {
        let mut pending = BTreeMap::from([
            (Snowflake(1), "old".into()),
            (Snowflake(2), "elsewhere".into()),
        ]);
        assert!(!complete_pending(
            &mut pending,
            Snowflake(1),
            "stale response"
        ));
        assert!(complete_pending(&mut pending, Snowflake(1), "old"));
        assert_eq!(
            pending.get(&Snowflake(2)).map(String::as_str),
            Some("elsewhere")
        );
        pending.insert(Snowflake(1), "newer".into());
        assert!(!complete_pending(&mut pending, Snowflake(1), "old"));
        assert_eq!(pending[&Snowflake(1)], "newer");
    }
    #[test]
    fn prepared_previews_are_bounded_reused_and_revision_replacement_retires_old_image() {
        let mut cache = PreviewCache::default();
        let mut preview = AttachmentPreview {
            revision: 1,
            width: 2,
            height: 1,
            bgra: Arc::new(vec![0, 1, 2, 255, 3, 4, 5, 255]),
        };
        let (first, retired) = cache.get(Snowflake(1), 1, &preview);
        assert!(retired.is_empty());
        let first = first.unwrap();
        assert!(Arc::ptr_eq(
            &first,
            &cache.get(Snowflake(1), 1, &preview).0.unwrap()
        ));
        preview.revision = 2;
        let (replacement, retired) = cache.get(Snowflake(1), 1, &preview);
        assert_ne!(replacement.unwrap().id, first.id);
        assert_eq!(retired[0].id, first.id);
        preview.width = 100_000;
        assert!(cache.get(Snowflake(2), 1, &preview).0.is_none());
        preview.width = 2;
        let mut evicted = 0;
        for id in 2..=65 {
            evicted += cache.get(Snowflake(1), id, &preview).1.len();
        }
        assert_eq!(cache.images.len(), 64);
        assert_eq!(evicted, 1);
    }
}
