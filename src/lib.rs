/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Distributed under the Boost Software License, Version 1.1.
See LICENSE or https://www.boost.org/LICENSE_1_0.txt for details.
*/
#![no_std]
// In a bare-metal kernel we intentionally access mutable statics directly;
// wrapping every site in a Mutex would require OS primitives we don't have yet.
#![allow(static_mut_refs)]

extern crate alloc;

#[path = "../arch/mod.rs"]
pub mod arch;

#[path = "../mm/mod.rs"]
pub mod mm;

// Re-export arch modules for easier access
pub use arch::x86_64::serial;

#[path = "../core/mod.rs"]
pub mod core;

// Kernel event log — used by panic handler and terminal
pub mod klog;

// Network stack (lives at repo root net/)
#[path = "../net/mod.rs"]
pub mod net;

// Storage drivers (ATA PIO, AHCI SATA — lives at repo root drivers/)
#[path = "../drivers/mod.rs"]
pub mod drivers;

// ACPI tables, PCI config, and XHCI USB
pub mod acpi;
pub mod pci;
pub mod xhci;

// Virtual filesystem layer + RamFS (lives at repo root vfs/)
#[path = "../vfs/mod.rs"]
pub mod fs;

// Userland tools (integrated as kernel modules until userspace is ready)
#[path = "../userland/grape/src/lib.rs"]
pub mod grape;

#[path = "../userland/tomato/src/lib.rs"]
pub mod tomato;

// Userspace modules (kernel-integrated, VFS-only access)
pub mod userspace;

// Re-export AXON coreutils
pub use userspace::axon;

// Re-export plum for backward compatibility
pub use userspace::plum;

#[path = "../userland/seed/src/lib.rs"]
pub mod seed;
// DOOM engine (bare-metal port)
pub mod doom;

// AETERNA AI Model (AAM) - minimal transformer + tokenizer
#[path = "../aam/mod.rs"]
pub mod aam;

// ─── ANE — Aeterna Neural Engine (no_std PyTorch replacement) ───────────────
#[path = "../lib/ane/mod.rs"]
pub mod ane;

// aai — Aeterna AI utility (model loader + chat + bench)
#[path = "../userland/aai/src/lib.rs"]
pub mod aai;

// ─── Tiny formatting helpers (no_std, no alloc) ─────────────────────────────

/// Format a u64 as decimal ASCII into `buf` (must be ≥ 8 bytes).
/// Returns the filled sub-slice as `&str`.
pub fn format_u64<'a>(buf: &'a mut [u8; 8], val: u64) -> &'a str {
    if val == 0 {
        buf[0] = b'0';
        return ::core::str::from_utf8(&buf[..1]).unwrap_or("0");
    }
    let mut n = val;
    let mut end = 0usize;
    while n > 0 && end < buf.len() {
        buf[end] = b'0' + (n % 10) as u8;
        n /= 10;
        end += 1;
    }
    buf[..end].reverse();
    ::core::str::from_utf8(&buf[..end]).unwrap_or("?")
}