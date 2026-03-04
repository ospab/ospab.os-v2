/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

DOOM bare-metal port for AETERNA.

Implementation:
  - rust_malloc/realloc/free: bridge to the kernel global allocator
  - rust_serial_print: bridge to arch serial output
  - rust_get_ticks_ms / rust_doom_sleep_ms: PIT timer (10 ms/tick at 100 Hz)
  - rust_doom_get_key: PS/2 scancode → DOOM key event queue
  - rust_doom_blit: 640×400 ARGB pixel buffer → centered BGRA framebuffer blit
  - run(): entry point called by the `doom` terminal command

WAD file:
  - ospab_libc.c intercepts fopen("doom1.wad") and serves doom1_wad_data/size.
  - If no WAD is present (doom1_wad_size == 0), DOOM prints an error and exits.
*/

#![allow(dead_code)]
#![allow(unused_unsafe)]

extern crate alloc;

use core::alloc::Layout;
use alloc::alloc::{alloc, dealloc};

// ─── WAD embedding ──────────────────────────────────────────────────────────

/// WAD data exported to C (ospab_libc.c intercepts fopen("doom1.wad"))
#[cfg(doom_wad_present)]
#[no_mangle]
pub static doom1_wad_data: &[u8] = include_bytes!("../doom1.wad");

#[cfg(doom_wad_present)]
#[no_mangle]
pub static doom1_wad_size: usize = include_bytes!("../doom1.wad").len();

// Fallback when WAD is absent – zero-length placeholder so DOOM refuses to start gracefully
#[cfg(not(doom_wad_present))]
#[no_mangle]
pub static doom1_wad_data: [u8; 1] = [0u8];

#[cfg(not(doom_wad_present))]
#[no_mangle]
pub static doom1_wad_size: usize = 0usize;

// ─── Allocation header ───────────────────────────────────────────────────────

/// Magic value stamped before every allocation to detect over/underflows
const ALLOC_MAGIC: u32 = 0xA110CA7E;

/// Header prepended to every allocation
#[repr(C)]
struct AllocHeader {
    magic: u32,
    size: usize,
}

const HEADER_SIZE: usize = core::mem::size_of::<AllocHeader>();

// ─── Memory allocator (exported to DOOM C engine) ───────────────────────────

/// malloc — called by DOOM C code
#[no_mangle]
pub unsafe extern "C" fn rust_malloc(size: usize) -> *mut u8 {
    if size == 0 { return core::ptr::null_mut(); }
    let total = size + HEADER_SIZE;
    let layout = match Layout::from_size_align(total, 8) {
        Ok(l) => l,
        Err(_) => return core::ptr::null_mut(),
    };
    let ptr = alloc(layout);
    if ptr.is_null() { return core::ptr::null_mut(); }
    let hdr = ptr as *mut AllocHeader;
    (*hdr).magic = ALLOC_MAGIC;
    (*hdr).size = size;
    ptr.add(HEADER_SIZE)
}

/// free — called by DOOM C code
#[no_mangle]
pub unsafe extern "C" fn rust_free(ptr: *mut u8) {
    if ptr.is_null() { return; }
    let raw = ptr.sub(HEADER_SIZE);
    let hdr = raw as *mut AllocHeader;
    if (*hdr).magic != ALLOC_MAGIC { return; } // corrupted / double-free guard
    let size = (*hdr).size;
    let total = size + HEADER_SIZE;
    let layout = match Layout::from_size_align(total, 8) {
        Ok(l) => l,
        Err(_) => return,
    };
    (*hdr).magic = 0; // invalidate
    dealloc(raw, layout);
}

/// calloc — called by DOOM C code
#[no_mangle]
pub unsafe extern "C" fn rust_calloc(count: usize, size: usize) -> *mut u8 {
    let total = count.saturating_mul(size);
    let ptr = rust_malloc(total);
    if !ptr.is_null() {
        core::ptr::write_bytes(ptr, 0, total);
    }
    ptr
}

