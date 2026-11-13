// SPDX-License-Identifier: AGPL-3.0-or-later
//! Presentation only: sources, downloading and playback stay with the controller.
use super::*;
use crate::controls::{Icon, icon_button, slider};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PlayerKind {
    Audio,
    Video,
    Gifv,
}

#[derive(Clone)]
pub(super) struct PlayerView {
    pub id: String,
    pub title: String,
    pub kind: PlayerKind,
    pub frame: Option<Arc<RenderImage>>,
    pub poster: Option<SharedString>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub position: f64,
    pub duration: Option<f64>,
    pub loading: bool,
    /// Download progress, normalized to 0..1. None means indeterminate.
    pub progress: Option<f32>,
    pub error: Option<String>,
    pub playing: bool,
    pub muted: bool,
    /// Linear volume normalized to 0..1.
    pub volume: f32,
    pub can_open_original: bool,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum PlayerAction {
    PlayPause,
    Seek(f64),
    Volume(f32),
    Mute,
    OpenFullscreen,
    Close,
    Retry,
    OpenOriginal,
}
pub(super) type PlayerCallback = Rc<dyn Fn(PlayerAction, &mut Window, &mut App)>;

/// The controller calls close for every Close action, restoring the invoking focus.
pub(super) struct PlayerModal {
    id: Option<String>,
    previous_focus: Option<FocusHandle>,
    focus: FocusHandle,
    play: FocusHandle,
    seek: FocusHandle,
    mute: FocusHandle,
    volume: FocusHandle,
    close: FocusHandle,
    original: FocusHandle,
}
impl PlayerModal {
    pub fn new(cx: &mut App) -> Self {
        Self {
            id: None,
            previous_focus: None,
            focus: cx.focus_handle(),
            play: cx.focus_handle().tab_stop(true),
            seek: cx.focus_handle().tab_stop(true),
            mute: cx.focus_handle().tab_stop(true),
            volume: cx.focus_handle().tab_stop(true),
            close: cx.focus_handle().tab_stop(true),
            original: cx.focus_handle().tab_stop(true),
        }
    }
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    pub fn is_open(&self) -> bool {
        self.id.is_some()
    }
    pub fn open(&mut self, id: String, window: &mut Window, cx: &mut App) {
        if !self.is_open() {
            self.previous_focus = window.focused(cx);
        }
        self.id = Some(id);
        self.close.focus(window);
    }
    pub fn close(&mut self, window: &mut Window, _: &mut App) {
        if let Some(previous) = self.previous_focus.take() {
            previous.focus(window);
        }
        self.reset();
    }
    pub fn reset(&mut self) {
        self.id = None;
        self.previous_focus = None;
    }
    fn controls(&self, view: &PlayerView) -> Vec<FocusHandle> {
        let mut controls = vec![self.play.clone()];
        if view.error.is_none() {
            if view.kind != PlayerKind::Gifv {
                controls.push(self.mute.clone());
                controls.push(self.volume.clone());
            }
            if valid_duration(view.duration).is_some() {
                controls.push(self.seek.clone());
            }
        }
        if view.can_open_original {
            controls.push(self.original.clone());
        }
        controls.push(self.close.clone());
        controls
    }
}

fn valid_duration(duration: Option<f64>) -> Option<f64> {
    duration.filter(|value| value.is_finite() && *value > 0. && *value <= f32::MAX as f64)
}
fn clamp_time(position: f64, duration: Option<f64>) -> f64 {
    if !position.is_finite() {
        return 0.;
    }
    position
        .max(0.)
        .min(valid_duration(duration).unwrap_or(f64::MAX))
}
fn time_label(seconds: f64) -> String {
    let seconds = if seconds.is_finite() {
        seconds.max(0.) as u64
    } else {
        0
    };
    if seconds >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{}:{:02}", seconds / 60, seconds % 60)
    }
}
/// Fits both portrait and landscape frames without stretching or upscaling.
fn fit_size(
    width: Option<u32>,
    height: Option<u32>,
    max_width: f32,
    max_height: f32,
) -> (f32, f32) {
    let (width, height) = match (width.filter(|v| *v > 0), height.filter(|v| *v > 0)) {
        (Some(w), Some(h)) => (w as f32, h as f32),
        _ => (480., 270.),
    };
    let scale = (max_width.max(1.) / width)
        .min(max_height.max(1.) / height)
        .min(1.);
    (width * scale, height * scale)
}

