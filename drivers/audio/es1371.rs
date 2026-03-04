/*
 * Ensoniq AudioPCI ES1371/ES1373/CT5880 — AETERNA Microkernel
 *
 * PCI IDs:  1274:1371 (ES1371) — VMware default audio
 *           1274:1373 (ES1373)
 *           1274:5880 (Creative CT5880, also ES1373-based)
 *
 * Hardware overview (vs Intel AC97):
 *   • Single I/O BAR (BAR0, 64 bytes) — no separate NAM/NABM
 *   • AC97 codec accessed via serialised CODEC register (not direct port I/O)
 *   • Sample Rate Converter (SRC) — internal → can produce any rate
 *     (we use 48000 Hz native passthrough for simplicity)
 *   • DMA: one contiguous physical buffer, hardware-loop mode
 *   • Software ring provides the CPU-side queue
 *
 * Chip register map (I/O at BAR0):
 *   0x00  CTRL      32-bit  Chip control
 *   0x04  STATUS    32-bit  Interrupt status (write to clear)
 *   0x0C  MEMPAGE    8-bit  Page select for paged DMA registers
 *   0x10  SRCONV    32-bit  Sample-rate-converter control
 *   0x14  CODEC     32-bit  AC97 codec access serialiser
 *   0x18  LEGACY    32-bit  Legacy mode control
 *   0x20  SCTRL     32-bit  Serial/stream control
 *   0x28  DAC2_CNT  32-bit  DAC2 sample count (hi=total-1, lo=current)
 *   0x30  PBA       32-bit  DMA physical base address    (MEMPAGE=0x0C)
 *   0x34  FC        32-bit  DMA frame count / current    (MEMPAGE=0x0C)
 */

#![allow(dead_code)]

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use core::arch::asm;

use crate::arch::x86_64::serial;
use crate::mm::physical;
use crate::mm::r#virtual::phys_to_virt;

// ─── PCI IDs ─────────────────────────────────────────────────────────────────

const VID_ENSONIQ: u16 = 0x1274;
const DID_ES1371:  u16 = 0x1371;
const DID_ES1373:  u16 = 0x1373;
const DID_CT5880:  u16 = 0x5880;

// ─── Register offsets (from BAR0 I/O base) ───────────────────────────────────

const REG_CTRL:     u16 = 0x00;
const REG_STATUS:   u16 = 0x04;
const REG_MEMPAGE:  u16 = 0x0C;   // 8-bit
const REG_SRCONV:   u16 = 0x10;
const REG_CODEC:    u16 = 0x14;
const REG_LEGACY:   u16 = 0x18;
const REG_SCTRL:    u16 = 0x20;
const REG_DAC2_CNT: u16 = 0x28;
// Paged DMA regs  (write MEMPAGE=0x0C first, then access at 0x30 / 0x34)
const REG_PBA:      u16 = 0x30;   // physical buffer address
const REG_FC:       u16 = 0x34;   // frame count

// MEMPAGE value for DAC2 DMA registers
const PAGE_DAC2: u8 = 0x0C;

// ─── SRCONV bits ─────────────────────────────────────────────────────────────
//
// Bit 22: SRC_DIS — set to disable the Sample Rate Converter and gate the
// AC97 serial port directly from the crystal clock.  Must be set before any
// codec register access, otherwise the serialiser clock stalls and
// CODEC_WIP never clears (causes a boot hang on VMware / bare metal).
const SRCONV_SRC_DIS: u32 = 1 << 22;

// ─── CTRL bits ────────────────────────────────────────────────────────────────

const CTRL_ADC_STOP:  u32 = 1 << 31; // disable ADC (leave off)
const CTRL_XCTL1:     u32 = 1 << 30; // crystal control (leave unchanged)
const CTRL_DAC2_EN:   u32 = 1 << 9;  // enable DAC2 DMA
const CTRL_ADC_EN:    u32 = 1 << 8;  // enable ADC (not needed)
const CTRL_DAC1_EN:   u32 = 1 << 7;  // enable DAC1 wavetable (not needed)
const CTRL_SYNC_RES:  u32 = 1 << 2;  // hold DMA channels in reset (1 → clear to release)

// ─── STATUS bits (W1C — write 1 to clear) ────────────────────────────────────

