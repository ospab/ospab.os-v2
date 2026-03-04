/*
 * Intel High Definition Audio (HDA) Driver — AETERNA microkernel
 *
 * Supports: QEMU ICH6-HDA (8086:2668), Intel ICH8+ HDA controller
 * PCI class = 0x04 (Multimedia), subclass = 0x03 (HDA)
 *
 * Pipeline:
 *   Kernel / Doom  →  write_pcm()  →  DMA ring buffer  →  HDA stream  →  DAC  →  speaker
 *
 * Configuration:
 *   Format:  44100 Hz, 16-bit, 2-channel (Stereo PCM)
 *   Buffer:  2 × 4 KiB pages (double-buffer, ping-pong)
 *   DMA:     Software-wrapped ring (BDLE list, LVI = 1)
 *
 * Codec communication:
 *   Initial enumeration uses the Immediate Command Interface (ICI/ICS registers)
 *   which avoids the CORB/RIRB DMA complexity during bring-up.
 *   CORB/RIRB are also set up for runtime operation.
 */

#![allow(dead_code)]

use core::sync::atomic::{AtomicBool, Ordering};
use core::ptr::{read_volatile, write_volatile};

use crate::arch::x86_64::serial;
use crate::mm::r#virtual::{phys_to_virt, FLAG_PRESENT, FLAG_WRITABLE, FLAG_PCD, FLAG_PWT};
use crate::mm::physical;

// ─── HDA Controller Register Offsets (from MMIO base) ───────────────────────
const REG_GCAP:     u32 = 0x00; // 16-bit  Global Capabilities
const REG_VMIN:     u32 = 0x02; // 8-bit   Minor Version
const REG_VMAJ:     u32 = 0x03; // 8-bit   Major Version
const REG_OUTPAY:   u32 = 0x04; // 16-bit  Output Payload Capability
const REG_INPAY:    u32 = 0x06; // 16-bit  Input Payload Capability
const REG_GCTL:     u32 = 0x08; // 32-bit  Global Control
const REG_WAKEEN:   u32 = 0x0C; // 16-bit  Wake Enable
const REG_STATESTS: u32 = 0x0E; // 16-bit  State Change Status (codec presence)
const REG_GSTS:     u32 = 0x10; // 16-bit  Global Status
const REG_INTCTL:   u32 = 0x20; // 32-bit  Interrupt Control
const REG_INTSTS:   u32 = 0x24; // 32-bit  Interrupt Status
const REG_WALCLK:   u32 = 0x30; // 32-bit  Wall Clock Counter
const REG_SSYNC:    u32 = 0x38; // 32-bit  Stream Synchronization

// CORB registers
const REG_CORBLBASE: u32 = 0x40; // 32-bit  CORB Lower Base Address
const REG_CORBUBASE: u32 = 0x44; // 32-bit  CORB Upper Base Address
const REG_CORBWP:    u32 = 0x48; // 16-bit  CORB Write Pointer
const REG_CORBRP:    u32 = 0x4A; // 16-bit  CORB Read Pointer (reset: CORBRPRST bit 15)
const REG_CORBCTL:   u32 = 0x4C; // 8-bit   CORB Control (CORBRUN bit 1)
const REG_CORBSTS:   u32 = 0x4D; // 8-bit   CORB Status
const REG_CORBSIZE:  u32 = 0x4E; // 8-bit   CORB Size (write 0x02 for 256-entry, 0x01 for 16, 0x00 for 2)

// RIRB registers
const REG_RIRBLBASE: u32 = 0x50; // 32-bit  RIRB Lower Base Address
const REG_RIRBUBASE: u32 = 0x54; // 32-bit  RIRB Upper Base Address
const REG_RIRBWP:    u32 = 0x58; // 16-bit  RIRB Write Pointer (reset: RIRBWPRST bit 15)
const REG_RINTCNT:   u32 = 0x5A; // 16-bit  Response Interrupt Count
const REG_RIRBCTL:   u32 = 0x5C; // 8-bit   RIRB Control (RIRBDMAEN bit 1)
const REG_RIRBSTS:   u32 = 0x5D; // 8-bit   RIRB Status
const REG_RIRBSIZE:  u32 = 0x5E; // 8-bit   RIRB Size

