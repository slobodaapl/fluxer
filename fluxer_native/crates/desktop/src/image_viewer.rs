// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local image viewing: no new authenticated requests or browser navigation on
//! image activation. MediaViewerModal supplies the keyboard/gallery conventions.
use super::*;
use crate::image_transform::{Metrics, Point as PanPoint, Transform};
use crate::{
    controls::{Icon, icon_button, quiet_button},
    image_gallery::{ImageGallery, ImageItem},
};
use std::{cell::Cell, rc::Rc};

#[derive(Clone, Copy, PartialEq)]
enum ImageControl {
    Previous,
    Next,
    Original,
    Close,
    ZoomIn,
    ZoomOut,
    Fit,
}

pub(super) struct ImageViewer {
    gallery: Option<ImageGallery>,
    transform: Transform,
    zoom_in: FocusHandle,
    zoom_out: FocusHandle,
    fit: FocusHandle,
    metrics: Rc<Cell<Metrics>>,
    drag: Option<(Point<Pixels>, PanPoint)>,
    dragged: bool,
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
            transform: Transform::default(),
            zoom_in: cx.focus_handle().tab_stop(true),
            zoom_out: cx.focus_handle().tab_stop(true),
            fit: cx.focus_handle().tab_stop(true),
            metrics: Rc::new(Cell::new(Metrics {
                viewport: PanPoint::default(),
                content: PanPoint::default(),
            })),
            drag: None,
            dragged: false,
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
        self.transform = Transform::default();
        self.drag = None;
        self.dragged = false;
        self.previous_focus = None;
        self.armed = None;
    }
    fn controls(&self, original_allowed: bool) -> Vec<(ImageControl, FocusHandle)> {
        let mut controls = Vec::new();
        if self
            .gallery
            .as_ref()
            .is_some_and(|gallery| gallery.items.len() > 1)
        {
            controls.push((ImageControl::Previous, self.previous.clone()));
            controls.push((ImageControl::Next, self.next.clone()));
        }
        if original_allowed {
            controls.push((ImageControl::Original, self.original.clone()));
        }
        controls.extend([
            (ImageControl::ZoomOut, self.zoom_out.clone()),
            (ImageControl::Fit, self.fit.clone()),
            (ImageControl::ZoomIn, self.zoom_in.clone()),
            (ImageControl::Close, self.close.clone()),
        ]);
        controls
    }
    fn focused_control(&self, window: &Window, original_allowed: bool) -> Option<ImageControl> {
        self.controls(original_allowed)
            .into_iter()
            .find(|(_, focus)| focus.is_focused(window))
            .map(|(control, _)| control)
    }
}

