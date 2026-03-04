/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

ANE Layers — Building blocks for neural networks.

  Linear      — y = xW^T + b
  LayerNorm   — LN(x) = γ * (x − μ) / √(σ² + ε) + β
  Attention   — Multi-head scaled dot-product attention
  Embedding   — Token → vector lookup table
*/

extern crate alloc;

use alloc::vec::Vec;
use alloc::vec;

use super::tensor::{DataType, Tape, Tensor};

/// Newton-Raphson square root (avoids libm dependency in no_std).
#[inline(always)]
fn sqrtf(x: f32) -> f32 {
    if x <= 0.0 { return 0.0; }
    let mut y = f32::from_bits((x.to_bits().wrapping_add(0x3f80_0000)) >> 1);
    y = 0.5 * (y + x / y);
    y = 0.5 * (y + x / y);
    y
}

// ─── Layer trait ─────────────────────────────────────────────────────────────

pub trait Layer {
    /// Forward pass: input variable id → output variable id on `tape`.
    fn forward(&self, tape: &mut Tape, input: usize) -> usize;

    /// Collect mutable references to all parameter tensors.
    fn parameters(&mut self) -> Vec<&mut Tensor>;

    /// Zero gradients of all parameters on the tape.
    fn zero_grad(&self, tape: &mut Tape, param_ids: &[usize]) {
        for &id in param_ids {
            tape.vars[id].zero_grad();
        }
    }
}

// ─── Linear ──────────────────────────────────────────────────────────────────

/// Fully-connected layer: y = x W^T + b
///
/// Weights W: [out_features, in_features]
/// Bias    b: [1, out_features]
pub struct Linear {
    pub weight: Tensor,  // [out, in]
    pub bias:   Tensor,  // [1, out]
    pub in_features:  usize,
    pub out_features: usize,
    /// Parameter variable ids on the shared tape (set after register).
    pub weight_id: usize,
    pub bias_id:   usize,
}

impl Linear {
    /// Xavier uniform initialisation.
    pub fn new(in_features: usize, out_features: usize) -> Self {
        let bound = sqrtf(6.0f32 / (in_features + out_features) as f32);
        let w_len = out_features * in_features;
        let b_len = out_features;

        let mut weight = Tensor::zeros(&[out_features, in_features], DataType::F32);
        let mut bias   = Tensor::zeros(&[1, out_features],           DataType::F32);

        // LCG pseudo-random init (no_std, deterministic)
        let mut state = 0x9E3779B9u64;
        let w_data = weight.as_f32_slice_mut();
        for i in 0..w_len {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let u = ((state >> 33) as u32) as f32 / u32::MAX as f32;
            w_data[i] = (u * 2.0 - 1.0) * bound;
        }
        let b_data = bias.as_f32_slice_mut();
        for i in 0..b_len {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let u = ((state >> 33) as u32) as f32 / u32::MAX as f32;
            b_data[i] = (u * 2.0 - 1.0) * bound * 0.1;
        }

        Linear { weight, bias, in_features, out_features, weight_id: 0, bias_id: 0 }
    }

    /// Register weight and bias as tape variables (requires_grad = true).
    pub fn register(&mut self, tape: &mut Tape) {
        self.weight_id = tape.var(self.weight.clone_tensor(), true);
        self.bias_id   = tape.var(self.bias.clone_tensor(),   true);
    }

    /// Forward using tape variable ids: out = input @ W^T + b
    /// input_id must be a [batch, in_features] matrix.
    pub fn forward_tape(&self, tape: &mut Tape, input_id: usize) -> usize {
        // W^T: [in_features, out_features]
        let wt = tape.vars[self.weight_id].data.t();
        let wt_id = tape.var(wt, false);
        let mm = tape.matmul(input_id, wt_id);
        // Broadcast bias [1, out] to [batch, out] via add
        tape.add(mm, self.bias_id)
    }
}

impl Layer for Linear {
    fn forward(&self, tape: &mut Tape, input_id: usize) -> usize {
        self.forward_tape(tape, input_id)
    }

    fn parameters(&mut self) -> Vec<&mut Tensor> {
        vec![&mut self.weight, &mut self.bias]
    }
}

// ─── LayerNorm ───────────────────────────────────────────────────────────────

/// Layer normalisation: y = γ * (x − μ) / √(σ² + ε) + β
pub struct LayerNorm {
    pub gamma: Tensor,  // [1, d_model]
    pub beta:  Tensor,  // [1, d_model]
    pub d_model:   usize,
    pub epsilon:   f32,
    pub gamma_id: usize,
    pub beta_id:  usize,
}