// Immediate Command Interface
const REG_IMMCMD:    u32 = 0x60; // 32-bit  Immediate Command Output
const REG_IMMRES:    u32 = 0x64; // 32-bit  Immediate Response Input
const REG_ICS:       u32 = 0x68; // 16-bit  Immediate Command Status

// DMA Position Buffer
const REG_DPLBASE:   u32 = 0x70; // 32-bit  DMA Position Lower Base
const REG_DPUBASE:   u32 = 0x74; // 32-bit  DMA Position Upper Base

// ─── GCTL bits ────────────────────────────────────────────────────────────
const GCTL_CRST:  u32 = 1 << 0;  // Controller Reset (0=reset, 1=running)
const GCTL_FCNTRL: u32 = 1 << 1; // Flush Control
const GCTL_UNSOL: u32 = 1 << 8;  // Accept Unsolicited Responses

// ─── ICS bits ─────────────────────────────────────────────────────────────
const ICS_ICB:  u16 = 1 << 0; // Immediate Command Busy
const ICS_IRV:  u16 = 1 << 1; // Immediate Result Valid

// ─── Stream Descriptor offsets (from SD base) ─────────────────────────────
const SD_OFF_CTL:   u32 = 0x00; // 24-bit  Stream Control
const SD_OFF_STS:   u32 = 0x03; // 8-bit   Stream Status
const SD_OFF_LPIB:  u32 = 0x04; // 32-bit  Link Position in Buffer
const SD_OFF_CBL:   u32 = 0x08; // 32-bit  Cyclic Buffer Length
const SD_OFF_LVI:   u32 = 0x0C; // 16-bit  Last Valid Index
const SD_OFF_FIFOW: u32 = 0x0E; // 16-bit  FIFO Watermark
const SD_OFF_FMT:   u32 = 0x12; // 16-bit  Stream Format
const SD_OFF_BDPL:  u32 = 0x18; // 32-bit  BDL Lower Physical Address
const SD_OFF_BDPU:  u32 = 0x1C; // 32-bit  BDL Upper Physical Address

// SD Control bits
const SD_CTL_SRST:  u32 = 1 << 0; // Stream Reset
const SD_CTL_RUN:   u32 = 1 << 1; // Stream Run
const SD_CTL_IOCE:  u32 = 1 << 2; // Interrupt on Completion Enable
const SD_CTL_DEIE:  u32 = 1 << 4; // FIFO Error Interrupt Enable
const SD_CTL_STRIPE: u32 = 0 << 16; // Stripe Control (single)
// Bits 23:20 = stream number (tag), set to 1 for first output stream
const SD_STREAM_TAG: u32 = 1 << 20;

// ─── HDA stream format (SDFMT) ────────────────────────────────────────────
// Produced by: type=PCM, base=44.1kHz, mult=1x, div=/1, bits=16, ch=2
// Bit [14]   = 0: PCM (implicit)
// Bit [13]   = 1: 44.1 kHz base (0 = 48 kHz base)
// Bits[13:11]= 001: base rate (44.1/48) — wait, let me recount:
//
// From HDA spec 1.0a, Section 3.7.1 (Stream Descriptor Format):
//   [14]   Type: 0=PCM, 1=non-PCM
//   [13]   Base: 0=48kHz family, 1=44.1kHz family
//   [12:11] Mult: 00=1×, 01=2×, 10=3×, 11=4×
//   [10:8]  Div:  000=/1, 001=/2, 010=/3, ...
//   [7:4]   Bits: 0000=8b, 0001=16b, 0010=20b, 0011=24b, 0100=32b
//   [3:0]   Chan: channels - 1  (0001 = 2ch stereo)
//
// 44100/1/1 16-bit 2ch = base=1 mult=00 div=000 bits=0001 chan=0001
// = 0b0_1_00_000_0001_0001 = 0x2011
const SDFMT_44100_16_STEREO: u16 = (1 << 13) | (0b0001 << 4) | 0b0001;

// ─── DMA Buffer Descriptor List Entry ─────────────────────────────────────
#[repr(C)]
struct BdlEntry {
    addr_lo: u32,  // Physical address low
    addr_hi: u32,  // Physical address high
    length:  u32,  // Buffer length in bytes
    ioc:     u32,  // Interrupt on Completion (bit 0)
}

// ─── HDA Codec Verb definitions ───────────────────────────────────────────
// Verb: bits[31:28]=codec, [27:20]=NID, [19:0]=verb+payload