const STATUS_INTR:    u32 = 1 << 31; // combined interrupt flag
const STATUS_DAC2:    u32 = 1 << 2;  // DAC2 interrupt
const STATUS_DAC1:    u32 = 1 << 1;
const STATUS_ADC:     u32 = 1 << 0;

// ─── SCTRL (serial/stream control) for DAC2 ──────────────────────────────────
//
// bits 21:19  P2_ENDINC  end-address increment per sample reconstruction step
//             0=8-bit-mono, 1=16-bit-mono/8-bit-stereo, 2=16-bit-stereo
// bit  15     P2_LOOPSEL  0=loop, 1=play-once
// bit  14     P2_INTEN    interrupt on loop/completion
// bit  13     P2_PAUSE    pause DAC2 DMA
// bit   2     P2_SEB      0=8-bit, 1=16-bit sample size
// bit   1     P2_SMB      0=mono,  1=stereo

const SCTRL_P2_ENDINC_STEREO16: u32 = 2 << 19; // 16-bit stereo → increment 2
const SCTRL_P2_LOOP:             u32 = 0;        // bit 15 = 0 → loop mode
const SCTRL_P2_INTEN:            u32 = 1 << 14;
const SCTRL_P2_SEB:              u32 = 1 << 2;   // 16-bit
const SCTRL_P2_SMB:              u32 = 1 << 1;   // stereo

// Combined mask for 16-bit stereo, loop, IRQ enabled
const SCTRL_DAC2_16S: u32 = SCTRL_P2_ENDINC_STEREO16
    | SCTRL_P2_LOOP
    | SCTRL_P2_INTEN
    | SCTRL_P2_SEB
    | SCTRL_P2_SMB;

// ─── CODEC register bits ──────────────────────────────────────────────────────

const CODEC_WIP:   u32 = 1 << 31; // write in progress (hardware-set)
const CODEC_READ:  u32 = 1 << 23; // 1=read, 0=write
// addr in bits 22:16, data in bits 15:0

// ─── Software ring buffer ────────────────────────────────────────────────────
//
// 128 KiB circular buffer — write_pcm() fills it from the producer side;
// the IRQ handler drains it into the DMA buffer.

const SW_RING_BYTES: usize = 128 * 1024;
static mut SW_RING: [u8; SW_RING_BYTES] = [0u8; SW_RING_BYTES];
static SW_RING_WR: AtomicU32 = AtomicU32::new(0); // producer write cursor
static SW_RING_RD: AtomicU32 = AtomicU32::new(0); // consumer read cursor

// ─── DMA buffer (single physically-contiguous 4 KiB page) ────────────────────

const DMA_BUF_BYTES: usize = 4096;
// 16-bit stereo frames per DMA buffer:  4096 / 4 = 1024
const DMA_FRAMES: u32 = (DMA_BUF_BYTES / 4) as u32;

static mut DMA_PHYS: u32 = 0; // physical address (<4 GiB, safe for ISA DMA)
static mut DMA_VIRT: u64 = 0; // virtual address for CPU writes

// ─── Driver state ─────────────────────────────────────────────────────────────

static AUDIO_READY:    AtomicBool = AtomicBool::new(false);
static IO_BASE:        AtomicU32  = AtomicU32::new(0);
static ES_IRQ:         AtomicU8   = AtomicU8::new(5);
static DMA_RUNNING:    AtomicBool = AtomicBool::new(false);
static SAMPLE_RATE_HZ: AtomicU32  = AtomicU32::new(44100);

// ─── I/O port helpers ─────────────────────────────────────────────────────────

#[inline(always)]
unsafe fn io_r8(off: u16) -> u8 {
    let port = IO_BASE.load(Ordering::Relaxed) as u16 + off;
    let v: u8;
    asm!("in al, dx", in("dx") port, out("al") v, options(nomem, nostack));
    v
}
#[inline(always)]
unsafe fn io_w8(off: u16, val: u8) {
    let port = IO_BASE.load(Ordering::Relaxed) as u16 + off;
    asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack));
}
#[inline(always)]
unsafe fn io_r32(off: u16) -> u32 {
    let port = IO_BASE.load(Ordering::Relaxed) as u16 + off;
    let v: u32;
    asm!("in eax, dx", in("dx") port, out("eax") v, options(nomem, nostack));
    v
}
#[inline(always)]
unsafe fn io_w32(off: u16, val: u32) {
    let port = IO_BASE.load(Ordering::Relaxed) as u16 + off;
    asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack));
}

