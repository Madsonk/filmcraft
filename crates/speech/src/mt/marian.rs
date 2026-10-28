//! The Marian machine-translation network (Junczys-Dowmunt et al. 2018), as released for Opus-MT:
//! a post-LayerNorm encoder–decoder transformer with scaled token embeddings, fixed sinusoidal
//! positions (sines in the first half of the dimensions, cosines in the second half), ReLU/SiLU
//! feed-forward blocks, tied input/output embeddings and a `final_logits_bias`. Written against
//! candle from the published architecture, reading the tensor names of the Hugging Face
//! safetensors release. Decoding is beam search with a key/value cache per beam.

use candle_core::{DType, Device, IndexOp, Module, Result, Tensor};
use candle_nn::{Embedding, LayerNorm, Linear, VarBuilder};

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Config {
    pub d_model: usize,
    pub encoder_layers: usize,
    pub decoder_layers: usize,
    pub encoder_attention_heads: usize,
    pub decoder_attention_heads: usize,
    pub vocab_size: usize,
    pub encoder_ffn_dim: usize,
    pub decoder_ffn_dim: usize,
    pub pad_token_id: u32,
    pub eos_token_id: u32,
    pub decoder_start_token_id: u32,
    #[serde(default)]
    pub scale_embedding: bool,
    #[serde(default)]
    pub activation_function: String,
    #[serde(default)]
    pub normalize_embedding: bool,
    #[serde(default = "default_positions")]
    pub max_position_embeddings: usize,
}

fn default_positions() -> usize {
    512
}

struct Attn {
    q: Linear,
    k: Linear,
    v: Linear,
    o: Linear,
    heads: usize,
}

impl Attn {
    fn new(d: usize, heads: usize, vb: VarBuilder) -> Result<Self> {
        Ok(Self {
            q: candle_nn::linear(d, d, vb.pp("q_proj"))?,
            k: candle_nn::linear(d, d, vb.pp("k_proj"))?,
            v: candle_nn::linear(d, d, vb.pp("v_proj"))?,
            o: candle_nn::linear(d, d, vb.pp("out_proj"))?,
            heads,
        })
    }

    fn split(&self, x: &Tensor) -> Result<Tensor> {
        let (b, t, d) = x.dims3()?;
        x.reshape((b, t, self.heads, d / self.heads))?.transpose(1, 2)?.contiguous()
    }

    fn project_kv(&self, x: &Tensor) -> Result<(Tensor, Tensor)> {
        Ok((self.split(&self.k.forward(x)?)?, self.split(&self.v.forward(x)?)?))
    }

    /// Attention of `x` (b, t, d) over keys/values `(k, v)` (b, heads, tk, dh); `mask` (additive) optional.
    fn attend(&self, x: &Tensor, k: &Tensor, v: &Tensor, mask: Option<&Tensor>) -> Result<Tensor> {
        let (b, t, d) = x.dims3()?;
        let dh = d / self.heads;
        let q = (self.split(&self.q.forward(x)?)? * (dh as f64).powf(-0.5))?;
        let mut qk = q.matmul(&k.transpose(2, 3)?.contiguous()?)?;
        if let Some(m) = mask {
            qk = qk.broadcast_add(m)?;
        }
        let w = candle_nn::ops::softmax_last_dim(&qk)?;
        let o = w.matmul(v)?.transpose(1, 2)?.contiguous()?.reshape((b, t, d))?;
        self.o.forward(&o)
    }
}

struct Ffn {
    fc1: Linear,
    fc2: Linear,
    silu: bool,
}

impl Ffn {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        let h = self.fc1.forward(x)?;
        let h = if self.silu { h.silu()? } else { h.relu()? };
        self.fc2.forward(&h)
    }
}

fn ffn(cfg: &Config, d: usize, ff: usize, vb: &VarBuilder) -> Result<Ffn> {
    Ok(Ffn {
        fc1: candle_nn::linear(d, ff, vb.pp("fc1"))?,
        fc2: candle_nn::linear(ff, d, vb.pp("fc2"))?,
        silu: matches!(cfg.activation_function.as_str(), "swish" | "silu"),
    })
}

struct EncLayer {
    attn: Attn,
    ln1: LayerNorm,
    ffn: Ffn,
    ln2: LayerNorm,
}

struct DecLayer {
    attn: Attn,
    ln1: LayerNorm,
    cross: Attn,
    ln2: LayerNorm,
    ffn: Ffn,
    ln3: LayerNorm,
}

/// Self-attention cache of one decoder layer plus the cross-attention keys/values (computed once).
struct LayerCache {
    k: Option<Tensor>,
    v: Option<Tensor>,
    ck: Tensor,
    cv: Tensor,
}

pub struct Marian {
    pub cfg: Config,
    shared: Embedding,
    shared_t: Tensor,
    bias: Tensor,
    enc: Vec<EncLayer>,
    dec: Vec<DecLayer>,
    enc_ln_emb: Option<LayerNorm>,
    dec_ln_emb: Option<LayerNorm>,
    pos: Tensor,
    device: Device,
}