/// realloc — called by DOOM C code
#[no_mangle]
pub unsafe extern "C" fn rust_realloc(ptr: *mut u8, new_size: usize) -> *mut u8 {
    if ptr.is_null() { return rust_malloc(new_size); }
    if new_size == 0 { rust_free(ptr); return core::ptr::null_mut(); }

    let raw = ptr.sub(HEADER_SIZE);
    let hdr = raw as *mut AllocHeader;
    if (*hdr).magic != ALLOC_MAGIC { return core::ptr::null_mut(); }
    let old_size = (*hdr).size;

    let new_ptr = rust_malloc(new_size);
    if new_ptr.is_null() { return core::ptr::null_mut(); }
    let copy_len = old_size.min(new_size);
    core::ptr::copy_nonoverlapping(ptr, new_ptr, copy_len);
    rust_free(ptr);
    new_ptr
}

// ─── Serial output (exported to DOOM C engine) ──────────────────────────────

/// Print a byte buffer to serial COM1
#[no_mangle]
pub unsafe extern "C" fn rust_serial_print(s: *const u8, len: usize) {
    if s.is_null() { return; }
    let slice = core::slice::from_raw_parts(s, len);
    if let Ok(text) = core::str::from_utf8(slice) {
        crate::arch::x86_64::serial::write_str(text);
    } else {
        // Non-UTF8: print byte by byte as hex (debug mode only)
        for &b in slice {
            let hex = [b"0123456789abcdef"[(b >> 4) as usize], b"0123456789abcdef"[(b & 0xf) as usize]];
            if let Ok(s) = core::str::from_utf8(&hex) {
                crate::arch::x86_64::serial::write_str(s);
            }
        }
    }
}

// ─── Timer (exported to DOOM C engine) ──────────────────────────────────────

/// PIT fires at 100 Hz → 10 ms per tick.
const MS_PER_TICK: u64 = 10;

/// Return milliseconds since kernel boot (using PIT tick counter)
#[no_mangle]
pub extern "C" fn rust_get_ticks_ms() -> u32 {
    let ticks = crate::arch::x86_64::idt::timer_ticks();
    (ticks * MS_PER_TICK) as u32
}

/// Spin-delay for `ms` milliseconds (HLT-based to avoid busy-spinning hard)
#[no_mangle]
pub extern "C" fn rust_doom_sleep_ms(ms: u32) {
    if ms == 0 { return; }
    let start = rust_get_ticks_ms();
    loop {
        let now = rust_get_ticks_ms();
        if now.wrapping_sub(start) >= ms { break; }
        unsafe { core::arch::asm!("hlt"); }
    }
}

// ─── Keyboard event queue ───────────────────────────────────────────────────

/// DOOM key event: (pressed, doom_key_code)
#[derive(Copy, Clone)]
struct KeyEvent {
    pressed: bool,
    key: u8,
}

const KEY_QUEUE_SIZE: usize = 64;
static mut KEY_QUEUE: [KeyEvent; KEY_QUEUE_SIZE] = [KeyEvent { pressed: false, key: 0 }; KEY_QUEUE_SIZE];
static mut KEY_QUEUE_HEAD: usize = 0;
static mut KEY_QUEUE_TAIL: usize = 0;

fn key_queue_push(ev: KeyEvent) {
    unsafe {
        let next = (KEY_QUEUE_TAIL + 1) % KEY_QUEUE_SIZE;
        if next != KEY_QUEUE_HEAD {
            KEY_QUEUE[KEY_QUEUE_TAIL] = ev;
            KEY_QUEUE_TAIL = next;
        }
    }
}

fn key_queue_pop() -> Option<KeyEvent> {
    unsafe {
        if KEY_QUEUE_HEAD == KEY_QUEUE_TAIL { return None; }
        let ev = KEY_QUEUE[KEY_QUEUE_HEAD];
        KEY_QUEUE_HEAD = (KEY_QUEUE_HEAD + 1) % KEY_QUEUE_SIZE;
        Some(ev)
    }
}

// ─── Scancode → DOOM key translation ────────────────────────────────────────