// Small delay — read a harmless I/O port
#[inline(always)]
unsafe fn io_delay() {
    let _: u8;
    asm!("in al, dx", in("dx") 0x80u16, out("al") _, options(nomem, nostack));
}

// ─── AC97 codec read / write (via ES1371 serialiser) ─────────────────────────

/// Wait for AC97 serialiser to be idle (CODEC_WIP clear), 2 000-iteration cap.
#[inline]
unsafe fn codec_wait_idle() {
    let mut i = 0u32;
    while io_r32(REG_CODEC) & CODEC_WIP != 0 && i < 2_000 { i += 1; }
}

/// Write `val` to AC97 codec register `reg`.
/// Polls WIP (Write In Progress) before and after.
unsafe fn codec_write(reg: u8, val: u16) {
    codec_wait_idle();
    // Issue write: bit31=0(hw sets), bit23=0(write), bits22:16=addr, bits15:0=data
    let cmd: u32 = ((reg as u32) << 16) | (val as u32);
    io_w32(REG_CODEC, cmd);
    codec_wait_idle();
}

/// Read from AC97 codec register `reg`. Returns 0xFFFF on timeout.
unsafe fn codec_read(reg: u8) -> u16 {
    codec_wait_idle();
    // Issue read request
    let cmd: u32 = CODEC_READ | ((reg as u32) << 16);
    io_w32(REG_CODEC, cmd);
    // Poll for WIP clear then grab returned data
    let mut i = 0u32;
    loop {
        let v = io_r32(REG_CODEC);
        if v & CODEC_WIP == 0 { return (v & 0xFFFF) as u16; }
        i += 1;
        if i >= 2_000 { return 0xFFFF; }
    }
}

/// Enable or disable the SRC so the AC97 serialiser has a clock.
/// Call src_set_dis(true) before any codec_read/codec_write,
/// src_set_dis(false) after codec init is complete.
unsafe fn src_set_dis(dis: bool) {
    let v = io_r32(REG_SRCONV);
    if dis { io_w32(REG_SRCONV, v |  SRCONV_SRC_DIS); }
    else   { io_w32(REG_SRCONV, v & !SRCONV_SRC_DIS); }
    for _ in 0..20u32 { io_delay(); }  // let clock settle
}

// ─── Public init ─────────────────────────────────────────────────────────────

