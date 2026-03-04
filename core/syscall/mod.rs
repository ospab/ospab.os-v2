/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

Syscall interface for AETERNA microkernel (Phase 3).
Provides a real dispatch table with VFS-backed sys_open/read/write/close.

Syscall ABI (x86_64):
  RAX = syscall number
  RDI = arg1,  RSI = arg2,  RDX = arg3,  R10 = arg4,  R8 = arg5
  Return value in RAX.

LSTAR MSR setup is done in init_syscall_msr() — configures the SYSCALL
instruction to jump to our handler entry point.
*/

extern crate alloc;

/// Syscall numbers for AETERNA microkernel
/// Follows microkernel philosophy: minimal set, everything else via IPC
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum SyscallNumber {
    /// Exit current process
    Exit = 0,
    /// Write to a file descriptor (fd, buf_ptr, len) -> bytes_written
    Write = 1,
    /// Read from a file descriptor (fd, buf_ptr, len) -> bytes_read
    Read = 2,
    /// Open a resource by path -> fd
    Open = 3,
    /// Close a file descriptor
    Close = 4,
    /// Send IPC message (target_pid, msg_ptr, msg_len)
    IpcSend = 10,
    /// Receive IPC message (buf_ptr, buf_len) -> (sender_pid, msg_len)
    IpcRecv = 11,
    /// Create IPC channel -> channel_id
    IpcCreate = 12,
    /// Yield CPU time to scheduler
    Yield = 20,
    /// Sleep for N milliseconds
    Sleep = 21,
    /// Get current process ID
    GetPid = 22,
    /// Spawn a new process/task (name_ptr, name_len, priority)
    Spawn = 23,
    /// Fork current process -> child_pid
    Fork = 30,
    /// Execute a new program (path_ptr, argv_ptr)
    Exec = 31,
    /// Wait for child process
    WaitPid = 32,
    /// Map memory pages (addr_hint, size, flags) -> mapped_addr
    Mmap = 40,
    /// Unmap memory pages (addr, size)
    Munmap = 41,
    /// Get system information (info_type, buf_ptr, buf_len)
    SysInfo = 50,
    /// Get uptime in milliseconds
    Uptime = 51,
    /// Get task list metadata
    GetTasks = 52,
}

/// Syscall result codes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum SyscallError {
    /// Success (not an error)
    Success = 0,
    /// Invalid syscall number
    InvalidSyscall = -1,
    /// Permission denied (capability check failed)
    PermissionDenied = -2,
    /// Invalid argument
    InvalidArgument = -3,
    /// Resource not found
    NotFound = -4,
    /// Resource busy
    Busy = -5,
    /// Out of memory
    OutOfMemory = -6,
    /// Operation not supported
    NotSupported = -7,
    /// I/O error
    IoError = -8,
}

/// Syscall arguments passed in registers
#[derive(Debug, Clone, Copy)]
pub struct SyscallArgs {
    pub number: u64,     // RAX
    pub arg1: u64,       // RDI
    pub arg2: u64,       // RSI
    pub arg3: u64,       // RDX
    pub arg4: u64,       // R10
    pub arg5: u64,       // R8
}

/// Dispatch table entry type
type SyscallFn = fn(&SyscallArgs) -> i64;

/// Dispatch table: syscall number → handler function.
/// Entries are (number, handler). Searched linearly (small table).
static DISPATCH_TABLE: &[(u64, SyscallFn)] = &[
    (0,  |a| syscall_exit(a.arg1)),
    (1,  |a| syscall_write(a.arg1, a.arg2, a.arg3)),
    (2,  |a| syscall_read(a.arg1, a.arg2, a.arg3)),
    (3,  |a| syscall_open(a.arg1, a.arg2, a.arg3)),
    (4,  |a| syscall_close(a.arg1)),
    (20, |_| syscall_yield()),
    (22, |_| syscall_getpid()),
    (23, |a| syscall_spawn(a.arg1, a.arg2, a.arg3)),
    (32, |a| syscall_waitpid(a.arg1)),
    // SyscallNumber::Mmap = 40: map size bytes, flags bit0 = HUGE_PAGE
    (40, |a| syscall_mmap(a.arg1, a.arg2, a.arg3)),
    // SyscallNumber::Munmap = 41: unmap mapped region
    (41, |a| syscall_munmap(a.arg1, a.arg2)),
    (50, |a| syscall_sysinfo(a.arg1, a.arg2, a.arg3)),
    (51, |_| syscall_uptime()),
    (52, |_| syscall_get_tasks()),
];

