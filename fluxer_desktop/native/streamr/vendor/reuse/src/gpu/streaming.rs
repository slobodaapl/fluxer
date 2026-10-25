use super::*;
#[backend_extension(Cube, Fusion: cfg(feature = "fused"))]
pub trait StatefulOps: Backend {
    #[fusion(dtype = xz, shape = { let dims = [xz[0] + *batch * (weight[1] - 1), weight[0]]; dims.into() })]
    fn state_conv(
        xz: FloatTensor<Self>,
        weight: FloatTensor<Self>,
        bias: FloatTensor<Self>,
        batch: usize,
        reverse: bool,
        state: FloatTensor<Self>,
    ) -> FloatTensor<Self>;

    #[allow(clippy::too_many_arguments)]
    #[fusion(dtype = xc, shape = { let dims = [xc[0] + *batch * 16, xc[1]]; dims.into() })]
    fn state_scan(
        xz: FloatTensor<Self>,
        xc: FloatTensor<Self>,
        dbl: FloatTensor<Self>,
        dt_w: FloatTensor<Self>,
        dt_b: FloatTensor<Self>,
        a: FloatTensor<Self>,
        d: FloatTensor<Self>,
        batch: usize,
        reverse: bool,
        state: FloatTensor<Self>,
    ) -> FloatTensor<Self>;
}
mod kernel {
    use cubecl::prelude::*;

    #[cube(launch)]
    pub fn state_conv_kernel<F: Float>(
        xz: &Tensor<F>,
        weight: &Tensor<F>,
        bias: &Tensor<F>,
        state: &Tensor<F>,
        out: &mut Tensor<F>,
        rows: u32,
        len: u32,
        d_inner: u32,
        taps: u32,
        _reverse: u32,
        #[define(F)] _dtype: ElemType,
    ) {
        let di = d_inner as usize;
        let (rows, len, hist) = (rows as usize, len as usize, taps as usize - 1);
        let row = ABSOLUTE_POS / di;
        let c = ABSOLUTE_POS % di;
        if row >= rows + rows / len * hist {
            terminate!();
        }
        if row < rows {
            let (seq, time) = (row / len, row % len);
            let mut acc = bias[c];
            for k in 0..hist + 1 {
                let pos = time + k;
                let x = if pos < hist {
                    state[(seq * hist + pos) * di + c]
                } else {
                    xz[(seq * len + pos - hist) * 2 * di + c]
                };
                acc += x * weight[c * (hist + 1) + k];
            }
            out[row * di + c] = acc / (F::new(1.0) + (-acc).exp());
        } else {
            let (seq, time) = ((row - rows) / hist, (row - rows) % hist);
            let pos = len + time;
            if pos < hist {
                out[row * di + c] = state[(seq * hist + pos) * di + c];
            } else {
                out[row * di + c] = xz[(seq * len + pos - hist) * 2 * di + c];
            }
        }
    }
    #[cube(launch)]
    pub fn state_scan_kernel<F: Float>(
        xz: &Tensor<F>,
        xc: &Tensor<F>,
        dbl: &Tensor<F>,
        dt_w: &Tensor<F>,
        dt_b: &Tensor<F>,
        a: &Tensor<F>,
        d: &Tensor<F>,
        state: &Tensor<F>,
        y: &mut Tensor<F>,
        batch: u32,
        len: u32,
        d_inner: u32,
        dt_rank: u32,
        reverse: u32,
        #[define(F)] _dtype: ElemType,
    ) {
        let di = d_inner as usize;
        if ABSOLUTE_POS >= batch as usize * di {
            terminate!();
        }
        let seq = ABSOLUTE_POS / di;
        let c = ABSOLUTE_POS % di;
        let (len, rank) = (len as usize, dt_rank as usize);
        let nd = rank + 32;
        let mut s = Array::new(16usize);
        let mut ac = Array::new(16usize);
        #[unroll]
        for n in 0..16usize {
            s[n] = state[(seq * 16 + n) * di + c];
            ac[n] = a[c * 16 + n];
        }
        let (bias, skip) = (dt_b[c], d[c]);
        for i in 0..len {
            let mut l = i;
            if reverse != 0 {
                l = len - 1 - i;
            }
            let row = seq * len + l;
            let mut raw = bias;
            for k in 0..rank {
                raw += dbl[row * nd + k] * dt_w[c * rank + k];
            }
            // torch softplus, threshold 20.
            let mut delta = raw;
            if raw <= F::new(20.0f32) {
                delta = raw.exp().log1p();
            }
            let x = xc[row * di + c];
            let dx = delta * x;
            let mut acc = F::new(0.0f32);
            #[unroll]
            for n in 0..16usize {
                let decay = (delta * ac[n]).exp();
                s[n] = decay * s[n] + dx * dbl[row * nd + rank + n];
                acc += s[n] * dbl[row * nd + rank + 16 + n];
            }
            let z = xz[row * 2 * di + di + c];
            y[row * di + c] = (acc + skip * x) * (z / (F::new(1.0f32) + (-z).exp()));
        }
        #[unroll]
        for n in 0..16usize {
            y[(batch as usize * len + seq * 16 + n) * di + c] = s[n];
        }
    }
}
use kernel::{state_conv_kernel, state_scan_kernel};
impl StatefulOps for CubeBackend {
    fn state_conv(
        xz: CubeTensor,
        weight: CubeTensor,
        bias: CubeTensor,
        batch: usize,
        reverse: bool,
        state: CubeTensor,
    ) -> CubeTensor {
        let (xz, weight, bias) = (
            into_contiguous(xz),
            into_contiguous(weight),
            into_contiguous(bias),
        );
        let state = into_contiguous(state);
        let rows = xz.meta.shape()[0];
        let (di, taps) = (weight.meta.shape()[0], weight.meta.shape()[1]);
        let xc = empty_device_dtype(
            xz.client.clone(),
            xz.device.clone(),
            [rows + batch * (taps - 1), di].into(),
            xz.dtype,
        );
        let threads = (rows + batch * (taps - 1)) * di;
        let dtype = burn::backend::cubecl::dtype_to_storage_type(xz.dtype);
        let client = xz.client.clone();
        state_conv_kernel::launch(
            &client,
            calculate_cube_count_elemwise(&client, threads, CubeDim::new_1d(BLOCK)),
            CubeDim::new_1d(BLOCK),
            xz.into_tensor_arg(),
            weight.into_tensor_arg(),
            bias.into_tensor_arg(),
            state.into_tensor_arg(),
            xc.clone().into_tensor_arg(),
            rows as u32,
            (rows / batch) as u32,
            di as u32,
            taps as u32,
            reverse as u32,
            dtype,
        );
        xc
    }

