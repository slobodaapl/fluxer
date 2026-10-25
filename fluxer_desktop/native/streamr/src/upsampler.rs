// SPDX-License-Identifier: AGPL-3.0-or-later
use eyre::{ensure, Result};
use std::{collections::VecDeque, path::Path};

pub struct Upsampler {
    taps: Vec<f64>,
    history: VecDeque<f64>,
    skip: usize,
}
impl Upsampler {
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        ensure!(bytes.len() == 385 * 8, "invalid resampler coefficients");
        let taps: Vec<_> = bytes
            .chunks_exact(8)
            .map(|x| f64::from_le_bytes(x.try_into().unwrap()))
            .collect();
        ensure!(
            taps.iter().all(|x| x.is_finite()),
            "nonfinite resampler coefficients"
        );
        Ok(Self {
            taps,
            history: VecDeque::from(vec![0.0; 129]),
            skip: 192,
        })
    }
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        let mut output = Vec::with_capacity(samples.len() * 3);
        for &sample in samples {
            self.history.pop_back();
            self.history.push_front(f64::from(sample));
            for phase in 0..3 {
                if self.skip > 0 {
                    self.skip -= 1;
                    continue;
                }
                let value: f64 = self
                    .taps
                    .iter()
                    .skip(phase)
                    .step_by(3)
                    .zip(&self.history)
                    .map(|(h, x)| h * x)
                    .sum();
                output.push((value * 3.0) as f32);
            }
        }
        output
    }
}