/// Fixed sinusoidal position table (n, d): sines of `pos · 10000^(-2i/d)` in the first d/2
/// columns, cosines in the last d/2.
pub fn sinusoid(n: usize, d: usize, dev: &Device) -> Result<Tensor> {
    let half = d / 2;
    let mut v = vec![0f32; n * d];
    for p in 0..n {
        for i in 0..half {
            let a = p as f64 / 10000f64.powf(2.0 * i as f64 / d as f64);
            v[p * d + i] = a.sin() as f32;
            v[p * d + half + i] = a.cos() as f32;
        }
    }
    Tensor::from_vec(v, (n, d), dev)
}

impl Marian {
    pub fn load(cfg: Config, weights: Vec<u8>) -> Result<Self> {
        let device = Device::Cpu;
        let vb = VarBuilder::from_buffered_safetensors(weights, DType::F32, &device)?;
        let d = cfg.d_model;
        let m = vb.pp("model");
        let shared = candle_nn::embedding(cfg.vocab_size, d, m.pp("shared"))?;
        let shared_t = shared.embeddings().t()?.contiguous()?;
        let bias = vb.get((1, cfg.vocab_size), "final_logits_bias")?;
        let mut enc = Vec::new();
        for i in 0..cfg.encoder_layers {
            let l = m.pp(format!("encoder.layers.{i}"));
            enc.push(EncLayer {
                attn: Attn::new(d, cfg.encoder_attention_heads, l.pp("self_attn"))?,
                ln1: candle_nn::layer_norm(d, 1e-5, l.pp("self_attn_layer_norm"))?,
                ffn: ffn(&cfg, d, cfg.encoder_ffn_dim, &l)?,
                ln2: candle_nn::layer_norm(d, 1e-5, l.pp("final_layer_norm"))?,
            });
        }
        let mut dec = Vec::new();
        for i in 0..cfg.decoder_layers {
            let l = m.pp(format!("decoder.layers.{i}"));
            dec.push(DecLayer {
                attn: Attn::new(d, cfg.decoder_attention_heads, l.pp("self_attn"))?,
                ln1: candle_nn::layer_norm(d, 1e-5, l.pp("self_attn_layer_norm"))?,
                cross: Attn::new(d, cfg.decoder_attention_heads, l.pp("encoder_attn"))?,
                ln2: candle_nn::layer_norm(d, 1e-5, l.pp("encoder_attn_layer_norm"))?,
                ffn: ffn(&cfg, d, cfg.decoder_ffn_dim, &l)?,
                ln3: candle_nn::layer_norm(d, 1e-5, l.pp("final_layer_norm"))?,
            });
        }
        let (enc_ln_emb, dec_ln_emb) = if cfg.normalize_embedding {
            (
                Some(candle_nn::layer_norm(d, 1e-5, m.pp("encoder.layernorm_embedding"))?),
                Some(candle_nn::layer_norm(d, 1e-5, m.pp("decoder.layernorm_embedding"))?),
            )
        } else {
            (None, None)
        };
        let pos = sinusoid(cfg.max_position_embeddings, d, &device)?;
        Ok(Self { cfg, shared, shared_t, bias, enc, dec, enc_ln_emb, dec_ln_emb, pos, device })
    }

    fn embed(&self, ids: &[u32], offset: usize, batch: usize) -> Result<Tensor> {
        let t = ids.len() / batch;
        let x = Tensor::new(ids, &self.device)?.reshape((batch, t))?;
        let mut e = self.shared.forward(&x)?;
        if self.cfg.scale_embedding {
            e = (e * (self.cfg.d_model as f64).sqrt())?;
        }
        e.broadcast_add(&self.pos.narrow(0, offset, t)?)
    }

    /// Encode source token ids (ending with EOS) → (1, t, d).
    pub fn encode(&self, ids: &[u32]) -> Result<Tensor> {
        let mut x = self.embed(ids, 0, 1)?;
        if let Some(ln) = &self.enc_ln_emb {
            x = ln.forward(&x)?;
        }
        for l in &self.enc {
            let (k, v) = l.attn.project_kv(&x)?;
            x = l.ln1.forward(&(&x + l.attn.attend(&x, &k, &v, None)?)?)?;
            x = l.ln2.forward(&(&x + l.ffn.forward(&x)?)?)?;
        }
        Ok(x)
    }

    fn new_caches(&self, enc: &Tensor, beams: usize) -> Result<Vec<LayerCache>> {
        let mut out = Vec::new();
        for l in &self.dec {
            let (k, v) = l.cross.project_kv(enc)?;
            let (_, h, t, dh) = k.dims4()?;
            let ck = k.broadcast_as((beams, h, t, dh))?.contiguous()?;
            let cv = v.broadcast_as((beams, h, t, dh))?.contiguous()?;
            out.push(LayerCache { k: None, v: None, ck, cv });
        }
        Ok(out)
    }