impl LayerNorm {
    pub fn new(d_model: usize) -> Self {
        let gamma = Tensor::full_f32(&[1, d_model], 1.0);
        let beta  = Tensor::zeros(&[1, d_model], DataType::F32);
        LayerNorm { gamma, beta, d_model, epsilon: 1e-5, gamma_id: 0, beta_id: 0 }
    }

    pub fn register(&mut self, tape: &mut Tape) {
        self.gamma_id = tape.var(self.gamma.clone_tensor(), true);
        self.beta_id  = tape.var(self.beta.clone_tensor(),  true);
    }

    /// Normalise each row of input [batch, d_model].
    /// Returns variable id of normalised output on tape.
    pub fn forward_tape(&self, tape: &mut Tape, input_id: usize) -> usize {
        let cols = self.d_model;
        let rows = tape.vars[input_id].data.len / cols;
        let src  = tape.vars[input_id].data.as_f32_slice().to_vec();

        // Compute mean and var per row
        let mut normed = Tensor::zeros(&[rows, cols], DataType::F32);
        {
            let dst = normed.as_f32_slice_mut();
            for r in 0..rows {
                let mut mu = 0.0f32;
                for c in 0..cols { mu += src[r*cols + c]; }
                mu /= cols as f32;
                let mut var = 0.0f32;
                for c in 0..cols {
                    let d = src[r*cols + c] - mu;
                    var += d * d;
                }
                var = sqrtf(var / cols as f32 + self.epsilon);
                for c in 0..cols {
                    dst[r*cols + c] = (src[r*cols + c] - mu) / var;
                }
            }
        }
        let normed_id = tape.var(normed, false);

        // Scale and shift: y = γ * norm + β
        let scaled = tape.mul(normed_id, self.gamma_id);
        tape.add(scaled, self.beta_id)
    }
}

impl Layer for LayerNorm {
    fn forward(&self, tape: &mut Tape, input_id: usize) -> usize {
        self.forward_tape(tape, input_id)
    }
    fn parameters(&mut self) -> Vec<&mut Tensor> {
        vec![&mut self.gamma, &mut self.beta]
    }
}

// ─── Embedding ───────────────────────────────────────────────────────────────

/// Token embedding: integer token id → dense vector.
pub struct Embedding {
    pub weight: Tensor,  // [vocab_size, d_model]
    pub vocab_size: usize,
    pub d_model:    usize,
    pub weight_id:  usize,
}

impl Embedding {
    pub fn new(vocab_size: usize, d_model: usize) -> Self {
        let bound = sqrtf(1.0f32 / d_model as f32);
        let mut weight = Tensor::zeros(&[vocab_size, d_model], DataType::F32);
        let mut state = 0xDEADBEEFu64;
        let w = weight.as_f32_slice_mut();
        for v in w.iter_mut() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let u = ((state >> 33) as u32) as f32 / u32::MAX as f32;
            *v = (u * 2.0 - 1.0) * bound;
        }
        Embedding { weight, vocab_size, d_model, weight_id: 0 }
    }

    pub fn register(&mut self, tape: &mut Tape) {
        self.weight_id = tape.var(self.weight.clone_tensor(), true);
    }

    /// Lookup token embeddings for a sequence of token ids.
    /// `tokens`: slice of token ids.  Returns a [seq_len, d_model] tensor id.
    pub fn lookup(&self, tape: &mut Tape, tokens: &[usize]) -> usize {
        let seq = tokens.len();
        let d   = self.d_model;
        let mut out = Tensor::zeros(&[seq, d], DataType::F32);
        {
            let w   = tape.vars[self.weight_id].data.as_f32_slice();
            let dst = out.as_f32_slice_mut();
            for (i, &tok) in tokens.iter().enumerate() {
                let tok = tok.min(self.vocab_size - 1);
                for j in 0..d {
                    dst[i*d + j] = w[tok*d + j];
                }
            }
        }
        tape.var(out, true)
    }
}

impl Layer for Embedding {
    fn forward(&self, tape: &mut Tape, input_id: usize) -> usize {
        // `input_id` is unused in lookup path; embeddings are looked up via `lookup()`
        input_id
    }
    fn parameters(&mut self) -> Vec<&mut Tensor> {
        vec![&mut self.weight]
    }
}

// ─── Multi-Head Attention ─────────────────────────────────────────────────────

/// Scaled dot-product multi-head attention.
///
/// Q, K, V projections: [d_model → d_model] (split into h heads)
/// Output projection:   [d_model → d_model]
pub struct MultiHeadAttention {
    pub wq:      Linear,
    pub wk:      Linear,
    pub wv:      Linear,
    pub wo:      Linear,
    pub n_heads: usize,
    pub d_model: usize,
    pub d_head:  usize,
}