fn action_button(
    id: String,
    icon: Icon,
    label: &'static str,
    action: PlayerAction,
    callback: PlayerCallback,
    focus: Option<&FocusHandle>,
) -> Stateful<Div> {
    let click = callback.clone();
    icon_button(SharedString::from(id), icon, label)
        .when_some(focus, |button, focus| button.track_focus(focus))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            click(action, window, cx);
        })
        .on_key_down(move |event, window, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space")
                && !event.keystroke.modifiers.modified()
            {
                cx.stop_propagation();
                if !event.is_held {
                    callback(action, window, cx);
                }
            }
        })
}

fn keyboard_action(view: &PlayerView, key: &str) -> Option<PlayerAction> {
    if view.error.is_some() {
        return (key == "space" || key == "k").then_some(PlayerAction::Retry);
    }
    let position = clamp_time(view.position, view.duration);
    let seek = |target| {
        valid_duration(view.duration).map(|_| PlayerAction::Seek(clamp_time(target, view.duration)))
    };
    match key {
        "space" | "k" => Some(PlayerAction::PlayPause),
        "left" | "j" => seek(position - 5.),
        "right" | "l" => seek(position + 5.),
        "home" => seek(0.),
        "end" => seek(view.duration.unwrap_or(0.)),
        "up" if view.kind != PlayerKind::Gifv => {
            Some(PlayerAction::Volume((view.volume + 0.05).clamp(0., 1.)))
        }
        "down" if view.kind != PlayerKind::Gifv => {
            Some(PlayerAction::Volume((view.volume - 0.05).clamp(0., 1.)))
        }
        "m" if view.kind != PlayerKind::Gifv => Some(PlayerAction::Mute),
        "f" if view.kind != PlayerKind::Audio => Some(PlayerAction::OpenFullscreen),
        _ => None,
    }
}

fn controls(
    view: &PlayerView,
    callback: PlayerCallback,
    modal: Option<&PlayerModal>,
) -> AnyElement {
    let prefix = format!(
        "{}-{}",
        if modal.is_some() { "modal" } else { "inline" },
        view.id
    );
    let failed = view.error.is_some();
    let mut row = div()
        .w_full()
        .min_w_0()
        .flex()
        .items_center()
        .gap_1()
        .child(action_button(
            format!("{prefix}-play"),
            if failed {
                Icon::Retry
            } else if view.playing {
                Icon::Pause
            } else {
                Icon::Play
            },
            if failed {
                "Retry playback"
            } else if view.playing {
                "Pause (Space)"
            } else {
                "Play (Space)"
            },
            if failed {
                PlayerAction::Retry
            } else {
                PlayerAction::PlayPause
            },
            callback.clone(),
            modal.map(|m| &m.play),
        ));
    if !failed {
        row = row.child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(px(11.))
                .text_color(rgb(MUTED))
                .child(format!(
                    "{} / {}",
                    time_label(clamp_time(view.position, view.duration)),
                    valid_duration(view.duration)
                        .map(time_label)
                        .unwrap_or_else(|| "–:––".into())
                )),
        );
        if view.kind != PlayerKind::Gifv {
            let volume = callback.clone();
            row = row
                .child(action_button(
                    format!("{prefix}-mute"),
                    if view.muted || view.volume <= 0. {
                        Icon::VolumeOff
                    } else {
                        Icon::Volume
                    },
                    if view.muted { "Unmute (M)" } else { "Mute (M)" },
                    PlayerAction::Mute,
                    callback.clone(),
                    modal.map(|m| &m.mute),
                ))
                .child(
                    div().w(px(60.)).flex_shrink_0().child(
                        slider(
                            SharedString::from(format!("{prefix}-volume")),
                            if view.muted { 0. } else { view.volume },
                            0.,
                            1.,
                            0.05,
                            "Volume · Arrow keys",
                            move |value, window, cx| {
                                cx.stop_propagation();
                                volume(PlayerAction::Volume(*value), window, cx);
                            },
                        )
                        .min_w(px(40.))
                        .when_some(modal, |s, m| s.track_focus(&m.volume)),
                    ),
                );
        }
    } else {
        row = row.child(div().flex_1());
    }
    if modal.is_none() && view.kind != PlayerKind::Audio {
        row = row.child(action_button(
            format!("{prefix}-fullscreen"),
            Icon::Fullscreen,
            "Full view (F)",
            PlayerAction::OpenFullscreen,
            callback.clone(),
            modal.map(|m| &m.original),
        ));
    }
    if view.can_open_original {
        row = row.child(action_button(
            format!("{prefix}-original"),
            Icon::Jump,
            "Open original",
            PlayerAction::OpenOriginal,
            callback.clone(),
            None,
        ));
    }
    let mut body = div().w_full().min_w_0().flex().flex_col().child(row);
    if !failed && let Some(duration) = valid_duration(view.duration) {
        body = body.child(
            slider(
                SharedString::from(format!("{prefix}-seek")),
                clamp_time(view.position, view.duration) as f32,
                0.,
                duration as f32,
                1.,
                "Seek · Arrow keys, Home, End",
                move |value, window, cx| {
                    cx.stop_propagation();
                    callback(PlayerAction::Seek(*value as f64), window, cx);
                },
            )
            .when_some(modal, |s, m| s.track_focus(&m.seek)),
        );
    }
    body.into_any_element()
}

