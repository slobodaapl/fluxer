// SPDX-License-Identifier: AGPL-3.0-or-later
//! GPUI advances an animated Img and requests its next frame during layout,
//! even when that layout will be clipped later. Defer the actual image's
//! layout until prepaint, when the scroll mask and allocated bounds are known.
use gpui::{prelude::*, *};

pub(super) struct VisibleImage {
    id: ElementId,
    width: f32,
    height: f32,
    build: Box<dyn Fn() -> Img>,
    enabled: bool,
    autoplay: bool,
}

impl VisibleImage {
    /// Build an unidentified Img with the desired fit/loading/fallback styles.
    /// Its factory runs only when the allocated box actually intersects the
    /// viewport and ancestor clip. Include a source revision in id if a given
    /// location can change to a different image, so GPUI resets its frame state.
    pub fn new(
        id: impl Into<ElementId>,
        width: f32,
        height: f32,
        build: impl Fn() -> Img + 'static,
    ) -> Self {
        Self {
            id: id.into(),
            width: dimension(width),
            height: dimension(height),
            build: Box::new(build),
            enabled: true,
            autoplay: true,
        }
    }

    /// Pause animation while keeping a first-frame image in a visible box.
    /// The caller owns policy such as modal occlusion/preferences; this element
    /// adds no window-focus or network policy of its own.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// GIF autoplay preferences fall back to interaction in useShouldAnimate.
    /// The parent can notify on hover to start the first animated frame.
    pub fn autoplay(mut self, autoplay: bool) -> Self {
        self.autoplay = autoplay;
        self
    }
}

fn dimension(value: f32) -> f32 {
    if value.is_finite() { value.max(0.) } else { 0. }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageMode {
    Clipped,
    Static,
    Animated,
}

fn image_mode(
    bounds: Bounds<Pixels>,
    mask: Bounds<Pixels>,
    viewport: Size<Pixels>,
    enabled: bool,
) -> ImageMode {
    let viewport = Bounds::new(point(px(0.), px(0.)), viewport);
    // Invalid or empty geometry cannot justify fetching or running a clock.
    for rect in [bounds, mask, viewport] {
        if ![
            rect.origin.x,
            rect.origin.y,
            rect.size.width,
            rect.size.height,
        ]
        .iter()
        .all(|value| f32::from(*value).is_finite())
            || rect.size.width <= px(0.)
            || rect.size.height <= px(0.)
        {
            return ImageMode::Clipped;
        }
    }
    let visible = bounds.intersect(&mask).intersect(&viewport);
    if visible.size.width <= px(0.) || visible.size.height <= px(0.) {
        ImageMode::Clipped
    } else if enabled {
        ImageMode::Animated
    } else {
        ImageMode::Static
    }
}

fn playback_mode(
    bounds: Bounds<Pixels>,
    mask: Bounds<Pixels>,
    viewport: Size<Pixels>,
    enabled: bool,
    autoplay: bool,
    pointer: Point<Pixels>,
) -> ImageMode {
    let visible = bounds
        .intersect(&mask)
        .intersect(&Bounds::new(point(px(0.), px(0.)), viewport));
    image_mode(
        bounds,
        mask,
        viewport,
        enabled && (autoplay || visible.contains(&pointer)),
    )
}

impl IntoElement for VisibleImage {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for VisibleImage {
    type RequestLayoutState = ();
    type PrepaintState = Option<AnyElement>;

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
            size: size(px(self.width).into(), px(self.height).into()),
            max_size: Size {
                width: relative(1.).into(),
                height: Length::Auto,
            },
            min_size: size(px(0.).into(), px(0.).into()),
            flex_shrink: 1.,
            ..Default::default()
        };
        // No child image, asset load, loading timer, or animation clock here.
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let mode = playback_mode(
            bounds,
            window.content_mask().bounds,
            window.viewport_size(),
            self.enabled,
            self.autoplay,
            window.mouse_position(),
        );
        if trace_enabled() {
            if let Some(global_id) = global_id {
                window.with_element_state(global_id, |previous: Option<ImageMode>, _| {
                    trace(mode, previous != Some(mode));
                    ((), mode)
                });
            } else {
                trace(mode, true);
            }
        }
        if mode == ImageMode::Clipped {
            return None;
        }
        let image = (self.build)()
            .w(bounds.size.width)
            .h(bounds.size.height)
            .max_w_full();
        // The wrapper's stable global ID namespaces this child state. Without
        // its own ID GPUI renders frame zero and schedules no animation frame.
        let mut child = if mode == ImageMode::Animated {
            image.id("frames").into_any_element()
        } else {
            image.into_any_element()
        };
        child.layout_as_root(
            size(
                AvailableSpace::Definite(bounds.size.width),
                AvailableSpace::Definite(bounds.size.height),
            ),
            window,
            cx,
        );
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            child.prepaint_at(bounds.origin, window, cx);
        });
        Some(child)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        child: &mut Option<AnyElement>,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(child) = child {
            // The outer message/image container may be wider than the bitmap.
            // Observe the actual clipped image edge so hover playback starts
            // and stops even when the pointer stays inside that parent.
            if self.enabled && !self.autoplay {
                let visible = bounds
                    .intersect(&window.content_mask().bounds)
                    .intersect(&Bounds::new(point(px(0.), px(0.)), window.viewport_size()));
                let hovered = visible.contains(&window.mouse_position());
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, _| {
                    if phase.bubble() && visible.contains(&event.position) != hovered {
                        window.refresh();
                    }
                });
            }
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                child.paint(window, cx)
            });
        }
    }
}

