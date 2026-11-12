// SPDX-License-Identifier: AGPL-3.0-or-later
//! Output to exact PipeWire sink nodes. This never changes the system default,
//! moves other applications, or collapses logical GoXLR routes into one card.
use crate::{Error, microphone::enumerate_audio_nodes, routing::PropMap};
use pipewire as pw;
use pw::{properties::properties, spa};
use std::{
    collections::BTreeMap,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};

const PREFIX: &str = "output:pipewire:";
const DEFAULT: &str = "output:default";
const RATE: u32 = 48_000;
const CHANNELS: usize = 2;
const STRIDE: usize = CHANNELS * std::mem::size_of::<f32>();
const MAX_FRAMES: usize = 32_768;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputDevice {
    pub id: String,
    pub label: String,
}
pub struct SpeakerOutput {
    shutdown: pw::channel::Sender<()>,
    worker: Mutex<Option<JoinHandle<()>>>,
    stopping: Arc<AtomicBool>,
}
impl SpeakerOutput {
    pub fn devices() -> Result<Vec<OutputDevice>, Error> {
        enumerate_audio_nodes()
            .map(output_devices)
            .map_err(|_| Error::OutputUnavailable)
    }
    /// `render` supplies exactly 48 kHz stereo interleaved F32. Callbacks run on
    /// the output owner thread and must return promptly without blocking.
    pub fn start(
        id: Option<&str>,
        render: impl FnMut(&mut [f32]) + Send + 'static,
        on_error: impl FnMut(String) + Send + 'static,
    ) -> Result<Self, Error> {
        let target = match id.filter(|id| *id != DEFAULT) {
            None => None,
            Some(id) => {
                if !Self::devices()?.iter().any(|device| device.id == id) {
                    return Err(Error::UnknownOutput);
                }
                Some(
                    id.strip_prefix(PREFIX)
                        .ok_or(Error::UnknownOutput)?
                        .to_owned(),
                )
            }
        };
        let (shutdown, commands) = pw::channel::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = stopping.clone();
        let worker = std::thread::Builder::new()
            .name("fluxer-speaker".into())
            .spawn(move || {
                let mut data = OutputData {
                    render: Box::new(render),
                    on_error: Box::new(on_error),
                    samples: Vec::with_capacity(MAX_FRAMES * CHANNELS),
                    ready: Some(ready_tx.clone()),
                    failed: false,
                    stopping: worker_stopping,
                };
                if output_worker(target, commands, &mut data).is_err() && !data.failed {
                    data.fail("Unable to initialize the selected PipeWire speaker".into());
                }
            })
            .map_err(|_| Error::OutputUnavailable)?;
        match ready_rx.recv_timeout(Duration::from_secs(4)) {
            Ok(Ok(())) => Ok(Self {
                shutdown,
                worker: Mutex::new(Some(worker)),
                stopping,
            }),
            _ => {
                stopping.store(true, Ordering::Release);
                let _ = shutdown.send(());
                let _ = worker.join();
                Err(Error::OutputUnavailable)
            }
        }
    }
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        let _ = self.shutdown.send(());
        if let Ok(mut owner) = self.worker.lock()
            && let Some(worker) = owner.take()
            && worker.thread().id() != std::thread::current().id()
        {
            let _ = worker.join();
        }
    }
}
impl Drop for SpeakerOutput {
    fn drop(&mut self) {
        self.stop();
    }
}