/// Translate a PS/2 set-1 make/break scancode to a DOOM key code.
/// `make` = true for key press, false for key release.
/// Returns None if the scancode is not interesting to DOOM.
fn scancode_to_doom(sc: u8) -> Option<(bool, u8)> {
    // Break codes = make code | 0x80
    let make = (sc & 0x80) == 0;
    let code = sc & 0x7F;

    // DOOM key constants (from doomkeys.h)
    const KEY_USE: u8       = 0xa2;
    const KEY_FIRE: u8      = 0xa3;
    const KEY_STRAFE_L: u8  = 0xa0;
    const KEY_STRAFE_R: u8  = 0xa1;

    let doom_key: u8 = match code {
        0x01 => 27,           // ESC → KEY_ESCAPE
        0x0F => 9,            // Tab → KEY_TAB
        0x1C => 13,           // Enter → KEY_ENTER
        0x39 => KEY_USE,      // Space → KEY_USE (open doors, switches)
        0x0E => 0x7F,         // Backspace → KEY_BACKSPACE

        // Arrow keys (standard scancodes, not extended E0 prefix here)
        0x48 => 0xAD,         // Up    → KEY_UPARROW
        0x50 => 0xAF,         // Down  → KEY_DOWNARROW
        0x4B => 0xAC,         // Left  → KEY_LEFTARROW
        0x4D => 0xAE,         // Right → KEY_RIGHTARROW

        // Function keys
        0x3B => 0x80 | 0x3B, // F1  → KEY_F1
        0x3C => 0x80 | 0x3C, // F2  → KEY_F2
        0x3D => 0x80 | 0x3D, // F3  → KEY_F3
        0x3E => 0x80 | 0x3E, // F4  → KEY_F4
        0x3F => 0x80 | 0x3F, // F5  → KEY_F5
        0x40 => 0x80 | 0x40, // F6  → KEY_F6
        0x41 => 0x80 | 0x41, // F7  → KEY_F7
        0x42 => 0x80 | 0x42, // F8  → KEY_F8
        0x43 => 0x80 | 0x43, // F9  → KEY_F9
        0x44 => 0x80 | 0x44, // F10 → KEY_F10

        // Modifier keys — mapped to DOOM action keys
        0x1D => KEY_FIRE,     // Left Ctrl  → KEY_FIRE (shoot)
        0x2A => 0x80 | 0x36, // Left Shift → KEY_RSHIFT (run)
        0x36 => 0x80 | 0x36, // Right Shift → KEY_RSHIFT
        0x38 => 0x80 | 0x38, // Left Alt   → KEY_RALT (strafe modifier)

        // Navigation
        0x47 => 0x80 | 0x47, // Home
        0x4F => 0x80 | 0x4F, // End
        0x49 => 0x80 | 0x49, // PgUp
        0x51 => 0x80 | 0x51, // PgDn
        0x52 => 0x80 | 0x52, // Insert
        0x53 => 0x80 | 0x53, // Delete

        // Number keys (weapon select 1-7)
        0x02 => b'1', 0x03 => b'2', 0x04 => b'3', 0x05 => b'4',
        0x06 => b'5', 0x07 => b'6', 0x08 => b'7', 0x09 => b'8',
        0x0A => b'9', 0x0B => b'0',
        0x0C => b'-', 0x0D => b'=',

        // Letter keys (DOOM uses lowercase)
        0x10 => b'q', 0x11 => b'w', 0x12 => b'e', 0x13 => b'r',
        0x14 => b't', 0x15 => b'y', 0x16 => b'u', 0x17 => b'i',
        0x18 => b'o', 0x19 => b'p',

        0x1E => b'a', 0x1F => b's', 0x20 => b'd', 0x21 => b'f',
        0x22 => b'g', 0x23 => b'h', 0x24 => b'j', 0x25 => b'k',
        0x26 => b'l',

        0x2C => b'z', 0x2D => b'x', 0x2E => b'c', 0x2F => b'v',
        0x30 => b'b', 0x31 => b'n', 0x32 => b'm',

        // Comma / Period → strafe left/right (classic DOOM binding)
        0x33 => KEY_STRAFE_L, // , → strafe left
        0x34 => KEY_STRAFE_R, // . → strafe right
        0x35 => b'/',
        0x27 => b';', 0x28 => b'\'',
        0x29 => b'`', 0x2B => b'\\',
        0x1A => b'[', 0x1B => b']',

        _ => return None,
    };

    Some((make, doom_key))
}