/// Try to initialise the ES1371/ES1373 driver.  Returns `true` on success.
pub fn init() -> bool {
    // ── PCI discovery ────────────────────────────────────────────────────────
    let mut found_pos: Option<(u8, u8, u8)> = None;
    let known: [(u16, u16); 3] = [
        (VID_ENSONIQ, DID_ES1371),
        (VID_ENSONIQ, DID_ES1373),
        (VID_ENSONIQ, DID_CT5880),
    ];
    'scan: for b in 0u8..=255 {
        for d in 0u8..32 {
            let vid = crate::pci::vendor_id(b, d, 0);
            if vid == 0xFFFF { continue; }
            let did = crate::pci::device_id(b, d, 0);
            for &(kv, kd) in &known {
                if vid == kv && did == kd {
                    found_pos = Some((b, d, 0));
                    break 'scan;
                }
            }
        }
    }
    let (bus, dev, func) = match found_pos {
        Some(p) => p,
        None => {
            serial::write_str("[ES1371] No AudioPCI device found\r\n");
            return false;
        }
    };

    let vid = crate::pci::vendor_id(bus, dev, func);
    let did = crate::pci::device_id(bus, dev, func);
    serial::write_str("[ES1371] Found PCI ");
    log_hex16(vid); serial::write_str(":"); log_hex16(did);
    serial::write_str(" @ "); log_pos(bus, dev, func);
    serial::write_str("\r\n");

    // ── BAR0 (single I/O space, 64 bytes) ────────────────────────────────────
    let io_base = match crate::pci::read_bar(bus, dev, func, 0) {
        crate::pci::BarType::Io { base, .. } => base as u32,
        _ => {
            serial::write_str("[ES1371] BAR0 is not I/O — aborting\r\n");
            return false;
        }
    };
    serial::write_str("[ES1371] I/O base=0x"); log_hex32(io_base); serial::write_str("\r\n");
    IO_BASE.store(io_base, Ordering::Relaxed);

    // ── Enable PCI bus mastering ──────────────────────────────────────────────
    crate::pci::enable_bus_master(bus, dev, func);

    // ── Read IRQ line ─────────────────────────────────────────────────────────
    let irq = (crate::pci::config_read32(bus, dev, func, 0x3C) & 0xFF) as u8;
    if irq < 16 {
        ES_IRQ.store(irq, Ordering::Relaxed);
        serial::write_str("[ES1371] IRQ="); log_hex8(irq); serial::write_str("\r\n");
    } else {
        serial::write_str("[ES1371] IRQ not assigned, default 5\r\n");
    }

    unsafe {
        // ── Chip soft reset ────────────────────────────────────────────────
        // Set SYNC_RES to hold DMA channels, then release.
        let ctrl = io_r32(REG_CTRL);
        io_w32(REG_CTRL, ctrl | CTRL_SYNC_RES);
        for _ in 0..100 { io_delay(); }
        io_w32(REG_CTRL, ctrl & !CTRL_SYNC_RES);
        for _ in 0..100 { io_delay(); }

        // Ensure power-on state: clear DAC1/ADC, keep DAC2 off for now
        io_w32(REG_CTRL, ctrl & !(CTRL_DAC2_EN | CTRL_ADC_EN | CTRL_DAC1_EN));

        // ── AC97 codec init ────────────────────────────────────────────────
        //
        // Disable the SRC first so the AC97 serialiser runs from the crystal
        // clock directly.  Without this, CODEC_WIP never clears on VMware and
        // the polling loops below would take hundreds of seconds (boot hang).
        src_set_dis(true);

        // Reset codec (AC97 register 0x00 — write any value)
        codec_write(0x00, 0xFFFF);
        // Short delay for codec to come out of reset (~1 ms)
        for _ in 0..500u32 { io_delay(); }

        // Poll for codec ready — 200 attempts, each capped at 2 000 PCI reads
        // → worst-case ~800 000 port reads total (well under 1 ms on any hypervisor)
        let mut ready = false;
        for _ in 0..200u32 {
            let r = codec_read(0x26); // AC97 Powerdown Control/Status
            if r != 0xFFFF { ready = true; break; }
            for _ in 0..5 { io_delay(); }
        }
        if !ready {
            src_set_dis(false);
            serial::write_str("[ES1371] Codec not ready — aborting\r\n");
            return false;
        }

        // AC97 volume registers (0=max gain, 0x8000=mute)
        // Master volume (0x02): unmute, 0 dB attenuation on both channels
        codec_write(0x02, 0x0000);
        // Headphone volume (0x04): same
        codec_write(0x04, 0x0000);
        // PCM out volume (0x18): unmute, 0dB
        codec_write(0x18, 0x0000);
        // Power down register (0x26): make sure DAC blocks are on
        codec_write(0x26, 0x0000);

        // Sample rate: prefer 44100 Hz (compatible with DOOM mixer output)
        // via AC97 Variable Rate Audio (VRA, Extended ID register 0x28 bit 0)
        let ext_id = codec_read(0x28);
        serial::write_str("[ES1371] AC97 ExtID=0x"); log_hex16(ext_id); serial::write_str("\r\n");
        if ext_id & 0x0001 != 0 {
            // Enable VRA
            let ext_ctrl = codec_read(0x2A);
            codec_write(0x2A, ext_ctrl | 0x0001);
            // Ask for 44100 Hz on DAC1 front channel (0x2C)
            codec_write(0x2C, 44100);
            let actual = codec_read(0x2C);
            SAMPLE_RATE_HZ.store(actual as u32, Ordering::Relaxed);
            serial::write_str("[ES1371] Rate set to ");
            log_dec(actual as u64); serial::write_str(" Hz\r\n");
        } else {
            // No VRA: codec locked at 48000 Hz; soundtest / write_pcm callers
            // must know to generate 48000 Hz data instead of 44100 Hz.
            SAMPLE_RATE_HZ.store(48000, Ordering::Relaxed);
            serial::write_str("[ES1371] VRA not supported, using default 48000 Hz\r\n");
        }

        // Re-enable the SRC now that codec registers are fully programmed.
        src_set_dis(false);

        // ── Allocate DMA buffer ────────────────────────────────────────────
        //
        // ES1371 DMA requires a single physically-contiguous buffer.
        // We allocate ONE physical page (4 KiB) and loop it at the hardware level.
        let phys_frame = match physical::alloc_frame() {
            Some(f) => f,
            None => {
                serial::write_str("[ES1371] DMA alloc failed\r\n");
                return false;
            }
        };
        // Physical address must fit in 32 bits for ISA-compatible DMA
        if phys_frame > 0xFFFF_FFFF {
            serial::write_str("[ES1371] DMA frame above 4 GiB — skipping\r\n");
            // Note: no free_frame API — this leaked frame is acceptable (one-time driver init)
            return false;
        }
        let phys = phys_frame as u32;
        let virt = phys_to_virt(phys_frame);

        DMA_PHYS = phys;
        DMA_VIRT = virt;

        // Zero-fill the DMA buffer (silence)
        let buf_ptr = virt as *mut u8;
        for i in 0..DMA_BUF_BYTES { *buf_ptr.add(i) = 0; }

        serial::write_str("[ES1371] DMA buf phys=0x"); log_hex32(phys);
        serial::write_str(" virt=0x"); log_hex64(virt);
        serial::write_str("\r\n");

        // ── Program DAC2 DMA registers ─────────────────────────────────────
        //
        // Select page 0x0C to expose DAC2 DMA address/count registers at 0x30/0x34
        io_w8(REG_MEMPAGE, PAGE_DAC2);

        // Write physical buffer address
        io_w32(REG_PBA, phys);

        // Frame count: (total_frames - 1) in bits 31:16; current (0) in bits 15:0
        // 1 frame = 4 bytes (16-bit stereo) → frames = DMA_BUF_BYTES/4 = 1024
        io_w32(REG_FC, ((DMA_FRAMES - 1) << 16) | 0);

        // DAC2 sample count: total samples - 1 in bits 31:16
        // At 16-bit stereo: 1 sample = 1 stereo pair = 4 bytes
        // So total_samples = DMA_FRAMES = 1024
        io_w32(REG_DAC2_CNT, ((DMA_FRAMES - 1) << 16) | 0);

        // ── Serial/stream control: DAC2 = 16-bit stereo, loop mode, IRQ on ─
        io_w32(REG_SCTRL, SCTRL_DAC2_16S);

        // ── Clear any pending interrupts ──────────────────────────────────
        io_w32(REG_STATUS, io_r32(REG_STATUS));

        // ── Enable DAC2 DMA ────────────────────────────────────────────────
        let cur_ctrl = io_r32(REG_CTRL);
        io_w32(REG_CTRL, cur_ctrl | CTRL_DAC2_EN);

        DMA_RUNNING.store(true, Ordering::Release);
    }

    AUDIO_READY.store(true, Ordering::Release);
    serial::write_str("[ES1371] Driver ready — 48000 Hz 16-bit stereo, DMA loop active\r\n");
    true
}