fn status(view: &PlayerView) -> Option<AnyElement> {
    let text = if let Some(error) = &view.error {
        format!("Playback unavailable · {error}")
    } else if view.loading {
        match view.progress.filter(|p| p.is_finite()) {
            Some(progress) => format!(
                "Loading… {}%",
                (progress.clamp(0., 1.) * 100.).round() as u32
            ),
            None => "Loading…".into(),
        }
    } else {
        return None;
    };
    Some(
        div()
            .px_2()
            .py_1()
            .max_w_full()
            .text_size(px(12.))
            .text_color(rgb(MUTED))
            .child(text)
            .into_any_element(),
    )
}

fn video_surface(
    view: &PlayerView,
    callback: PlayerCallback,
    width: f32,
    height: f32,
    modal: bool,
) -> AnyElement {
    let click = callback.clone();
    let click_action = if view.error.is_some() {
        PlayerAction::Retry
    } else if view.kind == PlayerKind::Gifv && !modal {
        PlayerAction::OpenFullscreen
    } else {
        PlayerAction::PlayPause
    };
    let mut surface = div()
        .id(SharedString::from(format!(
            "{}-{}-surface",
            if modal { "modal" } else { "inline" },
            view.id
        )))
        .w(px(width))
        .max_w_full()
        .mx_auto()
        .h(px(height))
        .relative()
        .overflow_hidden()
        .bg(rgb(0x0c0b10))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(!modal, |s| s.focusable().tab_stop(true))
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            click(click_action, window, cx);
        });
    if let Some(frame) = &view.frame {
        surface = surface.child(
            img(frame.clone())
                .size_full()
                .object_fit(ObjectFit::Contain),
        );
    } else if let Some(poster) = &view.poster {
        surface = surface.child(
            img(poster.clone())
                .size_full()
                .object_fit(ObjectFit::Contain)
                .with_fallback(|| div().size_full().into_any_element()),
        );
    }
    if !view.playing && !view.loading && view.error.is_none() {
        surface = surface.child(
            div()
                .absolute()
                .size(px(48.))
                .rounded_full()
                .bg(rgba(0x1d1b26cc))
                .flex()
                .items_center()
                .justify_center()
                .child(crate::controls::icon(Icon::Play, 28.)),
        );
    }
    if view.kind == PlayerKind::Gifv || !view.playing {
        surface = surface.child(
            div()
                .absolute()
                .top_2()
                .left_2()
                .px_1()
                .rounded_sm()
                .bg(rgba(0x1d1b26cc))
                .text_size(px(10.))
                .text_color(rgb(TEXT))
                .child(if view.kind == PlayerKind::Gifv {
                    "GIF"
                } else {
                    "VIDEO"
                }),
        );
    }
    if let Some(status) = status(view) {
        surface = surface.child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x0c0b10bb))
                .child(status),
        );
    }
    surface.into_any_element()
}

