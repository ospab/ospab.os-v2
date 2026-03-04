/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Distributed under the Boost Software License, Version 1.1.
See LICENSE or https://www.boost.org/LICENSE_1_0.txt for details.

Capability-based security for AETERNA microkernel.
Processes request capability tokens instead of running as root.

Design:
  - No "superuser" concept. Every privileged operation requires a token.
  - Tokens are unforgeable typed descriptors (Rust type-brand pattern).
  - Token set for a process is declared statically in its source code
    and verified at spawn time by the kernel.
  - Granularity: per-syscall, per-path-prefix, per-device.

Current token types:
  CapFsRead    — read files under a given path prefix
  CapFsWrite   — write/create files under a given path prefix
  CapFramebuf  — write to the GOP framebuffer
  CapSerial    — emit bytes to the serial port (COM1)
  CapMemHuge   — request huge-page (2MiB) allocations via sys_mmap
  CapSpawn     — spawn child tasks
  CapNet       — send/receive network packets

Usage in a module:
  fn init() {
      let _tok = CapFsRead::grant("/models");   // panics if not in manifest
      cmd_load("/models/tiny.tmt-ai");
  }
*/

/// Opaque zero-sized brand token.  One instance proves the caller holds the
/// corresponding capability.  Cannot be constructed outside this module.
#[derive(Debug, Clone, Copy)]
pub struct CapabilityToken(());

// ─── Concrete capability types ───────────────────────────────────────────────

/// Read access to VFS paths under `prefix`.
pub struct CapFsRead;
/// Write / create access to VFS paths under `prefix`.
pub struct CapFsWrite;
/// Direct framebuffer write (GOP / VRAM).
pub struct CapFramebuf;
/// Serial port output (COM1 debug console).
pub struct CapSerial;
/// Huge-page memory mapping via SyscallNumber::Mmap.
pub struct CapMemHuge;
/// Ability to spawn new kernel tasks.
pub struct CapSpawn;
/// Network packet send/receive.
pub struct CapNet;

// ─── Grant helpers ───────────────────────────────────────────────────────────
// In the current single-process kernel, these always succeed.
// When the scheduler tracks per-task capability sets, these will check
// against the task's token manifest before returning Ok.

impl CapFsRead  { pub fn grant(_prefix: &str)   -> CapabilityToken { CapabilityToken(()) } }
impl CapFsWrite { pub fn grant(_prefix: &str)   -> CapabilityToken { CapabilityToken(()) } }
impl CapFramebuf { pub fn grant()                -> CapabilityToken { CapabilityToken(()) } }
impl CapSerial  { pub fn grant()                -> CapabilityToken { CapabilityToken(()) } }
impl CapMemHuge { pub fn grant()                -> CapabilityToken { CapabilityToken(()) } }
impl CapSpawn   { pub fn grant()                -> CapabilityToken { CapabilityToken(()) } }
impl CapNet     { pub fn grant()                -> CapabilityToken { CapabilityToken(()) } }

// ─── Required-capability manifest helper ─────────────────────────────────────

/// Declare the capability manifest for a module at compile time.
/// Call from module init / top of main; returns a struct holding all tokens.
///
/// Example:
/// ```rust
/// let caps = AaiCaps::acquire();
/// cmd_load("/models/foo.tmt-ai", &caps.fs_read);
/// ```
pub struct AaiCaps {
    pub fs_read:  CapabilityToken,   // read /models/*, /doom/*
    pub framebuf: CapabilityToken,   // draw inference output
    pub serial:   CapabilityToken,   // log to COM1
    pub mem_huge: CapabilityToken,   // mmap huge-page weight buffers
}

impl AaiCaps {
    /// Acquire all capabilities required by the aai utility.
    /// Kernel will deny unauthorised tasks at spawn time (future enforcement).
    pub fn acquire() -> Self {
        AaiCaps {
            fs_read:  CapFsRead::grant("/models"),
            framebuf: CapFramebuf::grant(),
            serial:   CapSerial::grant(),
            mem_huge: CapMemHuge::grant(),
        }
    }
}
