// SPDX-License-Identifier: AGPL-3.0-or-later
//! The grid measures its allocated stage, not the window. Child roots receive
//! exact rectangles after the surrounding chat, header and filmstrip lay out.
use crate::stream_layout::tiled_layout;
use gpui::{prelude::*, *};

pub(super) struct StreamGrid {
    tiles: Vec<(f32, AnyElement)>,
    gap: f32,
}
impl StreamGrid {
    pub fn new(tiles: Vec<(f32, AnyElement)>, gap: f32) -> Self {
        Self { tiles, gap }
    }
}
impl IntoElement for StreamGrid {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for StreamGrid {
    type RequestLayoutState = ();
    type PrepaintState = usize;
    fn id(&self) -> Option<ElementId> {
        Some("watched-stream-grid".into())
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
            size: Size {
                width: relative(1.).into(),
                height: relative(1.).into(),
            },
            min_size: Size {
                width: px(0.).into(),
                height: px(0.).into(),
            },
            flex_grow: 1.,
            flex_shrink: 1.,
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> usize {
        let ratios = self
            .tiles
            .iter()
            .map(|(ratio, _)| *ratio)
            .collect::<Vec<_>>();
        let rectangles = tiled_layout(
            f32::from(bounds.size.width),
            f32::from(bounds.size.height),
            &ratios,
            self.gap,
        );
        let painted = rectangles.len().min(self.tiles.len());
        for ((_, tile), rect) in self.tiles.iter_mut().zip(rectangles) {
            tile.layout_as_root(
                size(
                    AvailableSpace::Definite(px(rect.width)),
                    AvailableSpace::Definite(px(rect.height)),
                ),
                window,
                cx,
            );
            tile.prepaint_at(bounds.origin + point(px(rect.x), px(rect.y)), window, cx);
        }
        painted
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        painted: &mut usize,
        window: &mut Window,
        cx: &mut App,
    ) {
        for (_, tile) in self.tiles.iter_mut().take(*painted) {
            tile.paint(window, cx);
        }
    }
}
