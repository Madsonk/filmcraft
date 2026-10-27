//! The Film Look bin: the character of photochemical film as video effects.
//!
//! * **Film Grain** — exposure-dependent (shadows / mids / highlights), clumped (two octaves of
//!   smooth noise plus a pixel-level "rough" term), partly colour-correlated, re-rolled every
//!   film frame; sized in full-resolution clip pixels so previews and exports match.
//! * **Halation** — light from bright areas scattering back through the film base: a wide,
//!   red-orange glow around highlights.
//! * **Bloom** — energy-spreading glow from a mip pyramid (soft threshold, scatter), with an
//!   optional anamorphic streak.
//! * **Diffusion Filter** — a Pro-Mist-style veil: softens and lifts around highlights.
//! * **Film Stock** — tone curve (S-curve, fade, shoulder), dye cross-talk, split toning and
//!   highlight desaturation as named recipes plus a few trims.
//! * **Gate Weave & Flicker** — per-frame jitter, tiny rotation, breathing and exposure flicker.
//! * **Dust & Scratches** — procedural specks, hairs and vertical scratches that come and go.
//! * **Lens Character** — lateral chromatic aberration and corner softness.
//! * **Film Look** — all of the above in one effect with seven presets and one Amount slider.
//!
//! Conventions as in the rest of [`super`]: premultiplied linear f32 images; spatial parameters
//! are full-resolution clip pixels scaled by `FxCtx::px_scale`; the tonal maths artists expect in
//! display space (grain, stock) runs on sRGB-encoded straight colour and never clips HDR values.

use filmcraft_color::{linear_to_srgb, srgb_to_linear};

use super::lights::{add_image, blurred, tint};
use super::*;
use crate::effects::gaussian;

// ------------------------------------------------------------------ shared helpers

#[inline]
fn frame_index(seconds: f64, fps: f32) -> i64 {
    (seconds.max(0.0) * f64::from(fps.clamp(1.0, 240.0))).floor() as i64
}

#[inline]
fn px_at(img: &Image, x: usize, y: usize, k: usize) -> f32 {
    img.px.get((y * img.w + x) * 4 + k).copied().unwrap_or(0.0)
}

/// sRGB-encoded straight colour of a premultiplied linear pixel (`None` when transparent).
#[inline]
fn display(p: &[f32]) -> Option<(f32, [f32; 3])> {
    let a = *p.get(3)?;
    if a <= 1e-6 {
        return None;
    }
    let c = |k: usize| linear_to_srgb((p.get(k).copied().unwrap_or(0.0) / a).max(0.0));
    Some((a, [c(0), c(1), c(2)]))
}

/// Write an sRGB-encoded straight colour back as a premultiplied linear pixel (HDR kept).
#[inline]
fn store(p: &mut [f32], a: f32, c: [f32; 3]) {
    for (k, v) in c.iter().enumerate() {
        if let Some(d) = p.get_mut(k) {
            *d = srgb_to_linear(v.max(0.0)) * a;
        }
    }
}

/// Halve an image with a 2 × 2 box filter.
fn down2(src: &Image) -> Image {
    let (w, h) = ((src.w / 2).max(1), (src.h / 2).max(1));
    let mut out = Image::new(w, h);
    out.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for (x, o) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x0, y0) = ((x * 2).min(src.w.saturating_sub(1)), (y * 2).min(src.h.saturating_sub(1)));
            let (x1, y1) = ((x0 + 1).min(src.w.saturating_sub(1)), (y0 + 1).min(src.h.saturating_sub(1)));
            for (k, v) in o.iter_mut().enumerate() {
                *v = 0.25 * (px_at(src, x0, y0, k) + px_at(src, x1, y0, k) + px_at(src, x0, y1, k) + px_at(src, x1, y1, k));
            }
        }
    });
    out
}

/// Resample `src` (≥ 1 px) to `w × h` with bilinear filtering.
fn up_to(src: &Image, w: usize, h: usize) -> Image {
    if src.w == w && src.h == h {
        return src.clone();
    }
    src.transformed(w, h, &Affine::scale(w as f64 / src.w.max(1) as f64, h as f64 / src.h.max(1) as f64))
}

fn zero_like(img: &Image) -> Image {
    Image::new(img.w, img.h)
}

// ------------------------------------------------------------------ Film Grain

#[derive(Clone, Copy)]
struct Grain {
    amount: f32,
    size: f32,
    rough: f32,
    shadows: f32,
    mids: f32,
    highs: f32,
    chroma: f32,
    animated: bool,
    fps: f32,
    seed: u64,
}

fn grain_params(e: &EffectInstance, cx: &FxCtx) -> Grain {
    Grain {
        amount: fv(e, "amount", cx).max(0.0) / 100.0,
        size: fv(e, "size", cx).clamp(0.2, 24.0),
        rough: (fv(e, "roughness", cx) / 100.0).clamp(0.0, 1.0),
        shadows: fv(e, "shadows", cx).max(0.0) / 100.0,
        mids: fv(e, "mids", cx).max(0.0) / 100.0,
        highs: fv(e, "highs", cx).max(0.0) / 100.0,
        chroma: (fv(e, "chroma", cx) / 100.0).clamp(0.0, 1.0),
        animated: bv(e, "animated"),
        fps: fv(e, "frame_rate", cx),
        seed: fv(e, "seed", cx).max(0.0) as u64,
    }
}

/// Unit-variance (approximately Gaussian) value of lattice point (ix, iy) for one film frame.
#[inline]
fn lattice(ix: i64, iy: i64, frame: i64, chan: u64, seed: u64) -> f32 {
    let h = hash_u(ix, iy, frame, seed ^ chan.wrapping_mul(0x9E37_79B9));
    let u = |s: u32| ((h >> s) & 0x1F_FFFF) as f32 / 2_097_152.0;
    (u(0) + u(21) + u(42) - 1.5) * 2.0
}

/// Smooth lattice noise (smoothstep-interpolated), variance ≈ 0.552.
#[inline]
fn vnoise(x: f32, y: f32, frame: i64, chan: u64, seed: u64) -> f32 {
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let (ix, iy) = (x0 as i64, y0 as i64);
    let v = |dx: i64, dy: i64| lattice(ix.wrapping_add(dx), iy.wrapping_add(dy), frame, chan, seed);
    let top = v(0, 0) + (v(1, 0) - v(0, 0)) * sx;
    let bot = v(0, 1) + (v(1, 1) - v(0, 1)) * sx;
    top + (bot - top) * sy
}

/// Exposure → grain strength: the shadows / mids / highlights responses, blended smoothly.
#[inline]
fn grain_response(l: f32, g: &Grain) -> f32 {
    let ws = 1.0 - smoothstep(0.0, 0.5, l);
    let wh = smoothstep(0.5, 1.0, l);
    let wm = (1.0 - ws - wh).max(0.0);
    // pure black carries no grain
    let floor = smoothstep(0.0, 0.04, l).sqrt();
    (ws * g.shadows + wm * g.mids + wh * g.highs) * floor
}

/// Share of the standard deviation of smooth lattice noise of spacing `l` clip pixels that
/// survives an n×n box average, relative to full resolution (n = 1 → exactly 1). The raw curve
/// `1 / √(1 + α u²)`, u = n / l, falls as 1/u once the box spans many lattice cells (their count
/// grows as u²); α is fitted on a box-filtered full-resolution render (see the Film Look preview
/// tests in `filmlook_preview_tests.rs`).
#[inline]
fn preview_retention(n: f32, l: f32) -> f32 {
    const ALPHA: f32 = 1.5;
    let raw = |u: f32| (1.0 + ALPHA * u * u).sqrt().recip();
    let l = l.max(0.3);
    raw(n / l) / raw(1.0 / l)
}

