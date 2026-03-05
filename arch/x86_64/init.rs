/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Early CPU/board init: SSE, Limine check, GDT, IDT, PIC, serial, framebuffer.
*/
use core::arch::asm;
use super::boot;
use super::gdt_simple;
use super::idt;
use super::pic;
use super::serial;
use super::framebuffer;

/// Disable CPU interrupts.
pub fn disable_interrupts() {
    unsafe { asm!("cli"); }
}

/// Enable CPU interrupts.
pub fn enable_interrupts() {
    unsafe { asm!("sti"); }
}

/// Enable SSE/SSE2 (CR4.OSFXSR, OSXMMEXCPT; CR0.EM clear, MP set).
pub fn enable_sse() {
    unsafe {
        asm!(
            "mov rax, cr4",
            "or rax, 0x660",
            "mov cr4, rax",
            "mov rax, cr0",
            "and rax, 0xFFFFFFFFFFFFFFFB",
            "or rax, 0x2",
            "mov cr0, rax",
            options(nostack, preserves_flags)
        );
    }
}

/// Enable AVX/AVX2: set CR4.OSXSAVE and configure XCR0 so that YMM registers
/// are saved/restored by the hardware.  Without this, any VEX-encoded AVX
/// instruction triggers #UD even when CPUID.7:EBX[5] reports AVX2 support.
///
/// Safe to call on pre-AVX CPUs — checks CPUID before touching any new bits.
pub fn enable_avx() {
    use core::arch::x86_64::__cpuid;
    unsafe {
        // CPUID leaf 1: ECX[26]=XSAVE, ECX[28]=AVX
        let leaf1 = __cpuid(1);
        if leaf1.ecx & (1 << 26) == 0 {
            serial::write_str("[ARCH] XSAVE not supported, skipping AVX enable\r\n");
            return;
        }

        // Set CR4.OSXSAVE (bit 18): tells CPU the OS will save XSTATE
        asm!(
            "mov rax, cr4",
            "or rax, 0x40000",
            "mov cr4, rax",
            out("rax") _,
            options(nostack, preserves_flags, nomem)
        );

        if leaf1.ecx & (1 << 28) == 0 {
            serial::write_str("[ARCH] CR4.OSXSAVE set; AVX not present, XCR0 left as-is\r\n");
            return;
        }

        // Write XCR0: enable x87 (bit 0) + SSE/XMM (bit 1) + AVX/YMM (bit 2)
        asm!(
            "xor ecx, ecx",   // XCR0 selector = 0
            "xgetbv",          // EAX ← XCR0[31:0], EDX ← XCR0[63:32]
            "or eax, 7",       // set bits 0,1,2
            "xsetbv",
            out("eax") _,
            out("ecx") _,
            out("edx") _,
            options(nostack, nomem)
        );

        serial::write_str("[ARCH] AVX/YMM enabled (CR4.OSXSAVE + XCR0[2])\r\n");
    }
}

/// Full arch init: SSE, Limine check, GDT, IDT, PIC, serial, framebuffer.
pub fn init() {
    disable_interrupts();
    enable_sse();
    enable_avx();

    serial::init();
    serial::write_str("[AETERNA] Serial OK\r\n");

    if !boot::base_revision_supported() {
        serial::write_str("[AETERNA] WARN: Limine base revision not 0\r\n");
    }
    if let Some(off) = boot::hhdm_offset() {
        serial::write_str("[AETERNA] HHDM offset: 0x");
        serial_hex(off);
        serial::write_str("\r\n");
    }

    gdt_simple::init();
    serial::write_str("[AETERNA] GDT loaded\r\n");

    idt::init();
    serial::write_str("[AETERNA] IDT loaded\r\n");

    // Initialize PIC (remap IRQs to IDT 32-47)
    pic::init();

    // Program PIT channel 0 to fire at 100 Hz (preemptive scheduling fallback)
    pic::init_pit_100hz();

    // Calibrate TSC — must come after PIT is live (uses 10 tick = 100 ms window).
    // Enable interrupts first so that `hlt` in calibrate() actually receives PIT ticks.
    enable_interrupts();
    super::tsc::calibrate();

    // Init Local APIC one-shot timer (vector 48) — arms 1 ms precision preemption.
    // Requires TSC calibrated and interrupts enabled.
    super::apic::init_timer();

    // Initialize framebuffer if available
    if let Some(fb) = boot::framebuffer() {
        unsafe {
            framebuffer::init(
                fb.address as *mut u32,
                fb.width,
                fb.height,
                fb.pitch,
                fb.bpp,
                fb.red_mask_shift,
                fb.green_mask_shift,
                fb.blue_mask_shift,
            );
        }
        serial::write_str("[AETERNA] Framebuffer initialized\r\n");

        // Initialize fbconsole for text output
        super::fbconsole::init();

        // Clear screen and draw welcome text (minimalist - white only)
        framebuffer::clear(0x00000000);
        framebuffer::draw_string_at(10, 10, "AETERNA Microkernel", 0x00FFFFFF, 0x00000000);
        framebuffer::draw_string_at(10, 26, "===================", 0x00FFFFFF, 0x00000000);
        framebuffer::draw_string_at(10, 50, "Framebuffer: OK", 0x00FFFFFF, 0x00000000);
        framebuffer::draw_string_at(10, 66, "Serial: OK", 0x00FFFFFF, 0x00000000);
        framebuffer::draw_string_at(10, 82, "GDT: OK", 0x00FFFFFF, 0x00000000);
        framebuffer::draw_string_at(10, 98, "IDT: OK", 0x00FFFFFF, 0x00000000);
        framebuffer::draw_string_at(10, 114, "PIC: OK", 0x00FFFFFF, 0x00000000);
    } else {
        serial::write_str("[AETERNA] No framebuffer available\r\n");
    }

    // Enable hardware interrupts
    enable_interrupts();
    serial::write_str("[AETERNA] Interrupts enabled\r\n");
}

pub fn serial_hex(mut val: u64) {
    const HEX: [u8; 16] = *b"0123456789ABCDEF";
    let mut buf = [0u8; 16];
    for i in (0..16).rev() {
        buf[i] = HEX[(val & 0xF) as usize];
        val >>= 4;
    }
    for b in buf {
        serial::write_byte(b);
    }
}
