// SPDX-License-Identifier: AGPL-3.0-or-later
//! Device-source capture through PipeWire's input Stream API. This is separate
//! from the playback-routing engine: selecting a microphone never moves apps
//! or captures a sink/monitor mix implicitly.
use crate::{Error, PcmFrame};
use pipewire as pw;
use pw::{properties::properties, spa};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};

const PREFIX: &str = "input:pipewire:";
const DEFAULT: &str = "input:default";
const RATE: u32 = 48_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputDevice {
    pub id: String,
    pub label: String,
}

pub struct MicrophoneInput {
    frames: Mutex<mpsc::Receiver<PcmFrame>>,
    shutdown: pw::channel::Sender<()>,
    worker: Mutex<Option<JoinHandle<()>>>,
    failed: Arc<AtomicBool>,
}

impl MicrophoneInput {
    pub fn devices() -> Result<Vec<InputDevice>, Error> {
        enumerate_audio_nodes()
            .map(input_devices)
            .map_err(|_| Error::Unavailable)
    }

    pub fn start(id: Option<&str>, channels: u32) -> Result<Self, Error> {
        if !matches!(channels, 1 | 2) {
            return Err(Error::InvalidInputFormat);
        }
        let target = match id.filter(|id| *id != DEFAULT) {
            None => None,
            Some(id) => {
                if !Self::devices()?.iter().any(|device| device.id == id) {
                    return Err(Error::UnknownInput);
                }
                Some(
                    id.strip_prefix(PREFIX)
                        .ok_or(Error::UnknownInput)?
                        .to_owned(),
                )
            }
        };
        let (shutdown, commands) = pw::channel::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (frames_tx, frames_rx) = mpsc::sync_channel(8);
        let failed = Arc::new(AtomicBool::new(false));
        let worker_failed = failed.clone();
        let worker = std::thread::Builder::new()
            .name("fluxer-microphone".into())
            .spawn(move || {
                let result = capture_worker(
                    target,
                    channels,
                    commands,
                    frames_tx,
                    ready_tx.clone(),
                    worker_failed.clone(),
                );
                if result.is_err() {
                    worker_failed.store(true, Ordering::Release);
                    let _ = ready_tx.try_send(Err(Error::Unavailable));
                }
            })
            .map_err(|_| Error::Unavailable)?;
        match ready_rx.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => Ok(Self {
                frames: Mutex::new(frames_rx),
                shutdown,
                worker: Mutex::new(Some(worker)),
                failed,
            }),
            _ => {
                let _ = shutdown.send(());
                let _ = worker.join();
                Err(Error::Unavailable)
            }
        }
    }

    pub fn read(&self) -> Result<Option<PcmFrame>, Error> {
        if self.failed.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        match self
            .frames
            .lock()
            .map_err(|_| Error::Unavailable)?
            .try_recv()
        {
            Ok(frame) => Ok(Some(frame)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(Error::Unavailable),
        }
    }

    pub fn stop(&self) {
        let _ = self.shutdown.send(());
        if let Ok(mut worker) = self.worker.lock()
            && let Some(worker) = worker.take()
        {
            let _ = worker.join();
        }
    }
}

