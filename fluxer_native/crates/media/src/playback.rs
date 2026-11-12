// SPDX-License-Identifier: AGPL-3.0-or-later
//! CPAL handles stay on one dedicated thread. Remote streams remain enabled for
//! decoding while WebRTC platform playout is disabled to prevent double audio.
use crate::{AudioDevice, MediaError, Result, audio::PlaybackMixer};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

enum Command {
    Device(String, mpsc::SyncSender<Result<()>>),
    Start(mpsc::SyncSender<Result<()>>),
    Stop,
    Shutdown,
}
pub(crate) struct PlaybackOutput {
    commands: mpsc::Sender<Command>,
    worker: Option<JoinHandle<()>>,
}
impl PlaybackOutput {
    pub fn new(mixer: Arc<Mutex<PlaybackMixer>>) -> Self {
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut selected: Option<String> = None;
            let mut stream: Option<cpal::Stream> = None;
            while let Ok(command) = rx.recv() {
                match command {
                    Command::Device(id, result) => {
                        let device = find_device(Some(&id));
                        let answer = match device {
                            Ok(device) if stream.is_some() => {
                                match build_stream(&device, mixer.clone()) {
                                    Ok(next) => {
                                        stream = Some(next);
                                        selected = Some(id);
                                        Ok(())
                                    }
                                    Err(e) => Err(e),
                                }
                            }
                            Ok(_) => {
                                selected = Some(id);
                                Ok(())
                            }
                            Err(e) => Err(e),
                        };
                        let _ = result.send(answer);
                    }
                    Command::Start(result) => {
                        let answer = find_device(selected.as_deref())
                            .and_then(|d| build_stream(&d, mixer.clone()));
                        let _ = result.send(match answer {
                            Ok(next) => {
                                stream = Some(next);
                                Ok(())
                            }
                            Err(e) => Err(e),
                        });
                    }
                    Command::Stop => {
                        stream.take();
                    }
                    Command::Shutdown => break,
                }
            }
            drop(stream);
        });
        Self {
            commands: tx,
            worker: Some(worker),
        }
    }
    fn request(&self, make: impl FnOnce(mpsc::SyncSender<Result<()>>) -> Command) -> Result<()> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.commands
            .send(make(tx))
            .map_err(|_| MediaError::Audio("Output worker stopped".into()))?;
        rx.recv_timeout(std::time::Duration::from_secs(3))
            .map_err(|_| MediaError::Audio("Output device did not respond".into()))?
    }
    pub fn start(&self) -> Result<()> {
        self.request(Command::Start)
    }
    pub fn set_device(&self, id: &str) -> Result<()> {
        self.request(|tx| Command::Device(id.into(), tx))
    }
    pub fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
    }
}
impl Drop for PlaybackOutput {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        // Dropping the sender makes all device handles close in their owner
        // thread; no join here because device teardown may block the UI worker.
        self.worker.take();
    }
}
pub(crate) fn devices() -> Vec<AudioDevice> {
    let host = cpal::default_host();
    std::iter::once(AudioDevice {
        id: "output:default".into(),
        label: "System Default".into(),
    })
    .chain(
        host.output_devices()
            .ok()
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(index, device)| {
                let label = device.name().ok()?;
                Some(AudioDevice {
                    id: format!("output:{index}:{label}"),
                    label,
                })
            }),
    )
    .collect()
}
fn find_device(id: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if let Some(id) = id.filter(|id| *id != "output:default") {
        return host
            .output_devices()
            .map_err(|e| MediaError::Audio(e.to_string()))?
            .enumerate()
            .find_map(|(index, device)| {
                let label = device.name().ok()?;
                (format!("output:{index}:{label}") == id).then_some(device)
            })
            .ok_or(MediaError::UnknownDevice("output"));
    }
    host.default_output_device()
        .ok_or_else(|| MediaError::Audio("No default output device".into()))
}
fn build_stream(device: &cpal::Device, mixer: Arc<Mutex<PlaybackMixer>>) -> Result<cpal::Stream> {
    let default = device
        .default_output_config()
        .map_err(|e| MediaError::Audio(e.to_string()))?;
    let format = default.sample_format();
    let config: cpal::StreamConfig = default.into();
    let stream = match format {
        cpal::SampleFormat::F32 => typed_stream::<f32>(device, &config, mixer),
        cpal::SampleFormat::I16 => typed_stream::<i16>(device, &config, mixer),
        cpal::SampleFormat::U16 => typed_stream::<u16>(device, &config, mixer),
        _ => {
            return Err(MediaError::Audio(format!(
                "Unsupported output sample format: {format:?}"
            )));
        }
    }
    .map_err(|e| MediaError::Audio(e.to_string()))?;
    stream
        .play()
        .map_err(|e| MediaError::Audio(e.to_string()))?;
    Ok(stream)
}
fn typed_stream<T: cpal::SizedSample + cpal::FromSample<f32>>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mixer: Arc<Mutex<PlaybackMixer>>,
) -> std::result::Result<cpal::Stream, cpal::BuildStreamError> {
    let channels = config.channels as usize;
    let rate = config.sample_rate.0;
    let errors = mixer.clone();
    device.build_output_stream(
        config,
        move |output: &mut [T], _| {
            let Ok(mut mixer) = mixer.try_lock() else {
                for sample in output {
                    *sample = T::from_sample(0.0);
                }
                return;
            };
            for frame in output.chunks_mut(channels.max(1)) {
                let stereo = mixer.render_stereo(rate);
                for (index, sample) in frame.iter_mut().enumerate() {
                    let value = if channels == 1 {
                        (stereo[0] + stereo[1]) * 0.5
                    } else if index < 2 {
                        stereo[index]
                    } else {
                        0.0
                    };
                    *sample = T::from_sample(value);
                }
            }
        },
        move |error| {
            if let Ok(mixer) = errors.lock()
                && let Some(events) = &mixer.events
            {
                let _ = events.try_send(crate::MediaEvent::Error(format!(
                    "Output device failed; refresh and select a device: {error}"
                )));
            }
        },
        None,
    )
}