// ─── Public: write PCM data ───────────────────────────────────────────────────
//
// `data` is raw 48000 Hz / 16-bit / stereo LE bytes.
// Returns number of bytes actually enqueued (may be less than data.len() if ring full).

pub fn write_pcm(data: &[u8]) -> usize {
    if !AUDIO_READY.load(Ordering::Acquire) { return 0; }
    if data.is_empty() { return 0; }

    // Write to the software ring; if the DMA stalled, restart it.
    let enqueued = ring_push(data);

    // Ensure DMA is running — restart if it somehow stopped
    if !DMA_RUNNING.load(Ordering::Acquire) {
        unsafe { start_dma(); }
    }
    enqueued
}

/// play_sample: blocking-style write — same as write_pcm for ES1371
pub fn play_sample(data: &[u8]) -> bool {
    let n = write_pcm(data);
    n > 0
}

pub fn is_ready() -> bool {
    AUDIO_READY.load(Ordering::Acquire)
}

pub fn irq_line() -> u8 {
    ES_IRQ.load(Ordering::Relaxed)
}

/// Returns the sample rate the codec was programmed to (Hz).
pub fn sample_rate() -> u32 {
    SAMPLE_RATE_HZ.load(Ordering::Relaxed)
}

// ─── IRQ handler ─────────────────────────────────────────────────────────────
//
// Called by `idt::irq_dispatch()` when the ES1371 PIC IRQ fires.
// Clears STATUS, refills the DMA buffer from the software ring.

