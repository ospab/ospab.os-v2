/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Kernel heap allocator for AETERNA microkernel.
Uses linked_list_allocator backed by physical memory from Limine.
Heap size: 128 MiB (per tech-manifest п.2).
*/

use linked_list_allocator::LockedHeap;

/// Maximum desired heap size (128 MiB). The allocator will try this first,
/// then fall back to smaller sizes so the kernel boots on low-memory and
/// UEFI systems where contiguous physical blocks are scarce.
pub const HEAP_SIZE_MAX: usize = 128 * 1024 * 1024;

/// Minimum acceptable heap size (4 MiB)
const HEAP_SIZE_MIN: usize = 4 * 1024 * 1024;

/// Actual heap size chosen at boot (set during init)
static mut HEAP_ACTUAL: usize = 0;

/// Kernel heap start address (virtual).
static mut HEAP_START: u64 = 0;
static mut HEAP_INITIALIZED: bool = false;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Get the actual heap size that was allocated
pub fn heap_size() -> usize {
    unsafe { HEAP_ACTUAL }
}

/// Initialize the kernel heap.
/// Must be called after physical memory manager is initialized.
/// Uses HHDM (Higher Half Direct Map) from Limine to access physical memory.
/// Tries 128 → 64 → 32 → 16 → 8 → 4 MiB, picks the largest that fits.
pub fn init() {
    let hhdm_offset = crate::arch::x86_64::boot::hhdm_offset().unwrap_or(0);
    let serial = crate::arch::x86_64::serial::write_str;

    // Try decreasing heap sizes until one succeeds
    let mut try_size = HEAP_SIZE_MAX;
    while try_size >= HEAP_SIZE_MIN {
        let frames = (try_size as u64) / crate::mm::physical::FRAME_SIZE;
        if let Some(phys) = crate::mm::physical::alloc_frames(frames) {
            let virt_base = phys + hhdm_offset;
            unsafe {
                HEAP_START = virt_base;
                HEAP_ACTUAL = try_size;
                ALLOCATOR.lock().init(virt_base as *mut u8, try_size);
                HEAP_INITIALIZED = true;
            }
            serial("[AETERNA] Heap initialized: ");
            // Print size in MiB
            let mib = try_size / (1024 * 1024);
            let mut buf = [0u8; 4];
            let s = fmt_u32(mib as u32, &mut buf);
            serial(s);
            serial(" MiB at 0x");
            crate::arch::x86_64::init::serial_hex(virt_base);
            serial("\r\n");
            return;
        }
        try_size /= 2;
    }

    serial("[AETERNA] FATAL: Cannot allocate heap (even 4 MiB)\r\n");
    serial("[AETERNA] Available memory insufficient\r\n");
}

/// Tiny u32→decimal helper (no alloc needed)
fn fmt_u32(mut v: u32, buf: &mut [u8; 4]) -> &str {
    let mut i = buf.len();
    if v == 0 {
        i -= 1;
        buf[i] = b'0';
    } else {
        while v > 0 && i > 0 {
            i -= 1;
            buf[i] = b'0' + (v % 10) as u8;
            v /= 10;
        }
    }
    core::str::from_utf8(&buf[i..]).unwrap_or("?")
}

/// Check if heap is initialized
pub fn is_initialized() -> bool {
    unsafe { HEAP_INITIALIZED }
}

/// Get heap usage statistics
pub fn stats() -> (usize, usize) {
    let allocator = ALLOCATOR.lock();
    let free = allocator.free();
    let total = heap_size();
    let used = if total > free { total - free } else { 0 };
    (used, free)
}
