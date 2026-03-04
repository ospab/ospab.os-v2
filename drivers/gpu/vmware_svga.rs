/*
 * VMware SVGA II Display Adapter Driver — AETERNA microkernel
 *
 * Supports: VMware Workstation / Fusion / ESXi virtual GPU
 * PCI ID:   VendorID=0x15AD (VMware), DeviceID=0x0405 (SVGA II)
 *
 * Hardware interface:
 *   BAR0  — I/O register ports (16 bytes)
 *             offset +0 : SVGA_INDEX_PORT  (u32 write: register selector)
 *             offset +1 : SVGA_VALUE_PORT  (u32 read/write: register value)
 *   BAR1  — Framebuffer MMIO  (linear RGB framebuffer)
 *   BAR2  — FIFO MMIO         (command ring buffer)
 *
 * Initialization sequence:
 *   1. Probe PCI 15AD:0405 and read BARs
 *   2. Negotiate SVGA ID (verify SVGA_ID_2 = 0x90000002)
 *   3. Read capabilities, FB/FIFO addresses from device registers
 *   4. Program FIFO_MIN / FIFO_MAX / FIFO_NEXT_CMD / FIFO_STOP
 *   5. Set SVGA_REG_ENABLE = 1
 *   6. Optionally set SVGA_REG_WIDTH / HEIGHT / BITS_PER_PIXEL
 *
 * Usage:
 *   ospab_os::drivers::gpu::vmware_svga::init()      — detect + init
 *   ospab_os::drivers::gpu::vmware_svga::set_mode()  — change resolution
 *   ospab_os::drivers::gpu::vmware_svga::is_ready()  — check init status
 *   ospab_os::drivers::gpu::vmware_svga::fb_base()   — get framebuffer base
 */

#![allow(dead_code)]

use core::sync::atomic::{AtomicBool, Ordering};
use crate::arch::x86_64::serial;

// ─── PCI identity ────────────────────────────────────────────────────────────
const VMWARE_VID:  u16 = 0x15AD;
const SVGA2_DID:   u16 = 0x0405;

// ─── SVGA magic / ID constants ────────────────────────────────────────────────
const SVGA_MAGIC:     u32 = 0x00900000;
const SVGA_ID_0:      u32 = SVGA_MAGIC << 8;                // 0x90000000
const SVGA_ID_1:      u32 = (SVGA_MAGIC << 8) | 1;         // 0x90000001
const SVGA_ID_2:      u32 = (SVGA_MAGIC << 8) | 2;         // 0x90000002

// ─── SVGA register indices (written to INDEX_PORT, value via VALUE_PORT) ─────
const SVGA_REG_ID:              u32 = 0;
const SVGA_REG_ENABLE:          u32 = 1;
const SVGA_REG_WIDTH:           u32 = 2;
const SVGA_REG_HEIGHT:          u32 = 3;
const SVGA_REG_MAX_WIDTH:       u32 = 4;
const SVGA_REG_MAX_HEIGHT:      u32 = 5;
const SVGA_REG_DEPTH:           u32 = 6;
const SVGA_REG_BITS_PER_PIXEL:  u32 = 7;
const SVGA_REG_PSEUDOCOLOR:     u32 = 8;
const SVGA_REG_RED_MASK:        u32 = 9;
const SVGA_REG_GREEN_MASK:      u32 = 10;
const SVGA_REG_BLUE_MASK:       u32 = 11;
const SVGA_REG_BYTES_PER_LINE:  u32 = 12;
const SVGA_REG_FB_START:        u32 = 13;
const SVGA_REG_FB_OFFSET:       u32 = 14;
const SVGA_REG_VRAM_SIZE:       u32 = 15;
const SVGA_REG_FB_SIZE:         u32 = 16;
const SVGA_REG_CAPABILITIES:    u32 = 17;
const SVGA_REG_MEM_START:       u32 = 18;   // FIFO region physical base
const SVGA_REG_MEM_SIZE:        u32 = 19;   // FIFO region size
const SVGA_REG_CONFIG_DONE:     u32 = 20;
const SVGA_REG_SYNC:            u32 = 21;   // write 1 → flush FIFO
const SVGA_REG_BUSY:            u32 = 22;   // read → 1 while flushing
const SVGA_REG_GUEST_ID:        u32 = 23;
const SVGA_REG_SCRATCH_SIZE:    u32 = 29;
const SVGA_REG_MEM_REGS:        u32 = 30;
const SVGA_REG_NUM_DISPLAYS:    u32 = 31;
const SVGA_REG_PITCHLOCK:       u32 = 32;

