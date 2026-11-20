// SPDX-License-Identifier: AGPL-3.0-or-later
//! Opt-in metadata diagnostics and a synthetic 1080p30 UI input source.
use gpui::Window;
use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct VideoStats {
    enabled: bool,
    inner: Arc<Mutex<Stats>>,
}
struct Stats {
    started: Instant,
    last_log: Instant,
    accepted: u64,
    presented: u64,
    last_accepted: u64,
    last_presented: u64,
    last_presented_serial: u64,
    poll_max: Duration,
    render_build_max: Duration,
    images: BTreeSet<usize>,
    stop_deadline: Option<Instant>,
    stop_handled: Option<Instant>,
    stop_presented: bool,
    stop_render_reported: bool,
}
impl VideoStats {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            enabled: std::env::var_os("FLUXER_VIDEO_PERF").is_some(),
            inner: Arc::new(Mutex::new(Stats {
                started: now,
                last_log: now,
                accepted: 0,
                presented: 0,
                last_accepted: 0,
                last_presented: 0,
                last_presented_serial: 0,
                poll_max: Duration::ZERO,
                render_build_max: Duration::ZERO,
                images: BTreeSet::new(),
                stop_deadline: None,
                stop_handled: None,
                stop_presented: false,
                stop_render_reported: false,
            })),
        }
    }
    pub fn accepted(&self) {
        if self.enabled {
            self.inner.lock().unwrap().accepted += 1;
        }
    }
    pub fn image_added(&self, id: usize) {
        if self.enabled {
            self.inner.lock().unwrap().images.insert(id);
        }
    }
    pub fn image_evicted(&self, id: usize) {
        if self.enabled {
            self.inner.lock().unwrap().images.remove(&id);
        }
    }
    pub fn poll(&self, elapsed: Duration, slots: usize) {
        if !self.enabled {
            return;
        }
        let mut stats = self.inner.lock().unwrap();
        stats.poll_max = stats.poll_max.max(elapsed);
        let period = stats.last_log.elapsed().as_secs_f64();
        if period >= 1.0 {
            eprintln!(
                "video_perf elapsed_s={:.2} accepted_fps={:.2} rendered_fps={:.2} poll_max_ms={:.3} render_build_max_ms={:.3} tracked_images={} frame_slots={slots}",
                stats.started.elapsed().as_secs_f64(),
                (stats.accepted - stats.last_accepted) as f64 / period,
                (stats.presented - stats.last_presented) as f64 / period,
                stats.poll_max.as_secs_f64() * 1000.,
                stats.render_build_max.as_secs_f64() * 1000.,
                stats.images.len()
            );
            stats.last_accepted = stats.accepted;
            stats.last_presented = stats.presented;
            stats.last_log = Instant::now();
            stats.poll_max = Duration::ZERO;
            stats.render_build_max = Duration::ZERO;
        }
    }
    pub fn render_built(&self, elapsed: Duration) {
        if self.enabled {
            let mut stats = self.inner.lock().unwrap();
            stats.render_build_max = stats.render_build_max.max(elapsed);
        }
    }
    pub fn rendered(&self, window: &Window, video_visible: bool) {
        if !self.enabled {
            return;
        }
        let (serial, cleared_stop) = {
            let mut stats = self.inner.lock().unwrap();
            let cleared = if video_visible {
                None
            } else {
                stats.stop_handled
            };
            if let Some(handled) = cleared
                && !stats.stop_render_reported
            {
                eprintln!(
                    "video_perf stop_render_received_ms={:.3}",
                    handled.elapsed().as_secs_f64() * 1000.
                );
                stats.stop_render_reported = true;
            }
            (stats.accepted, cleared)
        };
        let stats = self.inner.clone();
        // In GPUI 0.2.2 this runs at the beginning of the next platform frame,
        // after the previous scene reached Window::present. It counts rendered
        // scenes, not physical display scanout or GPU completion.
        window.on_next_frame(move |_, _| {
            let mut stats = stats.lock().unwrap();
            if video_visible && serial > stats.last_presented_serial {
                stats.presented += 1;
                stats.last_presented_serial = serial;
            }
            // A callback scheduled by a pre-stop scene cannot acknowledge a
            // later stop. Capture the stop epoch only on a cleared scene.
            if let Some(handled) = cleared_stop
                && stats.stop_handled == Some(handled)
                && !stats.stop_presented
            {
                eprintln!(
                    "video_perf stop_next_frame_callback_ms={:.3}",
                    handled.elapsed().as_secs_f64() * 1000.
                );
                stats.stop_presented = true;
            }
        });
    }
    pub fn request_stop(&self, deadline: Instant) {
        if self.enabled {
            self.inner.lock().unwrap().stop_deadline = Some(deadline);
        }
    }
    pub fn stopped(&self) {
        if !self.enabled {
            return;
        }
        let mut stats = self.inner.lock().unwrap();
        let now = Instant::now();
        let delay = stats
            .stop_deadline
            .map(|deadline| now.saturating_duration_since(deadline).as_secs_f64() * 1000.);
        eprintln!("video_perf stop_action_delay_ms={delay:?}");
        stats.stop_handled = Some(now);
        stats.stop_presented = false;
        stats.stop_render_reported = false;
    }
}

pub struct AnimatedFixture {
    latest: Arc<Mutex<Option<fluxer_media::VideoFrame>>>,
    stop: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    generation: Arc<AtomicU64>,
    thread: std::thread::Thread,
}
impl AnimatedFixture {
    pub fn new() -> Self {
        let latest = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicBool::new(true));
        let generation = Arc::new(AtomicU64::new(1));
        let (output, stopped, running, epoch) = (
            latest.clone(),
            stop.clone(),
            active.clone(),
            generation.clone(),
        );
        let worker = std::thread::spawn(move || {
            const WIDTH: usize = 1920;
            const HEIGHT: usize = 1080;
            let base = [35u8, 27, 20, 255].repeat(WIDTH * HEIGHT);
            let stripe = [140u8, 90, 210, 255].repeat(96);
            let mut sequence = 0usize;
            while !stopped.load(Ordering::Acquire) {
                let tick = Instant::now();
                if running.load(Ordering::Acquire) {
                    let mut bgra = base.clone();
                    let x = (sequence * 17) % (WIDTH - 96);
                    for row in 0..HEIGHT {
                        let start = (row * WIDTH + x) * 4;
                        bgra[start..start + stripe.len()].copy_from_slice(&stripe);
                    }
                    *output.lock().unwrap() = Some(fluxer_media::VideoFrame {
                        source: fluxer_media::VideoSource::ScreenShare,
                        track_id: "animated-preview".into(),
                        participant: "preview-0".into(),
                        watch_generation: epoch.load(Ordering::Acquire),
                        width: WIDTH as u32,
                        height: HEIGHT as u32,
                        bgra: Arc::new(bgra),
                    });
                    sequence += 1;
                }
                std::thread::park_timeout(
                    Duration::from_secs_f64(1. / 30.).saturating_sub(tick.elapsed()),
                );
            }
        });
        Self {
            latest,
            stop,
            active,
            generation,
            thread: worker.thread().clone(),
        }
    }
    pub fn take_frame(&self) -> Option<fluxer_media::VideoFrame> {
        self.latest.lock().unwrap().take()
    }
    pub fn set_active(&self, active: bool, generation: u64) {
        self.active.store(active, Ordering::Release);
        self.generation.store(generation, Ordering::Release);
        if !active {
            self.latest.lock().unwrap().take();
        }
    }
}
impl Drop for AnimatedFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.unpark();
    }
}
