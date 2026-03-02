/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

userspace — Kernel-integrated userland modules.

These modules will eventually run in ring-3 userspace once memory
protection and syscall ABI are finalized.  Until then they execute
inside the kernel but access the outside world exclusively through
the VFS syscall layer (crate::fs::*).
*/

pub mod plum;
pub mod axon;
