extern crate alloc;

use alloc::vec::Vec;

pub struct WeightBuffer {
    data: Vec<f32>,
    shape: (usize, usize),
}

impl WeightBuffer {
    pub fn new(rows: usize, cols: usize) -> Self {
        WeightBuffer {
            data: Vec::new(),
            shape: (rows, cols),
        }
    }

    pub fn from_slice(data: &[f32], rows: usize, cols: usize) -> Self {
        let mut wb = WeightBuffer {
            data: Vec::new(),
            shape: (rows, cols),
        };
        for &val in data {
            wb.data.push(val);
        }
        wb
    }

    pub fn get(&self, row: usize, col: usize) -> Option<f32> {
        if row < self.shape.0 && col < self.shape.1 {
            Some(self.data[row * self.shape.1 + col])
        } else {
            None
        }
    }

    pub fn set(&mut self, row: usize, col: usize, value: f32) {
        if row < self.shape.0 && col < self.shape.1 {
            self.data[row * self.shape.1 + col] = value;
        }
    }

    pub fn rows(&self) -> usize {
        self.shape.0
    }

    pub fn cols(&self) -> usize {
        self.shape.1
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.data
    }

    pub fn as_mut_slice(&mut self) -> &mut [f32] {
        &mut self.data
    }
}

pub struct ModelWeights {
    pub embedding: WeightBuffer,
    pub attention_q: WeightBuffer,
    pub attention_k: WeightBuffer,
    pub attention_v: WeightBuffer,
    pub attention_out: WeightBuffer,
    pub ffn_hidden: WeightBuffer,
    pub ffn_out: WeightBuffer,
}

impl ModelWeights {
    pub fn new(vocab_size: usize, hidden_dim: usize) -> Self {
        let embedding_data = alloc::vec![0.0f32; vocab_size * hidden_dim];
        let qkv_data = alloc::vec![0.0f32; hidden_dim * hidden_dim];
        let attention_out_data = alloc::vec![0.0f32; hidden_dim * hidden_dim];
        let ffn_hidden_data = alloc::vec![0.0f32; hidden_dim * (hidden_dim * 4)];
        let ffn_out_data = alloc::vec![0.0f32; (hidden_dim * 4) * hidden_dim];

        let mut emb = WeightBuffer::new(vocab_size, hidden_dim);
        emb.data = embedding_data;

        let mut aq = WeightBuffer::new(hidden_dim, hidden_dim);
        aq.data = qkv_data.clone();

        let mut ak = WeightBuffer::new(hidden_dim, hidden_dim);
        ak.data = qkv_data.clone();

        let mut av = WeightBuffer::new(hidden_dim, hidden_dim);
        av.data = qkv_data;

        let mut attout = WeightBuffer::new(hidden_dim, hidden_dim);
        attout.data = attention_out_data;

        let mut ffnh = WeightBuffer::new(hidden_dim, hidden_dim * 4);
        ffnh.data = ffn_hidden_data;

        let mut ffno = WeightBuffer::new(hidden_dim * 4, hidden_dim);
        ffno.data = ffn_out_data;

        ModelWeights {
            embedding: emb,
            attention_q: aq,
            attention_k: ak,
            attention_v: av,
            attention_out: attout,
            ffn_hidden: ffnh,
            ffn_out: ffno,
        }
    }

    pub fn init_random_seed(&mut self, seed: u32) {
        let mut rng = SimpleRng::new(seed);

        for val in self.embedding.as_mut_slice() {
            *val = (rng.next() as f32 - 0.5) * 0.1;
        }

        for val in self.attention_q.as_mut_slice() {
            *val = (rng.next() as f32 - 0.5) * 0.1;
        }

        for val in self.attention_k.as_mut_slice() {
            *val = (rng.next() as f32 - 0.5) * 0.1;
        }

        for val in self.attention_v.as_mut_slice() {
            *val = (rng.next() as f32 - 0.5) * 0.1;
        }

        for val in self.attention_out.as_mut_slice() {
            *val = (rng.next() as f32 - 0.5) * 0.1;
        }

        for val in self.ffn_hidden.as_mut_slice() {
            *val = (rng.next() as f32 - 0.5) * 0.1;
        }

        for val in self.ffn_out.as_mut_slice() {
            *val = (rng.next() as f32 - 0.5) * 0.1;
        }
    }
}

pub struct SimpleRng {
    state: u32,
}

impl SimpleRng {
    pub fn new(seed: u32) -> Self {
        SimpleRng {
            state: if seed == 0 { 1 } else { seed },
        }
    }

    pub fn next(&mut self) -> f32 {
        self.state = self.state.wrapping_mul(1664525).wrapping_add(1013904223);
        ((self.state >> 16) & 0x7fff) as f32 / 32768.0
    }
}

pub fn load_from_memory(_ptr: *const u8) -> ModelWeights {
    let weights = ModelWeights::new(256, 64);
    weights
}
