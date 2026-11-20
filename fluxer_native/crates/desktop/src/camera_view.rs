// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

impl Fluxer {
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
        div()
            .id("remote-cameras")
            .flex_shrink_0()
            .flex()
            .items_start()
            .gap_2()
            .overflow_x_scroll()
            .px_3()
            .py_2()
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
