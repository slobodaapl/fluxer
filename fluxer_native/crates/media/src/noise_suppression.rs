// SPDX-License-Identifier: AGPL-3.0-or-later
//! The web client's model binaries, running locally on a dedicated DSP worker.
//! Each channel has its own recurrent state. All input/output is 48 kHz PCM.
mod deep_filter;
use deep_filter::DeepFilter;
use serde::{Deserialize, Serialize};
use std::{collections::VecDeque, sync::OnceLock};
use wasmtime::{
    Caller, Engine, Instance, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder,
    TypedFunc,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseSuppressionBackend {
    None,
    #[default]
    Standard,
    Gate,
    Speex,
    Rnnoise,
    Gtcrn,
    #[serde(rename = "deep_filter")]
    DeepFilter,
}
impl NoiseSuppressionBackend {
    pub const ALL: [Self; 7] = [
        Self::None,
        Self::Standard,
        Self::Gate,
        Self::Speex,
        Self::Rnnoise,
        Self::Gtcrn,
        Self::DeepFilter,
    ];
    pub fn id(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Standard => "standard",
            Self::Gate => "gate",
            Self::Speex => "speex",
            Self::Rnnoise => "rnnoise",
            Self::Gtcrn => "gtcrn",
            Self::DeepFilter => "deep_filter",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "Off",
            Self::Standard => "WebRTC",
            Self::Gate => "Noise gate",
            Self::Speex => "Speex",
            Self::Rnnoise => "RNNoise",
            Self::Gtcrn => "GTCRN",
            Self::DeepFilter => "DeepFilterNet",
        }
    }
    pub fn description(self) -> &'static str {
        match self {
            Self::None => "Noise suppression is off.",
            Self::Standard => "WebRTC suppression reduces steady background noise.",
            Self::Gate => {
                "Closes the microphone below a noise threshold; noise during speech remains audible."
            }
            Self::Speex => "SpeexDSP reduces steady background noise locally.",
            Self::Rnnoise => "RNNoise uses a neural model to reduce background noise locally.",
            Self::Gtcrn => {
                "GTCRN uses a neural model to enhance speech locally, with 16 ms of additional buffering."
            }
            Self::DeepFilter => "DeepFilterNet3 uses a neural model to enhance speech locally.",
        }
    }
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|b| b.id() == id)
    }
    pub(crate) fn uses_worker(self) -> bool {
        !matches!(self, Self::None | Self::Standard)
    }
}

type DspResult<T> = std::result::Result<T, String>;
pub(crate) type Preparation = tokio::sync::oneshot::Receiver<DspResult<Vec<i16>>>;
struct Request {
    backend: NoiseSuppressionBackend,
    channels: u32,
    data: Vec<i16>,
    prepare: bool,
    reply: tokio::sync::oneshot::Sender<DspResult<Vec<i16>>>,
}

