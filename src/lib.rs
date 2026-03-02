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

// Network stack
pub mod net;

// Storage drivers (ATA PIO, AHCI SATA)
pub mod drivers;

// ACPI tables, PCI config, and XHCI USB
pub mod acpi;
pub mod pci;
pub mod xhci;

// Virtual filesystem layer + RamFS
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