impl Fluxer {
    fn image_original_available(&self) -> bool {
        self.image_viewer
            .gallery
            .as_ref()
            .and_then(ImageGallery::current)
            .is_some_and(|item| {
                item.context
                    .as_ref()
                    .is_none_or(|reference| self.media_original_allowed(reference))
            })
    }
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
        self.image_viewer.transform = Transform::default();
        self.image_viewer.drag = None;
        self.image_viewer.dragged = false;
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
            ImageControl::ZoomIn | ImageControl::ZoomOut | ImageControl::Fit => {
                let current = self.image_viewer.transform;
                let scale = match control {
                    ImageControl::ZoomIn => current.scale * 1.25,
                    ImageControl::ZoomOut => current.scale / 1.25,
                    _ => 1.,
                };
                self.image_viewer.transform =
                    current.zoom_to(scale, PanPoint::default(), self.image_viewer.metrics.get());
                cx.notify();
            }
            ImageControl::Close => self.dismiss_image_viewer(window, cx),
            ImageControl::Original => {
                if !self.image_original_available() {
                    return;
                }
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
                self.image_viewer.transform = Transform::default();
                self.image_viewer.drag = None;
                self.image_viewer.dragged = false;
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
        let metrics = Metrics {
            viewport: PanPoint {
                x: available_width,
                y: available_height,
            },
            content: PanPoint {
                x: width,
                y: height,
            },
        };
        self.image_viewer.metrics.set(metrics);
        let transform = self.image_viewer.transform.constrained(metrics);
        let bounds = Rc::new(Cell::new(Bounds::<Pixels>::default()));
        let painted = bounds.clone();
        let clicked = bounds.clone();
        let media = div()
            .id("image-pan-viewport")
            .relative()
            .w(px(available_width))
            .h(px(available_height))
            .max_w_full()
            .max_h_full()
            .flex_shrink_0()
            .overflow_hidden()
            .child(
                canvas(move |b, _, _| painted.set(b), |_, _, _, _| {})
                    .absolute()
                    .inset_0()
                    .size_full(),
            )
            .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _, cx| {
                let delta = event.delta.pixel_delta(px(24.));
                let current = this.image_viewer.transform;
                if event.modifiers.control {
                    let b = bounds.get();
                    let origin = PanPoint {
                        x: f32::from(event.position.x - b.center().x),
                        y: f32::from(event.position.y - b.center().y),
                    };
                    let next = current.scale * if delta.y > px(0.) { 1.25 } else { 1. / 1.25 };
                    this.image_viewer.transform = current.zoom_to(next, origin, metrics);
                } else if current.scale > 1.015 {
                    this.image_viewer.transform = current.panned(
                        PanPoint {
                            x: f32::from(delta.x),
                            y: f32::from(delta.y),
                        },
                        metrics,
                    );
                } else {
                    return;
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .child(
                div()
                    .id("image-viewer-media")
                    .absolute()
                    .left(px(
                        (available_width - width * transform.scale) / 2. + transform.pan.x
                    ))
                    .top(px(
                        (available_height - height * transform.scale) / 2. + transform.pan.y
                    ))
                    .w(px(width * transform.scale))
                    .h(px(height * transform.scale))
                    .flex_shrink_0()
                    .cursor(if transform.scale > 1.015 {
                        CursorStyle::OpenHand
                    } else {
                        CursorStyle::PointingHand
                    })
                    .when_some(item.context.clone(), |view, reference| {
                        crate::context_actions::trigger(
                            view,
                            crate::context_actions::Target::Media(reference),
                            cx,
                        )
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.image_viewer.drag =
                                Some((event.position, this.image_viewer.transform.pan));
                            this.image_viewer.dragged = false;
                            cx.stop_propagation();
                        }),
                    )
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        if !this.image_viewer.dragged {
                            let b = clicked.get();
                            let position = event.position();
                            let origin = PanPoint {
                                x: f32::from(position.x - b.center().x),
                                y: f32::from(position.y - b.center().y),
                            };
                            let current = this.image_viewer.transform;
                            let scale = if current.scale > 1.015 { 1. } else { 2.5 };
                            this.image_viewer.transform = current.zoom_to(scale, origin, metrics);
                        }
                        this.image_viewer.drag = None;
                        this.image_viewer.dragged = false;
                        cx.notify();
                    }))
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
                                    .items_center()
                                    .justify_center()
                                    .child("Image unavailable")
                                    .into_any_element()
                            })
                            .id(media_animation::image_id(
                                "image-viewer-frame",
                                &item.source,
                            )),
                    ),
            );
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
            .on_click(cx.listener(|this, _, window, cx| {
                if this.image_viewer.dragged {
                    this.image_viewer.dragged = false;
                    this.image_viewer.drag = None;
                    cx.stop_propagation();
                } else {
                    this.dismiss_image_viewer(window, cx);
                }
            }))
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                let Some((start, origin)) = this.image_viewer.drag else {
                    return;
                };
                if !event.dragging() {
                    this.image_viewer.drag = None;
                    return;
                }
                let delta = PanPoint {
                    x: f32::from(event.position.x - start.x),
                    y: f32::from(event.position.y - start.y),
                };
                this.image_viewer.dragged |= delta.x.hypot(delta.y) >= 20.;
                let current = this.image_viewer.transform;
                this.image_viewer.transform = Transform {
                    pan: origin,
                    ..current
                }
                .panned(delta, metrics);
                cx.stop_propagation();
                cx.notify();
            }))
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
                    "+" | "=" | "-" | "0" => {
                        this.activate_image_control(
                            match event.keystroke.key.as_str() {
                                "-" => ImageControl::ZoomOut,
                                "0" => ImageControl::Fit,
                                _ => ImageControl::ZoomIn,
                            },
                            window,
                            cx,
                        );
                    }
                    "tab" => {
                        this.image_viewer.armed = None;
                        let controls = this.image_viewer.controls(this.image_original_available());
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
                            this.image_viewer.armed = this
                                .image_viewer
                                .focused_control(window, this.image_original_available());
                        }
                    }
                    _ => return,
                }
                cx.stop_propagation();
            }))
            .capture_key_up(cx.listener(|this, event: &KeyUpEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    if let Some(control) = this.image_viewer.armed.take()
                        && this
                            .image_viewer
                            .focused_control(window, this.image_original_available())
                            == Some(control)
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
                    .child(media)
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
                    .when(self.image_original_available(), |footer| {
                        footer.child(
                            quiet_button("open-original-image", "Open original")
                                .track_focus(&self.image_viewer.original)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.activate_image_control(ImageControl::Original, window, cx);
                                })),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                quiet_button("zoom-image-out", "−")
                                    .size(px(28.))
                                    .p_0()
                                    .track_focus(&self.image_viewer.zoom_out)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.activate_image_control(
                                            ImageControl::ZoomOut,
                                            window,
                                            cx,
                                        );
                                    })),
                            )
                            .child(
                                quiet_button(
                                    "fit-image",
                                    format!("{}%", (transform.scale * 100.).round() as u32),
                                )
                                .text_size(px(12.))
                                .track_focus(&self.image_viewer.fit)
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.activate_image_control(ImageControl::Fit, window, cx);
                                    },
                                )),
                            )
                            .child(
                                quiet_button("zoom-image-in", "+")
                                    .size(px(28.))
                                    .p_0()
                                    .track_focus(&self.image_viewer.zoom_in)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.activate_image_control(
                                            ImageControl::ZoomIn,
                                            window,
                                            cx,
                                        );
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(MUTED))
                            .child(if multiple {
                                "← → Browse · Click to zoom · Drag to pan · Esc close"
                            } else {
                                "Click to zoom · Drag to pan · 0 Fit · Esc close"
                            }),
                    ),
            );
        deferred(overlay).with_priority(5).into_any_element()
    }
}