// ─── SVGA Capabilities bits ───────────────────────────────────────────────────
const SVGA_CAP_RECT_FILL:       u32 = 0x0001;
const SVGA_CAP_RECT_COPY:       u32 = 0x0002;
const SVGA_CAP_RECT_PAT_FILL:   u32 = 0x0004;
const SVGA_CAP_LEGACY_OFFSCREEN:u32 = 0x0008;
const SVGA_CAP_RASTER_OP:       u32 = 0x0010;
const SVGA_CAP_CURSOR:          u32 = 0x0020;
const SVGA_CAP_CURSOR_BYPASS:   u32 = 0x0040;
const SVGA_CAP_CURSOR_BYPASS_2: u32 = 0x0080;
const SVGA_CAP_8BIT_EMULATION:  u32 = 0x0100;
const SVGA_CAP_ALPHA_CURSOR:    u32 = 0x0200;
const SVGA_CAP_3D:              u32 = 0x4000;
const SVGA_CAP_EXTENDED_FIFO:   u32 = 0x8000;
const SVGA_CAP_MULTIMON:        u32 = 0x00010000;
const SVGA_CAP_PITCHLOCK:       u32 = 0x00020000;
const SVGA_CAP_IRQMASK:         u32 = 0x00040000;

// ─── FIFO register indices (as u32 word offsets into FIFO MMIO region) ───────
const SVGA_FIFO_MIN:       usize = 0;   // byte offset of first FIFO command
const SVGA_FIFO_MAX:       usize = 1;   // byte offset of FIFO end (size used)
const SVGA_FIFO_NEXT_CMD:  usize = 2;   // next write position (producer)
const SVGA_FIFO_STOP:      usize = 3;   // last committed position (consumer)
// Extended FIFO (available when CAP_EXTENDED_FIFO):
const SVGA_FIFO_CAPABILITIES: usize = 4;
const SVGA_FIFO_FLAGS:        usize = 5;
const SVGA_FIFO_FENCE:        usize = 6;
const SVGA_FIFO_3D_HWVERSION: usize = 7;
const SVGA_FIFO_PITCHLOCK:    usize = 8;
// Number of registers at the start of FIFO
const SVGA_FIFO_NUM_REGS:     usize = 9; // extended
const SVGA_FIFO_NUM_REGS_STD: usize = 4; // standard

// ─── SVGA FIFO Commands ───────────────────────────────────────────────────────
const SVGA_CMD_UPDATE:              u32 = 1;
const SVGA_CMD_RECT_FILL:           u32 = 2;
const SVGA_CMD_RECT_COPY:           u32 = 3;
const SVGA_CMD_DEFINE_BITMAP:       u32 = 4;
const SVGA_CMD_DEFINE_BITMAP_SCANLINE: u32 = 5;
const SVGA_CMD_DEFINE_PIXMAP:       u32 = 6;
const SVGA_CMD_DEFINE_PIXMAP_SCANLINE: u32 = 7;
const SVGA_CMD_RECT_BITMAP_FILL:    u32 = 8;
const SVGA_CMD_RECT_PIXMAP_FILL:    u32 = 9;
const SVGA_CMD_RECT_BITMAP_COPY:    u32 = 10;
const SVGA_CMD_RECT_PIXMAP_COPY:    u32 = 11;
const SVGA_CMD_FREE_OBJECT:         u32 = 12;
const SVGA_CMD_RECT_ROP_FILL:       u32 = 13;
const SVGA_CMD_RECT_ROP_COPY:       u32 = 14;
const SVGA_CMD_RECT_ROP_BITMAP_FILL: u32 = 15;
const SVGA_CMD_RECT_ROP_PIXMAP_FILL: u32 = 16;
const SVGA_CMD_RECT_ROP_BITMAP_COPY: u32 = 17;
const SVGA_CMD_RECT_ROP_PIXMAP_COPY: u32 = 18;
const SVGA_CMD_DEFINE_CURSOR:       u32 = 19;
const SVGA_CMD_DISPLAY_CURSOR:      u32 = 20;
const SVGA_CMD_MOVE_CURSOR:         u32 = 21;
const SVGA_CMD_DEFINE_ALPHA_CURSOR: u32 = 22;

// ─── Driver state ─────────────────────────────────────────────────────────────
static SVGA_READY: AtomicBool = AtomicBool::new(false);

