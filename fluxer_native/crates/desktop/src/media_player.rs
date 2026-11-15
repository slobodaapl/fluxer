// SPDX-License-Identifier: AGPL-3.0-or-later
//! GPUI ownership, download lifecycle and presentation of native file players.
use super::*;
use crate::{
    media_download::Download,
    media_player_view::{PlayerAction, PlayerCallback, PlayerKind, PlayerModal, PlayerView},
    playable_media::{Concealment, GifvPlaybackSignals, PlayableKind, PlayableMedia},
};
use fluxer_media::{FilePlaybackOptions, FilePlaybackSnapshot, FilePlaybackStatus, FilePlayer};
use futures::{FutureExt, StreamExt};
use std::{collections::HashSet, rc::Rc};

#[derive(Clone, Copy)]
enum PlayerEvent {
    Open,
    Close,
    Reveal,
    Original,
    Demo(PlayerAction),
    Preferences(PlayableKind, PlaybackPreference),
}

/// Player slider value and mute state, scoped separately for audio and video.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PlaybackPreference {
    pub volume: f32,
    pub muted: bool,
}
impl Default for PlaybackPreference {
    fn default() -> Self {
        Self {
            volume: 1.0,
            muted: false,
        }
    }
}

struct MediaPlayer {
    source: PlayableMedia,
    focus: FocusHandle,
    download: Option<Download>,
    range_progress: Option<Arc<crate::media_range::RangeProgress>>,
    file: Option<Arc<tempfile::NamedTempFile>>,
    player: Option<FilePlayer>,
    snapshot: Option<FilePlaybackSnapshot>,
    frame: Option<Arc<RenderImage>>,
    blurred: Option<Arc<RenderImage>>,
    download_error: Option<String>,
    visible: bool,
    hovered: bool,
    revealed: bool,
    autoplay: bool,
    enabled: bool,
    modal: bool,
    active: bool,
    wanted: bool,
    user_paused: bool,
    hidden_hold: bool,
    muted: bool,
    volume: f32,
    output: Option<String>,
    last_policy: bool,
    pending_seek: Option<f64>,
    wake: futures::channel::mpsc::Sender<()>,
    demo_actions: bool,
    demo_step: usize,
    demo_started: Option<std::time::Instant>,
    fast_poll_until: std::time::Instant,
    last_prepaint: std::time::Instant,
    activation_observer: Option<Subscription>,
    fixture_trace: bool,
    awaiting_prepaint: bool,
}
impl EventEmitter<PlayerEvent> for MediaPlayer {}