impl MultiHeadAttention {
    pub fn new(d_model: usize, n_heads: usize) -> Self {
        assert_eq!(d_model % n_heads, 0, "ANE: d_model must be divisible by n_heads");
        let d_head = d_model / n_heads;
        MultiHeadAttention {
            wq: Linear::new(d_model, d_model),
            wk: Linear::new(d_model, d_model),
            wv: Linear::new(d_model, d_model),
            wo: Linear::new(d_model, d_model),
            n_heads,
            d_model,
            d_head,
        }
    }

    pub fn register(&mut self, tape: &mut Tape) {
        self.wq.register(tape);
        self.wk.register(tape);
        self.wv.register(tape);
        self.wo.register(tape);
    }

    /// Forward pass (single-head simplification for graph; full MH in compiled path).
    /// input_id: [seq, d_model]
    pub fn forward_tape(&self, tape: &mut Tape, input_id: usize) -> usize {
        let scale = 1.0f32 / sqrtf(self.d_head as f32);
        let seq  = tape.vars[input_id].data.rows();

        // Q, K, V projections
        let q_id = self.wq.forward_tape(tape, input_id);
        let k_id = self.wk.forward_tape(tape, input_id);
        let v_id = self.wv.forward_tape(tape, input_id);

        // Scores = Q K^T / sqrt(d_head), then softmax, then * V
        let q = tape.vars[q_id].data.as_f32_slice().to_vec();
        let k = tape.vars[k_id].data.as_f32_slice().to_vec();
        let v = tape.vars[v_id].data.as_f32_slice().to_vec();
        let d = self.d_model;

        // scores: [seq, seq]
        let mut scores = Tensor::zeros(&[seq, seq], DataType::F32);
        {
            let s = scores.as_f32_slice_mut();
            for i in 0..seq {
                for j in 0..seq {
                    let mut dot = 0.0f32;
                    for dd in 0..d { dot += q[i*d + dd] * k[j*d + dd]; }
                    s[i*seq + j] = dot * scale;
                }
            }
        }
        scores.softmax_inplace();

        // context = scores @ V: [seq, d_model]
        let context = scores.matmul(&Tensor::from_flat_f32(&v, seq, d));
        let ctx_id  = tape.var(context, true);

        // Output projection
        self.wo.forward_tape(tape, ctx_id)
    }
}

impl Layer for MultiHeadAttention {
    fn forward(&self, tape: &mut Tape, input_id: usize) -> usize {
        self.forward_tape(tape, input_id)
    }
    fn parameters(&mut self) -> Vec<&mut Tensor> {
        let mut p = Vec::new();
        for pp in self.wq.parameters() { p.push(pp); }
        for pp in self.wk.parameters() { p.push(pp); }
        for pp in self.wv.parameters() { p.push(pp); }
        for pp in self.wo.parameters() { p.push(pp); }
        p
    }
}

// ─── TransformerBlock (bonus convenience) ────────────────────────────────────

/// One transformer block: Attention + FFN + residuals.
pub struct TransformerBlock {
    pub attn:  MultiHeadAttention,
    pub norm1: LayerNorm,
    pub ff1:   Linear,
    pub ff2:   Linear,
    pub norm2: LayerNorm,
}

impl TransformerBlock {
    pub fn new(d_model: usize, n_heads: usize, d_ff: usize) -> Self {
        TransformerBlock {
            attn:  MultiHeadAttention::new(d_model, n_heads),
            norm1: LayerNorm::new(d_model),
            ff1:   Linear::new(d_model, d_ff),
            ff2:   Linear::new(d_ff, d_model),
            norm2: LayerNorm::new(d_model),
        }
    }

    pub fn register(&mut self, tape: &mut Tape) {
        self.attn.register(tape);
        self.norm1.register(tape);
        self.ff1.register(tape);
        self.ff2.register(tape);
        self.norm2.register(tape);
    }

