use super::streaming::StatefulOps;
use super::*;

struct TimeBlock {
    mamba: Mamba,
    projection: Tensor<2>,
    bias: Tensor<1>,
    norm_w: Tensor<1>,
    norm_b: Tensor<1>,
    state: Option<(Tensor<2>, Tensor<2>)>,
}

impl TimeBlock {
    fn load(w: &Weights, prefix: &str, device: &Device) -> Result<Self> {
        Ok(Self {
            mamba: Mamba::load(w, &format!("{prefix}.forward_blocks"), device)?,
            projection: linear(w, &format!("{prefix}.output_proj.weight"), device)?,
            bias: tensor(w, &format!("{prefix}.output_proj.bias"), device)?,
            norm_w: tensor(w, &format!("{prefix}.norm.weight"), device)?,
            norm_b: tensor(w, &format!("{prefix}.norm.bias"), device)?,
            state: None,
        })
    }

    fn forward(&mut self, x: Tensor<2>, batch: usize) -> Tensor<2> {
        let m = &self.mamba;
        let rows = x.dims()[0];
        let [di, taps] = m.conv_w.dims();
        let (conv, ssm) = self.state.take().unwrap_or_else(|| {
            (
                Tensor::zeros([batch * (taps - 1), di], &x.device()),
                Tensor::zeros([batch * 16, di], &x.device()),
            )
        });
        assert_eq!(conv.dims(), [batch * (taps - 1), di]);
        let xz = affine(x.clone(), m.in_proj.clone());
        let packed = Tensor::<2>::from_dispatch(<Dispatch as StatefulOps>::state_conv(
            xz.clone().into_dispatch(),
            m.conv_w.clone().into_dispatch(),
            m.conv_b.clone().into_dispatch(),
            batch,
            false,
            conv.into_dispatch(),
        ));
        let conv = packed
            .clone()
            .slice([rows..rows + batch * (taps - 1), 0..di]);
        let xc = packed.slice([0..rows, 0..di]);
        let dbl = affine(xc.clone(), m.x_proj.clone());
        let packed = Tensor::<2>::from_dispatch(<Dispatch as StatefulOps>::state_scan(
            xz.into_dispatch(),
            xc.into_dispatch(),
            dbl.into_dispatch(),
            m.dt_w.clone().into_dispatch(),
            m.dt_b.clone().into_dispatch(),
            m.a.clone().into_dispatch(),
            m.d.clone().into_dispatch(),
            batch,
            false,
            ssm.into_dispatch(),
        ));
        let ssm = packed.clone().slice([rows..rows + batch * 16, 0..di]);
        self.state = Some((conv, ssm));
        let y = affine(packed.slice([0..rows, 0..di]), m.out_proj.clone()) + x;
        let y = affine(y, self.projection.clone()) + self.bias.clone().unsqueeze::<2>();
        layer_norm(y, &self.norm_w, &self.norm_b)
    }
}

pub(crate) struct StreamingModel {
    blocks: Vec<(TimeBlock, BiMamba)>,
    device: Device,
}

impl StreamingModel {
    pub(crate) fn load(w: &Weights, layers: usize) -> Result<Self> {
        let device = GpuDevice::Vulkan.device()?;
        let blocks = (0..layers)
            .map(|i| {
                Ok((
                    TimeBlock::load(w, &format!("TSMamba.{i}.time_mamba"), &device)?,
                    BiMamba::load(w, &format!("TSMamba.{i}.freq_mamba"), &device)?,
                ))
            })
            .collect::<Result<_>>()?;
        Ok(Self { blocks, device })
    }

    pub(crate) fn reset(&mut self) {
        for (time, _) in &mut self.blocks {
            time.state = None;
        }
    }

    pub(crate) fn forward(
        &mut self,
        rows: Vec<f32>,
        time: usize,
        freq: usize,
        channels: usize,
    ) -> Vec<f32> {
        let mut x: Tensor<2> =
            Tensor::from_data(TensorData::new(rows, [time * freq, channels]), &self.device);
        for (t, f) in &mut self.blocks {
            x = x
                .reshape([time, freq, channels])
                .swap_dims(0, 1)
                .reshape([freq * time, channels]);
            x = x.clone() + t.forward(x, freq);
            x = x
                .reshape([freq, time, channels])
                .swap_dims(0, 1)
                .reshape([time * freq, channels]);
            x = x.clone() + f.forward(x, time);
        }
        x.into_data()
            .try_into_vec::<f32>()
            .expect("GPU output is f32")
    }
}
