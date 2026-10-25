pub mod frontend;
pub mod threaded;
pub mod vox;

use crate::{Config, Denoiser};
use eyre::{ensure, Result};
use frontend::Frontend;
use std::path::Path;
use vox::Vox;

trait TractResultExt<T> {
    fn eyre(self) -> Result<T>;
}
impl<T> TractResultExt<T> for tract_nnef::prelude::TractResult<T> {
    fn eyre(self) -> Result<T> {
        self.map_err(|error| eyre::eyre!("{error:#}"))
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Engine {
    Cpu,
    #[cfg(feature = "vulkan")]
    Vulkan,
}

pub struct Pipeline {
    frontend: Frontend,
    vox: Vox,
    reuse: Denoiser,
    input_count: usize,
    output_count: usize,
    drained: bool,
}
impl Pipeline {
    fn enrollment(root: &Path, enrollment_48k: &[f32]) -> Result<Vec<f32>> {
        ensure!(
            enrollment_48k.len() == 144000 && enrollment_48k.iter().all(|v| v.is_finite()),
            "expected three-second finite 48kHz enrollment"
        );
        let mut enrollment_filter = Frontend::load(root)?;
        let mut enrollment = enrollment_filter.process(enrollment_48k)?;
        enrollment.extend(enrollment_filter.flush()?);
        ensure!(
            enrollment
                .iter()
                .map(|&x| f64::from(x).powi(2))
                .sum::<f64>()
                > 48000.0 * 1e-7,
            "enrollment has insufficient speech energy after resampling"
        );
        Ok(enrollment)
    }
    fn load_reuse(root: &Path, engine: Engine) -> Result<Denoiser> {
        let config = Config {
            layers: 4,
            sample_rate: 16000,
            look_ahead: 0,
        };
        Ok(match engine {
            Engine::Cpu => Denoiser::with_config(root.join("reuse.safetensors"), config)?,
            #[cfg(feature = "vulkan")]
            Engine::Vulkan => Denoiser::with_vulkan_config(root.join("reuse.safetensors"), config)?,
        })
    }
    pub fn load(root: &Path, enrollment_48k: &[f32], engine: Engine) -> Result<Self> {
        let enrollment = Self::enrollment(root, enrollment_48k)?;
        let vox = Vox::load(root, &enrollment, engine)?;
        let reuse = Self::load_reuse(root, engine)?;
        Ok(Self {
            frontend: Frontend::load(root)?,
            vox,
            reuse,
            input_count: 0,
            output_count: 0,
            drained: false,
        })
    }
    pub fn process(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        ensure!(!self.drained, "pipeline already drained");
        ensure!(samples.iter().all(|x| x.is_finite()), "nonfinite input");
        self.input_count += samples.len();
        let input = self.frontend.process(samples)?;
        let input = self.vox.process(&input)?;
        let output = self.reuse.process(&input)?;
        self.output_count += output.len();
        Ok(output)
    }
    pub fn flush(&mut self) -> Result<Vec<f32>> {
        ensure!(!self.drained, "pipeline already drained");
        self.drained = true;
        let input = self.frontend.flush()?;
        let mut input = self.vox.process(&input)?;
        input.extend(self.vox.flush()?);
        let mut output = self.reuse.process(&input)?;
        output.extend(self.reuse.flush()?);
        ensure!(
            self.output_count + output.len() == self.input_count.div_ceil(3),
            "pipeline sample count mismatch"
        );
        self.output_count += output.len();
        Ok(output)
    }
}
