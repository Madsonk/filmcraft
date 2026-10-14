//! `.mogrt` titles made in Premiere → native FilmCraft graphics templates (`.fcgt`, titles-native).
//!
//! A `.mogrt` is a ZIP: `definition.json` (name, size, duration, editable controls) and the template
//! itself. Two kinds exist:
//!
//! - **Premiere** (`project.prgraphic`): a ZIP holding a Premiere project (gzipped XML). Its graphic
//!   clip lists text and shape layers whose properties are plain values or JSON in the XML (text,
//!   font, size, fill, stroke, shadow, background, per-character styles, path vertices) and
//!   keyframes of position, scale, rotation and opacity. FilmCraft's graphic layers have the same
//!   model, so these become a `.fcgt` whose text, colours and fonts are editable.
//! - **After Effects** (`project.aegraphic`): an After Effects project (binary). It is never
//!   opened; such a title stays "preview only".
//!
//! Read only: the source file is never written. Fonts are never copied into the template (they are
//! not ours to redistribute): a font this Mac does not have falls back to FilmCraft's default and
//! the conversion says so. What does not translate (masks, clip effects such as Crop or Wave Warp,
//! a moving clip Motion, gradients) is listed in the result and the template's description.

use std::collections::HashMap;
use std::io::Read;

use filmcraft_geom::Vec2;
use filmcraft_project::graphic::{SHAPE_LAYER, TEXT_LAYER, new_shape_layer, new_text_layer};
use filmcraft_project::gtemplate::{ControlKind, GraphicsTemplate, TemplateControl, slug};
use filmcraft_project::{CharStyle, EffectInstance, Interpolation, Keyframe, Param, ParamValue, StyleRun};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde_json::Value;

/// Largest member read out of a `.mogrt` (ZIP bombs).
const MAX_MEMBER: u64 = 64 << 20;

/// Why a `.mogrt` cannot become a native title.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum MogrtError {
    #[error("this title is an After Effects template: FilmCraft cannot convert it (it would need After Effects); it stays “preview only”")]
    AfterEffects,
    #[error("not a .mogrt title: {0}")]
    NotMogrt(String),
    #[error("this Premiere title could not be read: {0}")]
    Unreadable(String),
}

/// Which application made a `.mogrt`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MogrtKind {
    Premiere,
    AfterEffects,
}

/// A converted title, and what the conversion could not keep.
#[derive(Clone, Debug)]
pub struct Converted {
    pub template: GraphicsTemplate,
    /// Things that render differently (missing fonts, masks rendered as plain text, gradients).
    pub warnings: Vec<String>,
    /// Effects and animations left out.
    pub lost: Vec<String>,
    /// Fonts of the title this Mac does not have (replaced by FilmCraft's default).
    pub missing_fonts: Vec<String>,
}

fn read_member<R: Read + std::io::Seek>(z: &mut zip::ZipArchive<R>, name: &str) -> Result<Vec<u8>, MogrtError> {
    let f = z.by_name(name).map_err(|e| MogrtError::NotMogrt(format!("{name}: {e}")))?;
    if f.size() > MAX_MEMBER {
        return Err(MogrtError::Unreadable(format!("{name} is too large")));
    }
    let mut out = Vec::new();
    f.take(MAX_MEMBER).read_to_end(&mut out).map_err(|e| MogrtError::Unreadable(format!("{name}: {e}")))?;
    Ok(out)
}

/// The kind of a `.mogrt` (reads the ZIP's directory only).
pub fn kind(bytes: &[u8]) -> Result<MogrtKind, MogrtError> {
    let z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| MogrtError::NotMogrt(e.to_string()))?;
    let names: Vec<&str> = z.file_names().collect();
    if names.contains(&"project.prgraphic") {
        Ok(MogrtKind::Premiere)
    } else if names.contains(&"project.aegraphic") {
        Ok(MogrtKind::AfterEffects)
    } else {
        Err(MogrtError::NotMogrt("no project.prgraphic or project.aegraphic inside".into()))
    }
}

/// `definition.json` text in the en_US locale, or the first one.
fn localized(v: &Value) -> String {
    match v.get("strDB").and_then(Value::as_array) {
        Some(a) => a
            .iter()
            .find(|e| e.get("localeString").and_then(Value::as_str) == Some("en_US"))
            .or_else(|| a.first())
            .and_then(|e| e.get("str"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        None => v.as_str().unwrap_or_default().to_string(),
    }
}

// ------------------------------------------------------------------------------------------------
// Premiere project XML
// ------------------------------------------------------------------------------------------------

struct Doc<'a> {
    by_id: HashMap<&'a str, roxmltree::Node<'a, 'a>>,
    /// Binary values by `BinaryHash`: Premiere writes identical blobs once and leaves the later
    /// copies empty with the same hash.
    blobs: HashMap<&'a str, &'a str>,
}

fn child<'a>(n: roxmltree::Node<'a, 'a>, tag: &str) -> Option<roxmltree::Node<'a, 'a>> {
    n.children().find(|c| c.has_tag_name(tag))
}

fn child_text<'a>(n: roxmltree::Node<'a, 'a>, tag: &str) -> Option<&'a str> {
    child(n, tag).and_then(|c| c.text()).map(str::trim)
}

/// First descendant with `tag`.
fn find<'a>(n: roxmltree::Node<'a, 'a>, tag: &str) -> Option<roxmltree::Node<'a, 'a>> {
    n.descendants().find(|c| c.has_tag_name(tag))
}

impl<'a> Doc<'a> {
    fn new(doc: &'a roxmltree::Document<'a>) -> Doc<'a> {
        let mut by_id = HashMap::new();
        let mut blobs = HashMap::new();
        for n in doc.descendants().filter(|n| n.is_element()) {
            if let Some(id) = n.attribute("ObjectID").or_else(|| n.attribute("ObjectUID")) {
                by_id.insert(id, n);
            }
            if let (Some(h), Some(t)) = (n.attribute("BinaryHash"), n.text().map(str::trim).filter(|t| !t.is_empty())) {
                blobs.entry(h).or_insert(t);
            }
        }
        Doc { by_id, blobs }
    }
    fn deref(&self, n: roxmltree::Node<'a, 'a>) -> Option<roxmltree::Node<'a, 'a>> {
        n.attribute("ObjectRef").or_else(|| n.attribute("ObjectURef")).and_then(|r| self.by_id.get(r).copied())
    }
}

