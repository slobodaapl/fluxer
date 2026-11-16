// SPDX-License-Identifier: AGPL-3.0-or-later
//! Native bindings for the existing client's wasm-bindgen DeepFilterNet3 build.
//! Initialization, warm-up and inference all run on the suppression worker.
use std::sync::OnceLock;
use wasmtime::{
    Caller, Engine, ExternRef, Linker, Memory, Module, Ref, RootScope, Rooted, Store, StoreLimits,
    StoreLimitsBuilder, TypedFunc,
};

const FRAME: usize = 480;
const ATTENUATION_DB: f32 = 30.;
// Match DeepFilter.ts: high-pass at 60 Hz, input x10, output x0.1.
const INPUT_GAIN: f32 = 10.;
const OUTPUT_GAIN: f32 = 0.1;
const WASM: &[u8] = include_bytes!(
    "../../../../../fluxer_app/src/features/voice/utils/noise_suppression/deepfilternet3/df_bg.wasm"
);
const MODEL: &[u8] = include_bytes!(env!("FLUXER_DEEP_FILTER_MODEL_PATH"));

pub(super) struct DeepFilter {
    store: Store<StoreLimits>,
    memory: Memory,
    handle: i32,
    malloc: TypedFunc<(i32, i32), i32>,
    process: TypedFunc<(i32, i32, i32), Option<Rooted<ExternRef>>>,
    input: [u8; FRAME * 4],
    high_pass: HighPass,
}

fn compiled() -> wasmtime::Result<&'static Module> {
    static MODULE: OnceLock<std::result::Result<Module, String>> = OnceLock::new();
    MODULE
        .get_or_init(|| {
            let mut config = wasmtime::Config::new();
            // Persist compiled code in Wasmtime's OS cache so later launches
            // avoid recompiling this large module. Cache failure is harmless:
            // the worker can still compile it while WebRTC remains active.
            if let Ok(cache) = wasmtime::Cache::new(wasmtime::CacheConfig::new()) {
                config.cache(Some(cache));
            }
            Engine::new(&config)
                .and_then(|engine| Module::new(&engine, WASM))
                .map_err(|error| format!("{error:#}"))
        })
        .as_ref()
        .map_err(|error| wasmtime::format_err!(error.clone()))
}

fn memory(caller: &mut Caller<'_, StoreLimits>) -> wasmtime::Result<Memory> {
    caller
        .get_export("memory")
        .and_then(|export| export.into_memory())
        .ok_or_else(|| wasmtime::format_err!("DeepFilterNet memory missing"))
}

fn linker(engine: &Engine) -> wasmtime::Result<Linker<StoreLimits>> {
    let mut linker = Linker::new(engine);
    const MODULE: &str = "./df_bg.js";
    linker.func_wrap(
        MODULE,
        "__wbg___wbindgen_throw_344f42d3211c4765",
        |mut caller: Caller<'_, StoreLimits>, ptr: i32, len: i32| -> wasmtime::Result<()> {
            let memory = memory(&mut caller)?;
            let mut bytes = vec![0; (len as u32 as usize).min(4096)];
            memory.read(&caller, ptr as u32 as usize, &mut bytes)?;
            wasmtime::bail!("DeepFilterNet: {}", String::from_utf8_lossy(&bytes))
        },
    )?;
    linker.func_wrap(
        MODULE,
        "__wbg_getRandomValues_cc7f052a444bb2ce",
        |mut caller: Caller<'_, StoreLimits>, ptr: i32, len: i32| -> wasmtime::Result<()> {
            let memory = memory(&mut caller)?;
            let start = ptr as u32 as usize;
            let end = start
                .checked_add(len as u32 as usize)
                .ok_or_else(|| wasmtime::format_err!("DeepFilterNet random buffer overflow"))?;
            let bytes = memory
                .data_mut(&mut caller)
                .get_mut(start..end)
                .ok_or_else(|| {
                    wasmtime::format_err!("DeepFilterNet random buffer out of bounds")
                })?;
            getrandom::fill(bytes)
                .map_err(|error| wasmtime::format_err!("DeepFilterNet random source: {error}"))
        },
    )?;
    linker.func_wrap(
        MODULE,
        "__wbg_new_from_slice_ddf8b82c4d6af38e",
        |mut caller: Caller<'_, StoreLimits>,
         ptr: i32,
         len: i32|
         -> wasmtime::Result<Option<Rooted<ExternRef>>> {
            wasmtime::ensure!(
                len == FRAME as i32,
                "Unexpected DeepFilterNet output frame length: {len}"
            );
            let memory = memory(&mut caller)?;
            let mut bytes = [0; FRAME * 4];
            memory.read(&caller, ptr as u32 as usize, &mut bytes)?;
            let samples: Vec<f32> = bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect();
            Ok(Some(ExternRef::new(&mut caller, samples)?))
        },
    )?;
    linker.func_wrap(
        MODULE,
        "__wbindgen_init_externref_table",
        // __wbindgen_start re-exports this import directly (function index 3),
        // so Caller has no guest exports when it is invoked from the host.
        // Initialize the table through Instance below, after instantiation.
        || {},
    )?;
    Ok(linker)
}