fn make_verb(codec: u8, nid: u8, verb: u32, payload: u32) -> u32 {
    ((codec as u32) << 28) | ((nid as u32) << 20) | ((verb & 0xFFF) << 8) | (payload & 0xFF)
}
fn make_verb_long(codec: u8, nid: u8, verb: u16, payload: u16) -> u32 {
    ((codec as u32) << 28) | ((nid as u32) << 20) | ((verb as u32) << 8) | (payload as u32 & 0xFF)
}

// HDA verbs
const VERB_GET_PARAM:         u32 = 0xF00;
const VERB_SET_STREAM_FMT:    u32 = 0x200; // 12-bit verb, 8-bit payload split (use make_verb_wide)
const VERB_SET_AMP_GAIN_MUTE: u32 = 0x300; // 4-bit payload
const VERB_SET_POWER_STATE:   u32 = 0x705;
const VERB_SET_STREAM_CHAN:    u32 = 0x706;
const VERB_SET_PIN_WIDGET_CTL:u32 = 0x707;
const VERB_SET_EAPD:          u32 = 0x70C;
const VERB_FUNC_RESET:        u32 = 0x7FF;

// GET_PARAM parameter IDs
const PARAM_VENDOR_ID:        u8 = 0x00;
const PARAM_REVISION_ID:      u8 = 0x02;
const PARAM_NODE_COUNT:       u8 = 0x04;
const PARAM_FUNC_TYPE:        u8 = 0x05;
const PARAM_AUDIO_CAP:        u8 = 0x09;
const PARAM_PCM_SIZE:         u8 = 0x0A;
const PARAM_CONN_LIST_LEN:    u8 = 0x0E;

fn make_get_param(codec: u8, nid: u8, param: u8) -> u32 {
    ((codec as u32) << 28) | ((nid as u32) << 20) | (VERB_GET_PARAM << 8) | (param as u32)
}

/// SET_STREAM_FORMAT: 4-bit verb ID + 16-bit format value
/// Encoding: [31:28]=codec, [27:20]=NID, [19:16]=0x2 (set fmt verb family), [15:0]=format
fn make_set_fmt(codec: u8, nid: u8, fmt: u16) -> u32 {
    ((codec as u32) << 28) | ((nid as u32) << 20) | (0x2u32 << 16) | (fmt as u32)
}

/// SET_AMP_GAIN_MUTE: [31:28]=codec, [27:20]=NID, [19:16]=0x3, [15:0]=amp payload
/// Payload: bit15=output, bit14=input, bit13=left, bit12=right, bit7=mute, bits6:0=gain
fn make_set_amp(codec: u8, nid: u8, payload: u16) -> u32 {
    ((codec as u32) << 28) | ((nid as u32) << 20) | (0x3u32 << 16) | (payload as u32)
}

// ─── Driver state ─────────────────────────────────────────────────────────
static AUDIO_READY: AtomicBool = AtomicBool::new(false);

/// MMIO base (virtual, via HHDM)
static mut MMIO_BASE: u64 = 0;

/// Number of input streams (from GCAP[11:8])
static mut ISS: u8 = 0;

/// Physical address of output stream's DMA buffer (2 pages = 8 KiB total)
static mut DMA_BUF_PHYS:  [u64; 2] = [0; 2];
static mut DMA_BUF_VIRT:  [u64; 2] = [0; 2];

/// BDL physical and virtual addresses
static mut BDL_PHYS: u64 = 0;
static mut BDL_VIRT: u64 = 0;

/// Write position (in bytes) within the ring buffer
static mut WRITE_POS: usize = 0;

/// Total DMA ring buffer size in bytes
const DMA_BUF_SIZE: usize = 4096; // per buffer entry
const DMA_ENTRIES:  usize = 2;
const DMA_TOTAL:    usize = DMA_BUF_SIZE * DMA_ENTRIES; // 8 KiB ring

// ─── MMIO helpers ─────────────────────────────────────────────────────────

