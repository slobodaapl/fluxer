// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local media playback with asynchronous decoding and teardown.
use std::{
    io::{Read, Seek},
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    sync::{Arc, Mutex, mpsc},
};

#[derive(Clone, Debug)]
pub struct FilePlaybackOptions {
    pub autoplay: bool,
    pub looping: bool,
    pub muted: bool,
    pub volume: f32,
    pub output_device: Option<String>,
}
impl Default for FilePlaybackOptions {
    fn default() -> Self {
        Self {
            autoplay: false,
            looping: false,
            muted: false,
            volume: 1.0,
            output_device: None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilePlaybackStatus {
    Loading,
    Paused,
    Playing,
    Ended,
    Failed,
}
#[derive(Clone, Debug)]
pub struct FilePlaybackSnapshot {
    pub status: FilePlaybackStatus,
    pub position: f64,
    pub duration: Option<f64>,
    pub width: u32,
    pub height: u32,
    pub error: Option<String>,
}
#[derive(Clone, Debug)]
pub struct FilePlaybackFrame {
    pub width: u32,
    pub height: u32,
    pub bgra: Arc<Vec<u8>>,
    pub revision: u64,
}
struct Shared {
    snapshot: FilePlaybackSnapshot,
    frame: Option<FilePlaybackFrame>,
    seek_generation: u64,
    pending_seek: bool,
}
#[cfg_attr(not(ffmpeg_file_playback), allow(dead_code))]
enum Command {
    Play,
    Pause,
    Seek(f64, u64),
    Volume(f32),
    Muted(bool),
    Output(Option<String>),
    Stop,
}
pub trait PlaybackSource: Read + Seek + Send {}
impl<T: Read + Seek + Send> PlaybackSource for T {}
#[cfg_attr(not(ffmpeg_file_playback), allow(dead_code))]
enum Source {
    Path(PathBuf),
    Reader(Box<dyn PlaybackSource>),
}
struct PlaybackGate {
    wanted: AtomicBool,
    seeking: AtomicBool,
    muted: AtomicBool,
    volume: AtomicU32,
    waiting: Arc<AtomicBool>,
    underflow: AtomicBool,
    video_only: AtomicBool,
    video_deadline: Mutex<Option<std::time::Instant>>,
}
pub struct FilePlayer {
    gate: Arc<PlaybackGate>,
    cancel: Arc<AtomicBool>,
    interrupt: Arc<AtomicU64>,
    ready: Arc<AtomicBool>,
    tx: mpsc::Sender<Command>,
    shared: Arc<Mutex<Shared>>,
}
impl FilePlayer {
    pub fn open(path: PathBuf, options: FilePlaybackOptions) -> Result<Self, String> {
        Self::open_impl(path, options, false)
    }
    fn open_impl(path: PathBuf, options: FilePlaybackOptions, fake: bool) -> Result<Self, String> {
        let path =
            std::fs::canonicalize(path).map_err(|e| format!("Cannot open media file: {e}"))?;
        Self::open_source_impl(
            Source::Path(path),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicBool::new(false)),
            options,
            fake,
        )
    }
    pub fn open_reader(
        source: Box<dyn PlaybackSource>,
        cancel: Arc<AtomicBool>,
        interrupt: Arc<AtomicU64>,
        waiting: Arc<AtomicBool>,
        options: FilePlaybackOptions,
    ) -> Result<Self, String> {
        Self::open_source_impl(
            Source::Reader(source),
            cancel,
            interrupt,
            waiting,
            options,
            false,
        )
    }
    fn open_source_impl(
        source: Source,
        cancel: Arc<AtomicBool>,
        interrupt: Arc<AtomicU64>,
        waiting: Arc<AtomicBool>,
        options: FilePlaybackOptions,
        fake: bool,
    ) -> Result<Self, String> {
        #[cfg(not(ffmpeg_file_playback))]
        {
            let _ = (source, cancel, interrupt, waiting, options, fake);
            Err(if cfg!(target_os = "linux") {
                "Native file playback requires FFmpeg"
            } else {
                "Native file playback is unavailable on this platform"
            }
            .into())
        }
        #[cfg(ffmpeg_file_playback)]
        {
            if !options.volume.is_finite() {
                return Err("Invalid volume".into());
            }
            let shared = Arc::new(Mutex::new(Shared {
                snapshot: FilePlaybackSnapshot {
                    status: FilePlaybackStatus::Loading,
                    position: 0.0,
                    duration: None,
                    width: 0,
                    height: 0,
                    error: None,
                },
                frame: None,
                seek_generation: 0,
                pending_seek: false,
            }));
            let (tx, rx) = mpsc::channel();
            let worker_shared = shared.clone();
            let gate = Arc::new(PlaybackGate {
                wanted: AtomicBool::new(options.autoplay),
                seeking: AtomicBool::new(false),
                muted: AtomicBool::new(options.muted),
                volume: AtomicU32::new(options.volume.clamp(0.0, 1.0).to_bits()),
                waiting,
                underflow: AtomicBool::new(false),
                video_only: AtomicBool::new(false),
                video_deadline: Mutex::new(None),
            });
            let worker_gate = gate.clone();
            let ready = Arc::new(AtomicBool::new(false));
            let worker_ready = ready.clone();
            std::thread::Builder::new()
                .name("file-playback".into())
                .spawn(move || {
                    native::run(
                        source,
                        options,
                        fake,
                        rx,
                        worker_shared,
                        worker_ready,
                        worker_gate,
                    )
                })
                .map_err(|e| e.to_string())?;
            Ok(Self {
                tx,
                shared,
                gate,
                cancel,
                interrupt,
                ready,
            })
        }
    }
    pub fn play(&self) {
        self.gate.wanted.store(true, Ordering::Release);
        let _ = self.tx.send(Command::Play);
    }
    pub fn pause(&self) {
        self.gate.wanted.store(false, Ordering::Release);
        let _ = self.tx.send(Command::Pause);
    }
    pub fn seek(&self, seconds: f64) {
        if seconds.is_finite() && seconds >= 0.0 {
            let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            shared.frame = None;
            shared.pending_seek = true;
            self.gate.seeking.store(true, Ordering::Release);
            if self.ready.load(Ordering::Acquire) {
                self.interrupt.fetch_add(1, Ordering::AcqRel);
            }
            shared.snapshot.position = seconds.min(i64::MAX as f64 / 1e6);
            shared.snapshot.status = FilePlaybackStatus::Loading;
            shared.seek_generation = shared.seek_generation.wrapping_add(1);
            let _ = self.tx.send(Command::Seek(
                seconds.min(i64::MAX as f64 / 1e6),
                shared.seek_generation,
            ));
        }
    }
    pub fn set_volume(&self, volume: f32) {
        if volume.is_finite() {
            self.gate
                .volume
                .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Release);
            let _ = self.tx.send(Command::Volume(volume.clamp(0.0, 1.0)));
        }
    }
    pub fn set_muted(&self, muted: bool) {
        self.gate.muted.store(muted, Ordering::Release);
        let _ = self.tx.send(Command::Muted(muted));
    }
    pub fn set_output_device(&self, id: Option<String>) {
        let _ = self.tx.send(Command::Output(id));
    }
    pub fn stop(&self) {
        self.gate.wanted.store(false, Ordering::Release);
        self.cancel.store(true, Ordering::Release);
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        shared.frame = None;
        shared.pending_seek = true;
        shared.seek_generation = shared.seek_generation.wrapping_add(1);
        let _ = self.tx.send(Command::Stop);
    }
    pub fn snapshot(&self) -> FilePlaybackSnapshot {
        let mut snapshot = self
            .shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot
            .clone();
        let video_stalled = self.gate.video_only.load(Ordering::Acquire)
            && self
                .gate
                .video_deadline
                .try_lock()
                .ok()
                .and_then(|deadline| *deadline)
                .is_some_and(|deadline| std::time::Instant::now() >= deadline);
        if snapshot.status == FilePlaybackStatus::Playing
            && self.gate.waiting.load(Ordering::Acquire)
            && (self.gate.underflow.load(Ordering::Acquire) || video_stalled)
        {
            snapshot.status = FilePlaybackStatus::Loading;
        }
        snapshot
    }
    pub fn take_frame(&self) -> Option<FilePlaybackFrame> {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .frame
            .take()
    }
}
impl Drop for FilePlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(ffmpeg_file_playback)]
mod native {
    use super::*;
    use std::{
        collections::VecDeque,
        ffi::{CStr, CString, c_char, c_int, c_void},
        os::unix::ffi::OsStrExt,
        ptr,
        time::{Duration, Instant},
    };
    #[repr(C)]
    struct Decoded {
        kind: c_int,
        width: c_int,
        height: c_int,
        pts: f64,
        duration: f64,
        data: *mut u8,
        len: usize,
    }
    unsafe extern "C" {
        fn flux_decoder_open(path: *const c_char, error: *mut c_char, len: usize) -> *mut c_void;
        fn flux_decoder_open_io(
            opaque: *mut c_void,
            read: unsafe extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int,
            seek: unsafe extern "C" fn(*mut c_void, i64, c_int) -> i64,
            error: *mut c_char,
            len: usize,
        ) -> *mut c_void;
        fn flux_decoder_free(p: *mut c_void);
        fn flux_decoder_has_audio(p: *mut c_void) -> c_int;
        fn flux_decoder_duration(p: *mut c_void) -> f64;
        fn flux_decoder_seek(p: *mut c_void, seconds: f64, error: *mut c_char, len: usize)
        -> c_int;
        fn flux_decoder_next(
            p: *mut c_void,
            out: *mut Decoded,
            error: *mut c_char,
            len: usize,
        ) -> c_int;
        fn flux_decoder_data_free(p: *mut c_void);
    }
    struct ReaderState {
        source: Box<dyn PlaybackSource>,
        error: Option<String>,
    }
    unsafe extern "C" fn read_source(opaque: *mut c_void, buffer: *mut u8, size: c_int) -> c_int {
        if size <= 0 {
            return -5;
        }
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let state = unsafe { &mut *(opaque as *mut ReaderState) };
            let output = unsafe { std::slice::from_raw_parts_mut(buffer, size as usize) };
            match state.source.read(output) {
                Ok(0) => -541478725,
                Ok(n) => {
                    state.error = None;
                    n as c_int
                }
                Err(error) => {
                    state.error = Some(error.to_string());
                    -5
                }
            }
        }))
        .unwrap_or(-5)
    }
    unsafe extern "C" fn seek_source(opaque: *mut c_void, offset: i64, whence: c_int) -> i64 {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            use std::io::SeekFrom;
            let source = &mut unsafe { &mut *(opaque as *mut ReaderState) }.source;
            if whence & 0x10000 != 0 {
                let pos = source.stream_position()?;
                let end = source.seek(SeekFrom::End(0))?;
                source.seek(SeekFrom::Start(pos))?;
                return i64::try_from(end).map_err(|_| std::io::Error::other("source too large"));
            }
            let seek = match whence & !0x20000 {
                0 if offset >= 0 => SeekFrom::Start(offset as u64),
                1 => SeekFrom::Current(offset),
                2 => SeekFrom::End(offset),
                _ => return Err(std::io::Error::other("invalid seek")),
            };
            i64::try_from(source.seek(seek)?).map_err(|_| std::io::Error::other("source too large"))
        }))
        .ok()
        .and_then(Result::ok)
        .unwrap_or(-1)
    }
    struct Decoder(*mut c_void);
    impl Drop for Decoder {
        fn drop(&mut self) {
            unsafe { flux_decoder_free(self.0) }
        }
    }
    struct AudioChunk {
        pts: f64,
        samples: Vec<f32>,
        offset: usize,
    }
    struct Audio {
        chunks: VecDeque<AudioChunk>,
        samples: usize,
        position: f64,
        playing: bool,
        started: bool,
        volume: f32,
        muted: bool,
        error: Option<String>,
    }
    impl Audio {
        fn render(&mut self, output: &mut [f32], freeze_underflow: bool) {
            output.fill(0.0);
            if !self.playing {
                return;
            }
            let gain = if self.muted { 0.0 } else { self.volume };
            let mut done = 0;
            while done < output.len() {
                let Some(chunk) = self.chunks.front_mut() else {
                    if self.started && !freeze_underflow {
                        self.position += (output.len() - done) as f64 / 96000.0;
                    }
                    break;
                };
                // Discard decoded samples that arrived after an output underrun.
                let chunk_position = chunk.pts + chunk.offset as f64 / 96000.0;
                let skip = (((self.position - chunk_position).max(0.0) * 48000.0) as usize * 2)
                    .min(chunk.samples.len() - chunk.offset);
                chunk.offset += skip;
                self.samples -= skip;
                if chunk.offset == chunk.samples.len() {
                    self.chunks.pop_front();
                    continue;
                }
                // Preserve timestamp gaps, including delayed audio tracks.
                let gap_frames = ((chunk.pts - self.position).max(0.0) * 48000.0).round() as usize;
                if gap_frames > 0 {
                    let silence = (gap_frames * 2).min(output.len() - done) & !1;
                    if silence > 0 {
                        done += silence;
                        self.position += silence as f64 / 96000.0;
                        continue;
                    }
                }
                let n = (output.len() - done).min(chunk.samples.len() - chunk.offset);
                for (dst, src) in output[done..done + n]
                    .iter_mut()
                    .zip(&chunk.samples[chunk.offset..chunk.offset + n])
                {
                    *dst = *src * gain;
                }
                chunk.offset += n;
                done += n;
                self.samples -= n;
                self.position = chunk.pts + chunk.offset as f64 / 96000.0;
                if chunk.offset == chunk.samples.len() {
                    self.chunks.pop_front();
                }
            }
        }
    }
    fn failed(shared: &Mutex<Shared>, error: String) {
        let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
        s.snapshot.status = FilePlaybackStatus::Failed;
        s.snapshot.error = Some(error);
    }
    fn error_text(error: &[c_char]) -> String {
        unsafe { CStr::from_ptr(error.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }
    fn render_gated(audio: &Mutex<Audio>, gate: &PlaybackGate, output: &mut [f32]) {
        output.fill(0.0);
        if !gate.wanted.load(Ordering::Acquire) || gate.seeking.load(Ordering::Acquire) {
            return;
        }
        if let Ok(mut audio) = audio.try_lock()
            && gate.wanted.load(Ordering::Acquire)
            && !gate.seeking.load(Ordering::Acquire)
        {
            audio.muted = gate.muted.load(Ordering::Acquire);
            audio.volume = f32::from_bits(gate.volume.load(Ordering::Acquire));
            let waiting = gate.waiting.load(Ordering::Acquire);
            gate.underflow
                .store(waiting && audio.samples == 0, Ordering::Release);
            audio.render(output, waiting);
        }
    }
    fn start_output(
        id: Option<&str>,
        audio: Arc<Mutex<Audio>>,
        gate: Arc<PlaybackGate>,
    ) -> Result<fluxer_system_audio::SpeakerOutput, String> {
        let errors = audio.clone();
        fluxer_system_audio::SpeakerOutput::start(
            id,
            move |out| render_gated(&audio, &gate, out),
            move |error| {
                if let Ok(mut a) = errors.lock() {
                    a.error = Some(error);
                }
            },
        )
        .map_err(|e| e.to_string())
    }
    #[cfg(test)]
    pub(super) fn audio_peak(path: &std::path::Path) -> f32 {
        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        let mut error = [0 as c_char; 512];
        let p = unsafe { flux_decoder_open(path.as_ptr(), error.as_mut_ptr(), error.len()) };
        assert!(!p.is_null(), "{}", error_text(&error));
        let _owner = Decoder(p);
        for _ in 0..1000 {
            let mut frame = Decoded {
                kind: 0,
                width: 0,
                height: 0,
                pts: -1.0,
                duration: 0.0,
                data: ptr::null_mut(),
                len: 0,
            };
            let result =
                unsafe { flux_decoder_next(p, &mut frame, error.as_mut_ptr(), error.len()) };
            assert!(result >= 0, "{}", error_text(&error));
            if result == 0 {
                break;
            }
            let peak = if frame.kind == 2 {
                unsafe { std::slice::from_raw_parts(frame.data.cast::<f32>(), frame.len / 4) }
                    .iter()
                    .fold(0.0_f32, |peak, v| peak.max(v.abs()))
            } else {
                0.0
            };
            unsafe {
                flux_decoder_data_free(frame.data.cast());
            }
            if peak > 0.0 {
                return peak;
            }
        }
        0.0
    }
    pub(super) fn run(
        source: Source,
        mut options: FilePlaybackOptions,
        fake: bool,
        rx: mpsc::Receiver<Command>,
        shared: Arc<Mutex<Shared>>,
        ready: Arc<AtomicBool>,
        gate: Arc<PlaybackGate>,
    ) {
        let mut error = [0 as c_char; 512];
        let (mut reader, path) = match source {
            Source::Reader(reader) => (
                Some(ReaderState {
                    source: reader,
                    error: None,
                }),
                None,
            ),
            Source::Path(path) => (None, Some(path)),
        };
        let p = if let Some(reader) = reader.as_mut() {
            unsafe {
                flux_decoder_open_io(
                    (reader as *mut ReaderState).cast(),
                    read_source,
                    seek_source,
                    error.as_mut_ptr(),
                    error.len(),
                )
            }
        } else if let Some(path) = path {
            let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
                failed(&shared, "Invalid file path".into());
                return;
            };
            unsafe { flux_decoder_open(path.as_ptr(), error.as_mut_ptr(), error.len()) }
        } else {
            unreachable!()
        };
        if p.is_null() {
            failed(
                &shared,
                reader
                    .as_ref()
                    .and_then(|reader| reader.error.clone())
                    .unwrap_or_else(|| error_text(&error)),
            );
            return;
        }
        let decoder = Decoder(p);
        ready.store(true, Ordering::Release);
        let has_audio = unsafe { flux_decoder_has_audio(p) != 0 };
        gate.video_only.store(!has_audio, Ordering::Release);
        let duration = unsafe { flux_decoder_duration(p) };
        let mut playing = options.autoplay;
        options.volume = options.volume.clamp(0.0, 1.0);
        let audio = Arc::new(Mutex::new(Audio {
            chunks: VecDeque::new(),
            samples: 0,
            position: 0.0,
            playing,
            started: false,
            volume: options.volume,
            muted: options.muted,
            error: None,
        }));
        let mut output = if has_audio && !fake {
            if let Err(error) = crate::pipewire_runtime::ensure() {
                failed(&shared, error.to_string());
                return;
            }
            match start_output(
                options.output_device.as_deref(),
                audio.clone(),
                gate.clone(),
            ) {
                Ok(v) => Some(v),
                Err(e) => {
                    failed(&shared, e);
                    return;
                }
            }
        } else {
            None
        };
        let mut video = VecDeque::<(f64, FilePlaybackFrame)>::new();
        let mut revision = 0;
        let mut preroll_shown = false;
        let mut seek_fallback: Option<FilePlaybackFrame> = None;
        let mut eof = false;
        let mut ended = false;
        let mut initialized = false;
        let mut seek_target = 0.0;
        let mut clock_base = 0.0;
        let mut clock_time = Instant::now();
        let mut fake_time = Instant::now();
        let mut audio_tail = 0.0;
        let mut video_end = 0.0;
        let mut previous_video_pts: Option<f64> = None;

        loop {
            match rx.recv_timeout(Duration::from_millis(2)) {
                Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Ok(Command::Play) => {
                    if ended {
                        if unsafe { flux_decoder_seek(p, 0.0, error.as_mut_ptr(), error.len()) }
                            == 0
                        {
                            failed(&shared, error_text(&error));
                            break;
                        }
                        video.clear();
                        preroll_shown = false;
                        seek_fallback = None;
                        eof = false;
                        ended = false;
                        seek_target = 0.0;
                        clock_base = 0.0;
                        audio_tail = 0.0;
                        video_end = 0.0;
                        previous_video_pts = None;
                        let mut a = audio.lock().unwrap_or_else(|e| e.into_inner());
                        a.chunks.clear();
                        a.samples = 0;
                        a.position = 0.0;
                        a.started = false;
                    }
                    clock_time = Instant::now();
                    fake_time = Instant::now();
                    playing = true;
                    audio.lock().unwrap_or_else(|e| e.into_inner()).playing = true;
                }
                Ok(Command::Pause) => {
                    if !has_audio && playing {
                        clock_base += clock_time.elapsed().as_secs_f64();
                    }
                    playing = false;
                    audio.lock().unwrap_or_else(|e| e.into_inner()).playing = false;
                }
                Ok(Command::Seek(pos, generation)) => {
                    if shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .seek_generation
                        != generation
                    {
                        continue;
                    }
                    if unsafe { flux_decoder_seek(p, pos, error.as_mut_ptr(), error.len()) } == 0 {
                        failed(&shared, error_text(&error));
                        break;
                    }
                    video.clear();
                    preroll_shown = false;
                    seek_fallback = None;
                    eof = false;
                    ended = false;
                    seek_target = pos;
                    clock_base = pos;
                    audio_tail = pos;
                    video_end = pos;
                    previous_video_pts = None;
                    clock_time = Instant::now();
                    fake_time = Instant::now();
                    initialized = false;
                    {
                        let mut a = audio.lock().unwrap_or_else(|e| e.into_inner());
                        a.chunks.clear();
                        a.samples = 0;
                        a.position = pos;
                        a.started = false;
                    }
                    let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                    s.snapshot.position = pos;
                    if s.seek_generation == generation {
                        s.pending_seek = false;
                        gate.seeking.store(false, Ordering::Release);
                    }
                }
                Ok(Command::Volume(v)) => {
                    audio.lock().unwrap_or_else(|e| e.into_inner()).volume = v
                }
                Ok(Command::Muted(v)) => audio.lock().unwrap_or_else(|e| e.into_inner()).muted = v,
                Ok(Command::Output(id)) => {
                    if has_audio && !fake {
                        output.take();
                        match start_output(id.as_deref(), audio.clone(), gate.clone()) {
                            Ok(v) => output = Some(v),
                            Err(e) => {
                                failed(&shared, e);
                                break;
                            }
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
            }
            if fake && has_audio && playing {
                let elapsed = fake_time.elapsed().as_secs_f64();
                if elapsed >= 0.005 {
                    let count = ((elapsed * 48000.0) as usize).min(4800) * 2;
                    let mut samples = vec![0.0; count];
                    gate.underflow.store(
                        gate.waiting.load(Ordering::Acquire)
                            && audio.lock().unwrap_or_else(|e| e.into_inner()).samples == 0,
                        Ordering::Release,
                    );
                    audio
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .render(&mut samples, gate.waiting.load(Ordering::Acquire));
                    fake_time = Instant::now();
                }
            }
            let (position, audio_samples, audio_error) = {
                let a = audio.lock().unwrap_or_else(|e| e.into_inner());
                (a.position, a.samples, a.error.clone())
            };
            if let Some(error) = audio_error {
                failed(&shared, error);
                break;
            }
            let position = if has_audio {
                position
            } else if playing {
                clock_base + clock_time.elapsed().as_secs_f64()
            } else {
                clock_base
            };
            let mut latest = None;
            while video
                .front()
                .is_some_and(|(pts, _)| *pts <= position + 0.015 || (!preroll_shown && !playing))
            {
                latest = video.pop_front().map(|(_, frame)| frame);
                initialized = true;
                preroll_shown = true;
            }
            if let Some(frame) = latest {
                let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                if !s.pending_seek {
                    s.snapshot.width = frame.width;
                    s.snapshot.height = frame.height;
                    s.frame = Some(frame);
                }
            }
            if eof
                && audio_samples == 0
                && video.is_empty()
                && playing
                && position + 0.005
                    >= if duration >= 0.0 {
                        duration.max(video_end)
                    } else {
                        video_end
                    }
            {
                if options.looping {
                    if unsafe { flux_decoder_seek(p, 0.0, error.as_mut_ptr(), error.len()) } == 0 {
                        failed(&shared, error_text(&error));
                        break;
                    }
                    eof = false;
                    // A loop is not a new load. Retain the last decoded frame
                    // and readiness while prerolling the next iteration; short
                    // loops otherwise keep the UI sampling Loading forever.
                    preroll_shown = false;
                    seek_fallback = None;
                    seek_target = 0.0;
                    clock_base = 0.0;
                    audio_tail = 0.0;
                    video_end = 0.0;
                    previous_video_pts = None;
                    clock_time = Instant::now();
                    let mut a = audio.lock().unwrap_or_else(|e| e.into_inner());
                    a.position = 0.0;
                    a.started = false;
                    fake_time = Instant::now();
                } else {
                    ended = true;
                    playing = false;
                    audio.lock().unwrap_or_else(|e| e.into_inner()).playing = false;
                }
            }
            {
                let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                if !s.pending_seek {
                    s.snapshot.position = if ended {
                        duration.max(position)
                    } else {
                        position
                    };
                }
                s.snapshot.duration = (duration >= 0.0).then_some(duration);
                s.snapshot.status = if ended {
                    FilePlaybackStatus::Ended
                } else if !initialized {
                    FilePlaybackStatus::Loading
                } else if playing {
                    FilePlaybackStatus::Playing
                } else {
                    FilePlaybackStatus::Paused
                };
            }
            // Decode a bounded amount ahead. PCM is limited to two seconds; video to two seconds and four frames.
            let video_ahead = video.back().map_or(0.0, |(pts, _)| *pts - position);
            if !eof
                && audio_samples < 192000
                && video.len() < 4
                && video
                    .iter()
                    .map(|(_, frame)| frame.bgra.len())
                    .sum::<usize>()
                    < 32 * 1024 * 1024
                && video_ahead < 2.0
            {
                let mut decoded = Decoded {
                    kind: 0,
                    width: 0,
                    height: 0,
                    pts: -1.0,
                    duration: 0.0,
                    data: ptr::null_mut(),
                    len: 0,
                };
                let io_started = Instant::now();
                let buffered_seconds = video
                    .front()
                    .map(|(pts, _)| (*pts - position).max(0.0))
                    .unwrap_or(0.0);
                if !has_audio {
                    gate.underflow.store(video.is_empty(), Ordering::Release);
                    if let Ok(mut deadline) = gate.video_deadline.lock() {
                        *deadline =
                            io_started.checked_add(Duration::from_secs_f64(buffered_seconds));
                    }
                }
                let result =
                    unsafe { flux_decoder_next(p, &mut decoded, error.as_mut_ptr(), error.len()) };
                if !has_audio && playing {
                    let stall = (io_started.elapsed().as_secs_f64() - buffered_seconds).max(0.0);
                    if let Some(adjusted) = clock_time.checked_add(Duration::from_secs_f64(stall)) {
                        clock_time = adjusted;
                    }
                }
                if !has_audio && result > 0 {
                    gate.underflow.store(false, Ordering::Release);
                }
                if result < 0 {
                    if shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .pending_seek
                    {
                        continue;
                    }
                    failed(
                        &shared,
                        reader
                            .as_ref()
                            .and_then(|reader| reader.error.clone())
                            .unwrap_or_else(|| error_text(&error)),
                    );
                    break;
                }
                if result > 0
                    && shared
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .pending_seek
                {
                    unsafe {
                        flux_decoder_data_free(decoded.data.cast());
                    }
                    continue;
                }
                if result == 0 {
                    eof = true;
                    if let Some(frame) = seek_fallback.take() {
                        let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                        if !s.pending_seek {
                            s.snapshot.width = frame.width;
                            s.snapshot.height = frame.height;
                            s.frame = Some(frame);
                        }
                    }
                    initialized = true;
                    continue;
                }
                let bytes = unsafe { std::slice::from_raw_parts(decoded.data, decoded.len) };
                if decoded.kind == 1 {
                    let pts = if decoded.pts < 0.0 {
                        position
                    } else {
                        decoded.pts.max(0.0)
                    };
                    if pts + 0.04 >= seek_target {
                        seek_fallback = None;
                        // Start the master clock from video even if the audio stream
                        // begins later. Silence is rendered until its first packet.
                        audio.lock().unwrap_or_else(|e| e.into_inner()).started = true;
                        let interval = previous_video_pts
                            .map(|previous| (pts - previous).clamp(0.0, 1.0))
                            .unwrap_or(0.04);
                        let frame_duration = if decoded.duration > 0.0 {
                            decoded.duration
                        } else {
                            interval.max(0.001)
                        };
                        video_end = pts + frame_duration;
                        previous_video_pts = Some(pts);
                        revision += 1;
                        video.push_back((
                            pts,
                            FilePlaybackFrame {
                                width: decoded.width as u32,
                                height: decoded.height as u32,
                                bgra: Arc::new(bytes.to_vec()),
                                revision,
                            },
                        ));
                    } else if seek_target > 0.0 {
                        revision += 1;
                        seek_fallback = Some(FilePlaybackFrame {
                            width: decoded.width as u32,
                            height: decoded.height as u32,
                            bgra: Arc::new(bytes.to_vec()),
                            revision,
                        });
                    }
                } else if decoded.kind == 2 {
                    let samples = unsafe {
                        std::slice::from_raw_parts(decoded.data.cast::<f32>(), decoded.len / 4)
                    };
                    let pts = if decoded.pts < 0.0 {
                        audio_tail
                    } else {
                        decoded.pts.max(0.0)
                    };
                    let skip =
                        (((seek_target - pts).max(0.0) * 48000.0) as usize * 2).min(samples.len());
                    let pts = pts + skip as f64 / 96000.0;
                    audio_tail = pts + (samples.len() - skip) as f64 / 96000.0;
                    if skip < samples.len() {
                        let samples = samples[skip..].to_vec();
                        let mut a = audio.lock().unwrap_or_else(|e| e.into_inner());
                        a.samples += samples.len();
                        a.started = true;
                        a.chunks.push_back(AudioChunk {
                            pts,
                            samples,
                            offset: 0,
                        });
                    }
                    if !initialized {
                        initialized = true;
                    }
                }
                unsafe {
                    flux_decoder_data_free(decoded.data.cast());
                }
            }
        }
        drop(output);
        drop(decoder);
    }
    #[cfg(test)]
    mod gate_tests {
        use super::*;
        #[test]
        fn network_underflow_freezes_audio_clock_and_reports_loading() {
            let audio = Mutex::new(Audio {
                chunks: VecDeque::new(),
                samples: 0,
                position: 2.0,
                playing: true,
                started: true,
                volume: 1.0,
                muted: false,
                error: None,
            });
            let gate = Arc::new(PlaybackGate {
                wanted: AtomicBool::new(true),
                seeking: AtomicBool::new(false),
                muted: AtomicBool::new(false),
                volume: AtomicU32::new(1.0f32.to_bits()),
                waiting: Arc::new(AtomicBool::new(true)),
                underflow: AtomicBool::new(false),
                video_only: AtomicBool::new(false),
                video_deadline: Mutex::new(None),
            });
            let mut output = [1.0; 960];
            render_gated(&audio, &gate, &mut output);
            assert!(output.iter().all(|sample| *sample == 0.0));
            assert_eq!(audio.lock().unwrap().position, 2.0);
            let (tx, _rx) = mpsc::channel();
            let player = FilePlayer {
                gate: gate.clone(),
                cancel: Arc::new(AtomicBool::new(false)),
                interrupt: Arc::new(AtomicU64::new(0)),
                ready: Arc::new(AtomicBool::new(true)),
                tx,
                shared: Arc::new(Mutex::new(Shared {
                    snapshot: FilePlaybackSnapshot {
                        status: FilePlaybackStatus::Playing,
                        position: 2.0,
                        duration: Some(4.0),
                        width: 0,
                        height: 0,
                        error: None,
                    },
                    frame: None,
                    seek_generation: 0,
                    pending_seek: false,
                })),
            };
            assert_eq!(player.snapshot().status, FilePlaybackStatus::Loading);
            gate.waiting.store(false, Ordering::Release);
            assert_eq!(player.snapshot().status, FilePlaybackStatus::Playing);
        }
        #[test]
        fn pause_and_seek_gate_queued_pcm_before_worker_handles_commands() {
            let audio = Mutex::new(Audio {
                chunks: VecDeque::from([AudioChunk {
                    pts: 0.0,
                    samples: vec![0.5; 8],
                    offset: 0,
                }]),
                samples: 8,
                position: 0.0,
                playing: true,
                started: true,
                volume: 1.0,
                muted: false,
                error: None,
            });
            let gate = PlaybackGate {
                wanted: AtomicBool::new(true),
                seeking: AtomicBool::new(false),
                muted: AtomicBool::new(false),
                volume: AtomicU32::new(1.0f32.to_bits()),
                waiting: Arc::new(AtomicBool::new(false)),
                underflow: AtomicBool::new(false),
                video_only: AtomicBool::new(false),
                video_deadline: Mutex::new(None),
            };
            let mut output = [0.0; 2];
            render_gated(&audio, &gate, &mut output);
            assert_eq!(output, [0.5; 2]);
            gate.muted.store(true, Ordering::Release);
            render_gated(&audio, &gate, &mut output);
            assert_eq!(output, [0.0; 2]);
            gate.muted.store(false, Ordering::Release);
            gate.wanted.store(false, Ordering::Release);
            render_gated(&audio, &gate, &mut output);
            assert_eq!(output, [0.0; 2]);
            assert_eq!(audio.lock().unwrap().samples, 4);
            gate.wanted.store(true, Ordering::Release);
            gate.seeking.store(true, Ordering::Release);
            render_gated(&audio, &gate, &mut output);
            assert_eq!(output, [0.0; 2]);
            assert_eq!(audio.lock().unwrap().samples, 4);
            gate.wanted.store(false, Ordering::Release);
            gate.seeking.store(false, Ordering::Release);
            render_gated(&audio, &gate, &mut output);
            assert_eq!(output, [0.0; 2]);
        }
    }
}

#[cfg(all(test, ffmpeg_file_playback))]
mod tests {
    use super::*;
    use std::{
        process::Command as Process,
        thread,
        time::{Duration, Instant},
    };
    fn wait(
        player: &FilePlayer,
        predicate: impl Fn(&FilePlaybackSnapshot) -> bool,
    ) -> FilePlaybackSnapshot {
        let end = Instant::now() + Duration::from_secs(12);
        loop {
            let s = player.snapshot();
            assert_ne!(s.status, FilePlaybackStatus::Failed, "{:?}", s.error);
            if predicate(&s) {
                return s;
            }
            assert!(Instant::now() < end, "timed out: {s:?}");
            thread::sleep(Duration::from_millis(10));
        }
    }
    fn fixture(ext: &str, video_codec: &str, audio_codec: &str) -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fluxer-playback-test-{}-{id}-{ext}.{ext}",
            std::process::id()
        ));
        let status = Process::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=64x48:rate=12:duration=3",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=3",
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                video_codec,
                "-c:a",
                audio_codec,
                "-shortest",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());
        path
    }
    #[test]
    fn custom_reader_rejects_playlist_inputs() {
        let playlist =
            b"#EXTM3U\n#EXT-X-TARGETDURATION:10\n#EXTINF:10,\nfile:///etc/passwd\n#EXT-X-ENDLIST\n"
                .to_vec();
        let player = FilePlayer::open_source_impl(
            Source::Reader(Box::new(std::io::Cursor::new(playlist))),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicBool::new(false)),
            FilePlaybackOptions::default(),
            true,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while player.snapshot().status != FilePlaybackStatus::Failed {
            assert!(Instant::now() < deadline, "playlist was not rejected");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn custom_reader_decodes_and_seeks_generated_mp4() {
        let path = fixture("mp4", "libx264", "aac");
        let bytes = std::fs::read(&path).unwrap();
        let player = FilePlayer::open_source_impl(
            Source::Reader(Box::new(std::io::Cursor::new(bytes))),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicBool::new(false)),
            FilePlaybackOptions::default(),
            true,
        )
        .unwrap();
        wait(&player, |s| {
            s.status == FilePlaybackStatus::Paused && s.width == 64
        });
        assert!(player.take_frame().is_some());
        player.seek(2.0);
        wait(&player, |s| {
            s.status == FilePlaybackStatus::Paused && s.position >= 1.9
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while player.take_frame().is_none() {
            assert!(Instant::now() < deadline, "seek preview did not arrive");
            thread::sleep(Duration::from_millis(10));
        }
        drop(player);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn mp4_and_webm_controls_and_frames() {
        for (ext, video, audio) in [("mp4", "libx264", "aac"), ("webm", "libvpx", "libopus")] {
            let path = fixture(ext, video, audio);
            let player =
                FilePlayer::open_impl(path.clone(), FilePlaybackOptions::default(), true).unwrap();
            let snapshot = wait(&player, |s| {
                s.status == FilePlaybackStatus::Paused && s.width == 64
            });
            assert_eq!(snapshot.height, 48);
            assert!(snapshot.duration.unwrap() > 2.8);
            assert_eq!(player.take_frame().unwrap().bgra.len(), 64 * 48 * 4);
            player.play();
            wait(&player, |s| {
                s.position > 0.35 && s.status == FilePlaybackStatus::Playing
            });
            player.pause();
            let paused = wait(&player, |s| s.status == FilePlaybackStatus::Paused).position;
            thread::sleep(Duration::from_millis(150));
            assert!((player.snapshot().position - paused).abs() < 0.06);
            player.seek(1.8);
            wait(&player, |s| {
                s.position >= 1.8 && s.status == FilePlaybackStatus::Paused
            });
            let preview_deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(frame) = player.take_frame() {
                    assert_eq!(frame.width, 64);
                    break;
                }
                assert!(
                    Instant::now() < preview_deadline,
                    "paused seek did not deliver a preview frame"
                );
                thread::sleep(Duration::from_millis(10));
            }
            let end = player.snapshot().duration.unwrap();
            player.seek(end);
            wait(&player, |s| {
                s.position >= end && s.status == FilePlaybackStatus::Paused
            });
            let preview_deadline = Instant::now() + Duration::from_secs(5);
            while player.take_frame().is_none() {
                assert!(
                    Instant::now() < preview_deadline,
                    "end seek did not deliver a final frame"
                );
                thread::sleep(Duration::from_millis(10));
            }
            player.seek(1.8);
            wait(&player, |s| {
                s.position >= 1.8 && s.position < 2.0 && s.status == FilePlaybackStatus::Paused
            });
            player.set_volume(0.2);
            player.set_muted(true);
            player.set_muted(false);
            player.set_output_device(None);
            player.play();
            wait(&player, |s| s.status == FilePlaybackStatus::Ended);
            player.play();
            wait(&player, |s| {
                s.status == FilePlaybackStatus::Playing && s.position < 1.0
            });
            drop(player);
            let _ = std::fs::remove_file(path);
        }
    }
    #[test]
    fn malformed_file_fails() {
        let path = std::env::temp_dir().join(format!("fluxer-invalid-{}.mp4", std::process::id()));
        std::fs::write(&path, b"not a media file").unwrap();
        let player =
            FilePlayer::open_impl(path.clone(), FilePlaybackOptions::default(), true).unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while player.snapshot().status != FilePlaybackStatus::Failed {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(10));
        }
        assert!(player.snapshot().error.is_some());
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn audio_only_decodes_tone_and_loops() {
        let path = std::env::temp_dir().join(format!("fluxer-audio-{}.wav", std::process::id()));
        let result = Process::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.7",
                "-c:a",
                "pcm_s16le",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(result.success());
        assert!(native::audio_peak(&path) > 0.01);
        let player = FilePlayer::open_impl(
            path.clone(),
            FilePlaybackOptions {
                autoplay: true,
                looping: true,
                ..Default::default()
            },
            true,
        )
        .unwrap();
        wait(&player, |s| s.position > 0.4);
        wait(&player, |s| {
            s.position < 0.2 && s.status == FilePlaybackStatus::Playing
        });
        player.stop();
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn video_only_uses_clock_and_reaches_end() {
        let path = std::env::temp_dir().join(format!("fluxer-video-{}.mp4", std::process::id()));
        let result = Process::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=64x48:rate=2:duration=1",
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                "libx264",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(result.success());
        let player =
            FilePlayer::open_impl(path.clone(), FilePlaybackOptions::default(), true).unwrap();
        wait(&player, |s| {
            s.status == FilePlaybackStatus::Paused && s.width == 64
        });
        let start = Instant::now();
        player.play();
        wait(&player, |s| s.status == FilePlaybackStatus::Ended);
        assert!(
            start.elapsed() >= Duration::from_millis(850),
            "last frame was cut short"
        );
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn short_audio_does_not_stall_longer_video() {
        let path =
            std::env::temp_dir().join(format!("fluxer-short-audio-{}.mp4", std::process::id()));
        let result = Process::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=64x48:rate=10:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=0.3",
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                "libx264",
                "-c:a",
                "aac",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(result.success());
        let player = FilePlayer::open_impl(
            path.clone(),
            FilePlaybackOptions {
                autoplay: true,
                ..Default::default()
            },
            true,
        )
        .unwrap();
        wait(&player, |s| s.status == FilePlaybackStatus::Ended);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    #[ignore = "opt-in synthetic 1080p playback performance test"]
    fn synthetic_1080p_30fps_keeps_clock_and_frames_current() {
        let path = std::env::temp_dir().join(format!("fluxer-1080p-{}.mp4", std::process::id()));
        let result = Process::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=1920x1080:rate=30:duration=5",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=5",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(result.success());
        let rotated =
            std::env::temp_dir().join(format!("fluxer-1080p-rotated-{}.mp4", std::process::id()));
        assert!(
            Process::new("ffmpeg")
                .args(["-v", "error", "-y", "-display_rotation", "90", "-i"])
                .arg(&path)
                .args(["-c", "copy"])
                .arg(&rotated)
                .status()
                .unwrap()
                .success()
        );
        for (file, expected_width, label) in
            [(&path, 1920, "landscape"), (&rotated, 1080, "rotated")]
        {
            let player = FilePlayer::open_impl(
                file.clone(),
                FilePlaybackOptions {
                    autoplay: true,
                    ..Default::default()
                },
                true,
            )
            .unwrap();
            wait(&player, |s| {
                s.status == FilePlaybackStatus::Playing && s.width == expected_width
            });
            let start = Instant::now();
            let initial = player.snapshot().position;
            let mut displayed = 0;
            while start.elapsed() < Duration::from_secs(3) {
                if let Some(frame) = player.take_frame() {
                    assert_eq!(frame.bgra.len(), 1920 * 1080 * 4);
                    displayed += 1;
                }
                thread::sleep(Duration::from_millis(8));
            }
            let elapsed = start.elapsed().as_secs_f64();
            let advanced = player.snapshot().position - initial;
            println!(
                "1080p30 {label} decoder presentation: {displayed} frames in {elapsed:.3}s ({:.2} fps), clock advanced {advanced:.3}s (ratio {:.3})",
                displayed as f64 / elapsed,
                advanced / elapsed
            );
            assert!(
                advanced > elapsed * 0.80 && advanced < elapsed * 1.20,
                "clock advanced {advanced:.3}s over {elapsed:.3}s"
            );
            assert!(
                displayed >= 60,
                "only {displayed} frames presented in {elapsed:.2}s"
            );
        }
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(rotated);
    }
    #[test]
    fn delayed_audio_does_not_block_video_clock() {
        let path =
            std::env::temp_dir().join(format!("fluxer-delayed-audio-{}.mp4", std::process::id()));
        let result = Process::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=64x48:rate=10:duration=2",
                "-itsoffset",
                "1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-c:v",
                "libx264",
                "-c:a",
                "aac",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(result.success());
        let player = FilePlayer::open_impl(
            path.clone(),
            FilePlaybackOptions {
                autoplay: true,
                ..Default::default()
            },
            true,
        )
        .unwrap();
        wait(&player, |s| s.position > 1.2);
        wait(&player, |s| s.status == FilePlaybackStatus::Ended);
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn short_loop_remains_ready_across_pause_and_resume() {
        let path =
            std::env::temp_dir().join(format!("fluxer-short-loop-{}.mp4", std::process::id()));
        assert!(
            Process::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=64x48:rate=20:duration=0.05",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:v",
                    "libx264"
                ])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let player = FilePlayer::open_impl(
            path.clone(),
            FilePlaybackOptions {
                autoplay: true,
                looping: true,
                muted: true,
                ..Default::default()
            },
            true,
        )
        .unwrap();
        wait(&player, |s| {
            s.status == FilePlaybackStatus::Playing && s.width == 64
        });
        for _ in 0..3 {
            let end = Instant::now() + Duration::from_millis(350);
            let mut previous = player.snapshot().position;
            let mut wraps = 0;
            while Instant::now() < end {
                let snapshot = player.snapshot();
                assert_eq!(
                    snapshot.status,
                    FilePlaybackStatus::Playing,
                    "ready GIF loop regressed to loading: {snapshot:?}"
                );
                if snapshot.position < previous {
                    wraps += 1;
                }
                previous = snapshot.position;
                thread::sleep(Duration::from_millis(1));
            }
            assert!(wraps >= 2, "short GIF did not continue looping");
            player.pause();
            let position = wait(&player, |s| s.status == FilePlaybackStatus::Paused).position;
            thread::sleep(Duration::from_millis(75));
            assert!((player.snapshot().position - position).abs() < 0.01);
            player.play();
            wait(&player, |s| s.status == FilePlaybackStatus::Playing);
        }
        drop(player);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn one_frame_loop_respects_duration() {
        let path =
            std::env::temp_dir().join(format!("fluxer-one-frame-{}.mp4", std::process::id()));
        let result = Process::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=64x48:rate=1:duration=1",
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                "libx264",
            ])
            .arg(&path)
            .status()
            .unwrap();
        assert!(result.success());
        let player = FilePlayer::open_impl(
            path.clone(),
            FilePlaybackOptions {
                autoplay: true,
                looping: true,
                ..Default::default()
            },
            true,
        )
        .unwrap();
        wait(&player, |s| s.position > 0.6);
        wait(&player, |s| {
            s.position < 0.2 && s.status == FilePlaybackStatus::Playing
        });
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn display_rotation_and_sample_aspect_are_applied() {
        let base =
            std::env::temp_dir().join(format!("fluxer-orient-base-{}.mp4", std::process::id()));
        let rotated =
            std::env::temp_dir().join(format!("fluxer-orient-{}.mp4", std::process::id()));
        let rotated_270 =
            std::env::temp_dir().join(format!("fluxer-orient-270-{}.mp4", std::process::id()));
        let wide = std::env::temp_dir().join(format!("fluxer-sar-{}.mp4", std::process::id()));
        let source = "color=black:size=80x40:rate=1:duration=1,drawbox=x=0:y=0:w=40:h=20:color=red:t=fill,drawbox=x=40:y=0:w=40:h=20:color=green:t=fill,drawbox=x=0:y=20:w=40:h=20:color=blue:t=fill,drawbox=x=40:y=20:w=40:h=20:color=white:t=fill";
        assert!(
            Process::new("ffmpeg")
                .args([
                    "-v", "error", "-y", "-f", "lavfi", "-i", source, "-c:v", "libx264",
                    "-pix_fmt", "yuv420p"
                ])
                .arg(&base)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Process::new("ffmpeg")
                .args(["-v", "error", "-y", "-display_rotation", "90", "-i"])
                .arg(&base)
                .args(["-c", "copy"])
                .arg(&rotated)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Process::new("ffmpeg")
                .args(["-v", "error", "-y", "-display_rotation", "270", "-i"])
                .arg(&base)
                .args(["-c", "copy"])
                .arg(&rotated_270)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Process::new("ffmpeg")
                .args(["-v", "error", "-y", "-i"])
                .arg(&base)
                .args(["-vf", "setsar=4/3", "-c:v", "libx264"])
                .arg(&wide)
                .status()
                .unwrap()
                .success()
        );
        let player =
            FilePlayer::open_impl(rotated.clone(), FilePlaybackOptions::default(), true).unwrap();
        wait(&player, |s| {
            s.status == FilePlaybackStatus::Paused && s.width > 0
        });
        let frame = player.take_frame().unwrap();
        assert_eq!((frame.width, frame.height), (40, 80));
        let pixel = |x: usize, y: usize| &frame.bgra[(y * 40 + x) * 4..(y * 40 + x) * 4 + 4];
        let top_left = pixel(10, 20);
        let bottom_left = pixel(10, 60);
        assert!(
            top_left[1] > 90 && top_left[2] < 40,
            "expected green in top-left: {top_left:?}"
        );
        assert!(
            bottom_left[2] > 200 && bottom_left[1] < 40,
            "expected red in bottom-left: {bottom_left:?}"
        );
        let player =
            FilePlayer::open_impl(rotated_270.clone(), FilePlaybackOptions::default(), true)
                .unwrap();
        wait(&player, |s| {
            s.status == FilePlaybackStatus::Paused && s.width > 0
        });
        let frame = player.take_frame().unwrap();
        assert_eq!((frame.width, frame.height), (40, 80));
        let pixel = &frame.bgra[(20 * 40 + 10) * 4..(20 * 40 + 10) * 4 + 4];
        assert!(
            pixel[0] > 200 && pixel[2] < 40,
            "expected blue in top-left: {pixel:?}"
        );
        let player =
            FilePlayer::open_impl(wide.clone(), FilePlaybackOptions::default(), true).unwrap();
        wait(&player, |s| {
            s.status == FilePlaybackStatus::Paused && s.width > 0
        });
        let frame = player.take_frame().unwrap();
        assert_eq!((frame.width, frame.height), (107, 40));
        assert_eq!(frame.bgra.len(), 107 * 40 * 4);
        // The last displayed row must be packed correctly even when the
        // scaler's intermediate row includes padding after its last pixel.
        let row = &frame.bgra[39 * 107 * 4..];
        assert!(row[0] > 200 && row[2] < 40, "expected blue at bottom left");
        assert!(
            row[106 * 4..][..3].iter().all(|&channel| channel > 200),
            "expected white at bottom right"
        );
        for path in [base, rotated, rotated_270, wide] {
            let _ = std::fs::remove_file(path);
        }
    }
    #[test]
    fn latest_seek_wins_during_rapid_scrubbing() {
        let path = fixture("mp4", "libx264", "aac");
        let player =
            FilePlayer::open_impl(path.clone(), FilePlaybackOptions::default(), true).unwrap();
        wait(&player, |s| {
            s.status == FilePlaybackStatus::Paused && s.width > 0
        });
        for n in 0..500 {
            player.seek((n % 20) as f64 / 10.0);
        }
        player.seek(2.2);
        let start = Instant::now();
        wait(&player, |s| {
            s.status == FilePlaybackStatus::Paused && s.position >= 2.2
        });
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "latest scrub seek was delayed"
        );
        let _ = std::fs::remove_file(path);
    }
}