impl DeepFilter {
    pub fn new() -> wasmtime::Result<Self> {
        Self::with_model(MODEL)
    }
    fn with_model(model: &[u8]) -> wasmtime::Result<Self> {
        let module = compiled()?;
        let mut store = Store::new(
            module.engine(),
            StoreLimitsBuilder::new()
                .memory_size(512 * 1024 * 1024)
                .build(),
        );
        store.limiter(|limits| limits);
        let instance = linker(module.engine())?.instantiate(&mut store, module)?;
        let table = instance
            .get_table(&mut store, "__wbindgen_externrefs")
            .ok_or_else(|| wasmtime::format_err!("DeepFilterNet externref table missing"))?;
        let offset = table.grow(&mut store, 4, Ref::Extern(None))?;
        // Preserve wasm-bindgen's distinct undefined/null reference values.
        let undefined = ExternRef::new(&mut store, ())?;
        table.set(&mut store, 0, Ref::Extern(Some(undefined)))?;
        table.set(&mut store, offset, Ref::Extern(Some(undefined)))?;
        for (index, value) in [(offset + 2, true), (offset + 3, false)] {
            let value = ExternRef::new(&mut store, value)?;
            table.set(&mut store, index, Ref::Extern(Some(value)))?;
        }
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| wasmtime::format_err!("DeepFilterNet memory missing"))?;
        let malloc = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "__wbindgen_malloc_command_export")?;
        let ptr = malloc.call(&mut store, (i32::try_from(model.len())?, 1))?;
        wasmtime::ensure!(ptr != 0, "DeepFilterNet model allocation failed");
        memory.write(&mut store, ptr as u32 as usize, model)?;
        // wasm-bindgen transfers ownership of this allocation to df_create.
        let handle = instance
            .get_typed_func::<(i32, i32, f32), i32>(&mut store, "df_create")?
            .call(&mut store, (ptr, model.len() as i32, ATTENUATION_DB))?;
        wasmtime::ensure!(handle != 0, "DeepFilterNet initialization failed");
        let frame = instance
            .get_typed_func::<i32, i32>(&mut store, "df_get_frame_length")?
            .call(&mut store, handle)?;
        wasmtime::ensure!(
            frame == FRAME as i32,
            "Unexpected DeepFilterNet frame size: {frame}"
        );
        let process = instance.get_typed_func(&mut store, "df_process_frame")?;
        let mut model = Self {
            store,
            memory,
            handle,
            malloc,
            process,
            input: [0; FRAME * 4],
            high_pass: HighPass::new(),
        };
        // Match the web worklet's eight silence frames before exposing output.
        let mut silence = [0; FRAME];
        for _ in 0..8 {
            model.process(&mut silence)?;
            silence.fill(0);
        }
        Ok(model)
    }
    pub fn process(&mut self, pcm: &mut [i16]) -> wasmtime::Result<()> {
        wasmtime::ensure!(
            pcm.len() == FRAME,
            "DeepFilterNet needs an exact 10 ms mono block"
        );
        for (i, sample) in pcm.iter().enumerate() {
            let filtered = self.high_pass.process(*sample as f32 / 32768.);
            self.input[i * 4..i * 4 + 4].copy_from_slice(&(filtered * INPUT_GAIN).to_le_bytes());
        }
        // df_process_frame takes ownership of its input Vec. Allocate each time,
        // as the existing wasm-bindgen glue does, rather than reusing freed PCM.
        let ptr = self.malloc.call(&mut self.store, ((FRAME * 4) as i32, 4))?;
        wasmtime::ensure!(ptr != 0, "DeepFilterNet PCM allocation failed");
        self.memory
            .write(&mut self.store, ptr as u32 as usize, &self.input)?;
        // Per-call roots prevent output Float32Array references accumulating for
        // the lifetime of a call. WASM memory and host arrays stay bounded.
        let mut scope = RootScope::new(&mut self.store);
        let output = self
            .process
            .call(&mut scope, (self.handle, ptr, FRAME as i32))?
            .ok_or_else(|| wasmtime::format_err!("DeepFilterNet returned no audio"))?;
        let samples = output
            .data(&scope)?
            .and_then(|data| data.downcast_ref::<Vec<f32>>())
            .ok_or_else(|| wasmtime::format_err!("DeepFilterNet returned invalid audio"))?;
        wasmtime::ensure!(
            samples.len() == FRAME && samples.iter().all(|s| s.is_finite()),
            "DeepFilterNet produced invalid/non-finite audio"
        );
        for (sample, value) in pcm.iter_mut().zip(samples) {
            *sample = (value * OUTPUT_GAIN * 32768.)
                .round()
                .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        }
        Ok(())
    }
}

/// The same second-order Butterworth high-pass as deepFilterProcessor.worklet.js.
struct HighPass {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}
impl HighPass {
    fn new() -> Self {
        let omega = 2. * std::f32::consts::PI * 60. / 48_000.;
        let alpha = omega.sin() / (2. * std::f32::consts::FRAC_1_SQRT_2);
        let cos = omega.cos();
        let a0 = 1. + alpha;
        Self {
            b0: (1. + cos) / 2. / a0,
            b1: -(1. + cos) / a0,
            b2: (1. + cos) / 2. / a0,
            a1: -2. * cos / a0,
            a2: (1. - alpha) / a0,
            x1: 0.,
            x2: 0.,
            y1: 0.,
            y2: 0.,
        }
    }
    fn process(&mut self, sample: f32) -> f32 {
        let output = self.b0 * sample + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = sample;
        self.y2 = self.y1;
        self.y1 = output;
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_model_and_a_fresh_instance_keeps_processing_without_heap_growth() {
        assert!(DeepFilter::with_model(b"invalid model archive").is_err());
        let mut model = DeepFilter::new().unwrap();
        let initial = model.memory.data_size(&model.store);
        let mut seed = 42_u32;
        for _ in 0..2_000 {
            let mut pcm = [0; FRAME];
            for sample in &mut pcm {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                *sample = (seed >> 16) as i16 / 16;
            }
            model.process(&mut pcm).unwrap();
        }
        model.store.gc(None).unwrap();
        assert!(
            model.memory.data_size(&model.store) <= initial + 8 * 1024 * 1024,
            "DFN PCM ownership must not leak allocations on each frame"
        );
        assert!(model.process(&mut [0; 17]).is_err());
    }
}