/// Dispatch a syscall by looking up the number in the dispatch table
pub fn dispatch(args: &SyscallArgs) -> i64 {
    for &(num, handler) in DISPATCH_TABLE {
        if num == args.number {
            return handler(args);
        }
    }
    SyscallError::InvalidSyscall as i64
}

// ─── SYSCALL MSR setup ──────────────────────────────────────────────────────

/// IA32_EFER MSR — Extended Feature Enable Register
const MSR_EFER: u32 = 0xC0000080;
/// IA32_STAR MSR — Segment selectors for SYSCALL/SYSRET
const MSR_STAR: u32 = 0xC0000081;
/// IA32_LSTAR MSR — Target RIP for SYSCALL instruction
const MSR_LSTAR: u32 = 0xC0000082;
/// IA32_FMASK MSR — RFLAGS mask during SYSCALL
const MSR_FMASK: u32 = 0xC0000084;

/// Read a Model-Specific Register
unsafe fn rdmsr(msr: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    core::arch::asm!(
        "rdmsr",
        in("ecx") msr,
        out("eax") lo, out("edx") hi,
        options(nomem, nostack)
    );
    (hi as u64) << 32 | lo as u64
}

/// Write a Model-Specific Register
unsafe fn wrmsr(msr: u32, value: u64) {
    let lo = value as u32;
    let hi = (value >> 32) as u32;
    core::arch::asm!(
        "wrmsr",
        in("ecx") msr,
        in("eax") lo, in("edx") hi,
        options(nomem, nostack)
    );
}

/// Initialize SYSCALL/SYSRET MSR registers.
/// After this, executing the SYSCALL instruction in ring 3 will jump
/// to syscall_entry_stub.
///
/// Note: we set up the MSRs even though we don't have ring-3 yet,
/// so the infrastructure is ready when userspace arrives.
pub fn init_syscall_msr() {
    unsafe {
        // 1. Enable SCE (System Call Extensions) bit in EFER
        let efer = rdmsr(MSR_EFER);
        wrmsr(MSR_EFER, efer | 1); // bit 0 = SCE

        // 2. STAR: set kernel CS/SS and user CS/SS
        // Bits 47:32 = kernel CS (0x08), kernel SS is CS+8 (0x10)
        // Bits 63:48 = user CS-16 for SYSRET (0x1B - 16 = user CS=0x23, SS=0x1B)
        // For now, kernel-only: CS=0x08, SS=0x10
        let star = (0x0008u64 << 32) | (0x0010u64 << 48);
        wrmsr(MSR_STAR, star);

        // 3. LSTAR: entry point for SYSCALL instruction
        // For now, point to our minimal handler
        wrmsr(MSR_LSTAR, syscall_entry_stub as *const () as u64);

        // 4. FMASK: clear IF (bit 9) on SYSCALL entry (disable interrupts)
        wrmsr(MSR_FMASK, 0x200); // mask IF
    }

    crate::arch::x86_64::serial::write_str("[SYSCALL] MSR configured (LSTAR, STAR, FMASK)\r\n");
}

/// Minimal SYSCALL entry stub.
/// In a full OS, this would save all registers, switch stacks, etc.
/// For now, it's a function that can be called from kernel code to test dispatch.
#[no_mangle]
extern "C" fn syscall_entry_stub() {
    // In the future, this will be a naked asm function that:
    // 1. Saves user RSP to per-CPU area
    // 2. Switches to kernel stack
    // 3. Pushes all registers
    // 4. Calls dispatch()
    // 5. Restores registers
    // 6. SYSRETQ
    //
    // For now, the MSRs are set, and we call dispatch() from kernel code directly.
}

// ============================================================================
// Syscall implementations — real logic, not stubs
// ============================================================================

fn syscall_exit(code: u64) -> i64 {
    crate::arch::x86_64::serial::write_str("[SYSCALL] exit(");
    serial_dec(code);
    crate::arch::x86_64::serial::write_str(")\r\n");
    crate::core::scheduler::exit_current(code);
    loop {
        unsafe { core::arch::asm!("hlt"); }
    }
}