pub fn handle_irq() {
    if !AUDIO_READY.load(Ordering::Acquire) { return; }
    unsafe {
        let status = io_r32(REG_STATUS);
        if status & STATUS_DAC2 == 0 { return; } // not us

        // Clear DAC2 interrupt status (W1C)
        io_w32(REG_STATUS, STATUS_DAC2);

        // Refill the DMA buffer from the software ring
        refill_dma();
    }
}

// ─── Internal: start DMA playback ─────────────────────────────────────────────

unsafe fn start_dma() {
    // Reset DAC2 DMA channel first (SYNC_RES briefly)
    let ctrl = io_r32(REG_CTRL);
    io_w32(REG_CTRL, ctrl | CTRL_SYNC_RES);
    for _ in 0..20 { io_delay(); }
    io_w32(REG_CTRL, ctrl & !CTRL_SYNC_RES);

    // Reset frame counter
    io_w8(REG_MEMPAGE, PAGE_DAC2);
    io_w32(REG_PBA, DMA_PHYS);
    io_w32(REG_FC, ((DMA_FRAMES - 1) << 16) | 0);
    io_w32(REG_DAC2_CNT, ((DMA_FRAMES - 1) << 16) | 0);

    // Enable DAC2
    let cur_ctrl = io_r32(REG_CTRL);
    io_w32(REG_CTRL, cur_ctrl | CTRL_DAC2_EN);
    DMA_RUNNING.store(true, Ordering::Release);
}

// ─── Internal: refill DMA buffer from software ring ──────────────────────────

unsafe fn refill_dma() {
    let dst = DMA_VIRT as *mut u8;
    let available = ring_available();

    if available == 0 {
        // No data — fill with silence (keep DMA alive but quiet)
        for i in 0..DMA_BUF_BYTES {
            *dst.add(i) = 0;
        }
        return;
    }

    let to_copy = if available >= DMA_BUF_BYTES { DMA_BUF_BYTES } else { available };

    // Copy from ring to DMA buffer, zero-pad rest
    ring_pop_into(dst, to_copy);
    for i in to_copy..DMA_BUF_BYTES {
        *dst.add(i) = 0;
    }
}

// ─── Software ring buffer ────────────────────────────────────────────────────
//
// Power-of-2 size allows efficient wraparound with & mask.
// Single writer (write_pcm), single reader (IRQ handle_irq/refill_dma).
// Safe on single-core because IRQs are level-triggered and cli disables them.

fn ring_push(data: &[u8]) -> usize {
    let wr   = SW_RING_WR.load(Ordering::Acquire) as usize;
    let rd   = SW_RING_RD.load(Ordering::Acquire) as usize;
    let cap  = SW_RING_BYTES;
    let used = (wr.wrapping_sub(rd)) & (cap - 1);
    let free = cap - 1 - used;
    let n    = data.len().min(free);

    for i in 0..n {
        unsafe {
            SW_RING[(wr + i) & (cap - 1)] = data[i];
        }
    }
    SW_RING_WR.store((wr + n) as u32, Ordering::Release);
    n
}

fn ring_available() -> usize {
    let wr = SW_RING_WR.load(Ordering::Acquire) as usize;
    let rd = SW_RING_RD.load(Ordering::Acquire) as usize;
    (wr.wrapping_sub(rd)) & (SW_RING_BYTES - 1)
}

/// Pop `count` bytes from the software ring into `dst`.
/// Caller must ensure count <= ring_available().
unsafe fn ring_pop_into(dst: *mut u8, count: usize) {
    let rd  = SW_RING_RD.load(Ordering::Acquire) as usize;
    let cap = SW_RING_BYTES;
    for i in 0..count {
        *dst.add(i) = SW_RING[(rd + i) & (cap - 1)];
    }
    SW_RING_RD.store((rd + count) as u32, Ordering::Release);
}

