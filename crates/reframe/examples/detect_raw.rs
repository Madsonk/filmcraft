//! `detect_raw MODELS_DIR WIDTH HEIGHT FRAME.rgba [yolox-nano|yolox-tiny|yolox-s]` — prints the persons
//! (YOLOX) and faces (YuNet, when installed) found in a raw RGBA frame, one JSON line each.
//! Models are read from `MODELS_DIR/<id>/…` like the store lays them out.

use filmcraft_reframe::detect::{Yolox, Yunet};
use filmcraft_reframe::{Detection, Image};

fn main() -> Result<(), String> {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 {
        return Err("usage: detect_raw MODELS_DIR WIDTH HEIGHT FRAME.rgba [model]".into());
    }
    let (w, h): (usize, usize) = (a[2].parse().map_err(|_| "bad width")?, a[3].parse().map_err(|_| "bad height")?);
    let rgba = std::fs::read(&a[4]).map_err(|e| e.to_string())?;
    let img = Image { width: w, height: h, rgba: &rgba };
    let id = a.get(5).map(String::as_str).unwrap_or("yolox-nano");
    let file = format!("{}.onnx", id.replace('-', "_"));
    let bytes = std::fs::read(std::path::Path::new(&a[1]).join(id).join(&file)).map_err(|e| format!("{file}: {e}"))?;
    let y = Yolox::from_bytes(&bytes).map_err(|e| e.to_string())?.with_size(if id == "yolox-s" { 640 } else { 416 });
    let t0 = std::time::Instant::now();
    let persons = y.detect(&img, 0.3).map_err(|e| e.to_string())?;
    eprintln!("yolox {:.0} ms", t0.elapsed().as_secs_f64() * 1e3);
    let show = |kind: &str, d: &Detection| {
        println!("{{\"kind\":\"{kind}\",\"x0\":{:.1},\"y0\":{:.1},\"x1\":{:.1},\"y1\":{:.1},\"score\":{:.3}}}", d.x0, d.y0, d.x1, d.y1, d.score)
    };
    persons.iter().for_each(|d| show("person", d));
    if let Ok(b) = std::fs::read(std::path::Path::new(&a[1]).join("yunet/face_detection_yunet_2023mar.onnx")) {
        let f = Yunet::from_bytes(&b).map_err(|e| e.to_string())?;
        let t0 = std::time::Instant::now();
        let faces = f.detect(&img, 0.7).map_err(|e| e.to_string())?;
        eprintln!("yunet {:.0} ms", t0.elapsed().as_secs_f64() * 1e3);
        faces.iter().for_each(|d| show("face", d));
    }
    Ok(())
}