fn grain_core(img: &mut Image, g: &Grain, seconds: f64, px_scale: f32) {
    if g.amount <= 0.0 || img.w == 0 {
        return;
    }
    const W1: f32 = 0.85;
    const W2: f32 = 0.5;
    const W3: f32 = 0.6;
    let scale = (g.size * px_scale).max(0.3);
    let frame = if g.animated { frame_index(seconds, g.fps) } else { 0 };
    let norm = 1.0 / (0.552 * (W1 * W1 + W2 * W2) + W3 * W3 * g.rough * g.rough).sqrt();
    // Below full resolution (Program monitor at 1/2, 1/4…) one working pixel stands for n×n clip
    // pixels: the grain must look like the full-resolution grain filtered down, not like
    // full-strength grain on bigger pixels. Each octave keeps the share of its deviation that
    // survives an n×n box average (`preview_retention`); the per-pixel term keeps 1/n.
    let n = (1.0 / px_scale.clamp(1e-3, 1.0)).max(1.0);
    let (w1, w2) = (W1 * preview_retention(n, g.size), W2 * preview_retention(n, g.size / 2.3));
    let w3 = W3 * g.rough / n;
    let (k_luma, k_chroma) = ((1.0 - g.chroma).sqrt(), g.chroma.sqrt());
    let field = |x: f32, y: f32, chan: u64| -> f32 {
        let (fx, fy) = (x / scale, y / scale);
        let o1 = vnoise(fx, fy, frame, chan, g.seed);
        let o2 = vnoise(fx * 2.3 + 17.0, fy * 2.3 + 5.0, frame, chan + 11, g.seed);
        let fine = lattice(x as i64, y as i64, frame, chan + 23, g.seed);
        (o1 * w1 + o2 * w2 + fine * w3) * norm
    };
    let w = img.w;
    img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for (x, p) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let Some((a, c)) = display(p) else { continue };
            let amp = grain_response(luma(c), g) * g.amount * 0.05;
            if amp <= 0.0 {
                continue;
            }
            let (fx, fy) = (x as f32, y as f32);
            let n_luma = field(fx, fy, 0);
            let mut o = c;
            for (i, v) in o.iter_mut().enumerate() {
                let n = k_luma * n_luma + if k_chroma > 0.0 { k_chroma * field(fx, fy, 1 + i as u64) } else { 0.0 };
                *v += n * amp;
            }
            store(p, a, o);
        }
    });
}

/// Film Grain.
pub fn film_grain(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    grain_core(img, &grain_params(e, cx), cx.seconds, cx.px_scale);
}

// ------------------------------------------------------------------ Halation

#[derive(Clone, Copy)]
struct Halation {
    threshold: f32,
    softness: f32,
    radius: f32,
    spread: f32,
    intensity: f32,
    color: [f32; 3],
    strength: f32,
    screen: bool,
    only: bool,
}

fn halation_params(e: &EffectInstance, cx: &FxCtx) -> Halation {
    Halation {
        threshold: fv(e, "threshold", cx) / 100.0,
        softness: fv(e, "softness", cx) / 100.0,
        radius: fv(e, "radius", cx).max(0.0),
        spread: (fv(e, "spread", cx) / 100.0).clamp(0.0, 1.0),
        intensity: fv(e, "intensity", cx).max(0.0) / 100.0,
        color: lin(cv(e, "color", cx)),
        strength: (fv(e, "tint_strength", cx) / 100.0).clamp(0.0, 1.0),
        screen: chv(e, "mode") == 1,
        only: bv(e, "only"),
    }
}

fn halation_core(img: &mut Image, h: &Halation, px_scale: f32) {
    if h.intensity <= 0.0 || h.radius <= 0.0 || img.w == 0 {
        return;
    }
    let mut hi = highlights(img, h.threshold, h.softness.max(0.02));
    tint(&mut hi, h.color, h.strength);
    let r = (h.radius * px_scale).max(0.5);
    let (inner, outer) = rayon::join(|| blurred(&hi, r * 0.5), || blurred(&hi, r * (1.0 + 1.5 * h.spread)));
    let (wi, wo) = (0.6 * (1.0 - 0.5 * h.spread), 0.4 * (0.5 + h.spread));
    let mut glow = zero_like(img);
    glow.px.par_iter_mut().zip(inner.px.par_iter().zip(outer.px.par_iter())).for_each(|(g, (i, o))| *g = i * wi + o * wo);
    if h.only {
        *img = zero_like(img);
    }
    add_image(img, &glow, [h.intensity * 1.2; 3], h.screen);
}

/// Halation.
pub fn halation(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    halation_core(img, &halation_params(e, cx), cx.px_scale);
}

// ------------------------------------------------------------------ Bloom

#[derive(Clone, Copy)]
struct Bloom {
    threshold: f32,
    softness: f32,
    scatter: f32,
    radius: f32,
    intensity: f32,
    color: [f32; 3],
    strength: f32,
    streak: f32,
    streak_len: f32,
    streak_color: [f32; 3],
    screen: bool,
    only: bool,
}

fn bloom_params(e: &EffectInstance, cx: &FxCtx) -> Bloom {
    Bloom {
        threshold: fv(e, "threshold", cx) / 100.0,
        softness: fv(e, "softness", cx) / 100.0,
        scatter: (fv(e, "scatter", cx) / 100.0).clamp(0.0, 1.0),
        radius: fv(e, "radius", cx).max(0.0),
        intensity: fv(e, "intensity", cx).max(0.0) / 100.0,
        color: lin(cv(e, "color", cx)),
        strength: (fv(e, "tint_strength", cx) / 100.0).clamp(0.0, 1.0),
        streak: (fv(e, "streak", cx) / 100.0).clamp(0.0, 1.0),
        streak_len: fv(e, "streak_length", cx).max(0.0),
        streak_color: lin(cv(e, "streak_color", cx)),
        screen: chv(e, "mode") == 1,
        only: bv(e, "only"),
    }
}

fn bloom_core(img: &mut Image, b: &Bloom, px_scale: f32) {
    if b.intensity <= 0.0 || b.radius <= 0.0 || img.w == 0 || img.h == 0 {
        return;
    }
    let (w, h) = (img.w, img.h);
    let base = highlights(img, b.threshold, b.softness.max(0.02));
    let r = (b.radius * px_scale).max(1.0);
    let n = (r.log2().ceil() as i32).clamp(1, 7) as usize;
    // mip pyramid of the highlights: level i is blurred by roughly 2^i pixels
    let mut levels = vec![base.clone()];
    for _ in 0..n {
        let Some(last) = levels.last() else { break };
        if last.w < 8 || last.h < 8 {
            break;
        }
        let next = down2(last);
        levels.push(next);
    }
    let s = 0.5 + 0.45 * b.scatter;
    let mut acc = zero_like(img);
    let mut wsum = 0.0f32;
    for (i, level) in levels.iter().enumerate() {
        let wi = if i == 0 { 0.5 } else { s.powi(i as i32) };
        let up = up_to(level, w, h);
        acc.px.par_iter_mut().zip(up.px.par_iter()).for_each(|(a, u)| *a += u * wi);
        wsum += wi;
    }
    let inv = if wsum > 0.0 { 1.0 / wsum } else { 0.0 };
    acc.px.par_iter_mut().for_each(|v| *v *= inv);
    if b.streak > 0.0 && b.streak_len > 0.0 {
        let mut st = base;
        gaussian(&mut st, b.streak_len * px_scale, (px_scale * 1.2).max(0.8), false);
        tint(&mut st, b.streak_color, 1.0);
        let k = b.streak * 2.0;
        acc.px.par_iter_mut().zip(st.px.par_iter()).for_each(|(a, s)| *a += s * k);
    }
    tint(&mut acc, b.color, b.strength);
    if b.only {
        *img = zero_like(img);
    }
    add_image(img, &acc, [b.intensity * 2.0; 3], b.screen);
}

