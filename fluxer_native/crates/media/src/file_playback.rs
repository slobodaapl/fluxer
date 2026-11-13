// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local media playback with asynchronous decoding and teardown.
use std::{
    path::PathBuf,
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
enum Command {
    Play,
    Pause,
    Seek(f64, u64),
    Volume(f32),
    Muted(bool),
    Output(Option<String>),
    Stop,
}
pub struct FilePlayer {
    tx: mpsc::Sender<Command>,
    shared: Arc<Mutex<Shared>>,
}
impl FilePlayer {
    pub fn open(path: PathBuf, options: FilePlaybackOptions) -> Result<Self, String> {
        Self::open_impl(path, options, false)
    }
    fn open_impl(path: PathBuf, options: FilePlaybackOptions, fake: bool) -> Result<Self, String> {
        #[cfg(not(ffmpeg_file_playback))]
        {
            let _ = (path, options, fake);
            Err("Native file playback requires FFmpeg on Linux".into())
        }
        #[cfg(ffmpeg_file_playback)]
        {
            if !options.volume.is_finite() {
                return Err("Invalid volume".into());
            }

            let path =
                std::fs::canonicalize(path).map_err(|e| format!("Cannot open media file: {e}"))?;
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
            std::thread::Builder::new()
                .name("file-playback".into())
                .spawn(move || native::run(path, options, fake, rx, worker_shared))
                .map_err(|e| e.to_string())?;
            Ok(Self { tx, shared })
        }
    }
    pub fn play(&self) {
        let _ = self.tx.send(Command::Play);
    }
    pub fn pause(&self) {
        let _ = self.tx.send(Command::Pause);
    }
    pub fn seek(&self, seconds: f64) {
        if seconds.is_finite() && seconds >= 0.0 {
            let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            shared.frame = None;
            shared.pending_seek = true;
            shared.seek_generation = shared.seek_generation.wrapping_add(1);
            let _ = self.tx.send(Command::Seek(
                seconds.min(i64::MAX as f64 / 1e6),
                shared.seek_generation,
            ));
        }
    }
    pub fn set_volume(&self, volume: f32) {
        if volume.is_finite() {
            let _ = self.tx.send(Command::Volume(volume.clamp(0.0, 1.0)));
        }
    }
    pub fn set_muted(&self, muted: bool) {
        let _ = self.tx.send(Command::Muted(muted));
    }
    pub fn set_output_device(&self, id: Option<String>) {
        let _ = self.tx.send(Command::Output(id));
    }
    pub fn stop(&self) {
        let mut shared = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        shared.frame = None;
        shared.pending_seek = true;
        let _ = self.tx.send(Command::Stop);
    }
    pub fn snapshot(&self) -> FilePlaybackSnapshot {
        self.shared
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .snapshot
            .clone()
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
        let _ = self.tx.send(Command::Stop);
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
        data: *mut u8,
        len: usize,
    }
    unsafe extern "C" {
        fn flux_decoder_open(path: *const c_char, error: *mut c_char, len: usize) -> *mut c_void;
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
        fn render(&mut self, output: &mut [f32]) {
            output.fill(0.0);
            if !self.playing {
                return;
            }
            let gain = if self.muted { 0.0 } else { self.volume };
            let mut done = 0;
            while done < output.len() {
                let Some(chunk) = self.chunks.front_mut() else {
                    if self.started {
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
    fn start_output(
        id: Option<&str>,
        audio: Arc<Mutex<Audio>>,
    ) -> Result<fluxer_system_audio::SpeakerOutput, String> {
        let errors = audio.clone();
        fluxer_system_audio::SpeakerOutput::start(
            id,
            move |out| {
                if let Ok(mut a) = audio.try_lock() {
                    a.render(out);
                } else {
                    out.fill(0.0);
                }
            },
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
        path: PathBuf,
        mut options: FilePlaybackOptions,
        fake: bool,
        rx: mpsc::Receiver<Command>,
        shared: Arc<Mutex<Shared>>,
    ) {
        let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
            failed(&shared, "Invalid file path".into());
            return;
        };
        let mut error = [0 as c_char; 512];
        let p = unsafe { flux_decoder_open(path.as_ptr(), error.as_mut_ptr(), error.len()) };
        if p.is_null() {
            failed(&shared, error_text(&error));
            return;
        }
        let decoder = Decoder(p);
        let has_audio = unsafe { flux_decoder_has_audio(p) != 0 };
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
            match start_output(options.output_device.as_deref(), audio.clone()) {
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
        let mut eof = false;
        let mut ended = false;
        let mut initialized = false;
        let mut seek_target = 0.0;
        let mut clock_base = 0.0;
        let mut clock_time = Instant::now();
        let mut fake_time = Instant::now();
        let mut audio_tail = 0.0;

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
                        eof = false;
                        ended = false;
                        seek_target = 0.0;
                        clock_base = 0.0;
                        audio_tail = 0.0;
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
                    if unsafe { flux_decoder_seek(p, pos, error.as_mut_ptr(), error.len()) } == 0 {
                        failed(&shared, error_text(&error));
                        break;
                    }
                    video.clear();
                    eof = false;
                    ended = false;
                    seek_target = pos;
                    clock_base = pos;
                    audio_tail = pos;
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
                    }
                }
                Ok(Command::Volume(v)) => {
                    audio.lock().unwrap_or_else(|e| e.into_inner()).volume = v
                }
                Ok(Command::Muted(v)) => audio.lock().unwrap_or_else(|e| e.into_inner()).muted = v,
                Ok(Command::Output(id)) => {
                    if has_audio && !fake {
                        output.take();
                        match start_output(id.as_deref(), audio.clone()) {
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
                    audio
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .render(&mut samples);
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
                .is_some_and(|(pts, _)| *pts <= position + 0.015 || (!initialized && !playing))
            {
                latest = video.pop_front().map(|(_, frame)| frame);
                initialized = true;
            }
            if let Some(frame) = latest {
                let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
                if !s.pending_seek {
                    s.snapshot.width = frame.width;
                    s.snapshot.height = frame.height;
                    s.frame = Some(frame);
                }
            }
            if eof && audio_samples == 0 && video.is_empty() && playing {
                if options.looping {
                    if unsafe { flux_decoder_seek(p, 0.0, error.as_mut_ptr(), error.len()) } == 0 {
                        failed(&shared, error_text(&error));
                        break;
                    }
                    eof = false;
                    initialized = false;
                    seek_target = 0.0;
                    clock_base = 0.0;
                    audio_tail = 0.0;
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
                    data: ptr::null_mut(),
                    len: 0,
                };
                let result =
                    unsafe { flux_decoder_next(p, &mut decoded, error.as_mut_ptr(), error.len()) };
                if result < 0 {
                    failed(&shared, error_text(&error));
                    break;
                }
                if result == 0 {
                    eof = true;
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
        let path = std::env::temp_dir().join(format!(
            "fluxer-playback-test-{}-{ext}.{ext}",
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
            assert!(player.take_frame().is_none());
            wait(&player, |s| {
                s.position >= 1.8 && s.status == FilePlaybackStatus::Paused
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
                "testsrc=size=64x48:rate=10:duration=1",
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
        player.play();
        wait(&player, |s| s.status == FilePlaybackStatus::Ended);
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
}