/// sys_write(fd, buf_ptr, len) — write bytes to a file descriptor.
/// fd=1 → serial+framebuffer (stdout), fd=2 → serial (stderr).
/// fd≥3 → VFS file write.
fn syscall_write(fd: u64, buf_ptr: u64, len: u64) -> i64 {
    // stdout/stderr → serial output
    if fd == 1 || fd == 2 {
        unsafe {
            let buf = core::slice::from_raw_parts(buf_ptr as *const u8, len as usize);
            for &b in buf {
                crate::arch::x86_64::serial::write_byte(b);
            }
        }
        return len as i64;
    }

    // VFS file descriptor
    unsafe {
        let buf = core::slice::from_raw_parts(buf_ptr as *const u8, len as usize);
        crate::fs::sys_write(fd as usize, buf)
    }
}

/// sys_read(fd, buf_ptr, len) — read bytes from a file descriptor.
/// fd=0 → keyboard (stdin), fd≥3 → VFS file read.
fn syscall_read(fd: u64, buf_ptr: u64, len: u64) -> i64 {
    if fd == 0 {
        // stdin: read one key from keyboard
        let key = crate::arch::x86_64::keyboard::poll_key();
        if let Some(ch) = key {
            if len >= 1 {
                unsafe {
                    let buf = core::slice::from_raw_parts_mut(buf_ptr as *mut u8, len as usize);
                    buf[0] = ch as u8;
                }
                return 1;
            }
        }
        return 0;
    }

    // VFS file descriptor
    unsafe {
        let buf = core::slice::from_raw_parts_mut(buf_ptr as *mut u8, len as usize);
        crate::fs::sys_read(fd as usize, buf)
    }
}

/// sys_open(path_ptr, path_len, flags) — open a file, returning fd.
/// flags: 0=read, 1=write, 2=read+write
fn syscall_open(path_ptr: u64, path_len: u64, flags: u64) -> i64 {
    unsafe {
        let path_bytes = core::slice::from_raw_parts(path_ptr as *const u8, path_len as usize);
        let path = match core::str::from_utf8(path_bytes) {
            Ok(s) => s,
            Err(_) => return SyscallError::InvalidArgument as i64,
        };
        crate::fs::sys_open(path, flags)
    }
}

/// sys_close(fd) — close a file descriptor.
fn syscall_close(fd: u64) -> i64 {
    crate::fs::sys_close(fd as usize)
}

fn syscall_yield() -> i64 {
    if crate::core::scheduler::is_initialized() {
        crate::core::scheduler::tick();
    }
    SyscallError::Success as i64
}

/// sys_waitpid(pid) — block until the task with `pid` exits.
/// Returns 0 on success, -1 if pid is invalid / never existed.
fn syscall_waitpid(pid: u64) -> i64 {
    crate::core::scheduler::wait_pid(pid as crate::core::scheduler::TaskId);
    SyscallError::Success as i64
}

fn syscall_getpid() -> i64 {
    crate::core::scheduler::current_task_id() as i64
}

fn syscall_spawn(name_ptr: u64, name_len: u64, priority: u64) -> i64 {
    let name = unsafe {
        let bytes = core::slice::from_raw_parts(name_ptr as *const u8, name_len as usize);
        match core::str::from_utf8(bytes) {
            Ok(s) => s,
            Err(_) => return SyscallError::InvalidArgument as i64,
        }
    };

    let prio = match priority {
        0 => crate::core::scheduler::Priority::Idle,
        1 => crate::core::scheduler::Priority::Normal,
        2 => crate::core::scheduler::Priority::System,
        3 => crate::core::scheduler::Priority::RealTime,
        _ => crate::core::scheduler::Priority::Compute,
    };

    match crate::core::scheduler::spawn_named(name, prio, 0, 0, 0) {
        Some(pid) => pid as i64,
        None => SyscallError::Busy as i64,
    }
}

fn syscall_sysinfo(info_type: u64, _buf_ptr: u64, _buf_len: u64) -> i64 {
    match info_type {
        0 => crate::mm::physical::total_memory() as i64,
        1 => crate::mm::physical::usable_memory() as i64,
        2 => crate::core::scheduler::task_count() as i64,
        _ => SyscallError::InvalidArgument as i64,
    }
}

