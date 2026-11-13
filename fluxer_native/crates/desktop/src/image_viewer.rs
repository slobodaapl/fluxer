// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local image viewing: no new authenticated requests or browser navigation on
//! image activation. MediaViewerModal supplies the keyboard/gallery conventions.
use super::*;
use crate::{
    controls::{Icon, icon_button, quiet_button},
    image_gallery::{ImageGallery, ImageItem},
};

#[derive(Clone, Copy, PartialEq)]
enum ImageControl {
    Previous,
    Next,
    Original,
    Close,
}

pub(super) struct ImageViewer {
    gallery: Option<ImageGallery>,
    focus: FocusHandle,
    previous: FocusHandle,
    next: FocusHandle,
    original: FocusHandle,
    close: FocusHandle,
    previous_focus: Option<FocusHandle>,
    armed: Option<ImageControl>,
}
impl ImageViewer {
    pub fn new(cx: &mut Context<Fluxer>) -> Self {
        Self {
            gallery: None,
            focus: cx.focus_handle(),
            previous: cx.focus_handle().tab_stop(true),
            next: cx.focus_handle().tab_stop(true),
            original: cx.focus_handle().tab_stop(true),
            close: cx.focus_handle().tab_stop(true),
            previous_focus: None,
            armed: None,
        }
    }
    pub fn is_open(&self) -> bool {
        self.gallery.is_some()
    }
    pub fn reset(&mut self) {
        self.gallery = None;
        self.previous_focus = None;
        self.armed = None;
    }
    fn controls(&self) -> Vec<(ImageControl, FocusHandle)> {
        let mut controls = Vec::new();
        if self
            .gallery
            .as_ref()
            .is_some_and(|gallery| gallery.items.len() > 1)
        {
            controls.push((ImageControl::Previous, self.previous.clone()));
            controls.push((ImageControl::Next, self.next.clone()));
        }
        controls.push((ImageControl::Original, self.original.clone()));
        controls.push((ImageControl::Close, self.close.clone()));
        controls
    }
    fn focused_control(&self, window: &Window) -> Option<ImageControl> {
        self.controls()
            .into_iter()
            .find(|(_, focus)| focus.is_focused(window))
            .map(|(control, _)| control)
    }
}

impl Fluxer {
    pub(super) fn open_image_gallery(
        &mut self,
        items: Vec<ImageItem>,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(gallery) = ImageGallery::new(items, index) else {
            return;
        };
        self.dismiss_message_actions(window, cx);
        if !self.image_viewer.is_open() {
            self.image_viewer.previous_focus = window.focused(cx);
        }
        self.image_viewer.gallery = Some(gallery);
        self.image_viewer.armed = None;
        self.image_viewer.close.focus(window);
        cx.notify();
    }