/// Opt-in metadata only: counts element visits, not decoded GIF frames. No
/// media URL, message/user identity or content enters performance output.
fn trace_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("FLUXER_GIF_PERF").is_some())
}

fn trace(mode: ImageMode, changed: bool) {
    use std::{
        sync::{Mutex, OnceLock},
        time::{Duration, Instant},
    };
    static COUNTS: OnceLock<Mutex<(Instant, [u64; 3])>> = OnceLock::new();
    let Ok(mut state) = COUNTS
        .get_or_init(|| Mutex::new((Instant::now(), [0; 3])))
        .lock()
    else {
        return;
    };
    let index = match mode {
        ImageMode::Clipped => 0,
        ImageMode::Static => 1,
        ImageMode::Animated => 2,
    };
    state.1[index] += 1;
    if changed {
        eprintln!(
            "GIF_IMAGE_GATE mode={mode:?} clipped={} static={} animated={}",
            state.1[0], state.1[1], state.1[2]
        );
    }
    if state.0.elapsed() >= Duration::from_secs(1) {
        eprintln!(
            "GIF_IMAGE_GATE elapsed_ms={} clipped={} static={} animated={}",
            state.0.elapsed().as_millis(),
            state.1[0],
            state.1[1],
            state.1[2]
        );
        *state = (Instant::now(), [0; 3]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;
    fn rect(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }
    fn viewport() -> Size<Pixels> {
        size(px(1000.), px(800.))
    }

    #[test]
    fn partial_scroll_clip_is_visible_but_full_clip_and_touching_edges_are_not() {
        let mask = rect(200., 100., 400., 300.);
        assert_eq!(
            image_mode(rect(250., 50., 200., 100.), mask, viewport(), true),
            ImageMode::Animated
        );
        assert_eq!(
            image_mode(rect(250., 0., 200., 100.), mask, viewport(), true),
            ImageMode::Clipped
        );
        assert_eq!(
            image_mode(rect(600., 150., 200., 100.), mask, viewport(), true),
            ImageMode::Clipped
        );
        assert_eq!(
            image_mode(rect(250., 400., 200., 100.), mask, viewport(), true),
            ImageMode::Clipped
        );
        assert_eq!(
            image_mode(rect(-50., 150., 100., 100.), mask, viewport(), true),
            ImageMode::Clipped
        );
    }

    #[test]
    fn viewport_limits_an_oversized_scroll_mask_and_zero_size_never_loads() {
        let mask = rect(-500., -500., 3000., 3000.);
        assert_eq!(
            image_mode(rect(999., 799., 100., 100.), mask, viewport(), true),
            ImageMode::Animated
        );
        assert_eq!(
            image_mode(rect(1000., 0., 100., 100.), mask, viewport(), true),
            ImageMode::Clipped
        );
        assert_eq!(
            image_mode(rect(0., 800., 100., 100.), mask, viewport(), true),
            ImageMode::Clipped
        );
        assert_eq!(
            image_mode(rect(0., 0., 0., 100.), mask, viewport(), true),
            ImageMode::Clipped
        );
        assert_eq!(
            image_mode(
                rect(0., 0., 100., 100.),
                rect(0., 0., 0., 100.),
                viewport(),
                true
            ),
            ImageMode::Clipped
        );
    }

    #[test]
    fn disabling_animation_keeps_a_visible_static_image_and_does_not_enable_clipped_images() {
        let mask = rect(0., 0., 1000., 800.);
        assert_eq!(
            image_mode(rect(10., 10., 200., 100.), mask, viewport(), false),
            ImageMode::Static
        );
        assert_eq!(
            image_mode(rect(10., 900., 200., 100.), mask, viewport(), false),
            ImageMode::Clipped
        );
    }

    #[test]
    fn autoplay_disabled_animates_only_hovered_visible_pixels_and_hard_pause_wins() {
        let bounds = rect(100., 50., 300., 200.);
        let mask = rect(200., 100., 400., 300.);
        let inside = point(px(250.), px(150.));
        let clipped_part = point(px(150.), px(75.));
        assert_eq!(
            playback_mode(bounds, mask, viewport(), true, false, inside),
            ImageMode::Animated
        );
        assert_eq!(
            playback_mode(bounds, mask, viewport(), true, false, clipped_part),
            ImageMode::Static
        );
        assert_eq!(
            playback_mode(bounds, mask, viewport(), false, true, inside),
            ImageMode::Static
        );
        assert_eq!(
            playback_mode(bounds, mask, viewport(), false, false, inside),
            ImageMode::Static
        );
        assert_eq!(
            playback_mode(
                rect(0., 900., 300., 200.),
                mask,
                viewport(),
                true,
                false,
                inside
            ),
            ImageMode::Clipped
        );
    }

    #[test]
    fn invalid_geometry_cannot_start_image_loading_or_an_animation_clock() {
        let mask = rect(0., 0., 1000., 800.);
        for bounds in [
            rect(f32::NAN, 0., 100., 100.),
            rect(0., 0., f32::INFINITY, 100.),
            rect(0., 0., 100., -1.),
        ] {
            assert_eq!(
                image_mode(bounds, mask, viewport(), true),
                ImageMode::Clipped
            );
        }
        assert_eq!(dimension(f32::NAN), 0.);
        assert_eq!(dimension(f32::INFINITY), 0.);
        assert_eq!(dimension(-1.), 0.);
        assert_eq!(dimension(123.), 123.);
    }
}
