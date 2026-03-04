/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

ANE — Aeterna Neural Engine
Native no_std replacement for PyTorch, optimised for server hardware.

Modules:
  tensor    — Core N-D tensor with SIMD GEMM, ReLU, Softmax, autograd tape
  layers    — Linear, LayerNorm, Attention, Embedding
  optimizers— AdamW, SGD (SIMD-vectorised weight update)
  compiler  — Aeterna-Graph Compiler (op-fusion, instruction scheduling)
*/

#![allow(clippy::needless_range_loop)]

extern crate alloc;

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

pub mod tensor;
pub mod layers;
pub mod optimizers;
pub mod compiler;

pub use tensor::{DataType, Tensor, Variable, Tape};
pub use layers::{Linear, LayerNorm, MultiHeadAttention, Embedding, Layer};
pub use optimizers::{AdamW, Sgd, Optimizer};
pub use compiler::{GraphCompiler, CompiledGraph};

// ─── Version ────────────────────────────────────────────────────────────────

pub const ANE_VERSION_MAJOR: u32 = 1;
pub const ANE_VERSION_MINOR: u32 = 0;
pub const ANE_VERSION_PATCH: u32 = 0;

/// Byte string embedded in every .tmt-ai model header.
pub const ANE_MAGIC: [u8; 4] = [b'T', b'M', b'T', 0x01];

// ─── Runtime feature detection ──────────────────────────────────────────────
// Uses AtomicBool so flags are safe to read from any future CPU core.
// Ordering::Relaxed is sufficient: the probe is idempotent and hardware-read-only.

/// 0 = not probed, non-zero = probed (uses AtomicU8 to represent a once-flag)
static CPU_PROBED:      AtomicBool = AtomicBool::new(false);
static CPU_HAS_AVX2:    AtomicBool = AtomicBool::new(false);
static CPU_HAS_AVX512F: AtomicBool = AtomicBool::new(false);

pub fn probe_cpu_features() {
    // Fast path — already probed.
    if CPU_PROBED.load(Ordering::Relaxed) { return; }

    #[cfg(target_arch = "x86_64")]
    {
        use core::arch::x86_64::__cpuid;
        // SAFETY: CPUID is always available on x86_64, reads no memory.
        let r = unsafe { __cpuid(7) };
        CPU_HAS_AVX2.store((r.ebx & (1 << 5))  != 0, Ordering::Relaxed);
        CPU_HAS_AVX512F.store((r.ebx & (1 << 16)) != 0, Ordering::Relaxed);
    }
    // Publish with Release so AVX2/512 stores are visible to all cores
    // before the PROBED flag becomes true.
    CPU_PROBED.store(true, Ordering::Release);
}

#[inline(always)]
pub fn has_avx2()    -> bool {
    probe_cpu_features();
    CPU_HAS_AVX2.load(Ordering::Relaxed)
}
#[inline(always)]
pub fn has_avx512f() -> bool {
    probe_cpu_features();
    CPU_HAS_AVX512F.load(Ordering::Relaxed)
}
