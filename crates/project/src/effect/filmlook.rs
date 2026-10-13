//! The Film Look bin: photochemical-film character as video effects — grain that depends on
//! exposure, halation, bloom, diffusion, stock emulation, gate weave and flicker, dust and
//! scratches, lens character — plus one all-in-one "Film Look" with ready-made presets.
//!
//! These are our own effects, not part of Premiere's catalogue, so they live in their own
//! top-level bin (outside "Video Effects") and the catalogue test that pins Premiere's list is
//! unaffected. Pixel implementations live in `filmcraft_render::vfx::filmlook`.

use super::*;

/// The Effects panel bin of the film-character effects.
pub(super) const FILM: &[&str] = &["Film Look"];

/// Stock-emulation recipes of "Film Stock" (index = `stock` choice; the renderer owns the recipes).
pub const FILM_STOCKS: &[&str] =
    &["Neutral", "Warm Print", "Cool Teal", "Bleach Bypass", "Faded Matte", "Cross Process", "Tungsten Night", "Vintage 70s", "Silver B&W"];
/// Presets of the all-in-one "Film Look" (index = `preset` choice; the renderer owns the values).
pub const FILM_LOOK_PRESETS: &[&str] =
    &["35mm Clean", "35mm Gritty", "16mm Documentary", "Super 8 Memory", "Large Format 65", "Vintage 70s Print", "Night Neon"];
const BLEND_MODES: &[&str] = &["Add", "Screen"];

fn pct(id: &'static str, label: &'static str, def: f64) -> ParamDef {
    f(id, label, def, 0.0, 100.0, "%")
}
fn pct_to(id: &'static str, label: &'static str, def: f64, max: f64) -> ParamDef {
    fs(id, label, def, (0.0, max), (0.0, max.min(200.0)), "%", 0)
}
fn bipolar(id: &'static str, label: &'static str, def: f64) -> ParamDef {
    fs(id, label, def, (-100.0, 100.0), (-100.0, 100.0), "", 0)
}
fn px(id: &'static str, label: &'static str, def: f64, max: f64, soft: f64) -> ParamDef {
    fs(id, label, def, (0.0, max), (0.0, soft), "", 1)
}
fn fps() -> ParamDef {
    fs("frame_rate", "Frame Rate", 24.0, (1.0, 120.0), (6.0, 60.0), "fps", 0)
}
fn seed() -> ParamDef {
    fs("seed", "Random Seed", 0.0, (0.0, 10000.0), (0.0, 1000.0), "", 0)
}
fn film(id: &'static str, name: &'static str, params: Vec<ParamDef>) -> EffectDef {
    video(id, name, FILM, params)
}

pub(super) fn defs() -> Vec<EffectDef> {
    vec![
        film(
            "film_look",
            "Film Look",
            vec![
                ch("preset", "Preset", FILM_LOOK_PRESETS, 0),
                pct("amount", "Amount", 100.0),
                pct_to("grain", "Grain", 100.0, 300.0),
                pct_to("halation", "Halation", 100.0, 300.0),
                pct_to("bloom", "Bloom", 100.0, 300.0),
                pct_to("color", "Stock Colour", 100.0, 200.0),
                pct_to("weave", "Gate Weave & Flicker", 100.0, 300.0),
                pct_to("damage", "Dust & Scratches", 100.0, 300.0),
                fps(),
                seed(),
            ],
        ),
        film(
            "film_grain",
            "Film Grain",
            vec![
                pct_to("amount", "Amount", 35.0, 300.0),
                px("size", "Grain Size", 1.6, 12.0, 6.0),
                pct("roughness", "Roughness", 45.0),
                pct_to("shadows", "Shadows Response", 70.0, 200.0),
                pct_to("mids", "Midtones Response", 100.0, 200.0),
                pct_to("highs", "Highlights Response", 45.0, 200.0),
                pct("chroma", "Colour Grain", 30.0),
                b("animated", "Animated", true),
                fps(),
                seed(),
            ],
        ),
        film(
            "halation",
            "Halation",
            vec![
                pct("threshold", "Threshold", 72.0),
                pct("softness", "Softness", 20.0),
                px("radius", "Radius", 40.0, 800.0, 200.0),
                pct("spread", "Wide Spread", 50.0),
                fs("intensity", "Intensity", 60.0, (0.0, 1000.0), (0.0, 400.0), "%", 0),
                col("color", "Halation Color", [1.0, 0.27, 0.10, 1.0]),
                pct("tint_strength", "Colour Strength", 85.0),
                ch("mode", "Blending Mode", BLEND_MODES, 0),
                b("only", "Halation Only", false),
            ],
        ),
        film(
            "bloom",
            "Bloom",
            vec![
                pct("threshold", "Threshold", 75.0),
                pct("softness", "Softness", 35.0),
                pct("scatter", "Scatter", 60.0),
                px("radius", "Radius", 80.0, 1000.0, 300.0),
                fs("intensity", "Intensity", 50.0, (0.0, 1000.0), (0.0, 400.0), "%", 0),
                col("color", "Bloom Color", [1.0, 1.0, 1.0, 1.0]),
                pct("tint_strength", "Colour Strength", 0.0),
                pct("streak", "Anamorphic Streak", 0.0),
                px("streak_length", "Streak Length", 200.0, 2000.0, 600.0),
                col("streak_color", "Streak Color", [0.45, 0.65, 1.0, 1.0]),
                ch("mode", "Blending Mode", BLEND_MODES, 0),
                b("only", "Bloom Only", false),
            ],
        ),
        film(
            "diffusion",
            "Diffusion Filter",
            vec![
                pct("strength", "Strength", 35.0),
                px("radius", "Radius", 14.0, 300.0, 100.0),
                pct("highlight_bias", "Highlight Bias", 60.0),
                bipolar("warmth", "Warmth", 0.0),
                pct("veil", "Black Lift", 0.0),
            ],
        ),
        film(
            "film_stock",
            "Film Stock",
            vec![
                ch("stock", "Stock", FILM_STOCKS, 1),
                pct_to("amount", "Amount", 100.0, 200.0),
                bipolar("contrast", "Contrast", 0.0),
                bipolar("saturation", "Saturation", 0.0),
                pct("fade", "Fade", 0.0),
                bipolar("warmth", "Warmth", 0.0),
                bipolar("tint", "Tint", 0.0),
                pct("rolloff", "Highlight Roll-off", 0.0),
            ],
        ),
        film(
            "gate_weave",
            "Gate Weave & Flicker",
            vec![
                px("weave", "Weave", 1.2, 20.0, 8.0),
                fs("rotation", "Rotation", 0.06, (0.0, 3.0), (0.0, 1.0), "°", 2),
                fs("breathing", "Breathing", 0.2, (0.0, 5.0), (0.0, 2.0), "%", 2),
                pct_to("flicker", "Flicker", 4.0, 50.0),
                fps(),
                seed(),
            ],
        ),
        film(
            "film_damage",
            "Dust & Scratches",
            vec![
                pct_to("dust", "Dust", 30.0, 300.0),
                px("dust_size", "Dust Size", 2.5, 30.0, 10.0),
                pct("dark_dust", "Dark Dust Share", 40.0),
                pct_to("hairs", "Hairs", 10.0, 300.0),
                pct_to("scratches", "Scratches", 15.0, 300.0),
                pct("intensity", "Intensity", 60.0),
                fps(),
                seed(),
            ],
        ),
        film("lens_character", "Lens Character", vec![pct_to("aberration", "Chromatic Aberration", 25.0, 300.0), pct("softness", "Edge Softness", 25.0)]),
    ]
}