pub(crate) fn enumerate_audio_nodes() -> Result<Vec<crate::routing::PropMap>, pw::Error> {
    use crate::pipewire::common::{InventorySnapshot, collect_props};
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };
    struct WatchedNode {
        _listener: pw::node::NodeListener,
        _node: pw::node::Node,
    }
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;
    let inventory = Rc::new(RefCell::new(InventorySnapshot::default()));
    let nodes = Rc::new(RefCell::new(Vec::<WatchedNode>::new()));
    let globals = inventory.clone();
    let bind_registry = registry.clone();
    let watches = nodes.clone();
    let _registry = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ == pw::types::ObjectType::Client {
                globals
                    .borrow_mut()
                    .clients
                    .insert(global.id, collect_props(global.props));
                return;
            }
            if global.type_ != pw::types::ObjectType::Node {
                return;
            }
            let Some(props) = global.props else { return };
            if !matches!(
                props.get("media.class"),
                Some(
                    "Audio/Source"
                        | "Audio/Source/Virtual"
                        | "Audio/Sink"
                        | "Audio/Sink/Virtual"
                        | "Stream/Output/Audio"
                )
            ) {
                return;
            }
            globals
                .borrow_mut()
                .nodes
                .insert(global.id, collect_props(Some(props)));
            // Stream globals can omit process identity. Bind the complete NodeInfo
            // properties, then enrich from Client identity like the shared engine.
            if let Ok(node) = bind_registry.bind::<pw::node::Node, _>(global) {
                let info_inventory = globals.clone();
                let id = global.id;
                let listener = node
                    .add_listener_local()
                    .info(move |info| {
                        if info.change_mask().contains(pw::node::NodeChangeMask::PROPS)
                            && let Some(props) = info.props()
                        {
                            info_inventory
                                .borrow_mut()
                                .nodes
                                .entry(id)
                                .or_default()
                                .extend(collect_props(Some(props)));
                        }
                    })
                    .register();
                watches.borrow_mut().push(WatchedNode {
                    _listener: listener,
                    _node: node,
                });
            }
        })
        .register();
    // First barrier drains registry globals; second drains NodeInfo replies for
    // binds created while processing those globals. Startup-ready alone is not
    // a complete application inventory.
    let pending = Rc::new(Cell::new(core.sync(0)?));
    let sequence = pending.clone();
    let enriched = Rc::new(Cell::new(false));
    let phase = enriched.clone();
    let done = Rc::new(Cell::new(false));
    let complete = done.clone();
    let quit = mainloop.clone();
    let sync_core = core.clone();
    let _core = core
        .add_listener_local()
        .done(move |id, observed| {
            if id != pw::core::PW_ID_CORE || observed != sequence.get() {
                return;
            }
            if !phase.replace(true) {
                if let Ok(next) = sync_core.sync(0) {
                    sequence.set(next);
                } else {
                    quit.quit();
                }
            } else {
                complete.set(true);
                quit.quit();
            }
        })
        .register();
    let quit = mainloop.clone();
    let timer = mainloop.loop_().add_timer(move |_| quit.quit());
    timer
        .update_timer(Some(Duration::from_secs(3)), None)
        .into_result()
        .map_err(|_| pw::Error::CreationFailed)?;
    mainloop.run();
    if !done.get() {
        return Err(pw::Error::CreationFailed);
    }
    let snapshot = inventory.borrow().enriched_node_values();
    Ok(snapshot)
}

impl Drop for MicrophoneInput {
    fn drop(&mut self) {
        self.stop();
    }
}

fn input_devices(inventory: Vec<crate::routing::PropMap>) -> Vec<InputDevice> {
    let mut sources = BTreeMap::new();
    for props in inventory {
        if !matches!(
            props.get("media.class").map(String::as_str),
            Some("Audio/Source" | "Audio/Source/Virtual")
        ) {
            continue;
        }
        let Some(name) = props.get("node.name").filter(|name| !name.is_empty()) else {
            continue;
        };
        let label = props
            .get("node.description")
            .or_else(|| props.get("node.nick"))
            .unwrap_or(name);
        sources.insert(name.clone(), label.clone());
    }
    std::iter::once(InputDevice {
        id: DEFAULT.into(),
        label: "System Default".into(),
    })
    .chain(sources.into_iter().map(|(name, label)| InputDevice {
        id: format!("{PREFIX}{name}"),
        label,
    }))
    .collect()
}

struct CaptureData {
    format: spa::param::audio::AudioInfoRaw,
    frames: mpsc::SyncSender<PcmFrame>,
    ready: Option<mpsc::SyncSender<Result<(), Error>>>,
    failed: Arc<AtomicBool>,
}

