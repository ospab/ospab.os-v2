#![no_std]

//! ospabOS v2 microkernel root

pub mod arch;
pub mod core;
pub mod drivers;
pub mod executive;
pub mod hpc;
pub mod mm;
pub mod net;
pub mod vfs;
pub mod api;