    /// One decoder step for `beams` rows (one new token each); returns log-probabilities (beams, vocab).
    fn step(&self, tokens: &[u32], pos: usize, caches: &mut [LayerCache]) -> Result<Tensor> {
        let b = tokens.len();
        let mut x = self.embed(tokens, pos, b)?;
        if let Some(ln) = &self.dec_ln_emb {
            x = ln.forward(&x)?;
        }
        for (l, c) in self.dec.iter().zip(caches.iter_mut()) {
            let (k, v) = l.attn.project_kv(&x)?;
            let (k, v) = match (&c.k, &c.v) {
                (Some(pk), Some(pv)) => (Tensor::cat(&[pk, &k], 2)?, Tensor::cat(&[pv, &v], 2)?),
                _ => (k, v),
            };
            c.k = Some(k.clone());
            c.v = Some(v.clone());
            x = l.ln1.forward(&(&x + l.attn.attend(&x, &k, &v, None)?)?)?;
            x = l.ln2.forward(&(&x + l.cross.attend(&x, &c.ck, &c.cv, None)?)?)?;
            x = l.ln3.forward(&(&x + l.ffn.forward(&x)?)?)?;
        }
        let logits = x.broadcast_matmul(&self.shared_t)?.broadcast_add(&self.bias)?;
        candle_nn::ops::log_softmax(&logits.i((.., 0, ..))?, 1)
    }

    /// Beam-search translation of encoded source ids; returns the best hypothesis (without EOS).
    pub fn translate(&self, src: &[u32], beams: usize, max_len: usize) -> Result<Vec<u32>> {
        let beams = beams.max(1);
        let enc = self.encode(src)?;
        let mut caches = self.new_caches(&enc, beams)?;
        // (tokens without the start token, summed log-prob)
        let mut alive: Vec<(Vec<u32>, f32)> = vec![(Vec::new(), 0.0)];
        alive.resize(beams, (Vec::new(), f32::NEG_INFINITY));
        let mut finished: Vec<(Vec<u32>, f32)> = Vec::new();
        let mut last: Vec<u32> = vec![self.cfg.decoder_start_token_id; beams];
        let pad = self.cfg.pad_token_id as usize;
        for step in 0..max_len {
            let lp = self.step(&last, step, &mut caches)?.to_vec2::<f32>()?;
            // candidates over the live beams
            let mut cands: Vec<(f32, usize, u32)> = Vec::new();
            for (bi, (_, score)) in alive.iter().enumerate() {
                let row = &lp[bi];
                let mut idx: Vec<usize> = (0..row.len()).filter(|&i| i != pad).collect();
                idx.sort_unstable_by(|a, b| row[*b].total_cmp(&row[*a]));
                for &i in idx.iter().take(beams * 2) {
                    cands.push((score + row[i], bi, i as u32));
                }
            }
            cands.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
            let mut next: Vec<(Vec<u32>, f32)> = Vec::new();
            let mut src_beam: Vec<u32> = Vec::new();
            for (score, bi, tok) in cands {
                if next.len() >= beams {
                    break;
                }
                if !score.is_finite() {
                    continue;
                }
                let mut toks = alive[bi].0.clone();
                if tok == self.cfg.eos_token_id {
                    finished.push((toks, score / (step + 1) as f32));
                    continue;
                }
                toks.push(tok);
                next.push((toks, score));
                src_beam.push(bi as u32);
            }
            if finished.len() >= beams || next.is_empty() {
                break;
            }
            // the best live beam cannot beat the best finished one any more (scores only fall)
            if let Some(best) = finished.iter().map(|f| f.1).reduce(f32::max)
                && next.iter().all(|(t, s)| s / (t.len() as f32) < best)
                && step > 8
            {
                break;
            }
            // keep exactly `beams` rows: pad with dead copies, reorder the caches to the survivors
            while next.len() < beams {
                next.push((Vec::new(), f32::NEG_INFINITY));
                src_beam.push(0);
            }
            let idx = Tensor::new(src_beam.as_slice(), &self.device)?;
            for c in &mut caches {
                if let (Some(k), Some(v)) = (&c.k, &c.v) {
                    c.k = Some(k.index_select(&idx, 0)?);
                    c.v = Some(v.index_select(&idx, 0)?);
                }
            }
            last = next.iter().map(|(t, _)| t.last().copied().unwrap_or(self.cfg.decoder_start_token_id)).collect();
            alive = next;
        }
        if finished.is_empty() {
            finished.extend(alive.into_iter().map(|(t, s)| {
                let n = t.len().max(1) as f32;
                (t, s / n)
            }));
        }
        finished.sort_by(|a, b| b.1.total_cmp(&a.1));
        Ok(finished.into_iter().next().map(|f| f.0).unwrap_or_default())
    }
}