/// Bloom.
pub fn bloom(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    bloom_core(img, &bloom_params(e, cx), cx.px_scale);
}

// ------------------------------------------------------------------ Diffusion

#[derive(Clone, Copy)]
struct Diffusion {
    strength: f32,
    radius: f32,
    bias: f32,
    warmth: f32,
    veil: f32,
}

fn diffusion_core(img: &mut Image, d: &Diffusion, px_scale: f32) {
    if (d.strength <= 0.0 && d.veil <= 0.0) || img.w == 0 {
        return;
    }
    let blur = blurred(img, (d.radius * px_scale).max(0.5));
    let glow = highlights(&blur, 0.3, 0.5);
    let warm = [1.0 + 0.3 * d.warmth, 1.0, 1.0 - 0.3 * d.warmth];
    let (soft, gain) = (0.5 * d.strength, 0.8 * d.strength * d.bias);
    img.px.par_chunks_mut(4).zip(blur.px.par_chunks(4).zip(glow.px.par_chunks(4))).for_each(|(p, (b, g))| {
        let a = p[3];
        for k in 0..3 {
            p[k] = p[k] * (1.0 - soft) + b[k] * soft + g[k] * gain * warm[k] + d.veil * 0.04 * a;
        }
    });
}

/// Diffusion Filter.
pub fn diffusion(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let d = Diffusion {
        strength: fv(e, "strength", cx).max(0.0) / 100.0,
        radius: fv(e, "radius", cx).max(0.0),
        bias: (fv(e, "highlight_bias", cx) / 100.0).clamp(0.0, 1.0),
        warmth: (fv(e, "warmth", cx) / 100.0).clamp(-1.0, 1.0),
        veil: (fv(e, "veil", cx) / 100.0).clamp(0.0, 1.0),
    };
    diffusion_core(img, &d, cx.px_scale);
}

// ------------------------------------------------------------------ Film Stock

/// One stock recipe (display-space tone and colour).
#[derive(Clone, Copy)]
struct Recipe {
    contrast: f32,
    fade: f32,
    shoulder: f32,
    sat: f32,
    hi_desat: f32,
    shadow: [f32; 3],
    high: [f32; 3],
    gamma: [f32; 3],
    mix: [[f32; 3]; 3],
}

const IDENT: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
const NEUTRAL: Recipe =
    Recipe { contrast: 0.0, fade: 0.0, shoulder: 0.0, sat: 1.0, hi_desat: 0.0, shadow: [0.0; 3], high: [0.0; 3], gamma: [1.0; 3], mix: IDENT };

/// In the order of [`filmcraft_project::effect::FILM_STOCKS`].
const RECIPES: [Recipe; 9] = [
    NEUTRAL,
    // Warm Print
    Recipe {
        contrast: 0.25,
        fade: 0.012,
        shoulder: 0.15,
        sat: 1.04,
        hi_desat: 0.10,
        shadow: [0.0, 0.004, 0.014],
        high: [0.028, 0.010, -0.022],
        mix: [[0.97, 0.03, 0.0], [0.02, 0.97, 0.01], [0.0, 0.03, 0.97]],
        ..NEUTRAL
    },
    // Cool Teal
    Recipe { contrast: 0.30, fade: 0.008, shoulder: 0.10, sat: 1.06, hi_desat: 0.05, shadow: [-0.020, 0.010, 0.026], high: [0.020, 0.006, -0.018], ..NEUTRAL },
    // Bleach Bypass
    Recipe { contrast: 0.55, shoulder: 0.05, sat: 0.55, hi_desat: 0.20, ..NEUTRAL },
    // Faded Matte
    Recipe { contrast: 0.08, fade: 0.075, shoulder: 0.28, sat: 0.85, hi_desat: 0.12, shadow: [-0.006, 0.006, 0.014], high: [0.012, 0.006, -0.004], ..NEUTRAL },
    // Cross Process
    Recipe {
        contrast: 0.35,
        fade: 0.01,
        shoulder: 0.10,
        sat: 1.20,
        shadow: [-0.012, 0.018, 0.030],
        high: [0.030, 0.012, -0.040],
        gamma: [0.88, 1.0, 1.22],
        ..NEUTRAL
    },
    // Tungsten Night
    Recipe {
        contrast: 0.35,
        fade: 0.01,
        shoulder: 0.12,
        sat: 1.12,
        hi_desat: 0.08,
        shadow: [-0.020, 0.0, 0.032],
        high: [0.040, 0.012, -0.030],
        gamma: [0.97, 1.0, 1.03],
        ..NEUTRAL
    },
    // Vintage 70s
    Recipe {
        contrast: 0.12,
        fade: 0.05,
        shoulder: 0.30,
        sat: 0.90,
        hi_desat: 0.15,
        shadow: [0.014, 0.012, -0.008],
        high: [0.026, 0.006, -0.030],
        mix: [[0.94, 0.06, 0.0], [0.03, 0.94, 0.03], [0.0, 0.08, 0.92]],
        ..NEUTRAL
    },
    // Silver B&W
    Recipe { contrast: 0.38, fade: 0.008, shoulder: 0.15, sat: 0.0, ..NEUTRAL },
];

#[derive(Clone, Copy)]
struct Stock {
    recipe: usize,
    amount: f32,
    contrast: f32,
    saturation: f32,
    fade: f32,
    warmth: f32,
    tint: f32,
    rolloff: f32,
}

#[inline]
fn scurve(x: f32) -> f32 {
    let t = x.clamp(0.0, 1.0);
    let s = t * t * (3.0 - 2.0 * t);
    s + (x - t)
}

fn stock_core(img: &mut Image, s: &Stock) {
    if s.amount <= 0.0 || img.w == 0 {
        return;
    }
    let r = RECIPES.get(s.recipe).copied().unwrap_or(NEUTRAL);
    let contrast = (r.contrast + s.contrast * 0.5).clamp(-0.6, 1.0);
    let fade = (r.fade + s.fade * 0.15).clamp(0.0, 0.3);
    let shoulder = (r.shoulder + s.rolloff * 0.5).clamp(0.0, 1.0);
    let sat = (r.sat * (1.0 + s.saturation)).max(0.0);
    img.px.par_chunks_mut(4).for_each(|p| {
        let Some((a, c)) = display(p) else { return };
        // per-channel gamma and dye cross-talk
        let g = [c[0].max(0.0).powf(r.gamma[0]), c[1].max(0.0).powf(r.gamma[1]), c[2].max(0.0).powf(r.gamma[2])];
        let mut y = [0.0f32; 3];
        for (i, row) in r.mix.iter().enumerate() {
            y[i] = row[0] * g[0] + row[1] * g[1] + row[2] * g[2];
        }
        for v in &mut y {
            *v += (scurve(*v) - *v) * contrast;
            *v -= shoulder * 0.6 * smoothstep(0.55, 1.0, *v) * (*v - 0.55).max(0.0);
            *v = fade + *v * (1.0 - fade);
        }
        // split toning
        let l = luma(y);
        let (ws, wh) = (1.0 - smoothstep(0.0, 0.55, l), smoothstep(0.45, 1.0, l));
        for (i, v) in y.iter_mut().enumerate() {
            *v += r.shadow[i] * ws + r.high[i] * wh;
        }
        // trims: warmth (red/blue), tint (green/magenta)
        y[0] += s.warmth * 0.03 + s.tint * 0.01;
        y[2] += -s.warmth * 0.03 + s.tint * 0.01;
        y[1] += -s.tint * 0.02;
        // saturation, with highlights desaturating like film
        let l2 = luma(y);
        let k = sat * (1.0 - r.hi_desat * smoothstep(0.6, 1.0, l2));
        for v in &mut y {
            *v = l2 + (*v - l2) * k;
        }
        let o = [c[0] + (y[0] - c[0]) * s.amount, c[1] + (y[1] - c[1]) * s.amount, c[2] + (y[2] - c[2]) * s.amount];
        store(p, a, o);
    });
}