/// Poll PS/2 buffer and populate the key event queue.
/// Called once per DOOM frame before DG_GetKey drain.
fn poll_keyboard() {
    // Read up to 32 pending scancodes per frame
    let mut extended = false;
    for _ in 0..32 {
        let sc = match crate::arch::x86_64::idt::kb_irq_read() {
            Some(s) => s,
            None => break,
        };

        if sc == 0xE0 {
            extended = true; // next byte is extended (arrow keys etc.)
            continue;
        }

        if extended {
            extended = false;
            // Extended key: remap to standard codes our table expects
            let remapped: u8 = match sc & 0x7F {
                0x48 => 0x48, // E0 48 = Up arrow
                0x50 => 0x50, // E0 50 = Down arrow
                0x4B => 0x4B, // E0 4B = Left arrow
                0x4D => 0x4D, // E0 4D = Right arrow
                0x47 => 0x47, // E0 47 = Home
                0x4F => 0x4F, // E0 4F = End
                0x49 => 0x49, // E0 49 = PgUp
                0x51 => 0x51, // E0 51 = PgDn
                0x52 => 0x52, // E0 52 = Insert
                0x53 => 0x53, // E0 53 = Delete
                _ => continue,
            };
            // Right Ctrl (E0 1D) and Right Alt (E0 38) treated specially
            let ext_code = sc & 0x7F;
            if ext_code == 0x1D {
                // Right Ctrl → KEY_FIRE
                let pressed = (sc & 0x80) == 0;
                key_queue_push(KeyEvent { pressed, key: 0xa3 });
                continue;
            }
            if ext_code == 0x38 {
                // Right Alt → KEY_RALT (strafe modifier)
                let pressed = (sc & 0x80) == 0;
                key_queue_push(KeyEvent { pressed, key: 0x80 | 0x38 });
                continue;
            }
            let make_flag = if (sc & 0x80) == 0 { remapped } else { remapped | 0x80 };
            if let Some((make, doom_key)) = scancode_to_doom(make_flag) {
                key_queue_push(KeyEvent { pressed: make, key: doom_key });
            }
            continue;
        }

        if let Some((make, doom_key)) = scancode_to_doom(sc) {
            key_queue_push(KeyEvent { pressed: make, key: doom_key });
        }
    }
}

/// Called by doomgeneric to get the next key event.
/// Returns 1 if an event was available, 0 otherwise.
#[no_mangle]
pub extern "C" fn rust_doom_get_key(pressed: *mut i32, key: *mut u8) -> i32 {
    poll_keyboard();
    if let Some(ev) = key_queue_pop() {
        unsafe {
            if !pressed.is_null() { *pressed = if ev.pressed { 1 } else { 0 }; }
            if !key.is_null() { *key = ev.key; }
        }
        1
    } else {
        0
    }
}

// ─── Framebuffer blit (exported to DOOM C engine) ───────────────────────────