#[inline(always)]
unsafe fn read8(offset: u32) -> u8 {
    read_volatile((MMIO_BASE + offset as u64) as *const u8)
}
#[inline(always)]
unsafe fn read16(offset: u32) -> u16 {
    read_volatile((MMIO_BASE + offset as u64) as *const u16)
}
#[inline(always)]
unsafe fn read32(offset: u32) -> u32 {
    read_volatile((MMIO_BASE + offset as u64) as *const u32)
}
#[inline(always)]
unsafe fn write8(offset: u32, val: u8) {
    write_volatile((MMIO_BASE + offset as u64) as *mut u8, val);
}
#[inline(always)]
unsafe fn write16(offset: u32, val: u16) {
    write_volatile((MMIO_BASE + offset as u64) as *mut u16, val);
}
#[inline(always)]
unsafe fn write32(offset: u32, val: u32) {
    write_volatile((MMIO_BASE + offset as u64) as *mut u32, val);
}

/// Read from a stream descriptor register
unsafe fn sd_read32(sd_base: u32, off: u32) -> u32 {
    read32(sd_base + off)
}
unsafe fn sd_write32(sd_base: u32, off: u32, val: u32) {
    write32(sd_base + off, val)
}
unsafe fn sd_read16(sd_base: u32, off: u32) -> u16 {
    read16(sd_base + off)
}
unsafe fn sd_write16(sd_base: u32, off: u32, val: u16) {
    write16(sd_base + off, val)
}
unsafe fn sd_read8(sd_base: u32, off: u32) -> u8 {
    read8(sd_base + off)
}
unsafe fn sd_write8(sd_base: u32, off: u32, val: u8) {
    write8(sd_base + off, val)
}

// ─── PIT-based delay (busy-wait) ──────────────────────────────────────────
fn wait_ticks(n: u64) {
    let target = crate::arch::x86_64::idt::timer_ticks() + n;
    while crate::arch::x86_64::idt::timer_ticks() < target {
        unsafe { core::arch::asm!("pause"); }
    }
}

// ─── Immediate Command Interface ──────────────────────────────────────────

/// Send a verb via Immediate Command Interface; return response or 0 on timeout.
unsafe fn imm_send(verb: u32) -> u32 {
    // Wait until not busy (ICB = 0)
    let deadline = crate::arch::x86_64::idt::timer_ticks() + 10;
    while read16(REG_ICS) & ICS_ICB != 0 {
        if crate::arch::x86_64::idt::timer_ticks() > deadline { return 0; }
        core::arch::asm!("pause");
    }

    // Write verb, set ICB to start transmission
    write32(REG_IMMCMD, verb);
    let ics = read16(REG_ICS);
    write16(REG_ICS, (ics & !ICS_IRV) | ICS_ICB); // set ICB, clear IRV

    // Wait for IRV (result valid)
    let deadline = crate::arch::x86_64::idt::timer_ticks() + 10;
    loop {
        let ics = read16(REG_ICS);
        if ics & ICS_IRV != 0 {
            // Clear IRV
            write16(REG_ICS, ics | ICS_IRV);
            return read32(REG_IMMRES);
        }
        if crate::arch::x86_64::idt::timer_ticks() > deadline { return 0; }
        core::arch::asm!("pause");
    }
}

// ─── CORB/RIRB setup ──────────────────────────────────────────────────────

unsafe fn setup_corb_rirb() -> bool {
    // Allocate one 4K page: first 4KiB = CORB, second = RIRB (we pack in one frame)
    // CORB: 256 × 4 bytes = 1024 bytes; RIRB: 256 × 8 bytes = 2048 bytes → fits in one 4K page
    let frame = match physical::alloc_frame() {
        Some(f) => f,
        None => {
            serial::write_str("[HDA] CORB/RIRB alloc failed\r\n");
            return false;
        }
    };
    // Zero the frame
    let virt = phys_to_virt(frame) as *mut u8;
    core::ptr::write_bytes(virt, 0, 4096);

    // CORB at frame+0, RIRB at frame+1024
    let corb_phys = frame;
    let rirb_phys = frame + 1024;

    // Stop CORB DMA
    write8(REG_CORBCTL, 0x00);
    wait_ticks(1);

    // Set CORB size = 256 entries (0x02 in CORBSIZE)
    write8(REG_CORBSIZE, 0x02);

    // Set CORB base
    write32(REG_CORBLBASE, (corb_phys & 0xFFFF_FFFF) as u32);
    write32(REG_CORBUBASE, (corb_phys >> 32) as u32);

    // Reset CORB read pointer (set CORBRPRST bit, then clear)
    write16(REG_CORBRP, 0x8000);
    let timeout = crate::arch::x86_64::idt::timer_ticks() + 5;
    while read16(REG_CORBRP) & 0x8000 == 0 {
        if crate::arch::x86_64::idt::timer_ticks() > timeout { break; }
    }
    write16(REG_CORBRP, 0x0000);

    // Reset CORB write pointer
    write16(REG_CORBWP, 0x0000);

    // Start CORB DMA
    write8(REG_CORBCTL, 0x02); // CORBRUN

    // Stop RIRB DMA
    write8(REG_RIRBCTL, 0x00);
    wait_ticks(1);

    // Set RIRB size = 256 entries
    write8(REG_RIRBSIZE, 0x02);

    // Set RIRB base
    write32(REG_RIRBLBASE, (rirb_phys & 0xFFFF_FFFF) as u32);
    write32(REG_RIRBUBASE, (rirb_phys >> 32) as u32);

    // Reset RIRB write pointer
    write16(REG_RIRBWP, 0x8000);

    // Set RIRB interrupt count (respond every 1 entry)
    write16(REG_RINTCNT, 0x01);

    // Start RIRB DMA
    write8(REG_RIRBCTL, 0x02); // RIRBDMAEN

    serial::write_str("[HDA] CORB/RIRB initialized\r\n");
    true
}

