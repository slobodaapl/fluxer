use super::*;
use burn::tensor::activation::{sigmoid, softmax};

#[cfg(feature = "grouped-vox")]
#[path = "grouped.rs"]
pub mod grouped;

const WIDTHS: [usize; 32] = [
    3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 16, 16, 16, 16, 16,
    64, 8,
];

fn affine_parts(weight: Tensor<2>, bias: Tensor<1>) -> Linear {
    #[cfg(feature = "half-affines")]
    let weight = weight.cast(burn::tensor::DType::F16);
    Linear { weight, bias }
}
fn conv(w: &Weights, p: &str, device: &Device) -> Result<Linear> {
    let v = tensor::<3>(w, &format!("{p}.weight"), device)?;
    let [out, input, kernel] = v.dims();
    assert_eq!(kernel, 1);
    Ok(affine_parts(
        v.reshape([out, input]).transpose(),
        tensor(w, &format!("{p}.bias"), device)?,
    ))
}
fn norm(w: &Weights, p: &str, device: &Device) -> Result<Norm> {
    let mut norm = Norm::load(w, p, device)?;
    norm.eps = f32::EPSILON;
    Ok(norm)
}

pub struct VoxModel {
    pub separator: Separator,
    stft: Tensor<2>,
    br: Tensor<2>,
    bi: Tensor<2>,
    enrollment: Tensor<3>,
    enrollment_transpose: Tensor<3>,
    keys: Vec<Tensor<4>>,
    values: Vec<Tensor<4>>,
    bands: Vec<(Norm, Linear)>,
    masks: Vec<(Norm, Linear, Linear, Linear)>,
    query: Linear,
    attention_out: Linear,
    fusion: Linear,
}
impl VoxModel {
    pub fn load(w: &Weights, cache: &Weights) -> Result<Self> {
        let separator = Separator::load(w)?;
        let device = &separator.device;
        let mut bands = Vec::new();
        let mut masks = Vec::new();
        let mut keys = Vec::new();
        let mut values = Vec::new();
        for band in 0..32 {
            let p = format!("t.sep_model.subband_norm.BN.{band}");
            bands.push((
                norm(w, &format!("{p}.0"), device)?,
                conv(w, &format!("{p}.1"), device)?,
            ));
            let p = format!("t.sep_model.band_masker.mask.{band}");
            masks.push((
                norm(w, &format!("{p}.0"), device)?,
                conv(w, &format!("{p}.1"), device)?,
                conv(w, &format!("{p}.3"), device)?,
                conv(w, &format!("{p}.5"), device)?,
            ));
            keys.push(tensor(cache, &format!("cache_{}", 2 + band * 2), device)?);
            values.push(tensor(cache, &format!("cache_{}", 3 + band * 2), device)?);
        }
        let p = "t.spk_ft.context.attenFuse.attention";
        #[allow(clippy::single_range_in_vec_init)]
        let query = affine_parts(
            tensor::<2>(w, &format!("{p}.in_proj_weight"), device)?
                .slice([0..128, 0..128])
                .transpose(),
            tensor::<1>(w, &format!("{p}.in_proj_bias"), device)?.slice([0..128]),
        );
        let attention_out = Linear::load(
            w,
            &format!("{p}.out_proj.weight"),
            &format!("{p}.out_proj.bias"),
            device,
        )?;
        let p = "t.spk_ft.context.fusionLayer.fc.linear";
        let fusion = Linear::load(w, &format!("{p}.weight"), &format!("{p}.bias"), device)?;
        let stft = tensor::<3>(w, "stft.w", device)?
            .reshape([514, 512])
            .transpose();
        let br = tensor(w, "istft.br", device)?;
        let bi = tensor(w, "istft.bi", device)?;
        let enrollment = tensor(cache, "cache_0", device)?;
        let enrollment_transpose = tensor(cache, "cache_1", device)?;
        Ok(Self {
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
        let score = softmax(normalized.matmul(self.enrollment.clone()), 2);
        let tf = score.matmul(self.enrollment_transpose.clone());
        let tf = tf.clone() / tf.square().sum_dim(2).sqrt();
        let tf = (mag * tf.clone()).sum_dim(2) * tf;
        let mut features = Vec::new();
        let mut offset = 0;
        for (band, &width) in WIDTHS.iter().enumerate() {
            let bounds = [0..batch, 0..5, offset..offset + width];
            let sub = Tensor::cat(
                vec![
                    re.clone().slice(bounds.clone()),
                    im.clone().slice(bounds.clone()),
                    tf.clone().slice(bounds),
                ],
                2,
            );
            let (norm, projection) = &self.bands[band];
            let x = projection
                .forward(norm.forward(sub, false).reshape([batch * 5, width * 3]))
                .reshape([batch, 5, 128]);
            let query = self
                .query
                .forward(x.clone().reshape([batch * 5, 128]))
                .reshape([batch, 5, 2, 64])
                .swap_dims(1, 2);
            let score = softmax((query * 0.125).matmul(self.keys[band].clone()), 3);
            let context = score
                .matmul(self.values[band].clone())
                .swap_dims(1, 2)
                .reshape([batch * 5, 128]);
            let context = self
                .fusion
                .forward(self.attention_out.forward(context))
                .reshape([batch, 5, 128]);
            features.push((x * context).reshape([batch, 1, 5, 128]));
            offset += width;
        }
        let output = self
            .separator
            .forward_tensor(Tensor::cat(features, 1), batch);
        let mut enhanced_re = Vec::new();
        let mut enhanced_im = Vec::new();
        let mut offset = 0;
        for (band, &width) in WIDTHS.iter().enumerate() {
            let x = output
                .clone()
                .slice([0..batch, 0..5, band..band + 1, 0..128])
                .reshape([batch, 5, 128]);
            let (norm, a, b, c) = &self.masks[band];
            let x = norm.forward(x, false).reshape([batch * 5, 128]);
            let mask =
                c.forward(b.forward(a.forward(x).tanh()).tanh())
                    .reshape([batch, 5, width * 4]);
            let mr = mask.clone().slice([0..batch, 0..5, 0..width])
                * sigmoid(mask.clone().slice([0..batch, 0..5, width * 2..width * 3]));
            let mi = mask.clone().slice([0..batch, 0..5, width..width * 2])
                * sigmoid(mask.slice([0..batch, 0..5, width * 3..width * 4]));
            let r = re.clone().slice([0..batch, 0..5, offset..offset + width]);
            let i = im.clone().slice([0..batch, 0..5, offset..offset + width]);
            enhanced_re.push(r.clone() * mr.clone() - i.clone() * mi.clone());
            enhanced_im.push(r * mi + i * mr);
            offset += width;
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