/// Blit DOOM's 640×400 ARGB pixel buffer to the UEFI framebuffer.
/// Blits DOOM's framebuffer to the kernel framebuffer with correct colour
/// handling and integer up-scaling.  DOOM emits 0x00RRGGBB; we remap to the
/// actual channel shifts reported by the Limine bootloader.
#[no_mangle]
pub unsafe extern "C" fn rust_doom_blit(pixels: *const u32, width: i32, height: i32) {
    use crate::arch::x86_64::framebuffer;

    if pixels.is_null() { return; }

    let fb = match framebuffer::info() {
        Some(f) => f,
        None => return,
    };

    let fb_w = fb.width as i32;
    let fb_h = fb.height as i32;
    let src_w = width;
    let src_h = height;

    // Integer scale that fits the screen (at least 1x)
    let scale_x = if fb_w / src_w >= 1 { fb_w / src_w } else { 1 };
    let scale_y = if fb_h / src_h >= 1 { fb_h / src_h } else { 1 };
    let scale = if scale_x < scale_y { scale_x } else { scale_y };

    let dst_w = src_w * scale;
    let dst_h = src_h * scale;
    let off_x = (fb_w - dst_w) / 2;
    let off_y = (fb_h - dst_h) / 2;

    let rs = fb.red_shift;
    let gs = fb.green_shift;
    let bs = fb.blue_shift;

    // Direct pointer for fast row writes
    let fb_base = fb.address;
    let pitch_px = (fb.pitch / 4) as i32; // pitch in u32 pixels

    for y in 0..src_h {
        for x in 0..src_w {
            // DOOM pixel: 0x00RRGGBB
            let px = *pixels.add((y * src_w + x) as usize);
            let r = (px >> 16) & 0xFF;
            let g = (px >>  8) & 0xFF;
            let b = (px      ) & 0xFF;
            let colour = (r << rs) | (g << gs) | (b << bs);

            // Write scaled block
            for sy in 0..scale {
                let dy = off_y + y * scale + sy;
                if dy < 0 || dy >= fb_h { continue; }
                let row = fb_base.offset((dy * pitch_px) as isize);
                for sx in 0..scale {
                    let dx = off_x + x * scale + sx;
                    if dx < 0 || dx >= fb_w { continue; }
                    *row.offset(dx as isize) = colour;
                }
            }
        }
    }
}

// ─── Exit callback ──────────────────────────────────────────────────────────

static mut DOOM_EXIT_REQUESTED: bool = false;

/// Called by DOOM C code to request graceful exit
#[no_mangle]
pub extern "C" fn rust_doom_exit(code: i32) {
    unsafe { DOOM_EXIT_REQUESTED = true; }
    let _ = code;
}

// ─── VFS bridge for DOOM save/config files ──────────────────────────────────
// C functions: rust_vfs_open, rust_vfs_read, rust_vfs_write, rust_vfs_close
// Returns fd (≥0) or -1 on error.

/// Check whether a VFS path exists.  Returns 0 on success, -1 if not found.
/// Exposed as rust_vfs_access() — DOOM uses this to test for save-file presence.
#[no_mangle]
pub extern "C" fn rust_vfs_access(path_ptr: *const u8, path_len: usize) -> i32 {
    if path_ptr.is_null() || path_len == 0 { return -1; }
    let path = unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(path_ptr, path_len)) };
    let found = crate::fs::exists(path);
    // Debug: log every .dsg probe so we can see what DOOM is checking
    if path.ends_with(".dsg") || path.contains("doom") {
        crate::arch::x86_64::serial::write_str("[VFS] Access: ");
        crate::arch::x86_64::serial::write_str(path);
        crate::arch::x86_64::serial::write_str(if found { " -> FOUND\r\n" } else { " -> NOT FOUND\r\n" });
    }
    if found { 0 } else { -1 }
}

// ─── Directory listing state for rust_vfs_opendir / readdir_next / closedir ──
// DOOM opens /doom once per Load-Game menu open; we cache the listing in RAM.

struct DirListing {
    entries: alloc::vec::Vec<alloc::string::String>,
    pos: usize,
}

/// Single-slot directory iterator (DOOM never opens two dirs at once)
static mut DIR_LISTING: Option<DirListing> = None;
/// Unique handle token returned to C callers
const DIR_HANDLE_TOKEN: i64 = 0x4449_5200; // "DIR\0"