static mut IO_INDEX: u16 = 0;   // I/O port for register index
static mut IO_VALUE: u16 = 0;   // I/O port for register value
static mut FB_PHYS:  u64 = 0;   // Framebuffer physical address
static mut FB_SIZE:  u32 = 0;   // Framebuffer size in bytes
static mut FIFO_PHYS: u64 = 0;  // FIFO region physical address
static mut FIFO_SIZE: u32 = 0;  // FIFO region size in bytes
static mut CAPABILITIES: u32 = 0;
static mut CURRENT_WIDTH:  u32 = 0;
static mut CURRENT_HEIGHT: u32 = 0;
static mut CURRENT_BPP:    u32 = 0;

// ─── I/O helpers ──────────────────────────────────────────────────────────────

#[inline(always)]
unsafe fn outl(port: u16, val: u32) {
    core::arch::asm!("out dx, eax", in("dx") port, in("eax") val, options(nomem, nostack));
}

#[inline(always)]
unsafe fn inl(port: u16) -> u32 {
    let val: u32;
    core::arch::asm!("in eax, dx", in("dx") port, out("eax") val, options(nomem, nostack));
    val
}

// ─── SVGA register access ─────────────────────────────────────────────────────

unsafe fn svga_write(reg: u32, val: u32) {
    outl(IO_INDEX, reg);
    outl(IO_VALUE, val);
}

unsafe fn svga_read(reg: u32) -> u32 {
    outl(IO_INDEX, reg);
    inl(IO_VALUE)
}

// ─── FIFO access ──────────────────────────────────────────────────────────────

#[inline(always)]
unsafe fn fifo_ptr() -> *mut u32 {
    // FIFO is mapped via HHDM (Limine maps all physical memory in upper half)
    crate::mm::r#virtual::phys_to_virt(FIFO_PHYS) as *mut u32
}

unsafe fn fifo_read(idx: usize) -> u32 {
    core::ptr::read_volatile(fifo_ptr().add(idx))
}

unsafe fn fifo_write(idx: usize, val: u32) {
    core::ptr::write_volatile(fifo_ptr().add(idx), val);
}

/// Write a u32 command word into the FIFO ring.
unsafe fn fifo_push(val: u32) {
    let next_cmd = fifo_read(SVGA_FIFO_NEXT_CMD);
    let max      = fifo_read(SVGA_FIFO_MAX);
    let min      = fifo_read(SVGA_FIFO_MIN);

    // Wrap around
    let new_next = if next_cmd + 4 >= max { min } else { next_cmd + 4 };
    let stop     = fifo_read(SVGA_FIFO_STOP);

    // Block if FIFO is full (new_next would equal stop)
    // In practice on QEMU/VMware the FIFO is large so this rarely triggers
    let mut spins = 0u32;
    let mut cur_stop = stop;
    while new_next == cur_stop {
        svga_write(SVGA_REG_SYNC, 1);
        cur_stop = fifo_read(SVGA_FIFO_STOP);
        spins += 1;
        if spins > 10000 { return; } // bail on stuck FIFO
    }

    // Write value at current write pointer (byte offset → word index)
    let word_idx = (next_cmd / 4) as usize;
    fifo_write(word_idx, val);

    // Advance NEXT_CMD
    fifo_write(SVGA_FIFO_NEXT_CMD, new_next);
}

