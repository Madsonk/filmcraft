//! SigLIP (Zhai et al., Google 2023; SigLIP 2, 2025): an image tower (ViT-B/16 with an attention
//! pooling head) and a text tower (12-layer transformer) trained so that an image and a caption
//! that describe each other have a high cosine similarity, scored with a sigmoid.
//!
//! Modified work (Apache License 2.0, §4(b)): ported by the LightCraft contributors in 2026 from
//! the Python/PyTorch SigLIP code of Hugging Face Transformers (`models/siglip`), Copyright The
//! HuggingFace Team, Google; translated to Rust on candle and restructured. See NOTICE.

use std::collections::HashMap;
use std::path::Path;

use candle_core::{DType, Device, IndexOp, Module, Tensor};
use candle_nn::{Conv2d, Conv2dConfig, LayerNorm, Linear, VarBuilder};

use crate::tokenizer::{CONTEXT, Tokenizer};
use crate::{Error, Result};

/// Side of the square image the vision tower takes.
pub const IMAGE: usize = 224;
const PATCH: usize = 16;
const DIM: usize = 768;
const HEADS: usize = 12;
const LAYERS: usize = 12;
const MLP: usize = 3072;
const EPS: f64 = 1e-6;

struct Attention {
    q: Linear,
    k: Linear,
    v: Linear,
    out: Linear,
}

impl Attention {
    fn new(vb: VarBuilder) -> candle_core::Result<Self> {
        Ok(Self {
            q: candle_nn::linear(DIM, DIM, vb.pp("q_proj"))?,
            k: candle_nn::linear(DIM, DIM, vb.pp("k_proj"))?,
            v: candle_nn::linear(DIM, DIM, vb.pp("v_proj"))?,
            out: candle_nn::linear(DIM, DIM, vb.pp("out_proj"))?,
        })
    }
}

/// Multi-head attention of `q` over `kv` (`[b, n, DIM]` each; the projections are given).
fn attend(q: &Tensor, k: &Tensor, v: &Tensor) -> candle_core::Result<Tensor> {
    let heads = |t: &Tensor| -> candle_core::Result<Tensor> {
        let (b, n, _) = t.dims3()?;
        t.reshape((b, n, HEADS, DIM / HEADS))?.transpose(1, 2)?.contiguous()
    };
    let (q, k, v) = (heads(q)?, heads(k)?, heads(v)?);
    let att = (q.matmul(&k.transpose(2, 3)?.contiguous()?)? * (1.0 / ((DIM / HEADS) as f64).sqrt()))?;
    let att = candle_nn::ops::softmax_last_dim(&att)?;
    let ctx = att.matmul(&v)?;
    let (b, _, n, _) = ctx.dims4()?;
    ctx.transpose(1, 2)?.reshape((b, n, DIM))
}

impl Module for Attention {
    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        self.out.forward(&attend(&self.q.forward(x)?, &self.k.forward(x)?, &self.v.forward(x)?)?)
    }
}

struct Mlp {
    fc1: Linear,
    fc2: Linear,
}

impl Mlp {
    fn new(vb: VarBuilder) -> candle_core::Result<Self> {
        Ok(Self { fc1: candle_nn::linear(DIM, MLP, vb.pp("fc1"))?, fc2: candle_nn::linear(MLP, DIM, vb.pp("fc2"))? })
    }
}

impl Module for Mlp {
    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        // gelu_pytorch_tanh
        self.fc2.forward(&self.fc1.forward(x)?.gelu()?)
    }
}

struct Layer {
    norm1: LayerNorm,
    attn: Attention,
    norm2: LayerNorm,
    mlp: Mlp,
}

impl Layer {
    fn new(vb: VarBuilder) -> candle_core::Result<Self> {
        Ok(Self {
            norm1: candle_nn::layer_norm(DIM, EPS, vb.pp("layer_norm1"))?,
            attn: Attention::new(vb.pp("self_attn"))?,
            norm2: candle_nn::layer_norm(DIM, EPS, vb.pp("layer_norm2"))?,
            mlp: Mlp::new(vb.pp("mlp"))?,
        })
    }
}

impl Module for Layer {
    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let x = (x + self.attn.forward(&self.norm1.forward(x)?)?)?;
        &x + self.mlp.forward(&self.norm2.forward(&x)?)?
    }
}

fn layers(vb: VarBuilder) -> candle_core::Result<Vec<Layer>> {
    (0..LAYERS).map(|i| Layer::new(vb.pp(i))).collect()
}

/// The attention-pooling head: one learned query attends over the patches.
struct Head {
    probe: Tensor,
    in_w: Tensor,
    in_b: Tensor,
    out: Linear,
    norm: LayerNorm,
    mlp: Mlp,
}