/// Open a directory for iteration.  Returns DIR_HANDLE_TOKEN on success or -1.
#[no_mangle]
pub extern "C" fn rust_vfs_opendir(path_ptr: *const u8, path_len: usize) -> i64 {
    if path_ptr.is_null() || path_len == 0 { return -1; }
    let path = unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(path_ptr, path_len)) };
    crate::arch::x86_64::serial::write_str("[VFS] Opendir: ");
    crate::arch::x86_64::serial::write_str(path);
    crate::arch::x86_64::serial::write_str("\r\n");
    match crate::fs::readdir(path) {
        Some(entries) => {
            let names: alloc::vec::Vec<alloc::string::String> = entries.into_iter().map(|e| e.name).collect();
            crate::arch::x86_64::serial::write_str("[VFS] Opendir: found ");
            // log count
            let count = names.len();
            let mut nbuf = [0u8; 8];
            let s = crate::format_u64(&mut nbuf, count as u64);
            crate::arch::x86_64::serial::write_str(s);
            crate::arch::x86_64::serial::write_str(" entries\r\n");
            unsafe { DIR_LISTING = Some(DirListing { entries: names, pos: 0 }); }
            DIR_HANDLE_TOKEN
        }
        None => {
            crate::arch::x86_64::serial::write_str("[VFS] Opendir: NOT FOUND\r\n");
            -1
        }
    }
}

/// Read the next directory entry name into out_buf (null-terminated).
/// Returns 1 if an entry was written, 0 if end-of-directory, -1 on error.
#[no_mangle]
pub extern "C" fn rust_vfs_readdir_next(
    handle: i64,
    out_buf: *mut u8,
    out_max: usize,
) -> i32 {
    if handle != DIR_HANDLE_TOKEN || out_buf.is_null() || out_max == 0 { return -1; }
    unsafe {
        let listing = match DIR_LISTING.as_mut() {
            Some(l) => l,
            None => return -1,
        };
        if listing.pos >= listing.entries.len() {
            return 0; // end of directory
        }
        let name = &listing.entries[listing.pos];
        listing.pos += 1;
        let bytes = name.as_bytes();
        let copy_len = bytes.len().min(out_max - 1);
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), out_buf, copy_len);
        *out_buf.add(copy_len) = 0; // null terminator
        1
    }
}

/// Close a directory handle opened with rust_vfs_opendir.
#[no_mangle]
pub extern "C" fn rust_vfs_closedir(handle: i64) -> i64 {
    if handle != DIR_HANDLE_TOKEN { return -1; }
    unsafe { DIR_LISTING = None; }
    0
}

/// Open a file in the VFS.  flags: 0=read, 1=write (truncate), 2=read+write
#[no_mangle]
pub extern "C" fn rust_vfs_open(path_ptr: *const u8, path_len: usize, flags: u64) -> i64 {
    if path_ptr.is_null() || path_len == 0 { return -1; }
    let path = unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(path_ptr, path_len)) };

    // Debug logging for save files
    if path.ends_with(".dsg") || path.contains("doom") {
        crate::arch::x86_64::serial::write_str(if flags == 1 { "[VFS] Create: " } else { "[VFS] Open: " });
        crate::arch::x86_64::serial::write_str(path);
        crate::arch::x86_64::serial::write_str("\r\n");
    }

    if flags == 1 {
        // Write-only (e.g. "wb"): always truncate so stale bytes from a
        // previous, larger save don't corrupt the new one.
        crate::fs::write_file(path, &[]);
    } else if flags & 1 != 0 {
        // Read+write: create if absent, but don't truncate.
        if crate::fs::read_file(path).is_none() {
            crate::fs::write_file(path, &[]);
        }
    }

    let fd = crate::fs::sys_open(path, flags);
    // Confirm open result for save files
    if path.ends_with(".dsg") || path.contains("doom") {
        if fd >= 0 {
            crate::arch::x86_64::serial::write_str("[VFS] Open OK, fd=");
            let mut nbuf = [0u8; 8];
            let s = crate::format_u64(&mut nbuf, fd as u64);
            crate::arch::x86_64::serial::write_str(s);
            crate::arch::x86_64::serial::write_str("\r\n");
        } else {
            crate::arch::x86_64::serial::write_str("[VFS] Open FAILED\r\n");
        }
    }
    fd
}

