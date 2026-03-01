/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Simple GDT implementation without external dependencies.
*/

use core::arch::asm;

#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct GdtEntry {
    limit_low: u16,
    base_low: u16,
    base_mid: u8,
    access: u8,
    flags_limit_high: u8,
    base_high: u8,
}

impl GdtEntry {
    pub const fn new() -> Self {
        Self {
            limit_low: 0,
            base_low: 0,
            base_mid: 0,
            access: 0,
            flags_limit_high: 0,
            base_high: 0,
        }
    }

    pub fn set_code_segment(&mut self) {
        self.access = 0x9A; // Present, Ring 0, Code, Executable, Accessed
        self.flags_limit_high = 0xA0; // Granularity 4KB, 32-bit, Limit high 0xF
    }

    pub fn set_data_segment(&mut self) {
        self.access = 0x92; // Present, Ring 0, Data, Writable, Accessed
        self.flags_limit_high = 0xC0; // Granularity 4KB, 32-bit, Limit high 0xF
    }

    pub fn set_tss_segment(&mut self) {
        self.access = 0x89; // Present, Ring 0, TSS, Available
        self.flags_limit_high = 0x00; // TSS special case
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct GdtPointer {
    pub limit: u16,
    pub base: u64,
}

pub const GDT_ENTRIES: usize = 5;
static mut GDT: [GdtEntry; GDT_ENTRIES] = [
    GdtEntry::new(), // Null
    GdtEntry::new(), // Code
    GdtEntry::new(), // Data
    GdtEntry::new(), // TSS (low)
    GdtEntry::new(), // TSS (high)
];

/// Kernel code segment selector (GDT index 1)
pub const KERNEL_CS: u16 = 0x08;
/// Kernel data segment selector (GDT index 2)
pub const KERNEL_DS: u16 = 0x10;

pub fn init() {
    unsafe {
        // Set up code segment (64-bit long mode)
        GDT[1].set_code_segment();
        GDT[1].limit_low = 0xFFFF;
        GDT[1].base_low = 0;
        GDT[1].base_mid = 0;
        GDT[1].base_high = 0;

        // Set up data segment
        GDT[2].set_data_segment();
        GDT[2].limit_low = 0xFFFF;
        GDT[2].base_low = 0;
        GDT[2].base_mid = 0;
        GDT[2].base_high = 0;

        let gdt_ptr = GdtPointer {
            limit: (core::mem::size_of::<[GdtEntry; GDT_ENTRIES]>() - 1) as u16,
            base: &GDT as *const _ as u64,
        };

        asm!("lgdt [{}]", in(reg) &gdt_ptr, options(readonly, nostack));

        // Reload CS via far return (push new CS + return address, then retfq)
        asm!(
            "push {cs}",        // push new CS selector
            "lea {tmp}, [rip + 2f]", // load address of label 2
            "push {tmp}",       // push return address
            "retfq",            // far return: pops RIP and CS
            "2:",               // landing label
            cs = in(reg) KERNEL_CS as u64,
            tmp = lateout(reg) _,
            options(preserves_flags),
        );

        // Reload all data segment registers with our data selector
        asm!(
            "mov ds, {0:x}",
            "mov es, {0:x}",
            "mov fs, {0:x}",
            "mov gs, {0:x}",
            "mov ss, {0:x}",
            in(reg) KERNEL_DS as u64,
            options(nostack, preserves_flags),
        );
    }
}