impl MediaPlayer {
    fn new(
        source: PlayableMedia,
        output: Option<String>,
        preference: PlaybackPreference,
        cx: &mut Context<Self>,
    ) -> Self {
        let defaults = source.kind.defaults();
        let demo_auto = source.kind == PlayableKind::Video
            && source.title == "Generated silent MP4"
            && source.source.starts_with("http://127.0.0.1:")
            && std::env::args().any(|arg| arg == "--demo-media")
            && std::env::var("FLUXER_MEDIA_DEMO_AUTO_PLAY").as_deref() == Ok("1");
        let (wake, mut events) = futures::channel::mpsc::channel(1);
        cx.spawn(async move |entity, cx| {
            let mut cadence = Duration::from_secs(10);
            loop {
                let timer = FutureExt::fuse(Timer::after(cadence));
                let awakened = events.next().fuse();
                futures::pin_mut!(timer, awakened);
                futures::select! {
                    _ = timer => (),
                    event = awakened => if event.is_none() { break; },
                }
                let Ok(next_cadence) = entity.update(cx, |player, cx| {
                    player.poll(cx);
                    if std::time::Instant::now() < player.fast_poll_until {
                        Duration::from_millis(16)
                    } else if player.demo_actions && player.demo_step < 5 {
                        Duration::from_millis(50)
                    } else if player.source.kind != PlayableKind::Audio
                        && (player.visible || player.modal)
                        && player.is_playing()
                    {
                        Duration::from_millis(16)
                    } else if player.download.is_some()
                        || player.is_playing()
                        || player
                            .snapshot
                            .as_ref()
                            .is_some_and(|snapshot| snapshot.status == FilePlaybackStatus::Loading)
                    {
                        Duration::from_millis(100)
                    } else if player.player.is_some() {
                        Duration::from_secs(1)
                    } else {
                        Duration::from_secs(10)
                    }
                }) else {
                    break;
                };
                cadence = next_cadence;
            }
        })
        .detach();
        Self {
            source: source.clone(),
            focus: cx.focus_handle(),
            output,
            download: None,
            range_progress: None,
            file: None,
            player: None,
            snapshot: None,
            frame: None,
            blurred: None,
            download_error: None,
            visible: false,
            hovered: false,
            revealed: false,
            autoplay: defaults.autoplay,
            enabled: true,
            modal: false,
            active: true,
            wanted: demo_auto,
            user_paused: false,
            hidden_hold: false,
            muted: if source.kind == PlayableKind::Gifv {
                defaults.muted
            } else {
                preference.muted
            },
            volume: preference.volume,
            last_policy: false,
            pending_seek: None,
            wake,
            demo_actions: demo_auto
                && std::env::var("FLUXER_MEDIA_DEMO_ACTIONS").as_deref() == Ok("1"),
            demo_step: 0,
            demo_started: None,
            fast_poll_until: std::time::Instant::now(),
            last_prepaint: std::time::Instant::now(),
            activation_observer: None,
            awaiting_prepaint: false,
            fixture_trace: std::env::args().any(|arg| arg == "--demo-media")
                && std::env::var("FLUXER_MEDIA_DEMO_GIF_ONLY").as_deref() == Ok("1"),
        }
    }

