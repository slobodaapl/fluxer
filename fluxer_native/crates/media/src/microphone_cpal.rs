// SPDX-License-Identifier: AGPL-3.0-or-later
//! Native input backend for platforms without the Linux PipeWire source picker.
use super::*;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
pub(crate) fn devices() -> Result<Vec<AudioDevice>> {
    let host = cpal::default_host();
    Ok(std::iter::once(AudioDevice {
        id: "input:default".into(),
        label: "System Default".into(),
    })
    .chain(
        host.input_devices()
            .map_err(|e| MediaError::Audio(e.to_string()))?
            .enumerate()
            .filter_map(|(index, d)| {
                let label = d.name().ok()?;
                Some(AudioDevice {
                    id: format!("input:{index}:{label}"),
                    label,
                })
            }),
    )
    .collect())
}
fn find(id: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if let Some(id) = id.filter(|id| *id != "input:default") {
        return host
            .input_devices()
            .map_err(|e| MediaError::Audio(e.to_string()))?
            .enumerate()
            .find_map(|(i, d)| {
                let label = d.name().ok()?;
                (format!("input:{i}:{label}") == id).then_some(d)
            })
            .ok_or(MediaError::UnknownDevice("input"));
    }
    host.default_input_device()
        .ok_or_else(|| MediaError::Audio("No default microphone device".into()))
}
pub(super) struct PlatformInput {
    _stream: cpal::Stream,
}
impl PlatformInput {
    pub(super) fn start(
        id: Option<&str>,
        _channels: u32,
        pcm: async_mpsc::Sender<Pcm>,
        events: async_mpsc::Sender<MediaEvent>,
    ) -> Result<Self> {
        let device = find(id)?;
        let default = device
            .default_input_config()
            .map_err(|e| MediaError::Audio(e.to_string()))?;
        let format = default.sample_format();
        let config: cpal::StreamConfig = default.into();
        if config.channels > 2 || config.channels == 0 || !config.sample_rate.0.is_multiple_of(100)
        {
            return Err(MediaError::Audio(
                "Microphone must provide mono or stereo PCM at a standard audio sample rate".into(),
            ));
        }
        let stream = match format {
            cpal::SampleFormat::F32 => typed::<f32>(&device, &config, pcm, events),
            cpal::SampleFormat::I16 => typed::<i16>(&device, &config, pcm, events),
            cpal::SampleFormat::U16 => typed::<u16>(&device, &config, pcm, events),
            _ => {
                return Err(MediaError::Audio(format!(
                    "Unsupported microphone sample format: {format:?}"
                )));
            }
        }
        .map_err(|e| MediaError::Audio(e.to_string()))?;
        stream
            .play()
            .map_err(|e| MediaError::Audio(e.to_string()))?;
        Ok(Self { _stream: stream })
    }
    pub(super) fn pump(&self, _tx: &async_mpsc::Sender<Pcm>) -> Result<()> {
        Ok(())
    }
}
fn typed<T: cpal::SizedSample>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    pcm: async_mpsc::Sender<Pcm>,
    events: async_mpsc::Sender<MediaEvent>,
) -> std::result::Result<cpal::Stream, cpal::BuildStreamError>
where
    i16: cpal::FromSample<T>,
{
    let rate = config.sample_rate.0;
    let channels = u32::from(config.channels);
    device.build_input_stream(
        config,
        move |data: &[T], _| {
            use cpal::Sample;
            let data = data.iter().map(|v| v.to_sample::<i16>()).collect();
            let _ = pcm.try_send(Pcm {
                data,
                sample_rate: rate,
                channels,
            });
        },
        move |error| {
            let _ = events.try_send(MediaEvent::Error(format!(
                "Microphone device failed; refresh and select a device: {error}"
            )));
        },
        None,
    )
}