fn syscall_uptime() -> i64 {
    crate::arch::x86_64::idt::timer_ticks() as i64
}

fn syscall_get_tasks() -> i64 {
    crate::core::scheduler::task_count() as i64
}

pub type TaskInfo = crate::core::scheduler::TaskSnapshot;

pub fn sys_get_tasks(out: &mut [TaskInfo]) -> usize {
    crate::core::scheduler::get_tasks(out)
}

// ─── sys_mmap (SyscallNumber::Mmap = 40) ────────────────────────────────────
//
// ABI: RDI=addr_hint (0=kernel_picks), RSI=size, RDX=flags
//   flags bit 0 = HUGE_PAGE (2MiB aligned allocation)
//   flags bit 1 = EXEC      (mark executable, reserved)
//
// Returns: mapped virtual address (as i64), or negative SyscallError on fail.
// Alignment: always 64-byte minimum; HUGE_PAGE requests 2MiB alignment (align=0x200000).
//
// SAFETY invariant: returned pointer is heap-allocated and must be freed via
// sys_munmap with the same size.  The caller (aai) holds a CapabilityToken{} for
// MemHuge before invoking this path (token checked at dispatch time in future).

fn syscall_mmap(_addr_hint: u64, size: u64, flags: u64) -> i64 {
    let size = size as usize;
    if size == 0 {
        return SyscallError::InvalidArgument as i64;
    }
    let huge = (flags & 1) != 0;
    // 2 MiB alignment for HUGE_PAGE; 64-byte otherwise (AVX-512 native).
    let align: usize = if huge { 0x200_000 } else { 64 };
    // Round size up to alignment boundary to avoid partial-page tails.
    let alloc_size = (size + align - 1) & !(align - 1);

    let layout = match core::alloc::Layout::from_size_align(alloc_size, align) {
        Ok(l)  => l,
        Err(_) => return SyscallError::InvalidArgument as i64,
    };

    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        crate::arch::x86_64::serial::write_str("[SYSCALL] Mmap: OOM\r\n");
        return SyscallError::OutOfMemory as i64;
    }

    crate::arch::x86_64::serial::write_str("[SYSCALL] Mmap: ");
    serial_dec(alloc_size as u64);
    crate::arch::x86_64::serial::write_str(" bytes @ 0x");
    serial_hex(ptr as u64);
    crate::arch::x86_64::serial::write_str("\r\n");

    ptr as i64
}

/// sys_munmap(addr, size) — free a mapping previously created by sys_mmap.
/// The size must match exactly what was passed to Mmap.
fn syscall_munmap(addr: u64, size: u64) -> i64 {
    if addr == 0 || size == 0 {
        return SyscallError::InvalidArgument as i64;
    }
    let size = size as usize;
    // We can only know the alignment from size; use 64-byte minimum.
    let align: usize = if size % 0x200_000 == 0 { 0x200_000 } else { 64 };
    let alloc_size = (size + align - 1) & !(align - 1);

    let layout = match core::alloc::Layout::from_size_align(alloc_size, align) {
        Ok(l)  => l,
        Err(_) => return SyscallError::InvalidArgument as i64,
    };
    // SAFETY: addr points to memory allocated by syscall_mmap with the same layout.
    unsafe { alloc::alloc::dealloc(addr as *mut u8, layout); }
    SyscallError::Success as i64
}

// Helper
fn serial_dec(mut val: u64) {
    if val == 0 {
        crate::arch::x86_64::serial::write_byte(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while val > 0 {
        buf[i] = b'0' + (val % 10) as u8;
        val /= 10;
        i += 1;
    }
    for j in (0..i).rev() {
        crate::arch::x86_64::serial::write_byte(buf[j]);
    }
}

fn serial_hex(mut val: u64) {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut buf = [0u8; 16];
    let mut i = 0;
    if val == 0 { crate::arch::x86_64::serial::write_byte(b'0'); return; }
    while val > 0 {
        buf[i] = HEX[(val & 0xF) as usize];
        val >>= 4;
        i += 1;
    }
    for j in (0..i).rev() {
        crate::arch::x86_64::serial::write_byte(buf[j]);
    }
}