/// One parameter of a Premiere component: its static value and keyframes (raw strings).
#[derive(Clone, Debug, Default)]
struct PParam {
    name: String,
    control: i64,
    start: Option<String>,
    keys: Option<String>,
    blob: Option<Vec<u8>>,
}

impl PParam {
    /// The `value` field of a keyframe string `time,value,…`.
    fn value_of(s: &str) -> Option<&str> {
        s.split(',').nth(1)
    }
    fn static_str(&self) -> Option<&str> {
        self.start.as_deref().and_then(Self::value_of)
    }
    fn f64(&self) -> Option<f64> {
        self.static_str().and_then(|v| v.trim_end_matches('.').parse().ok())
    }
    fn bool(&self) -> Option<bool> {
        self.static_str().map(|v| v == "true")
    }
    /// The blob as JSON (Premiere stores UTF-16 JSON after an 8-byte header).
    fn json(&self) -> Option<Value> {
        let b = self.blob.as_ref()?;
        let body = b.get(8..)?;
        let units: Vec<u16> = body.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        let s = String::from_utf16_lossy(&units);
        serde_json::from_str(s.trim_end_matches('\0')).ok()
    }
    /// Keyframes: (time, value string, interpolation code).
    fn keyframes(&self) -> Vec<(i64, String, i64)> {
        let Some(k) = self.keys.as_deref() else { return Vec::new() };
        k.split(';')
            .filter(|s| !s.trim().is_empty())
            .filter_map(|kf| {
                let f: Vec<&str> = kf.split(',').collect();
                let t = f.first()?.trim().parse::<i64>().ok()?;
                let v = f.get(1)?.to_string();
                let code = f.get(2).and_then(|c| c.parse::<i64>().ok()).unwrap_or(0);
                Some((t, v, code))
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug)]
struct PinInfo {
    size: (f64, f64),
    edges: [bool; 4],
}

struct PComponent {
    match_name: String,
    display: String,
    instance: String,
    params: Vec<PParam>,
}

impl PComponent {
    fn param(&self, name: &str) -> Option<&PParam> {
        self.params.iter().find(|p| p.name == name)
    }
    /// Responsive Design – Position: the size of the layer this one is pinned to (`Parent Width` /
    /// `Parent Height`, > 0 when pinned) and the four edge boxes after `end`. Premiere then stores
    /// the layer's position relative to the top-left corner of that parent.
    fn pin(&self) -> Option<PinInfo> {
        let w = self.param("Parent Width").and_then(PParam::f64).unwrap_or(0.0);
        let h = self.param("Parent Height").and_then(PParam::f64).unwrap_or(0.0);
        if w <= 0.0 && h <= 0.0 {
            return None;
        }
        let i = self.params.iter().position(|p| p.name == "end")?;
        let boxes: Vec<bool> = self.params[i + 1..].iter().take_while(|p| p.control == 4).map(|p| p.bool().unwrap_or(false)).collect();
        let edge = |k: usize| boxes.get(k).copied().unwrap_or(false);
        Some(PinInfo { size: (w, h), edges: [edge(0), edge(1), edge(2), edge(3)] })
    }
    /// The "Uniform Scale" checkbox: the unnamed checkbox right after Horizontal Scale.
    fn uniform(&self) -> bool {
        let i = self.params.iter().position(|p| p.name == "Horizontal Scale");
        i.and_then(|i| self.params.get(i + 1)).filter(|p| p.control == 4).and_then(PParam::bool).unwrap_or(true)
    }
}

fn read_params<'a>(d: &Doc<'a>, comp: roxmltree::Node<'a, 'a>) -> Vec<PParam> {
    let Some(params) = find(comp, "Params") else { return Vec::new() };
    params
        .children()
        .filter(|c| c.has_tag_name("Param"))
        .filter_map(|p| d.deref(p))
        .map(|pe| {
            let blob = child(pe, "StartKeyframeValue")
                .filter(|v| v.attribute("Encoding") == Some("base64"))
                .and_then(|v| v.text().map(str::trim).filter(|t| !t.is_empty()).or_else(|| v.attribute("BinaryHash").and_then(|h| d.blobs.get(h).copied())))
                .and_then(|t| filmcraft_project::gtemplate::base64_decode(&t.split_whitespace().collect::<String>()));
            let keys = child_text(pe, "Keyframes").filter(|k| !k.is_empty()).map(str::to_string);
            // an animated blob (colour, path) has no start value: the first keyframe's
            let blob = blob.or_else(|| {
                let first = keys.as_deref()?.split(';').next()?;
                filmcraft_project::gtemplate::base64_decode(first.split(',').nth(1)?)
            });
            PParam {
                name: child_text(pe, "Name").unwrap_or_default().to_string(),
                control: child_text(pe, "ParameterControlType").and_then(|v| v.parse().ok()).unwrap_or(-1),
                start: child_text(pe, "StartKeyframe").map(str::to_string),
                keys,
                blob,
            }
        })
        .collect()
}

/// The graphic clip of the project: (duration, media in point, frame size, components in order).
fn graphic_clip<'a>(d: &Doc<'a>, doc: &'a roxmltree::Document<'a>) -> Result<(i64, i64, [u32; 2], Vec<PComponent>), MogrtError> {
    let item = doc.descendants().find(|n| n.has_tag_name("VideoClipTrackItem")).ok_or_else(|| MogrtError::Unreadable("no graphic clip".into()))?;
    let start: i64 = find(item, "Start").and_then(|n| n.text()).and_then(|t| t.trim().parse().ok()).unwrap_or(0);
    let end: i64 = find(item, "End").and_then(|n| n.text()).and_then(|t| t.trim().parse().ok()).unwrap_or(5 * TICKS_PER_SECOND);
    let frame = child_text(item, "FrameRect")
        .map(|r| r.split(',').filter_map(|v| v.trim().parse::<u32>().ok()).collect::<Vec<_>>())
        .filter(|v| v.len() == 4 && v[2] > v[0] && v[3] > v[1])
        .map(|v| [v[2] - v[0], v[3] - v[1]])
        .unwrap_or([1920, 1080]);
    // media in point: TrackItem → SubClip → Clip (VideoClip) → InPoint
    let in_point = find(item, "SubClip")
        .and_then(|s| d.deref(s))
        .and_then(|s| child(s, "Clip"))
        .and_then(|c| d.deref(c))
        .and_then(|c| find(c, "InPoint"))
        .and_then(|n| n.text())
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(0);
    let chain = find(item, "Components").and_then(|c| d.deref(c)).ok_or_else(|| MogrtError::Unreadable("the graphic clip has no components".into()))?;
    let comps_node = find(chain, "Components").ok_or_else(|| MogrtError::Unreadable("empty component chain".into()))?;
    let mut comps = Vec::new();
    for c in comps_node.children().filter(|c| c.has_tag_name("Component")) {
        let Some(vfc) = d.deref(c) else { continue };
        let inner = child(vfc, "Component").unwrap_or(vfc);
        comps.push(PComponent {
            match_name: child_text(vfc, "MatchName").unwrap_or_default().to_string(),
            display: child_text(inner, "DisplayName").unwrap_or_default().to_string(),
            instance: child_text(inner, "InstanceName").unwrap_or_default().replace('\r', " "),
            params: read_params(d, inner),
        });
    }
    Ok(((end - start).max(TICKS_PER_SECOND / 10), in_point, frame, comps))
}

