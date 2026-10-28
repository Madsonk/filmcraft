//! ONNX `ModelProto` reading: graph, nodes, attributes, initialisers.

use std::collections::HashMap;

use crate::proto::{Reader, Wire, bytes, f32_of, int, push_f32s, push_varints, text};
use crate::tensor::{Data, Tensor, numel};
use crate::{Result, err};

#[derive(Clone, Debug)]
pub(crate) enum Attr {
    Int(i64),
    Float(f32),
    Ints(Vec<i64>),
    Floats(Vec<f32>),
    Str(String),
    Tensor(Tensor),
}

#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub op: String,
    pub name: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub attrs: HashMap<String, Attr>,
}

impl Node {
    pub fn int(&self, k: &str, default: i64) -> i64 {
        match self.attrs.get(k) {
            Some(Attr::Int(v)) => *v,
            _ => default,
        }
    }
    pub fn ints(&self, k: &str) -> Option<&[i64]> {
        match self.attrs.get(k) {
            Some(Attr::Ints(v)) => Some(v),
            _ => None,
        }
    }
    pub fn str(&self, k: &str) -> Option<&str> {
        match self.attrs.get(k) {
            Some(Attr::Str(v)) => Some(v),
            _ => None,
        }
    }
}

pub(crate) struct Graph {
    pub nodes: Vec<Node>,
    pub initializers: HashMap<String, Tensor>,
    /// Graph inputs that are not initialisers.
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

const MAX_NODES: usize = 200_000;

pub(crate) fn parse(file: &[u8]) -> Result<Graph> {
    let mut r = Reader::new(file);
    let mut graph: Option<&[u8]> = None;
    while let Some((f, w)) = r.next()? {
        if f == 7 {
            graph = Some(bytes(&w)?);
        }
    }
    let Some(g) = graph else { return err("not an ONNX model (no graph)") };
    let mut r = Reader::new(g);
    let mut nodes = Vec::new();
    let mut inits = HashMap::new();
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    while let Some((f, w)) = r.next()? {
        match f {
            1 => {
                if nodes.len() >= MAX_NODES {
                    return err("model has too many nodes");
                }
                nodes.push(node(bytes(&w)?)?);
            }
            5 => {
                let (name, t) = tensor(bytes(&w)?)?;
                inits.insert(name, t);
            }
            11 => inputs.push(value_name(bytes(&w)?)?),
            12 => outputs.push(value_name(bytes(&w)?)?),
            _ => {}
        }
    }
    inputs.retain(|n| !inits.contains_key(n));
    Ok(Graph { nodes, initializers: inits, inputs, outputs })
}

fn value_name(b: &[u8]) -> Result<String> {
    let mut r = Reader::new(b);
    while let Some((f, w)) = r.next()? {
        if f == 1 {
            return text(&w);
        }
    }
    Ok(String::new())
}

fn node(b: &[u8]) -> Result<Node> {
    let mut n = Node { op: String::new(), name: String::new(), inputs: vec![], outputs: vec![], attrs: HashMap::new() };
    let mut r = Reader::new(b);
    while let Some((f, w)) = r.next()? {
        match f {
            1 => n.inputs.push(text(&w)?),
            2 => n.outputs.push(text(&w)?),
            3 => n.name = text(&w)?,
            4 => n.op = text(&w)?,
            5 => {
                let (k, a) = attr(bytes(&w)?)?;
                if let Some(a) = a {
                    n.attrs.insert(k, a);
                }
            }
            _ => {}
        }
    }
    Ok(n)
}

fn attr(b: &[u8]) -> Result<(String, Option<Attr>)> {
    let mut name = String::new();
    let mut ints = Vec::new();
    let mut floats = Vec::new();
    let mut a: Option<Attr> = None;
    let mut r = Reader::new(b);
    while let Some((f, w)) = r.next()? {
        match f {
            1 => name = text(&w)?,
            2 => {
                if let Wire::Fixed32(x) = &w {
                    a = Some(Attr::Float(f32_of(x)));
                }
            }
            3 => a = Some(Attr::Int(int(&w)?)),
            4 => a = Some(Attr::Str(text(&w)?)),
            5 => a = Some(Attr::Tensor(tensor(bytes(&w)?)?.1)),
            7 => push_f32s(&w, &mut floats)?,
            8 => push_varints(&w, &mut ints)?,
            _ => {}
        }
    }
    if !ints.is_empty() {
        a = Some(Attr::Ints(ints));
    } else if !floats.is_empty() {
        a = Some(Attr::Floats(floats));
    }
    Ok((name, a))
}

fn tensor(b: &[u8]) -> Result<(String, Tensor)> {
    let mut dims: Vec<i64> = Vec::new();
    let mut dtype = 0i64;
    let mut name = String::new();
    let mut raw: Option<&[u8]> = None;
    let mut fl: Vec<f32> = Vec::new();
    let mut i64s: Vec<i64> = Vec::new();
    let mut i32s: Vec<i64> = Vec::new();
    let mut dbl: Vec<f64> = Vec::new();
    let mut external = false;
    let mut r = Reader::new(b);
    while let Some((f, w)) = r.next()? {
        match f {
            1 => push_varints(&w, &mut dims)?,
            2 => dtype = int(&w)?,
            4 => push_f32s(&w, &mut fl)?,
            5 => push_varints(&w, &mut i32s)?,
            7 => push_varints(&w, &mut i64s)?,
            8 => name = text(&w)?,
            9 => raw = Some(bytes(&w)?),
            10 => match &w {
                Wire::Len(x) => dbl.extend(x.as_chunks::<8>().0.iter().map(|c| f64::from_le_bytes(*c))),
                Wire::Fixed64(x) => {
                    if let Some(c) = x.first_chunk::<8>() {
                        dbl.push(f64::from_le_bytes(*c));
                    }
                }
                _ => {}
            },
            14 => external = int(&w)? != 0,
            _ => {}
        }
    }
    if external {
        return err(format!("tensor {name}: external data is not supported"));
    }
    let mut shape = Vec::with_capacity(dims.len());
    for d in &dims {
        shape.push(usize::try_from(*d).map_err(|_| crate::NnError(format!("tensor {name}: negative dimension")))?);
    }
    let n = numel(&shape)?;
    let data = match dtype {
        // FLOAT
        1 => Data::F(match raw {
            Some(r) => r.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect(),
            None => fl,
        }),
        // DOUBLE
        11 => Data::F(match raw {
            Some(r) => r.as_chunks::<8>().0.iter().map(|c| f64::from_le_bytes(*c) as f32).collect(),
            None => dbl.iter().map(|x| *x as f32).collect(),
        }),
        // INT64
        7 => Data::I(match raw {
            Some(r) => r.as_chunks::<8>().0.iter().map(|c| i64::from_le_bytes(*c)).collect(),
            None => i64s,
        }),
        // INT32
        6 => Data::I(match raw {
            Some(r) => r.as_chunks::<4>().0.iter().map(|c| i64::from(i32::from_le_bytes(*c))).collect(),
            None => i32s.iter().map(|x| i64::from(*x as i32)).collect(),
        }),
        // BOOL, UINT8, INT8
        9 | 2 | 3 => Data::I(match raw {
            Some(r) => r.iter().map(|x| i64::from(*x)).collect(),
            None => i32s,
        }),
        t => return err(format!("tensor {name}: unsupported data type {t}")),
    };
    let t = Tensor { shape, data };
    if t.len() != n {
        return err(format!("tensor {name}: {} values for shape {:?}", t.len(), t.shape));
    }
    Ok((name, t))
}
