use super::{Engine, TractResultExt};
use eyre::{ensure, Result};
use std::path::Path;
use tract_nnef::prelude::*;

type Plan = TypedRunnableModel<TypedModel>;
fn load(path: impl AsRef<Path>) -> Result<Plan> {
    Ok(tract_nnef::nnef()
        .with_tract_core()
        .model_for_path(path)
        .eyre()?
        .into_optimized()
        .eyre()?
        .into_runnable()
        .eyre()?)
}
enum Backend {
    Cpu {
        model: Plan,
        fixed: TVec<TValue>,
    },
    #[cfg(feature = "vulkan")]
    Vulkan(Box<crate::vox_separator::full::grouped::VoxModel>),
}
pub struct Vox {
    backend: Backend,
    pending: Vec<f32>,
    started: bool,
    ola: Vec<f32>,
    envelope: Vec<f32>,
    window2: Vec<f32>,
    skip: usize,
    seen: usize,
    produced: usize,
}
impl Vox {
    pub fn load(root: &Path, enrollment: &[f32], engine: Engine) -> Result<Self> {
        ensure!(
            enrollment.len() == 48000,
            "expected three-second 16kHz enrollment"
        );
        let encoded = load(root.join("vox_enroll_fp32.nnef"))?
            .run(tvec![Tensor::from_shape(&[1, 48000], enrollment)
                .eyre()?
                .into_tvalue()])
            .eyre()?;
        let cached = load(root.join("vox_cache_fp32.nnef"))?
            .run(encoded)
            .eyre()?;
        ensure!(
            cached.len() == 66,
            "invalid VoxLock conditioning output count"
        );
        let backend = match engine {
            Engine::Cpu => {
                let mut fixed = tvec![
                    Tensor::zero::<f32>(&[1, 1024]).eyre()?.into_tvalue(),
                    Tensor::zero::<f32>(&[6, 32, 256]).eyre()?.into_tvalue(),
                    Tensor::zero::<f32>(&[6, 32, 256]).eyre()?.into_tvalue()
                ];
                fixed.extend(cached);
                Backend::Cpu {
                    model: load(root.join("vox_chunk_fp32.nnef"))?,
                    fixed,
                }
            }
            #[cfg(feature = "vulkan")]
            Engine::Vulkan => {
                use crate::weights::Weights;
                let names: Vec<String> =
                    serde_json::from_slice(&std::fs::read(root.join("vox_cache_names.json"))?)?;
                ensure!(names.len() == cached.len(), "invalid conditioning manifest");
                let mut tensors = std::collections::HashMap::new();
                for i in 0..66 {
                    let name = if i < 2 {
                        names[i].clone()
                    } else {
                        let band = (i - 2) / 2;
                        let prefix = if band == 0 {
                            "multihead_attn".to_owned()
                        } else {
                            format!("multihead_attn_{band}")
                        };
                        let transpose = if i % 2 == 0 { 7 } else { 6 };
                        format!("/attenFuse/{prefix}/Transpose_{transpose}_output_0")
                    };
                    let index = names
                        .iter()
                        .position(|n| *n == name)
                        .ok_or_else(|| eyre::eyre!("missing conditioning tensor {name}"))?;
                    let value = &cached[index];
                    let mut shape = value.shape().to_vec();
                    if i >= 2 {
                        shape.insert(0, 1);
                    }
                    tensors.insert(
                        format!("cache_{i}"),
                        (shape, value.as_slice::<f32>().eyre()?.to_vec()),
                    );
                }
                Backend::Vulkan(Box::new(
                    crate::vox_separator::full::grouped::VoxModel::load(
                        &Weights::load(root.join("vox_chunk_fp32.safetensors"))?,
                        &Weights::from_tensors(tensors),
                    )?,
                ))
            }
        };
        let window2 = (0..512)
            .map(|i| {
                let h = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / 512.0).cos();
                (h * h) as f32
            })
            .collect();
        Ok(Self {
            backend,
            pending: Vec::new(),
            started: false,
            ola: vec![0.0; 512],
            envelope: vec![0.0; 512],
            window2,
            skip: 256,
            seen: 0,
            produced: 0,
        })
    }
    fn chunk(&mut self, input: &[f32]) -> Result<Vec<f32>> {
        match &mut self.backend {
            Backend::Cpu { model, fixed } => {
                fixed[0] = Tensor::from_shape(&[1, 1024], input).eyre()?.into_tvalue();
                let output = model.run(fixed.clone()).eyre()?;
                fixed[1] = output[1].clone();
                fixed[2] = output[2].clone();
                Ok(output[0].as_slice::<f32>().eyre()?.to_vec())
            }
            #[cfg(feature = "vulkan")]
            Backend::Vulkan(model) => Ok(model.forward(input.to_vec(), 1)),
        }
    }
    fn begin(&mut self) {
        let len = self.pending.len();
        let mut prefix = Vec::with_capacity(256 + len);
        for i in (1..=256).rev() {
            let index = if len == 1 {
                0
            } else {
                let period = 2 * (len - 1);
                let offset = i % period;
                offset.min(period - offset)
            };
            prefix.push(self.pending[index]);
        }
        prefix.append(&mut self.pending);
        self.pending = prefix;
        self.started = true;
    }
    fn run(&mut self, final_chunk: bool) -> Result<Vec<f32>> {
        let mut result = Vec::new();
        while self.pending.len() >= if final_chunk { 512 } else { 1024 } {
            let frames = (1 + (self.pending.len() - 512) / 128).min(5);
            let needed = (frames - 1) * 128 + 512;
            let mut input = vec![0.0; 1024];
            input[..needed].copy_from_slice(&self.pending[..needed]);
            let output = self.chunk(&input)?;
            ensure!(
                output.len() == 2560 && output.iter().all(|x| x.is_finite()),
                "invalid VoxLock output"
            );
            let done = frames * 128;
            self.ola.resize(512 + done, 0.0);
            self.envelope.resize(512 + done, 0.0);
            for frame in 0..frames {
                for j in 0..512 {
                    self.ola[frame * 128 + j] += output[frame * 512 + j];
                    self.envelope[frame * 128 + j] += self.window2[j];
                }
            }
            let skip = self.skip.min(done);
            self.skip -= skip;
            result.extend((skip..done).map(|i| {
                -0.5 * self.ola[i]
                    / if self.envelope[i] > 1e-11 {
                        self.envelope[i]
                    } else {
                        1.0
                    }
            }));
            self.ola.drain(..done);
            self.envelope.drain(..done);
            self.pending.drain(..done);
        }
        Ok(result)
    }
    pub fn process(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        self.seen += samples.len();
        self.pending.extend_from_slice(samples);
        if !self.started {
            if self.pending.len() <= 256 {
                return Ok(Vec::new());
            }
            self.begin();
        }
        let result = self.run(false)?;
        self.produced += result.len();
        Ok(result)
    }
    pub fn flush(&mut self) -> Result<Vec<f32>> {
        if self.seen == 0 {
            return Ok(Vec::new());
        }
        if !self.started {
            self.begin();
        }
        let len = self.pending.len();
        let suffix: Vec<_> = self.pending[len - 257..len - 1]
            .iter()
            .rev()
            .copied()
            .collect();
        self.pending.extend(suffix);
        let mut result = self.run(true)?;
        result.extend((self.skip.min(512)..512).map(|i| {
            -0.5 * self.ola[i]
                / if self.envelope[i] > 1e-11 {
                    self.envelope[i]
                } else {
                    1.0
                }
        }));
        let owed = self.seen - self.produced;
        ensure!(result.len() >= owed, "VoxLock flush lost samples");
        result.truncate(owed);
        Ok(result)
    }
}