/// Flush FIFO to device
unsafe fn fifo_flush() {
    svga_write(SVGA_REG_SYNC, 1);
    // Poll BUSY until done (with timeout)
    let start = crate::arch::x86_64::idt::timer_ticks();
    while svga_read(SVGA_REG_BUSY) != 0 {
        if crate::arch::x86_64::idt::timer_ticks() - start > 50 {
            serial::write_str("[SVGA] FIFO flush timeout\r\n");
            break;
        }
        core::hint::spin_loop();
    }
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Returns true if SVGA II driver was successfully initialized.
pub fn is_ready() -> bool {
    SVGA_READY.load(Ordering::Relaxed)
}

/// Returns the physical address of the linear framebuffer, or 0 if not ready.
pub fn fb_base() -> u64 {
    unsafe { FB_PHYS }
}

/// Returns current display geometry (width, height, bytes_per_line).
pub fn display_info() -> (u32, u32, u32) {
    unsafe {
        let bpl = svga_read(SVGA_REG_BYTES_PER_LINE);
        (CURRENT_WIDTH, CURRENT_HEIGHT, bpl)
    }
}

/// Change display resolution. BPP must be 32 (BGRA) for Limine compatibility.
/// Returns false if adapter is not ready.
pub fn set_mode(width: u32, height: u32, bpp: u32) -> bool {
    if !is_ready() { return false; }
    unsafe {
        svga_write(SVGA_REG_ENABLE, 0);  // disable while reconfiguring
        svga_write(SVGA_REG_WIDTH,         width);
        svga_write(SVGA_REG_HEIGHT,        height);
        svga_write(SVGA_REG_BITS_PER_PIXEL, bpp);
        svga_write(SVGA_REG_ENABLE, 1);
        CURRENT_WIDTH  = width;
        CURRENT_HEIGHT = height;
        CURRENT_BPP    = bpp;

        serial::write_str("[SVGA] Mode set to ");
        serial_dec(width as u64);
        serial::write_byte(b'x');
        serial_dec(height as u64);
        serial::write_byte(b'x');
        serial_dec(bpp as u64);
        serial::write_str("\r\n");
    }
    true
}

/// Send an SVGA_CMD_UPDATE command to blit a dirty region to the screen.
pub fn update_rect(x: u32, y: u32, w: u32, h: u32) {
    if !is_ready() { return; }
    unsafe {
        fifo_push(SVGA_CMD_UPDATE);
        fifo_push(x);
        fifo_push(y);
        fifo_push(w);
        fifo_push(h);
        fifo_flush();
    }
}

// ─── FIFO state ───────────────────────────────────────────────────────────────

/// Whether the FIFO has been programmed (via `activate()`).
static FIFO_ACTIVE: AtomicBool = AtomicBool::new(false);

// ─── Initialization ───────────────────────────────────────────────────────────

/// Probe VMware SVGA II hardware (non-destructive).
///
/// Detects the PCI device, reads BARs, negotiates SVGA protocol version,
/// and stores all hardware parameters.  Does **not** write SVGA_REG_ENABLE,
/// FIFO registers, or CONFIG_DONE — the display that Limine set up keeps
/// working undisturbed.
///
/// Returns true if the adapter was found and identified.
pub fn init() -> bool {
    // 1. Find PCI device
    let dev = match crate::pci::find_by_vendor_device(VMWARE_VID, SVGA2_DID) {
        Some(d) => d,
        None => {
            // Try legacy SVGA I (0x0710) as fallback
            match crate::pci::find_by_vendor_device(VMWARE_VID, 0x0710) {
                Some(d) => d,
                None => {
                    serial::write_str("[SVGA] No VMware SVGA adapter found\r\n");
                    return false;
                }
            }
        }
    };

    let (bus, device, function) = (dev.bus, dev.device, dev.function);

    crate::pci::enable_bus_master(bus, device, function);

    // 2. Read I/O BAR0 (port base)
    let io_base = match crate::pci::read_bar(bus, device, function, 0) {
        crate::pci::BarType::Io { base, .. } => base as u16,
        _ => {
            serial::write_str("[SVGA] BAR0 is not I/O — cannot access SVGA registers\r\n");
            return false;
        }
    };

    unsafe {
        IO_INDEX = io_base;
        IO_VALUE = io_base + 1;
    }

    serial::write_str("[SVGA] I/O base=0x");
    serial_hex16(io_base);
    serial::write_str("\r\n");

    // 3. Negotiate SVGA ID (read-modify — does NOT change display state)
    unsafe {
        // Try SVGA_ID_2 first
        svga_write(SVGA_REG_ID, SVGA_ID_2);
        let readback = svga_read(SVGA_REG_ID);
        if readback != SVGA_ID_2 {
            // Try ID_1
            svga_write(SVGA_REG_ID, SVGA_ID_1);
            let rb1 = svga_read(SVGA_REG_ID);
            if rb1 != SVGA_ID_1 {
                serial::write_str("[SVGA] ID negotiation failed (read=0x");
                serial_hex32(readback);
                serial::write_str(")\r\n");
                return false;
            }
            serial::write_str("[SVGA] Using SVGA ID_1\r\n");
        } else {
            serial::write_str("[SVGA] SVGA ID_2 confirmed\r\n");
        }

        // 4. Read device capabilities
        CAPABILITIES = svga_read(SVGA_REG_CAPABILITIES);
        serial::write_str("[SVGA] Capabilities=0x");
        serial_hex32(CAPABILITIES);
        serial::write_str("\r\n");

        // 5. Read framebuffer physical address and size
        FB_PHYS  = svga_read(SVGA_REG_FB_START) as u64;
        FB_SIZE  = svga_read(SVGA_REG_VRAM_SIZE);

        // 6. Read FIFO region physical address and size
        FIFO_PHYS = svga_read(SVGA_REG_MEM_START) as u64;
        FIFO_SIZE = svga_read(SVGA_REG_MEM_SIZE);

        serial::write_str("[SVGA] FB phys=0x");
        serial_hex32(FB_PHYS as u32);
        serial::write_str(" FIFO phys=0x");
        serial_hex32(FIFO_PHYS as u32);
        serial::write_str("\r\n");

        // 7. Read current mode that Limine/firmware configured
        //    (read-only — we do NOT write ENABLE / CONFIG_DONE here)
        CURRENT_WIDTH  = svga_read(SVGA_REG_WIDTH);
        CURRENT_HEIGHT = svga_read(SVGA_REG_HEIGHT);
        CURRENT_BPP    = svga_read(SVGA_REG_BITS_PER_PIXEL);
    }

    SVGA_READY.store(true, Ordering::Release);

    serial::write_str("[SVGA] Detected — ");
    unsafe {
        serial_dec(CURRENT_WIDTH as u64);
        serial::write_byte(b'x');
        serial_dec(CURRENT_HEIGHT as u64);
        serial::write_byte(b'x');
        serial_dec(CURRENT_BPP as u64);
    }
    serial::write_str(" (Limine GOP active, FIFO deferred)\r\n");
    true
}

/// Fully activate SVGA II: program FIFO, enable device, redirect framebuffer.
///
/// This switches display output from the Limine GOP legacy path to the SVGA
/// command-driven path.  Call this when you need resolution switching or
/// hardware-accelerated 2D operations (rect copy/fill, cursor).
///
/// After activation, kernel framebuffer writes continue via `update_rect()`
/// dirty-region flushes — callers should periodically invoke `flush_screen()`
/// or `update_rect()` for visible updates.
///
/// Returns false if SVGA was not detected or activation failed.
pub fn activate() -> bool {
    if !is_ready() { return false; }
    if FIFO_ACTIVE.load(Ordering::Relaxed) { return true; } // already done

    unsafe {
        // Program FIFO
        let num_regs = if CAPABILITIES & SVGA_CAP_EXTENDED_FIFO != 0 {
            SVGA_FIFO_NUM_REGS
        } else {
            SVGA_FIFO_NUM_REGS_STD
        };
        let fifo_min = (num_regs * 4) as u32;
        let fifo_max = FIFO_SIZE.min(256 * 1024);

        fifo_write(SVGA_FIFO_MIN,      fifo_min);
        fifo_write(SVGA_FIFO_MAX,      fifo_max);
        fifo_write(SVGA_FIFO_NEXT_CMD, fifo_min);
        fifo_write(SVGA_FIFO_STOP,     fifo_min);

        // Enable SVGA + FIFO command path
        svga_write(SVGA_REG_GUEST_ID,    0x5010);
        svga_write(SVGA_REG_ENABLE,      1);
        svga_write(SVGA_REG_CONFIG_DONE, 1);

        // Re-read mode (enable may change geometry)
        CURRENT_WIDTH  = svga_read(SVGA_REG_WIDTH);
        CURRENT_HEIGHT = svga_read(SVGA_REG_HEIGHT);
        CURRENT_BPP    = svga_read(SVGA_REG_BITS_PER_PIXEL);

        // Redirect kernel framebuffer to SVGA buffer via HHDM
        let hhdm = crate::arch::x86_64::boot::hhdm_offset()
            .unwrap_or(0xFFFF_8000_0000_0000);
        let fb_virt = (FB_PHYS + hhdm) as *mut u32;
        let bpl = svga_read(SVGA_REG_BYTES_PER_LINE) as u64;
        crate::arch::x86_64::framebuffer::redirect(
            fb_virt,
            CURRENT_WIDTH  as u64,
            CURRENT_HEIGHT as u64,
            bpl,
        );
    }

    FIFO_ACTIVE.store(true, Ordering::Release);

    serial::write_str("[SVGA] Activated — FIFO + redirect live\r\n");
    // Blit full screen so previous content becomes visible immediately
    flush_screen();
    true
}

/// Flush the full screen (sends SVGA_CMD_UPDATE for entire display).
/// Only does anything after activate() has been called.
pub fn flush_screen() {
    if !FIFO_ACTIVE.load(Ordering::Relaxed) { return; }
    unsafe {
        update_rect(0, 0, CURRENT_WIDTH, CURRENT_HEIGHT);
    }
}

// ─── Serial helpers ───────────────────────────────────────────────────────────

fn serial_hex32(v: u32) {
    crate::pci::serial_hex16((v >> 16) as u16);
    crate::pci::serial_hex16(v as u16);
}

fn serial_hex16(v: u16) {
    crate::pci::serial_hex16(v);
}

fn serial_dec(mut val: u64) {
    if val == 0 { serial::write_byte(b'0'); return; }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while val > 0 { buf[i] = b'0' + (val % 10) as u8; val /= 10; i += 1; }
    for j in (0..i).rev() { serial::write_byte(buf[j]); }
}
