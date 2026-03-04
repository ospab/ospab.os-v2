pub mod tokenizer;
pub mod weights;
pub mod inference;

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use self::tokenizer::AeternaTokenizer;
use self::inference::TinyModel;

// ─── Data Paths for RAG ──────────────────────────────────────────────────────

/// Root directory for AAM data (docs and training materials)
pub const DATA_ROOT: &str = "/aam/data";

/// Documentation directory for context loading (manifesto, spec, etc.)
pub const DATA_DOCS: &str = "/aam/data/docs";

/// Training data directory (dataset.jsonl, fine-tuning examples)
pub const DATA_TRAINING: &str = "/aam/data/training";

/// Manifesto file path (philosophy and architecture)
pub const MANIFESTO_PATH: &str = "/aam/data/docs/manifesto.txt";

/// Technical specification file path
pub const SPEC_PATH: &str = "/aam/data/docs/spec.md";

/// Training dataset path (JSON Lines format)
pub const DATASET_PATH: &str = "/aam/data/training/dataset.jsonl";

// ────────────────────────────────────────────────────────────────────────────

pub struct AeternaAiModel {
    tokenizer: AeternaTokenizer,
    model: TinyModel,
}

impl AeternaAiModel {
    pub fn new(vocab_size: usize, hidden_dim: usize) -> Self {
        let tokenizer = AeternaTokenizer::new();
        let mut model = TinyModel::new(vocab_size, hidden_dim);
        model.init_random(42);

        AeternaAiModel { tokenizer, model }
    }

    pub fn generate_step(&self, prompt: &str) -> String {
        let tokens = self.tokenizer.encode(prompt);

        let last_token = if !tokens.is_empty() {
            tokens[tokens.len() - 1]
        } else {
            32u32
        };

        let logits = self.model.forward(last_token);

        let next_token = self.model.sample_top_k(&logits, 5);

        let next_char = if next_token < 256 {
            core::char::from_u32(next_token).unwrap_or('?')
        } else {
            '?'
        };

        let mut result = String::new();
        result.push(next_char);
        result
    }

    pub fn generate_sequence(&self, prompt: &str, length: usize) -> String {
        let mut result = String::new();
        let mut current = String::from(prompt);

        for _ in 0..length {
            let step_result = self.generate_step(&current);
            result.push_str(&step_result);
            current.push_str(&step_result);
        }

        result
    }

    pub fn encode(&self, input: &str) -> Vec<u32> {
        self.tokenizer.encode(input)
    }

    pub fn decode(&self, tokens: &[u32]) -> String {
        self.tokenizer.decode(tokens)
    }

    /// Load context from documentation files for Retrieval-Augmented Generation (RAG)
    /// Returns raw text from manifesto.txt and spec.md as a concatenated string
    pub fn load_rag_context() -> String {
        let mut context = String::new();

        // Try to load manifesto
        if let Some(manifesto_bytes) = crate::fs::read_file(MANIFESTO_PATH) {
            if let Ok(text) = core::str::from_utf8(&manifesto_bytes) {
                context.push_str(text);
                context.push_str("\n\n");
            }
        }

        // Try to load spec
        if let Some(spec_bytes) = crate::fs::read_file(SPEC_PATH) {
            if let Ok(text) = core::str::from_utf8(&spec_bytes) {
                context.push_str(text);
                context.push_str("\n\n");
            }
        }

        context
    }

    /// Load training dataset (JSON Lines) for potential fine-tuning
    /// Returns raw JSONL content
    pub fn load_training_set() -> String {
        let mut dataset = String::new();

        if let Some(data_bytes) = crate::fs::read_file(DATASET_PATH) {
            if let Ok(text) = core::str::from_utf8(&data_bytes) {
                dataset.push_str(text);
            }
        }

        dataset
    }

    /// Get the data root directory for utilities
    pub fn data_root() -> &'static str {
        DATA_ROOT
    }

    /// Get the docs directory for utilities
    pub fn data_docs() -> &'static str {
        DATA_DOCS
    }

    /// Get the training directory for utilities
    pub fn data_training() -> &'static str {
        DATA_TRAINING
    }
}

pub fn create_model() -> AeternaAiModel {
    AeternaAiModel::new(256, 64)
}

pub fn generate_step(prompt: &str) -> String {
    let model = create_model();
    model.generate_step(prompt)
}

pub fn generate_text(prompt: &str, length: usize) -> String {
    let model = create_model();
    model.generate_sequence(prompt, length)
}

// ─── RAG Integration Helpers ─────────────────────────────────────────────────

/// Get the full path to a documentation file in /aam/data/docs/
pub fn doc_path(filename: &str) -> String {
    alloc::format!("{}/{}", DATA_DOCS, filename)
}

/// Get the full path to a training file in /aam/data/training/
pub fn training_path(filename: &str) -> String {
    alloc::format!("{}/{}", DATA_TRAINING, filename)
}

/// List all documentation files available for RAG
pub fn list_doc_files() -> Vec<String> {
    let mut files = Vec::new();
    if let Some(entries) = crate::fs::readdir(DATA_DOCS) {
        for entry in entries {
            files.push(entry.name);
        }
    }
    files
}

/// List all training files available
pub fn list_training_files() -> Vec<String> {
    let mut files = Vec::new();
    if let Some(entries) = crate::fs::readdir(DATA_TRAINING) {
        for entry in entries {
            files.push(entry.name);
        }
    }
    files
}

/// Load and parse a single documentation file for context
pub fn load_doc_file(filename: &str) -> String {
    let path = doc_path(filename);
    if let Some(bytes) = crate::fs::read_file(&path) {
        if let Ok(text) = core::str::from_utf8(&bytes) {
            return String::from(text);
        }
    }
    String::new()
}

/// Chunk a document into fixed-size overlapping segments for retrieval
/// Each chunk is approximately max_tokens in size
pub fn chunk_document(doc: &str, max_tokens: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let chunk_bytes = max_tokens * 4; // Rough estimate: 1 token ≈ 4 bytes
    let mut start = 0;

    while start < doc.len() {
        let end = (start + chunk_bytes).min(doc.len());

        // Try to break at newline or space
        let mut break_point = end;
        for i in (start..end).rev() {
            if i < doc.len() && (doc.as_bytes()[i] == b'\n' || doc.as_bytes()[i] == b' ') {
                break_point = i;
                break;
            }
        }

        if break_point <= start {
            break_point = end;
        }

        if let Some(chunk_text) = doc.get(start..break_point) {
            chunks.push(String::from(chunk_text));
        }

        start = break_point.saturating_add(1);
    }

    chunks
}

