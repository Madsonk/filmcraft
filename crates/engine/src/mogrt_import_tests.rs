//! `.mogrt` (Premiere) → `.fcgt`: synthetic titles built in `mogrt_import::fixtures`.

use serde_json::json;

use crate::Session;
use crate::media_test_util::tmp_dir;
use crate::mogrt_fixtures::{IN, Spec, after_effects, premiere, zip_of};
use crate::mogrt_import::{MogrtError, MogrtKind, convert, kind};
use filmcraft_project::graphic::{SHAPE_LAYER, TEXT_LAYER};
use filmcraft_project::gtemplate::{ControlKind, GraphicsTemplate};
use filmcraft_project::{EffectInstance, Interpolation, ParamValue};
use filmcraft_time::{TICKS_PER_SECOND, Tick};

fn spec() -> Spec {
    Spec { text: "Hello Title", font: "NoSuchFontAtAll-Bold", second_run_size: Some(60.0), opacity_keys: true, extra_effect: true }
}

fn v(e: &EffectInstance, k: &str) -> ParamValue {
    e.params.get(k).map(|p| p.value.clone()).unwrap_or(ParamValue::Bool(false))
}

#[test]
#[allow(clippy::cognitive_complexity)] // one end-to-end scenario: import a title, check layers, look, animation
fn a_premiere_title_becomes_editable_layers_with_its_look_and_animation() {
    let c = convert(&premiere(&spec()).unwrap(), "Test.mogrt").unwrap();
    let t = &c.template;
    assert_eq!(t.name, "Test Lower Third");
    assert_eq!(t.canvas, [1920, 1080]);
    assert_eq!(t.duration, Tick(1_270_080_000_000));
    // paint order: Premiere lists the front layer first; FilmCraft paints in order
    assert_eq!(t.layers.iter().map(|l| l.effect.as_str()).collect::<Vec<_>>(), [SHAPE_LAYER, TEXT_LAYER]);
    let (shape, text) = (&t.layers[0], &t.layers[1]);
    assert_eq!(v(text, "text"), ParamValue::Text("Hello Title".into()));
    assert_eq!(v(text, "size"), ParamValue::Float(120.0));
    assert_eq!(v(text, "fill_color"), ParamValue::Color([1.0, 0.0, 0.0, 1.0]));
    assert_eq!(v(text, "tracking"), ParamValue::Float(50.0));
    assert_eq!(v(text, "shadow"), ParamValue::Bool(true));
    assert_eq!(v(text, "shadow_opacity"), ParamValue::Float(60.0));
    match v(text, "position") {
        ParamValue::Vec2(p) => assert!((p.x - 192.0).abs() < 1e-6 && (p.y - 864.0).abs() < 1e-6, "{p:?}"),
        o => panic!("{o:?}"),
    }
    // the second size run (from character 5)
    let runs = &text.layer.as_ref().unwrap().runs;
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!((runs[0].start, runs[0].end, runs[0].style.size), (5, 11, Some(60.0)));
    // missing font: FilmCraft's default, said
    assert_eq!(v(text, "font"), ParamValue::Text(filmcraft_text::fonts::DEFAULT_FAMILY.into()));
    assert_eq!(c.missing_fonts, ["NoSuchFontAtAll-Bold"]);
    assert!(c.warnings.iter().any(|w| w.contains("NoSuchFontAtAll-Bold") && w.contains("not installed")), "{:?}", c.warnings);
    // opacity fade: 0 → 100 over half a second, the second key eased
    let op = text.params.get("opacity").unwrap();
    assert_eq!(op.keyframes.len(), 2);
    assert_eq!(op.keyframes[0].time, Tick::ZERO);
    assert_eq!(op.keyframes[1].time, Tick(TICKS_PER_SECOND / 2));
    assert_eq!(op.keyframes[1].value, ParamValue::Float(100.0));
    assert_eq!(op.keyframes[1].interp, Interpolation::Bezier);
    let _ = IN;
    // the box: a 600×200 rectangle, centred, its anchor moved with it, non-uniform scale kept
    assert_eq!(v(shape, "shape"), ParamValue::Choice(0));
    match v(shape, "size") {
        ParamValue::Vec2(p) => assert_eq!((p.x, p.y), (600.0, 200.0)),
        o => panic!("{o:?}"),
    }
    match v(shape, "anchor") {
        ParamValue::Vec2(p) => assert_eq!((p.x, p.y), (-300.0, -100.0)),
        o => panic!("{o:?}"),
    }
    assert_eq!(v(shape, "scale_width"), ParamValue::Float(50.0));
    assert_eq!(v(shape, "uniform_scale"), ParamValue::Bool(false));
    assert_eq!(v(shape, "fill_color"), ParamValue::Color([81.0 / 255.0, 132.0 / 255.0, 200.0 / 255.0, 1.0]));
    // what does not translate is listed
    assert!(c.lost.iter().any(|l| l.contains("Wave Warp")), "{:?}", c.lost);
    assert!(t.description.contains("Wave Warp"));
    // editable: text, font and colour of the text, colour of the box
    let kinds: Vec<(u64, ControlKind)> = t.controls.iter().map(|c| (c.layer, c.kind)).collect();
    assert!(
        kinds.contains(&(2, ControlKind::Text))
            && kinds.contains(&(2, ControlKind::Font))
            && kinds.contains(&(2, ControlKind::Color))
            && kinds.contains(&(1, ControlKind::Color)),
        "{kinds:?}"
    );
    // a valid .fcgt that reads back the same
    let back = GraphicsTemplate::from_bytes(t.to_json().as_bytes()).unwrap();
    assert_eq!(&back, t);
}

