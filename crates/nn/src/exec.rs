//! The graph executor.

use std::collections::HashMap;
use std::sync::Arc;

use crate::conv::{ConvArgs, ConvTArgs, conv_transpose2d, conv2d, max_pool2d, resize_nearest};
use crate::onnx::{self, Attr, Node};
use crate::ops::{self, Bin};
use crate::rnn::{self, GruArgs};
use crate::tensor::Tensor;
use crate::{NnError, Result, err};

/// A parsed model, ready to run.
pub struct Model {
    nodes: Vec<Node>,
    initializers: HashMap<String, Arc<Tensor>>,
    inputs: Vec<String>,
    outputs: Vec<String>,
    /// Index of the last node that reads each tensor name.
    last_use: HashMap<String, usize>,
}

impl Model {
    /// Reads an `.onnx` file's bytes.
    pub fn parse(file: &[u8]) -> Result<Model> {
        let g = onnx::parse(file)?;
        let mut last_use = HashMap::new();
        for (i, n) in g.nodes.iter().enumerate() {
            for name in n.inputs.iter().filter(|s| !s.is_empty()) {
                last_use.insert(name.clone(), i);
            }
        }
        let initializers = g.initializers.into_iter().map(|(k, v)| (k, Arc::new(v))).collect();
        Ok(Model { nodes: g.nodes, initializers, inputs: g.inputs, outputs: g.outputs, last_use })
    }

    /// Graph inputs (initialisers excluded), in file order.
    pub fn input_names(&self) -> &[String] {
        &self.inputs
    }

    /// Graph outputs, in file order.
    pub fn output_names(&self) -> &[String] {
        &self.outputs
    }

    /// Runs the graph. `inputs` are `(name, tensor)`; the result holds the graph outputs in file order.
    pub fn run(&self, inputs: Vec<(&str, Tensor)>) -> Result<Vec<Tensor>> {
        let mut env: HashMap<String, Arc<Tensor>> = HashMap::new();
        for (name, t) in inputs {
            if !self.inputs.iter().any(|n| n == name) {
                return err(format!("the model has no input named {name}"));
            }
            env.insert(name.to_string(), Arc::new(t));
        }
        for n in &self.inputs {
            if !env.contains_key(n) {
                return err(format!("missing model input {n}"));
            }
        }
        for (i, node) in self.nodes.iter().enumerate() {
            let mut ins: Vec<Option<Arc<Tensor>>> = Vec::with_capacity(node.inputs.len());
            for name in &node.inputs {
                if name.is_empty() {
                    ins.push(None);
                } else {
                    let t = env
                        .get(name)
                        .or_else(|| self.initializers.get(name))
                        .cloned()
                        .ok_or_else(|| NnError(format!("{} {}: input {name} is not computed", node.op, node.name)))?;
                    ins.push(Some(t));
                }
            }
            let outs = run_node(node, &ins).map_err(|e| NnError(format!("{} {}: {}", node.op, node.name, e.0)))?;
            drop(ins);
            for (name, t) in node.outputs.iter().zip(outs) {
                if !name.is_empty() {
                    env.insert(name.clone(), Arc::new(t));
                }
            }
            for name in node.inputs.iter().filter(|s| !s.is_empty()) {
                if self.last_use.get(name) == Some(&i) && !self.outputs.contains(name) {
                    env.remove(name);
                }
            }
        }
        self.outputs
            .iter()
            .map(|n| env.get(n).or_else(|| self.initializers.get(n)).map(|t| (**t).clone()).ok_or_else(|| NnError(format!("output {n} was not produced"))))
            .collect()
    }
}

fn need(ins: &[Option<Arc<Tensor>>], i: usize) -> Result<&Tensor> {
    match ins.get(i) {
        Some(Some(t)) => Ok(t),
        _ => err(format!("missing input {i}")),
    }
}

fn opt(ins: &[Option<Arc<Tensor>>], i: usize) -> Option<&Tensor> {
    ins.get(i).and_then(|t| t.as_deref())
}

fn pair(v: Option<&[i64]>, default: usize) -> Result<[usize; 2]> {
    match v {
        None => Ok([default; 2]),
        Some([a, b]) if *a >= 0 && *b >= 0 => Ok([*a as usize, *b as usize]),
        Some(other) => err(format!("expected two non-negative values, got {other:?}")),
    }
}

fn pads4(v: Option<&[i64]>) -> Result<[usize; 4]> {
    match v {
        None => Ok([0; 4]),
        Some([a, b, c, d]) if [a, b, c, d].iter().all(|x| **x >= 0) => Ok([*a as usize, *b as usize, *c as usize, *d as usize]),
        Some(other) => err(format!("unsupported pads {other:?}")),
    }
}

