/*
 * Local APIC — x2APIC-compatible xAPIC driver
 *
 * Implements:
 *   • init_timer()    — detect APIC, enable it (spurious vector 0xFF),
 *                       calibrate divide-by-16 one-shot using TSC 10 ms shot,
 *                       then ARM the very first one-shot tick (1 ms).
 *   • one_shot_us(n)  — arm the APIC timer to fire in n µs (one-shot, vector 48)
 *   • send_eoi()      — write APIC EOI register; call from ISR
 *   • is_present()    — returns true if APIC hardware was found
 *
 * Register map (xAPIC MMIO, base 0xFEE00_000; all 32-bit reads/writes)
 * ──────────────────────────────────────────────────────────────────────
 *  Offset   Register
 *  0x020    APIC ID
 *  0x030    APIC Version
 *  0x0B0    EOI                  (write 0 to ACK interrupt)
 *  0x0F0    Spurious Int Vector  (bit 8 = SW enable; low byte = vector)
 *  0x320    LVT Timer            (bit 17 = periodic; bit 16 = mask; low byte = vector)
 *  0x380    Initial Count        (write → starts countdown; write 0 → stops)
 *  0x390    Current Count        (read)
 *  0x3E0    Divide Configuration (0b1011 = divide-by-1; 0b0011 = divide-by-16)
 *
 * One-shot mode
 * ─────────────
 * LVT Timer bit 17 = 0  (not periodic)
 * LVT Timer bit 16 = 0  (not masked)
 * Write Initial Count → hardware counts down at  bus_freq / divisor.
 * When it hits 0 → fires vector, stops.  Write again to re-arm.
 *
 * Safety note
 * ───────────
 * All MMIO accesses use `core::ptr::read_volatile` / `write_volatile`.
 * This module uses `unsafe` only for MMIO + MSR reads — documented inline.
 */

use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

// ─────────────────────────────────────────────────────────────────────────────
// APIC MMIO base (default; overridden if IA32_APIC_BASE MSR says otherwise)
const APIC_DEFAULT_BASE: u64 = 0xFEE0_0000;

// Register offsets in bytes
const REG_ID:      usize = 0x020;
const REG_VERSION: usize = 0x030;
const REG_EOI:     usize = 0x0B0;
const REG_SVR:     usize = 0x0F0;  // Spurious Vector Register
const REG_LVT_TMR: usize = 0x320;  // LVT Timer
const REG_INIT_CNT:usize = 0x380;
const REG_CUR_CNT: usize = 0x390;
const REG_DIV_CFG: usize = 0x3E0;

// Divide-by-16 → 0b0011 in bits [3:0]
const DIV_BY_16: u32 = 0b0011;

// IDT vector assigned to APIC timer one-shot mode
pub const APIC_TIMER_VECTOR: u8 = 48;

// ─────────────────────────────────────────────────────────────────────────────
// Module state
static APIC_PRESENT:  AtomicBool = AtomicBool::new(false);
static APIC_BASE_LO:  AtomicU32  = AtomicU32::new((APIC_DEFAULT_BASE & 0xFFFF_FFFF) as u32);
static APIC_BASE_HI:  AtomicU32  = AtomicU32::new((APIC_DEFAULT_BASE >> 32) as u32);
/// APIC timer counts per microsecond (bus_freq / divisor in counts/µs).
static APIC_CNT_PER_US: AtomicU32 = AtomicU32::new(0);

// ─────────────────────────────────────────────────────────────────────────────
// MMIO helpers

/// Returns the physical base address of the Local APIC (from IA32_APIC_BASE MSR).
fn apic_phys_base() -> u64 {
    let lo = APIC_BASE_LO.load(Ordering::Relaxed) as u64;
    let hi = APIC_BASE_HI.load(Ordering::Relaxed) as u64;
    (hi << 32) | lo
}

/// Returns the HHDM-mapped virtual address of the Local APIC MMIO page.
///
/// The APIC resides at a physical address (default 0xFEE00000) that is NOT
/// identity-mapped in AETERNA — only the HHDM window (HHDM_OFF + phys) is valid.
/// Using the raw physical address as a pointer causes an immediate #PF.
#[inline(always)]
fn apic_virt_base() -> u64 {
    let phys = apic_phys_base();
    // Limine maps all physical RAM (and MMIO) at `hhdm_offset + phys`.
    // boot::hhdm_offset() is set from the Limine HHDM response before arch::init().
    let hhdm = super::boot::hhdm_offset().unwrap_or(0xFFFF_8000_0000_0000);
    hhdm + phys
}