/// One outstanding block at a time. Dropping the sender releases the worker
/// after its current block, including when the microphone task is aborted.
pub(crate) struct SuppressionWorker(std::sync::mpsc::SyncSender<Request>);
impl SuppressionWorker {
    pub fn new() -> std::io::Result<Self> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Request>(1);
        std::thread::Builder::new()
            .name("fluxer-noise-suppression".into())
            .spawn(move || {
                let mut processor: Option<Suppressor> = None;
                while let Ok(request) = rx.recv() {
                    if request.reply.is_closed() {
                        break;
                    }
                    let result = (|| {
                        if processor.as_ref().is_none_or(|p| {
                            p.backend != request.backend || p.channels != request.channels
                        }) {
                            processor = None;
                            processor = Some(Suppressor::new(request.backend, request.channels)?);
                        }
                        let mut data = request.data;
                        if !request.prepare {
                            processor.as_mut().unwrap().process(&mut data)?;
                        }
                        Ok(data)
                    })();
                    let _ = request.reply.send(result);
                }
            })?;
        Ok(Self(tx))
    }
    pub fn preparing(
        backend: NoiseSuppressionBackend,
        channels: u32,
    ) -> DspResult<(Self, Preparation)> {
        let worker = Self::new().map_err(|error| error.to_string())?;
        let (reply, ready) = tokio::sync::oneshot::channel();
        worker
            .0
            .try_send(Request {
                backend,
                channels,
                data: Vec::new(),
                prepare: true,
                reply,
            })
            .map_err(|_| "Noise suppression worker unavailable".to_owned())?;
        Ok((worker, ready))
    }
    pub async fn process(
        &self,
        backend: NoiseSuppressionBackend,
        channels: u32,
        data: Vec<i16>,
    ) -> DspResult<Vec<i16>> {
        let (reply, result) = tokio::sync::oneshot::channel();
        self.0
            .try_send(Request {
                backend,
                channels,
                data,
                prepare: false,
                reply,
            })
            .map_err(|_| "Noise suppression worker unavailable".to_owned())?;
        result
            .await
            .map_err(|_| "Noise suppression worker stopped".to_owned())?
    }
}

struct Suppressor {
    backend: NoiseSuppressionBackend,
    channels: u32,
    models: Vec<Model>,
    deep_filter: Vec<DeepFilter>,
    gate_hold: usize,
}
impl Suppressor {
    fn new(backend: NoiseSuppressionBackend, channels: u32) -> DspResult<Self> {
        if !(1..=2).contains(&channels) {
            return Err("Noise suppression needs mono or stereo PCM".into());
        }
        let models = if matches!(
            backend,
            NoiseSuppressionBackend::Speex
                | NoiseSuppressionBackend::Rnnoise
                | NoiseSuppressionBackend::Gtcrn
        ) {
            (0..channels)
                .map(|_| Model::new(backend).map_err(|e| format!("{e:#}")))
                .collect::<DspResult<_>>()?
        } else {
            Vec::new()
        };
        let deep_filter = if backend == NoiseSuppressionBackend::DeepFilter {
            (0..channels)
                .map(|_| DeepFilter::new().map_err(|error| format!("{error:#}")))
                .collect::<DspResult<_>>()?
        } else {
            Vec::new()
        };
        Ok(Self {
            backend,
            channels,
            models,
            deep_filter,
            gate_hold: 0,
        })
    }
    fn process(&mut self, data: &mut [i16]) -> DspResult<()> {
        let channels = self.channels as usize;
        if data.len() != 480 * channels {
            return Err("Noise suppression needs an exact 10 ms block".into());
        }
        if self.backend == NoiseSuppressionBackend::Gate {
            // Same hysteresis and 180 ms hold as NoiseSuppressionChain.ts.
            let rms = (data
                .iter()
                .map(|&s| (s as f32 / 32768.).powi(2))
                .sum::<f32>()
                / data.len() as f32)
                .sqrt();
            let threshold = if self.gate_hold == 0 { -41.6 } else { -47.6 };
            if rms > 10_f32.powf(threshold / 20.) {
                self.gate_hold = 18;
            } else {
                self.gate_hold = self.gate_hold.saturating_sub(1);
            }
            if self.gate_hold == 0 {
                data.fill(0);
            }
        }
        for (channel, model) in self.models.iter_mut().enumerate() {
            for sample in data.iter().skip(channel).step_by(channels) {
                model.pending.push_back(*sample);
            }
            model.process_pending().map_err(|e| format!("{e:#}"))?;
            for sample in data.iter_mut().skip(channel).step_by(channels) {
                *sample = model
                    .output
                    .pop_front()
                    .ok_or("Noise suppression output underrun")?;
            }
        }
        for (channel, model) in self.deep_filter.iter_mut().enumerate() {
            let mut mono = [0; 480];
            for (out, input) in mono
                .iter_mut()
                .zip(data.iter().skip(channel).step_by(channels))
            {
                *out = *input;
            }
            model
                .process(&mut mono)
                .map_err(|error| format!("{error:#}"))?;
            for (out, input) in data.iter_mut().skip(channel).step_by(channels).zip(mono) {
                *out = input;
            }
        }
        Ok(())
    }
}