/// Film Stock.
pub fn film_stock(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let s = Stock {
        recipe: chv(e, "stock") as usize,
        amount: fv(e, "amount", cx).max(0.0) / 100.0,
        contrast: (fv(e, "contrast", cx) / 100.0).clamp(-1.0, 1.0),
        saturation: (fv(e, "saturation", cx) / 100.0).clamp(-1.0, 1.0),
        fade: (fv(e, "fade", cx) / 100.0).clamp(0.0, 1.0),
        warmth: (fv(e, "warmth", cx) / 100.0).clamp(-1.0, 1.0),
        tint: (fv(e, "tint", cx) / 100.0).clamp(-1.0, 1.0),
        rolloff: (fv(e, "rolloff", cx) / 100.0).clamp(0.0, 1.0),
    };
    stock_core(img, &s);
}

// ------------------------------------------------------------------ Gate Weave & Flicker

#[derive(Clone, Copy)]
struct Weave {
    weave: f32,
    rotation: f32,
    breathing: f32,
    flicker: f32,
    fps: f32,
    seed: u64,
}

fn weave_core(img: &mut Image, wv: &Weave, seconds: f64, px_scale: f32) {
    if img.w == 0 || img.h == 0 || (wv.weave <= 0.0 && wv.rotation <= 0.0 && wv.breathing <= 0.0 && wv.flicker <= 0.0) {
        return;
    }
    let f = frame_index(seconds, wv.fps);
    let drift = f as f64 * 0.21;
    let jitter = |ch: i64| hash01(f, ch, 7, wv.seed) * 2.0 - 1.0;
    let dx = wv.weave * px_scale * (0.7 * noise1(drift, 0, wv.seed) + 0.35 * jitter(0));
    let dy = wv.weave * px_scale * (0.7 * noise1(drift, 1, wv.seed) + 0.35 * jitter(1)) * 0.6;
    let rot = wv.rotation * (0.8 * noise1(drift, 2, wv.seed) + 0.3 * jitter(2));
    let breath = 1.0 + wv.breathing * noise1(f as f64 * 0.06, 3, wv.seed);
    let (w, h) = (img.w as f32, img.h as f32);
    // zoom in just enough that the shifted, rotated frame still covers the canvas
    let cover = 2.0 * (dx.abs() / w + dy.abs() / h) + rot.abs().to_radians() * 0.7;
    let scale = (breath + cover).max(1.0);
    if dx != 0.0 || dy != 0.0 || rot != 0.0 || scale != 1.0 {
        place(img, Vec2::new(f64::from(w) / 2.0, f64::from(h) / 2.0), Vec2::new(f64::from(dx), f64::from(dy)), f64::from(scale), f64::from(rot));
    }
    if wv.flicker > 0.0 {
        let ev = wv.flicker * (0.65 * noise1(f as f64 * 0.45, 5, wv.seed) + 0.35 * jitter(5));
        let gain = ev.exp2();
        img.px.par_chunks_mut(4).for_each(|p| {
            for v in p.iter_mut().take(3) {
                *v *= gain;
            }
        });
    }
}

/// Gate Weave & Flicker.
pub fn gate_weave(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let wv = Weave {
        weave: fv(e, "weave", cx).max(0.0),
        rotation: fv(e, "rotation", cx).max(0.0),
        breathing: fv(e, "breathing", cx).max(0.0) / 100.0,
        flicker: fv(e, "flicker", cx).max(0.0) / 100.0,
        fps: fv(e, "frame_rate", cx),
        seed: fv(e, "seed", cx).max(0.0) as u64,
    };
    weave_core(img, &wv, cx.seconds, cx.px_scale);
}

// ------------------------------------------------------------------ Dust & Scratches

#[derive(Clone, Copy)]
struct Damage {
    dust: f32,
    dust_size: f32,
    dark_share: f32,
    hairs: f32,
    scratches: f32,
    intensity: f32,
    fps: f32,
    seed: u64,
}

/// A soft disc of light (`dark` = false) or shadow at (cx, cy).
fn stamp_disc(img: &mut Image, cx: f32, cy: f32, r: f32, dark: bool, alpha: f32) {
    let r = r.max(0.4);
    let (x0, x1) = ((cx - r).floor().max(0.0) as usize, ((cx + r).ceil().max(0.0) as usize).min(img.w.saturating_sub(1)));
    let (y0, y1) = ((cy - r).floor().max(0.0) as usize, ((cy + r).ceil().max(0.0) as usize).min(img.h.saturating_sub(1)));
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let t = (1.0 - (dx * dx + dy * dy) / (r * r)).max(0.0);
            if t <= 0.0 {
                continue;
            }
            let k = alpha * t * t;
            let i = (y * img.w + x) * 4;
            if let Some(p) = img.px.get_mut(i..i + 4) {
                if dark {
                    for v in p.iter_mut().take(3) {
                        *v *= 1.0 - k.min(1.0);
                    }
                } else {
                    add_light(p, [k; 3]);
                }
            }
        }
    }
}

/// How many events of an expected rate: the integer part plus one more with the fractional chance.
fn event_count(expected: f32, key: i64, seed: u64, cap: usize) -> usize {
    let base = expected.max(0.0).floor();
    let extra = usize::from(hash01(key, 99, 99, seed) < expected.max(0.0) - base);
    (base as usize + extra).min(cap)
}