fn check_auto_pad(node: &Node) -> Result<()> {
    match node.str("auto_pad") {
        None | Some("NOTSET") | Some("") => Ok(()),
        Some(other) => err(format!("auto_pad {other} is not supported")),
    }
}

fn run_node(node: &Node, ins: &[Option<Arc<Tensor>>]) -> Result<Vec<Tensor>> {
    let one = |t: Tensor| Ok(vec![t]);
    match node.op.as_str() {
        "Constant" => {
            let t = match node.attrs.get("value") {
                Some(Attr::Tensor(t)) => t.clone(),
                _ => match (node.attrs.get("value_float"), node.attrs.get("value_int"), node.attrs.get("value_ints"), node.attrs.get("value_floats")) {
                    (Some(Attr::Float(f)), ..) => Tensor::f32(vec![], vec![*f])?,
                    (_, Some(Attr::Int(i)), ..) => Tensor::i64(vec![], vec![*i])?,
                    (_, _, Some(Attr::Ints(v)), _) => Tensor::i64(vec![v.len()], v.clone())?,
                    (.., Some(Attr::Floats(v))) => Tensor::f32(vec![v.len()], v.clone())?,
                    _ => return err("Constant without a supported value"),
                },
            };
            one(t)
        }
        "Identity" => one(need(ins, 0)?.clone()),
        "Add" => one(ops::binary(Bin::Add, need(ins, 0)?, need(ins, 1)?)?),
        "Sub" => one(ops::binary(Bin::Sub, need(ins, 0)?, need(ins, 1)?)?),
        "Mul" => one(ops::binary(Bin::Mul, need(ins, 0)?, need(ins, 1)?)?),
        "Div" => one(ops::binary(Bin::Div, need(ins, 0)?, need(ins, 1)?)?),
        "Relu" => one(ops::map_f32(need(ins, 0)?, |x| x.max(0.0))?),
        "Sigmoid" => one(ops::map_f32(need(ins, 0)?, |x| 1.0 / (1.0 + (-x).exp()))?),
        "Tanh" => one(ops::map_f32(need(ins, 0)?, f32::tanh)?),
        "Exp" => one(ops::map_f32(need(ins, 0)?, f32::exp)?),
        "Neg" => one(ops::map_f32(need(ins, 0)?, |x| -x)?),
        "Sqrt" => one(ops::map_f32(need(ins, 0)?, f32::sqrt)?),
        "Cast" => one(ops::cast(need(ins, 0)?, node.int("to", 1))?),
        "Shape" => one(ops::shape_of(need(ins, 0)?)?),
        "ConstantOfShape" => {
            let v = match node.attrs.get("value") {
                Some(Attr::Tensor(t)) => Some(t),
                _ => None,
            };
            one(ops::constant_of_shape(need(ins, 0)?, v)?)
        }
        "Reshape" => one(ops::reshape(need(ins, 0)?, need(ins, 1)?)?),
        "Squeeze" => {
            let axes = match opt(ins, 1) {
                Some(t) => Some(t.to_i64_vec()),
                None => node.ints("axes").map(<[i64]>::to_vec),
            };
            one(ops::squeeze(need(ins, 0)?, axes)?)
        }
        "Unsqueeze" => {
            let axes = match opt(ins, 1) {
                Some(t) => t.to_i64_vec(),
                None => node.ints("axes").map(<[i64]>::to_vec).ok_or_else(|| NnError("Unsqueeze without axes".into()))?,
            };
            one(ops::unsqueeze(need(ins, 0)?, &axes)?)
        }
        "Transpose" => one(ops::transpose(need(ins, 0)?, node.ints("perm"))?),
        "Concat" => {
            let ts: Vec<&Tensor> = ins.iter().filter_map(|t| t.as_deref()).collect();
            one(ops::concat(&ts, node.int("axis", 0))?)
        }
        "Slice" => {
            let starts = need(ins, 1)?.to_i64_vec();
            let ends = need(ins, 2)?.to_i64_vec();
            let axes = opt(ins, 3).map(Tensor::to_i64_vec);
            let steps = opt(ins, 4).map(Tensor::to_i64_vec);
            one(ops::slice(need(ins, 0)?, &starts, &ends, axes.as_deref(), steps.as_deref())?)
        }
        "Gather" => one(ops::gather(need(ins, 0)?, need(ins, 1)?, node.int("axis", 0))?),
        "Pad" => {
            if node.str("mode").is_some_and(|m| m != "constant") {
                return err("only constant padding is supported");
            }
            if opt(ins, 3).is_some() {
                return err("Pad with an axes input is not supported");
            }
            let pads = match opt(ins, 1) {
                Some(t) => t.to_i64_vec(),
                None => node.ints("pads").map(<[i64]>::to_vec).ok_or_else(|| NnError("Pad without pads".into()))?,
            };
            let value = opt(ins, 2).and_then(|t| t.to_f32_vec().first().copied()).unwrap_or(0.0);
            one(ops::pad(need(ins, 0)?, &pads, value)?)
        }
        "MatMul" => one(rnn::matmul(need(ins, 0)?, need(ins, 1)?)?),
        "Einsum" => {
            let eq = node.str("equation").ok_or_else(|| NnError("Einsum without an equation".into()))?;
            one(rnn::einsum(eq, need(ins, 0)?, need(ins, 1)?)?)
        }
        "Conv" => {
            check_auto_pad(node)?;
            let a = ConvArgs {
                strides: pair(node.ints("strides"), 1)?,
                pads: pads4(node.ints("pads"))?,
                dil: pair(node.ints("dilations"), 1)?,
                group: node.int("group", 1).max(1) as usize,
            };
            one(conv2d(need(ins, 0)?, need(ins, 1)?, opt(ins, 2), &a)?)
        }
        "ConvTranspose" => {
            check_auto_pad(node)?;
            if node.ints("output_shape").is_some() {
                return err("ConvTranspose output_shape is not supported");
            }
            let a = ConvTArgs {
                strides: pair(node.ints("strides"), 1)?,
                pads: pads4(node.ints("pads"))?,
                dil: pair(node.ints("dilations"), 1)?,
                out_pad: pair(node.ints("output_padding"), 0)?,
                group: node.int("group", 1).max(1) as usize,
            };
            one(conv_transpose2d(need(ins, 0)?, need(ins, 1)?, opt(ins, 2), &a)?)
        }
        "MaxPool" => {
            check_auto_pad(node)?;
            if node.int("ceil_mode", 0) != 0 || node.ints("dilations").is_some_and(|d| d.iter().any(|x| *x != 1)) {
                return err("MaxPool with ceil_mode or dilations is not supported");
            }
            let k = pair(node.ints("kernel_shape"), 1)?;
            one(max_pool2d(need(ins, 0)?, k, pair(node.ints("strides"), 1)?, pads4(node.ints("pads"))?)?)
        }
        "Resize" => {
            if node.str("mode").is_some_and(|m| m != "nearest") {
                return err("Resize: only mode nearest is supported");
            }
            let x = need(ins, 0)?;
            let [n, c, h, w] = match x.shape.as_slice() {
                [a, b, c, d] => [*a, *b, *c, *d],
                _ => return err("Resize: expected a 4-D input"),
            };
            let out_hw = if let Some(sizes) = opt(ins, 3) {
                match sizes.to_i64_vec().as_slice() {
                    [sn, sc, sh, sw] if *sn == n as i64 && *sc == c as i64 && *sh > 0 && *sw > 0 => [*sh as usize, *sw as usize],
                    _ => return err("Resize: unsupported sizes"),
                }
            } else {
                let sc = opt(ins, 2).map(Tensor::to_f32_vec).ok_or_else(|| NnError("Resize without scales or sizes".into()))?;
                match sc.as_slice() {
                    [a, b, sh, sw] if (*a - 1.0).abs() < 1e-6 && (*b - 1.0).abs() < 1e-6 && *sh > 0.0 && *sw > 0.0 => {
                        [(h as f32 * sh).floor() as usize, (w as f32 * sw).floor() as usize]
                    }
                    _ => return err("Resize: unsupported scales"),
                }
            };
            one(resize_nearest(x, out_hw)?)
        }
        "GRU" => {
            if node.str("direction").is_some_and(|d| d != "forward") {
                return err("GRU: only the forward direction is supported");
            }
            if opt(ins, 4).is_some() {
                return err("GRU: sequence_lens is not supported");
            }
            let a = GruArgs { hidden: node.int("hidden_size", 0).max(0) as usize, linear_before_reset: node.int("linear_before_reset", 0) != 0 };
            let (y, yh) = rnn::gru(need(ins, 0)?, need(ins, 1)?, need(ins, 2)?, opt(ins, 3), opt(ins, 5), &a)?;
            Ok(vec![y, yh])
        }
        other => err(format!("operator {other} is not supported")),
    }
}