struct Model {
    backend: NoiseSuppressionBackend,
    store: Store<StoreLimits>,
    memory: Memory,
    state: i32,
    input_ptr: i32,
    output_ptr: i32,
    process: ModelCall,
    frame: usize,
    pending: VecDeque<i16>,
    output: VecDeque<i16>,
    bytes: Vec<u8>,
}
enum ModelCall {
    Rnnoise(TypedFunc<(i32, i32, i32), f32>),
    Speex(TypedFunc<(i32, i32), i32>),
    Gtcrn(TypedFunc<(i32, i32, i32), ()>),
}

fn compiled(backend: NoiseSuppressionBackend) -> wasmtime::Result<&'static Module> {
    // Only compiled code is shared; every microphone/channel owns its memory and
    // recurrent state. Compiling happens on the DSP worker, never on GPUI/Tokio.
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    static RNNOISE: OnceLock<std::result::Result<Module, String>> = OnceLock::new();
    static SPEEX: OnceLock<std::result::Result<Module, String>> = OnceLock::new();
    static GTCRN: OnceLock<std::result::Result<Module, String>> = OnceLock::new();
    let (slot, bytes): (_, &[u8]) = match backend {
        NoiseSuppressionBackend::Rnnoise => {
            (&RNNOISE, include_bytes!("../assets/noise/rnnoise.wasm"))
        }
        NoiseSuppressionBackend::Speex => (&SPEEX, include_bytes!("../assets/noise/speex.wasm")),
        NoiseSuppressionBackend::Gtcrn => (&GTCRN, include_bytes!("../assets/noise/gtcrn.wasm")),
        _ => wasmtime::bail!("Backend has no WASM model"),
    };
    slot.get_or_init(|| {
        Module::new(ENGINE.get_or_init(Engine::default), bytes).map_err(|e| format!("{e:#}"))
    })
    .as_ref()
    .map_err(|e| wasmtime::format_err!(e.clone()))
}

