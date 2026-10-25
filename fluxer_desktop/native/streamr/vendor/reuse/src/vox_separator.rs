use crate::weights::Weights;
use burn::backend::{backend_extension, tensor::FloatTensor, Backend, Dispatch};
use burn::tensor::{Device, DeviceKind, Tensor, TensorData};
use burn_cubecl::{
    kernel::into_contiguous, ops::numeric::empty_device_dtype, tensor::CubeTensor, CubeBackend,
};
#[cfg(not(feature = "persistent-lstm"))]
use cubecl::calculate_cube_count_elemwise;
use cubecl::prelude::CubeDim;
use eyre::Result;

#[cfg(feature = "persistent-lstm")]
#[path = "vox_separator/recurrent.rs"]
mod recurrent;

#[cfg(not(feature = "persistent-lstm"))]
#[backend_extension(Cube, Fusion: cfg(feature = "fused"))]
trait LstmOps: Backend {
    #[fusion(dtype = cell, shape = { let dims = [cell[0], cell[1] * 2]; dims.into() })]
    fn update(gates: FloatTensor<Self>, cell: FloatTensor<Self>) -> FloatTensor<Self>;
}

#[cfg(not(feature = "persistent-lstm"))]
mod kernel {
    use cubecl::prelude::*;

    #[cube(launch)]
    pub fn update<F: Float>(
        gates: &Tensor<F>,
        cell: &Tensor<F>,
        out: &mut Tensor<F>,
        batch: u32,
        hidden: u32,
        #[define(F)] _dtype: ElemType,
    ) {
        let hidden = hidden as usize;
        let pos = ABSOLUTE_POS;
        if pos >= batch as usize * hidden {
            terminate!();
        }
        let (row, col) = (pos / hidden, pos % hidden);
        let base = row * hidden * 4 + col;
        let i = F::new(1.0) / (F::new(1.0) + (-gates[base]).exp());
        let f = F::new(1.0) / (F::new(1.0) + (-gates[base + hidden]).exp());
        let g = gates[base + hidden * 2].tanh();
        let o = F::new(1.0) / (F::new(1.0) + (-gates[base + hidden * 3]).exp());
        let c = f * cell[pos] + i * g;
        out[row * hidden * 2 + col] = o * c.tanh();
        out[row * hidden * 2 + hidden + col] = c;
    }
}

#[cfg(not(feature = "persistent-lstm"))]
impl LstmOps for CubeBackend {
    fn update(gates: CubeTensor, cell: CubeTensor) -> CubeTensor {
        let gates = into_contiguous(gates);
        let cell = into_contiguous(cell);
        let (batch, hidden) = (cell.meta.shape()[0], cell.meta.shape()[1]);
        let out = empty_device_dtype(
            cell.client.clone(),
            cell.device.clone(),
            [batch, hidden * 2].into(),
            cell.dtype,
        );
        let client = cell.client.clone();
        let dtype = burn::backend::cubecl::dtype_to_storage_type(cell.dtype);
        let dim = CubeDim::new_1d(256);
        kernel::update::launch(
            &client,
            calculate_cube_count_elemwise(&client, batch * hidden, dim),
            dim,
            gates.into_tensor_arg(),
            cell.into_tensor_arg(),
            out.clone().into_tensor_arg(),
            batch as u32,
            hidden as u32,
            dtype,
        );
        out
    }
}

fn tensor<const D: usize>(weights: &Weights, name: &str, device: &Device) -> Result<Tensor<D>> {
    let dims: [usize; D] = weights
        .shape(name)?
        .try_into()
        .map_err(|_| eyre::eyre!("wrong rank: {name}"))?;
    Ok(Tensor::from_data(
        TensorData::new(weights.get(name)?, dims),
        device,
    ))
}