    fn is_playing(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.status == FilePlaybackStatus::Playing)
    }
    fn ensure_loaded(&mut self) {
        let _ = self.wake.try_send(());
        if self.player.is_some() || self.download.is_some() || self.download_error.is_some() {
            return;
        }
        if self.fixture_trace {
            eprintln!("GIF_FIXTURE decoder-open");
        }
        if let Some(file) = self.file.clone() {
            self.open_file(file);
        } else {
            let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let interrupt = Arc::new(std::sync::atomic::AtomicU64::new(0));
            let source = crate::media_range::RangeReader::new(
                self.source.source.clone(),
                self.source.size,
                cancel.clone(),
                interrupt.clone(),
            );
            let waiting = source
                .as_ref()
                .map(|source| source.progress_handle().wait_handle())
                .unwrap_or_else(|_| Arc::new(std::sync::atomic::AtomicBool::new(false)));
            if let Ok(source) = &source {
                self.range_progress = Some(source.progress_handle());
            }
            let defaults = self.source.kind.defaults();
            match source.and_then(|source| {
                FilePlayer::open_reader(
                    Box::new(source),
                    cancel,
                    interrupt,
                    waiting,
                    FilePlaybackOptions {
                        autoplay: false,
                        looping: defaults.looping,
                        muted: self.muted,
                        volume: self.volume,
                        output_device: self.output.clone(),
                    },
                )
            }) {
                Ok(player) => {
                    self.player = Some(player);
                    self.last_policy = false;
                    if let Some(position) = self.pending_seek.take()
                        && let Some(player) = &self.player
                    {
                        player.seek(position);
                    }
                }
                Err(error) => self.download_error = Some(error),
            }
        }
    }
    fn open_file(&mut self, file: Arc<tempfile::NamedTempFile>) {
        let defaults = self.source.kind.defaults();
        match FilePlayer::open(
            file.path().to_owned(),
            FilePlaybackOptions {
                autoplay: false,
                looping: defaults.looping,
                muted: self.muted,
                volume: self.volume,
                output_device: self.output.clone(),
            },
        ) {
            Ok(player) => {
                self.player = Some(player);
                self.file = Some(file);
                self.last_policy = false;
                if let Some(position) = self.pending_seek.take()
                    && let Some(native) = &self.player
                {
                    native.seek(position);
                }
            }
            Err(error) => self.download_error = Some(error),
        }
    }
    fn should_play(&self) -> bool {
        if !self.revealed || self.user_paused || (self.hidden_hold && !self.active) {
            return false;
        }
        if self.modal {
            return self.wanted;
        }
        if self.source.kind == PlayableKind::Gifv {
            if self.awaiting_prepaint {
                return false;
            }
            playable_media::gifv_should_play(GifvPlaybackSignals {
                near_viewport: self.visible,
                in_viewport: self.visible,
                revealed: self.revealed,
                document_visible: self.active,
                window_focused_or_interactive: self.active || self.hovered,
                viewer_open: !self.enabled,
                poster_revealed: true,
                autoplay: self.autoplay,
                interacting: self.hovered || self.wanted,
                reduced_motion: false,
                keep_under_reduced_motion: false,
                save_data: false,
            })
        } else {
            self.wanted
        }
    }
    fn apply_policy(&mut self) {
        let play = self.should_play();
        if let Some(player) = &self.player
            && play != self.last_policy
        {
            if play {
                player.play();
            } else {
                player.pause();
            }
            self.last_policy = play;
            self.fast_poll_until = std::time::Instant::now() + Duration::from_millis(300);
        }
    }
    fn poll(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        // Virtualized rows may unmount without another clipped prepaint. Only
        // actively playing media promises continuous redraws: loading, buffering
        // and paused players can legitimately have no new frame for a while.
        // Expiring their visibility would cancel a slow fetch and restart it on
        // the next prepaint, keeping the visible GIF in Loading forever.
        // An inactive Wayland window may receive no frames: this is not evidence
        // that its on-screen GIF scrolled away. Keep the paused decoder cached.
        if self.visible
            && self.active
            && self.is_playing()
            && !self.awaiting_prepaint
            && !self.modal
            && self.last_prepaint.elapsed() > Duration::from_millis(250)
        {
            if self.fixture_trace {
                eprintln!(
                    "GIF_FIXTURE stale-visibility active={} age_ms={}",
                    self.active,
                    self.last_prepaint.elapsed().as_millis()
                );
            }
            self.visible = false;
            self.hovered = false;
            changed = true;
        }
        if let Some(download) = &self.download {
            if let Some(result) = download.try_finish() {
                self.download.take();
                match result {
                    Ok(file) => {
                        if self.visible || self.modal || self.wanted {
                            self.open_file(file);
                        } else {
                            self.file = Some(file);
                        }
                    }
                    Err(error) => self.download_error = Some(error),
                }
            }
            changed = true;
        }
        if !self.visible
            && !self.modal
            && (self.source.kind == PlayableKind::Gifv || !self.revealed)
            && self.player.is_some()
        {
            // Keep the cached file and tiny concealed preview, not a collection
            // of paused decoders/full-resolution GPU textures after scrolling.
            if self.fixture_trace {
                eprintln!("GIF_FIXTURE decoder-retired");
            }
            self.player.take();
            self.last_policy = false;
            if let Some(snapshot) = &mut self.snapshot {
                snapshot.status = FilePlaybackStatus::Paused;
            }
            if let Some(old) = self.frame.take() {
                cx.defer(move |cx| cx.drop_image(old, None));
            }
            changed = true;
        }
        self.apply_policy();
        if let Some(player) = &self.player {
            let snapshot = player.snapshot();
            let frame = if self.visible || self.modal {
                player.take_frame()
            } else {
                None
            };
            changed |= self.snapshot.as_ref().is_none_or(|old| {
                old.status != snapshot.status
                    || old.error != snapshot.error
                    || (old.position - snapshot.position).abs() > 0.04
                    || old.duration != snapshot.duration
            });
            if snapshot.status == FilePlaybackStatus::Ended
                && self.source.kind != PlayableKind::Gifv
            {
                self.wanted = false;
                self.last_policy = false;
            }
            self.snapshot = Some(snapshot);
            if let Some(frame) = frame
                && let Some(buffer) = image::RgbaImage::from_raw(
                    frame.width,
                    frame.height,
                    Arc::unwrap_or_clone(frame.bgra),
                )
            {
                let image = Arc::new(RenderImage::new(vec![image::Frame::new(buffer)]));
                if let Some(old) = self.frame.replace(image.clone()) {
                    cx.defer(move |cx| cx.drop_image(old, None));
                }
                if !self.revealed && self.blurred.is_none() {
                    let executor = cx.background_executor().clone();
                    cx.spawn(async move |entity, cx| {
                        let result = executor
                            .spawn(async move {
                                concealed_image::blur_first_frame(&image).ok().map(Arc::new)
                            })
                            .await;
                        let _ = entity.update(cx, |player, cx| {
                            if let Some(image) = result
                                && let Some(old) = player.blurred.replace(image)
                            {
                                cx.defer(move |cx| cx.drop_image(old, None));
                            }
                            cx.notify();
                        });
                    })
                    .detach();
                }
                changed = true;
            }
        }
        if self.demo_actions {
            if self.demo_started.is_none() && self.is_playing() {
                self.demo_started = Some(std::time::Instant::now());
            }
            if let Some(started) = self.demo_started {
                let steps = [
                    (0.3, PlayerAction::OpenFullscreen),
                    (0.7, PlayerAction::PlayPause),
                    (1.0, PlayerAction::Seek(1.5)),
                    (1.3, PlayerAction::PlayPause),
                    (1.8, PlayerAction::Close),
                ];
                if let Some((after, action)) = steps.get(self.demo_step).copied()
                    && started.elapsed().as_secs_f64() >= after
                {
                    self.demo_step += 1;
                    eprintln!(
                        "FILE_MEDIA_DEMO_ACTION step={} action={action:?}",
                        self.demo_step
                    );
                    cx.emit(PlayerEvent::Demo(action));
                }
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn view(&self) -> PlayerView {
        let snapshot = self.snapshot.as_ref();
        PlayerView {
            id: self.source.resource_key.clone(),
            title: self.source.title.clone(),
            kind: match self.source.kind {
                PlayableKind::Audio => PlayerKind::Audio,
                PlayableKind::Video => PlayerKind::Video,
                PlayableKind::Gifv => PlayerKind::Gifv,
            },
            frame: if self.revealed {
                self.frame.clone()
            } else {
                self.blurred.clone()
            },
            poster: self
                .revealed
                .then(|| self.source.poster.clone().map(SharedString::from))
                .flatten(),
            width: snapshot
                .map(|s| s.width)
                .filter(|v| *v > 0)
                .or(self.source.width),
            height: snapshot
                .map(|s| s.height)
                .filter(|v| *v > 0)
                .or(self.source.height),
            position: self
                .pending_seek
                .unwrap_or_else(|| snapshot.map_or(0., |s| s.position)),
            duration: snapshot.and_then(|s| s.duration).or(self.source.duration),
            loading: self.download.is_some()
                || snapshot.is_some_and(|s| s.status == FilePlaybackStatus::Loading),
            buffering: snapshot
                .is_some_and(|s| s.status == FilePlaybackStatus::Loading && s.position > 0.0),
            progress: self
                .download
                .as_ref()
                .and_then(Download::progress)
                .or_else(|| {
                    self.range_progress
                        .as_ref()
                        .and_then(|progress| progress.progress())
                }),
            error: self
                .download_error
                .clone()
                .or_else(|| snapshot.and_then(|s| s.error.clone())),
            playing: !self.user_paused && (self.is_playing() || self.wanted),
            muted: self.muted,
            volume: self.volume,
            can_open_original: self.source.original.is_some(),
        }
    }

    fn callback(entity: WeakEntity<Self>) -> PlayerCallback {
        Rc::new(move |action, window, cx| {
            let _ = entity.update(cx, |player, cx| player.action(action, window, cx));
        })
    }
    fn action(&mut self, action: PlayerAction, window: &mut Window, cx: &mut Context<Self>) {
        self.fast_poll_until = std::time::Instant::now() + Duration::from_millis(500);
        match action {
            PlayerAction::OpenOriginal => cx.emit(PlayerEvent::Original),
            PlayerAction::OpenFullscreen => {
                self.modal = true;
                self.wanted = self.is_playing() || self.source.kind == PlayableKind::Gifv;
                cx.emit(PlayerEvent::Open);
            }
            PlayerAction::Close => {
                self.modal = false;
                cx.emit(PlayerEvent::Close);
            }
            PlayerAction::Retry => {
                self.download_error = None;
                self.player.take();
                self.file.take();
                self.snapshot = None;
                self.ensure_loaded();
            }
            PlayerAction::PlayPause => {
                self.wanted = !((self.is_playing() && !self.user_paused) || self.wanted);
                self.user_paused = !self.wanted;
                self.hidden_hold = self.wanted
                    && playable_media::hold_play_started_hidden(
                        self.source.kind,
                        window.is_window_active(),
                        false,
                    );
                if self.wanted && !self.hidden_hold {
                    self.ensure_loaded();
                }
            }
            PlayerAction::Seek(position) => {
                self.ensure_loaded();
                if let Some(player) = &self.player {
                    player.seek(position.max(0.));
                } else {
                    self.pending_seek = Some(position.max(0.));
                }
                if let Some(snapshot) = &mut self.snapshot {
                    snapshot.position = position.max(0.);
                }
            }
            PlayerAction::Volume(volume) => {
                self.volume = volume.clamp(0., 1.);
                if self.volume > 0. {
                    self.muted = false;
                }
                if let Some(player) = &self.player {
                    player.set_volume(self.volume);
                    player.set_muted(self.muted);
                }
                if self.source.kind != PlayableKind::Gifv {
                    cx.emit(PlayerEvent::Preferences(
                        self.source.kind,
                        PlaybackPreference {
                            volume: self.volume,
                            muted: self.muted,
                        },
                    ));
                }
            }
            PlayerAction::Mute => {
                self.muted = !self.muted;
                if let Some(player) = &self.player {
                    player.set_muted(self.muted);
                }
                if self.source.kind != PlayableKind::Gifv {
                    cx.emit(PlayerEvent::Preferences(
                        self.source.kind,
                        PlaybackPreference {
                            volume: self.volume,
                            muted: self.muted,
                        },
                    ));
                }
            }
        }
        self.apply_policy();
        let _ = self.wake.try_send(());
        cx.notify();
    }
    fn retire(&mut self, cx: &mut Context<Self>) {
        self.download.take();
        self.player.take();
        self.file.take();
        for old in [self.frame.take(), self.blurred.take()]
            .into_iter()
            .flatten()
        {
            cx.defer(move |cx| cx.drop_image(old, None));
        }
    }
}

impl Render for MediaPlayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.activation_observer.is_none() {
            // Compositors may stop frame callbacks for an inactive window.
            // Apply focus policy from the event, not from a future repaint.
            self.activation_observer =
                Some(cx.observe_window_activation(window, |player, window, cx| {
                    player.active = window.is_window_active();
                    player.awaiting_prepaint = player.active;
                    if player.fixture_trace {
                        eprintln!("GIF_FIXTURE activation={}", player.active);
                    }
                    if player.active {
                        player.last_prepaint = std::time::Instant::now();
                        player.hidden_hold = false;
                    }
                    player.apply_policy();
                    cx.notify();
                }));
        }
        self.active = window.is_window_active();
        self.apply_policy();
        let weak = cx.entity().downgrade();
        let marker = canvas(
            move |bounds, window, cx| {
                let clipped = bounds
                    .intersect(&window.content_mask().bounds)
                    .intersect(&Bounds::new(point(px(0.), px(0.)), window.viewport_size()));
                let visible = clipped.size.width > px(0.) && clipped.size.height > px(0.);
                let hovered = visible && clipped.contains(&window.mouse_position());
                cx.defer(move |cx| {
                    let _ = weak.update(cx, |player, cx| {
                        player.last_prepaint = std::time::Instant::now();
                        if player.visible != visible
                            || player.hovered != hovered
                            || player.awaiting_prepaint
                        {
                            player.awaiting_prepaint = false;
                            player.visible = visible;
                            player.hovered = hovered;
                            // Concealed media preloads paused. Normal video/audio
                            // require Play; GIFV follows the source animation gate.
                            if visible
                                && (player.wanted
                                    || !player.revealed
                                    || player.source.kind == PlayableKind::Gifv)
                            {
                                player.ensure_loaded();
                            }
                            player.apply_policy();
                            cx.notify();
                        }
                    });
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let view = self.view();
        let concealed = !self.revealed;
        let reveal = cx.entity().downgrade();
        div()
            .id("native-player-root")
            .relative()
            .max_w_full()
            .min_w_0()
            .track_focus(&self.focus)
            .on_hover(cx.listener(|player, hovered: &bool, _, cx| {
                player.hovered = *hovered;
                player.apply_policy();
                cx.notify();
            }))
            .child(if concealed {
                let label = if self.source.concealment == Some(Concealment::Sensitive) {
                    "Sensitive media · Click to reveal"
                } else {
                    "Spoiler · Click to reveal"
                };
                div()
                    .id("concealed-native-media")
                    .relative()
                    .w(px(400.))
                    .max_w_full()
                    .min_w_0()
                    .h(px(if self.source.kind == PlayableKind::Audio {
                        64.
                    } else {
                        225.
                    }))
                    .rounded_lg()
                    .overflow_hidden()
                    .bg(rgb(PANEL))
                    .cursor_pointer()
                    .focusable()
                    .tab_stop(true)
                    .when_some(view.frame, |node, image| {
                        node.child(img(image).size_full().object_fit(ObjectFit::Cover))
                    })
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .rounded_full()
                                    .bg(rgba(0x17131fee))
                                    .text_size(px(12.))
                                    .child(label),
                            ),
                    )
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        let _ = reveal.update(cx, |_, cx| cx.emit(PlayerEvent::Reveal));
                    })
                    .on_key_down(cx.listener(|_, event: &KeyDownEvent, _, cx| {
                        if matches!(event.keystroke.key.as_str(), "enter" | "space")
                            && !event.is_held
                        {
                            cx.stop_propagation();
                            cx.emit(PlayerEvent::Reveal);
                        }
                    }))
                    .into_any_element()
            } else {
                media_player_view::render_inline(&view, Self::callback(cx.entity().downgrade()), cx)
            })
            .child(marker)
    }
}