    pub(super) fn dismiss_image_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self.image_viewer.previous_focus.take();
        self.image_viewer.reset();
        if let Some(previous) = previous {
            previous.focus(window);
        }
        cx.notify();
    }

    fn activate_image_control(
        &mut self,
        control: ImageControl,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.image_viewer.armed = None;
        match control {
            ImageControl::Close => self.dismiss_image_viewer(window, cx),
            ImageControl::Original => {
                if let Some(item) = self
                    .image_viewer
                    .gallery
                    .as_ref()
                    .and_then(ImageGallery::current)
                {
                    let url = item.original.clone().unwrap_or_else(|| item.source.clone());
                    self.activate_message_target(
                        &message_markup::MessageTarget::Link(url),
                        window,
                        cx,
                    );
                }
            }
            ImageControl::Previous | ImageControl::Next => {
                if let Some(gallery) = self.image_viewer.gallery.as_mut() {
                    if control == ImageControl::Next {
                        gallery.next();
                    } else {
                        gallery.previous();
                    }
                }
                cx.notify();
            }
        }
    }

    pub(super) fn image_viewer_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(gallery) = &self.image_viewer.gallery else {
            return div().into_any_element();
        };
        let Some(item) = gallery.current() else {
            return div().into_any_element();
        };
        let multiple = gallery.items.len() > 1;
        // The overlay fills this viewport; reserve its fixed controls and outer
        // padding. Keep native-sized small images and contain oversized images.
        let available_width =
            (f32::from(window.viewport_size().width) - if multiple { 112. } else { 48. }).max(1.);
        let available_height = (f32::from(window.viewport_size().height) - 144.).max(1.);
        let (width, height) = match (
            item.width.filter(|v| *v > 0),
            item.height.filter(|v| *v > 0),
        ) {
            (Some(width), Some(height)) => {
                let scale = (available_width / width as f32)
                    .min(available_height / height as f32)
                    .min(1.);
                (width as f32 * scale, height as f32 * scale)
            }
            _ => (available_width, available_height),
        };
        let overlay = div()
            .id("image-viewer-overlay")
            .absolute()
            .inset_0()
            .occlude()
            .track_focus(&self.image_viewer.focus)
            .bg(rgba(0x08070aee))
            .px_4()
            .py_3()
            .flex()
            .flex_col()
            .gap_2()
            .on_click(cx.listener(|this, _, window, cx| this.dismiss_image_viewer(window, cx)))
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => this.dismiss_image_viewer(window, cx),
                    "left" | "right" if !event.keystroke.modifiers.modified() => {
                        this.activate_image_control(
                            if event.keystroke.key == "left" {
                                ImageControl::Previous
                            } else {
                                ImageControl::Next
                            },
                            window,
                            cx,
                        );
                    }
                    "tab" => {
                        this.image_viewer.armed = None;
                        let controls = this.image_viewer.controls();
                        let current = controls
                            .iter()
                            .position(|(_, focus)| focus.is_focused(window));
                        let next = match (current, event.keystroke.modifiers.shift) {
                            (Some(index), true) => (index + controls.len() - 1) % controls.len(),
                            (Some(index), false) => (index + 1) % controls.len(),
                            (None, true) => controls.len() - 1,
                            (None, false) => 0,
                        };
                        controls[next].1.focus(window);
                    }
                    "enter" | "space" if !event.keystroke.modifiers.modified() => {
                        if !event.is_held {
                            this.image_viewer.armed = this.image_viewer.focused_control(window);
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .capture_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    if let Some(control) = this.image_viewer.armed.take()
                        && this.image_viewer.focused_control(window) == Some(control)
                    {
                        this.activate_image_control(control, window, cx);
                    }
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .id("image-viewer-header")
                    .h(px(40.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap_3()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_ellipsis()
                            .font_weight(FontWeight::MEDIUM)
                            .child(item.name.clone()),
                    )
                    .when(multiple, |header| {
                        header.child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(MUTED))
                                .child(format!("{} / {}", gallery.index + 1, gallery.items.len())),
                        )
                    })
                    .child(
                        icon_button("close-image-viewer", Icon::Close, "Close image (Esc)")
                            .track_focus(&self.image_viewer.close)
                            .on_click(cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.dismiss_image_viewer(window, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .when(multiple, |body| {
                        body.child(
                            quiet_button("previous-image", "‹")
                                .w(px(28.))
                                .h(px(40.))
                                .p_0()
                                .text_size(px(28.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .track_focus(&self.image_viewer.previous)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.activate_image_control(ImageControl::Previous, window, cx);
                                })),
                        )
                    })
                    .child(
                        div()
                            .id("image-viewer-media")
                            .w(px(width))
                            .h(px(height))
                            .min_w_0()
                            .flex_shrink_0()
                            .on_click(|_, _, cx| cx.stop_propagation())
                            .child(
                                img(item.source.clone())
                                    .size_full()
                                    .object_fit(ObjectFit::Contain)
                                    .with_loading(|| {
                                        div()
                                            .size_full()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .text_color(rgb(MUTED))
                                            .child("Loading image…")
                                            .into_any_element()
                                    })
                                    .with_fallback(|| {
                                        div()
                                            .size_full()
                                            .flex()
                                            .flex_col()
                                            .items_center()
                                            .justify_center()
                                            .gap_2()
                                            .child("Image unavailable")
                                            .child(
                                                div()
                                                    .text_size(px(12.))
                                                    .text_color(rgb(MUTED))
                                                    .child("You can still try Open original."),
                                            )
                                            .into_any_element()
                                    }),
                            ),
                    )
                    .when(multiple, |body| {
                        body.child(
                            quiet_button("next-image", "›")
                                .w(px(28.))
                                .h(px(40.))
                                .p_0()
                                .text_size(px(28.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .track_focus(&self.image_viewer.next)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.activate_image_control(ImageControl::Next, window, cx);
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .id("image-viewer-footer")
                    .h(px(36.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        quiet_button("open-original-image", "Open original")
                            .track_focus(&self.image_viewer.original)
                            .on_click(cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.activate_image_control(ImageControl::Original, window, cx);
                            })),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(MUTED))
                            .child(if multiple {
                                "← → Browse images · Esc to close"
                            } else {
                                "Esc to close"
                            }),
                    ),
            );
        deferred(overlay).with_priority(5).into_any_element()
    }
}
