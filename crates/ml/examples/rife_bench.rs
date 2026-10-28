//! `rife_bench <rife dir> <frame0.png> <frame1.png> <out.png> [t depth]`: the frame at time t (default the midpoint).
use std::path::Path;

use filmcraft_ml::Image;

fn load(p: &str) -> Image {
    let im = image::open(p).unwrap().to_rgb8();
    Image { width: im.width() as usize, height: im.height() as usize, data: im.as_raw().iter().map(|v| *v as f32 / 255.0).collect() }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut rife = filmcraft_ml::rife::Rife::load(Path::new(&a[1])).unwrap();
    rife.scale = std::env::var("RIFE_SCALE").ok().map_or(1.0, |v| v.parse().unwrap());
    println!("device {:?}", rife.device());
    let (f0, f1) = (load(&a[2]), load(&a[3]));
    let t = a.get(5).map_or(0.5, |v| v.parse().unwrap());
    let depth = a.get(6).map_or(1, |v| v.parse().unwrap());
    let mut y = rife.at(&f0, &f1, t, depth).unwrap(); // warm up
    let t0 = std::time::Instant::now();
    for _ in 0..3 {
        y = rife.at(&f0, &f1, t, depth).unwrap();
    }
    println!("{}x{}: {:.3}s per frame at t={t} depth {depth}", y.width, y.height, t0.elapsed().as_secs_f64() / 3.0);
    let out: Vec<u8> = y.data.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect();
    image::save_buffer(&a[4], &out, y.width as u32, y.height as u32, image::ColorType::Rgb8).unwrap();
}
