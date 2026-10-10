//! `rvm_check <rvm dir> <ratio> <out prefix> <frame.png>...`: runs the frames in order (recurrent state kept) and
//! writes `<prefix>_alpha<i>.png` / `<prefix>_fgr<i>.png` for comparison with the PyTorch reference.
use std::path::Path;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let rvm = filmcraft_ml::rvm::Rvm::load(Path::new(&a[1])).unwrap();
    let ratio: f64 = a[2].parse().unwrap();
    println!("device {:?}", rvm.device());
    let mut st = filmcraft_ml::rvm::State::default();
    for (i, f) in a[4..].iter().enumerate() {
        let im = image::open(f).unwrap().to_rgb8();
        let (w, h) = (im.width() as usize, im.height() as usize);
        let rgb: Vec<f32> = im.as_raw().iter().map(|v| *v as f32 / 255.0).collect();
        let t0 = std::time::Instant::now();
        let (fgr, pha) = rvm.frame(&rgb, h, w, ratio, &mut st).unwrap();
        println!("frame {i}: {w}x{h} ratio {ratio} in {:.3}s", t0.elapsed().as_secs_f64());
        let al: Vec<u8> = pha.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect();
        image::save_buffer(format!("{}_alpha{i}.png", a[3]), &al, w as u32, h as u32, image::ColorType::L8).unwrap();
        let fg: Vec<u8> = fgr.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect();
        image::save_buffer(format!("{}_fgr{i}.png", a[3]), &fg, w as u32, h as u32, image::ColorType::Rgb8).unwrap();
    }
}