// ------------------------------------------------------------------------------------------------
// Values
// ------------------------------------------------------------------------------------------------

/// Premiere colour integer 0xRRGGBB → sRGB RGBA.
fn color(v: &Value, alpha: f64) -> Option<[f32; 4]> {
    let c = v.as_u64()?;
    Some([((c >> 16) & 255) as f32 / 255.0, ((c >> 8) & 255) as f32 / 255.0, (c & 255) as f32 / 255.0, alpha.clamp(0.0, 1.0) as f32])
}

/// A style-sheet property: the value at character 0 and the later runs `(start, value)`.
fn sheet<'v>(ss: &'v Value, key: &str) -> (Option<&'v Value>, Vec<(usize, &'v Value)>) {
    let Some(a) = ss.get(key).and_then(|v| v.get("mParamValues")).and_then(Value::as_array) else { return (None, Vec::new()) };
    let mut runs: Vec<(usize, &Value)> = a.iter().filter_map(|p| p.as_array()).filter_map(|p| Some((p.first()?.as_u64()? as usize, p.get(1)?))).collect();
    runs.sort_by_key(|r| r.0);
    let base = runs.first().filter(|r| r.0 == 0).map(|r| r.1);
    let rest = runs.into_iter().filter(|r| r.0 > 0).collect();
    (base, rest)
}

fn set(e: &mut EffectInstance, id: &str, v: ParamValue) {
    e.params.insert(id.to_string(), Param::new(v));
}

/// Split a PostScript font name into family + style candidates (`BebasNeue-Bold` →
/// [("BebasNeue","Bold"), ("Bebas Neue","Bold")]).
fn font_candidates(ps: &str) -> Vec<(String, String)> {
    let (fam, style) = match ps.rsplit_once('-') {
        Some((f, s)) => (f.to_string(), s.to_string()),
        None => (ps.to_string(), "Regular".to_string()),
    };
    let spaced = |s: &str| {
        let mut o = String::new();
        let cs: Vec<char> = s.chars().collect();
        for (i, c) in cs.iter().enumerate() {
            if i > 0 && c.is_uppercase() && (cs[i - 1].is_lowercase() || cs.get(i + 1).is_some_and(|n| n.is_lowercase()) && cs[i - 1].is_uppercase()) {
                o.push(' ');
            }
            o.push(*c);
        }
        o
    };
    let style = spaced(&style);
    let mut v = vec![(fam.clone(), style.clone()), (spaced(&fam), style.clone())];
    if style == "Regular" {
        v.push((ps.replace('-', " "), "Regular".into()));
    }
    v.dedup();
    v
}

/// Premiere sometimes stores a font id (a GUID) instead of a name for the run after the last
/// character: not a font.
fn is_font_id(ps: &str) -> bool {
    ps.len() == 36 && ps.chars().filter(|c| *c == '-').count() == 4 && ps.chars().all(|c| c == '-' || c.is_ascii_hexdigit())
}

/// The font FilmCraft will use for PostScript name `ps`, and whether it is the real one.
fn resolve_font(ps: &str) -> (String, String, bool) {
    if ps.is_empty() || is_font_id(ps) {
        return (filmcraft_text::fonts::DEFAULT_FAMILY.into(), "Regular".into(), true);
    }
    for (fam, style) in font_candidates(ps) {
        let r = filmcraft_text::fonts::resolve(&fam, &style);
        if !r.missing {
            let f = filmcraft_text::fonts::face(r.face);
            return (f.info.family.clone(), f.info.style.clone(), true);
        }
    }
    let style = font_candidates(ps).first().map(|c| c.1.clone()).unwrap_or_else(|| "Regular".into());
    (filmcraft_text::fonts::DEFAULT_FAMILY.into(), style, false)
}

/// Keyframed or static parameter → FilmCraft param (value mapped by `f`).
fn param_from(p: &PParam, in_point: i64, f: impl Fn(&str) -> Option<ParamValue>) -> Option<Param> {
    let mut out = Param::new(p.static_str().and_then(&f)?);
    for (t, v, code) in p.keyframes() {
        let Some(val) = f(&v) else { continue };
        let mut k = Keyframe::new(Tick((t - in_point).max(0)), val);
        k.interp = if code == 5 { Interpolation::Bezier } else { Interpolation::Linear };
        out.keyframes.push(k);
    }
    out.keyframes.sort_by_key(|k| k.time);
    out.keyframes.dedup_by_key(|k| k.time);
    if let Some(k) = out.keyframes.first() {
        out.value = k.value.clone();
    }
    Some(out)
}

