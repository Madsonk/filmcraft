//! Film Look bin: the Program monitor (GPU plan at a reduced scale) and the export (full-scale
//! reference render) must show the same picture.
//!
//! The Film Look effects have no WGSL implementation: `FxOp::eval` returns `None` for them, so
//! the plan renders the clip on the CPU with the export code and hands the GPU a finished image.
//! These tests pin that fallback, and that the result at the preview scale matches the
//! full-resolution export once both are seen at the same size (the export box-filtered down).

use super::*;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_media::{DemoScene, Generator, MediaSource};
use filmcraft_project::{Label, MediaClip, MediaRef, SequenceSettings, TrackKind, find_effect};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, TimeRange};

const FILM_LOOK_IDS: [&str; 9] = ["film_look", "film_grain", "halation", "bloom", "diffusion", "film_stock", "gate_weave", "film_damage", "lens_character"];

fn add(p: &mut Project, map: &mut SourceMap, g: GeneratorSource) -> ItemId {
    let info = g.info().clone();
    let generator = g.generator.clone();
    let id = p.add_item(
        &info.name.clone(),
        Label::Iris,
        ItemKind::Media(MediaClip {
            media: MediaRef::Generator(generator),
            info,
            interpret: Default::default(),
            mark_in: None,
            mark_out: None,
            markers: vec![],
            offline: false,
            proxy: None,
            identity: None,
        }),
        None,
    );
    map.0.insert(id, Arc::new(g) as SharedSource);
    id
}

/// A 640×360 sequence with one clip (`scene`) on V1 carrying `effect` (None: no effect).
fn project(scene: Option<[f32; 4]>, effect: Option<&str>, tweak: &[(&str, f64)]) -> (Project, ItemId, SourceMap) {
    let mut p = Project::new("fl");
    let mut map = SourceMap::default();
    let g = match scene {
        Some(color) => GeneratorSource::new(Generator::ColorMatte { color }, 640, 360, FrameRate::FPS_24, Tick(10 * TICKS_PER_SECOND)),
        None => GeneratorSource::demo(DemoScene::OceanSunset),
    };
    let item = add(&mut p, &mut map, g);
    let seq = p.new_sequence("s", SequenceSettings { width: 640, height: 360, frame_rate: FrameRate::FPS_24, ..Default::default() }, 1, 1, None);
    let r = FrameRate::FPS_24;
    let mut ti = p.make_track_item(item, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(48)), r).unwrap();
    ti.scale_to_frame = true;
    if let Some(id) = effect {
        let mut inst = find_effect(id).unwrap().instance();
        for (k, v) in tweak {
            inst.params.get_mut(*k).unwrap_or_else(|| panic!("{id} has no {k}")).value = filmcraft_project::ParamValue::Float(*v);
        }
        ti.effects.push(inst);
    }
    p.sequence_mut(seq).unwrap().video_tracks[0].items.push(ti);
    (p, seq, map)
}

/// Box-filter a premultiplied image down by an integer factor.
fn down(img: &Image, n: usize) -> Image {
    let (w, h) = (img.w / n, img.h / n);
    let mut out = Image::filled(w, h, [0.0; 4]);
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0f32; 4];
            for dy in 0..n {
                for dx in 0..n {
                    let p = img.get(x * n + dx, y * n + dy);
                    for k in 0..4 {
                        acc[k] += p[k];
                    }
                }
            }
            let o = (y * w + x) * 4;
            for k in 0..4 {
                out.px[o + k] = acc[k] / (n * n) as f32;
            }
        }
    }
    out
}

fn render(p: &Project, seq: ItemId, map: &SourceMap, f: i64, scale: f32) -> Image {
    render_sequence(p, seq, FrameRate::FPS_24.tick_of(f), RenderOptions { scale, ..Default::default() }, map)
}

/// The effect's contribution (with − without), green channel, as a flat vector.
fn delta(with: &Image, without: &Image) -> Vec<f32> {
    with.px.chunks(4).zip(without.px.chunks(4)).map(|(a, b)| a[1] - b[1]).collect()
}