    pub fn forward_tape(&self, tape: &mut Tape, input_id: usize) -> usize {
        // Self-attention sub-layer + residual
        let attn_out  = self.attn.forward_tape(tape, input_id);
        let residual1 = tape.add(input_id, attn_out);
        let norm1_out = self.norm1.forward_tape(tape, residual1);

        // Feed-forward sub-layer + residual
        let ff1_out   = self.ff1.forward_tape(tape, norm1_out);
        let relu_out  = tape.relu(ff1_out);
        let ff2_out   = self.ff2.forward_tape(tape, relu_out);
        let residual2 = tape.add(norm1_out, ff2_out);
        self.norm2.forward_tape(tape, residual2)
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tensor::{Tape, Tensor, DataType};

    // ── sqrtf helper ─────────────────────────────────────────────────────────

    #[test]
    fn test_sqrtf_accuracy() {
        let cases = [(0.0f32, 0.0f32), (1.0, 1.0), (4.0, 2.0), (9.0, 3.0), (2.0, 1.41421356)];
        for (x, expected) in cases {
            let got = sqrtf(x);
            assert!((got - expected).abs() < 1e-4, "sqrtf({x})={got}, expected {expected}");
        }
    }

    #[test]
    fn test_sqrtf_negative_is_zero() {
        assert_eq!(sqrtf(-1.0), 0.0);
        assert_eq!(sqrtf(-100.0), 0.0);
    }

    // ── Linear ───────────────────────────────────────────────────────────────

    #[test]
    fn test_linear_output_shape() {
        let layer = Linear::new(8, 4);
        // weight: [4, 8], bias: [1, 4]
        assert_eq!(layer.weight.shape(), &[4, 8]);
        assert_eq!(layer.bias.shape(), &[1, 4]);
        assert_eq!(layer.in_features,  8);
        assert_eq!(layer.out_features, 4);
    }

    #[test]
    fn test_linear_forward_tape_shape() {
        let mut tape = Tape::new();
        let mut layer = Linear::new(4, 2);
        let input_data = Tensor::zeros(&[1, 4], DataType::F32);
        let in_id = tape.leaf(input_data, false);
        layer.register(&mut tape);
        let out_id = layer.forward_tape(&mut tape, in_id);
        let out_shape = tape.vars[out_id].data.shape().to_vec();
        // Output should be [1, 2]
        assert_eq!(out_shape, vec![1, 2],
            "Linear(4→2) output shape should be [1,2], got {:?}", out_shape);
    }

    #[test]
    fn test_linear_zero_input_gives_bias() {
        // When input is zero, output is exactly the bias vector.
        let mut tape = Tape::new();
        let mut layer = Linear::new(4, 2);
        // Zero out weights so output = bias
        let wd = layer.weight.as_f32_slice_mut();
        for v in wd.iter_mut() { *v = 0.0; }
        let bd = layer.bias.as_f32_slice_mut();
        bd[0] = 1.5; bd[1] = -0.5;

        let input_data = Tensor::zeros(&[1, 4], DataType::F32);
        let in_id = tape.leaf(input_data, false);
        layer.register(&mut tape);
        let out_id = layer.forward_tape(&mut tape, in_id);
        let out = tape.vars[out_id].data.as_f32_slice().to_vec();
        assert!((out[0] - 1.5).abs() < 1e-5, "expected 1.5, got {}", out[0]);
        assert!((out[1] - (-0.5)).abs() < 1e-5, "expected -0.5, got {}", out[1]);
    }

    // ── LayerNorm ─────────────────────────────────────────────────────────────

    #[test]
    fn test_layernorm_output_shape() {
        let norm = LayerNorm::new(8);
        assert_eq!(norm.gamma.shape(), &[8]);
        assert_eq!(norm.beta.shape(), &[8]);
        assert_eq!(norm.d_model, 8);
    }

    #[test]
    fn test_layernorm_normalises() {
        let mut tape = Tape::new();
        let data = Tensor::from_slice_f32(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        let in_id = tape.leaf(data, false);
        let mut norm = LayerNorm::new(8);
        norm.register(&mut tape);
        let out_id = norm.forward_tape(&mut tape, in_id);
        let out = tape.vars[out_id].data.as_f32_slice().to_vec();
        // Post-layernorm output should have mean ≈ 0 and std ≈ 1 (γ=1, β=0 initially)
        let mean: f32 = out.iter().sum::<f32>() / out.len() as f32;
        let var: f32 = out.iter().map(|&x| (x - mean).powi(2)).sum::<f32>() / out.len() as f32;
        assert!(mean.abs() < 1e-4, "LayerNorm mean={mean}, expected ~0");
        assert!((var - 1.0).abs() < 0.1, "LayerNorm var={var}, expected ~1");
    }

    // ── Embedding ─────────────────────────────────────────────────────────────

    #[test]
    fn test_embedding_shape() {
        let emb = Embedding::new(100, 16);
        assert_eq!(emb.vocab_size, 100);
        assert_eq!(emb.d_model,    16);
        assert_eq!(emb.table.len, 100 * 16);
    }

    #[test]
    fn test_embedding_different_tokens_differ() {
        let emb = Embedding::new(50, 8);
        let row0: Vec<f32> = (0..8).map(|i| emb.table.get_f32(i)).collect();
        let row1: Vec<f32> = (8..16).map(|i| emb.table.get_f32(i)).collect();
        // LCG initialisation ensures rows are different
        assert_ne!(row0, row1, "different token embeddings should differ");
    }
}