fn num(s: &str) -> Option<f64> {
    s.trim().trim_end_matches('.').parse().ok()
}

fn point(s: &str, frame: [u32; 2]) -> Option<ParamValue> {
    let (x, y) = s.split_once(':')?;
    Some(ParamValue::Vec2(Vec2::new(num(x)? * frame[0] as f64, num(y)? * frame[1] as f64)))
}

/// Transform of a text / shape component → layer params. `offset` moves the anchor (shapes whose
/// path is drawn around the origin in FilmCraft but from the corner in Premiere).
fn transform(e: &mut EffectInstance, c: &PComponent, in_point: i64, frame: [u32; 2], offset: Vec2) {
    if let Some(p) = c.param("Position").and_then(|p| param_from(p, in_point, |s| point(s, frame))) {
        e.params.insert("position".into(), p);
    }
    if let Some(p) = c.param("Anchor Point").and_then(|p| {
        param_from(p, in_point, |s| match point(s, frame)? {
            ParamValue::Vec2(v) => Some(ParamValue::Vec2(Vec2::new(v.x - offset.x, v.y - offset.y))),
            o => Some(o),
        })
    }) {
        e.params.insert("anchor".into(), p);
    }
    let fl = |name: &str| c.param(name).and_then(|p| param_from(p, in_point, |s| num(s).map(ParamValue::Float)));
    for (from, to) in [("Scale", "scale"), ("Horizontal Scale", "scale_width"), ("Rotation", "rotation"), ("Opacity", "opacity")] {
        if let Some(p) = fl(from) {
            e.params.insert(to.into(), p);
        }
    }
    set(e, "uniform_scale", ParamValue::Bool(c.uniform()));
}

/// Fill / stroke / shadow / background shared by text (`mTextParam` + style sheet) and shapes
/// (`mStyle`).
fn appearance(e: &mut EffectInstance, style: &Value, sheet_v: Option<&Value>, warnings: &mut Vec<String>, what: &str) {
    let get = |k: &str| -> Option<&Value> {
        match sheet_v {
            Some(ss) => sheet(ss, k).0.or_else(|| style.get(k)),
            None => style.get(k),
        }
    };
    if let Some(c) = get("mFillColor").and_then(|v| color(v, 1.0)) {
        set(e, "fill_color", ParamValue::Color(c));
    }
    if let Some(v) = get("mFillVisible").and_then(Value::as_bool) {
        set(e, "fill", ParamValue::Bool(v));
    }
    if get("mFillColorType").and_then(Value::as_u64).unwrap_or(0) != 0 {
        warnings.push(format!("{what}: gradient fill shown as a plain colour"));
    }
    if let Some(v) = get("mStrokeVisible").and_then(Value::as_bool) {
        set(e, "stroke", ParamValue::Bool(v));
    }
    if let Some(c) = get("mStrokeColor").and_then(|v| color(v, 1.0)) {
        set(e, "stroke_color", ParamValue::Color(c));
    }
    if let Some(w) = get("mStrokeWidth").and_then(Value::as_f64) {
        set(e, "stroke_width", ParamValue::Float(w));
    }
    let f = |k: &str| style.get(k).and_then(Value::as_f64);
    if let Some(v) = style.get("mShadowVisible").and_then(Value::as_bool) {
        set(e, "shadow", ParamValue::Bool(v));
    }
    if let Some(c) = style.get("mShadowColor").and_then(|v| color(v, 1.0)) {
        set(e, "shadow_color", ParamValue::Color(c));
    }
    for (k, to) in [
        ("mShadowOpacity", "shadow_opacity"),
        ("mShadowAngle", "shadow_angle"),
        ("mShadowOffset", "shadow_distance"),
        ("mShadowSize", "shadow_size"),
        ("mShadowBlur", "shadow_blur"),
    ] {
        if let Some(v) = f(k) {
            set(e, to, ParamValue::Float(v));
        }
    }
    if let Some(v) = style.get("mBackFillVisible").and_then(Value::as_bool) {
        set(e, "background", ParamValue::Bool(v));
    }
    if let Some(c) = style.get("mBackFillColor").and_then(|v| color(v, 1.0)) {
        set(e, "background_color", ParamValue::Color(c));
    }
    for (k, to) in [("mBackFillOpacity", "background_opacity"), ("mBackFillSize", "background_size")] {
        if let Some(v) = f(k) {
            set(e, to, ParamValue::Float(v));
        }
    }
    if style.get("mIsMask").and_then(Value::as_bool) == Some(true) {
        warnings.push(format!("{what}: used as a mask in the original (cut-out); shown as a plain layer"));
    }
}

