extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

pub struct AeternaTokenizer {
    vocab_size: u32,
}

impl AeternaTokenizer {
    pub const fn new() -> Self {
        AeternaTokenizer { vocab_size: 256 }
    }

    pub fn encode(&self, input: &str) -> Vec<u32> {
        let mut tokens = Vec::new();
        for byte in input.as_bytes() {
            tokens.push(*byte as u32);
        }
        tokens
    }

    pub fn decode(&self, tokens: &[u32]) -> String {
        let mut result = String::new();
        for &token in tokens {
            if token < 256 {
                result.push(token as u8 as char);
            } else {
                result.push('?');
            }
        }
        result
    }

    pub const fn vocab_size(&self) -> u32 {
        self.vocab_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_decode() {
        let tokenizer = AeternaTokenizer::new();
        let text = "Hello";
        let tokens = tokenizer.encode(text);
        let decoded = tokenizer.decode(&tokens);
        assert_eq!(decoded, text);
    }

    #[test]
    fn test_ascii_range() {
        let tokenizer = AeternaTokenizer::new();
        let text = "ABC";
        let tokens = tokenizer.encode(text);
        assert_eq!(tokens, vec![65, 66, 67]);
    }
}
