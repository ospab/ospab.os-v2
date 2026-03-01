/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Kernel heap allocator for AETERNA microkernel.
Uses linked_list_allocator backed by physical memory from Limine.
Heap size: 128 MiB (per tech-manifest п.2).
*/

use linked_list_allocator::LockedHeap;

/// Heap size: 128 MiB (tech-manifest requirement)
pub const HEAP_SIZE: usize = 128 * 1024 * 1024;

/// Kernel heap start address (virtual).
/// We place the heap in a well-known virtual address range.
/// With Limine HHDM, physical memory is identity-mapped at a high offset.
/// We use physical memory directly via HHDM offset.
static mut HEAP_START: u64 = 0;
static mut HEAP_INITIALIZED: bool = false;

#[global_allocator]
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Initialize the kernel heap.
/// Must be called after physical memory manager is initialized.
/// Uses HHDM (Higher Half Direct Map) from Limine to access physical memory.
pub fn init() {
    let hhdm_offset = crate::arch::x86_64::boot::hhdm_offset().unwrap_or(0);

    // We need contiguous physical memory for the heap
    // 128 MiB = 32768 frames of 4 KiB each
    let heap_frames = (HEAP_SIZE as u64) / crate::mm::physical::FRAME_SIZE;
    let phys_base = crate::mm::physical::alloc_frames(heap_frames);

    match phys_base {
        Some(phys) => {
            // Convert physical address to virtual via HHDM
            let virt_base = phys + hhdm_offset;

            unsafe {
                HEAP_START = virt_base;
                ALLOCATOR.lock().init(virt_base as *mut u8, HEAP_SIZE);
                HEAP_INITIALIZED = true;
            }

            crate::arch::x86_64::serial::write_str("[AETERNA] Heap initialized: 128 MiB at 0x");
            crate::arch::x86_64::init::serial_hex(virt_base);
            crate::arch::x86_64::serial::write_str("\r\n");
        }
        None => {
            crate::arch::x86_64::serial::write_str("[AETERNA] FATAL: Cannot allocate heap (128 MiB)\r\n");
            crate::arch::x86_64::serial::write_str("[AETERNA] Available memory insufficient\r\n");
        }
    }
}

/// Check if heap is initialized
pub fn is_initialized() -> bool {
    unsafe { HEAP_INITIALIZED }
}

/// Get heap usage statistics
pub fn stats() -> (usize, usize) {
    let allocator = ALLOCATOR.lock();
    let free = allocator.free();
    let used = HEAP_SIZE - free;
    (used, free)
}