// VoiceDeviceManager.ts:53-62 excludes these label substrings. Also inspect
// node.name/node.nick because PipeWire's node description can be overridden.
fn internal(props: &PropMap) -> bool {
    ["node.name", "node.description", "node.nick"]
        .iter()
        .filter_map(|key| props.get(*key))
        .any(|value| {
            let value = value.trim().to_lowercase();
            [
                "fluxer-screen-share",
                "fluxer screen share",
                "fluxer-direct-capture",
                "fluxer direct capture",
            ]
            .iter()
            .any(|name| value.contains(name))
        })
}
fn output_devices(nodes: Vec<PropMap>) -> Vec<OutputDevice> {
    let mut names = BTreeMap::new();
    for props in nodes {
        if !matches!(
            props.get("media.class").map(String::as_str),
            Some("Audio/Sink" | "Audio/Sink/Virtual")
        ) || internal(&props)
        {
            continue;
        }
        let Some(name) = props.get("node.name").filter(|name| !name.is_empty()) else {
            continue;
        };
        let label = props
            .get("node.description")
            .or_else(|| props.get("node.nick"))
            .unwrap_or(name);
        names.insert(name.clone(), label.clone());
    }
    let mut devices: Vec<_> = names
        .into_iter()
        .map(|(name, label)| OutputDevice {
            id: format!("{PREFIX}{name}"),
            label,
        })
        .collect();
    devices.sort_by(|a, b| {
        a.label
            .to_lowercase()
            .cmp(&b.label.to_lowercase())
            .then_with(|| a.id.cmp(&b.id))
    });
    devices.insert(
        0,
        OutputDevice {
            id: DEFAULT.into(),
            label: "Linux default".into(),
        },
    );
    devices
}
type RenderCallback = Box<dyn FnMut(&mut [f32]) + Send>;
struct OutputData {
    render: RenderCallback,
    on_error: Box<dyn FnMut(String) + Send>,
    samples: Vec<f32>,
    ready: Option<mpsc::SyncSender<Result<(), Error>>>,
    failed: bool,
    stopping: Arc<AtomicBool>,
}
impl OutputData {
    fn fail(&mut self, message: String) {
        if self.failed || self.stopping.load(Ordering::Acquire) {
            return;
        }
        self.failed = true;
        if let Some(ready) = self.ready.take() {
            let _ = ready.try_send(Err(Error::OutputUnavailable));
        }
        let _ = catch_unwind(AssertUnwindSafe(|| (self.on_error)(message)));
    }
}
fn output_worker(
    target: Option<String>,
    commands: pw::channel::Receiver<()>,
    data: &mut OutputData,
) -> Result<(), pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let quit = mainloop.clone();
    let _commands = commands.attach(mainloop.loop_(), move |_| quit.quit());
    let mut props = properties! {
        "node.name"=>format!("fluxer-native-speaker-{}",std::process::id()),
        "application.name"=>"Fluxer Native",
        "application.process.id"=>std::process::id().to_string(),
        "media.type"=>"Audio","media.category"=>"Playback","media.role"=>"Communication",
        "media.class"=>"Stream/Output/Audio","node.latency"=>"480/48000",
    };
    if let Some(target) = target {
        props.insert("target.object", target);
        props.insert("node.dont-fallback", "true");
        props.insert("node.dont-move", "true");
    }
    let mut format = spa::param::audio::AudioInfoRaw::new();
    format.set_format(spa::param::audio::AudioFormat::F32LE);
    format.set_rate(RATE);
    format.set_channels(CHANNELS as u32);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    format.set_position(position);
    let stream = pw::stream::StreamRc::new(core.clone(), "Fluxer speakers", props)?;
    let quit = mainloop.clone();
    let quit_format = mainloop.clone();
    let quit_process = mainloop.clone();
    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(move |_, data, _, state| match state {
            pw::stream::StreamState::Streaming => {
                if !data.failed
                    && let Some(ready) = data.ready.take()
                {
                    let _ = ready.try_send(Ok(()));
                }
            }
            pw::stream::StreamState::Error(error) => {
                data.fail(format!("Selected PipeWire speaker failed: {error}"));
                quit.quit();
            }
            pw::stream::StreamState::Unconnected if data.ready.is_none() => {
                data.fail(
                    "Selected PipeWire speaker disconnected; refresh and select an output".into(),
                );
                quit.quit();
            }
            _ => (),
        })
        .param_changed(move |_, data, id, param| {
            if id == spa::param::ParamType::Format.as_raw()
                && let Some(param) = param
            {
                let mut negotiated = spa::param::audio::AudioInfoRaw::new();
                if negotiated.parse(param).is_err() || !valid_format(&negotiated) {
                    data.fail(Error::InvalidOutputFormat.to_string());
                    quit_format.quit();
                }
            }
        })
        .process(move |stream, data| {
            if data.failed || data.stopping.load(Ordering::Acquire) {
                return;
            }
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let requested = buffer.requested();
            let Some(payload) = buffer.datas_mut().first_mut() else {
                return;
            };
            let Some(bytes) = payload.data() else { return };
            let answer = render_chunk(bytes, requested, &mut data.samples, &mut *data.render);
            let chunk = payload.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = STRIDE as i32;
            match answer {
                Ok(size) => *chunk.size_mut() = size as u32,
                Err(error) => {
                    *chunk.size_mut() = 0;
                    data.fail(error.to_string());
                    quit_process.quit();
                }
            }
        })
        .register()?;
    let object = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: format.into(),
    };
    let bytes = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map_err(|_| pw::Error::CreationFailed)?
    .0
    .into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&bytes).ok_or(pw::Error::CreationFailed)?];
    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;
    mainloop.run();
    stream.disconnect()?;
    Ok(())
}
fn valid_format(format: &spa::param::audio::AudioInfoRaw) -> bool {
    format.format() == spa::param::audio::AudioFormat::F32LE
        && format.rate() == RATE
        && format.channels() == CHANNELS as u32
}
fn render_chunk(
    bytes: &mut [u8],
    requested: u64,
    samples: &mut Vec<f32>,
    render: &mut dyn FnMut(&mut [f32]),
) -> Result<usize, Error> {
    let capacity = (bytes.len() / STRIDE).min(MAX_FRAMES);
    let frames = if requested == 0 {
        capacity
    } else {
        capacity.min(usize::try_from(requested).unwrap_or(usize::MAX))
    };
    if frames == 0 {
        return Err(Error::InvalidOutputBuffer);
    }
    let count = frames * CHANNELS;
    samples.resize(count, 0.0);
    samples.fill(0.0);
    if catch_unwind(AssertUnwindSafe(|| render(samples))).is_err() {
        return Err(Error::InvalidOutputBuffer);
    }
    for (sample, dest) in samples.iter().zip(bytes.as_chunks_mut::<4>().0) {
        let sample = if sample.is_finite() {
            sample.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        dest.copy_from_slice(&sample.to_le_bytes());
    }
    Ok(frames * STRIDE)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn node(class: &str, name: &str, label: &str) -> PropMap {
        PropMap::from([
            ("media.class".into(), class.into()),
            ("node.name".into(), name.into()),
            ("node.description".into(), label.into()),
        ])
    }
    #[test]
    fn logical_sink_picker_preserves_goxlr_routes_filters_sources_and_internal_nodes() {
        let mut nodes: Vec<_> = ["Chat", "Game", "Music", "Sample", "System"]
            .into_iter()
            .map(|name| {
                node(
                    "Audio/Sink",
                    &format!("goxlr-{name}"),
                    &format!("GoXLR {name}"),
                )
            })
            .collect();
        nodes.extend([
            node("Audio/Sink", "jds", "JDS Labs Atom DAC 2 Analog Stereo"),
            node("Audio/Sink", "hdmi", "GA102 HDMI LG ULTRAGEAR"),
            node("Audio/Source", "goxlr-mic", "GoXLR Chat Mic"),
            node("Audio/Sink/Internal", "alsa_output.hw_GoXLR_0", "Raw GoXLR"),
            node("Stream/Output/Audio", "player", "App playback"),
            node(
                "Audio/Sink",
                "fluxer-direct-capture-123-1",
                "Temporary routing sink",
            ),
            node(
                "Audio/Sink/Virtual",
                "unknown",
                " FLUXER SCREEN SHARE audio ",
            ),
            node(
                "Audio/Sink/Virtual",
                "users-virtual",
                "User's virtual speakers",
            ),
        ]);
        let devices = output_devices(nodes);
        assert_eq!(
            devices[0],
            OutputDevice {
                id: "output:default".into(),
                label: "Linux default".into()
            }
        );
        assert_eq!(devices.len(), 9);
        let goxlr: Vec<_> = devices
            .iter()
            .filter(|d| d.label.starts_with("GoXLR"))
            .collect();
        assert_eq!(goxlr.len(), 5);
        for name in ["Chat", "Game", "Music", "Sample", "System"] {
            assert!(
                goxlr
                    .iter()
                    .any(|d| d.id == format!("output:pipewire:goxlr-{name}"))
            );
        }
        assert!(!devices.iter().any(|d| d.label.contains("Mic")
            || d.label.contains("Raw")
            || d.label.contains("routing")));
        assert!(
            devices[1..]
                .windows(2)
                .all(|pair| pair[0].label.to_lowercase() <= pair[1].label.to_lowercase())
        );
    }
    #[test]
    fn sink_identifiers_remain_distinct_when_labels_match() {
        let devices = output_devices(vec![
            node("Audio/Sink", "a", "Speakers"),
            node("Audio/Sink", "b", "Speakers"),
            node("Audio/Sink", "a", "Speakers"),
        ]);
        assert_eq!(devices.len(), 3);
        assert_ne!(devices[1].id, devices[2].id);
    }
    #[test]
    fn output_honors_requested_frames_capacity_and_sanitizes_samples() {
        let mut bytes = vec![0xAA; 84];
        let mut scratch = Vec::new();
        let mut calls = 0;
        let size = render_chunk(&mut bytes, 3, &mut scratch, &mut |samples| {
            calls += 1;
            assert_eq!(samples.len(), 6);
            samples.copy_from_slice(&[0.25, -0.5, f32::NAN, f32::INFINITY, 2.0, -2.0]);
        })
        .unwrap();
        assert_eq!(calls, 1);
        assert_eq!(size, 24);
        let samples: Vec<_> = bytes[..size]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|v| f32::from_le_bytes(*v))
            .collect();
        assert_eq!(samples, [0.25, -0.5, 0.0, 0.0, 1.0, -1.0]);
        assert!(bytes[size..].iter().all(|v| *v == 0xAA));
        let size = render_chunk(&mut bytes, u64::MAX, &mut scratch, &mut |samples| {
            assert_eq!(samples.len(), 20)
        })
        .unwrap();
        assert_eq!(size, 80);
        assert!(bytes[80..].iter().all(|v| *v == 0xAA));
        assert_eq!(
            render_chunk(&mut bytes, 0, &mut scratch, &mut |_| {}).unwrap(),
            80
        );
    }
    #[test]
    fn output_allocation_is_bounded_and_unwritten_samples_become_silence() {
        let mut bytes = vec![0xAA; MAX_FRAMES * STRIDE * 2];
        let mut scratch = Vec::new();
        assert_eq!(
            render_chunk(&mut bytes, 0, &mut scratch, &mut |samples| {
                assert_eq!(samples.len(), MAX_FRAMES * 2);
                samples[0] = 0.5;
            })
            .unwrap(),
            MAX_FRAMES * STRIDE
        );
        assert_eq!(scratch.len(), MAX_FRAMES * 2);
        assert_eq!(
            render_chunk(&mut bytes, 1, &mut scratch, &mut |_| {}).unwrap(),
            STRIDE
        );
        assert_eq!(&bytes[..STRIDE], &[0; STRIDE]);
        assert!(matches!(
            render_chunk(&mut [0; 7], 1, &mut scratch, &mut |_| panic!(
                "must not render undersized buffer"
            )),
            Err(Error::InvalidOutputBuffer)
        ));
    }
    #[test]
    fn output_rejects_negotiated_format_mismatches() {
        let mut format = spa::param::audio::AudioInfoRaw::new();
        format.set_rate(RATE);
        format.set_channels(2);
        format.set_format(spa::param::audio::AudioFormat::F32LE);
        assert!(valid_format(&format));
        format.set_channels(1);
        assert!(!valid_format(&format));
        format.set_channels(2);
        format.set_rate(44_100);
        assert!(!valid_format(&format));
        format.set_rate(RATE);
        format.set_format(spa::param::audio::AudioFormat::F32P);
        assert!(!valid_format(&format));
    }
}