/// Read up to `len` bytes from fd into buf. Returns bytes read or -1.
#[no_mangle]
pub extern "C" fn rust_vfs_read(fd: usize, buf: *mut u8, len: usize) -> i64 {
    if buf.is_null() || len == 0 { return 0; }
    let slice = unsafe { core::slice::from_raw_parts_mut(buf, len) };
    crate::fs::sys_read(fd, slice)
}

/// Write `len` bytes from buf to fd. Returns bytes written or -1.
#[no_mangle]
pub extern "C" fn rust_vfs_write(fd: usize, buf: *const u8, len: usize) -> i64 {
    if buf.is_null() || len == 0 { return 0; }
    let slice = unsafe { core::slice::from_raw_parts(buf, len) };
    crate::fs::sys_write(fd, slice)
}

/// Close an fd. Triggers auto-sync to disk if dirty.
#[no_mangle]
pub extern "C" fn rust_vfs_close(fd: usize) -> i64 {
    crate::fs::sys_close(fd)
}

/// Seek in a VFS fd. whence: 0=SET, 1=CUR, 2=END. Returns new pos or -1.
#[no_mangle]
pub extern "C" fn rust_vfs_seek(fd: usize, offset: i64, whence: i32) -> i64 {
    crate::fs::sys_seek(fd, offset, whence)
}

/// Get size of file at path. Returns -1 if not found.
#[no_mangle]
pub extern "C" fn rust_vfs_file_size(path_ptr: *const u8, path_len: usize) -> i64 {
    if path_ptr.is_null() || path_len == 0 { return -1; }
    let path = unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(path_ptr, path_len)) };
    match crate::fs::read_file(path) {
        Some(data) => data.len() as i64,
        None => -1,
    }
}

/// Rename (move) a file in the VFS.  Returns 0 on success, -1 on error.
/// Implemented as read-old → write-new → remove-old (RamFS has no native rename).
#[no_mangle]
pub extern "C" fn rust_vfs_rename(
    old_ptr: *const u8, old_len: usize,
    new_ptr: *const u8, new_len: usize,
) -> i32 {
    if old_ptr.is_null() || new_ptr.is_null() || old_len == 0 || new_len == 0 { return -1; }
    let old = unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(old_ptr, old_len)) };
    let new = unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(new_ptr, new_len)) };

    // Debug log
    crate::arch::x86_64::serial::write_str("[VFS] Rename: ");
    crate::arch::x86_64::serial::write_str(old);
    crate::arch::x86_64::serial::write_str(" -> ");
    crate::arch::x86_64::serial::write_str(new);
    crate::arch::x86_64::serial::write_str("\r\n");

    // Read source file
    let data = match crate::fs::read_file(old) {
        Some(d) => d,
        None => {
            crate::arch::x86_64::serial::write_str("[VFS] Rename FAILED: src not found\r\n");
            return -1;
        }
    };
    // Write to destination
    if !crate::fs::write_file(new, &data) {
        crate::arch::x86_64::serial::write_str("[VFS] Rename FAILED: write dst\r\n");
        return -1;
    }
    // Remove source
    crate::fs::remove(old);
    0
}

// ─── DOOM C engine declarations ─────────────────────────────────────────────

#[cfg(doom_supported)]
extern "C" {
    /// Safe wrapper: calls doomgeneric_Create inside setjmp; returns 0 on
    /// success, 1 if DOOM's exit() fired during init.
    fn doom_create_safe(argc: i32, argv: *mut *const u8) -> i32;
    /// Safe wrapper: calls doomgeneric_Tick inside setjmp; returns 0 on
    /// normal return, 1 if exit() fired (longjmp escape).
    fn doom_tick_safe() -> i32;
}

// ─── Entry point ────────────────────────────────────────────────────────────