// ─── Output stream setup ──────────────────────────────────────────────────

/// Returns the MMIO offset of output stream 0
unsafe fn output_sd_base() -> u32 {
    // Stream descriptors start at 0x80.
    // Input streams come first: ISS input SDs × 0x20 each, then output SDs.
    0x80 + (ISS as u32) * 0x20
}

unsafe fn setup_output_stream() -> bool {
    // Allocate 2 physical pages for the DMA ring buffer
    for i in 0..DMA_ENTRIES {
        let frame = match physical::alloc_frame() {
            Some(f) => f,
            None => {
                serial::write_str("[HDA] DMA buffer alloc failed\r\n");
                return false;
            }
        };
        // Zero the buffer (silence)
        let virt = phys_to_virt(frame) as *mut u8;
        core::ptr::write_bytes(virt, 0, 4096);
        DMA_BUF_PHYS[i] = frame;
        DMA_BUF_VIRT[i] = virt as u64;
    }

    // Allocate BDL (Buffer Descriptor List): DMA_ENTRIES × 16 bytes each
    let bdl_frame = match physical::alloc_frame() {
        Some(f) => f,
        None => {
            serial::write_str("[HDA] BDL alloc failed\r\n");
            return false;
        }
    };
    let bdl_virt_addr = phys_to_virt(bdl_frame);
    core::ptr::write_bytes(bdl_virt_addr as *mut u8, 0, 4096);
    BDL_PHYS = bdl_frame;
    BDL_VIRT = bdl_virt_addr;

    // Fill BDL entries
    let bdl = BDL_VIRT as *mut BdlEntry;
    for i in 0..DMA_ENTRIES {
        let e = &mut *bdl.add(i);
        let phys = DMA_BUF_PHYS[i];
        e.addr_lo = (phys & 0xFFFF_FFFF) as u32;
        e.addr_hi = (phys >> 32) as u32;
        e.length  = DMA_BUF_SIZE as u32;
        e.ioc     = 1; // interrupt on completion (optional but useful)
    }

    let sd = output_sd_base();

    // Reset the stream descriptor
    let ctl = sd_read32(sd, SD_OFF_CTL);
    sd_write32(sd, SD_OFF_CTL, ctl | SD_CTL_SRST);
    let timeout = crate::arch::x86_64::idt::timer_ticks() + 5;
    while sd_read8(sd, SD_OFF_CTL) & (SD_CTL_SRST as u8) == 0 {
        if crate::arch::x86_64::idt::timer_ticks() > timeout { break; }
    }
    sd_write32(sd, SD_OFF_CTL, ctl & !SD_CTL_SRST);
    let timeout = crate::arch::x86_64::idt::timer_ticks() + 5;
    while sd_read8(sd, SD_OFF_CTL) & (SD_CTL_SRST as u8) != 0 {
        if crate::arch::x86_64::idt::timer_ticks() > timeout { break; }
    }

    // Set stream format
    sd_write16(sd, SD_OFF_FMT, SDFMT_44100_16_STEREO);

    // Set Cyclic Buffer Length (total DMA ring size)
    sd_write32(sd, SD_OFF_CBL, DMA_TOTAL as u32);

    // Last Valid Index = DMA_ENTRIES - 1
    sd_write16(sd, SD_OFF_LVI, (DMA_ENTRIES - 1) as u16);

    // Set BDL base address
    sd_write32(sd, SD_OFF_BDPL, (BDL_PHYS & 0xFFFF_FFFF) as u32);
    sd_write32(sd, SD_OFF_BDPU, (BDL_PHYS >> 32) as u32);

    // Set stream tag = 1 (bits 23:20), no stripe (bits 17:16 = 0)
    let ctl = sd_read32(sd, SD_OFF_CTL);
    let ctl = (ctl & 0x00FFFFFF) | SD_STREAM_TAG;
    sd_write32(sd, SD_OFF_CTL, ctl);

    serial::write_str("[HDA] Output stream configured: 44100Hz/16-bit/2ch\r\n");
    true
}