fn damage_core(img: &mut Image, d: &Damage, seconds: f64, px_scale: f32) {
    if img.w == 0 || img.h == 0 || d.intensity <= 0.0 {
        return;
    }
    let (w, h) = (img.w as f32, img.h as f32);
    let mp = (w * h) / 1.0e6 / (px_scale * px_scale).max(1e-6);
    let mp = mp.max(1e-3) * px_scale * px_scale; // megapixels at working size: counts follow the picture, not the zoom
    let f = frame_index(seconds, d.fps);
    let seed = d.seed;
    // dust specks: one frame each
    let n = event_count(d.dust * mp * 30.0, f, seed, 4000);
    for i in 0..n as i64 {
        let u = |k: i64| hash01(i, f, k, seed);
        let r = d.dust_size * px_scale * (0.35 + 1.8 * u(3).powi(3));
        let alpha = d.intensity * (0.5 + 0.5 * u(5));
        stamp_disc(img, u(1) * w, u(2) * h, r, u(4) < d.dark_share, alpha);
    }
    // hairs: wandering polylines that stay for three frames
    let group = f.div_euclid(3);
    let n = event_count(d.hairs * mp * 1.5, group, seed ^ 0x4841, 400);
    for i in 0..n as i64 {
        let u = |k: i64| hash01(i, group, 10 + k, seed);
        let (mut x, mut y) = (u(0) * w, u(1) * h);
        let mut ang = u(2) * std::f32::consts::TAU;
        let dark = u(3) < 0.55;
        let seg = (6.0 + 18.0 * u(4)) * px_scale;
        for s in 0..6i64 {
            ang += (hash01(i, group, 40 + s, seed) - 0.5) * 1.6;
            let steps = (seg / 0.7).ceil().max(1.0) as usize;
            for _ in 0..steps {
                x += ang.cos() * 0.7;
                y += ang.sin() * 0.7;
                stamp_disc(img, x, y, 0.9 * px_scale.max(0.6), dark, d.intensity * 0.8);
            }
        }
    }
    // vertical scratches: four slots, each living a few frames
    for j in 0..4i64 {
        let life = 3 + 2 * j;
        let g = f.div_euclid(life);
        let u = |k: i64| hash01(j, g, 20 + k, seed);
        if u(0) >= (d.scratches * 0.8).min(1.0) {
            continue;
        }
        let x = u(1) * w;
        let (top, bottom) = (u(2) * h * 0.3, h * (0.6 + 0.4 * u(3)));
        let wobble_seed = seed.wrapping_add(j as u64 * 31);
        let (mut y, y_end) = (top, bottom);
        while y < y_end {
            let m = (0.5 + 0.5 * noise1(f64::from(y) * 0.02, 30, wobble_seed)).clamp(0.0, 1.0);
            let xo = x + 0.8 * px_scale * noise1(f64::from(y) * 0.01, 40, wobble_seed);
            stamp_disc(img, xo, y, 0.9 * px_scale.max(0.6), false, d.intensity * 0.6 * m);
            y += 0.6;
        }
    }
}

/// Dust & Scratches.
pub fn film_damage(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let d = Damage {
        dust: fv(e, "dust", cx).max(0.0) / 100.0,
        dust_size: fv(e, "dust_size", cx).max(0.1),
        dark_share: (fv(e, "dark_dust", cx) / 100.0).clamp(0.0, 1.0),
        hairs: fv(e, "hairs", cx).max(0.0) / 100.0,
        scratches: fv(e, "scratches", cx).max(0.0) / 100.0,
        intensity: (fv(e, "intensity", cx) / 100.0).clamp(0.0, 1.0),
        fps: fv(e, "frame_rate", cx),
        seed: fv(e, "seed", cx).max(0.0) as u64,
    };
    damage_core(img, &d, cx.seconds, cx.px_scale);
}

// ------------------------------------------------------------------ Lens Character

fn lens_core(img: &mut Image, aberration: f32, softness: f32, px_scale: f32) {
    if img.w == 0 || img.h == 0 || (aberration <= 0.0 && softness <= 0.0) {
        return;
    }
    let (w, h) = (img.w as f32, img.h as f32);
    let (ccx, ccy) = (w / 2.0, h / 2.0);
    let half_diag = (ccx * ccx + ccy * ccy).sqrt().max(1.0);
    let mut src = img.clone();
    if softness > 0.0 {
        let blur = blurred(&src, (3.0 * px_scale).max(0.8));
        src.px.par_chunks_mut(w as usize * 4).zip(blur.px.par_chunks(w as usize * 4)).enumerate().for_each(|(y, (row, brow))| {
            for (x, (p, b)) in row.as_chunks_mut::<4>().0.iter_mut().zip(brow.as_chunks::<4>().0.iter()).enumerate() {
                let (dx, dy) = (x as f32 + 0.5 - ccx, y as f32 + 0.5 - ccy);
                let t = smoothstep(0.35, 1.0, (dx * dx + dy * dy).sqrt() / half_diag) * softness;
                for (v, bv) in p.iter_mut().zip(b) {
                    *v += (bv - *v) * t;
                }
            }
        });
    }
    if aberration <= 0.0 {
        *img = src;
        return;
    }
    let k = aberration * 0.012;
    let width = img.w;
    img.px.par_chunks_mut(width * 4).enumerate().for_each(|(y, row)| {
        for (x, p) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let (dx, dy) = (fx - ccx, fy - ccy);
            let r = (dx * dx + dy * dy).sqrt() / half_diag;
            let (kr, kb) = (1.0 + k * r, 1.0 - k * r);
            let red = src.sample_bilinear_clamped(ccx + dx * kr, ccy + dy * kr);
            let green = src.sample_bilinear_clamped(fx, fy);
            let blue = src.sample_bilinear_clamped(ccx + dx * kb, ccy + dy * kb);
            p.copy_from_slice(&[red[0], green[1], blue[2], green[3]]);
        }
    });
}

/// Lens Character.
pub fn lens_character(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    lens_core(img, fv(e, "aberration", cx).max(0.0) / 100.0, (fv(e, "softness", cx) / 100.0).clamp(0.0, 1.0), cx.px_scale);
}

// ------------------------------------------------------------------ Film Look (all in one)

/// A film format's character: stock recipe, grain, halation, bloom, diffusion, gate, damage.
struct Look {
    stock: usize,
    stock_amount: f32,
    /// amount %, size px, roughness %, colour grain %
    grain: [f32; 4],
    /// intensity %, radius px, threshold 0..1
    halation: [f32; 3],
    /// intensity %, radius px, threshold 0..1, scatter 0..1
    bloom: [f32; 4],
    /// diffusion strength %
    diffusion: f32,
    /// weave px, rotation °, breathing %, flicker %
    weave: [f32; 4],
    /// dust %, hairs %, scratches %
    damage: [f32; 3],
}

/// In the order of [`filmcraft_project::effect::FILM_LOOK_PRESETS`].
const LOOKS: [Look; 7] = [
    // 35mm Clean
    Look {
        stock: 1,
        stock_amount: 0.6,
        grain: [18.0, 1.2, 30.0, 20.0],
        halation: [35.0, 30.0, 0.78],
        bloom: [20.0, 70.0, 0.80, 0.55],
        diffusion: 8.0,
        weave: [0.5, 0.03, 0.1, 1.0],
        damage: [0.0, 0.0, 0.0],
    },
    // 35mm Gritty
    Look {
        stock: 2,
        stock_amount: 0.8,
        grain: [48.0, 1.7, 50.0, 30.0],
        halation: [55.0, 40.0, 0.72],
        bloom: [25.0, 70.0, 0.78, 0.60],
        diffusion: 12.0,
        weave: [1.0, 0.05, 0.15, 2.5],
        damage: [12.0, 2.0, 3.0],
    },
    // 16mm Documentary
    Look {
        stock: 1,
        stock_amount: 0.9,
        grain: [55.0, 2.6, 60.0, 35.0],
        halation: [60.0, 55.0, 0.70],
        bloom: [30.0, 80.0, 0.74, 0.65],
        diffusion: 15.0,
        weave: [1.8, 0.08, 0.25, 4.0],
        damage: [28.0, 8.0, 6.0],
    },
    // Super 8 Memory
    Look {
        stock: 7,
        stock_amount: 1.0,
        grain: [95.0, 4.2, 70.0, 50.0],
        halation: [85.0, 80.0, 0.62],
        bloom: [45.0, 110.0, 0.68, 0.70],
        diffusion: 25.0,
        weave: [3.5, 0.25, 0.4, 10.0],
        damage: [45.0, 18.0, 22.0],
    },
    // Large Format 65
    Look {
        stock: 1,
        stock_amount: 0.35,
        grain: [10.0, 0.9, 25.0, 15.0],
        halation: [20.0, 35.0, 0.82],
        bloom: [28.0, 90.0, 0.80, 0.60],
        diffusion: 10.0,
        weave: [0.2, 0.0, 0.05, 0.4],
        damage: [0.0, 0.0, 0.0],
    },
    // Vintage 70s Print
    Look {
        stock: 7,
        stock_amount: 1.0,
        grain: [45.0, 2.0, 55.0, 30.0],
        halation: [50.0, 45.0, 0.72],
        bloom: [35.0, 80.0, 0.76, 0.60],
        diffusion: 18.0,
        weave: [1.2, 0.05, 0.2, 3.0],
        damage: [14.0, 4.0, 5.0],
    },
    // Night Neon
    Look {
        stock: 6,
        stock_amount: 1.0,
        grain: [35.0, 1.5, 45.0, 30.0],
        halation: [90.0, 60.0, 0.60],
        bloom: [55.0, 100.0, 0.62, 0.70],
        diffusion: 10.0,
        weave: [0.6, 0.0, 0.1, 1.5],
        damage: [4.0, 0.0, 0.0],
    },
];

