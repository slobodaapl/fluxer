// SPDX-License-Identifier: AGPL-3.0-or-later
//! A narrow, always discoverable scrollbar backed by the timeline's real scroll handle.
use gpui::{prelude::*, *};
use std::{cell::Cell, rc::Rc};

pub(super) struct Scrollbar {
    id: ElementId,
    handle: ScrollHandle,
}

impl Scrollbar {
    pub fn new(id: impl Into<ElementId>, handle: ScrollHandle) -> Self {
        Self {
            id: id.into(),
            handle,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Thumb {
    top: f32,
    height: f32,
    travel: f32,
    max_scroll: f32,
}

fn thumb(track: f32, viewport: f32, maximum: f32, offset: f32) -> Option<Thumb> {
    if ![track, viewport, maximum, offset]
        .iter()
        .all(|v| v.is_finite())
        || track <= 0.
        || viewport <= 0.
        || maximum <= 0.5
    {
        return None;
    }
    let height = (track * viewport / (viewport + maximum))
        .max(28.)
        .min(track);
    let travel = track - height;
    Some(Thumb {
        top: (-offset / maximum).clamp(0., 1.) * travel,
        height,
        travel,
        max_scroll: maximum,
    })
}

fn drag_offset(start_offset: f32, pointer_delta: f32, geometry: Thumb) -> f32 {
    if geometry.travel <= 0. {
        return start_offset.clamp(-geometry.max_scroll, 0.);
    }
    (start_offset - pointer_delta / geometry.travel * geometry.max_scroll)
        .clamp(-geometry.max_scroll, 0.)
}

#[derive(Clone, Copy)]
struct Drag {
    pointer_y: f32,
    offset_y: f32,
    thumb: Thumb,
}

type DragState = Rc<Cell<Option<Drag>>>;

pub(super) struct ScrollbarPaint {
    hitbox: Hitbox,
    track: Bounds<Pixels>,
    thumb: Thumb,
    drag: DragState,
}

impl IntoElement for Scrollbar {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Scrollbar {
    type RequestLayoutState = ();
    type PrepaintState = Option<ScrollbarPaint>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let style = Style {
            size: size(px(12.).into(), relative(1.).into()),
            flex_shrink: 0.,
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
        let drag = window.with_element_state(
            id.expect("scrollbar id"),
            |previous: Option<DragState>, _| {
                let state = previous.unwrap_or_default();
                (state.clone(), state)
            },
        );
        let track = Bounds::new(
            point(bounds.left(), bounds.top() + px(4.)),
            size(bounds.size.width, (bounds.size.height - px(8.)).max(px(0.))),
        );
        let Some(thumb) = thumb(
            track.size.height.into(),
            self.handle.bounds().size.height.into(),
            self.handle.max_offset().height.into(),
            self.handle.offset().y.into(),
        ) else {
            drag.set(None);
            return None;
        };
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        Some(ScrollbarPaint {
            hitbox,
            track,
            thumb,
            drag,
        })
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        state: &mut Self::PrepaintState,
        window: &mut Window,
        _: &mut App,
    ) {
        let Some(state) = state else { return };
        let hovered = state.hitbox.is_hovered(window);
        let active = state.drag.get().is_some();
        let thumb_bounds = Bounds::new(
            point(
                state.track.left() + px(3.),
                state.track.top() + px(state.thumb.top),
            ),
            size(px(6.), px(state.thumb.height)),
        );
        window.paint_quad(
            fill(
                thumb_bounds,
                rgb(if hovered || active {
                    0x777080
                } else {
                    0x4b4556
                }),
            )
            .corner_radii(px(3.)),
        );
        window.set_cursor_style(CursorStyle::Arrow, &state.hitbox);

        let drag = state.drag.clone();
        let handle = self.handle.clone();
        let hitbox = state.hitbox.clone();
        let geometry = state.thumb;
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if !phase.bubble() || event.button != MouseButton::Left || !hitbox.is_hovered(window) {
                return;
            }
            if thumb_bounds.contains(&event.position) {
                drag.set(Some(Drag {
                    pointer_y: event.position.y.into(),
                    offset_y: handle.offset().y.into(),
                    thumb: geometry,
                }));
            } else {
                let direction = if event.position.y < thumb_bounds.top() {
                    1.
                } else {
                    -1.
                };
                let offset = handle.offset();
                let y = (offset.y + handle.bounds().size.height * (0.9 * direction))
                    .clamp(-handle.max_offset().height, px(0.));
                handle.set_offset(point(offset.x, y));
            }
            window.refresh();
            cx.stop_propagation();
        });

        let drag = state.drag.clone();
        let handle = self.handle.clone();
        let hitbox = state.hitbox.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Capture {
                return;
            }
            if let Some(start) = drag.get() {
                // Release capture even if the button was released outside the window.
                if !event
                    .pressed_button
                    .is_some_and(|button| button == MouseButton::Left)
                {
                    drag.set(None);
                } else {
                    let y = drag_offset(
                        start.offset_y,
                        f32::from(event.position.y) - start.pointer_y,
                        start.thumb,
                    );
                    handle.set_offset(point(
                        handle.offset().x,
                        px(y).max(-handle.max_offset().height),
                    ));
                    cx.stop_propagation();
                }
                window.refresh();
            } else if hitbox.is_hovered(window) != hovered {
                window.refresh();
            }
        });

        let drag = state.drag.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase == DispatchPhase::Capture
                && event.button == MouseButton::Left
                && drag.replace(None).is_some()
            {
                window.refresh();
                cx.stop_propagation();
            }
        });
        let handle = self.handle.clone();
        let hitbox = state.hitbox.clone();
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase.bubble() && hitbox.should_handle_scroll(window) {
                let offset = handle.offset();
                let y = (offset.y + event.delta.pixel_delta(px(20.)).y)
                    .clamp(-handle.max_offset().height, px(0.));
                handle.set_offset(point(offset.x, y));
                window.refresh();
                cx.stop_propagation();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn thumb_tracks_scroll_range_and_resizing() {
        let first = thumb(400., 400., 1200., 0.).unwrap();
        assert_eq!((first.top, first.height), (0., 100.));
        let last = thumb(400., 400., 1200., -1200.).unwrap();
        assert_eq!(last.top + last.height, 400.);
        let halfway = thumb(400., 400., 1200., -600.).unwrap();
        assert_eq!(halfway.top, 150.);
        assert!(thumb(800., 800., 800., -600.).unwrap().height > first.height);
    }

    #[test]
    fn dragging_preserves_grab_offset_and_clamps_to_both_ends() {
        let geometry = thumb(400., 400., 1200., -600.).unwrap();
        assert_eq!(drag_offset(-600., 0., geometry), -600.);
        assert_eq!(drag_offset(-600., 75., geometry), -900.);
        assert_eq!(drag_offset(-600., 1000., geometry), -1200.);
        assert_eq!(drag_offset(-600., -1000., geometry), 0.);
    }

    #[test]
    fn no_overflow_or_invalid_geometry_has_no_thumb() {
        assert!(thumb(400., 400., 0., 0.).is_none());
        assert!(thumb(0., 400., 10., 0.).is_none());
        assert!(thumb(400., 400., f32::NAN, 0.).is_none());
        let small = thumb(10., 10., 100000., 0.).unwrap();
        assert_eq!(small.height, 10.);
        assert_eq!(drag_offset(0., 20., small), 0.);
    }
}
