use super::*;

struct Affine {
    weight: Tensor<3>,
    bias: Tensor<3>,
}
impl Affine {
    fn pack(items: &[&Linear]) -> Self {
        Self {
            weight: Tensor::stack(items.iter().map(|x| x.weight.clone()).collect(), 0),
            bias: Tensor::stack::<2>(items.iter().map(|x| x.bias.clone()).collect(), 0)
                .unsqueeze_dim(1),
        }
    }
    fn forward(&self, x: Tensor<3>) -> Tensor<3> {
        #[cfg(feature = "half-affines")]
        let x = x.cast(burn::tensor::DType::F16);
        let y = x.matmul(self.weight.clone());
        #[cfg(feature = "half-affines")]
        let y = y.cast(burn::tensor::DType::F32);
        y + self.bias.clone()
    }
}
struct BandNorm {
    weight: Tensor<3>,
    bias: Tensor<3>,
}
impl BandNorm {
    fn pack(items: &[&Norm]) -> Self {
        Self {
            weight: Tensor::stack::<2>(items.iter().map(|x| x.weight.clone()).collect(), 0)
                .unsqueeze_dim(1),
            bias: Tensor::stack::<2>(items.iter().map(|x| x.bias.clone()).collect(), 0)
                .unsqueeze_dim(1),
        }
    }
    fn forward(&self, x: Tensor<3>) -> Tensor<3> {
        let mean = x.clone().mean_dim(2);
        let centered = x - mean;
        let variance = centered.clone().square().mean_dim(2);
        centered / (variance + f32::EPSILON).sqrt() * self.weight.clone() + self.bias.clone()
    }
}
struct Group {
    start: usize,
    count: usize,
    width: usize,
    norm: BandNorm,
    projection: Affine,
    mask: Affine,
}
pub struct VoxModel {
    pub separator: Separator,
    stft: Tensor<2>,
    br: Tensor<2>,
    bi: Tensor<2>,
    enrollment: Tensor<3>,
    enrollment_transpose: Tensor<3>,
    keys: Tensor<5>,
    values: Tensor<5>,
    groups: Vec<Group>,
    mask_norm: BandNorm,
    mask_a: Affine,
    mask_b: Affine,
    query: Linear,
    attention_out: Linear,
    fusion: Linear,
}
impl VoxModel {
    pub fn load(w: &Weights, cache: &Weights) -> Result<Self> {
        let super::VoxModel {
            separator,
            stft,
            br,
            bi,
            enrollment,
            enrollment_transpose,
            keys,
            values,
            bands,
            masks,
            query,
            attention_out,
            fusion,
        } = super::VoxModel::load(w, cache)?;
        let mut groups = Vec::new();
        let mut start = 0;
        while start < WIDTHS.len() {
            let width = WIDTHS[start];
            let count = WIDTHS[start..].iter().take_while(|&&w| w == width).count();
            let end = start + count;
            groups.push(Group {
                start,
                count,
                width,
                norm: BandNorm::pack(&bands[start..end].iter().map(|b| &b.0).collect::<Vec<_>>()),
                projection: Affine::pack(
                    &bands[start..end].iter().map(|b| &b.1).collect::<Vec<_>>(),
                ),
                mask: Affine::pack(&masks[start..end].iter().map(|m| &m.3).collect::<Vec<_>>()),
            });
            start = end;
        }
        let mask_norm = BandNorm::pack(&masks.iter().map(|m| &m.0).collect::<Vec<_>>());
        let mask_a = Affine::pack(&masks.iter().map(|m| &m.1).collect::<Vec<_>>());
        let mask_b = Affine::pack(&masks.iter().map(|m| &m.2).collect::<Vec<_>>());
        let keys = Tensor::cat(keys, 0).unsqueeze_dim(1);
        let values = Tensor::cat(values, 0).unsqueeze_dim(1);
        // Finish queued weight packing before a later stage can fail initialization.
        separator.device.sync()?;
        Ok(Self {
            separator,
            stft,
            br,
            bi,
            enrollment,
            enrollment_transpose,
            keys,
            values,
            groups,
            mask_norm,
            mask_a,
            mask_b,
            query,
            attention_out,
            fusion,
        })
    }
    pub fn reset(&mut self) {
        self.separator.reset();
    }
    pub fn forward(&mut self, wav: Vec<f32>, batch: usize) -> Vec<f32> {
        let wav: Tensor<2> =
            Tensor::from_data(TensorData::new(wav, [batch, 1024]), &self.separator.device);
        let windows = (0..5)
            .map(|t| {
                wav.clone()
                    .slice([0..batch, t * 128..t * 128 + 512])
                    .reshape([batch, 1, 512])
            })
            .collect();
        let spec = Tensor::cat(windows, 1)
            .reshape([batch * 5, 512])
            .matmul(self.stft.clone())
            .reshape([batch, 5, 514]);
        let re = spec.clone().slice([0..batch, 0..5, 0..257]);
        let im = spec.slice([0..batch, 0..5, 257..514]);
        let mag = (re.clone().square() + im.clone().square()).sqrt();
        let normalized = mag.clone() / mag.clone().square().sum_dim(2).sqrt().clamp_min(1e-12);
        let tf = softmax(normalized.matmul(self.enrollment.clone()), 2)
            .matmul(self.enrollment_transpose.clone());
        let tf = tf.clone() / tf.square().sum_dim(2).sqrt();
        let tf = (mag * tf.clone()).sum_dim(2) * tf;
        let mut features = Vec::new();
        let mut offset = 0;
        for g in &self.groups {
            let sub = [&re, &im, &tf]
                .into_iter()
                .map(|s| {
                    s.clone()
                        .slice([0..batch, 0..5, offset..offset + g.count * g.width])
                        .reshape([batch, 5, g.count, g.width])
                        .permute([2, 0, 1, 3])
                        .reshape([g.count, batch * 5, g.width])
                })
                .collect();
            features.push(g.projection.forward(g.norm.forward(Tensor::cat(sub, 2))));
            offset += g.count * g.width;
        }
        let features = Tensor::cat(features, 0);
        let query = self
            .query
            .forward(features.clone().reshape([32 * batch * 5, 128]))
            .reshape([32, batch, 5, 2, 64])
            .swap_dims(2, 3);
        let score = softmax((query * 0.125).matmul(self.keys.clone()), 4);
        let context = score
            .matmul(self.values.clone())
            .swap_dims(2, 3)
            .reshape([32 * batch * 5, 128]);
        let context = self
            .fusion
            .forward(self.attention_out.forward(context))
            .reshape([32, batch * 5, 128]);
        let input = (features * context)
            .reshape([32, batch, 5, 128])
            .swap_dims(0, 1);
        let output = self
            .separator
            .forward_tensor(input, batch)
            .permute([2, 0, 1, 3])
            .reshape([32, batch * 5, 128]);
        let hidden = self
            .mask_b
            .forward(self.mask_a.forward(self.mask_norm.forward(output)).tanh())
            .tanh();
        let mut enhanced_re = Vec::new();
        let mut enhanced_im = Vec::new();
        let mut offset = 0;
        for g in &self.groups {
            let mask = g.mask.forward(hidden.clone().slice([
                g.start..g.start + g.count,
                0..batch * 5,
                0..512,
            ]));
            let mr = mask.clone().slice([0..g.count, 0..batch * 5, 0..g.width])
                * sigmoid(
                    mask.clone()
                        .slice([0..g.count, 0..batch * 5, g.width * 2..g.width * 3]),
                );
            let mi = mask
                .clone()
                .slice([0..g.count, 0..batch * 5, g.width..g.width * 2])
                * sigmoid(mask.slice([0..g.count, 0..batch * 5, g.width * 3..g.width * 4]));
            let mr = mr
                .reshape([g.count, batch, 5, g.width])
                .permute([1, 2, 0, 3])
                .reshape([batch, 5, g.count * g.width]);
            let mi = mi
                .reshape([g.count, batch, 5, g.width])
                .permute([1, 2, 0, 3])
                .reshape([batch, 5, g.count * g.width]);
            let r = re
                .clone()
                .slice([0..batch, 0..5, offset..offset + g.count * g.width]);
            let i = im
                .clone()
                .slice([0..batch, 0..5, offset..offset + g.count * g.width]);
            enhanced_re.push(r.clone() * mr.clone() - i.clone() * mi.clone());
            enhanced_im.push(r * mi + i * mr);
            offset += g.count * g.width;
        }
        let frames = Tensor::cat(enhanced_re, 2)
            .reshape([batch * 5, 257])
            .matmul(self.br.clone())
            + Tensor::cat(enhanced_im, 2)
                .reshape([batch * 5, 257])
                .matmul(self.bi.clone());
        frames
            .into_data()
            .try_into_vec::<f32>()
            .expect("FP32 waveform frames")
    }
}
