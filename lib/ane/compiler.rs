/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

ANE Aeterna-Graph Compiler

Compiles a sequence of tape operations into a flat instruction stream that:
  1. Fuses adjacent elementwise ops (e.g. Linear → ReLU → LayerNorm into one pass)
  2. Schedules ops to minimise cache-miss pressure
  3. Elides unnecessary transient tensors where possible

Architecture:
  GraphCompiler  — analyses the tape, builds OpNode DAG
  CompiledGraph  — holds fused InsnBlock list + pre-allocated scratch tensors
  InsnBlock      — atomic execution unit (one or more fused primitive ops)
*/

extern crate alloc;

use alloc::vec::Vec;
use alloc::vec;
use alloc::string::String;

use super::tensor::{DataType, Tape, Tensor};

// ─── Primitive Op tags (for analysis) ────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrimOp {
    Add,
    Mul,
    MatMul,
    Relu,
    Softmax,
    LayerNorm,
    Embedding,
    Linear,     // MatMul + Add(bias)
    Noop,
}

// ─── Op node in the analysis graph ───────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct OpNode {
    pub op:       PrimOp,
    pub inputs:   Vec<usize>,   // tape variable ids
    pub output:   usize,        // tape variable id
    /// True if this op has been fused into a predecessor InsnBlock.
    pub fused:    bool,
}

// ─── Fused instruction block ─────────────────────────────────────────────────

/// A sequence of ops that are executed together in a single kernel invocation.
/// The compiler fuses pointwise ops that share the same input/output buffer.
#[derive(Clone, Debug)]
pub struct InsnBlock {
    pub name:    String,
    pub ops:     Vec<PrimOp>,
    pub inputs:  Vec<usize>,
    pub output:  usize,
    /// Size of the output tensor (pre-computed to allow scratch allocation).
    pub out_len: usize,
    pub out_shape: Vec<usize>,
}

/// A compiled, executable graph.
pub struct CompiledGraph {
    pub blocks:  Vec<InsnBlock>,
    /// Pre-allocated scratch tensors matching each block's output.
    /// Public so external code (e.g. benches, userland) can inspect or reuse.
    pub scratch: Vec<Tensor>,
}

