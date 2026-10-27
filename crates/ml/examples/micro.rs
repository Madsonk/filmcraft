//! Micro-benchmarks of candle ops on the chosen device.
use candle_core::{DType, Device, Tensor};
use std::time::Instant;

fn bench(name: &str, dev: &Device, mut f: impl FnMut() -> Tensor) {
    let _ = f();
    dev.synchronize().unwrap();
    let t0 = Instant::now();
    let n = 5;
    let mut out = None;
    for _ in 0..n {
        out = Some(f());
    }
    dev.synchronize().unwrap();
    let _ = out.unwrap().flatten_all().unwrap().get(0).unwrap().to_scalar::<f32>().unwrap_or(0.0);
    println!("{name}: {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0 / n as f64);
}

fn main() {
    let dev = filmcraft_ml::pick_device();
    println!("{:?}", dev);
    for dt in [DType::F32, DType::F16] {
        let x = Tensor::randn(0f32, 1f32, (1, 90, 180, 320), &dev).unwrap().to_dtype(dt).unwrap();
        let w = Tensor::randn(0f32, 0.05f32, (90, 90, 3, 3), &dev).unwrap().to_dtype(dt).unwrap();
        bench(&format!("conv3x3 90->90 @180x320 {dt:?}"), &dev, || x.conv2d(&w, 1, 1, 1, 1).unwrap());
        let x2 = Tensor::randn(0f32, 1f32, (2, 90, 180, 320), &dev).unwrap().to_dtype(dt).unwrap();
        bench(&format!("conv3x3 90->90 batch2 {dt:?}"), &dev, || x2.conv2d(&w, 1, 1, 1, 1).unwrap());
        let a = Tensor::randn(0f32, 1f32, (4096, 4096), &dev).unwrap().to_dtype(dt).unwrap();
        bench(&format!("matmul 4096^3 {dt:?}"), &dev, || a.matmul(&a).unwrap());
    }
    let x = Tensor::randn(0f32, 1f32, (1, 3, 720, 1280), &dev).unwrap();
    let idx = Tensor::zeros((1, 3, 720 * 1280), DType::U32, &dev).unwrap();
    let flat = x.reshape((1, 3, 720 * 1280)).unwrap();
    bench("gather 3x720x1280", &dev, || flat.gather(&idx, 2).unwrap());
    bench("upsample bilinear 1/4->full x4", &dev, || x.upsample_bilinear2d_with_scale(0.25, 0.25, false).unwrap());
    let k = Tensor::randn(0f32, 0.05f32, (90, 45, 4, 4), &dev).unwrap();
    let y = Tensor::randn(0f32, 1f32, (1, 90, 90, 160), &dev).unwrap();
    bench("convT 90->45 k4 s2 @90x160", &dev, || y.conv_transpose2d(&k, 1, 0, 2, 1).unwrap());
}