/// A text component → text layer (+ font warnings).
fn text_layer(c: &PComponent, in_point: i64, frame: [u32; 2], out: &mut Converted) -> Option<EffectInstance> {
    let src = c.param("Source Text")?.json()?;
    let tp = src.get("mTextParam")?;
    let ss = tp.get("mStyleSheet")?;
    let text = ss.get("mText").and_then(Value::as_str).unwrap_or_default().replace("\r\n", "\n").replace('\r', "\n");
    let base_size = sheet(ss, "mFontSize").0.and_then(Value::as_f64).unwrap_or(100.0);
    let mut e = new_text_layer(&text, Vec2::new(0.0, 0.0), base_size);
    let name = if c.instance.trim().is_empty() { text.lines().next().unwrap_or("Text").to_string() } else { c.instance.trim().to_string() };
    set(&mut e, "name", ParamValue::Text(name.clone()));
    let font_of = |ps: &str, out: &mut Converted| -> (String, String) {
        let (fam, style, found) = resolve_font(ps);
        if !found && !out.missing_fonts.iter().any(|m| m == ps) {
            out.missing_fonts.push(ps.to_string());
            out.warnings.push(format!("font “{ps}” is not installed: {fam} is used instead"));
        }
        (fam, style)
    };
    if let Some(ps) = sheet(ss, "mFontName").0.and_then(Value::as_str) {
        let (fam, style) = font_of(ps, out);
        set(&mut e, "font", ParamValue::Text(fam));
        set(&mut e, "font_style", ParamValue::Text(style));
    }
    let f = |k: &str| sheet(ss, k).0.and_then(Value::as_f64);
    let b = |k: &str| sheet(ss, k).0.and_then(Value::as_bool);
    if let Some(v) = f("mTracking") {
        set(&mut e, "tracking", ParamValue::Float(v));
    }
    if let Some(v) = f("mBaselineShift") {
        set(&mut e, "baseline_shift", ParamValue::Float(v));
    }
    if let Some(v) = b("mFauxBold") {
        set(&mut e, "faux_bold", ParamValue::Bool(v));
    }
    if let Some(v) = b("mFauxItalic") {
        set(&mut e, "faux_italic", ParamValue::Bool(v));
    }
    if let Some(v) = b("mUnderline") {
        set(&mut e, "underline", ParamValue::Bool(v));
    }
    if let Some(v) = sheet(ss, "mCapsOption").0.and_then(Value::as_u64) {
        set(&mut e, "caps", ParamValue::Choice(v.min(2) as u32));
    }
    // Premiere: 0 left, 1 right, 2 centre (Essential Graphics order) → FilmCraft 0 left, 1 centre, 2 right
    let align = match tp.get("mAlignment").and_then(Value::as_u64).unwrap_or(0) {
        1 => 2,
        2 => 1,
        3..=6 => 3,
        _ => 0,
    };
    set(&mut e, "align", ParamValue::Choice(align));
    if let Some(v) = tp.get("mLeading").and_then(Value::as_f64).filter(|v| *v != 0.0) {
        set(&mut e, "leading", ParamValue::Float(v));
    }
    if let Some(v) = tp.get("mLigatures").and_then(Value::as_bool) {
        set(&mut e, "ligatures", ParamValue::Bool(v));
    }
    if tp.get("mIsVerticalText").and_then(Value::as_bool) == Some(true) {
        set(&mut e, "vertical", ParamValue::Bool(true));
    }
    let (w, h) = (tp.get("mWidth").and_then(Value::as_f64).unwrap_or(0.0), tp.get("mHeight").and_then(Value::as_f64).unwrap_or(0.0));
    if w > 0.0 {
        set(&mut e, "box_width", ParamValue::Float(w));
        set(&mut e, "box_height", ParamValue::Float(h.max(0.0)));
    }
    appearance(&mut e, tp, Some(ss), &mut out.warnings, &format!("text “{name}”"));
    // per-character styles after character 0
    let mut runs: Vec<StyleRun> = Vec::new();
    let n = text.chars().count();
    let mut starts: Vec<usize> = Vec::new();
    for k in ["mFontSize", "mFontName", "mFillColor", "mTracking", "mFauxBold", "mFauxItalic", "mBaselineShift", "mCapsOption"] {
        starts.extend(sheet(ss, k).1.iter().map(|r| r.0));
    }
    starts.sort_unstable();
    starts.dedup();
    let at = |k: &str, i: usize| -> Option<Value> {
        let (b0, rest) = sheet(ss, k);
        rest.iter().rev().find(|r| r.0 <= i).map(|r| r.1.clone()).or_else(|| b0.cloned())
    };
    for (j, &s) in starts.iter().enumerate() {
        if s >= n {
            continue;
        }
        let end = starts.get(j + 1).copied().unwrap_or(n).min(n);
        let mut st = CharStyle::default();
        let differs = |k: &str| at(k, s) != sheet(ss, k).0.cloned();
        if differs("mFontSize") {
            st.size = at("mFontSize", s).and_then(|v| v.as_f64()).map(|v| v as f32);
        }
        if differs("mFontName")
            && let Some(ps) = at("mFontName", s).and_then(|v| v.as_str().map(str::to_string)).filter(|ps| !is_font_id(ps))
        {
            let (fam, style) = font_of(&ps, out);
            st.font = Some(fam);
            st.font_style = Some(style);
        }
        if differs("mFillColor") {
            st.fill = at("mFillColor", s).and_then(|v| color(&v, 1.0));
        }
        if differs("mTracking") {
            st.tracking = at("mTracking", s).and_then(|v| v.as_f64()).map(|v| v as f32);
        }
        if differs("mFauxBold") {
            st.faux_bold = at("mFauxBold", s).and_then(|v| v.as_bool());
        }
        if differs("mFauxItalic") {
            st.faux_italic = at("mFauxItalic", s).and_then(|v| v.as_bool());
        }
        if differs("mBaselineShift") {
            st.baseline_shift = at("mBaselineShift", s).and_then(|v| v.as_f64()).map(|v| v as f32);
        }
        if differs("mCapsOption") {
            st.caps = at("mCapsOption", s).and_then(|v| v.as_u64()).map(|v| v.min(2) as u32);
        }
        if !st.is_empty() && end > s {
            runs.push(StyleRun { start: s, end, style: st });
        }
    }
    e.layer.get_or_insert_with(Default::default).runs = runs;
    transform(&mut e, c, in_point, frame, Vec2::new(0.0, 0.0));
    Some(e)
}

/// The vertices of a Premiere shape path: (vertex, in tangent, out tangent) per vertex, in
/// layer pixels, and whether it is closed. Layout (little endian): u32 version, u32 count, then
/// per vertex u32 flags (1 = smooth) + 6 f32, then a closed byte.
fn path_vertices(b: &[u8]) -> Option<(Vec<[[f32; 2]; 3]>, bool)> {
    let u32_at = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]));
    let f32_at = |o: usize| b.get(o..o + 4).map(|s| f32::from_le_bytes([s[0], s[1], s[2], s[3]]));
    let n = u32_at(4)? as usize;
    if n == 0 || n > 10_000 || b.len() < 8 + n * 28 {
        return None;
    }
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let o = 8 + i * 28 + 4;
        let p = |k: usize| -> Option<[f32; 2]> { Some([f32_at(o + k * 8)?, f32_at(o + k * 8 + 4)?]) };
        let (a, c, d) = (p(0)?, p(1)?, p(2)?);
        if !a.iter().chain(&c).chain(&d).all(|x| x.is_finite()) {
            return None;
        }
        v.push([a, c, d]);
    }
    let closed = b.get(8 + n * 28).is_none_or(|x| *x != 0);
    Some((v, closed))
}

