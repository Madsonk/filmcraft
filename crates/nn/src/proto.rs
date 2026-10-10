//! A minimal protobuf wire-format reader (just what ONNX files need).

use crate::{Result, err};

pub(crate) enum Wire<'a> {
    Varint(u64),
    Fixed64(&'a [u8]),
    Len(&'a [u8]),
    Fixed32(&'a [u8]),
}

pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn varint(&mut self) -> Result<u64> {
        let mut v: u64 = 0;
        let mut shift = 0u32;
        loop {
            let Some(&b) = self.buf.get(self.pos) else { return err("truncated protobuf (varint)") };
            self.pos += 1;
            if shift >= 64 {
                return err("protobuf varint too long");
            }
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|e| *e <= self.buf.len());
        let Some(end) = end else { return err("truncated protobuf (field longer than the file)") };
        let s = self.buf.get(self.pos..end).unwrap_or(&[]);
        self.pos = end;
        Ok(s)
    }

    /// The next `(field number, value)`, or `None` at the end.
    pub fn next(&mut self) -> Result<Option<(u32, Wire<'a>)>> {
        if self.pos >= self.buf.len() {
            return Ok(None);
        }
        let key = self.varint()?;
        let field = (key >> 3) as u32;
        let w = match key & 7 {
            0 => Wire::Varint(self.varint()?),
            1 => Wire::Fixed64(self.take(8)?),
            2 => {
                let n = self.varint()?;
                let n = usize::try_from(n).map_err(|_| crate::NnError("protobuf length overflow".into()))?;
                Wire::Len(self.take(n)?)
            }
            5 => Wire::Fixed32(self.take(4)?),
            t => return err(format!("unsupported protobuf wire type {t}")),
        };
        Ok(Some((field, w)))
    }
}

/// A packed (or single) repeated varint field.
pub(crate) fn push_varints(w: &Wire, out: &mut Vec<i64>) -> Result<()> {
    match w {
        Wire::Varint(v) => out.push(*v as i64),
        Wire::Len(b) => {
            let mut r = Reader::new(b);
            while r.pos < r.buf.len() {
                out.push(r.varint()? as i64);
            }
        }
        _ => return err("bad repeated integer field"),
    }
    Ok(())
}

pub(crate) fn push_f32s(w: &Wire, out: &mut Vec<f32>) -> Result<()> {
    match w {
        Wire::Fixed32(b) => out.push(f32_of(b)),
        Wire::Len(b) => out.extend(b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c))),
        _ => return err("bad repeated float field"),
    }
    Ok(())
}

pub(crate) fn f32_of(b: &[u8]) -> f32 {
    let mut a = [0u8; 4];
    for (d, s) in a.iter_mut().zip(b) {
        *d = *s;
    }
    f32::from_le_bytes(a)
}

pub(crate) fn text(w: &Wire) -> Result<String> {
    match w {
        Wire::Len(b) => Ok(String::from_utf8_lossy(b).into_owned()),
        _ => err("expected a string field"),
    }
}

pub(crate) fn bytes<'a>(w: &Wire<'a>) -> Result<&'a [u8]> {
    match w {
        Wire::Len(b) => Ok(b),
        _ => err("expected a length-delimited field"),
    }
}

pub(crate) fn int(w: &Wire) -> Result<i64> {
    match w {
        Wire::Varint(v) => Ok(*v as i64),
        _ => err("expected an integer field"),
    }
}