impl Head {
    fn new(vb: VarBuilder) -> candle_core::Result<Self> {
        let a = vb.pp("attention");
        Ok(Self {
            probe: vb.get((1, 1, DIM), "probe")?,
            in_w: a.get((3 * DIM, DIM), "in_proj_weight")?,
            in_b: a.get(3 * DIM, "in_proj_bias")?,
            out: candle_nn::linear(DIM, DIM, a.pp("out_proj"))?,
            norm: candle_nn::layer_norm(DIM, EPS, vb.pp("layernorm"))?,
            mlp: Mlp::new(vb.pp("mlp"))?,
        })
    }

    fn forward(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        let b = x.dim(0)?;
        let proj = |i: usize, t: &Tensor| -> candle_core::Result<Tensor> {
            let w = self.in_w.narrow(0, i * DIM, DIM)?;
            let bias = self.in_b.narrow(0, i * DIM, DIM)?;
            t.broadcast_matmul(&w.t()?)?.broadcast_add(&bias)
        };
        let probe = self.probe.broadcast_as((b, 1, DIM))?.contiguous()?;
        let a = self.out.forward(&attend(&proj(0, &probe)?, &proj(1, x)?, &proj(2, x)?)?)?;
        let h = (&a + self.mlp.forward(&self.norm.forward(&a)?)?)?;
        h.i((.., 0))?.contiguous()
    }
}

struct Vision {
    patch: Conv2d,
    pos: Tensor,
    layers: Vec<Layer>,
    post: LayerNorm,
    head: Head,
}

impl Vision {
    fn new(vb: VarBuilder) -> candle_core::Result<Self> {
        let e = vb.pp("embeddings");
        let cfg = Conv2dConfig { stride: PATCH, ..Default::default() };
        let n = (IMAGE / PATCH) * (IMAGE / PATCH);
        Ok(Self {
            patch: candle_nn::conv2d(3, DIM, PATCH, cfg, e.pp("patch_embedding"))?,
            pos: e.pp("position_embedding").get((n, DIM), "weight")?,
            layers: layers(vb.pp("encoder").pp("layers"))?,
            post: candle_nn::layer_norm(DIM, EPS, vb.pp("post_layernorm"))?,
            head: Head::new(vb.pp("head"))?,
        })
    }

    /// `[b, 3, 224, 224]` (−1..1) → `[b, DIM]`.
    fn forward(&self, pixels: &Tensor) -> candle_core::Result<Tensor> {
        let x = self.patch.forward(pixels)?.flatten_from(2)?.transpose(1, 2)?;
        let mut x = x.broadcast_add(&self.pos)?;
        for l in &self.layers {
            x = l.forward(&x)?;
        }
        self.head.forward(&self.post.forward(&x)?)
    }
}

struct Text {
    pos: Tensor,
    layers: Vec<Layer>,
    norm: LayerNorm,
    head: Linear,
}

impl Text {
    fn new(vb: VarBuilder) -> candle_core::Result<Self> {
        Ok(Self {
            pos: vb.pp("embeddings").pp("position_embedding").get((CONTEXT, DIM), "weight")?,
            layers: layers(vb.pp("encoder").pp("layers"))?,
            norm: candle_nn::layer_norm(DIM, EPS, vb.pp("final_layer_norm"))?,
            head: candle_nn::linear(DIM, DIM, vb.pp("head"))?,
        })
    }

    /// `[b, CONTEXT, DIM]` token embeddings → `[b, DIM]` (the last position carries the sentence).
    fn forward(&self, tokens: &Tensor) -> candle_core::Result<Tensor> {
        let mut x = tokens.broadcast_add(&self.pos)?;
        for l in &self.layers {
            x = l.forward(&x)?;
        }
        let last = self.norm.forward(&x)?.i((.., CONTEXT - 1))?.contiguous()?;
        self.head.forward(&last)
    }
}

/// How the image is shrunk to the model's input: SigLIP 1 uses bicubic, SigLIP 2 bilinear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resample {
    Bilinear,
    Bicubic,
}

pub struct Embedder {
    device: Device,
    vision: Vision,
    text: Text,
    /// Token embeddings stay on the CPU (SigLIP 2's table is 780 MB; a query needs 64 rows).
    tokens: Tensor,
    tokenizer: Tokenizer,
    scale: f32,
    bias: f32,
    resample: Resample,
    dtype: DType,
    id: String,
}

/// Whether `dir` holds a usable checkpoint.
pub fn is_model_dir(dir: &Path) -> bool {
    ["model.safetensors", "tokenizer.json", "preprocessor_config.json"].iter().all(|f| dir.join(f).is_file())
}

/// What identifies the checkpoint in `dir`, hence the vectors it makes: its size (two SigLIP
/// variants never share one), so vectors made by one are never read as another's even when the
/// files are swapped in one folder. Costs a `stat`, not a load.
pub fn model_id(dir: &Path) -> String {
    format!("siglip-{}", std::fs::metadata(dir.join("model.safetensors")).map(|m| m.len()).unwrap_or(0))
}

/// The best device here: Metal on macOS (when a GPU is available), else the CPU.
pub fn best_device() -> Device {
    #[cfg(target_os = "macos")]
    if let Ok(d) = Device::new_metal(0) {
        return d;
    }
    Device::Cpu
}

