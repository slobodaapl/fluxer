// SPDX-License-Identifier: AGPL-3.0-or-later
//! Device handles live on their owner thread; settings never open a microphone.
use crate::{AudioDevice, MediaError, MediaEvent, Result, microphone::Pcm};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;
use tokio::sync::mpsc as async_mpsc;

enum Command {
    Enabled(bool, mpsc::SyncSender<Result<()>>),
    Device(
        String,
        Option<Arc<AtomicBool>>,
        mpsc::SyncSender<Result<()>>,
    ),
    Stop(mpsc::SyncSender<Result<()>>),
    Shutdown,
}
#[derive(Clone)]
pub(crate) struct CaptureControl {
    commands: mpsc::Sender<Command>,
    stopped: Arc<AtomicBool>,
}
impl CaptureControl {
    fn request(&self, make: impl FnOnce(mpsc::SyncSender<Result<()>>) -> Command) -> Result<()> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.commands
            .send(make(tx))
            .map_err(|_| MediaError::Audio("Microphone worker stopped".into()))?;
        rx.recv_timeout(Duration::from_secs(5))
            .map_err(|_| MediaError::Audio("Microphone device did not respond".into()))?
    }
    pub fn set_enabled(&self, enabled: bool) -> Result<()> {
        self.request(|tx| Command::Enabled(enabled, tx))
    }
    pub fn set_device(&self, id: &str) -> Result<()> {
        self.request(|tx| Command::Device(id.into(), None, tx))
    }
    pub fn set_device_guarded(&self, id: &str, cancel: Arc<AtomicBool>) -> Result<()> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.commands
            .send(Command::Device(id.into(), Some(cancel.clone()), tx))
            .map_err(|_| MediaError::Audio("Microphone worker stopped".into()))?;
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(result) => result,
            Err(_) => {
                cancel.store(true, Ordering::Release);
                Err(MediaError::Audio(
                    "Microphone device did not respond".into(),
                ))
            }
        }
    }
    pub fn stop(&self) -> Result<()> {
        if self.stopped.load(Ordering::Acquire) {
            return Ok(());
        }
        let answer = self.request(Command::Stop);
        // Another owner (e.g. remote room closure) may already have stopped
        // the worker; repeated teardown succeeds once physical release finished.
        if self.stopped.load(Ordering::Acquire) {
            Ok(())
        } else {
            answer
        }
    }
    pub fn shutdown(&self) {
        let _ = self.commands.send(Command::Shutdown);
    }
}
pub(crate) struct CaptureDevice {
    control: CaptureControl,
}
impl CaptureDevice {
    pub fn controller(&self) -> CaptureControl {
        self.control.clone()
    }
    pub fn stop(&self) -> Result<()> {
        self.control.stop()
    }
    pub fn shutdown(&self) {
        self.control.shutdown()
    }
    pub fn new(
        selected: Option<&str>,
        channels: u32,
        pcm: async_mpsc::Sender<Pcm>,
        events: async_mpsc::Sender<MediaEvent>,
        active: Arc<AtomicBool>,
    ) -> Result<Self> {
        let mut selected = selected.map(String::from);
        if let Some(id) = &selected {
            validate(id)?;
        }
        let (tx, rx) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = stopped.clone();
        std::thread::Builder::new()
            .name("fluxer-microphone".into())
            .spawn(move || {
                let mut source: Option<PlatformInput> = None;
                let mut enabled_intent = false;
                loop {
                    match rx.recv_timeout(Duration::from_millis(3)) {
                        Ok(Command::Enabled(enabled, result)) => {
                            enabled_intent = enabled;
                            let answer = if enabled && source.is_none() {
                                PlatformInput::start(
                                    selected.as_deref(),
                                    channels,
                                    pcm.clone(),
                                    events.clone(),
                                )
                                .map(|next| {
                                    source = Some(next);
                                })
                            } else {
                                if !enabled {
                                    source.take();
                                }
                                Ok(())
                            };
                            active.store(source.is_some(), Ordering::Release);
                            let _ = result.send(answer);
                        }
                        Ok(Command::Device(id, cancel, result)) => {
                            let answer = validate(&id).and_then(|()| {
                                switch_source(
                                    &mut source,
                                    enabled_intent,
                                    cancel.as_deref(),
                                    || {
                                        PlatformInput::start(
                                            Some(&id),
                                            channels,
                                            pcm.clone(),
                                            events.clone(),
                                        )
                                    },
                                )?;
                                selected = Some(id);
                                Ok(())
                            });
                            active.store(source.is_some(), Ordering::Release);
                            let _ = result.send(answer);
                        }
                        Ok(Command::Stop(result)) => {
                            release_input(&mut source, &active);
                            worker_stopped.store(true, Ordering::Release);
                            // The ACK happens after native stream destruction,
                            // never merely after accepting the command.
                            let _ = result.send(Ok(()));
                            break;
                        }
                        Ok(Command::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => (),
                    }
                    if pcm.is_closed() || events.is_closed() {
                        break;
                    }
                    if let Some(input) = &source
                        && let Err(error) = input.pump(&pcm)
                    {
                        let _ = error;
                        let _ = events.try_send(MediaEvent::InputDeviceFailed);
                        source.take();
                        active.store(false, Ordering::Release);
                    }
                }
                release_input(&mut source, &active);
                worker_stopped.store(true, Ordering::Release);
            })
            .map_err(|e| MediaError::Audio(e.to_string()))?;
        Ok(Self {
            control: CaptureControl {
                commands: tx,
                stopped,
            },
        })
    }
}
impl Drop for CaptureDevice {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn switch_source<S>(
    source: &mut Option<S>,
    enabled: bool,
    cancel: Option<&AtomicBool>,
    start: impl FnOnce() -> Result<S>,
) -> Result<()> {
    let cancelled = || cancel.is_some_and(|c| c.load(Ordering::Acquire));
    if cancelled() {
        return Err(MediaError::Audio("Device change cancelled".into()));
    }
    if enabled {
        // The old native source must be gone before opening another one.
        source.take();
        let next = start()?;
        if cancelled() {
            return Err(MediaError::Audio("Device change cancelled".into()));
        }
        *source = Some(next);
    }
    Ok(())
}
fn release_input<T>(source: &mut Option<T>, active: &AtomicBool) {
    source.take();
    active.store(false, Ordering::Release);
}
fn validate(id: &str) -> Result<()> {
    if devices()?.iter().any(|d| d.id == id) {
        Ok(())
    } else {
        Err(MediaError::UnknownDevice("input"))
    }
}
#[cfg(target_os = "linux")]
pub(crate) fn devices() -> Result<Vec<AudioDevice>> {
    fluxer_system_audio::MicrophoneInput::devices()
        .map(|items| {
            items
                .into_iter()
                .map(|d| AudioDevice {
                    id: d.id,
                    label: d.label,
                })
                .collect()
        })
        .map_err(|e| MediaError::Audio(e.to_string()))
}
#[cfg(target_os = "linux")]
struct PlatformInput(fluxer_system_audio::MicrophoneInput);
#[cfg(target_os = "linux")]
impl PlatformInput {
    fn start(
        id: Option<&str>,
        channels: u32,
        _pcm: async_mpsc::Sender<Pcm>,
        _events: async_mpsc::Sender<MediaEvent>,
    ) -> Result<Self> {
        fluxer_system_audio::MicrophoneInput::start(id, channels)
            .map(Self)
            .map_err(|e| MediaError::Audio(e.to_string()))
    }
    fn pump(&self, tx: &async_mpsc::Sender<Pcm>) -> Result<()> {
        // Bound each pass so a busy device cannot starve stop/device commands.
        for _ in 0..16 {
            let Some(frame) = self
                .0
                .read()
                .map_err(|e| MediaError::Audio(e.to_string()))?
            else {
                break;
            };
            let data = frame
                .samples
                .into_iter()
                .map(|v| (v.clamp(-1.0, 1.0) * 32767.0).round() as i16)
                .collect();
            let _ = tx.try_send(Pcm {
                data,
                sample_rate: frame.sample_rate,
                channels: frame.channels,
            });
        }
        Ok(())
    }
}
#[cfg(not(target_os = "linux"))]
#[path = "microphone_cpal.rs"]
mod platform;
#[cfg(not(target_os = "linux"))]
use platform::PlatformInput;
#[cfg(not(target_os = "linux"))]
pub(crate) use platform::devices;

#[cfg(test)]
mod tests {
    use super::*;
    struct RecordingSource(Arc<AtomicBool>);
    impl Drop for RecordingSource {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    #[test]
    fn cancelled_capture_replacement_releases_both_sources_and_idle_never_opens() {
        let live = Arc::new(AtomicBool::new(true));
        let mut source = Some(RecordingSource(live.clone()));
        let cancel = AtomicBool::new(false);
        let result = switch_source(&mut source, true, Some(&cancel), || {
            assert!(
                !live.load(Ordering::Acquire),
                "old source must be released first"
            );
            live.store(true, Ordering::Release);
            cancel.store(true, Ordering::Release);
            Ok(RecordingSource(live.clone()))
        });
        assert!(result.is_err());
        assert!(source.is_none());
        assert!(!live.load(Ordering::Acquire));
        let idle_cancel = AtomicBool::new(false);
        switch_source(
            &mut source,
            false,
            Some(&idle_cancel),
            || -> Result<RecordingSource> { panic!("idle selection opened capture") },
        )
        .unwrap();
        assert!(source.is_none());
    }

    #[test]
    fn stop_acknowledges_only_after_device_owner_releases_capture() {
        let (commands, rx) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let control = CaptureControl {
            commands,
            stopped: stopped.clone(),
        };
        let recording = Arc::new(AtomicBool::new(true));
        let active = Arc::new(AtomicBool::new(true));
        let device_recording = recording.clone();
        let device_active = active.clone();
        let owner = std::thread::spawn(move || {
            let mut source = Some(RecordingSource(device_recording));
            if let Command::Stop(result) = rx.recv().unwrap() {
                release_input(&mut source, &device_active);
                stopped.store(true, Ordering::Release);
                result.send(Ok(())).unwrap();
            } else {
                panic!("expected explicit stop request");
            }
        });
        control.stop().unwrap();
        assert!(!recording.load(Ordering::Acquire));
        assert!(!active.load(Ordering::Acquire));
        owner.join().unwrap();
        control.stop().expect("repeated stop is idempotent");
    }
}
