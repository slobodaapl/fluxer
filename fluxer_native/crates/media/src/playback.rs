// SPDX-License-Identifier: AGPL-3.0-or-later
//! Speaker handles stay on a dedicated owner thread. Linux targets logical
//! PipeWire sinks; other platforms retain CPAL. WebRTC direct playout is off.
use crate::{AudioDevice, MediaError, Result, audio::PlaybackMixer};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

enum Command {
    Device(String, mpsc::SyncSender<Result<()>>),
    Start(mpsc::SyncSender<Result<()>>),
    Stop(mpsc::SyncSender<Result<()>>),
    Shutdown(mpsc::SyncSender<Result<()>>),
}
pub(crate) struct PlaybackOutput {
    commands: mpsc::Sender<Command>,
    worker: Option<JoinHandle<()>>,
    mixer: Arc<Mutex<PlaybackMixer>>,
}
fn run_worker<S>(
    rx: mpsc::Receiver<Command>,
    validate: impl Fn(Option<&str>) -> Result<()>,
    mut start: impl FnMut(Option<&str>) -> Result<S>,
) {
    let mut selected: Option<String> = None;
    let mut stream: Option<S> = None;
    // Call intent survives a failed live device replacement, so selecting a
    // working device can recover without requiring leave/rejoin.
    let mut enabled = false;
    while let Ok(command) = rx.recv() {
        match command {
            Command::Device(id, result) => {
                let answer = validate(Some(&id)).and_then(|()| {
                    if enabled {
                        // Old output teardown completes before a new consumer
                        // can drain the same mixer.
                        drop(stream.take());
                        stream = Some(start(Some(&id))?);
                    }
                    selected = Some(id);
                    Ok(())
                });
                let _ = result.send(answer);
            }
            Command::Start(result) => {
                drop(stream.take());
                let answer = start(selected.as_deref());
                enabled = answer.is_ok();
                let _ = result.send(match answer {
                    Ok(next) => {
                        stream = Some(next);
                        Ok(())
                    }
                    Err(e) => Err(e),
                });
            }
            Command::Stop(result) => {
                enabled = false;
                drop(stream.take());
                let _ = result.send(Ok(()));
            }
            Command::Shutdown(result) => {
                drop(stream.take());
                let _ = result.send(Ok(()));
                break;
            }
        }
    }
    drop(stream);
}
impl PlaybackOutput {
    pub fn new(mixer: Arc<Mutex<PlaybackMixer>>) -> Self {
        let (tx, rx) = mpsc::channel();
        let output_mixer = mixer.clone();
        let worker = std::thread::spawn(move || {
            run_worker(rx, platform::validate, |id| {
                platform::start(id, output_mixer.clone())
            });
        });
        Self {
            commands: tx,
            worker: Some(worker),
            mixer,
        }
    }
    fn request(&self, make: impl FnOnce(mpsc::SyncSender<Result<()>>) -> Command) -> Result<()> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.commands
            .send(make(tx))
            .map_err(|_| MediaError::Audio("Output worker stopped".into()))?;
        // Enumeration plus opening the exact sink can each take several seconds.
        rx.recv_timeout(std::time::Duration::from_secs(12))
            .map_err(|_| MediaError::Audio("Output device did not respond".into()))?
    }
    pub fn start(&self) -> Result<()> {
        self.request(Command::Start)
    }
    pub fn set_device(&self, id: &str) -> Result<()> {
        self.request(|tx| Command::Device(id.into(), tx))
    }
    pub fn stop(&self) {
        // An acknowledgement means stream teardown finished, including joining
        // the PipeWire owner thread, before reconnect can start another output.
        if let Err(error) = self.request(Command::Stop) {
            report_error(&self.mixer, error.to_string());
        }
    }
}
impl Drop for PlaybackOutput {
    fn drop(&mut self) {
        // Bound a broken driver's teardown wait; a healthy worker has already
        // released its stream when it acknowledges shutdown.
        if self.request(Command::Shutdown).is_ok()
            && let Some(worker) = self.worker.take()
        {
            let _ = worker.join();
        } else {
            self.worker.take();
        }
    }
}
pub(crate) fn devices() -> Result<Vec<AudioDevice>> {
    platform::devices()
}
fn report_error(mixer: &Mutex<PlaybackMixer>, error: String) {
    if let Ok(mixer) = mixer.lock()
        && let Some(events) = &mixer.events
    {
        let _ = events.try_send(crate::MediaEvent::Error(format!(
            "Output device failed; refresh and select a device: {error}"
        )));
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::*;
    pub(super) type Stream = fluxer_system_audio::SpeakerOutput;

    pub(super) fn devices() -> Result<Vec<AudioDevice>> {
        fluxer_system_audio::SpeakerOutput::devices()
            .map(|devices| {
                devices
                    .into_iter()
                    .map(|device| AudioDevice {
                        id: device.id,
                        label: device.label,
                    })
                    .collect()
            })
            .map_err(|error| MediaError::Audio(error.to_string()))
    }
    pub(super) fn validate(id: Option<&str>) -> Result<()> {
        let id = id.unwrap_or("output:default");
        if devices()?.iter().any(|device| device.id == id) {
            Ok(())
        } else {
            Err(MediaError::UnknownDevice("output"))
        }
    }
    pub(super) fn start(id: Option<&str>, mixer: Arc<Mutex<PlaybackMixer>>) -> Result<Stream> {
        let errors = mixer.clone();
        Stream::start(
            id,
            move |output| render_stereo48k(&mixer, output),
            move |error| report_error(&errors, error),
        )
        .map_err(|error| MediaError::Audio(error.to_string()))
    }
}

#[cfg(any(target_os = "linux", test))]
fn render_stereo48k(mixer: &Mutex<PlaybackMixer>, output: &mut [f32]) {
    output.fill(0.0);
    let Ok(mut mixer) = mixer.try_lock() else {
        return;
    };
    for frame in output.as_chunks_mut::<2>().0 {
        frame.copy_from_slice(&mixer.render_stereo(48_000));
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    pub(super) type Stream = cpal::Stream;
    pub(super) fn validate(id: Option<&str>) -> Result<()> {
        find_device(id).map(|_| ())
    }
    pub(super) fn start(id: Option<&str>, mixer: Arc<Mutex<PlaybackMixer>>) -> Result<Stream> {
        build_stream(&find_device(id)?, mixer)
    }
    pub(super) fn devices() -> Result<Vec<AudioDevice>> {
        let host = cpal::default_host();
        Ok(std::iter::once(AudioDevice {
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
        .collect())
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
    fn build_stream(device: &cpal::Device, mixer: Arc<Mutex<PlaybackMixer>>) -> Result<Stream> {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_output_recovers_after_failed_replacement_and_stop_releases_before_ack() {
        struct Output(String, Arc<Mutex<Vec<String>>>);
        impl Drop for Output {
            fn drop(&mut self) {
                self.1.lock().unwrap().push(format!("close:{}", self.0));
            }
        }
        fn request(
            tx: &mpsc::Sender<Command>,
            make: impl FnOnce(mpsc::SyncSender<Result<()>>) -> Command,
        ) -> Result<()> {
            let (answer, result) = mpsc::sync_channel(1);
            tx.send(make(answer)).unwrap();
            result
                .recv_timeout(std::time::Duration::from_secs(1))
                .unwrap()
        }
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            run_worker(
                rx,
                |_| Ok(()),
                |id| {
                    let id = id.unwrap_or("default");
                    observed.lock().unwrap().push(format!("open:{id}"));
                    if id == "broken" {
                        Err(MediaError::Audio("sink disappeared during open".into()))
                    } else {
                        Ok(Output(id.into(), observed.clone()))
                    }
                },
            );
        });
        request(&tx, Command::Start).unwrap();
        assert!(request(&tx, |answer| Command::Device("broken".into(), answer)).is_err());
        request(&tx, |answer| Command::Device("working".into(), answer)).unwrap();
        request(&tx, Command::Stop).unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            [
                "open:default",
                "close:default",
                "open:broken",
                "open:working",
                "close:working"
            ]
        );
        // Choosing a device outside a call records the preference without opening it.
        request(&tx, |answer| Command::Device("idle".into(), answer)).unwrap();
        assert_eq!(events.lock().unwrap().len(), 5);
        request(&tx, Command::Shutdown).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn pipewire_callback_renders_gain_adjusted_stereo_and_echo_reference() {
        let mut mixer = PlaybackMixer::new();
        mixer.master_gain = 0.5;
        mixer.participants.insert(
            "speaker".into(),
            crate::ParticipantAudioSettings {
                gain: 0.5,
                muted: false,
            },
        );
        mixer.add_track("mic".into(), "speaker".into());
        mixer.push("mic", &[16_384, -32_768].repeat(500), 2);
        let mixer = Mutex::new(mixer);
        let mut output = vec![f32::NAN; 964];
        render_stereo48k(&mixer, &mut output);
        assert_eq!(&output[4..6], &[0.125, -0.25]);
        let reference = mixer.lock().unwrap().take_render_reference();
        assert_eq!(reference.len(), 1);
        assert_eq!(reference[0][0], 4096);
        assert_eq!(reference[0][1], -8192);
        mixer.lock().unwrap().deafened = true;
        render_stereo48k(&mixer, &mut output);
        assert!(output.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn pipewire_callback_clears_output_when_mixer_is_busy_and_odd_tail() {
        let mixer = Mutex::new(PlaybackMixer::new());
        let guard = mixer.lock().unwrap();
        let mut output = [1.0; 5];
        render_stereo48k(&mixer, &mut output);
        assert_eq!(output, [0.0; 5]);
        drop(guard);
        output.fill(1.0);
        render_stereo48k(&mixer, &mut output);
        assert_eq!(output, [0.0; 5]);
    }
}
