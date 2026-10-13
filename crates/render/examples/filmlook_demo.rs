//! Renders every Film Look effect (defaults) and every Film Look preset on one still, to look at.
//!
//!     cargo run --release -p filmcraft-render --example filmlook_demo -- in.png out_dir [seconds]

use std::path::Path;

use filmcraft_color::{linear_to_srgb, srgb_to_linear};
use filmcraft_project::{ParamValue, find_effect};
use filmcraft_render::Image;
use filmcraft_render::effects::{FxCtx, apply};
use filmcraft_time::Tick;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let (Some(input), Some(out)) = (args.get(1), args.get(2)) else { return Err("usage: filmlook_demo in.png out_dir [seconds]".into()) };
    let seconds: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let rgb = image::open(input).map_err(|e| e.to_string())?.to_rgb8();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    let mut src = Image::new(w, h);
    for (p, o) in rgb.pixels().zip(src.px.chunks_exact_mut(4)) {
        for k in 0..3 {
            o[k] = srgb_to_linear(f32::from(p.0[k]) / 255.0);
        }
        o[3] = 1.0;
    }
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let save = |name: &str, img: &Image| -> Result<(), String> {
        let mut buf = image::RgbImage::new(w as u32, h as u32);
        for (p, o) in img.px.chunks_exact(4).zip(buf.pixels_mut()) {
            for k in 0..3 {
                o.0[k] = (linear_to_srgb(p[k].clamp(0.0, 1.0)) * 255.0 + 0.5) as u8;
            }
        }
        buf.save(Path::new(out).join(format!("{name}.png"))).map_err(|e| e.to_string())
    };
    let cx =
        FxCtx { t: Tick::ZERO, px_scale: 1.0, seconds, timecode: "", clip_name: "", project: None, env: None, working: filmcraft_color::WorkingSpace::Rec709 };
    save("00_original", &src)?;
    for id in ["film_grain", "halation", "bloom", "diffusion", "film_stock", "gate_weave", "film_damage", "lens_character"] {
        let e = find_effect(id).ok_or(id)?.instance();
        let mut img = src.clone();
        apply(&mut img, &e, &cx);
        save(id, &img)?;
    }
    for (i, name) in filmcraft_project::effect::FILM_LOOK_PRESETS.iter().enumerate() {
        let mut e = find_effect("film_look").ok_or("film_look")?.instance();
        if let Some(p) = e.params.get_mut("preset") {
            p.value = ParamValue::Choice(i as u32);
        }
        let mut img = src.clone();
        apply(&mut img, &e, &cx);
        save(&format!("look_{i}_{}", name.replace(' ', "_")), &img)?;
    }
    Ok(())
}
