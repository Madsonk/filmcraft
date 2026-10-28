//! Puts a third-party `.3dl` / `.cube` look next to FilmCraft's own film looks on one still:
//! original | the LUT | Film Stock only | full Film Look. Output is a 2 × 2 contact sheet.
//!
//!     cargo run --release -p filmcraft-render --example lut_compare -- in.png out.png look.3dl <stock 0-8> <preset 0-6> [seconds]

use filmcraft_color::lut::{Lut, LutFormat};
use filmcraft_color::{linear_to_srgb, srgb_to_linear};
use filmcraft_project::{ParamValue, find_effect};
use filmcraft_render::Image;
use filmcraft_render::effects::{FxCtx, apply};
use filmcraft_time::Tick;

fn main() -> Result<(), String> {
    let a: Vec<String> = std::env::args().collect();
    let arg = |i: usize| a.get(i).ok_or_else(|| "usage: lut_compare in.png out.png look.3dl stock preset [seconds]".to_string());
    let rgb = image::open(arg(1)?).map_err(|e| e.to_string())?.to_rgb8();
    let (stock, preset): (u32, u32) = (arg(4)?.parse().map_err(|_| "stock")?, arg(5)?.parse().map_err(|_| "preset")?);
    let seconds: f64 = a.get(6).and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let path = arg(3)?;
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let lut = Lut::parse(&text, LutFormat::from_path(path)).map_err(|e| format!("{path}: {e}"))?;
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    let mut src = Image::new(w, h);
    for (p, o) in rgb.pixels().zip(src.px.as_chunks_mut::<4>().0.iter_mut()) {
        for k in 0..3 {
            o[k] = srgb_to_linear(f32::from(p.0[k]) / 255.0);
        }
        o[3] = 1.0;
    }
    let cx = FxCtx {
        t: Tick::ZERO,
        px_scale: 1.0,
        seconds,
        timecode: "",
        clip_name: "",
        project: None,
        env: None,
        frame_rate: 24.0,
        working: filmcraft_color::WorkingSpace::Rec709,
    };
    // the LUT, applied to the sRGB-encoded picture as the app's own presets expect
    let mut lutted = src.clone();
    for p in lutted.px.as_chunks_mut::<4>().0.iter_mut() {
        let c = lut.apply([linear_to_srgb(p[0]), linear_to_srgb(p[1]), linear_to_srgb(p[2])]);
        for k in 0..3 {
            p[k] = srgb_to_linear(c[k].clamp(0.0, 1.0));
        }
    }
    let mut stocked = src.clone();
    let mut e = find_effect("film_stock").ok_or("film_stock")?.instance();
    if let Some(p) = e.params.get_mut("stock") {
        p.value = ParamValue::Choice(stock);
    }
    apply(&mut stocked, &e, &cx);
    let mut looked = src.clone();
    let mut e = find_effect("film_look").ok_or("film_look")?.instance();
    if let Some(p) = e.params.get_mut("preset") {
        p.value = ParamValue::Choice(preset);
    }
    apply(&mut looked, &e, &cx);
    // 2 × 2 sheet at half size
    let (hw, hh) = (w / 2, h / 2);
    let mut sheet = image::RgbImage::new((hw * 2) as u32, (hh * 2) as u32);
    for (n, img) in [&src, &lutted, &stocked, &looked].into_iter().enumerate() {
        let (ox, oy) = ((n % 2) * hw, (n / 2) * hh);
        for y in 0..hh {
            for x in 0..hw {
                let mut acc = [0.0f32; 3];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let i = ((y * 2 + dy) * w + x * 2 + dx) * 4;
                    for k in 0..3 {
                        acc[k] += img.px.get(i + k).copied().unwrap_or(0.0) * 0.25;
                    }
                }
                let px = acc.map(|v| (linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0 + 0.5) as u8);
                sheet.put_pixel((ox + x) as u32, (oy + y) as u32, image::Rgb(px));
            }
        }
    }
    sheet.save(arg(2)?).map_err(|e| e.to_string())
}