// ─── Diagnostics (called by soundtest command) ────────────────────────────────

pub fn dump_status() {
    serial::write_str("[ES1371-DIAG] === ES1371 Status Dump ===\r\n");

    if !AUDIO_READY.load(Ordering::Acquire) {
        serial::write_str("[ES1371-DIAG] Driver NOT ready\r\n");
        return;
    }

    unsafe {
        let ctrl   = io_r32(REG_CTRL);
        let status = io_r32(REG_STATUS);
        let sctrl  = io_r32(REG_SCTRL);

        serial::write_str("[ES1371-DIAG] CTRL=0x");   log_hex32(ctrl);   serial::write_str("\r\n");
        serial::write_str("[ES1371-DIAG] STATUS=0x"); log_hex32(status); serial::write_str("\r\n");
        serial::write_str("[ES1371-DIAG] SCTRL=0x");  log_hex32(sctrl);  serial::write_str("\r\n");
        serial::write_str("[ES1371-DIAG]   DAC2_EN=");
        serial::write_byte(if ctrl & CTRL_DAC2_EN != 0 { b'1' } else { b'0' });
        serial::write_str("  DMA_RUNNING=");
        serial::write_byte(if DMA_RUNNING.load(Ordering::Relaxed) { b'1' } else { b'0' });
        serial::write_str("\r\n");

        io_w8(REG_MEMPAGE, PAGE_DAC2);
        let pba = io_r32(REG_PBA);
        let fc  = io_r32(REG_FC);
        serial::write_str("[ES1371-DIAG] PBA=0x"); log_hex32(pba);
        serial::write_str("  FC=0x"); log_hex32(fc);
        serial::write_str("\r\n");

        let dac2_cnt = io_r32(REG_DAC2_CNT);
        serial::write_str("[ES1371-DIAG] DAC2_CNT=0x"); log_hex32(dac2_cnt); serial::write_str("\r\n");

        let wr = SW_RING_WR.load(Ordering::Relaxed) as usize;
        let rd = SW_RING_RD.load(Ordering::Relaxed) as usize;
        let avail = (wr.wrapping_sub(rd)) & (SW_RING_BYTES - 1);
        serial::write_str("[ES1371-DIAG] SWRing avail=");
        log_dec(avail as u64);
        serial::write_str(" / ");
        log_dec(SW_RING_BYTES as u64);
        serial::write_str(" bytes\r\n");

        // AC97 codec volumes
        let master = codec_read(0x02);
        let pcm    = codec_read(0x18);
        let rate   = codec_read(0x2C);
        serial::write_str("[ES1371-DIAG] Codec: MASTER_VOL=0x"); log_hex16(master);
        serial::write_str("  PCM_VOL=0x"); log_hex16(pcm);
        serial::write_str("  RATE="); log_dec(rate as u64);
        serial::write_str(" Hz\r\n");
    }
    serial::write_str("[ES1371-DIAG] === End ===\r\n");
}

// ─── Tiny hex/dec logging helpers ─────────────────────────────────────────────

fn log_hex8(v: u8) {
    let h = b"0123456789ABCDEF";
    serial::write_byte(h[((v >> 4) & 0xF) as usize]);
    serial::write_byte(h[(v & 0xF) as usize]);
}
fn log_hex16(v: u16) {
    log_hex8((v >> 8) as u8);
    log_hex8(v as u8);
}
fn log_hex32(v: u32) {
    log_hex16((v >> 16) as u16);
    log_hex16(v as u16);
}
fn log_hex64(v: u64) {
    log_hex32((v >> 32) as u32);
    log_hex32(v as u32);
}
fn log_pos(bus: u8, dev: u8, func: u8) {
    serial::write_str("bus="); log_hex8(bus);
    serial::write_str(" dev="); log_hex8(dev);
    serial::write_str(" func="); log_hex8(func);
}
fn log_dec(mut v: u64) {
    if v == 0 { serial::write_byte(b'0'); return; }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while v > 0 { buf[i] = b'0' + (v % 10) as u8; v /= 10; i += 1; }
    for j in (0..i).rev() { serial::write_byte(buf[j]); }
}