struct Linear {
    weight: Tensor<2>,
    bias: Tensor<1>,
}
impl Linear {
    fn load(w: &Weights, weight: &str, bias: &str, device: &Device) -> Result<Self> {
        let weight = tensor::<2>(w, weight, device)?.transpose();
        #[cfg(feature = "half-affines")]
        let weight = weight.cast(burn::tensor::DType::F16);
        Ok(Self {
            weight,
            bias: tensor(w, bias, device)?,
        })
    }
    fn forward(&self, x: Tensor<2>) -> Tensor<2> {
        #[cfg(feature = "half-affines")]
        let x = x.cast(burn::tensor::DType::F16);
        let y = x.matmul(self.weight.clone());
        #[cfg(feature = "half-affines")]
        let y = y.cast(burn::tensor::DType::F32);
        y + self.bias.clone().unsqueeze::<2>()
    }
}

struct Norm {
    weight: Tensor<1>,
    bias: Tensor<1>,
    eps: f32,
}
impl Norm {
    fn load(w: &Weights, p: &str, device: &Device) -> Result<Self> {
        Ok(Self {
            weight: tensor(w, &format!("{p}.weight"), device)?,
            bias: tensor(w, &format!("{p}.bias"), device)?,
            eps: 1e-5,
        })
    }
    fn forward(&self, x: Tensor<3>, group: bool) -> Tensor<3> {
        let mut mean = x.clone().mean_dim(2);
        if group {
            mean = mean.mean_dim(1);
        }
        let centered = x - mean;
        let mut variance = centered.clone().square().mean_dim(2);
        if group {
            variance = variance.mean_dim(1);
        }
        centered / (variance + self.eps).sqrt() * self.weight.clone().unsqueeze::<3>()
            + self.bias.clone().unsqueeze::<3>()
    }
}

struct Lstm {
    input: Linear,
    recurrent: Linear,
}
impl Lstm {
    fn load(w: &Weights, p: &str, reverse: bool, device: &Device) -> Result<Self> {
        let suffix = if reverse { "_reverse" } else { "" };
        let recurrent = Linear::load(
            w,
            &format!("{p}.weight_hh_l0{suffix}"),
            &format!("{p}.bias_hh_l0{suffix}"),
            device,
        )?;
        #[cfg(feature = "persistent-lstm")]
        let recurrent = recurrent::prepare(recurrent);
        Ok(Self {
            input: Linear::load(
                w,
                &format!("{p}.weight_ih_l0{suffix}"),
                &format!("{p}.bias_ih_l0{suffix}"),
                device,
            )?,
            recurrent,
        })
    }
    fn forward(
        &self,
        x: Tensor<3>,
        state: Option<(Tensor<2>, Tensor<2>)>,
        reverse: bool,
    ) -> (Tensor<3>, Tensor<2>, Tensor<2>) {
        let [batch, len, width] = x.dims();
        let device = x.device();
        let projected = self
            .input
            .forward(x.reshape([batch * len, width]))
            .reshape([batch, len, 1024]);
        let (h, c) = state.unwrap_or_else(|| {
            (
                Tensor::zeros([batch, 256], &device),
                Tensor::zeros([batch, 256], &device),
            )
        });
        #[cfg(feature = "persistent-lstm")]
        {
            recurrent::sequence(projected, &self.recurrent, h, c, reverse)
        }
        #[cfg(not(feature = "persistent-lstm"))]
        {
            let (mut h, mut c) = (h, c);
            let mut outputs = Vec::with_capacity(len);
            for step in 0..len {
                let t = if reverse { len - 1 - step } else { step };
                let gates = projected
                    .clone()
                    .slice([0..batch, t..t + 1, 0..1024])
                    .reshape([batch, 1024])
                    + self.recurrent.forward(h);
                let packed = Tensor::<2>::from_dispatch(<Dispatch as LstmOps>::update(
                    gates.into_dispatch(),
                    c.into_dispatch(),
                ));
                h = packed.clone().slice([0..batch, 0..256]);
                c = packed.slice([0..batch, 256..512]);
                outputs.push(h.clone().reshape([batch, 1, 256]));
            }
            if reverse {
                outputs.reverse();
            }
            (Tensor::cat(outputs, 1), h, c)
        }
    }
}