fn memory(caller: &mut Caller<'_, StoreLimits>) -> wasmtime::Result<Memory> {
    for name in ["memory", "e", "c"] {
        if let Some(m) = caller.get_export(name).and_then(|e| e.into_memory()) {
            return Ok(m);
        }
    }
    wasmtime::bail!("Model memory missing")
}
fn resize(mut caller: Caller<'_, StoreLimits>, requested: i32) -> wasmtime::Result<i32> {
    let memory = memory(&mut caller)?;
    let current = memory.data_size(&caller);
    let requested = requested as u32 as usize;
    if requested <= current {
        return Ok(1);
    }
    Ok(i32::from(
        memory
            .grow(&mut caller, (requested - current).div_ceil(65536) as u64)
            .is_ok(),
    ))
}
fn assert_failed(_: i32, _: i32, _: i32, _: i32) -> wasmtime::Result<()> {
    wasmtime::bail!("Noise suppression model assertion failed")
}
fn linker(
    engine: &Engine,
    backend: NoiseSuppressionBackend,
) -> wasmtime::Result<Linker<StoreLimits>> {
    let mut linker = Linker::new(engine);
    match backend {
        NoiseSuppressionBackend::Rnnoise => {
            linker.func_wrap("env", "emscripten_resize_heap", resize)?;
            linker.func_wrap("env", "__assert_fail", assert_failed)?;
            linker.func_wrap(
                "env",
                "emscripten_memcpy_big",
                |mut caller: Caller<'_, StoreLimits>,
                 dst: i32,
                 src: i32,
                 len: i32|
                 -> wasmtime::Result<i32> {
                    let memory = memory(&mut caller)?;
                    let data = memory.data_mut(&mut caller);
                    let (dst, src, len) = (
                        dst as u32 as usize,
                        src as u32 as usize,
                        len as u32 as usize,
                    );
                    wasmtime::ensure!(
                        src.checked_add(len).is_some_and(|end| end <= data.len())
                            && dst.checked_add(len).is_some_and(|end| end <= data.len()),
                        "Model memcpy out of bounds"
                    );
                    data.copy_within(src..src + len, dst);
                    Ok(dst as i32)
                },
            )?;
        }
        NoiseSuppressionBackend::Gtcrn => {
            linker.func_wrap("a", "a", assert_failed)?;
            linker.func_wrap("a", "b", resize)?;
        }
        NoiseSuppressionBackend::Speex => {
            linker.func_wrap("a", "d", resize)?;
            // These are the package's fd_close/fd_seek errno returns. No host
            // files or WASI access are exposed to the embedded preprocessor.
            linker.func_wrap("a", "c", |_: i32| -> i32 { 52 })?;
            linker.func_wrap("a", "b", |_: i32, _: i64, _: i32, _: i32| -> i32 { 70 })?;
            linker.func_wrap(
                "a",
                "a",
                |mut caller: Caller<'_, StoreLimits>,
                 _: i32,
                 iov: i32,
                 count: i32,
                 written: i32|
                 -> wasmtime::Result<i32> {
                    let memory = memory(&mut caller)?;
                    let mut total = 0_u32;
                    for i in 0..count as u32 {
                        let mut bytes = [0; 4];
                        memory.read(
                            &caller,
                            iov as u32 as usize + i as usize * 8 + 4,
                            &mut bytes,
                        )?;
                        total = total
                            .checked_add(u32::from_le_bytes(bytes))
                            .ok_or_else(|| wasmtime::format_err!("Model write overflow"))?;
                    }
                    memory.write(&mut caller, written as u32 as usize, &total.to_le_bytes())?;
                    Ok(0)
                },
            )?;
        }
        _ => wasmtime::bail!("Backend has no WASM imports"),
    }
    Ok(linker)
}

