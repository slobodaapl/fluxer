// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exact clicked-media menus and explicit, credential-free Save/Copy operations.
use super::*;
use crate::{
    context_menu::{ContextMenuEvent, ContextMenuItem as Item},
    controls::{Icon, icon_button},
    media_context_policy::{self as policy, MediaAccess, MediaContext, MediaTarget},
    media_download::Download,
    media_export::{self, ExportJob},
    message_actions::MessageMenuContext,
};
use std::{collections::VecDeque, path::PathBuf, time::Instant};

#[derive(Clone, Debug)]
pub(super) struct MediaReference {
    pub message: Arc<Message>,
    pub target: MediaTarget,
    pub surface: &'static str,
}
impl PartialEq for MediaReference {
    fn eq(&self, other: &Self) -> bool {
        self.message.id == other.message.id
            && self.message.channel_id == other.message.channel_id
            && self.target == other.target
            && self.surface == other.surface
    }
}
impl Eq for MediaReference {}
impl MediaReference {
    pub fn new(message: Arc<Message>, target: MediaTarget, surface: &'static str) -> Self {
        Self {
            message,
            target,
            surface,
        }
    }
}
#[derive(Default)]
pub(super) struct MediaUi {
    pub active: bool,
    jobs: Vec<Transfer>,
    notices: VecDeque<(String, Instant)>,
    generation: u64,
    next_id: u64,
}
impl MediaUi {
    pub fn reset(&mut self) {
        self.jobs.clear();
        self.notices.clear();
        self.generation = self.generation.wrapping_add(1);
    }
}
enum Intent {
    Save(PathBuf),
    CopyImage,
}
enum Stage {
    Download(Download),
    Save {
        _file: Arc<tempfile::NamedTempFile>,
        job: ExportJob<()>,
    },
    Copy {
        _file: Arc<tempfile::NamedTempFile>,
        job: ExportJob<Vec<u8>>,
    },
    ReadyCopy {
        _file: Arc<tempfile::NamedTempFile>,
        bytes: Vec<u8>,
    },
}
struct Transfer {
    id: u64,
    reference: MediaReference,
    resource_key: String,
    name: String,
    intent: Intent,
    stage: Stage,
    percent: Option<u8>,
}
fn row(id: &str, label: &str, icon: Icon) -> Item {
    Item::Action {
        id: id.into(),
        label: label.into(),
        icon: Some(icon),
        enabled: true,
        danger: false,
    }
}

fn live_media_message<'a>(
    state: &'a ClientState,
    browser: Option<&'a Message>,
    reference: &MediaReference,
) -> Option<&'a Message> {
    if reference.surface == "browser" {
        browser.filter(|message| {
            message.id == reference.message.id && message.channel_id == reference.message.channel_id
        })
    } else {
        state
            .messages
            .get(&reference.message.channel_id)?
            .get(&reference.message.id)
    }
}

fn media_menu_title(media: &MediaContext, revealed: &BTreeSet<String>) -> String {
    if media.concealment.is_some() && !revealed.contains(&media.reveal_key) {
        match media.concealment {
            Some(crate::playable_media::Concealment::Sensitive) => "Sensitive media",
            _ => "Spoiler",
        }
        .to_string()
    } else {
        media.filename.clone()
    }
}