pub(super) fn render_inline(
    view: &PlayerView,
    callback: PlayerCallback,
    cx: &mut App,
) -> AnyElement {
    let (width, height) = fit_size(view.width, view.height, 420., 300.);
    let key_view = view.clone();
    let keyboard = callback.clone();
    let mut card = div()
        .id(SharedString::from(format!("inline-player-{}", view.id)))
        .w(px(if view.kind == PlayerKind::Audio {
            420.
        } else {
            width.max(300.)
        }))
        .max_w_full()
        .min_w_0()
        .rounded_lg()
        .overflow_hidden()
        .border_1()
        .border_color(rgb(0x3b3549))
        .bg(rgb(PANEL))
        .flex()
        .flex_col()
        .on_click(|_, _, cx| cx.stop_propagation())
        .on_key_down(move |event, window, cx| {
            if !event.keystroke.modifiers.modified()
                && let Some(action) = keyboard_action(&key_view, &event.keystroke.key)
            {
                cx.stop_propagation();
                if !event.is_held
                    || matches!(action, PlayerAction::Seek(_) | PlayerAction::Volume(_))
                {
                    keyboard(action, window, cx);
                }
            }
        });
    if view.kind != PlayerKind::Audio {
        card = card.child(video_surface(view, callback.clone(), width, height, false));
    }
    if view.kind == PlayerKind::Gifv {
        return card.into_any_element();
    }
    card = card.child(
        div()
            .w_full()
            .min_w_0()
            .px_2()
            .pt_1()
            .text_size(px(12.))
            .text_color(rgb(TEXT))
            .text_ellipsis()
            .child(emoji_sprites::unicode_text(&view.title, 12., false, cx)),
    );
    if view.kind == PlayerKind::Audio
        && let Some(status) = status(view)
    {
        card = card.child(status);
    }
    card.child(div().px_1().pb_1().child(controls(view, callback, None)))
        .into_any_element()
}

