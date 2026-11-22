// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const LOCAL_SOURCES: [VideoSource; 2] = [VideoSource::Camera, VideoSource::ScreenShare];

impl Fluxer {
    fn local_preview_active(&self, source: VideoSource) -> bool {
        self.state.voice.channel_id.is_some_and(|channel| {
            (self.pane == Pane::Call || self.state.selected_channel == Some(channel))
                && match source {
                    VideoSource::Camera => self.state.voice.camera,
                    VideoSource::ScreenShare => self.state.voice.screen_share,
                }
        })
    }

    pub(super) fn local_preview_visible(&self, source: VideoSource) -> bool {
        self.controller
            .as_ref()
            .is_some_and(|controller| controller.local_previews().visible(source))
    }

    pub(super) fn set_local_preview_visible(
        &mut self,
        source: VideoSource,
        visible: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(controller) = &self.controller {
            controller.local_previews().set_visible(source, visible);
        }
        if !visible {
            self.retire_local_preview(source, cx);
        }
        cx.notify();
    }

    pub(super) fn retire_local_preview(&mut self, source: VideoSource, cx: &mut Context<Self>) {
        if let Some(image) = self.local_preview_images.remove(&source) {
            self.retire_image(image, cx);
        }
    }

    pub(super) fn clear_local_previews(&mut self, cx: &mut Context<Self>) {
        for (_, image) in std::mem::take(&mut self.local_preview_images) {
            self.retire_image(image, cx);
        }
    }

    pub(super) fn poll_local_previews(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        for source in LOCAL_SOURCES {
            if !self.local_preview_active(source) {
                continue;
            }
            let Some(frame) = self
                .controller
                .as_ref()
                .and_then(|controller| controller.local_previews().take_frame(source))
            else {
                continue;
            };
            let Some(buffer) = image::RgbaImage::from_raw(frame.width, frame.height, frame.bgra)
            else {
                continue;
            };
            let image = RenderImage::new(smallvec::smallvec![image::Frame::new(buffer)]);
            if let Some(previous) = self.local_preview_images.insert(source, Arc::new(image)) {
                self.retire_image(previous, cx);
            }
            changed = true;
        }
        changed
    }

    pub(super) fn camera_strip_visible(&self) -> bool {
        !self.available_cameras.is_empty()
            || LOCAL_SOURCES.iter().any(|&source| {
                self.local_preview_active(source) && self.local_preview_visible(source)
            })
    }

    fn local_preview_tile(&self, source: VideoSource, cx: &mut Context<Self>) -> AnyElement {
        let (id, label) = match source {
            VideoSource::Camera => ("local-camera-preview", "Your camera"),
            VideoSource::ScreenShare => ("local-screen-preview", "Your screen share"),
        };
        div()
            .id(id)
            .when_some(self.state.voice.channel_id, |tile, channel| {
                crate::context_actions::trigger(
                    tile,
                    crate::context_actions::Target::SelfVoice(channel),
                    cx,
                )
            })
            .w(px(194.))
            .flex_shrink_0()
            .rounded_md()
            .overflow_hidden()
            .bg(rgb(PANEL))
            .child(
                div()
                    .h(px(94.))
                    .bg(rgb(0x09080b))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        if let Some(image) = self.local_preview_images.get(&source) {
                            img(image.clone())
                                .size_full()
                                .object_fit(ObjectFit::Contain)
                                .into_any_element()
                        } else {
                            muted("Waiting for preview…").into_any_element()
                        },
                    ),
            )
            .child(div().px_2().py_1().text_size(px(11.)).child(label))
            .into_any_element()
    }

    fn watch_camera(&mut self, identity: String, watch: bool, cx: &mut Context<Self>) {
        if watch == self.camera_viewer.desired.contains(&identity) {
            return;
        }
        if watch {
            if !self.available_cameras.contains(&identity)
                || !self.pending_camera_starts.insert(identity.clone())
            {
                return;
            }
            self.camera_viewer.watch(identity.clone());
        } else {
            self.hide_camera(&identity, cx);
        }
        self.command(Command::WatchCamera { identity, watch }, cx);
    }

    pub(super) fn hide_camera(&mut self, identity: &str, cx: &mut Context<Self>) {
        if let Some(frame) = self.camera_viewer.frames.get(identity) {
            self.retire_image(frame.image.clone(), cx);
        }
        self.camera_viewer.stop(identity);
    }

    pub(super) fn camera_strip(&self, cx: &mut Context<Self>) -> AnyElement {
        let previews = LOCAL_SOURCES
            .into_iter()
            .filter(|&source| {
                self.local_preview_active(source) && self.local_preview_visible(source)
            })
            .map(|source| self.local_preview_tile(source, cx))
            .collect::<Vec<_>>();
        div()
            .id("call-videos")
            .flex_shrink_0()
            .flex()
            .items_start()
            .gap_2()
            .overflow_x_scroll()
            .px_3()
            .py_2()
            .children(previews)
            .children(self.available_cameras.iter().map(|identity| {
                let name = self
                    .video_participant_name(identity)
                    .unwrap_or_else(|| "Participant".into());
                let watching = self.camera_viewer.desired.contains(identity);
                let frame = self.camera_viewer.frames.get(identity);
                let pending = !watching && self.pending_camera_starts.contains(identity);
                let error = self.camera_viewer.errors.get(identity).cloned();
                let identity = identity.clone();
                div()
                    .id(SharedString::from(format!("remote-camera-{identity}")))
                    .w(px(194.))
                    .flex_shrink_0()
                    .rounded_md()
                    .overflow_hidden()
                    .bg(rgb(PANEL))
                    .when(watching, |tile| {
                        tile.child(
                            div()
                                .h(px(94.))
                                .bg(rgb(0x09080b))
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(if let Some(frame) = frame {
                                    img(frame.image.clone())
                                        .size_full()
                                        .object_fit(ObjectFit::Contain)
                                        .into_any_element()
                                } else {
                                    muted("Waiting for camera…").into_any_element()
                                }),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .pl_2()
                            .pr_1()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(11.))
                                    .text_ellipsis()
                                    .child(name),
                            )
                            .child(
                                controls::quiet_button(
                                    "toggle-remote-camera",
                                    if watching {
                                        "Hide camera"
                                    } else {
                                        "Show camera"
                                    },
                                )
                                .when(pending, |button| button.opacity(0.5))
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.watch_camera(identity.clone(), !watching, cx);
                                    },
                                )),
                            ),
                    )
                    .when_some(error, |tile, error| {
                        tile.child(div().px_2().pb_2().text_size(px(11.)).child(error))
                    })
            }))
            .into_any_element()
    }
}