impl Model {
    fn new(backend: NoiseSuppressionBackend) -> wasmtime::Result<Self> {
        let module = compiled(backend)?;
        let mut store = Store::new(
            module.engine(),
            StoreLimitsBuilder::new()
                .memory_size(64 * 1024 * 1024)
                .build(),
        );
        store.limiter(|limits| limits);
        let instance: Instance =
            linker(module.engine(), backend)?.instantiate(&mut store, module)?;
        let (memory_name, ctors, malloc_name, frame) = match backend {
            NoiseSuppressionBackend::Rnnoise => ("memory", "__wasm_call_ctors", "malloc", 480),
            NoiseSuppressionBackend::Speex => ("e", "f", "l", 480),
            NoiseSuppressionBackend::Gtcrn => ("c", "d", "k", 768),
            _ => wasmtime::bail!("Backend has no model"),
        };
        instance
            .get_typed_func::<(), ()>(&mut store, ctors)?
            .call(&mut store, ())?;
        let memory = instance
            .get_memory(&mut store, memory_name)
            .ok_or_else(|| wasmtime::format_err!("Model memory missing"))?;
        let malloc = instance.get_typed_func::<i32, i32>(&mut store, malloc_name)?;
        let input_ptr = malloc.call(&mut store, (frame * 4) as i32)?;
        let output_ptr = malloc.call(&mut store, (frame * 4) as i32)?;
        wasmtime::ensure!(
            input_ptr != 0 && output_ptr != 0,
            "Model buffer allocation failed"
        );
        let (state, process) = match backend {
            NoiseSuppressionBackend::Rnnoise => {
                let size = instance
                    .get_typed_func::<(), i32>(&mut store, "rnnoise_get_frame_size")?
                    .call(&mut store, ())?;
                wasmtime::ensure!(size == frame as i32, "Unexpected RNNoise frame size");
                (
                    instance
                        .get_typed_func::<i32, i32>(&mut store, "rnnoise_create")?
                        .call(&mut store, 0)?,
                    ModelCall::Rnnoise(
                        instance.get_typed_func(&mut store, "rnnoise_process_frame")?,
                    ),
                )
            }
            NoiseSuppressionBackend::Speex => {
                let state = instance
                    .get_typed_func::<(i32, i32), i32>(&mut store, "g")?
                    .call(&mut store, (frame as i32, 48_000))?;
                wasmtime::ensure!(state != 0, "Speex state allocation failed");
                memory.write(&mut store, input_ptr as usize, &1_i32.to_le_bytes())?;
                let status = instance
                    .get_typed_func::<(i32, i32, i32), i32>(&mut store, "j")?
                    .call(&mut store, (state, 0, input_ptr))?;
                wasmtime::ensure!(status == 0, "Speex denoise configuration failed");
                (
                    state,
                    ModelCall::Speex(instance.get_typed_func(&mut store, "i")?),
                )
            }
            NoiseSuppressionBackend::Gtcrn => (
                instance
                    .get_typed_func::<(), i32>(&mut store, "e")?
                    .call(&mut store, ())?,
                ModelCall::Gtcrn(instance.get_typed_func(&mut store, "j")?),
            ),
            _ => unreachable!(),
        };
        wasmtime::ensure!(state != 0, "Model state allocation failed");
        // GTCRN takes 768 samples, while capture provides 480. A fixed 768-sample
        // delay keeps output cadence exact, without dropping samples or padding
        // individual model frames with silence. Other backends take 480 directly.
        let output = if frame == 768 {
            VecDeque::from(vec![0; frame])
        } else {
            VecDeque::new()
        };
        Ok(Self {
            backend,
            store,
            memory,
            state,
            input_ptr,
            output_ptr,
            process,
            frame,
            pending: VecDeque::with_capacity(frame + 480),
            output,
            bytes: vec![0; frame * 4],
        })
    }
    fn process_pending(&mut self) -> wasmtime::Result<()> {
        while self.pending.len() >= self.frame {
            let speex = self.backend == NoiseSuppressionBackend::Speex;
            for (i, sample) in self.pending.drain(..self.frame).enumerate() {
                if speex {
                    self.bytes[i * 2..i * 2 + 2].copy_from_slice(&sample.to_le_bytes());
                } else {
                    let value = if self.backend == NoiseSuppressionBackend::Rnnoise {
                        sample as f32
                    } else {
                        sample as f32 / 32768.
                    };
                    self.bytes[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
                }
            }
            self.memory.write(
                &mut self.store,
                self.input_ptr as usize,
                &self.bytes[..self.frame * if speex { 2 } else { 4 }],
            )?;
            match &self.process {
                ModelCall::Rnnoise(f) => {
                    f.call(
                        &mut self.store,
                        (self.state, self.output_ptr, self.input_ptr),
                    )?;
                }
                ModelCall::Speex(f) => {
                    f.call(&mut self.store, (self.state, self.input_ptr))?;
                }
                ModelCall::Gtcrn(f) => {
                    f.call(
                        &mut self.store,
                        (self.state, self.input_ptr, self.output_ptr),
                    )?;
                }
            }
            let ptr = if speex {
                self.input_ptr
            } else {
                self.output_ptr
            };
            self.memory.read(
                &self.store,
                ptr as usize,
                &mut self.bytes[..self.frame * if speex { 2 } else { 4 }],
            )?;
            for i in 0..self.frame {
                let sample = if speex {
                    i16::from_le_bytes(self.bytes[i * 2..i * 2 + 2].try_into()?)
                } else {
                    let value = f32::from_le_bytes(self.bytes[i * 4..i * 4 + 4].try_into()?);
                    wasmtime::ensure!(value.is_finite(), "Model produced non-finite audio");
                    let scale = if self.backend == NoiseSuppressionBackend::Rnnoise {
                        1.
                    } else {
                        32768.
                    };
                    (value * scale)
                        .round()
                        .clamp(i16::MIN as f32, i16::MAX as f32) as i16
                };
                self.output.push_back(sample);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_hysteresis_and_hold_do_not_chop_quiet_speech() {
        let mut gate = Suppressor::new(NoiseSuppressionBackend::Gate, 1).unwrap();
        let mut quiet = vec![50; 480];
        gate.process(&mut quiet).unwrap();
        assert!(quiet.iter().all(|&s| s == 0));
        let mut loud = vec![400; 480];
        gate.process(&mut loud).unwrap();
        assert_eq!(loud, vec![400; 480]);
        // Between close and open thresholds: retain speech while already open.
        let mut speech = vec![200; 480];
        gate.process(&mut speech).unwrap();
        assert_eq!(speech, vec![200; 480]);
        for _ in 0..17 {
            let mut quiet = vec![50; 480];
            gate.process(&mut quiet).unwrap();
            assert_eq!(quiet, vec![50; 480]);
        }
        let mut quiet = vec![50; 480];
        gate.process(&mut quiet).unwrap();
        assert_eq!(quiet, vec![0; 480]);
    }

    #[test]
    fn bundled_models_reduce_noise_with_exact_cadence_and_isolated_stereo() {
        for backend in [
            NoiseSuppressionBackend::Speex,
            NoiseSuppressionBackend::Rnnoise,
            NoiseSuppressionBackend::Gtcrn,
            NoiseSuppressionBackend::DeepFilter,
        ] {
            let mut processor = Suppressor::new(backend, 2).unwrap();
            let mut seed = 12345_u32;
            let (mut input_energy, mut output_energy) = (0_f64, 0_f64);
            for block in 0..200 {
                let mut pcm = Vec::with_capacity(960);
                for _ in 0..480 {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    let noise = ((seed >> 16) as i16) / 8;
                    pcm.extend([noise, 0]);
                    if block >= 100 {
                        input_energy += (noise as f64).powi(2);
                    }
                }
                processor.process(&mut pcm).unwrap();
                assert_eq!(pcm.len(), 960);
                for pair in pcm.as_chunks::<2>().0 {
                    assert!(
                        pair[1].abs() <= 1,
                        "{} leaked into the silent channel: {}",
                        backend.label(),
                        pair[1]
                    );
                    if block >= 100 {
                        output_energy += (pair[0] as f64).powi(2);
                    }
                }
                for model in &processor.models {
                    assert!(model.pending.len() < model.frame);
                    assert!(model.output.len() <= model.frame);
                }
            }
            assert!(
                output_energy < input_energy * 0.8,
                "{} did not reduce stationary noise: {output_energy}/{input_energy}",
                backend.label()
            );
            assert!(
                output_energy > 0.,
                "{} returned only silence",
                backend.label()
            );
        }
    }

    #[tokio::test]
    async fn changing_backend_discards_recurrent_state_and_buffered_audio() {
        let worker = SuppressionWorker::new().unwrap();
        worker
            .process(NoiseSuppressionBackend::Gtcrn, 1, vec![4000; 480])
            .await
            .unwrap();
        let off = worker
            .process(NoiseSuppressionBackend::None, 1, vec![500; 480])
            .await
            .unwrap();
        assert_eq!(off, vec![500; 480]);
        let fresh = worker
            .process(NoiseSuppressionBackend::Gtcrn, 1, vec![0; 480])
            .await
            .unwrap();
        assert_eq!(fresh, vec![0; 480]);
        let stereo = worker
            .process(NoiseSuppressionBackend::Rnnoise, 2, vec![0; 960])
            .await
            .unwrap();
        assert_eq!(stereo, vec![0; 960]);
        assert!(
            worker
                .process(NoiseSuppressionBackend::Rnnoise, 1, vec![0; 17])
                .await
                .is_err()
        );
    }
}