fn capture_worker(
    target: Option<String>,
    channels: u32,
    commands: pw::channel::Receiver<()>,
    frames: mpsc::SyncSender<PcmFrame>,
    ready: mpsc::SyncSender<Result<(), Error>>,
    failed: Arc<AtomicBool>,
) -> Result<(), pw::Error> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let quit = mainloop.clone();
    let _commands = commands.attach(mainloop.loop_(), move |_| quit.quit());
    let mut props = properties! {
        "node.name" => format!("fluxer-native-microphone-{}", std::process::id()),
        "application.name" => "Fluxer Native",
        "media.type" => "Audio", "media.category" => "Capture", "media.role" => "Communication",
        "media.class" => "Stream/Input/Audio", "node.latency" => "480/48000",
    };
    if let Some(target) = target {
        props.insert("target.object", target);
        props.insert("node.dont-fallback", "true");
        props.insert("node.dont-move", "true");
    }
    let mut format = spa::param::audio::AudioInfoRaw::new();
    format.set_format(spa::param::audio::AudioFormat::F32LE);
    format.set_rate(RATE);
    format.set_channels(channels);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    if channels == 1 {
        position[0] = spa::sys::SPA_AUDIO_CHANNEL_MONO;
    } else {
        position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
        position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    }
    format.set_position(position);
    let data = CaptureData {
        format,
        frames,
        ready: Some(ready),
        failed,
    };
    let stream = pw::stream::StreamRc::new(core.clone(), "Fluxer microphone", props)?;
    let quit = mainloop.clone();
    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(move |_, data, _, state| {
            if matches!(state, pw::stream::StreamState::Streaming) {
                if let Some(ready) = data.ready.take() {
                    let _ = ready.try_send(Ok(()));
                }
            } else if matches!(state, pw::stream::StreamState::Error(_))
                || (matches!(state, pw::stream::StreamState::Unconnected) && data.ready.is_none())
            {
                data.failed.store(true, Ordering::Release);
                if let Some(ready) = data.ready.take() {
                    let _ = ready.try_send(Err(Error::Unavailable));
                }
                quit.quit();
            }
        })
        .param_changed(|_, data, id, param| {
            if id == spa::param::ParamType::Format.as_raw()
                && let Some(param) = param
                && data.format.parse(param).is_err()
            {
                data.failed.store(true, Ordering::Release);
            }
        })
        .process(|stream, data| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(payload) = buffer.datas_mut().first_mut() else {
                return;
            };
            let offset = payload.chunk().offset() as usize;
            let size = payload.chunk().size() as usize;
            let Some(bytes) = payload.data() else {
                return;
            };
            let Some(samples) = decode_chunk(bytes, offset, size, data.format.channels()) else {
                return;
            };
            let _ = data.frames.try_send(PcmFrame {
                samples,
                sample_rate: data.format.rate(),
                channels: data.format.channels(),
            });
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
        spa::utils::Direction::Input,
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

fn decode_chunk(bytes: &[u8], offset: usize, size: usize, channels: u32) -> Option<Vec<f32>> {
    if !matches!(channels, 1 | 2) || size == 0 || size > 256 * 1024 {
        return None;
    }
    let end = offset.checked_add(size)?.min(bytes.len());
    let raw = bytes.get(offset..end)?;
    let count = raw.len() / 4 / channels as usize * channels as usize;
    if count == 0 {
        return None;
    }
    Some(
        raw.as_chunks::<4>()
            .0
            .iter()
            .take(count)
            .map(|value| {
                let sample = f32::from_le_bytes(*value);
                if sample.is_finite() {
                    sample.clamp(-1., 1.)
                } else {
                    0.
                }
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn logical_microphones_keep_exact_names_and_exclude_playback_mixes() {
        let source = |class: &str, name: &str, label: &str| {
            crate::routing::PropMap::from([
                ("media.class".into(), class.into()),
                ("node.name".into(), name.into()),
                ("node.description".into(), label.into()),
            ])
        };
        let devices = input_devices(vec![
            source("Audio/Source", "goxlr_chat", "GoXLR Chat Mic"),
            source("Audio/Source/Virtual", "goxlr_stream", "GoXLR Stream Mix"),
            source("Audio/Sink", "output", "Speakers"),
        ]);
        assert_eq!(devices.len(), 3);
        assert!(
            devices
                .iter()
                .any(|device| device.id == "input:pipewire:goxlr_chat"
                    && device.label == "GoXLR Chat Mic")
        );
    }
    #[test]
    fn chunk_offsets_channels_bounds_and_nonfinite_samples_are_respected() {
        let bytes = [9., 0.25_f32, -0.75, f32::NAN, f32::INFINITY, 1.]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(
            decode_chunk(&bytes, 4, 16, 2).unwrap(),
            [0.25, -0.75, 0., 0.]
        );
        assert!(decode_chunk(&bytes, usize::MAX, 8, 1).is_none());
        assert!(decode_chunk(&bytes, 0, 3, 1).is_none());
        assert!(decode_chunk(&bytes, 0, 8, 3).is_none());
    }
}
