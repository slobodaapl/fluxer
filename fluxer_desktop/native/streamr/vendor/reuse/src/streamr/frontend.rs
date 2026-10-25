use eyre::{ensure, Result};
use std::{collections::VecDeque, path::Path};

pub struct Frontend {
    decimator: Decimator,
    drained: bool,
}
impl Frontend {
    pub fn load(root: &Path) -> Result<Self> {
        let bytes = std::fs::read(root.join("resampler.f64"))?;
        ensure!(bytes.len() == 385 * 8, "invalid decimator coefficients");
        let taps = bytes
            .chunks_exact(8)
            .map(|x| f64::from_le_bytes(x.try_into().unwrap()))
            .collect();
        Ok(Self {
            decimator: Decimator::new(taps),
            drained: false,
        })
    }
    pub fn process(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        ensure!(!self.drained, "resampler already drained");
        ensure!(
            samples.iter().all(|x| x.is_finite()),
            "nonfinite resampler input"
        );
        Ok(self.decimator.process(samples))
    }
    pub fn flush(&mut self) -> Result<Vec<f32>> {
        ensure!(!self.drained, "resampler already drained");
        self.drained = true;
        Ok(self.decimator.flush())
    }
}

struct Decimator {
    taps: Vec<f64>,
    history: VecDeque<f64>,
    seen: usize,
    produced: usize,
    skip: usize,
}
impl Decimator {
    fn new(taps: Vec<f64>) -> Self {
        Self {
            taps,
            history: VecDeque::from(vec![0.0; 385]),
            seen: 0,
            produced: 0,
            skip: 64,
        }
    }
    fn process(&mut self, values: &[f32]) -> Vec<f32> {
        let mut result = Vec::new();
        for &value in values {
            self.history.pop_back();
            self.history.push_front(f64::from(value));
            if self.seen % 3 == 0 {
                if self.skip > 0 {
                    self.skip -= 1;
                } else {
                    result.push(
                        self.taps
                            .iter()
                            .zip(&self.history)
                            .map(|(a, b)| a * b)
                            .sum::<f64>() as f32,
                    );
                    self.produced += 1;
                }
            }
            self.seen += 1;
        }
        result
    }
    fn flush(&mut self) -> Vec<f32> {
        let owed = self.seen.div_ceil(3) - self.produced;
        let mut output = self.process(&[0.0; 192]);
        assert!(output.len() >= owed);
        output.truncate(owed);
        output
    }
}