// ─── Codec enumeration and widget configuration ────────────────────────────

unsafe fn configure_codec(codec: u8) {
    let log_codec = || {
        serial::write_str("[HDA] Codec ");
        serial::write_str(if codec == 0 { "0" } else { "1" });
    };

    // Send function reset to codec root node
    imm_send(make_get_param(codec, 0x00, PARAM_VENDOR_ID));
    wait_ticks(1);

    // Get codec root: node 0 — get audio function group start node
    let node_count = imm_send(make_get_param(codec, 0x00, PARAM_NODE_COUNT));
    let start_nid = (node_count >> 16) as u8;
    let _count    = (node_count & 0xFF) as u8;

    // For QEMU ICH6-HDA, audio function group is at NID=1
    // Power up the audio function group and ALL sub-widgets
    let afg = start_nid;

    // Power state D0 for function group
    imm_send(make_verb(codec, afg, VERB_SET_POWER_STATE, 0x00));
    wait_ticks(2);

    // Get widget start NID within AFG
    let sub_nodes = imm_send(make_get_param(codec, afg, PARAM_NODE_COUNT));
    let widget_start = (sub_nodes >> 16) as u8;
    let widget_count = (sub_nodes & 0xFF) as u8;

    log_codec();
    serial::write_str(": AFG=");
    serial_u8(afg);
    serial::write_str(" widgets=");
    serial_u8(widget_count);
    serial::write_str("\r\n");

    // Walk widgets, power up all, find output DAC (type 0x0) and pin (type 0x4)
    let mut dac_nid: u8 = 0;
    let mut pin_nid: u8 = 0;

    for i in 0..widget_count {
        let nid = widget_start + i;

        // Power up every widget
        imm_send(make_verb(codec, nid, VERB_SET_POWER_STATE, 0x00));

        let cap = imm_send(make_get_param(codec, nid, PARAM_AUDIO_CAP));
        let widget_type = (cap >> 20) & 0xF;

        match widget_type {
            0x0 => { // Audio Output (DAC)
                if dac_nid == 0 { dac_nid = nid; }
            }
            0x4 => { // Pin Complex
                if pin_nid == 0 { pin_nid = nid; }
            }
            _ => {}
        }
    }

    // Fallback: QEMU HDA fixed topology (NID 2 = DAC, NID 3 = Pin)
    if dac_nid == 0 { dac_nid = 2; }
    if pin_nid  == 0 { pin_nid  = 3; }

    log_codec();
    serial::write_str(": DAC NID=");
    serial_u8(dac_nid);
    serial::write_str(" Pin NID=");
    serial_u8(pin_nid);
    serial::write_str("\r\n");

    // Set stream format on DAC: stream 1, channel 0
    imm_send(make_set_fmt(codec, dac_nid, SDFMT_44100_16_STEREO));
    // Assign stream 1, channel 0 to DAC
    imm_send(make_verb(codec, dac_nid, VERB_SET_STREAM_CHAN, (1 << 4) | 0));

    // Set DAC amplifier: output, both channels, no mute, 0 dB (gain = 0x7F for QEMU)
    // Payload: [15]=out, [14]=in, [13]=left, [12]=right, [7]=mute, [6:0]=gain
    let amp_payload: u16 = (1 << 15) | (1 << 13) | (1 << 12) | 0x50;
    imm_send(make_set_amp(codec, dac_nid, amp_payload));

    // Enable pin: output enable + headphone (HP) enable
    imm_send(make_verb(codec, pin_nid, VERB_SET_PIN_WIDGET_CTL, 0xC0)); // OUT | HP

    // Pin amplifier: output enable, both channels, full gain
    let pin_amp: u16 = (1 << 15) | (1 << 13) | (1 << 12) | 0x50;
    imm_send(make_set_amp(codec, pin_nid, pin_amp));

    // EAPD enable (external amplifier power)
    imm_send(make_verb(codec, pin_nid, VERB_SET_EAPD, 0x02));

    log_codec();
    serial::write_str(": configured DAC+pin for stream 1\r\n");
}