impl Embedder {
    pub fn load(dir: &Path) -> Result<Self> {
        Self::load_on(dir, best_device(), DType::F32)
    }

    /// Load from `dir` (`model.safetensors`, `tokenizer.json`, `preprocessor_config.json`) onto
    /// `device`, running in `dtype` (F32, or F16 for speed).
    pub fn load_on(dir: &Path, device: Device, dtype: DType) -> Result<Self> {
        if !is_model_dir(dir) {
            return Err(Error::Missing(dir.to_path_buf()));
        }
        let pre: serde_json::Value = std::fs::read_to_string(dir.join("preprocessor_config.json"))
            .map_err(|e| Error::Model(format!("preprocessor_config.json: {e}")))
            .and_then(|t| serde_json::from_str(&t).map_err(|e| Error::Model(format!("preprocessor_config.json: {e}"))))?;
        let resample = if pre.get("resample").and_then(serde_json::Value::as_u64) == Some(3) { Resample::Bicubic } else { Resample::Bilinear };
        let mut all = candle_core::safetensors::load(dir.join("model.safetensors"), &Device::Cpu)?;
        let tokens = all.remove("text_model.embeddings.token_embedding.weight").ok_or_else(|| Error::Model("missing the token embeddings".into()))?;
        let mut on_device: HashMap<String, Tensor> = HashMap::with_capacity(all.len());
        for (name, t) in all {
            on_device.insert(name, t.to_dtype(dtype)?.to_device(&device)?);
        }
        let scalar = |name: &str| -> Result<f32> {
            let t = on_device.get(name).ok_or_else(|| Error::Model(format!("missing `{name}`")))?;
            Ok(t.to_dtype(DType::F32)?.to_vec1::<f32>()?.first().copied().unwrap_or(0.0))
        };
        let (scale, bias) = (scalar("logit_scale")?.exp(), scalar("logit_bias")?);
        let vb = VarBuilder::from_tensors(on_device, dtype, &device);
        let vision = Vision::new(vb.pp("vision_model")).map_err(|e| Error::Model(format!("vision tower: {e} (only the base 224 px models are supported)")))?;
        let text = Text::new(vb.pp("text_model")).map_err(|e| Error::Model(format!("text tower: {e}")))?;
        let tokenizer = Tokenizer::load(dir)?;
        let id = model_id(dir);
        Ok(Self { device, vision, text, tokens: tokens.to_dtype(dtype)?, tokenizer, scale, bias, resample, dtype, id })
    }

    pub fn resample(&self) -> Resample {
        self.resample
    }

    /// Identifies the checkpoint (and so the vectors it makes).
    pub fn id(&self) -> &str {
        &self.id
    }

    pub const fn dim(&self) -> usize {
        DIM
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Unit-length image embeddings for preprocessed images (each `3 × 224 × 224` floats, see
    /// [`crate::preprocess`]).
    pub fn embed_images(&self, images: &[Vec<f32>]) -> Result<Vec<Vec<f32>>> {
        if images.is_empty() {
            return Ok(Vec::new());
        }
        let mut flat = Vec::with_capacity(images.len() * 3 * IMAGE * IMAGE);
        for im in images {
            if im.len() != 3 * IMAGE * IMAGE {
                return Err(Error::Model("an image is not 224 × 224 × 3".into()));
            }
            flat.extend_from_slice(im);
        }
        let px = Tensor::from_vec(flat, (images.len(), 3, IMAGE, IMAGE), &self.device)?.to_dtype(self.dtype)?;
        unit_rows(&self.vision.forward(&px)?)
    }

    /// Unit-length text embeddings.
    pub fn embed_texts(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<u32> = texts.iter().flat_map(|t| self.tokenizer.encode(t)).collect();
        let ids = Tensor::from_vec(ids, texts.len() * CONTEXT, &Device::Cpu)?;
        let emb = self.tokens.index_select(&ids, 0)?.reshape((texts.len(), CONTEXT, DIM))?.to_device(&self.device)?;
        unit_rows(&self.text.forward(&emb)?)
    }

    pub fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        self.embed_texts(&[text])?.pop().ok_or_else(|| Error::Model("no embedding".into()))
    }

    /// The probability (0..1) that an image and a caption with this cosine similarity match:
    /// the model's sigmoid, with its learned scale and bias.
    pub fn probability(&self, cosine: f32) -> f32 {
        1.0 / (1.0 + (-(cosine * self.scale + self.bias)).exp())
    }

    /// Wait for queued GPU work (benchmarks).
    pub fn sync(&self) -> Result<()> {
        Ok(self.device.synchronize()?)
    }
}

fn unit_rows(t: &Tensor) -> Result<Vec<Vec<f32>>> {
    let t = t.to_dtype(DType::F32)?;
    let n = t.sqr()?.sum_keepdim(1)?.sqrt()?;
    Ok(t.broadcast_div(&n)?.to_vec2::<f32>()?)
}
