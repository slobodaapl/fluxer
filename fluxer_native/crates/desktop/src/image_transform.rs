// SPDX-License-Identifier: AGPL-3.0-or-later
//! Port of media_modal/pan_zoom/PanZoomMath.ts, in fit-relative coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub viewport: Point,
    pub content: Point,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub scale: f32,
    pub pan: Point,
}
impl Default for Transform {
    fn default() -> Self {
        Self {
            scale: 1.,
            pan: Point::default(),
        }
    }
}
impl Transform {
    pub fn constrained(self, m: Metrics) -> Self {
        let scale = if self.scale.is_finite() {
            self.scale.clamp(1., 5.)
        } else {
            1.
        };
        if scale <= 1.015 {
            return Self::default();
        }
        let x = ((m.content.x * scale - m.viewport.x) / 2.).max(0.);
        let y = ((m.content.y * scale - m.viewport.y) / 2.).max(0.);
        Self {
            scale,
            pan: Point {
                x: if self.pan.x.is_finite() {
                    self.pan.x.clamp(-x, x)
                } else {
                    0.
                },
                y: if self.pan.y.is_finite() {
                    self.pan.y.clamp(-y, y)
                } else {
                    0.
                },
            },
        }
    }
    pub fn zoom_to(self, scale: f32, origin: Point, m: Metrics) -> Self {
        let next = if scale.is_finite() {
            scale.clamp(1., 5.)
        } else {
            1.
        };
        let ratio = next / self.scale.max(1.);
        Self {
            scale: next,
            pan: Point {
                x: origin.x - (origin.x - self.pan.x) * ratio,
                y: origin.y - (origin.y - self.pan.y) * ratio,
            },
        }
        .constrained(m)
    }
    pub fn panned(self, delta: Point, m: Metrics) -> Self {
        Self {
            pan: Point {
                x: self.pan.x + delta.x,
                y: self.pan.y + delta.y,
            },
            ..self
        }
        .constrained(m)
    }
}
#[cfg(test)]
mod tests {
    use super::{Metrics, Point, Transform};
    fn metrics() -> Metrics {
        Metrics {
            viewport: Point { x: 800., y: 600. },
            content: Point { x: 800., y: 400. },
        }
    }
    #[test]
    fn anchored_pixel_retains_position_then_pan_clamps_and_fit_clears() {
        let fit = Transform::default();
        let origin = Point { x: 100., y: 40. };
        let z = fit.zoom_to(2.5, origin, metrics());
        assert_eq!(z.pan, Point { x: -150., y: -60. });
        assert_eq!(100. * z.scale + z.pan.x, 100.);
        let edge = z.panned(
            Point {
                x: 10000.,
                y: -10000.,
            },
            metrics(),
        );
        assert_eq!(edge.pan, Point { x: 600., y: -200. });
        assert_eq!(edge.zoom_to(1., origin, metrics()), fit);
    }
    #[test]
    fn small_content_cannot_pan_outside_view_and_resize_reclamps() {
        let z = Transform {
            scale: 5.,
            pan: Point { x: 999., y: 999. },
        };
        let m = Metrics {
            viewport: Point { x: 1000., y: 800. },
            content: Point { x: 100., y: 100. },
        };
        assert_eq!(z.constrained(m).pan, Point::default());
        assert_eq!(z.zoom_to(99., Point::default(), metrics()).scale, 5.);
        assert_eq!(
            z.zoom_to(f32::NAN, Point::default(), metrics()),
            Transform::default()
        );
    }
}