/// Read a 32-bit xAPIC register at byte-offset `off` from APIC MMIO base.
#[inline(always)]
unsafe fn reg_r(off: usize) -> u32 {
    // SAFETY: APIC MMIO — volatile read; address is valid via HHDM virtual mapping.
    core::ptr::read_volatile((apic_virt_base() + off as u64) as *const u32)
}

/// Write a 32-bit xAPIC register.
#[inline(always)]
unsafe fn reg_w(off: usize, val: u32) {
    // SAFETY: volatile write to APIC MMIO via HHDM virtual mapping.
    core::ptr::write_volatile((apic_virt_base() + off as u64) as *mut u32, val);
}

// ─────────────────────────────────────────────────────────────────────────────
// MSR helpers

/// Read IA32_APIC_BASE MSR (0x1B).
fn read_apic_base_msr() -> u64 {
    let lo: u32; let hi: u32;
    unsafe { asm!("rdmsr", in("ecx") 0x1Bu32, out("eax") lo, out("edx") hi, options(nomem, nostack)); }
    ((hi as u64) << 32) | lo as u64
}

// ─────────────────────────────────────────────────────────────────────────────
// Detect CPUID support for APIC

fn cpuid_apic_present() -> bool {
    // rbx is reserved by LLVM; push/pop it manually inside the asm block.
    let edx: u32;
    unsafe {
        asm!(
            "push rbx",
            "cpuid",
            "pop rbx",
            inout("eax") 1u32 => _,
            out("ecx") _,
            out("edx") edx,
            options(nomem, nostack)
        );
    }
    edx & (1 << 9) != 0   // APIC bit
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API

/// Returns `true` if APIC was successfully initialized.
pub fn is_present() -> bool { APIC_PRESENT.load(Ordering::Relaxed) }

/// Initialize the Local APIC and calibrate the one-shot timer.
///
/// Must be called **after**:
///   • PIC is initialized (pic::init)
///   • PIT is running at 100 Hz (pic::init_pit_100hz)
///   • TSC is calibrated (tsc::calibrate)
///   • Interrupts are enabled (sti)
///
/// After this call, one-shot timer fires on IDT vector 48.
/// PIT IRQ 0 is kept alive for scheduler preemption (100 Hz).
pub fn init_timer() {
    use crate::arch::x86_64::serial;

    // 1. Check CPUID: does this CPU have an APIC?
    if !cpuid_apic_present() {
        serial::write_str("[APIC] Not present (CPUID.EDX[9]=0) — skipping\r\n");
        return;
    }

    // 2. Read IA32_APIC_BASE MSR and extract physical base address (bits 51:12)
    let msr = read_apic_base_msr();
    let phys_base = msr & 0x000F_FFFF_FFFF_F000;

    serial::write_str("[APIC] Base MSR=0x");
    serial_hex64(msr);
    serial::write_str("  phys=0x");
    serial_hex64(phys_base);
    serial::write_str("\r\n");

    // Override default if firmware placed APIC elsewhere
    if phys_base != 0 && phys_base != APIC_DEFAULT_BASE {
        let base = phys_base;
        APIC_BASE_LO.store((base & 0xFFFF_FFFF) as u32, Ordering::Relaxed);
        APIC_BASE_HI.store((base >> 32) as u32, Ordering::Relaxed);
    }

    // 3. SW-enable APIC: set Spurious Vector Register (bit 8 = APIC SW enable)
    // Vector 0xFF is the spurious interrupt vector (never delivered for real work)
    unsafe {
        let svr = reg_r(REG_SVR);
        reg_w(REG_SVR, svr | 0x1FF);   // bit 8 = enable; 0xFF = spurious vector
    }

    // Verify we can read the version register sanity-check
    let version = unsafe { reg_r(REG_VERSION) } & 0xFF;
    if version == 0 || version == 0xFF {
        serial::write_str("[APIC] Version register sanity FAIL — APIC not mapped, skip\r\n");
        return;
    }
    serial::write_str("[APIC] Version=0x");
    serial_hex8(version as u8);
    serial::write_str("\r\n");

    // 4. Calibrate APIC timer against TSC (10 ms window)
    // Set divide-by-16, mask the LVT timer, write a max initial count, wait 10 ms,
    // read current count to get counts-per-10ms.
    let cnt_per_us = calibrate_apic_timer();
    if cnt_per_us == 0 {
        serial::write_str("[APIC] Timer calibration failed — counts_per_us=0\r\n");
        return;
    }
    APIC_CNT_PER_US.store(cnt_per_us, Ordering::Relaxed);

    serial::write_str("[APIC] Timer calibrated: ");
    serial_dec(cnt_per_us as u64);
    serial::write_str(" counts/µs\r\n");

    // 5. Set LVT Timer:
    //    • vector = APIC_TIMER_VECTOR (48)
    //    • mode   = one-shot (bit 17 = 0)
    //    • masked = 0 (unmasked)
    unsafe {
        reg_w(REG_DIV_CFG, DIV_BY_16);
        reg_w(REG_LVT_TMR, APIC_TIMER_VECTOR as u32);  // one-shot, unmasked
    }

    APIC_PRESENT.store(true, Ordering::Relaxed);
    serial::write_str("[APIC] One-shot timer armed on vector 48\r\n");

    // 6. Arm the first one-shot tick (1 ms from now) — so the scheduler gets
    //    its first APIC-driven wake even if there are no NIC packets.
    one_shot_us(1_000);
}

/// Calibrate APIC timer: returns counts per microsecond.
/// Uses TSC busy-wait for a precise 10 ms window.
fn calibrate_apic_timer() -> u32 {
    unsafe {
        // Mask timer during calibration
        reg_w(REG_LVT_TMR, (APIC_TIMER_VECTOR as u32) | (1 << 16)); // masked
        reg_w(REG_DIV_CFG, DIV_BY_16);
        reg_w(REG_INIT_CNT, 0xFFFF_FFFF); // start countdown from max
    }

    // Busy-wait 10 ms using TSC (safe here — we're in init, ints enabled)
    crate::arch::x86_64::tsc::busy_wait_us(10_000);

    let current = unsafe { reg_r(REG_CUR_CNT) };
    let elapsed_counts = 0xFFFF_FFFFu32.wrapping_sub(current);

    // Stop the timer
    unsafe { reg_w(REG_INIT_CNT, 0); }

    // counts_per_10ms / 10_000 = counts_per_µs
    let cnt_per_us = elapsed_counts / 10_000;
    cnt_per_us
}

/// Arm the APIC timer to fire once after `us` microseconds.
///
/// After the one-shot fires (IDT vector 48), the timer stops; call `one_shot_us` again
/// from the handler if periodic behaviour is desired.
///
/// # Safety
/// `send_eoi()` must be called from the IDT 48 handler before returning via `iretq`.
pub fn one_shot_us(us: u64) {
    if !APIC_PRESENT.load(Ordering::Relaxed) { return; }
    let cnt_per_us = APIC_CNT_PER_US.load(Ordering::Relaxed) as u64;
    if cnt_per_us == 0 { return; }

    let count = (us * cnt_per_us).min(0xFFFF_FFFF);
    unsafe {
        // Ensure timer is in one-shot mode (bit 17 = 0) and unmasked
        reg_w(REG_LVT_TMR, APIC_TIMER_VECTOR as u32);
        reg_w(REG_INIT_CNT, count as u32);
    }
}

/// Read remaining count of in-progress one-shot.
pub fn remaining_count() -> u32 {
    if !APIC_PRESENT.load(Ordering::Relaxed) { return 0; }
    unsafe { reg_r(REG_CUR_CNT) }
}

/// Must be called at the end of every APIC-sourced interrupt handler.
/// For PIC IRQs, use `pic::send_eoi()` instead.
#[inline(always)]
pub fn send_eoi() {
    // SAFETY: volatile write of 0 to APIC EOI register (write-only).
    unsafe { reg_w(REG_EOI, 0); }
}

// ─────────────────────────────────────────────────────────────────────────────
// Serial helpers (no alloc)

fn serial_hex64(mut v: u64) {
    let hex = b"0123456789ABCDEF";
    for i in (0..16).rev() {
        let nibble = ((v >> (i * 4)) & 0xF) as usize;
        crate::arch::x86_64::serial::write_byte(hex[nibble]);
    }
}

fn serial_hex8(v: u8) {
    let hex = b"0123456789ABCDEF";
    crate::arch::x86_64::serial::write_byte(hex[(v >> 4) as usize]);
    crate::arch::x86_64::serial::write_byte(hex[(v & 0xF) as usize]);
}

fn serial_dec(mut v: u64) {
    if v == 0 { crate::arch::x86_64::serial::write_byte(b'0'); return; }
    let mut buf = [0u8; 20]; let mut i = 0;
    while v > 0 { buf[i] = b'0' + (v % 10) as u8; v /= 10; i += 1; }
    for j in (0..i).rev() { crate::arch::x86_64::serial::write_byte(buf[j]); }
}
