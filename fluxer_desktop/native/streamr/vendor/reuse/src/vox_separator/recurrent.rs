use super::*;
use cubecl::prelude::CubeCount;

#[backend_extension(Cube, Fusion: cfg(feature = "fused"))]
trait SequenceOps: Backend {
    #[fusion(dtype = weight, shape = { let dims = [weight[0], weight[1]]; dims.into() })]
    fn contiguous(weight: FloatTensor<Self>) -> FloatTensor<Self>;

    #[fusion(dtype = projected, shape = { let dims = [projected[0], projected[1] + 2, 256]; dims.into() })]
    fn sequence(
        projected: FloatTensor<Self>,
        weight: FloatTensor<Self>,
        bias: FloatTensor<Self>,
        h: FloatTensor<Self>,
        c: FloatTensor<Self>,
        reverse: bool,
    ) -> FloatTensor<Self>;
}

mod kernel {
    use cubecl::prelude::*;

    #[cube(launch)]
    pub fn sequence<F: Float>(
        projected: &Tensor<F>,
        weight: &Tensor<F>,
        bias: &Tensor<F>,
        h: &Tensor<F>,
        c: &Tensor<F>,
        out: &mut Tensor<F>,
        len: u32,
        reverse: u32,
        #[define(F)] _dtype: ElemType,
    ) {
        let batch = CUBE_POS_X as usize;
        let col = UNIT_POS_X as usize;
        let len = len as usize;
        let mut hidden = Shared::<[F]>::new_slice(256usize);
        hidden[col] = h[batch * 256 + col];
        let mut cell = c[batch * 256 + col];
        sync_cube();
        for step in 0..len {
            let mut time = step;
            if reverse != 0 {
                time = len - 1 - step;
            }
            let base = (batch * len + time) * 1024 + col;
            let mut i = F::new(0.0);
            let mut f = F::new(0.0);
            let mut g = F::new(0.0);
            let mut o = F::new(0.0);
            for k in 0..256usize {
                let value = hidden[k];
                let index = k * 1024 + col;
                i += value * weight[index];
                f += value * weight[index + 256];
                g += value * weight[index + 512];
                o += value * weight[index + 768];
            }
            i = (i + bias[col]) + projected[base];
            f = (f + bias[col + 256]) + projected[base + 256];
            g = (g + bias[col + 512]) + projected[base + 512];
            o = (o + bias[col + 768]) + projected[base + 768];
            i = F::new(1.0) / (F::new(1.0) + (-i).exp());
            f = F::new(1.0) / (F::new(1.0) + (-f).exp());
            o = F::new(1.0) / (F::new(1.0) + (-o).exp());
            cell = f * cell + i * g.tanh();
            let value = o * cell.tanh();
            sync_cube();
            hidden[col] = value;
            out[(batch * (len + 2) + time) * 256 + col] = value;
            sync_cube();
        }
        out[(batch * (len + 2) + len) * 256 + col] = hidden[col];
        out[(batch * (len + 2) + len + 1) * 256 + col] = cell;
    }
}

impl SequenceOps for CubeBackend {
    fn contiguous(weight: CubeTensor) -> CubeTensor {
        into_contiguous(weight)
    }

    fn sequence(
        projected: CubeTensor,
        weight: CubeTensor,
        bias: CubeTensor,
        h: CubeTensor,
        c: CubeTensor,
        reverse: bool,
    ) -> CubeTensor {
        let projected = into_contiguous(projected);
        let weight = into_contiguous(weight);
        let bias = into_contiguous(bias);
        let h = into_contiguous(h);
        let c = into_contiguous(c);
        let shape = projected.meta.shape();
        let (batch, len) = (shape[0], shape[1]);
        let out = empty_device_dtype(
            projected.client.clone(),
            projected.device.clone(),
            [batch, len + 2, 256].into(),
            projected.dtype,
        );
        let client = projected.client.clone();
        let dtype = burn::backend::cubecl::dtype_to_storage_type(projected.dtype);
        kernel::sequence::launch(
            &client,
            CubeCount::Static(batch as u32, 1, 1),
            CubeDim::new_1d(256),
            projected.into_tensor_arg(),
            weight.into_tensor_arg(),
            bias.into_tensor_arg(),
            h.into_tensor_arg(),
            c.into_tensor_arg(),
            out.clone().into_tensor_arg(),
            len as u32,
            u32::from(reverse),
            dtype,
        );
        out
    }
}

pub(super) fn prepare(mut recurrent: Linear) -> Linear {
    recurrent.weight = Tensor::from_dispatch(<Dispatch as SequenceOps>::contiguous(
        recurrent
            .weight
            .cast(burn::tensor::DType::F32)
            .into_dispatch(),
    ));
    recurrent
}

pub(super) fn sequence(
    projected: Tensor<3>,
    recurrent: &Linear,
    h: Tensor<2>,
    c: Tensor<2>,
    reverse: bool,
) -> (Tensor<3>, Tensor<2>, Tensor<2>) {
    let [batch, len, _] = projected.dims();
    let packed = Tensor::<3>::from_dispatch(<Dispatch as SequenceOps>::sequence(
        projected.into_dispatch(),
        recurrent.weight.clone().into_dispatch(),
        recurrent.bias.clone().into_dispatch(),
        h.into_dispatch(),
        c.into_dispatch(),
        reverse,
    ));
    let y = packed.clone().slice([0..batch, 0..len, 0..256]);
    let h = packed
        .clone()
        .slice([0..batch, len..len + 1, 0..256])
        .reshape([batch, 256]);
    let c = packed
        .slice([0..batch, len + 1..len + 2, 0..256])
        .reshape([batch, 256]);
    (y, h, c)
}
