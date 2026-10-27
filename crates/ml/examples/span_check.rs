//! `span_check <span dir> <in.png> <out.png>`: SPAN 2x on one image, timing for several tile sizes.
use std::path::Path;

use filmcraft_mlrestore::{Image, Upscaler};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let up = Upscaler::load(&Path::new(&a[1]).join(filmcraft_mlrestore::files::SPAN2X.file.name)).unwrap();
    let im = image::open(&a[2]).unwrap().to_rgb8();
    let src = Image { width: im.width() as usize, height: im.height() as usize, data: im.as_raw().iter().map(|v| *v as f32 / 255.0).collect() };
    let mean = |d: &[f32]| d.iter().map(|v| *v as f64).sum::<f64>() / d.len() as f64;
    let mut out = None;
    for tile in [128usize, 256, 512, 1024] {
        let mut u = Upscaler::load(&Path::new(&a[1]).join(filmcraft_mlrestore::files::SPAN2X.file.name)).unwrap();
        u.tiling.tile = tile;
        let _ = u.upscale(&src, &mut |_, _| true).unwrap();
        let t0 = std::time::Instant::now();
        let y = u.upscale(&src, &mut |_, _| true).unwrap();
        println!("tile {tile}: {:.2}s  mean in {:.4} out {:.4}", t0.elapsed().as_secs_f64(), mean(&src.data), mean(&y.data));
        out = Some(y);
    }
    let _ = &up;
    let y = out.unwrap();
    let px: Vec<u8> = y.data.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8).collect();
    image::save_buffer(&a[3], &px, y.width as u32, y.height as u32, image::ColorType::Rgb8).unwrap();
}