    fn state_scan(
        xz: CubeTensor,
        xc: CubeTensor,
        dbl: CubeTensor,
        dt_w: CubeTensor,
        dt_b: CubeTensor,
        a: CubeTensor,
        d: CubeTensor,
        batch: usize,
        reverse: bool,
        state: CubeTensor,
    ) -> CubeTensor {
        let [xz, xc, dbl, dt_w, dt_b, a, d] = [xz, xc, dbl, dt_w, dt_b, a, d].map(into_contiguous);
        let (rows, di) = (xc.meta.shape()[0], xc.meta.shape()[1]);
        let state = into_contiguous(state);
        let rank = dt_w.meta.shape()[1];
        let y = empty_device_dtype(
            xc.client.clone(),
            xc.device.clone(),
            [rows + batch * 16, di].into(),
            xc.dtype,
        );
        let threads = batch * di;
        let dtype = burn::backend::cubecl::dtype_to_storage_type(xc.dtype);
        let client = xc.client.clone();
        state_scan_kernel::launch(
            &client,
            calculate_cube_count_elemwise(&client, threads, CubeDim::new_1d(BLOCK)),
            CubeDim::new_1d(BLOCK),
            xz.into_tensor_arg(),
            xc.into_tensor_arg(),
            dbl.into_tensor_arg(),
            dt_w.into_tensor_arg(),
            dt_b.into_tensor_arg(),
            a.into_tensor_arg(),
            d.into_tensor_arg(),
            state.into_tensor_arg(),
            y.clone().into_tensor_arg(),
            batch as u32,
            (rows / batch) as u32,
            di as u32,
            rank as u32,
            reverse as u32,
            dtype,
        );
        y
    }
}