pub(super) fn render_modal(
    view: &PlayerView,
    modal: &PlayerModal,
    callback: PlayerCallback,
    window: &Window,
    cx: &mut App,
) -> AnyElement {
    if modal.id() != Some(view.id.as_str()) {
        return div().into_any_element();
    }
    let (width, height) = fit_size(
        view.width,
        view.height,
        f32::from(window.viewport_size().width) - 48.,
        f32::from(window.viewport_size().height) - 190.,
    );
    let close = callback.clone();
    let keys = callback.clone();
    let backdrop = callback.clone();
    let controls_focus = modal.controls(view);
    let key_view = view.clone();
    deferred(
        div()
            .id("media-player-modal")
            .absolute()
            .inset_0()
            .occlude()
            .track_focus(&modal.focus)
            .bg(rgba(0x08070aee))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                backdrop(PlayerAction::Close, window, cx);
            })
            .capture_key_down(
                move |event, window, cx| match event.keystroke.key.as_str() {
                    "escape" => {
                        cx.stop_propagation();
                        close(PlayerAction::Close, window, cx);
                    }
                    "tab" => {
                        cx.stop_propagation();
                        let current = controls_focus.iter().position(|f| f.is_focused(window));
                        let index = match (current, event.keystroke.modifiers.shift) {
                            (Some(i), true) => {
                                (i + controls_focus.len() - 1) % controls_focus.len()
                            }
                            (Some(i), false) => (i + 1) % controls_focus.len(),
                            (None, true) => controls_focus.len() - 1,
                            (None, false) => 0,
                        };
                        controls_focus[index].focus(window);
                    }
                    _ => {}
                },
            )
            .on_key_down(move |event, window, cx| {
                cx.stop_propagation();
                if !event.keystroke.modifiers.modified()
                    && let Some(action) = keyboard_action(&key_view, &event.keystroke.key)
                    && action != PlayerAction::OpenFullscreen
                    && (!event.is_held
                        || matches!(action, PlayerAction::Seek(_) | PlayerAction::Volume(_)))
                {
                    keys(action, window, cx);
                }
            })
            .child(
                div()
                    .id("media-player-header")
                    .w_full()
                    .min_w_0()
                    .h(px(36.))
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
                            .child(emoji_sprites::unicode_text(&view.title, 12., false, cx)),
                    )
                    .child(action_button(
                        "media-player-close".into(),
                        Icon::Close,
                        "Close media (Esc)",
                        PlayerAction::Close,
                        callback.clone(),
                        Some(&modal.close),
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .id("media-player-body")
                            .w(px(width.max(300.)))
                            .flex_shrink_0()
                            .when(view.kind != PlayerKind::Audio, |node| {
                                node.h(px(height
                                    + if view.error.is_none()
                                        && valid_duration(view.duration).is_some()
                                    {
                                        72.
                                    } else {
                                        44.
                                    }))
                            })
                            .max_w_full()
                            .flex()
                            .flex_col()
                            .rounded_lg()
                            .overflow_hidden()
                            .bg(rgb(PANEL))
                            .on_click(|_, _, cx| cx.stop_propagation())
                            .when(view.kind != PlayerKind::Audio, |s| {
                                s.child(video_surface(view, callback.clone(), width, height, true))
                            })
                            .when(view.kind == PlayerKind::Audio, |s| {
                                s.child(div().p_4().child(emoji_sprites::unicode_text(
                                    &view.title,
                                    12.,
                                    false,
                                    cx,
                                )))
                                .children(status(view))
                            })
                            .child(div().p_2().child(controls(view, callback, Some(modal)))),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .text_color(rgb(MUTED))
                    .text_center()
                    .child("Space Play / pause · ← → Seek · ↑ ↓ Volume · M Mute · Esc Close"),
            ),
    )
    .with_priority(6)
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{
        PlayerAction, PlayerKind, PlayerView, clamp_time, fit_size, keyboard_action, time_label,
        valid_duration,
    };
    #[test]
    fn aspect_fit_preserves_portrait_landscape_and_small_media() {
        assert_eq!(fit_size(Some(1920), Some(1080), 420., 300.), (420., 236.25));
        assert_eq!(fit_size(Some(100), Some(200), 420., 300.), (100., 200.));
        assert_eq!(fit_size(Some(1080), Some(1920), 420., 300.), (168.75, 300.));
        assert_eq!(fit_size(Some(0), None, 480., 270.), (480., 270.));
    }
    #[test]
    fn invalid_metadata_cannot_poison_seek_or_time_labels() {
        assert_eq!(clamp_time(f64::NAN, Some(10.)), 0.);
        assert_eq!(clamp_time(12., Some(10.)), 10.);
        assert_eq!(clamp_time(-2., Some(f64::INFINITY)), 0.);
        assert_eq!(valid_duration(Some(f64::NAN)), None);
        assert_eq!(time_label(3661.), "1:01:01");
        assert_eq!(time_label(62.), "1:02");
        assert_eq!(time_label(f64::INFINITY), "0:00");
    }
    #[test]
    fn keyboard_seeking_clamps_and_gifv_never_exposes_audio_actions() {
        let mut view = PlayerView {
            id: "test".into(),
            title: "clip.mp4".into(),
            kind: PlayerKind::Video,
            frame: None,
            poster: None,
            width: None,
            height: None,
            position: 8.,
            duration: Some(10.),
            loading: false,
            progress: None,
            error: None,
            playing: false,
            muted: false,
            volume: 1.,
            can_open_original: false,
        };
        assert_eq!(
            keyboard_action(&view, "right"),
            Some(PlayerAction::Seek(10.))
        );
        assert_eq!(keyboard_action(&view, "home"), Some(PlayerAction::Seek(0.)));
        assert_eq!(keyboard_action(&view, "up"), Some(PlayerAction::Volume(1.)));
        view.duration = None;
        assert_eq!(keyboard_action(&view, "left"), None);
        view.kind = PlayerKind::Gifv;
        assert_eq!(keyboard_action(&view, "m"), None);
        assert_eq!(keyboard_action(&view, "down"), None);
        assert_eq!(
            keyboard_action(&view, "space"),
            Some(PlayerAction::PlayPause)
        );
        view.error = Some("decoder unavailable".into());
        assert_eq!(keyboard_action(&view, "space"), Some(PlayerAction::Retry));
    }
}