/// Film Look: the chosen preset's weave → stock → diffusion → halation → bloom → grain → damage,
/// each section scaled by its own slider and by Amount (0 = untouched picture).
pub fn film_look(img: &mut Image, e: &EffectInstance, cx: &FxCtx) {
    let amount = (fv(e, "amount", cx) / 100.0).max(0.0);
    if amount <= 0.0 || img.w == 0 || img.h == 0 {
        return;
    }
    let look = LOOKS.get(chv(e, "preset") as usize).unwrap_or(&LOOKS[0]);
    let sect = |id: &str| (fv(e, id, cx) / 100.0).max(0.0) * amount;
    let (grain_k, hal_k, bloom_k, color_k, weave_k, dmg_k) = (sect("grain"), sect("halation"), sect("bloom"), sect("color"), sect("weave"), sect("damage"));
    let fps = fv(e, "frame_rate", cx);
    let seed = fv(e, "seed", cx).max(0.0) as u64;
    let ps = cx.px_scale;

    weave_core(
        img,
        &Weave {
            weave: look.weave[0] * weave_k,
            rotation: look.weave[1] * weave_k,
            breathing: look.weave[2] * weave_k / 100.0,
            flicker: look.weave[3] * weave_k / 100.0,
            fps,
            seed,
        },
        cx.seconds,
        ps,
    );
    stock_core(
        img,
        &Stock { recipe: look.stock, amount: look.stock_amount * color_k, contrast: 0.0, saturation: 0.0, fade: 0.0, warmth: 0.0, tint: 0.0, rolloff: 0.0 },
    );
    diffusion_core(img, &Diffusion { strength: look.diffusion / 100.0 * amount, radius: 14.0, bias: 0.6, warmth: 0.0, veil: 0.0 }, ps);
    halation_core(
        img,
        &Halation {
            threshold: look.halation[2],
            softness: 0.2,
            radius: look.halation[1],
            spread: 0.5,
            intensity: look.halation[0] / 100.0 * hal_k,
            color: lin([1.0, 0.27, 0.10, 1.0]),
            strength: 0.85,
            screen: false,
            only: false,
        },
        ps,
    );
    bloom_core(
        img,
        &Bloom {
            threshold: look.bloom[2],
            softness: 0.35,
            scatter: look.bloom[3],
            radius: look.bloom[1],
            intensity: look.bloom[0] / 100.0 * bloom_k,
            color: [1.0; 3],
            strength: 0.0,
            streak: 0.0,
            streak_len: 0.0,
            streak_color: [0.45, 0.65, 1.0],
            screen: false,
            only: false,
        },
        ps,
    );
    grain_core(
        img,
        &Grain {
            amount: look.grain[0] / 100.0 * grain_k,
            size: look.grain[1],
            rough: look.grain[2] / 100.0,
            shadows: 0.7,
            mids: 1.0,
            highs: 0.45,
            chroma: look.grain[3] / 100.0,
            animated: true,
            fps,
            seed,
        },
        cx.seconds,
        ps,
    );
    damage_core(
        img,
        &Damage {
            dust: look.damage[0] / 100.0 * dmg_k,
            dust_size: 2.5,
            dark_share: 0.4,
            hairs: look.damage[1] / 100.0 * dmg_k,
            scratches: look.damage[2] / 100.0 * dmg_k,
            intensity: 0.6,
            fps,
            seed,
        },
        cx.seconds,
        ps,
    );
}

#[cfg(test)]
mod tests {
    use filmcraft_project::{ParamValue, find_effect};
    use filmcraft_time::Tick;

    use super::*;

    const IDS: &[&str] = &["film_look", "film_grain", "halation", "bloom", "diffusion", "film_stock", "gate_weave", "film_damage", "lens_character"];