struct Block {
    time_norm: Norm,
    time: Lstm,
    time_proj: Linear,
    band_norm: Norm,
    forward: Lstm,
    reverse: Lstm,
    band_proj: Linear,
    state: Option<(Tensor<2>, Tensor<2>)>,
}

pub struct Separator {
    blocks: Vec<Block>,
    device: Device,
}
impl Separator {
    pub fn load(w: &Weights) -> Result<Self> {
        let device = Device::vulkan(DeviceKind::DefaultDevice);
        let mut blocks = Vec::new();
        for index in 0..6 {
            let t = format!("{index}.band_rnn");
            let b = format!("{index}.band_comm");
            blocks.push(Block {
                time_norm: Norm::load(w, &format!("{t}.norm"), &device)?,
                time: Lstm::load(w, &format!("{t}.rnn"), false, &device)?,
                time_proj: Linear::load(
                    w,
                    &format!("{t}.proj.weight"),
                    &format!("{t}.proj.bias"),
                    &device,
                )?,
                band_norm: Norm::load(w, &format!("{b}.norm"), &device)?,
                forward: Lstm::load(w, &format!("{b}.rnn"), false, &device)?,
                reverse: Lstm::load(w, &format!("{b}.rnn"), true, &device)?,
                band_proj: Linear::load(
                    w,
                    &format!("{b}.proj.weight"),
                    &format!("{b}.proj.bias"),
                    &device,
                )?,
                state: None,
            });
        }
        Ok(Self { blocks, device })
    }
    pub fn reset(&mut self) {
        for b in &mut self.blocks {
            b.state = None;
        }
    }
    pub fn forward(&mut self, input: Vec<f32>, chunks: usize) -> Vec<f32> {
        assert!(chunks > 0);
        let x: Tensor<4> =
            Tensor::from_data(TensorData::new(input, [chunks, 32, 5, 128]), &self.device);
        self.forward_tensor(x, chunks)
            .into_data()
            .try_into_vec::<f32>()
            .expect("FP32 separator output")
    }
    fn forward_tensor(&mut self, x: Tensor<4>, chunks: usize) -> Tensor<4> {
        let len = chunks * 5;
        let mut x = x.swap_dims(0, 1).reshape([32, len, 128]);
        for block in &mut self.blocks {
            let normalized = block.time_norm.forward(x.clone(), false);
            let (y, h, c) = block.time.forward(normalized, block.state.take(), false);
            block.state = Some((h, c));
            x = x + block
                .time_proj
                .forward(y.reshape([32 * len, 256]))
                .reshape([32, len, 128]);
            let original = x.swap_dims(0, 1);
            let normalized = block.band_norm.forward(original.clone(), true);
            let (fw, _, _) = block.forward.forward(normalized.clone(), None, false);
            let (bw, _, _) = block.reverse.forward(normalized, None, true);
            let y = Tensor::cat(vec![fw, bw], 2).reshape([len * 32, 512]);
            x = (original + block.band_proj.forward(y).reshape([len, 32, 128])).swap_dims(0, 1);
        }
        x.reshape([32, chunks, 5, 128])
            .swap_dims(0, 1)
            .swap_dims(1, 2)
    }
    pub fn state(&self) -> (Vec<f32>, Vec<f32>) {
        let mut hs = Vec::new();
        let mut cs = Vec::new();
        for block in &self.blocks {
            let (h, c) = block.state.as_ref().expect("state after inference");
            hs.extend(h.clone().into_data().try_into_vec::<f32>().expect("FP32 h"));
            cs.extend(c.clone().into_data().try_into_vec::<f32>().expect("FP32 c"));
        }
        (hs, cs)
    }
}

#[path = "vox_separator/full.rs"]
#[allow(dead_code)]
pub mod full;