/// Run the DOOM game loop. Called by the `doom` terminal command.
/// Blocks until the user exits (ESC → F10 to quit, or game triggers exit).
pub fn run() {
    use crate::arch::x86_64::{framebuffer, serial};

    serial::write_str("[DOOM] Starting engine\r\n");

    // Check WAD availability
    let wad_available = unsafe { doom1_wad_size > 0 };
    if !wad_available {
        crate::arch::x86_64::framebuffer::draw_string(
            "DOOM: doom1.wad not found.\n\
             Place doom1.wad in the project root and rebuild.\n\
             Shareware WAD available at: https://doomworld.com/classicdoom/\n",
            0x00FF4444, 0x00000000,
        );
        serial::write_str("[DOOM] WAD not present — aborting\r\n");
        return;
    }

    #[cfg(doom_supported)]
    {
        serial::write_str("[DOOM] WAD present, calling doom_create_safe\r\n");

        // Close any zombie FDs left open by a previous DOOM session that
        // exited via longjmp (bypassing fclose).  Without this the FD table
        // fills up after a few restarts and fopen() starts returning NULL,
        // making saves appear to vanish.
        crate::fs::close_all_vfs_fds();

        // Ensure /doom/ directory exists for save files
        crate::fs::mkdir("/doom");

        // Clear screen to black before DOOM takes over
        framebuffer::clear(0x00000000);

        unsafe {
            DOOM_EXIT_REQUESTED = false;
            KEY_QUEUE_HEAD = 0;
            KEY_QUEUE_TAIL = 0;

            // Pass -iwad doom1.wad so DOOM doesn't try doom2.wad / plutonia / etc. first
            let arg0 = b"ospab\0".as_ptr();
            let arg1 = b"-iwad\0".as_ptr();
            let arg2 = b"doom1.wad\0".as_ptr();
            let mut argv: [*const u8; 4] = [arg0, arg1, arg2, core::ptr::null()];

            if doom_create_safe(3, argv.as_mut_ptr()) != 0 {
                serial::write_str("[DOOM] exit() during Create — aborting\r\n");
            } else {
                serial::write_str("[DOOM] Entering game loop\r\n");

                loop {
                    if DOOM_EXIT_REQUESTED { break; }
                    if doom_tick_safe() != 0 { break; }
                    if DOOM_EXIT_REQUESTED { break; }
                }
            }
        }

        serial::write_str("[DOOM] Engine exited\r\n");

        // Force a final disk sync so the last-written save state survives
        // an OS reboot. Deferred tick fires ~10 s after the last write, but
        // we do one unconditional flush here to guarantee consistency at exit.
        if crate::fs::disk_sync::is_dirty() {
            serial::write_str("[DOOM] Final disk sync...\r\n");
            crate::fs::disk_sync::sync_filesystem();
            serial::write_str("[DOOM] Sync done\r\n");
        } else {
            serial::write_str("[DOOM] FS clean — no sync needed\r\n");
        }

        // Restore framebuffer text mode cursor position
        framebuffer::clear(0x00000000);
        framebuffer::set_cursor_pos(0, 0);
    }

    #[cfg(not(doom_supported))]
    {
        framebuffer::draw_string(
            "DOOM engine not compiled.\n\
             Install clang and run: bash doom_engine/build_doom.sh target/doom\n\
             Then rebuild with: bash build.sh\n",
            0x00FFAA44, 0x00000000,
        );
        serial::write_str("[DOOM] C engine not compiled (no libdoom.a)\r\n");
    }
}

// ─── HDA audio bridge (exported to DOOM C engine) ────────────────────────────

/// Push raw PCM data into the HDA ring buffer.
/// Called by the DOOM sound module on every game tick.
/// `data` must point to interleaved stereo 16-bit LE @ 44100 Hz.
#[no_mangle]
pub unsafe extern "C" fn rust_hda_write_pcm(data: *const u8, len: u32) {
    if data.is_null() || len == 0 { return; }
    let slice = core::slice::from_raw_parts(data, len as usize);
    crate::drivers::audio::write_pcm(slice);
}

/// Returns 1 if the HDA audio driver is initialized and streaming, 0 otherwise.
#[no_mangle]
pub extern "C" fn rust_hda_is_ready() -> i32 {
    if crate::drivers::audio::is_ready() { 1 } else { 0 }
}
