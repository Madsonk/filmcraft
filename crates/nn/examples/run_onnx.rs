//! `run_onnx MODEL.onnx INPUTS.bin OUTPUTS.bin` — runs a model on tensors read from a small binary
//! container and writes the outputs in the same container. Used to compare this interpreter with
//! `onnxruntime` on the real models (see `docs/extensions.md`).
//!
//! Container: `u32 count`, then per tensor `u32 name_len, name, u32 dtype (0 = f32, 1 = i64),
//! u32 rank, rank × u64 dims, raw little-endian data`.

use filmcraft_nn::{Data, Model, Tensor};

fn rd_u32(b: &[u8], p: &mut usize) -> Result<u32, String> {
    let s = b.get(*p..*p + 4).ok_or("truncated")?;
    *p += 4;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn rd_u64(b: &[u8], p: &mut usize) -> Result<u64, String> {
    let s = b.get(*p..*p + 8).ok_or("truncated")?;
    *p += 8;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Ok(u64::from_le_bytes(a))
}

fn read(path: &str) -> Result<Vec<(String, Tensor)>, String> {
    let b = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let mut p = 0usize;
    let n = rd_u32(&b, &mut p)?;
    let mut out = Vec::new();
    for _ in 0..n {
        let nl = rd_u32(&b, &mut p)? as usize;
        let name = String::from_utf8_lossy(b.get(p..p + nl).ok_or("truncated")?).into_owned();
        p += nl;
        let dt = rd_u32(&b, &mut p)?;
        let rank = rd_u32(&b, &mut p)? as usize;
        let mut shape = Vec::new();
        for _ in 0..rank {
            shape.push(rd_u64(&b, &mut p)? as usize);
        }
        let count: usize = shape.iter().product();
        let t = if dt == 0 {
            let raw = b.get(p..p + count * 4).ok_or("truncated")?;
            p += count * 4;
            Tensor::f32(shape, raw.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect())
        } else {
            let raw = b.get(p..p + count * 8).ok_or("truncated")?;
            p += count * 8;
            Tensor::i64(shape, raw.as_chunks::<8>().0.iter().map(|c| i64::from_le_bytes(*c)).collect())
        };
        out.push((name, t.map_err(|e| e.to_string())?));
    }
    Ok(out)
}

fn write(path: &str, ts: &[(String, Tensor)]) -> Result<(), String> {
    let mut o: Vec<u8> = Vec::new();
    o.extend((ts.len() as u32).to_le_bytes());
    for (name, t) in ts {
        o.extend((name.len() as u32).to_le_bytes());
        o.extend(name.as_bytes());
        o.extend(u32::from(!t.is_float()).to_le_bytes());
        o.extend((t.shape.len() as u32).to_le_bytes());
        for d in &t.shape {
            o.extend((*d as u64).to_le_bytes());
        }
        match &t.data {
            Data::F(v) => v.iter().for_each(|x| o.extend(x.to_le_bytes())),
            Data::I(v) => v.iter().for_each(|x| o.extend(x.to_le_bytes())),
        }
    }
    std::fs::write(path, o).map_err(|e| format!("{path}: {e}"))
}

fn main() -> Result<(), String> {
    let a: Vec<String> = std::env::args().collect();
    let [_, model, inputs, outputs] = a.as_slice() else { return Err("usage: run_onnx MODEL.onnx INPUTS.bin OUTPUTS.bin".into()) };
    let m = Model::parse(&std::fs::read(model).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let ins = read(inputs)?;
    let t0 = std::time::Instant::now();
    let outs = m.run(ins.iter().map(|(n, t)| (n.as_str(), t.clone())).collect()).map_err(|e| e.to_string())?;
    eprintln!("ran in {:.3} s", t0.elapsed().as_secs_f64());
    let named: Vec<(String, Tensor)> = m.output_names().iter().cloned().zip(outs).collect();
    write(outputs, &named)
}
