//! Synthetic `.mogrt` files for tests (made here, no third-party content): a project XML in the
//! shape Premiere writes, gzipped, in `project.prgraphic`, in a ZIP with `definition.json`; and
//! an After Effects stand-in that is never opened. Functions return `None` only if writing the
//! in-memory ZIP fails.

use std::io::Write;

fn utf16_blob(json: &str) -> String {
    let mut b = vec![0u8; 8];
    for u in json.encode_utf16() {
        b.extend_from_slice(&u.to_le_bytes());
    }
    b64(&b)
}

pub fn b64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// A rectangle path `w`×`h` from the origin.
pub fn rect_path(w: f32, h: f32) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&2u32.to_le_bytes());
    b.extend_from_slice(&4u32.to_le_bytes());
    for (x, y) in [(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)] {
        b.extend_from_slice(&0u32.to_le_bytes());
        for _ in 0..3 {
            b.extend_from_slice(&f32::to_le_bytes(x));
            b.extend_from_slice(&f32::to_le_bytes(y));
        }
    }
    b.push(1);
    b
}

pub struct Spec {
    pub text: &'static str,
    pub font: &'static str,
    pub second_run_size: Option<f64>,
    pub opacity_keys: bool,
    pub extra_effect: bool,
}

pub const IN: i64 = 914_457_600_000_000;

fn param(id: u32, name: &str, ctl: u32, start: &str, keys: Option<&str>) -> String {
    let k = keys.map(|k| format!("<Keyframes>{k}</Keyframes>")).unwrap_or_default();
    format!(
        "<VideoComponentParam ObjectID=\"{id}\"><Name>{name}</Name><ParameterControlType>{ctl}</ParameterControlType><StartKeyframe>-91445760000000000,{start},0,0,0,0,0,0</StartKeyframe>{k}</VideoComponentParam>"
    )
}

fn arb(id: u32, name: &str, blob: &str) -> String {
    format!(
        "<ArbVideoComponentParam ObjectID=\"{id}\"><Name>{name}</Name><ParameterControlType>9</ParameterControlType><StartKeyframeValue Encoding=\"base64\">{blob}</StartKeyframeValue></ArbVideoComponentParam>"
    )
}

fn comp(id: u32, match_name: &str, display: &str, inst: &str, params: &[u32]) -> String {
    let ps: String = params.iter().enumerate().map(|(i, p)| format!("<Param Index=\"{i}\" ObjectRef=\"{p}\"/>")).collect();
    format!(
        "<VideoFilterComponent ObjectID=\"{id}\"><Component Version=\"6\"><Params Version=\"1\">{ps}</Params><DisplayName>{display}</DisplayName><InstanceName>{inst}</InstanceName></Component><MatchName>{match_name}</MatchName></VideoFilterComponent>"
    )
}

