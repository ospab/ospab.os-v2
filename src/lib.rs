/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Distributed under the Boost Software License, Version 1.1.
See LICENSE or https://www.boost.org/LICENSE_1_0.txt for details.
*/
#![no_std]

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

// Network stack
pub mod net;

// Storage drivers (ATA PIO, AHCI SATA)
pub mod drivers;

// Virtual filesystem layer + RamFS
pub mod fs;

// Userland tools (integrated as kernel modules until userspace is ready)
#[path = "../userland/grape/src/lib.rs"]
pub mod grape;

#[path = "../userland/tomato/src/lib.rs"]
pub mod tomato;

#[path = "../userland/plum/src/lib.rs"]
pub mod plum;

#[path = "../userland/seed/src/lib.rs"]
pub mod seed;
// DOOM engine (bare-metal port)
pub mod doom;