fn serial_u8(n: u8) {
    let hi = b"0123456789ABCDEF"[(n >> 4) as usize];
    let lo = b"0123456789ABCDEF"[(n & 0xF) as usize];
    serial::write_byte(b'0');
    serial::write_byte(b'x');
    serial::write_byte(hi);
    serial::write_byte(lo);
}

// ─── Public init ──────────────────────────────────────────────────────────

/// Scan PCI, initialize HDA controller, configure codec and output stream.
/// Returns true if an HDA controller was found and initialized.
pub fn init() -> bool {
    // Fast path: use pre-enumerated PCI table (pci::enumerate() called at boot Phase 2.8)
    let (found_bus, found_dev, found_func) =
        if let Some(d) = crate::pci::find_by_class(0x04, 0x03, 0x00) {
            serial::write_str("[HDA] Found via PCI table\r\n");
            (d.bus, d.device, d.function)
        } else {
            // Fallback: raw PCI scan (enumerate not called yet or found nothing)
            let mut fb = 0u8;
            let mut fd = 0u8;
            let mut ff = 0u8;
            let mut found = false;
            'scan: for bus in 0..=255u8 {
                for dev in 0..32u8 {
                    for func in 0..8u8 {
                        if crate::pci::vendor_id(bus, dev, func) == 0xFFFF { continue; }
                        let (class, subclass, _) = crate::pci::class_code(bus, dev, func);
                        if class == 0x04 && subclass == 0x03 {
                            fb = bus; fd = dev; ff = func;
                            found = true;
                            break 'scan;
                        }
                    }
                }
            }
            if !found {
                serial::write_str("[HDA] No HD Audio controller found\r\n");
                return false;
            }
            (fb, fd, ff)
        };

    let vid = crate::pci::vendor_id(found_bus, found_dev, found_func);
    let did = crate::pci::device_id(found_bus, found_dev, found_func);
    serial::write_str("[HDA] Found ");
    serial::write_byte(b'0' + found_bus / 10);
    serial::write_byte(b'0' + found_bus % 10);
    serial::write_byte(b':');
    serial::write_byte(b'0' + found_dev / 10);
    serial::write_byte(b'0' + found_dev % 10);
    serial::write_str(".0 VID:DID=");
    serial_u16(vid); serial::write_byte(b':'); serial_u16(did);
    serial::write_str("\r\n");

    // Enable bus mastering + memory space
    crate::pci::enable_bus_master(found_bus, found_dev, found_func);

    // Read BAR0 (MMIO base)
    let bar0 = match crate::pci::read_bar(found_bus, found_dev, found_func, 0) {
        crate::pci::BarType::Mmio { base, .. } => base,
        _ => {
            serial::write_str("[HDA] BAR0 is not MMIO — aborting\r\n");
            return false;
        }
    };

    serial::write_str("[HDA] MMIO BAR0=0x");
    serial_u64(bar0);
    serial::write_str("\r\n");

    unsafe {
        // Map MMIO via HHDM (Limine already identity-mapped all physical memory)
        MMIO_BASE = phys_to_virt(bar0);

        // Controller reset sequence
        // 1. Clear CRST (assert reset)
        let gctl = read32(REG_GCTL);
        write32(REG_GCTL, gctl & !GCTL_CRST);

        // Wait at least 100 µs (1 tick at 100 Hz = 10ms, more than enough)
        wait_ticks(2);

        // 2. Set CRST (deassert reset)
        write32(REG_GCTL, gctl | GCTL_CRST);

        // Wait for controller to come out of reset and for codecs to enumerate
        // Spec says wait 521 µs minimum after CRST = 1 before checking STATESTS
        wait_ticks(2);

        // 3. Wait for at least one codec to appear (STATESTS != 0)
        let mut codec_mask = 0u16;
        let deadline = crate::arch::x86_64::idt::timer_ticks() + 20;
        while crate::arch::x86_64::idt::timer_ticks() < deadline {
            codec_mask = read16(REG_STATESTS);
            if codec_mask != 0 { break; }
            core::arch::asm!("pause");
        }

        if codec_mask == 0 {
            serial::write_str("[HDA] No codecs detected (STATESTS=0)\r\n");
            return false;
        }

        serial::write_str("[HDA] STATESTS=0x");
        serial_u16(codec_mask);
        serial::write_str(" — codec(s) present\r\n");

        // Clear STATESTS by writing 1 to each set bit
        write16(REG_STATESTS, codec_mask);

        // Read GCAP to learn ISS/OSS
        let gcap = read16(REG_GCAP);
        ISS = ((gcap >> 8) & 0xF) as u8;
        let oss = ((gcap >> 12) & 0xF) as u8;
        serial::write_str("[HDA] GCAP=0x");
        serial_u16(gcap);
        serial::write_str(" ISS=");
        serial::write_byte(b'0' + ISS);
        serial::write_str(" OSS=");
        serial::write_byte(b'0' + oss);
        serial::write_str("\r\n");

        if oss == 0 {
            serial::write_str("[HDA] No output streams available\r\n");
            return false;
        }

        // Enable unsolicited responses
        write32(REG_GCTL, read32(REG_GCTL) | GCTL_UNSOL);

        // Set up CORB and RIRB
        if !setup_corb_rirb() {
            return false;
        }

        // Set up output stream DMA buffer
        if !setup_output_stream() {
            return false;
        }

        // Configure codec 0 (find DAC, set stream format, set amp gains)
        configure_codec(0);

        // Start the output stream
        let sd = output_sd_base();
        let ctl = sd_read32(sd, SD_OFF_CTL);
        sd_write32(sd, SD_OFF_CTL, ctl | SD_CTL_RUN);

        // Wait for run bit to stabilize
        wait_ticks(1);

        WRITE_POS = 0;
        AUDIO_READY.store(true, Ordering::Release);
    }

    serial::write_str("[HDA] Audio driver initialized — stream running\r\n");
    true
}

