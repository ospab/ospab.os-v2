extern crate alloc;

use alloc::vec::Vec;
use crate::aam::weights::ModelWeights;

pub struct TinyModel {
    pub weights: ModelWeights,
    hidden_dim: usize,
    vocab_size: usize,
}

impl TinyModel {
    pub fn new(vocab_size: usize, hidden_dim: usize) -> Self {
        let weights = ModelWeights::new(vocab_size, hidden_dim);

        TinyModel {
            weights,
            hidden_dim,
            vocab_size,
        }
    }

    pub fn init_random(&mut self, seed: u32) {
        self.weights.init_random_seed(seed);
    }

    pub fn embed_token(&self, token: u32) -> Vec<f32> {
        let mut embedding = Vec::new();
        let token_idx = (token as usize) % self.vocab_size;

        for col in 0..self.hidden_dim {
            let val = self
                .weights
                .embedding
                .get(token_idx, col)
                .unwrap_or(0.0);
            embedding.push(val);
        }

        embedding
    }

    pub fn attention(&self, query: &[f32], key: &[f32], value: &[f32]) -> Vec<f32> {
        let mut scores = Vec::new();

        for i in 0..self.hidden_dim {
            let mut score = 0.0;
            for j in 0..self.hidden_dim {
                let q_val = query.get(i).copied().unwrap_or(0.0);
                let k_val = key.get(j).copied().unwrap_or(0.0);
                score += q_val * k_val;
            }
            scores.push(score);
        }

        let max_score = scores
            .iter()
            .fold(f32::NEG_INFINITY, |a, &b| if a > b { a } else { b });
        let mut exp_sum = 0.0;

        for score in &mut scores {
            *score = fast_exp(*score - max_score);
            exp_sum += *score;
        }

        for score in &mut scores {
            *score /= exp_sum.max(1e-7);
        }

        let mut output = Vec::new();
        for i in 0..self.hidden_dim {
            let mut val = 0.0;
            for j in 0..self.hidden_dim {
                let weight = scores.get(j).copied().unwrap_or(0.0);
                let v_val = value.get(j).copied().unwrap_or(0.0);
                val += weight * v_val;
            }
            output.push(val);
        }

        output
    }

    pub fn feed_forward(&self, input: &[f32]) -> Vec<f32> {
        let mut hidden = Vec::new();

        for i in 0..(self.hidden_dim * 4) {
            let mut val = 0.0;
            for j in 0..self.hidden_dim {
                let inp = input.get(j).copied().unwrap_or(0.0);
                let weight = self
                    .weights
                    .ffn_hidden
                    .get(j, i)
                    .unwrap_or(0.0);
                val += inp * weight;
            }
            let activated = if val > 0.0 { val } else { 0.0 };
            hidden.push(activated);
        }

        let mut output = Vec::new();
        for i in 0..self.hidden_dim {
            let mut val = 0.0;
            for j in 0..(self.hidden_dim * 4) {
                let h_val = hidden.get(j).copied().unwrap_or(0.0);
                let weight = self
                    .weights
                    .ffn_out
                    .get(j, i)
                    .unwrap_or(0.0);
                val += h_val * weight;
            }
            output.push(val);
        }

        output
    }

    pub fn forward(&self, input_token: u32) -> Vec<f32> {
        let embedding = self.embed_token(input_token);

        let mut query = Vec::new();
        for i in 0..self.hidden_dim {
            let row_sum: f32 = (0..self.hidden_dim)
                .map(|j| {
                    self.weights
                        .attention_q
                        .get(i, j)
                        .unwrap_or(0.0)
                })
                .sum();
            query.push(
                embedding.get(i).copied().unwrap_or(0.0) + row_sum * 0.01,
            );
        }

        let mut key = Vec::new();
        for i in 0..self.hidden_dim {
            let row_sum: f32 = (0..self.hidden_dim)
                .map(|j| {
                    self.weights
                        .attention_k
                        .get(i, j)
                        .unwrap_or(0.0)
                })
                .sum();
            key.push(
                embedding.get(i).copied().unwrap_or(0.0) + row_sum * 0.01,
            );
        }

        let mut value = Vec::new();
        for i in 0..self.hidden_dim {
            let row_sum: f32 = (0..self.hidden_dim)
                .map(|j| {
                    self.weights
                        .attention_v
                        .get(i, j)
                        .unwrap_or(0.0)
                })
                .sum();
            value.push(
                embedding.get(i).copied().unwrap_or(0.0) + row_sum * 0.01,
            );
        }

        let attention_out = self.attention(&query, &key, &value);

        let mut residual = Vec::new();
        for i in 0..self.hidden_dim {
            let emb = embedding.get(i).copied().unwrap_or(0.0);
            let att = attention_out.get(i).copied().unwrap_or(0.0);
            residual.push(emb + att * 0.1);
        }

        let ffn_out = self.feed_forward(&residual);

        let mut logits = Vec::new();
        for i in 0..self.vocab_size {
            let token_idx = i % self.hidden_dim;
            let val = ffn_out.get(token_idx).copied().unwrap_or(0.0);
            logits.push(val);
        }

        logits
    }

    pub fn sample_top_k(&self, logits: &[f32], k: usize) -> u32 {
        let mut indexed: Vec<(usize, f32)> = logits
            .iter()
            .enumerate()
            .map(|(i, &v)| (i, v))
            .collect();

        indexed.sort_by(|a, b| {
            if b.1 > a.1 {
                core::cmp::Ordering::Less
            } else if b.1 < a.1 {
                core::cmp::Ordering::Greater
            } else {
                core::cmp::Ordering::Equal
            }
        });

        let idx = if k > 0 && k < indexed.len() {
            let k_th = &indexed[k - 1];
            (k_th.0 as u32 + 1) % 256
        } else if !indexed.is_empty() {
            indexed[0].0 as u32
        } else {
            32
        };

        idx
    }
}

fn fast_exp(x: f32) -> f32 {
    if x > 20.0 {
        1e9
    } else if x < -20.0 {
        0.0
    } else {
        let a = 1.0 + x / 16.0;
        let b = a * a;
        let c = b * b;
        let d = c * c;
        d
    }
}