pub fn premiere(spec: &Spec) -> Option<Vec<u8>> {
    let runs = match spec.second_run_size {
        Some(s) => format!("[[0,120],[5,{s}]]"),
        None => "[[0,120]]".into(),
    };
    let text_json = format!(
        r#"{{"mTextParam":{{"mAlignment":0,"mShadowVisible":true,"mShadowColor":0,"mShadowOpacity":60,"mShadowAngle":135,"mShadowOffset":5,"mShadowBlur":10,"mShadowSize":0,"mStyleSheet":{{"mText":"{}","mFontName":{{"mParamValues":[[0,"{}"]]}},"mFontSize":{{"mParamValues":{runs}}},"mFillColor":{{"mParamValues":[[0,16711680]]}},"mFillVisible":{{"mParamValues":[[0,true]]}},"mStrokeVisible":{{"mParamValues":[[0,false]]}},"mTracking":{{"mParamValues":[[0,50]]}}}},"mWidth":0,"mHeight":0}},"mVersion":1}}"#,
        spec.text, spec.font
    );
    let shape_json = r#"{"mStyle":{"mFillColor":5342408,"mFillVisible":true,"mFillColorType":0,"mStrokeVisible":false,"mShadowVisible":false},"mVersion":1}"#;
    let opk = spec.opacity_keys.then(|| format!("{},0.,0,0,0,0.16,0,0.16;{},100.,5,0,0,0.16,0,0.16", IN, IN + 254_016_000_000 / 2));
    let objs = [
        arb(11, "Source Text", &utf16_blob(&text_json)),
        param(12, "Position", 6, "0.1:0.8", None),
        param(13, "Scale", 2, "100.", None),
        param(14, "Horizontal Scale", 2, "100.", None),
        param(15, " ", 4, "true", None),
        param(16, "Rotation", 3, "0.", None),
        param(17, "Opacity", 2, "100.", opk.as_deref()),
        param(18, "Anchor Point", 6, "0:0", None),
        comp(10, "AE.ADBE Text", "Text", "Name", &[11, 12, 13, 14, 15, 16, 17, 18]),
        arb(21, "Path", &b64(&rect_path(600.0, 200.0))),
        arb(22, "Appearance", &utf16_blob(shape_json)),
        param(23, "Position", 6, "0.1:0.8", None),
        param(24, "Scale", 2, "100.", None),
        param(25, "Horizontal Scale", 2, "50.", None),
        param(26, " ", 4, "false", None),
        param(27, "Rotation", 3, "0.", None),
        param(28, "Opacity", 2, "100.", None),
        param(29, "Anchor Point", 6, "0.0:0.0", None),
        comp(20, "AE.ADBE Shape", "Shape", "Box", &[21, 22, 23, 24, 25, 26, 27, 28, 29]),
        param(31, "Wave Height", 2, "10.", None),
        comp(30, "AE.ADBE Wave Warp", "Wave Warp", "", &[31]),
    ];
    let chain: String = if spec.extra_effect { "<Component Index=\"0\" ObjectRef=\"30\"/>".to_string() } else { String::new() }
        + "<Component Index=\"1\" ObjectRef=\"10\"/><Component Index=\"2\" ObjectRef=\"20\"/>";
    let xml = format!(
        "<?xml version=\"1.0\"?><PremiereData Version=\"3\"><VideoClipTrackItem ObjectID=\"1\"><ClipTrackItem><ComponentOwner><Components ObjectRef=\"2\"/></ComponentOwner><TrackItem><Start>0</Start><End>1270080000000</End></TrackItem><SubClip ObjectRef=\"3\"/></ClipTrackItem><FrameRect>0,0,1920,1080</FrameRect></VideoClipTrackItem>\
<VideoComponentChain ObjectID=\"2\"><ComponentChain><Components>{chain}</Components></ComponentChain></VideoComponentChain>\
<SubClip ObjectID=\"3\"><Clip ObjectRef=\"4\"/></SubClip><VideoClip ObjectID=\"4\"><Clip><InPoint>{IN}</InPoint></Clip></VideoClip>{}</PremiereData>",
        objs.join("")
    );
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(xml.as_bytes()).ok()?;
    let gz = gz.finish().ok()?;
    let inner = zip_of(&[("Title.prproj", &gz)])?;
    let def = r#"{"authorApp":"ppro","capsuleID":"1234abcd-0000","capsuleNameLocalized":{"strDB":[{"localeString":"en_US","str":"Test Lower Third"}]},"clientControls":[]}"#;
    zip_of(&[("definition.json", def.as_bytes()), ("project.prgraphic", &inner), ("thumb.png", b"\x89PNG")])
}

pub fn after_effects() -> Option<Vec<u8>> {
    zip_of(&[("definition.json", br#"{"authorApp":"aefx"}"#), ("project.aegraphic", b"PK not read")])
}

pub fn zip_of(files: &[(&str, &[u8])]) -> Option<Vec<u8>> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (n, b) in files {
        w.start_file(*n, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)).ok()?;
        w.write_all(b).ok()?;
    }
    Some(w.finish().ok()?.into_inner())
}