#[test]
fn an_installed_font_is_kept_and_after_effects_titles_are_never_opened() {
    let c = convert(&premiere(&Spec { text: "Bold", font: "Inter-Bold", second_run_size: None, opacity_keys: false, extra_effect: false }).unwrap(), "x.mogrt")
        .unwrap();
    let text = c.template.layers.iter().find(|l| l.effect == TEXT_LAYER).unwrap();
    assert_eq!(v(text, "font"), ParamValue::Text("Inter".into()));
    assert!(matches!(v(text, "font_style"), ParamValue::Text(s) if s.contains("Bold")));
    assert!(c.missing_fonts.is_empty() && c.lost.is_empty(), "{:?} {:?}", c.missing_fonts, c.lost);
    assert_eq!(kind(&after_effects().unwrap()).unwrap(), MogrtKind::AfterEffects);
    assert_eq!(convert(&after_effects().unwrap(), "ae.mogrt").unwrap_err(), MogrtError::AfterEffects);
    assert!(matches!(convert(b"not a zip", "x").unwrap_err(), MogrtError::NotMogrt(_)));
    assert!(matches!(convert(&zip_of(&[("definition.json", b"{}")]).unwrap(), "x").unwrap_err(), MogrtError::NotMogrt(_)));
    // a Premiere package whose project is damaged: an error, never a panic
    let broken = zip_of(&[("definition.json", b"{}"), ("project.prgraphic", b"PK\x03\x04garbage")]).unwrap();
    assert!(matches!(convert(&broken, "x").unwrap_err(), MogrtError::Unreadable(_)));
}

#[test]
fn import_converts_once_installs_the_template_and_its_text_is_editable_after_apply() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let d = tmp_dir("mogrt-import");
    s.prefs_path = Some(d.join("prefs.json"));
    s.execute("playhead.set", json!({"seconds": 600})).unwrap();
    let m = d.join("Title.mogrt");
    std::fs::write(&m, premiere(&spec()).unwrap()).unwrap();
    let r = s.execute("graphics.template.importMogrt", json!({"path": m.to_string_lossy()})).unwrap();
    assert_eq!(r["cached"], false, "{r}");
    assert_eq!(r["layers"], 2);
    assert!(r["missingFonts"][0].as_str().unwrap().contains("NoSuchFont"));
    let path = r["path"].as_str().unwrap().to_string();
    assert!(path.ends_with(".fcgt") && std::path::Path::new(&path).exists());
    let again = s.execute("graphics.template.importMogrt", json!({"path": m.to_string_lossy()})).unwrap();
    assert_eq!(again["cached"], true, "{again}");
    assert_eq!(again["id"], r["id"]);
    let list = s.execute("graphics.template.list", json!({})).unwrap();
    assert!(list.to_string().contains("Test Lower Third"), "{list}");
    let a = s.execute("graphics.template.apply", json!({"template": r["id"]})).unwrap();
    let clip = a["clip"].as_u64().unwrap();
    s.execute("graphics.template.set", json!({"clip": clip, "control": "text-2", "value": "Madson Kurtis"})).unwrap();
    let q = s.active_sequence().unwrap();
    let it = q.find_item(filmcraft_project::ClipId(clip)).unwrap().1;
    let txt = it.effects.iter().find(|e| e.effect == TEXT_LAYER).map(|e| v(e, "text"));
    assert_eq!(txt, Some(ParamValue::Text("Madson Kurtis".into())));
    // applying the .mogrt path directly converts (or reuses) it too
    let b = s.execute("graphics.template.apply", json!({"template": m.to_string_lossy()})).unwrap();
    assert_eq!(b["template"], r["id"]);
}