/// Write PCM samples into the DMA ring buffer.
///
/// `pcm` must be raw interleaved stereo 16-bit LE samples.
/// Silently wraps around the ring buffer.
pub fn write_pcm(pcm: &[u8]) {
    if !AUDIO_READY.load(Ordering::Relaxed) { return; }

    unsafe {
        let ring = [
            core::slice::from_raw_parts_mut(DMA_BUF_VIRT[0] as *mut u8, DMA_BUF_SIZE),
            core::slice::from_raw_parts_mut(DMA_BUF_VIRT[1] as *mut u8, DMA_BUF_SIZE),
        ];

        let mut remaining = pcm;
        while !remaining.is_empty() {
            let buf_idx   = WRITE_POS / DMA_BUF_SIZE;
            let buf_off   = WRITE_POS % DMA_BUF_SIZE;
            let space     = DMA_BUF_SIZE - buf_off;
            let copy_len  = space.min(remaining.len());

            ring[buf_idx % DMA_ENTRIES][buf_off..buf_off + copy_len]
                .copy_from_slice(&remaining[..copy_len]);

            WRITE_POS = (WRITE_POS + copy_len) % DMA_TOTAL;
            remaining = &remaining[copy_len..];
        }
    }
}

/// Returns true if HDA is initialized and the stream is running.
pub fn is_ready() -> bool {
    AUDIO_READY.load(Ordering::Relaxed)
}

// ─── Serial formatting helpers ────────────────────────────────────────────

fn serial_u16(n: u16) {
    let hex = b"0123456789ABCDEF";
    serial::write_byte(hex[((n >> 12) & 0xF) as usize]);
    serial::write_byte(hex[((n >>  8) & 0xF) as usize]);
    serial::write_byte(hex[((n >>  4) & 0xF) as usize]);
    serial::write_byte(hex[((n >>  0) & 0xF) as usize]);
}

fn serial_u64(n: u64) {
    let hex = b"0123456789ABCDEF";
    for i in (0..16).rev() {
        serial::write_byte(hex[((n >> (i * 4)) & 0xF) as usize]);
    }
}