/// Flatten a cubic path (tangents absolute) into points.
fn flatten(v: &[[[f32; 2]; 3]], closed: bool) -> Vec<[f32; 2]> {
    let mut out = Vec::new();
    let segs = if closed { v.len() } else { v.len().saturating_sub(1) };
    for i in 0..segs {
        let (a, b) = (&v[i], &v[(i + 1) % v.len()]);
        let (p0, p1, p2, p3) = (a[0], a[2], b[1], b[0]);
        let straight = p1 == p0 && p2 == p3;
        let steps = if straight { 1 } else { 16 };
        for s in 0..steps {
            let t = s as f32 / steps as f32;
            let u = 1.0 - t;
            let q = |k: usize| u * u * u * p0[k] + 3.0 * u * u * t * p1[k] + 3.0 * u * t * t * p2[k] + t * t * t * p3[k];
            out.push([q(0), q(1)]);
        }
    }
    if !closed && let Some(l) = v.last() {
        out.push(l[0]);
    }
    out
}

/// A shape component → shape layer. Axis-aligned 4-vertex paths become rectangles, others a path.
fn shape_layer(c: &PComponent, in_point: i64, frame: [u32; 2], out: &mut Converted) -> Option<EffectInstance> {
    let name = if c.instance.trim().is_empty() { "Shape".to_string() } else { c.instance.trim().to_string() };
    let Some((verts, closed)) = c.param("Path").and_then(|p| p.blob.as_deref()).and_then(path_vertices) else {
        out.lost.push(format!("shape “{name}”: path not readable"));
        return None;
    };
    let pts = flatten(&verts, closed);
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in &pts {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    if !(x1 > x0 && y1 > y0) {
        out.lost.push(format!("shape “{name}”: empty path"));
        return None;
    }
    let centre = Vec2::new(((x0 + x1) / 2.0) as f64, ((y0 + y1) / 2.0) as f64);
    let is_rect = verts.len() == 4
        && verts.iter().all(|v| v[0] == v[1] && v[2] == v[0])
        && verts.iter().all(|v| (v[0][0] == x0 || v[0][0] == x1) && (v[0][1] == y0 || v[0][1] == y1));
    let size = Vec2::new((x1 - x0) as f64, (y1 - y0) as f64);
    let mut e = if is_rect {
        new_shape_layer(0, Vec2::new(0.0, 0.0), size, Vec::new())
    } else {
        // the path in layer pixels, centred like FilmCraft's shapes
        let local: Vec<[f32; 2]> = pts.iter().map(|p| [p[0] - centre.x as f32, p[1] - centre.y as f32]).collect();
        new_shape_layer(3, Vec2::new(0.0, 0.0), size, local)
    };
    set(&mut e, "name", ParamValue::Text(name.clone()));
    if let Some(st) = c.param("Appearance").and_then(PParam::json)
        && let Some(style) = st.get("mStyle")
    {
        appearance(&mut e, style, None, &mut out.warnings, &format!("shape “{name}”"));
    }
    transform(&mut e, c, in_point, frame, centre);
    Some(e)
}

/// Clip opacity folded into every layer (keyframes multiplied at the union of key times).
fn fold_opacity(layers: &mut [EffectInstance], clip: &PComponent, in_point: i64) -> bool {
    let Some(p) = clip.param("Opacity").and_then(|p| param_from(p, in_point, |s| num(s).map(ParamValue::Float))) else { return false };
    let f = |q: &Param, t: Tick| match q.value_at(t) {
        ParamValue::Float(v) => v,
        _ => 100.0,
    };
    if p.keyframes.is_empty() && (f(&p, Tick::ZERO) - 100.0).abs() < 1e-6 {
        return false;
    }
    for l in layers.iter_mut() {
        let own = l.params.get("opacity").cloned().unwrap_or_else(|| Param::new(ParamValue::Float(100.0)));
        let mut times: Vec<Tick> = own.keyframes.iter().chain(&p.keyframes).map(|k| k.time).collect();
        times.sort();
        times.dedup();
        let mut out = Param::new(ParamValue::Float(f(&own, Tick::ZERO) * f(&p, Tick::ZERO) / 100.0));
        for t in times {
            let mut k = Keyframe::new(t, ParamValue::Float(f(&own, t) * f(&p, t) / 100.0));
            k.interp = p.keyframes.iter().chain(&own.keyframes).find(|x| x.time == t).map(|x| x.interp).unwrap_or_default();
            out.keyframes.push(k);
        }
        l.params.insert("opacity".into(), out);
    }
    true
}

/// Whether the clip Motion is the identity (centre, 100 %, no rotation), never animated.
fn motion_is_identity(c: &PComponent) -> bool {
    let animated = c.params.iter().any(|p| p.keys.is_some());
    let pos = c.param("Position").and_then(PParam::static_str).and_then(|s| s.split_once(':')).and_then(|(x, y)| Some((num(x)?, num(y)?)));
    let scale = c.param("Scale").and_then(PParam::f64).unwrap_or(100.0);
    let rot = c.param("Rotation").and_then(PParam::f64).unwrap_or(0.0);
    !animated && pos.is_none_or(|(x, y)| (x - 0.5).abs() < 1e-3 && (y - 0.5).abs() < 1e-3) && (scale - 100.0).abs() < 1e-3 && rot.abs() < 1e-3
}

/// Canvas bounds of a layer at `t`.
fn bounds_at(e: &EffectInstance, t: Tick, frame: [u32; 2]) -> Option<[f64; 4]> {
    let spec = filmcraft_project::graphic::eval_layer(e, t, (frame[0], frame[1]))?;
    let b = filmcraft_render::graphic_clip::layer_bounds(&spec);
    (b[2] >= b[0] && b[3] >= b[1] && b.iter().all(|v| v.is_finite())).then_some(b)
}

/// Move a layer's position (static value and keyframes) by `d`.
fn shift_position(e: &mut EffectInstance, d: Vec2) {
    if let Some(p) = e.params.get_mut("position") {
        p.map_values(|v| match v {
            ParamValue::Vec2(q) => ParamValue::Vec2(Vec2::new(q.x + d.x, q.y + d.y)),
            o => o,
        });
    }
}

/// Pinned layers: Premiere stores their position relative to the parent's top-left corner; they
/// become absolute, with a FilmCraft pin to the parent (found by its size) so they follow it.
fn resolve_pins(layers: &mut [EffectInstance], pins: &[Option<PinInfo>], frame: [u32; 2], duration: i64, warnings: &mut Vec<String>) {
    let mut done = vec![false; layers.len()];
    for _pass in 0..layers.len() {
        for i in 0..layers.len() {
            let Some(pin) = pins[i] else { continue };
            if done[i] {
                continue;
            }
            // the parent: the other layer whose size matches, at its start, end or a keyframe
            let mut best: Option<(usize, Tick, [f64; 4], f64)> = None;
            for (j, l) in layers.iter().enumerate() {
                if j == i || (pins[j].is_some() && !done[j]) {
                    continue;
                }
                let mut times = vec![Tick::ZERO, Tick((duration - 1).max(0))];
                times.extend(l.params.values().flat_map(|p| p.keyframes.iter().map(|k| k.time)));
                for t in times {
                    if let Some(b) = bounds_at(l, t, frame) {
                        let err = ((b[2] - b[0]) - pin.size.0).abs() + ((b[3] - b[1]) - pin.size.1).abs();
                        if best.is_none_or(|x| err < x.3) {
                            best = Some((j, t, b, err));
                        }
                    }
                }
            }
            let Some((j, t, pb, err)) = best else { continue };
            let name = layers[i].params.get("name").map(|p| format!("{:?}", p.value)).unwrap_or_default();
            if err > 0.05 * (pin.size.0 + pin.size.1) + 4.0 {
                warnings.push(format!("layer {name}: pinned to a layer that could not be found exactly; placed approximately"));
            }
            shift_position(&mut layers[i], Vec2::new(pb[0], pb[1]));
            let uid = layers[j].layer.as_ref().map(|x| x.uid).unwrap_or(0);
            if uid != 0
                && pin.edges.iter().any(|e| *e)
                && let Some(own) = bounds_at(&layers[i], t, frame)
            {
                let p = filmcraft_project::Pin::new(filmcraft_project::PinTarget::Layer(uid), pin.edges, own, pb);
                layers[i].layer.get_or_insert_with(Default::default).pin = Some(p);
            }
            done[i] = true;
        }
    }
}

/// The clip's Motion (position / scale / rotation of the whole graphic), static: folded into
/// every layer. Returns false (left out) when it is animated or scales unevenly.
fn fold_motion(layers: &mut [EffectInstance], c: &PComponent, frame: [u32; 2]) -> bool {
    if c.params.iter().any(|p| p.keys.is_some()) {
        return false;
    }
    let pt = |name: &str, d: (f64, f64)| {
        c.param(name).and_then(PParam::static_str).and_then(|s| s.split_once(':')).and_then(|(x, y)| Some((num(x)?, num(y)?))).unwrap_or(d)
    };
    let (px, py) = pt("Position", (0.5, 0.5));
    let (ax, ay) = pt("Anchor Point", (0.5, 0.5));
    let s = c.param("Scale").and_then(PParam::f64).unwrap_or(100.0) / 100.0;
    let sw = c.param("Scale Width").and_then(PParam::f64).unwrap_or(100.0) / 100.0;
    if (sw - 1.0).abs() > 1e-6 && c.params.iter().any(|p| p.control == 4 && p.bool() == Some(false)) {
        return false;
    }
    let r = c.param("Rotation").and_then(PParam::f64).unwrap_or(0.0);
    let (fw, fh) = (frame[0] as f64, frame[1] as f64);
    let m = filmcraft_geom::Affine::motion(Vec2::new(px * fw, py * fh), Vec2::new(s, s), r, Vec2::new(ax * fw, ay * fh));
    for l in layers.iter_mut() {
        if let Some(p) = l.params.get_mut("position") {
            p.map_values(|v| match v {
                ParamValue::Vec2(q) => ParamValue::Vec2(m.apply(q)),
                o => o,
            });
        }
        for (k, f) in [("scale", s), ("rotation", r)] {
            let p = l.params.entry(k.to_string()).or_insert_with(|| Param::new(ParamValue::Float(if k == "scale" { 100.0 } else { 0.0 })));
            p.map_values(|v| match v {
                ParamValue::Float(x) if k == "scale" => ParamValue::Float(x * f),
                ParamValue::Float(x) => ParamValue::Float(x + f),
                o => o,
            });
        }
        if let Some(pin) = l.layer.as_mut().and_then(|x| x.pin.as_mut()) {
            for o in &mut pin.offsets {
                *o *= s;
            }
        }
    }
    true
}

/// Convert a Premiere `.mogrt` (bytes of the file) into a graphics template. `source` names the
/// file in messages and in the template's description.
pub fn convert(bytes: &[u8], source: &str) -> Result<Converted, MogrtError> {
    if kind(bytes)? == MogrtKind::AfterEffects {
        return Err(MogrtError::AfterEffects);
    }
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| MogrtError::NotMogrt(e.to_string()))?;
    let def: Value = serde_json::from_slice(&read_member(&mut z, "definition.json")?).map_err(|e| MogrtError::Unreadable(format!("definition.json: {e}")))?;
    let inner = read_member(&mut z, "project.prgraphic")?;
    let mut iz = zip::ZipArchive::new(std::io::Cursor::new(inner)).map_err(|e| MogrtError::Unreadable(format!("project.prgraphic: {e}")))?;
    let proj = iz
        .file_names()
        .find(|n| n.to_ascii_lowercase().ends_with(".prproj"))
        .map(str::to_string)
        .ok_or_else(|| MogrtError::Unreadable("no .prproj inside".into()))?;
    let gz = read_member(&mut iz, &proj)?;
    let mut xml = String::new();
    flate2::read::GzDecoder::new(&gz[..]).take(MAX_MEMBER).read_to_string(&mut xml).map_err(|e| MogrtError::Unreadable(format!("{proj}: {e}")))?;
    let doc = roxmltree::Document::parse(&xml).map_err(|e| MogrtError::Unreadable(format!("{proj}: {e}")))?;
    let d = Doc::new(&doc);
    let (duration, in_point, frame, comps) = graphic_clip(&d, &doc)?;

    let mut out = Converted { template: GraphicsTemplate::default(), warnings: Vec::new(), lost: Vec::new(), missing_fonts: Vec::new() };
    let mut layers = Vec::new();
    let mut pins: Vec<Option<PinInfo>> = Vec::new();
    let mut clip_opacity = None;
    let mut clip_motion = None;
    for c in &comps {
        match c.match_name.as_str() {
            "AE.ADBE Text" => match text_layer(c, in_point, frame, &mut out) {
                Some(l) => {
                    layers.push(l);
                    pins.push(c.pin());
                }
                None => out.lost.push(format!("text layer “{}”: not readable", c.instance)),
            },
            "AE.ADBE Shape" => {
                if let Some(l) = shape_layer(c, in_point, frame, &mut out) {
                    layers.push(l);
                    pins.push(c.pin());
                }
            }
            "AE.ADBE Opacity" => clip_opacity = Some(c),
            "AE.ADBE Motion" if motion_is_identity(c) => {}
            "AE.ADBE Motion" => clip_motion = Some(c),
            "AE.ADBE Graphic Group" => out.lost.push(format!("layer group “{}” (its layers are kept, the group's own transform is not)", c.instance)),
            _ => out.lost.push(format!("effect “{}”", if c.display.is_empty() { &c.match_name } else { &c.display })),
        }
    }
    if layers.is_empty() {
        return Err(MogrtError::Unreadable("no text or shape layer".into()));
    }
    // masks (often animated reveals) hang off the clip's Opacity, outside the component chain
    let masks = doc.descendants().filter(|n| n.has_tag_name("MatchName") && n.text() == Some("AE.ADBE AEMask")).count();
    if masks > 0 {
        out.lost.push(format!("masks ({masks}): reveals and cut-outs are not drawn"));
    }
    // Premiere draws the graphic's layers from the last component to the first (the first one listed
    // is in front): FilmCraft paints in list order
    layers.reverse();
    pins.reverse();
    for (i, l) in layers.iter_mut().enumerate() {
        l.layer.get_or_insert_with(Default::default).uid = i as u64 + 1;
    }
    resolve_pins(&mut layers, &pins, frame, duration, &mut out.warnings);
    if let Some(c) = clip_motion
        && !fold_motion(&mut layers, c, frame)
    {
        out.lost.push("movement of the whole title (animated clip Motion)".into());
    }
    if let Some(c) = clip_opacity
        && fold_opacity(&mut layers, c, in_point)
    {
        out.warnings.push("the clip's fade is applied to each layer".into());
    }
    let mut controls = Vec::new();
    for (i, l) in layers.iter_mut().enumerate() {
        let uid = i as u64 + 1;
        let name = match l.params.get("name").map(|p| &p.value) {
            Some(ParamValue::Text(t)) if !t.is_empty() => t.clone(),
            _ => format!("Layer {uid}"),
        };
        if l.effect == TEXT_LAYER {
            controls.push(TemplateControl {
                id: format!("text-{uid}"),
                name: name.clone(),
                kind: ControlKind::Text,
                layer: uid,
                param: "text".into(),
                ..Default::default()
            });
            controls.push(TemplateControl {
                id: format!("font-{uid}"),
                name: format!("{name} · Font"),
                kind: ControlKind::Font,
                layer: uid,
                param: "font".into(),
                ..Default::default()
            });
            controls.push(TemplateControl {
                id: format!("color-{uid}"),
                name: format!("{name} · Colour"),
                kind: ControlKind::Color,
                layer: uid,
                param: "fill_color".into(),
                ..Default::default()
            });
        } else if l.effect == SHAPE_LAYER {
            controls.push(TemplateControl {
                id: format!("color-{uid}"),
                name: format!("{name} · Colour"),
                kind: ControlKind::Color,
                layer: uid,
                param: "fill_color".into(),
                ..Default::default()
            });
        }
    }
    let name = {
        let n = localized(def.get("capsuleNameLocalized").unwrap_or(&Value::Null));
        if n.trim().is_empty() { def.get("capsuleName").and_then(Value::as_str).unwrap_or("Title").to_string() } else { n }
    };
    let capsule = def.get("capsuleID").and_then(Value::as_str).unwrap_or_default();
    let mut description = format!("Converted from the Premiere title {source}.");
    if !out.lost.is_empty() {
        description.push_str(&format!(" Not converted: {}.", out.lost.join("; ")));
    }
    if !out.warnings.is_empty() {
        description.push_str(&format!(" Differences: {}.", out.warnings.join("; ")));
    }
    out.template = GraphicsTemplate {
        id: format!("user:mogrt-{}-{}", slug(&name), slug(capsule).chars().take(8).collect::<String>()),
        name,
        category: "Converted Titles".into(),
        description,
        tags: vec!["mogrt".into(), "premiere".into()],
        canvas: frame,
        duration: Tick(duration),
        layers,
        controls,
        ..Default::default()
    };
    out.template.validate().map_err(|e| MogrtError::Unreadable(e.to_string()))?;
    Ok(out)
}