impl Fluxer {
    pub(super) fn schedule_media_menu_demo(&self, window: &Window, cx: &mut Context<Self>) {
        if !self.demo
            || !std::env::args().any(|arg| arg == "--demo-images" || arg == "--demo-media")
        {
            return;
        }
        let Ok(kind) = std::env::var("FLUXER_MEDIA_MENU_DEMO") else {
            return;
        };
        if kind == "composer" {
            let handle = window.window_handle();
            cx.spawn(async move |entity, cx| {
                Timer::after(Duration::from_millis(700)).await;
                let _ = handle.update(cx, |_, window, cx| {
                    let _ = entity.update(cx, |view, cx| {
                        view.composer
                            .update(cx, |input, cx| input.set("Menu fixture 🙂".into(), cx));
                        view.composer.focus_handle(cx).focus(window);
                    });
                });
            })
            .detach();
            return;
        }
        let (message_id, asset, snapshot) = match kind.as_str() {
            "image" | "image-modal" | "hidden" => (
                30000,
                policy::MediaAsset::Attachment(Snowflake(31000)),
                None,
            ),
            "embed" => (30001, policy::MediaAsset::Embed(0), None),
            "forward" => (
                30002,
                policy::MediaAsset::Attachment(Snowflake(31002)),
                Some(0),
            ),
            "spoiler" => (
                30003,
                policy::MediaAsset::Attachment(Snowflake(31003)),
                None,
            ),
            "expired" => (
                30003,
                policy::MediaAsset::Attachment(Snowflake(31005)),
                None,
            ),
            "video" | "native-modal" => (
                70000,
                policy::MediaAsset::Attachment(Snowflake(71000)),
                None,
            ),
            "audio" => (
                70001,
                policy::MediaAsset::Attachment(Snowflake(71001)),
                None,
            ),
            "gifv" => (70002, policy::MediaAsset::Embed(0), None),
            _ => return,
        };
        let handle = window.window_handle();
        cx.spawn(async move |entity, cx| {
            Timer::after(Duration::from_millis(700)).await;
            let _ = handle.update(cx, |_, window, cx| {
                let _ = entity.update(cx, |view, cx| {
                    let Some(channel) = view.state.selected_channel else {
                        return;
                    };
                    if kind == "hidden" {
                        let parent = Snowflake(99000);
                        view.state.channels.insert(
                            parent,
                            fluxer_core::Channel {
                                id: parent,
                                kind: ChannelType::Media,
                                guild_id: Some(Snowflake(1)),
                                extra: BTreeMap::from([(
                                    "flags".into(),
                                    serde_json::json!(1 << 15),
                                )]),
                                ..Default::default()
                            },
                        );
                        if let Some(channel) = view.state.channels.get_mut(&channel) {
                            channel.kind = ChannelType::PublicThread;
                            channel.parent_id = Some(parent);
                        }
                    }
                    let Some(message) = view
                        .state
                        .messages
                        .get(&channel)
                        .and_then(|messages| messages.get(&Snowflake(message_id)))
                        .cloned()
                    else {
                        return;
                    };
                    let reference = MediaReference::new(
                        Arc::new(message),
                        MediaTarget {
                            snapshot_index: snapshot,
                            asset,
                        },
                        "timeline",
                    );
                    if kind == "image-modal"
                        && let Some(media) = view.media_descriptor(&reference)
                        && let Some(source) = media.image_source
                    {
                        view.open_image_gallery(
                            vec![crate::image_gallery::ImageItem {
                                context: Some(reference.clone()),
                                source,
                                original: media.original_url,
                                name: media.filename,
                                width: Some(180),
                                height: Some(100),
                                animated: false,
                            }],
                            0,
                            window,
                            cx,
                        );
                    }
                    if kind == "native-modal"
                        && let Some(media) = view.media_descriptor(&reference)
                    {
                        view.playback.modal.open(
                            format!("timeline:{}", media.resource_key),
                            window,
                            cx,
                        );
                    }
                    if !view.image_viewer.is_open() && !view.playback.modal.is_open() {
                        view.focus.focus(window);
                    }
                    let size = window.viewport_size();
                    view.open_context_menu(
                        crate::context_actions::Target::Media(reference),
                        point(size.width - px(24.), size.height - px(24.)),
                        window,
                        cx,
                    );
                });
            });
        })
        .detach();
    }
    fn current_media_message<'a>(&'a self, reference: &MediaReference) -> Option<&'a Message> {
        live_media_message(
            &self.state,
            self.current_browser_media_message(reference.message.channel_id, reference.message.id),
            reference,
        )
    }
    pub(super) fn media_descriptor(&self, reference: &MediaReference) -> Option<MediaContext> {
        let message = self.current_media_message(reference)?;
        let user = self.state.current_user.as_ref()?;
        if !self.state.channels.contains_key(&message.channel_id)
            || !self
                .state
                .permissions_for(message.channel_id, user.id)
                .contains(fluxer_core::Permissions::VIEW_CHANNEL)
        {
            return None;
        }
        let now = chrono::Utc::now().timestamp_millis();
        let resolve = |message: &Message| match reference.target.asset {
            policy::MediaAsset::Attachment(id) => policy::resolve_attachment(
                message,
                id,
                reference.target.snapshot_index,
                &self.state.channels,
                &self.revealed_media,
                now,
            ),
            policy::MediaAsset::EmbedVideo(_) => policy::resolve(
                message,
                &reference.target,
                &self.state.channels,
                &self.revealed_media,
                rich_message::context_embed_spoiler(message, &reference.target),
                now,
            ),
            policy::MediaAsset::Embed(index) => policy::resolve_embed(
                message,
                index,
                reference.target.snapshot_index,
                &self.state.channels,
                &self.revealed_media,
                rich_message::context_embed_spoiler(message, &reference.target),
                now,
            ),
        };
        let context = resolve(message)?;
        // A menu/dialog must not silently switch to a replacement attachment.
        if context.resource_key != resolve(&reference.message)?.resource_key {
            return None;
        }
        Some(context)
    }
    pub(super) fn media_context_items(&self, reference: &MediaReference) -> Vec<Item> {
        let Some(media) = self.media_descriptor(reference) else {
            return Vec::new();
        };
        let title = media_menu_title(&media, &self.revealed_media);
        let mut items = vec![Item::Header(title)];
        if media.available() {
            items.push(row("media-save", "Save as…", Icon::File));
            if media.can_copy_image() {
                items.push(row("media-copy-image", "Copy image", Icon::Copy));
            }
            if media.original_url.is_some() {
                items.extend([
                    row("media-copy-link", "Copy media link", Icon::Copy),
                    row("media-open", "Open original", Icon::Jump),
                ]);
            }
        } else if media.concealment.is_some()
            && !self.revealed_media.contains(&media.reveal_key)
            && media.access != MediaAccess::Expired
        {
            items.push(row("media-reveal", "Reveal media", Icon::Eye));
        }
        if media.attachment_id.is_some() {
            items.push(row("media-copy-id", "Copy attachment ID", Icon::Copy));
        }
        let Some(message) = self.current_media_message(reference) else {
            return items;
        };
        let scope = if reference.surface == "timeline" {
            MessageMenuContext::Timeline
        } else {
            MessageMenuContext::Preview
        };
        let actions = self.message_action_options(message, scope);
        if !actions.is_empty() {
            items.push(Item::Separator);
        }
        for action in actions {
            items.push(row(
                &format!("message:{action:?}"),
                action.label(message.pinned),
                action.icon(),
            ));
        }
        items
    }
    pub(super) fn perform_media_context_action(
        &mut self,
        reference: &MediaReference,
        event: ContextMenuEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ContextMenuEvent::Action(id) = event else {
            return;
        };
        let Some(media) = self.media_descriptor(reference) else {
            return;
        };
        if let Some(wanted) = id.strip_prefix("message:") {
            let Some(message) = self.current_media_message(reference).cloned() else {
                return;
            };
            let scope = if reference.surface == "timeline" {
                MessageMenuContext::Timeline
            } else {
                MessageMenuContext::Preview
            };
            if let Some(action) = self
                .message_action_options(&message, scope)
                .into_iter()
                .find(|action| format!("{action:?}") == wanted)
            {
                if action != crate::message_actions::MessageAction::Copy {
                    if self.image_viewer.is_open() {
                        self.dismiss_image_viewer(window, cx);
                    }
                    if self.playback.modal.is_open() {
                        self.playback.close(window, cx);
                    }
                }
                self.perform_message_action(
                    message,
                    action,
                    window.mouse_position(),
                    false,
                    window,
                    cx,
                );
            }
            return;
        }
        match id.as_str() {
            "media-reveal"
                if media.concealment.is_some() && media.access != MediaAccess::Expired =>
            {
                self.revealed_media.insert(media.reveal_key);
                cx.notify();
            }
            "media-copy-id" => {
                if let Some(id) = media.attachment_id {
                    cx.write_to_clipboard(ClipboardItem::new_string(id.to_string()));
                }
            }
            "media-copy-link" if media.available() => {
                if let Some(url) = media.original_url {
                    cx.write_to_clipboard(ClipboardItem::new_string(url));
                }
            }
            "media-open" if media.available() => {
                if let Some(url) = media.original_url {
                    self.activate_message_target(
                        &message_markup::MessageTarget::Link(url),
                        window,
                        cx,
                    );
                }
            }
            "media-copy-image" if media.can_copy_image() => {
                self.start_media_transfer(reference.clone(), media, Intent::CopyImage, cx)
            }
            "media-save" if media.available() => {
                let home = std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
                    .unwrap_or_else(|| PathBuf::from("/"));
                let directory = if home.join("Downloads").is_dir() {
                    home.join("Downloads")
                } else {
                    home
                };
                let name = media_export::safe_filename(&media.suggested_filename());
                let picked = cx.prompt_for_new_path(&directory, Some(&name));
                let reference = reference.clone();
                let revision = media.resource_key;
                let generation = self.media_ui.generation;
                cx.spawn(async move |entity, cx| {
                    let result = picked.await;
                    let _ = entity.update(cx, |this, cx| {
                        if generation != this.media_ui.generation {
                            return;
                        }
                        match result {
                            Ok(Ok(Some(path))) => {
                                if let Some(media) =
                                    this.media_descriptor(&reference).filter(|media| {
                                        media.available() && media.resource_key == revision
                                    })
                                {
                                    this.start_media_transfer(
                                        reference,
                                        media,
                                        Intent::Save(path),
                                        cx,
                                    );
                                } else {
                                    this.media_notice(
                                        "Media changed or is no longer available".into(),
                                        cx,
                                    );
                                }
                            }
                            Ok(Ok(None)) => (),
                            _ => this.media_notice("Could not open the Save dialog".into(), cx),
                        }
                    });
                })
                .detach();
            }
            _ => (),
        }
    }
    fn media_notice(&mut self, message: String, cx: &mut Context<Self>) {
        if self.media_ui.notices.len() >= 3 {
            self.media_ui.notices.pop_front();
        }
        self.media_ui.notices.push_back((message, Instant::now()));
        cx.notify();
    }
    fn start_media_transfer(
        &mut self,
        reference: MediaReference,
        media: MediaContext,
        intent: Intent,
        cx: &mut Context<Self>,
    ) {
        if self.media_ui.jobs.len() >= 4 {
            self.media_notice("Finish or cancel another media download first".into(), cx);
            return;
        }
        let url = match intent {
            Intent::CopyImage => media.image_source.clone(),
            Intent::Save(_) => media.download_url.clone(),
        };
        let Some(url) = url else {
            return;
        };
        let stage = if let Some(file) = self.playback.cached_file(&url, cx) {
            Self::media_local_stage(file, &intent)
        } else {
            Download::start(url, None).map(Stage::Download)
        };
        match stage {
            Ok(stage) => {
                self.media_ui.next_id = self.media_ui.next_id.wrapping_add(1);
                self.media_ui.jobs.push(Transfer {
                    id: self.media_ui.next_id,
                    reference,
                    resource_key: media.resource_key,
                    name: media.filename,
                    intent,
                    stage,
                    percent: None,
                });
                cx.notify();
            }
            Err(error) => self.media_notice(error, cx),
        }
    }
    fn media_local_stage(
        file: Arc<tempfile::NamedTempFile>,
        intent: &Intent,
    ) -> Result<Stage, String> {
        match intent {
            Intent::CopyImage => media_export::prepare_clipboard_image(file.path().to_owned())
                .map(|job| Stage::Copy { _file: file, job }),
            Intent::Save(path) => media_export::export_file(file.path().to_owned(), path.clone())
                .map(|job| Stage::Save { _file: file, job }),
        }
    }
    pub(super) fn poll_media_transfers(&mut self, cx: &mut Context<Self>) {
        let count = self.media_ui.notices.len();
        self.media_ui
            .notices
            .retain(|(_, time)| time.elapsed() < Duration::from_secs(6));
        let mut changed = count != self.media_ui.notices.len();
        for mut transfer in std::mem::take(&mut self.media_ui.jobs) {
            let allowed = self
                .media_descriptor(&transfer.reference)
                .is_some_and(|media| {
                    media.available() && media.resource_key == transfer.resource_key
                });
            if !allowed {
                self.media_notice("Media is no longer available".into(), cx);
                continue;
            }
            match &transfer.stage {
                Stage::Download(download) => {
                    let percent = download.progress().map(|n| (n * 100.).round() as u8);
                    changed |= percent != transfer.percent;
                    transfer.percent = percent;
                    if let Some(result) = download.try_finish() {
                        match result
                            .and_then(|file| Self::media_local_stage(file, &transfer.intent))
                        {
                            Ok(stage) => {
                                transfer.stage = stage;
                                changed = true;
                            }
                            Err(error) => {
                                self.media_notice(error, cx);
                                continue;
                            }
                        }
                    }
                }
                Stage::Copy { job, _file } => match job.try_recv() {
                    Ok(Some(Ok(bytes))) => {
                        if self.media_ui.active {
                            let image = gpui::Image::from_bytes(gpui::ImageFormat::Png, bytes);
                            cx.write_to_clipboard(ClipboardItem::new_image(&image));
                            self.media_notice("Image copied".into(), cx);
                            continue;
                        }
                        transfer.stage = Stage::ReadyCopy {
                            _file: _file.clone(),
                            bytes,
                        };
                        changed = true;
                    }
                    Ok(Some(Err(error))) | Err(error) => {
                        self.media_notice(error, cx);
                        continue;
                    }
                    Ok(None) => (),
                },
                Stage::ReadyCopy { .. } => (),
                Stage::Save { job, .. } => match job.try_recv() {
                    Ok(Some(Ok(()))) => {
                        self.media_notice(format!("Saved {}", transfer.name), cx);
                        continue;
                    }
                    Ok(Some(Err(error))) | Err(error) => {
                        self.media_notice(error, cx);
                        continue;
                    }
                    Ok(None) => (),
                },
            }
            self.media_ui.jobs.push(transfer);
        }
        if changed {
            cx.notify();
        }
    }
    pub(super) fn media_transfer_overlay(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.media_ui.jobs.is_empty() && self.media_ui.notices.is_empty() {
            return div().into_any_element();
        }
        let jobs = self
            .media_ui
            .jobs
            .iter()
            .map(|job| {
                let id = job.id;
                let verb = match job.stage {
                    Stage::Download(_) => "Downloading",
                    Stage::Copy { .. } => "Copying",
                    Stage::ReadyCopy { .. } => "Ready to copy",
                    Stage::Save { .. } => "Saving",
                };
                let text = format!(
                    "{verb} {}{}",
                    job.name,
                    job.percent
                        .filter(|_| matches!(job.stage, Stage::Download(_)))
                        .map(|p| format!(" · {p}%"))
                        .unwrap_or_default()
                );
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(0x4c4458))
                    .bg(rgb(PANEL))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(emoji_sprites::unicode_text(&text, 12., false, cx)),
                    )
                    .when(matches!(job.stage, Stage::ReadyCopy { .. }), |view| {
                        view.child(
                            icon_button(("copy-ready-media", id), Icon::Copy, "Copy image")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    if !window.is_window_active() {
                                        return;
                                    }
                                    if let Some(index) =
                                        this.media_ui.jobs.iter().position(|job| job.id == id)
                                    {
                                        let transfer = this.media_ui.jobs.remove(index);
                                        if this.media_descriptor(&transfer.reference).is_some_and(
                                            |media| {
                                                media.can_copy_image()
                                                    && media.resource_key == transfer.resource_key
                                            },
                                        ) && let Stage::ReadyCopy { bytes, .. } = transfer.stage
                                        {
                                            let image = gpui::Image::from_bytes(
                                                gpui::ImageFormat::Png,
                                                bytes,
                                            );
                                            cx.write_to_clipboard(ClipboardItem::new_image(&image));
                                            this.media_notice("Image copied".into(), cx);
                                        }
                                        cx.notify();
                                    }
                                })),
                        )
                    })
                    .child(
                        icon_button(("cancel-media-transfer", id), Icon::Close, "Cancel").on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.media_ui.jobs.retain(|job| job.id != id);
                                cx.notify();
                            }),
                        ),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let notices = self
            .media_ui
            .notices
            .iter()
            .map(|(text, _)| {
                div()
                    .p_2()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(0x4c4458))
                    .bg(rgb(PANEL))
                    .child(emoji_sprites::unicode_text(text, 12., false, cx))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        deferred(
            div()
                .absolute()
                .bottom_3()
                .right_3()
                .w(px(320.))
                .max_w_full()
                .flex()
                .flex_col()
                .gap_1()
                .text_size(px(12.))
                .children(jobs)
                .children(notices),
        )
        .with_priority(9)
        .into_any_element()
    }
    pub(super) fn media_original_allowed(&self, reference: &MediaReference) -> bool {
        self.media_descriptor(reference)
            .is_some_and(|media| media.available() && media.original_url.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::{MediaReference, MediaTarget, live_media_message, media_menu_title};
    use crate::media_context_policy::MediaAsset;
    use fluxer_core::{ClientState, Message, Snowflake};
    use std::{collections::BTreeMap, sync::Arc};
    fn reference(surface: &'static str) -> MediaReference {
        MediaReference::new(
            Arc::new(Message {
                id: Snowflake(1),
                channel_id: Snowflake(11),
                ..Default::default()
            }),
            MediaTarget {
                snapshot_index: None,
                asset: MediaAsset::Attachment(Snowflake(2)),
            },
            surface,
        )
    }
    #[test]
    fn removed_timeline_message_never_falls_back_to_captured_snapshot() {
        let reference = reference("timeline");
        let mut state = ClientState::default();
        state.messages.insert(
            Snowflake(11),
            BTreeMap::from([(Snowflake(1), reference.message.as_ref().clone())]),
        );
        assert!(live_media_message(&state, None, &reference).is_some());
        state
            .messages
            .get_mut(&Snowflake(11))
            .unwrap()
            .remove(&Snowflake(1));
        assert!(live_media_message(&state, Some(&reference.message), &reference).is_none());
    }
    #[test]
    fn browser_requires_current_exact_result_and_uses_new_revision() {
        let state = ClientState::default();
        let reference = reference("browser");
        assert!(live_media_message(&state, None, &reference).is_none());
        let mut current = reference.message.as_ref().clone();
        current.content = "updated".into();
        assert_eq!(
            live_media_message(&state, Some(&current), &reference)
                .unwrap()
                .content,
            "updated"
        );
        current.channel_id = Snowflake(12);
        assert!(live_media_message(&state, Some(&current), &reference).is_none());
    }
    #[test]
    fn concealed_embed_menu_does_not_expose_original_basename_or_title() {
        let mut message = Message {
            id: Snowflake(1),
            channel_id: Snowflake(11),
            ..Default::default()
        };
        message.extra.insert("embeds".into(),serde_json::json!([{"type":"image","flags":8,"title":"Private title","image":{"url":"https://images.example/private-basename.png","proxy_url":"https://proxy.example/image"}}]));
        let target = MediaTarget {
            snapshot_index: None,
            asset: MediaAsset::Embed(0),
        };
        let mut revealed = std::collections::BTreeSet::new();
        let media = crate::media_context_policy::resolve(
            &message,
            &target,
            &Default::default(),
            &revealed,
            false,
            0,
        )
        .unwrap();
        assert_eq!(media_menu_title(&media, &revealed), "Spoiler");
        revealed.insert(media.reveal_key.clone());
        assert_eq!(media_menu_title(&media, &revealed), media.filename);
    }
}