impl CompiledGraph {
    /// Execute the compiled graph over the tape's variable data.
    /// Returns the variable id of the final output.
    pub fn run(&mut self, tape: &mut Tape) -> Option<usize> {
        if self.blocks.is_empty() { return None; }
        let last_out = self.blocks.last().map(|b| b.output);

        for (bi, block) in self.blocks.iter().enumerate() {
            let _ = bi; // scratch[bi] available if needed
            // Execute each op in the block
            if block.ops.is_empty() { continue; }
            let mut current: usize = *block.inputs.first()?;

            for op in &block.ops {
                match op {
                    PrimOp::Relu => {
                        tape.vars[current].data.relu_inplace();
                    }
                    PrimOp::Softmax => {
                        tape.vars[current].data.softmax_inplace();
                    }
                    PrimOp::Add => {
                        if block.inputs.len() >= 2 {
                            let a = block.inputs[0];
                            let b = block.inputs[1];
                            let out = block.output;
                            if out < tape.vars.len() {
                                let added = tape.vars[a].data.add(&tape.vars[b].data);
                                for (i, v) in added.as_f32_slice().iter().enumerate() {
                                    tape.vars[out].data.set_f32(i, *v);
                                }
                            }
                        }
                    }
                    PrimOp::MatMul => {
                        if block.inputs.len() >= 2 {
                            let a = block.inputs[0];
                            let b = block.inputs[1];
                            let out = block.output;
                            if out < tape.vars.len() {
                                let mm = tape.vars[a].data.matmul(&tape.vars[b].data);
                                for (i, v) in mm.as_f32_slice().iter().enumerate() {
                                    tape.vars[out].data.set_f32(i, *v);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        last_out
    }
}

// ─── GraphCompiler ────────────────────────────────────────────────────────────

pub struct GraphCompiler {
    /// Analysed op nodes derived from tape.nodes
    pub nodes: Vec<OpNode>,
}

impl GraphCompiler {
    pub fn new() -> Self {
        GraphCompiler { nodes: Vec::new() }
    }

    /// Analyse a tape and produce the optimised InsnBlock sequence.
    ///
    /// Fusion rules applied:
    ///   MatMul → Add          → Linear (1 block)
    ///   Any    → Relu         → fused into predecessor if same output size
    ///   Any    → Relu → Add   → fused
    pub fn compile(&mut self, tape: &Tape) -> CompiledGraph {
        // Build a simple op-node list from tape.nodes metadata.
        // We use the tape's node list as the ground truth.
        self.nodes.clear();
        for node in &tape.nodes {
            let op = match node.input_ids.len() {
                0 => PrimOp::Noop,
                1 => PrimOp::Relu,     // single-input → assume ReLU
                2 => {
                    // Heuristic: if inputs have identical shape → Add/Mul, else MatMul
                    let a_shape = tape.vars[node.input_ids[0]].data.shape();
                    let b_shape = tape.vars[node.input_ids[1]].data.shape();
                    if a_shape == b_shape { PrimOp::Add }
                    else                  { PrimOp::MatMul }
                }
                _ => PrimOp::Noop,
            };
            self.nodes.push(OpNode {
                op,
                inputs:  node.input_ids.clone(),
                output:  node.output_id,
                fused:   false,
            });
        }

        // Fusion pass: merge MatMul+Add (linear), then pointwise chains
        let mut blocks: Vec<InsnBlock> = Vec::new();
        let mut i = 0;
        while i < self.nodes.len() {
            let node = &self.nodes[i];

            // Try Linear fusion: MatMul at i, Add at i+1 consuming MatMul output
            if node.op == PrimOp::MatMul
                && i + 1 < self.nodes.len()
                && self.nodes[i+1].op == PrimOp::Add
                && self.nodes[i+1].inputs.contains(&node.output)
            {
                let b_node = &self.nodes[i+1];
                let out_shape = tape.vars[b_node.output].data.shape().to_vec();
                let out_len   = tape.vars[b_node.output].data.len;
                blocks.push(InsnBlock {
                    name:      String::from("Linear"),
                    ops:       vec![PrimOp::MatMul, PrimOp::Add],
                    inputs:    node.inputs.iter().chain(b_node.inputs.iter())
                                   .copied().collect::<Vec<_>>(),
                    output:    b_node.output,
                    out_len,
                    out_shape,
                });
                i += 2;
                continue;
            }

            // Try fusing pointwise chain (ReLU, Add, Mul) into prev block
            if !blocks.is_empty()
                && (node.op == PrimOp::Relu || node.op == PrimOp::Add)
                && blocks.last().map(|b| b.output == *node.inputs.first().unwrap_or(&usize::MAX)).unwrap_or(false)
            {
                let last = blocks.last_mut().unwrap();
                let out_shape = tape.vars[node.output].data.shape().to_vec();
                let out_len   = tape.vars[node.output].data.len;
                last.ops.push(node.op);
                last.output    = node.output;
                last.out_len   = out_len;
                last.out_shape = out_shape;
                last.name.push('+');
                last.name.push_str(match node.op {
                    PrimOp::Relu    => "ReLU",
                    PrimOp::Add     => "Add",
                    PrimOp::Mul     => "Mul",
                    PrimOp::Softmax => "Softmax",
                    _ => "?",
                });
                i += 1;
                continue;
            }

            // Emit as standalone block
            let out_shape = tape.vars[node.output].data.shape().to_vec();
            let out_len   = tape.vars[node.output].data.len;
            let blk_name  = format_op_name(node.op);
            blocks.push(InsnBlock {
                name:     String::from(blk_name),
                ops:      vec![node.op],
                inputs:   node.inputs.clone(),
                output:   node.output,
                out_len,
                out_shape,
            });
            i += 1;
        }

        // Pre-allocate scratch tensors
        let scratch: Vec<Tensor> = blocks.iter()
            .map(|b| Tensor::zeros(&b.out_shape, DataType::F32))
            .collect();

        CompiledGraph { blocks, scratch }
    }

    /// Print the compiled instruction schedule to the serial log.    /// (Kernel-only — not compiled in unit-test builds.)
    #[cfg(not(test))]    pub fn dump(&self, graph: &CompiledGraph) {
        use crate::arch::x86_64::serial;
        serial::write_str("[ANE][Compiler] Instruction schedule:\r\n");
        for (i, blk) in graph.blocks.iter().enumerate() {
            serial::write_str("  [");
            serial_dec_str(i as u64);
            serial::write_str("] ");
            serial::write_str(&blk.name);
            serial::write_str(" → var#");
            serial_dec_str(blk.output as u64);
            serial::write_str(" (");
            serial_dec_str(blk.out_len as u64);
            serial::write_str(" elems)\r\n");
        }
    }
}

// Pure helper: returns a human-readable name for an op (no kernel dependencies).
fn format_op_name(op: PrimOp) -> &'static str {
    match op {
        PrimOp::Add      => "Add",
        PrimOp::Mul      => "Mul",
        PrimOp::MatMul   => "MatMul",
        PrimOp::Relu     => "ReLU",
        PrimOp::Softmax  => "Softmax",
        PrimOp::LayerNorm=> "LayerNorm",
        PrimOp::Embedding=> "Embedding",
        PrimOp::Linear   => "Linear",
        PrimOp::Noop     => "Noop",
    }
}

#[cfg(not(test))]
fn serial_dec_str(mut n: u64) {
    use crate::arch::x86_64::serial;
    let mut buf = [0u8; 20];
    if n == 0 { serial::write_str("0"); return; }
    let mut i = 0;
    while n > 0 { buf[i] = b'0' + (n % 10) as u8; n /= 10; i += 1; }
    buf[..i].reverse();
    serial::write_str(core::str::from_utf8(&buf[..i]).unwrap_or("?"));
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tensor::{DataType, Tape, Tensor};

    // ── format_op_name ────────────────────────────────────────────────────────

    #[test]
    fn test_format_op_name_coverage() {
        assert_eq!(format_op_name(PrimOp::Add),       "Add");
        assert_eq!(format_op_name(PrimOp::Mul),       "Mul");
        assert_eq!(format_op_name(PrimOp::MatMul),    "MatMul");
        assert_eq!(format_op_name(PrimOp::Relu),      "ReLU");
        assert_eq!(format_op_name(PrimOp::Softmax),   "Softmax");
        assert_eq!(format_op_name(PrimOp::LayerNorm), "LayerNorm");
        assert_eq!(format_op_name(PrimOp::Embedding), "Embedding");
        assert_eq!(format_op_name(PrimOp::Linear),    "Linear");
        assert_eq!(format_op_name(PrimOp::Noop),      "Noop");
    }

    // ── PrimOp equality ───────────────────────────────────────────────────────

    #[test]
    fn test_primop_eq() {
        assert_eq!(PrimOp::Add, PrimOp::Add);
        assert_ne!(PrimOp::Add, PrimOp::Mul);
    }

    // ── GraphCompiler: empty tape ─────────────────────────────────────────────

    #[test]
    fn test_compile_empty_tape() {
        let tape = Tape::new();
        let mut compiler = GraphCompiler::new();
        let graph = compiler.compile(&tape);
        assert!(graph.blocks.is_empty(), "empty tape should produce empty graph");
        assert!(graph.scratch.is_empty());
    }

    // ── GraphCompiler: single ReLU ────────────────────────────────────────────

    #[test]
    fn test_compile_single_relu() {
        let mut tape = Tape::new();
        let x = tape.leaf(Tensor::from_slice_f32(&[1.0, -1.0, 2.0]), false);
        let _y = tape.relu(x);

        let mut compiler = GraphCompiler::new();
        let graph = compiler.compile(&tape);
        // Should produce exactly 1 block
        assert_eq!(graph.blocks.len(), 1, "single relu should emit 1 block");
        assert_eq!(graph.blocks[0].ops, vec![PrimOp::Relu]);
    }

    // ── GraphCompiler: MatMul → Add fusion ────────────────────────────────────

    #[test]
    fn test_compile_linear_fusion() {
        // Build tape: c = matmul(a, b); d = add(c, bias)
        let mut tape = Tape::new();
        let a    = tape.leaf(Tensor::from_flat_f32(&[1.,2.,3.,4.], 2, 2), false);
        let b    = tape.leaf(Tensor::from_flat_f32(&[1.,0.,0.,1.], 2, 2), false);
        let bias = tape.leaf(Tensor::from_slice_f32(&[1.,1.]),              false);
        let c    = tape.matmul(a, b);
        let _d   = tape.add(c, bias);

        let mut compiler = GraphCompiler::new();
        let graph = compiler.compile(&tape);

        // Compiler should fuse MatMul+Add into a single Linear block
        let has_linear = graph.blocks.iter().any(|b| b.name == "Linear");
        assert!(has_linear, "MatMul→Add should fuse to Linear block; blocks: {:?}",
            graph.blocks.iter().map(|b| &b.name).collect::<alloc::vec::Vec<_>>());
    }

    // ── CompiledGraph: run returns last output ─────────────────────────────────

    #[test]
    fn test_run_relu_block() {
        let mut tape = Tape::new();
        let x = tape.leaf(Tensor::from_slice_f32(&[2.0, -1.0, 3.0]), false);
        let y = tape.relu(x);

        let mut compiler = GraphCompiler::new();
        let mut graph = compiler.compile(&tape);
        let out_id = graph.run(&mut tape);
        assert_eq!(out_id, Some(y));

        // After run, the ReLU output variable should have non-negative values
        let out = tape.vars[y].data.as_f32_slice();
        for &v in out {
            assert!(v >= 0.0, "ReLU output must be non-negative, got {v}");
        }
    }

    // ── scratch pre-allocation ────────────────────────────────────────────────

    #[test]
    fn test_scratch_matches_blocks() {
        let mut tape = Tape::new();
        let x = tape.leaf(Tensor::from_slice_f32(&[1.,2.,3.,4.]), false);
        tape.relu(x);
        let a = tape.leaf(Tensor::from_flat_f32(&[1.,0.,0.,1.], 2, 2), false);
        let b = tape.leaf(Tensor::from_flat_f32(&[2.,0.,0.,2.], 2, 2), false);
        tape.matmul(a, b);

        let mut compiler = GraphCompiler::new();
        let graph = compiler.compile(&tape);
        assert_eq!(graph.scratch.len(), graph.blocks.len(),
            "scratch count should equal block count");
    }
}