fn std(v: &[f32]) -> f32 {
    let m = v.iter().sum::<f32>() / v.len() as f32;
    (v.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / v.len() as f32).sqrt()
}

#[test]
fn film_look_effects_fall_back_to_the_cpu_render_in_the_gpu_plan() {
    for id in FILM_LOOK_IDS {
        let (p, seq, map) = project(None, Some(id), &[]);
        for f in [0, 13] {
            let opts = RenderOptions { scale: 0.5, ..Default::default() };
            let t = FrameRate::FPS_24.tick_of(f);
            let plan = plan::plan_frame(&p, seq, t, opts, &map);
            if let plan::FramePlan::Layers { layers, .. } = &plan {
                assert!(layers.iter().all(|l| l.fx.is_none()), "{id}: no GPU effect stage may claim a Film Look effect");
            }
            let got = plan::execute_cpu(&plan);
            let reference = render_sequence(&p, seq, t, opts, &map);
            assert_eq!((got.w, got.h), (reference.w, reference.h));
            let max = got.px.iter().zip(&reference.px).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
            assert!(max < 0.02, "{id} frame {f}: GPU plan vs reference max diff {max}");
        }
    }
}

/// Grain seen in the half / quarter-resolution Program monitor has the strength of the export
/// seen at the same size (not the per-pixel strength of a full-resolution frame).
#[test]
fn film_grain_preview_strength_matches_the_export() {
    let grey = Some([0.18, 0.18, 0.18, 1.0]);
    let (p0, s0, m0) = project(grey, None, &[]);
    for (size, rough) in [(1.6, 45.0), (1.0, 100.0), (4.0, 0.0)] {
        let (p, seq, map) = project(grey, Some("film_grain"), &[("size", size), ("roughness", rough), ("chroma", 0.0)]);
        let full = render(&p, seq, &map, 5, 1.0);
        let full0 = render(&p0, s0, &m0, 5, 1.0);
        for n in [2usize, 4] {
            let scale = 1.0 / n as f32;
            let export = std(&delta(&down(&full, n), &down(&full0, n)));
            let preview = std(&delta(&render(&p, seq, &map, 5, scale), &render(&p0, s0, &m0, 5, scale)));
            let ratio = preview / export;
            println!("grain size {size} rough {rough} at 1/{n}: export {export:.5} preview {preview:.5} ratio {ratio:.3}");
            assert!((0.8..1.25).contains(&ratio), "size {size} rough {rough} at 1/{n}: preview/export grain std {ratio:.3}");
        }
    }
}

/// Halation (and Bloom, Diffusion) glows are sized in clip pixels: the preview's glow is the
/// export's glow, filtered down.
#[test]
fn glows_preview_matches_the_export() {
    let (p0, s0, m0) = project(None, None, &[]);
    for id in ["halation", "bloom", "diffusion", "film_look"] {
        let tweak: &[(&str, f64)] = if id == "film_look" { &[("grain", 0.0), ("weave", 0.0), ("damage", 0.0)] } else { &[] };
        let (p, seq, map) = project(None, Some(id), tweak);
        let full = delta(&down(&render(&p, seq, &map, 5, 1.0), 2), &down(&render(&p0, s0, &m0, 5, 1.0), 2));
        let half = delta(&render(&p, seq, &map, 5, 0.5), &render(&p0, s0, &m0, 5, 0.5));
        assert_eq!(full.len(), half.len());
        let energy = full.iter().map(|v| v.abs()).sum::<f32>() / full.len() as f32;
        let err = full.iter().zip(&half).map(|(a, b)| (a - b).abs()).sum::<f32>() / full.len() as f32;
        println!("{id}: mean |effect| {energy:.5}, mean |preview − export| {err:.5} ({:.1} %)", 100.0 * err / energy.max(1e-9));
        assert!(energy > 1e-4, "{id} did nothing on the sunset");
        assert!(err < 0.2 * energy + 1e-4, "{id}: preview differs from export by {err} (effect {energy})");
    }
}