    fn cx_at(seconds: f64) -> FxCtx<'static> {
        FxCtx {
            t: Tick::ZERO,
            px_scale: 1.0,
            frame_rate: 24.0,
            seconds,
            timecode: "01:00:00:00",
            clip_name: "clip",
            project: None,
            env: None,
            working: filmcraft_color::WorkingSpace::Rec709,
        }
    }

    fn inst(id: &str) -> EffectInstance {
        find_effect(id).unwrap_or_else(|| panic!("no effect {id}")).instance()
    }
    fn setf(e: &mut EffectInstance, k: &str, v: f64) {
        e.params.get_mut(k).unwrap_or_else(|| panic!("{}: no param {k}", e.effect)).value = ParamValue::Float(v);
    }
    fn setc(e: &mut EffectInstance, k: &str, v: u32) {
        e.params.get_mut(k).unwrap_or_else(|| panic!("{}: no param {k}", e.effect)).value = ParamValue::Choice(v);
    }

    /// A flat picture of display value `v` (straight sRGB, grey).
    fn flat(w: usize, h: usize, v: f32) -> Image {
        let l = srgb_to_linear(v);
        Image::filled(w, h, [l, l, l, 1.0])
    }

    /// A deterministic textured picture.
    fn picture(w: usize, h: usize) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (u, v) = (x as f32 / w as f32, y as f32 / h as f32);
                let c = dec([(0.15 + 0.7 * u).clamp(0.0, 1.0), (0.2 + 0.6 * v).clamp(0.0, 1.0), (0.5 + 0.3 * (u - v)).clamp(0.0, 1.0)]);
                if let Some(p) = img.px.get_mut((y * w + x) * 4..(y * w + x) * 4 + 4) {
                    p.copy_from_slice(&[c[0], c[1], c[2], 1.0]);
                }
            }
        }
        img
    }

    fn run(e: &EffectInstance, img: &Image, seconds: f64) -> Image {
        let mut o = img.clone();
        crate::effects::apply(&mut o, e, &cx_at(seconds));
        o
    }

    fn max_diff(a: &Image, b: &Image) -> f32 {
        a.px.iter().zip(&b.px).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
    }

    /// Mean and standard deviation of the display luma of an image.
    fn stats(img: &Image) -> (f32, f32) {
        let v: Vec<f32> = img.px.as_chunks::<4>().0.iter().map(|p| linear_to_srgb(luma([p[0], p[1], p[2]]).max(0.0))).collect();
        let n = v.len().max(1) as f32;
        let m = v.iter().sum::<f32>() / n;
        (m, (v.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / n).sqrt())
    }

    #[test]
    fn every_film_effect_runs_finite_and_deterministic() {
        let img = picture(96, 64);
        for id in IDS {
            let e = inst(id);
            let a = run(&e, &img, 0.5);
            let b = run(&e, &img, 0.5);
            assert!(a.px.iter().all(|v| v.is_finite()), "{id}: non-finite");
            assert!(a.px.iter().all(|v| *v >= -1e-4), "{id}: negative light");
            assert_eq!(max_diff(&a, &b), 0.0, "{id}: not deterministic");
            assert!(max_diff(&a, &img) > 0.0, "{id}: default parameters change nothing");
        }
    }

    #[test]
    fn disabled_or_empty_images_are_left_alone() {
        for id in IDS {
            let mut e = inst(id);
            e.enabled = false;
            let img = picture(32, 32);
            assert_eq!(max_diff(&run(&e, &img, 0.0), &img), 0.0, "{id} disabled");
            let mut empty = Image::new(0, 0);
            crate::effects::apply(&mut empty, &inst(id), &cx_at(0.0));
        }
    }

    #[test]
    fn grain_strength_follows_amount_and_keeps_the_mean() {
        let img = flat(160, 160, 0.5);
        let mut weak = inst("film_grain");
        setf(&mut weak, "amount", 20.0);
        let mut strong = inst("film_grain");
        setf(&mut strong, "amount", 80.0);
        let ((m1, s1), (m2, s2)) = (stats(&run(&weak, &img, 0.0)), stats(&run(&strong, &img, 0.0)));
        assert!((m1 - 0.5).abs() < 0.01 && (m2 - 0.5).abs() < 0.01, "mean moved: {m1} {m2}");
        assert!(s1 > 0.003 && s2 > s1 * 3.0 && s2 < s1 * 5.0, "std {s1} -> {s2}");
    }

    #[test]
    fn grain_depends_on_exposure() {
        let mut e = inst("film_grain");
        setf(&mut e, "amount", 60.0);
        setf(&mut e, "shadows", 0.0);
        setf(&mut e, "highs", 0.0);
        let std_at = |v: f32| stats(&run(&e, &flat(128, 128, v), 0.0)).1;
        let (dark, mid, bright) = (std_at(0.08), std_at(0.5), std_at(0.97));
        assert!(mid > 0.01, "mids should have grain: {mid}");
        assert!(dark < mid * 0.2 && bright < mid * 0.2, "dark {dark} mid {mid} bright {bright}");
    }

    #[test]
    fn grain_rerolls_each_film_frame_unless_static() {
        let img = flat(64, 64, 0.5);
        let mut e = inst("film_grain");
        let (f0, f0b, f1) = (run(&e, &img, 0.0), run(&e, &img, 0.01), run(&e, &img, 1.0 / 24.0 + 0.001));
        assert_eq!(max_diff(&f0, &f0b), 0.0, "same film frame must be identical");
        assert!(max_diff(&f0, &f1) > 0.001, "next film frame must differ");
        if let Some(p) = e.params.get_mut("animated") {
            p.value = ParamValue::Bool(false);
        }
        assert_eq!(max_diff(&run(&e, &img, 0.0), &run(&e, &img, 5.0)), 0.0, "static grain never changes");
    }

    #[test]
    fn grain_size_makes_it_coarser() {
        let img = flat(160, 160, 0.5);
        let corr = |size: f64| {
            let mut e = inst("film_grain");
            setf(&mut e, "size", size);
            setf(&mut e, "roughness", 0.0);
            let o = run(&e, &img, 0.0);
            let v: Vec<f32> = o.px.as_chunks::<4>().0.iter().map(|p| p[0]).collect();
            let m = v.iter().sum::<f32>() / v.len() as f32;
            let (mut num, mut den) = (0.0, 0.0);
            for y in 0..160 {
                for x in 0..159 {
                    let (a, b) = (v[y * 160 + x] - m, v[y * 160 + x + 1] - m);
                    num += a * b;
                    den += a * a;
                }
            }
            num / den.max(1e-9)
        };
        assert!(corr(5.0) > corr(1.0) + 0.2, "neighbour correlation: coarse {} fine {}", corr(5.0), corr(1.0));
    }

    #[test]
    fn halation_is_a_red_glow_around_highlights_only() {
        let (w, h) = (128usize, 128usize);
        let mut img = Image::new(w, h);
        for y in 56..72 {
            for x in 56..72 {
                img.px[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
            }
        }
        for (i, p) in img.px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let (x, y) = (i % w, i / w);
            if !(56..72).contains(&x) || !(56..72).contains(&y) {
                p.copy_from_slice(&[0.0, 0.0, 0.0, 1.0]);
            }
        }
        let mut e = inst("halation");
        setf(&mut e, "radius", 12.0);
        let o = run(&e, &img, 0.0);
        let at = |x: usize, y: usize| [o.px[(y * w + x) * 4], o.px[(y * w + x) * 4 + 1], o.px[(y * w + x) * 4 + 2]];
        let near = at(76, 64);
        assert!(near[0] > near[1] && near[1] > near[2] && near[2] > 0.0, "should glow red-orange: {near:?}");
        assert!(at(4, 4)[0] < near[0] * 0.2, "far corner should be nearly dark: {:?}", at(4, 4));
        // a picture with nothing above the threshold is untouched
        let dim = flat(64, 64, 0.3);
        assert_eq!(max_diff(&run(&e, &dim, 0.0), &dim), 0.0);
    }

    #[test]
    fn bloom_spreads_energy_from_highlights_and_streaks_sideways() {
        let (w, h) = (128usize, 128usize);
        let mut img = Image::new(w, h);
        img.px.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p.copy_from_slice(&[0.0, 0.0, 0.0, 1.0]));
        for y in 62..66 {
            for x in 62..66 {
                img.px[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[4.0, 4.0, 4.0, 1.0]);
            }
        }
        let sum = |i: &Image| i.px.as_chunks::<4>().0.iter().map(|p| p[0]).sum::<f32>();
        let mut e = inst("bloom");
        setf(&mut e, "radius", 24.0);
        let o = run(&e, &img, 0.0);
        assert!(sum(&o) > sum(&img) * 1.05, "bloom must add light");
        let r = |x: usize, y: usize| o.px[(y * w + x) * 4];
        assert!(r(70, 64) > r(90, 64) && r(90, 64) > 0.0, "falls off with distance");
        let mut s = inst("bloom");
        setf(&mut s, "radius", 8.0);
        setf(&mut s, "streak", 100.0);
        setf(&mut s, "streak_length", 30.0);
        let o = run(&s, &img, 0.0);
        let r = |x: usize, y: usize| o.px[(y * w + x) * 4 + 2];
        assert!(r(64 + 24, 64) > r(64, 64 + 24) * 2.0, "streak must be horizontal: {} vs {}", r(88, 64), r(64, 88));
        // flat mid-grey is under the threshold
        let dim = flat(64, 64, 0.4);
        assert_eq!(max_diff(&run(&inst("bloom"), &dim, 0.0), &dim), 0.0);
    }

    #[test]
    fn diffusion_off_is_identity_and_on_softens() {
        let img = picture(64, 64);
        let mut off = inst("diffusion");
        setf(&mut off, "strength", 0.0);
        assert_eq!(max_diff(&run(&off, &img, 0.0), &img), 0.0);
        let mut on = inst("diffusion");
        setf(&mut on, "strength", 100.0);
        setf(&mut on, "radius", 6.0);
        let mut checker = Image::new(64, 64);
        for (i, p) in checker.px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let v = if ((i % 64) / 4 + (i / 64) / 4) % 2 == 0 { 0.9 } else { 0.1 };
            p.copy_from_slice(&[v, v, v, 1.0]);
        }
        assert!(stats(&run(&on, &checker, 0.0)).1 < stats(&checker).1, "contrast should drop");
    }

    #[test]
    fn film_stock_neutral_is_identity_and_recipes_do_what_they_say() {
        let img = picture(64, 48);
        let mut n = inst("film_stock");
        setc(&mut n, "stock", 0);
        assert!(max_diff(&run(&n, &img, 0.0), &img) < 1e-4, "neutral recipe must not change the picture");
        let mut zero = inst("film_stock");
        setf(&mut zero, "amount", 0.0);
        assert_eq!(max_diff(&run(&zero, &img, 0.0), &img), 0.0);
        let mut silver = inst("film_stock");
        setc(&mut silver, "stock", 8);
        let o = run(&silver, &img, 0.0);
        assert!(o.px.as_chunks::<4>().0.iter().all(|p| (p[0] - p[1]).abs() < 1e-3 && (p[1] - p[2]).abs() < 1e-3), "Silver B&W must be neutral");
        let sat = |i: &Image| i.px.as_chunks::<4>().0.iter().map(|p| (p[0] - p[1]).abs() + (p[1] - p[2]).abs()).sum::<f32>();
        let mut bleach = inst("film_stock");
        setc(&mut bleach, "stock", 3);
        assert!(sat(&run(&bleach, &img, 0.0)) < sat(&img) * 0.75, "bleach bypass desaturates");
        let mut faded = inst("film_stock");
        setc(&mut faded, "stock", 4);
        let black = flat(8, 8, 0.0);
        assert!(run(&faded, &black, 0.0).px[0] > 0.003, "faded matte lifts the blacks");
    }

    #[test]
    fn film_stock_keeps_hdr_light() {
        let hdr = Image::filled(4, 4, [3.0, 2.0, 1.5, 1.0]);
        let mut e = inst("film_stock");
        setc(&mut e, "stock", 0);
        let o = run(&e, &hdr, 0.0);
        assert!((o.px[0] - 3.0).abs() < 0.01 && (o.px[1] - 2.0).abs() < 0.01, "HDR values must survive: {:?}", &o.px[..4]);
    }

    #[test]
    fn gate_weave_moves_the_frame_per_film_frame_and_keeps_it_covered() {
        let img = picture(96, 64);
        let mut e = inst("gate_weave");
        setf(&mut e, "weave", 6.0);
        setf(&mut e, "rotation", 0.5);
        setf(&mut e, "flicker", 0.0);
        let (a, b) = (run(&e, &img, 0.0), run(&e, &img, 5.0 / 24.0));
        assert!(max_diff(&a, &img) > 0.0 && max_diff(&a, &b) > 0.0, "frames should jitter differently");
        assert!(a.px.as_chunks::<4>().0.iter().all(|p| p[3] > 0.99), "no transparent edges after weave");
        let mut still = inst("gate_weave");
        for (k, v) in [("weave", 0.0), ("rotation", 0.0), ("breathing", 0.0), ("flicker", 0.0)] {
            setf(&mut still, k, v);
        }
        assert_eq!(max_diff(&run(&still, &img, 3.0), &img), 0.0);
        let mut flick = inst("gate_weave");
        for (k, v) in [("weave", 0.0), ("rotation", 0.0), ("breathing", 0.0), ("flicker", 40.0)] {
            setf(&mut flick, k, v);
        }
        let ratios: Vec<f32> = (0..8).map(|f| run(&flick, &img, f as f64 / 24.0).px[0] / img.px[0]).collect();
        assert!(ratios.iter().cloned().fold(0.0, f32::max) - ratios.iter().cloned().fold(9.0, f32::min) > 0.05, "exposure should flicker: {ratios:?}");
    }

    #[test]
    fn dust_and_scratches_show_up_and_zero_is_clean() {
        let img = flat(160, 120, 0.5);
        let mut e = inst("film_damage");
        setf(&mut e, "dust", 300.0);
        setf(&mut e, "scratches", 100.0);
        setf(&mut e, "hairs", 300.0);
        setf(&mut e, "intensity", 100.0);
        let changed = |o: &Image| o.px.as_chunks::<4>().0.iter().zip(img.px.as_chunks::<4>().0.iter()).filter(|(a, b)| (a[0] - b[0]).abs() > 0.01).count();
        assert!((0..8).any(|f| changed(&run(&e, &img, f as f64 / 24.0)) > 5), "damage should appear within a few frames");
        let mut none = inst("film_damage");
        for k in ["dust", "scratches", "hairs"] {
            setf(&mut none, k, 0.0);
        }
        assert_eq!(max_diff(&run(&none, &img, 0.2), &img), 0.0);
    }

    #[test]
    fn lens_character_shifts_colour_fringes_at_the_edges_only() {
        let (w, h) = (160usize, 100usize);
        let mut img = Image::new(w, h);
        for (i, p) in img.px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let v = if (i % w) % 20 < 10 { 0.9 } else { 0.05 };
            p.copy_from_slice(&[v, v, v, 1.0]);
        }
        let mut e = inst("lens_character");
        setf(&mut e, "aberration", 300.0);
        setf(&mut e, "softness", 0.0);
        let o = run(&e, &img, 0.0);
        let px = |x: usize, y: usize| &o.px[(y * w + x) * 4..(y * w + x) * 4 + 3];
        let centre = px(80, 50);
        assert!((centre[0] - centre[2]).abs() < 0.02, "centre stays clean: {centre:?}");
        let fringe = (0..w).map(|x| (px(x, 50)[0] - px(x, 50)[2]).abs()).fold(0.0, f32::max);
        assert!(fringe > 0.05, "edges should fringe: {fringe}");
        let mut zero = inst("lens_character");
        setf(&mut zero, "aberration", 0.0);
        setf(&mut zero, "softness", 0.0);
        assert_eq!(max_diff(&run(&zero, &img, 0.0), &img), 0.0);
    }

    #[test]
    fn film_look_presets_all_run_and_amount_zero_is_untouched() {
        let img = picture(96, 64);
        for preset in 0..LOOKS.len() as u32 {
            let mut e = inst("film_look");
            setc(&mut e, "preset", preset);
            let o = run(&e, &img, 0.3);
            assert!(o.px.iter().all(|v| v.is_finite() && *v >= -1e-4), "preset {preset}");
            assert!(max_diff(&o, &img) > 0.001, "preset {preset} changes nothing");
            setf(&mut e, "amount", 0.0);
            assert_eq!(max_diff(&run(&e, &img, 0.3), &img), 0.0, "preset {preset} at amount 0");
        }
        // the heavier formats are visibly rougher than the large format
        let rough = |preset: u32| {
            let mut e = inst("film_look");
            setc(&mut e, "preset", preset);
            stats(&run(&e, &flat(128, 128, 0.5), 0.0)).1
        };
        assert!(rough(3) > rough(4) * 3.0, "Super 8 {} vs 65mm {}", rough(3), rough(4));
    }

    #[test]
    fn film_look_effects_live_in_their_own_bin() {
        for id in IDS {
            let d = find_effect(id).unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(d.category, &["Film Look"], "{id}");
        }
        assert_eq!(filmcraft_project::effect::FILM_LOOK_PRESETS.len(), LOOKS.len());
        assert_eq!(filmcraft_project::effect::FILM_STOCKS.len(), RECIPES.len());
    }
}