pub(super) struct PlaybackUi {
    players: BTreeMap<String, Entity<MediaPlayer>>,
    seen: HashSet<String>,
    contexts: BTreeMap<String, crate::media_actions::MediaReference>,
    pub modal: PlayerModal,
    output: Option<String>,
    retained_browser: HashSet<String>,
    audio_preference: PlaybackPreference,
    video_preference: PlaybackPreference,
}
impl PlaybackUi {
    pub fn new(cx: &mut App) -> Self {
        Self {
            players: BTreeMap::new(),
            seen: HashSet::new(),
            contexts: BTreeMap::new(),
            modal: PlayerModal::new(cx),
            output: None,
            retained_browser: HashSet::new(),
            audio_preference: PlaybackPreference::default(),
            video_preference: PlaybackPreference::default(),
        }
    }
    pub fn set_preference(
        &mut self,
        kind: PlayableKind,
        preference: PlaybackPreference,
        cx: &mut App,
    ) {
        if !preference.volume.is_finite() || !(0.0..=1.0).contains(&preference.volume) {
            return;
        }
        match kind {
            PlayableKind::Audio => self.audio_preference = preference,
            PlayableKind::Video => self.video_preference = preference,
            PlayableKind::Gifv => return,
        }
        for player in self.players.values() {
            player.update(cx, |player, cx| {
                if player.source.kind == kind {
                    player.volume = preference.volume;
                    player.muted = preference.muted;
                    if let Some(native) = &player.player {
                        native.set_volume(preference.volume);
                        native.set_muted(preference.muted);
                    }
                    cx.notify();
                }
            });
        }
    }
    pub fn reset_preferences(&mut self, cx: &mut App) {
        self.set_preference(PlayableKind::Audio, PlaybackPreference::default(), cx);
        self.set_preference(PlayableKind::Video, PlaybackPreference::default(), cx);
    }
    pub fn begin_frame(
        &mut self,
        output: Option<String>,
        retained_browser: HashSet<String>,
        cx: &mut App,
    ) {
        self.seen.clear();
        self.retained_browser = retained_browser;
        if output != self.output {
            self.output = output;
            for player in self.players.values() {
                player.update(cx, |player, _| {
                    player.output = self.output.clone();
                    if let Some(native) = &player.player {
                        native.set_output_device(player.output.clone());
                    }
                });
            }
        }
    }
    pub fn reset(&mut self, cx: &mut App) {
        for (_, player) in std::mem::take(&mut self.players) {
            player.update(cx, |player, cx| player.retire(cx));
        }
        self.seen.clear();
        self.contexts.clear();
        self.modal.reset();
    }
    pub fn finish_frame(&mut self, cx: &mut App) {
        // List processors run during prepaint, after the parent's render pass.
        // Preserve active result resources even before their row is rebuilt.
        self.seen.extend(self.retained_browser.iter().cloned());
        if let Some(id) = self.modal.id() {
            self.seen.insert(id.to_owned());
        }
        let obsolete = self
            .players
            .keys()
            .filter(|id| !self.seen.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in obsolete {
            self.contexts.remove(&id);
            if let Some(player) = self.players.remove(&id) {
                player.update(cx, |player, cx| player.retire(cx));
            }
        }
    }
    pub fn set_context(&mut self, key: String, reference: crate::media_actions::MediaReference) {
        self.contexts.insert(key, reference);
    }
    pub fn cached_file(&self, url: &str, cx: &App) -> Option<Arc<tempfile::NamedTempFile>> {
        self.players.values().find_map(|player| {
            let player = player.read(cx);
            (player.source.source == url)
                .then(|| player.file.clone())
                .flatten()
        })
    }
    pub fn render(
        &mut self,
        source: PlayableMedia,
        revealed: bool,
        autoplay: bool,
        enabled: bool,
        window: &Window,
        cx: &mut Context<Fluxer>,
    ) -> AnyElement {
        let id = source.resource_key.clone();
        let preference = match source.kind {
            PlayableKind::Audio => self.audio_preference,
            PlayableKind::Video => self.video_preference,
            PlayableKind::Gifv => PlaybackPreference::default(),
        };
        self.seen.insert(id.clone());
        let entity =
            self.players.entry(id.clone()).or_insert_with(|| {
                let entity = cx.new(|cx| {
                    MediaPlayer::new(source.clone(), self.output.clone(), preference, cx)
                });
                cx.subscribe_in(
                    &entity,
                    window,
                    |this, player, event: &PlayerEvent, window, cx| {
                        match event {
                            PlayerEvent::Open => {
                                this.emoji_target = None;
                                this.dismiss_message_actions(window, cx);
                                this.playback.modal.open(
                                    player.read(cx).source.resource_key.clone(),
                                    window,
                                    cx,
                                );
                            }
                            PlayerEvent::Close => this.playback.modal.close(window, cx),
                            PlayerEvent::Original => {
                                let key = player.read(cx).source.resource_key.clone();
                                if this.playback.contexts.get(&key).is_some_and(|reference| {
                                    !this.media_original_allowed(reference)
                                }) {
                                    return;
                                }
                                let original = player.read(cx).source.original.clone();
                                if let Some(url) = original {
                                    this.activate_message_target(
                                        &message_markup::MessageTarget::Link(url),
                                        window,
                                        cx,
                                    );
                                }
                            }
                            PlayerEvent::Demo(action) => {
                                player.update(cx, |player, cx| player.action(*action, window, cx));
                            }
                            PlayerEvent::Preferences(kind, preference) => {
                                this.playback.set_preference(*kind, *preference, cx);
                                let kind = match kind {
                                    PlayableKind::Audio => fluxer_client::FilePlaybackKind::Audio,
                                    PlayableKind::Video => fluxer_client::FilePlaybackKind::Video,
                                    PlayableKind::Gifv => return,
                                };
                                this.command(
                                    Command::SetFilePlaybackPreference {
                                        kind,
                                        preference: fluxer_client::FilePlaybackPreference {
                                            volume: preference.volume,
                                            muted: preference.muted,
                                        },
                                    },
                                    cx,
                                );
                            }
                            PlayerEvent::Reveal => {
                                this.revealed_media
                                    .insert(player.read(cx).source.reveal_key.clone());
                            }
                        }
                        cx.notify();
                    },
                )
                .detach();
                entity
            });
        entity.update(cx, |player, cx| {
            if player.source.original != source.original {
                player.source.original = source.original.clone();
                cx.notify();
            }
            let active = window.is_window_active();
            if player.revealed != revealed
                || player.autoplay != autoplay
                || player.enabled != enabled
                || player.active != active
            {
                player.revealed = revealed;
                player.autoplay = autoplay;
                player.enabled = enabled;
                player.active = active;
                if active {
                    player.awaiting_prepaint = true;
                    // Allow the reactivated window its first prepaint before
                    // judging visibility from an old background timestamp.
                    player.last_prepaint = std::time::Instant::now();
                    player.hidden_hold = false;
                    if player.wanted {
                        player.ensure_loaded();
                    }
                }
                player.apply_policy();
                cx.notify();
            }
        });
        entity.clone().into_any_element()
    }
    pub fn overlay(&self, window: &Window, cx: &mut Context<Fluxer>) -> AnyElement {
        let Some(player) = self.modal.id().and_then(|id| self.players.get(id)) else {
            return div().into_any_element();
        };
        let view = player.read(cx).view();
        let content = media_player_view::render_modal(
            &view,
            &self.modal,
            MediaPlayer::callback(player.downgrade()),
            window,
            cx,
        );
        if let Some(reference) = self
            .modal
            .id()
            .and_then(|id| self.contexts.get(id))
            .cloned()
        {
            crate::context_actions::trigger(
                div().id("native-player-context"),
                crate::context_actions::Target::Media(reference),
                cx,
            )
            .absolute()
            .inset_0()
            .child(content)
            .into_any_element()
        } else {
            content
        }
    }
    pub fn close(&mut self, window: &mut Window, cx: &mut App) {
        if let Some(player) = self.modal.id().and_then(|id| self.players.get(id)) {
            player.update(cx, |player, cx| {
                player.modal = false;
                player.apply_policy();
                cx.notify();
            });
        }
        self.modal.close(window, cx);
    }
}
#[cfg(test)]
#[path = "../../../compat/gpui/src/platform/blade/atlas_lifetime.rs"]
mod atlas_lifetime_regressions;
