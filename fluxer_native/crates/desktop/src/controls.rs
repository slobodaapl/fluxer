// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared compact controls. SVG artwork matches the existing client's Phosphor set.
use super::*;
use std::{borrow::Cow, cell::Cell, rc::Rc};
#[derive(Clone, Copy)]
pub(super) enum Icon {
    React,
    Reply,
    More,
    Forward,
    Copy,
    Edit,
    Delete,
    Pin,
    Close,
    Jump,
    Search,
    Attachment,
    File,
    Eye,
    EyeOff,
    Retry,
    Send,
    Mic,
    MicOff,
    Headphones,
    Deafen,
    Settings,
    ScreenShare,
    PhoneOff,
    Volume,
    VolumeOff,
    Chat,
    Expand,
    Minimize,
    Fullscreen,
    Back,
    Refresh,
    Stop,
    Home,
}
impl Icon {
    fn path(self) -> &'static str {
        match self {
            Self::React => "icons/React.svg",
            Self::Reply => "icons/Reply.svg",
            Self::More => "icons/More.svg",
            Self::Forward => "icons/Forward.svg",
            Self::Copy => "icons/Copy.svg",
            Self::Edit => "icons/Edit.svg",
            Self::Delete => "icons/Delete.svg",
            Self::Pin => "icons/Pin.svg",
            Self::Close => "icons/Close.svg",
            Self::Jump => "icons/Jump.svg",
            Self::Search => "icons/Search.svg",
            Self::Attachment => "icons/Attachment.svg",
            Self::File => "icons/File.svg",
            Self::Eye => "icons/Eye.svg",
            Self::EyeOff => "icons/EyeOff.svg",
            Self::Retry => "icons/Retry.svg",
            Self::Send => "icons/Send.svg",
            Self::Mic => "icons/Mic.svg",
            Self::MicOff => "icons/MicOff.svg",
            Self::Headphones => "icons/Headphones.svg",
            Self::Deafen => "icons/Deafen.svg",
            Self::Settings => "icons/Settings.svg",
            Self::ScreenShare => "icons/ScreenShare.svg",
            Self::PhoneOff => "icons/PhoneOff.svg",
            Self::Volume => "icons/Volume.svg",
            Self::VolumeOff => "icons/VolumeOff.svg",
            Self::Chat => "icons/Chat.svg",
            Self::Expand => "icons/Expand.svg",
            Self::Minimize => "icons/Minimize.svg",
            Self::Fullscreen => "icons/Fullscreen.svg",
            Self::Back => "icons/Back.svg",
            Self::Refresh => "icons/Refresh.svg",
            Self::Stop => "icons/Stop.svg",
            Self::Home => "icons/Home.svg",
        }
    }
}
const ASSETS: &[(&str, &[u8])] = &[
    (
        "icons/React.svg",
        include_bytes!("../assets/icons/React.svg"),
    ),
    (
        "icons/Reply.svg",
        include_bytes!("../assets/icons/Reply.svg"),
    ),
    ("icons/More.svg", include_bytes!("../assets/icons/More.svg")),
    (
        "icons/Forward.svg",
        include_bytes!("../assets/icons/Forward.svg"),
    ),
    ("icons/Copy.svg", include_bytes!("../assets/icons/Copy.svg")),
    ("icons/Edit.svg", include_bytes!("../assets/icons/Edit.svg")),
    (
        "icons/Delete.svg",
        include_bytes!("../assets/icons/Delete.svg"),
    ),
    ("icons/Pin.svg", include_bytes!("../assets/icons/Pin.svg")),
    (
        "icons/Close.svg",
        include_bytes!("../assets/icons/Close.svg"),
    ),
    ("icons/Jump.svg", include_bytes!("../assets/icons/Jump.svg")),
    (
        "icons/Search.svg",
        include_bytes!("../assets/icons/Search.svg"),
    ),
    (
        "icons/Attachment.svg",
        include_bytes!("../assets/icons/Attachment.svg"),
    ),
    ("icons/File.svg", include_bytes!("../assets/icons/File.svg")),
    ("icons/Eye.svg", include_bytes!("../assets/icons/Eye.svg")),
    (
        "icons/EyeOff.svg",
        include_bytes!("../assets/icons/EyeOff.svg"),
    ),
    (
        "icons/Retry.svg",
        include_bytes!("../assets/icons/Retry.svg"),
    ),
    ("icons/Send.svg", include_bytes!("../assets/icons/Send.svg")),
    ("icons/Mic.svg", include_bytes!("../assets/icons/Mic.svg")),
    (
        "icons/MicOff.svg",
        include_bytes!("../assets/icons/MicOff.svg"),
    ),
    (
        "icons/Headphones.svg",
        include_bytes!("../assets/icons/Headphones.svg"),
    ),
    (
        "icons/Deafen.svg",
        include_bytes!("../assets/icons/Deafen.svg"),
    ),
    (
        "icons/Settings.svg",
        include_bytes!("../assets/icons/Settings.svg"),
    ),
    (
        "icons/ScreenShare.svg",
        include_bytes!("../assets/icons/ScreenShare.svg"),
    ),
    (
        "icons/PhoneOff.svg",
        include_bytes!("../assets/icons/PhoneOff.svg"),
    ),
    (
        "icons/Volume.svg",
        include_bytes!("../assets/icons/Volume.svg"),
    ),
    (
        "icons/VolumeOff.svg",
        include_bytes!("../assets/icons/VolumeOff.svg"),
    ),
    ("icons/Chat.svg", include_bytes!("../assets/icons/Chat.svg")),
    (
        "icons/Expand.svg",
        include_bytes!("../assets/icons/Expand.svg"),
    ),
    (
        "icons/Minimize.svg",
        include_bytes!("../assets/icons/Minimize.svg"),
    ),
    (
        "icons/Fullscreen.svg",
        include_bytes!("../assets/icons/Fullscreen.svg"),
    ),
    ("icons/Back.svg", include_bytes!("../assets/icons/Back.svg")),
    (
        "icons/Refresh.svg",
        include_bytes!("../assets/icons/Refresh.svg"),
    ),
    ("icons/Stop.svg", include_bytes!("../assets/icons/Stop.svg")),
    ("icons/Home.svg", include_bytes!("../assets/icons/Home.svg")),
];
pub(super) struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(ASSETS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }
    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(ASSETS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| (*name).into())
            .collect())
    }
}
pub(super) fn icon(kind: Icon, size: f32) -> Svg {
    svg()
        .path(kind.path())
        .size(px(size))
        .flex_shrink_0()
        .text_color(rgb(MUTED))
}
struct ControlTooltip(SharedString);
impl Render for ControlTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .max_w(px(320.))
            .whitespace_normal()
            .rounded_md()
            .bg(rgb(0x15121c))
            .text_color(rgb(TEXT))
            .text_size(px(12.))
            .child(self.0.clone())
    }
}
pub(super) fn icon_button(
    id: impl Into<ElementId>,
    kind: Icon,
    label: impl Into<SharedString>,
) -> Stateful<Div> {
    let label = label.into();
    div()
        .id(id)
        .size(px(28.))
        .p_0()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .cursor_pointer()
        .text_color(rgb(MUTED))
        .border_1()
        .border_color(transparent_black())
        .focusable()
        .tab_stop(true)
        .hover(|s| s.bg(rgb(0x3b3549)).text_color(rgb(TEXT)))
        .focus(|s| {
            s.opacity(1.)
                .bg(rgb(0x3b3549))
                .text_color(rgb(TEXT))
                .border_color(rgb(ACCENT))
        })
        .tooltip(move |_, cx| cx.new(|_| ControlTooltip(label.clone())).into())
        .child(icon(kind, 18.))
}
pub(super) fn quiet_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded_md()
        .text_size(px(12.))
        .text_color(rgb(MUTED))
        .cursor_pointer()
        .border_1()
        .border_color(transparent_black())
        .focusable()
        .tab_stop(true)
        .hover(|s| s.bg(rgb(0x3b3549)).text_color(rgb(TEXT)))
        .focus(|s| {
            s.opacity(1.)
                .bg(rgb(0x3b3549))
                .text_color(rgb(TEXT))
                .border_color(rgb(ACCENT))
        })
        .child(label.into())
}
pub(super) fn toggle(
    id: impl Into<ElementId>,
    enabled: bool,
    label: impl Into<SharedString>,
) -> Stateful<Div> {
    let label = label.into();
    div()
        .id(id)
        .w(px(36.))
        .h(px(22.))
        .px(px(3.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .when(enabled, |s| s.justify_end())
        .rounded_full()
        .bg(rgb(if enabled { ACCENT } else { 0x514c5e }))
        .cursor_pointer()
        .border_1()
        .border_color(transparent_black())
        .focusable()
        .tab_stop(true)
        .focus(|s| s.border_color(rgb(TEXT)))
        .tooltip(move |_, cx| cx.new(|_| ControlTooltip(label.clone())).into())
        .child(div().size(px(16.)).rounded_full().bg(rgb(TEXT)))
}
#[derive(Clone)]
struct SliderDrag(ElementId);
impl Render for SliderDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_0()
    }
}
fn quantize(value: f32, min: f32, max: f32, step: f32) -> f32 {
    if !value.is_finite() || !min.is_finite() || !max.is_finite() || max <= min {
        return min;
    }
    let step = if step.is_finite() && step > 0. {
        step
    } else {
        1.
    };
    (min + ((value.clamp(min, max) - min) / step).round() * step).clamp(min, max)
}
#[allow(clippy::too_many_arguments)]
pub(super) fn slider(
    id: impl Into<ElementId>,
    value: f32,
    min: f32,
    max: f32,
    step: f32,
    label: impl Into<SharedString>,
    on_change: impl Fn(&f32, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let id = id.into();
    let drag_id = id.clone();
    let label = label.into();
    let bounds = Rc::new(Cell::new(Bounds::default()));
    let paint_bounds = bounds.clone();
    let on_change = Rc::new(on_change);
    let click_change = on_change.clone();
    let drag_change = on_change.clone();
    let fraction = if max > min {
        ((value - min) / (max - min)).clamp(0., 1.)
    } else {
        0.
    };
    let at_position = move |x: Pixels, bounds: Bounds<Pixels>| {
        let width = f32::from(bounds.size.width) - 12.;
        let fraction = if width > 0. {
            ((f32::from(x - bounds.origin.x) - 6.) / width).clamp(0., 1.)
        } else {
            0.
        };
        quantize(min + fraction * (max - min), min, max, step)
    };
    div()
        .id(id)
        .h(px(28.))
        .w_full()
        .min_w(px(72.))
        .relative()
        .cursor_pointer()
        .border_1()
        .border_color(transparent_black())
        .focusable()
        .tab_stop(true)
        .rounded_md()
        .focus(|s| s.border_color(rgb(ACCENT)))
        .tooltip(move |_, cx| cx.new(|_| ControlTooltip(label.clone())).into())
        .child(
            canvas(move |b, _, _| paint_bounds.set(b), |_, _, _, _| {})
                .absolute()
                .size_full(),
        )
        .child(
            div()
                .absolute()
                .left(px(6.))
                .right(px(6.))
                .top(px(12.))
                .h(px(4.))
                .rounded_full()
                .bg(rgb(0x514c5e))
                .child(
                    div()
                        .h_full()
                        .w(relative(fraction))
                        .rounded_full()
                        .bg(rgb(ACCENT)),
                ),
        )
        .child(
            div()
                .absolute()
                .left(relative(fraction))
                .ml(px(6. - 12. * fraction))
                .top(px(8.))
                .size(px(12.))
                .rounded_full()
                .bg(rgb(TEXT)),
        )
        .on_mouse_down(MouseButton::Left, move |event, window, cx| {
            click_change(&at_position(event.position.x, bounds.get()), window, cx);
        })
        .on_drag(SliderDrag(drag_id.clone()), |drag, _, _, cx| {
            cx.new(|_| drag.clone())
        })
        .on_drag_move::<SliderDrag>(move |event, window, cx| {
            if event.drag(cx).0 == drag_id {
                drag_change(
                    &at_position(event.event.position.x, event.bounds),
                    window,
                    cx,
                );
                cx.stop_propagation();
            }
        })
        .on_key_down(move |event, window, cx| {
            let next = match event.keystroke.key.as_str() {
                "left" | "down" => value - step,
                "right" | "up" => value + step,
                "pageup" => value + step * 10.,
                "pagedown" => value - step * 10.,
                "home" => min,
                "end" => max,
                _ => return,
            };
            on_change(&quantize(next, min, max, step), window, cx);
            cx.stop_propagation();
        })
}
pub(super) fn fonts() -> Vec<std::borrow::Cow<'static, [u8]>> {
    vec![
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/FluxerSans-Regular.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/FluxerSans-Medium.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/FluxerSans-SemiBold.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/FluxerSans-Bold.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/FluxerSans-Italic.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!(
            "../assets/fonts/FluxerSans-MediumItalic.ttf"
        )),
        std::borrow::Cow::Borrowed(include_bytes!(
            "../assets/fonts/FluxerSans-SemiBoldItalic.ttf"
        )),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/FluxerSans-BoldItalic.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/FluxerMono-Regular.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/FluxerMono-Bold.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/FluxerMono-Italic.ttf")),
        std::borrow::Cow::Borrowed(include_bytes!("../assets/fonts/FluxerMono-BoldItalic.ttf")),
    ]
}

#[cfg(test)]
mod tests {
    use super::quantize;
    #[test]
    fn slider_clamps_and_rounds_to_the_control_step() {
        assert_eq!(quantize(-200., 0., 200., 1.), 0.);
        assert_eq!(quantize(345., 0., 200., 1.), 200.);
        assert_eq!(quantize(32.7, 0., 200., 1.), 33.);
        assert!((quantize(-37.4, -60., 0., 0.5) + 37.5).abs() < 0.001);
    }
}
