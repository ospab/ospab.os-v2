/*
 * VMware Backdoor Absolute Mouse Driver — AETERNA microkernel
 *
 * Provides absolute cursor coordinates via the VMware "backdoor" mechanism.
 * No PS/2 relative motion — direct absolute (x, y) matching screen pixels.
 *
 * Protocol:
 *   VMware exposes a backdoor I/O port at 0x5658 ("VMXh" = 0x564D5868).
 *   Sending a crafted IN instruction to this port with EAX = VMWARE_MAGIC
 *   and ECX = command number causes the hypervisor to intercept and respond.
 *
 *   Detection (GETVERSION, cmd=0x0A):
 *     IN: EAX=VMWARE_MAGIC, EBX=0, ECX=0x0A, EDX=0x5658
 *     ACK: EBX == VMWARE_MAGIC  ← confirms running in VMware
 *
 *   Enable absolute mode (ABSPOINTER_COMMAND, cmd=0x29):
 *     IN: EAX=VMWARE_MAGIC, EBX=VMMOUSE_CMD_ENABLE_ABS, ECX=0x29
 *
 *   Read position (ABSPOINTER_STATUS, cmd=0x27 then ABSPOINTER_DATA, cmd=0x28):
 *     Status: IN EBX → number of pending packet words (0 = no data)
 *     Data:   Each call to cmd=0x28 returns ONE u32 from the packet queue.
 *             A full mouse event is 4 words: [buttons, x, y, z]
 *
 * Packet format (word 0 = buttons, 1 = x, 2 = y, 3 = z/scroll):
 *   buttons: bit0=left, bit1=right, bit2=middle
 *   x, y:    absolute position (not scaled; use with screen dimensions)
 *   z:       scroll delta
 *
 * Reference: https://wiki.osdev.org/VMware_tools#The_Backdoor_I/O_port
 */

#![allow(dead_code)]

use core::sync::atomic::{AtomicBool, Ordering};
use crate::arch::x86_64::serial;

// ─── VMware backdoor constants ────────────────────────────────────────────────
const VMWARE_MAGIC:         u32 = 0x564D5868; // "VMXh"
const VMWARE_PORT:          u16 = 0x5658;
const VMWARE_PORT_HB:       u16 = 0x5659;     // High-bandwidth port (not used here)

// ─── Backdoor command numbers ─────────────────────────────────────────────────
const CMD_GETVERSION:       u32 = 0x0A;
const CMD_MESSAGE:          u32 = 0x1E;       // RPC messaging
const CMD_ABSPOINTER_STATUS:u32 = 0x27;       // query pending packet count
const CMD_ABSPOINTER_DATA:  u32 = 0x28;       // read next u32 from packet queue
const CMD_ABSPOINTER_COMMAND:u32 = 0x29;      // send mouse command

// ─── ABSPOINTER_COMMAND payloads ──────────────────────────────────────────────
const VMMOUSE_CMD_ENABLE_ABS:   u32 = 0x45414552; // "EARE" — enable absolute
const VMMOUSE_CMD_DISABLE_ABS:  u32 = 0x000000F5; // disable absolute
const VMMOUSE_CMD_REQUEST_RELATIVE: u32 = 0x4C455252; // "LERR"
const VMMOUSE_CMD_REQUEST_ABSOLUTE: u32 = 0x53424152; // "SBAR"

// ─── Packet button bits ───────────────────────────────────────────────────────
const BTN_LEFT:   u32 = 1 << 0;
const BTN_RIGHT:  u32 = 1 << 1;
const BTN_MIDDLE: u32 = 1 << 2;

// ─── Mouse state ──────────────────────────────────────────────────────────────
static VMMOUSE_ENABLED: AtomicBool = AtomicBool::new(false);

static mut LAST_X:       u32 = 0;
static mut LAST_Y:       u32 = 0;
static mut LAST_BUTTONS: u8  = 0;

/// A decoded absolute mouse event.
#[derive(Debug, Clone, Copy)]
pub struct MouseEvent {
    /// Absolute X position (raw VMware units, typically 0..65535)
    pub x: u32,
    /// Absolute Y position (raw VMware units, typically 0..65535)
    pub y: u32,
    /// Scroll delta (signed, positive = scroll up)
    pub scroll: i8,
    /// Button state: bit0=left, bit1=right, bit2=middle
    pub buttons: u8,
}

// ─── Backdoor invocation ──────────────────────────────────────────────────────

/// Execute a VMware backdoor command.
/// Returns (eax, ebx, ecx, edx) after the IN instruction.
/// NOTE: rbx cannot be used directly as an asm operand on x86_64 (LLVM restriction).
/// We manually save/restore rbx via push/pop.
#[inline(always)]
unsafe fn backdoor(cmd: u32, ebx_in: u32) -> (u32, u32, u32, u32) {
    let mut rax: u32 = VMWARE_MAGIC;
    let mut rcx: u32 = cmd;
    let mut rdx: u32 = VMWARE_PORT as u32;
    let rbx_out: u32;

    core::arch::asm!(
        // Manually save/restore rbx because LLVM uses it as a base pointer
        // and forbids using it as an asm operand directly.
        "push rbx",
        "mov ebx, {ebx_in:e}",
        "in eax, dx",
        "mov {rbx_out:e}, ebx",
        "pop rbx",
        ebx_in  = in(reg)  ebx_in,
        rbx_out = out(reg) rbx_out,
        inout("eax") rax,
        inout("ecx") rcx,
        inout("edx") rdx,
        // nostack REMOVED — push/pop modifies the stack
        options(nomem),
    );

    (rax, rbx_out, rcx, rdx)
}

// ─── Public API ───────────────────────────────────────────────────────────────

/// Returns true if running inside VMware and VMMouse was successfully enabled.
pub fn is_absolute() -> bool {
    VMMOUSE_ENABLED.load(Ordering::Relaxed)
}

/// Poll for a new mouse event.  Returns Some(event) if data is available.
/// Should be called in the keyboard/input poll loop.
pub fn poll() -> Option<MouseEvent> {
    if !VMMOUSE_ENABLED.load(Ordering::Relaxed) {
        return None;
    }

    unsafe {
        // Query how many u32 words are pending
        let (_ax, bx, _cx, _dx) = backdoor(CMD_ABSPOINTER_STATUS, 0);
        let count = bx & 0xFFFF;
        if count < 4 {
            return None; // Not a full packet yet
        }

        // Read 4 words: [buttons_flags, x, y, z]
        // EBX=1 per call: request exactly 1 word from the queue each time
        let (_ax, buttons_raw, _cx, _dx) = backdoor(CMD_ABSPOINTER_DATA, 1);
        let (_ax, x,           _cx, _dx) = backdoor(CMD_ABSPOINTER_DATA, 1);
        let (_ax, y,           _cx, _dx) = backdoor(CMD_ABSPOINTER_DATA, 1);
        let (_ax, z_raw,       _cx, _dx) = backdoor(CMD_ABSPOINTER_DATA, 1);

        let buttons = (buttons_raw & 0x07) as u8;
        let scroll  = (z_raw as i32) as i8;

        LAST_X       = x;
        LAST_Y       = y;
        LAST_BUTTONS = buttons;

        Some(MouseEvent { x, y, scroll, buttons })
    }
}

/// Return last known mouse position (cached from last poll).
pub fn last_pos() -> (u32, u32) {
    unsafe { (LAST_X, LAST_Y) }
}

/// Return last known button state.
pub fn last_buttons() -> u8 {
    unsafe { LAST_BUTTONS }
}

// ─── Initialization ───────────────────────────────────────────────────────────

/// Detect VMware and initialize the absolute pointing device.
/// Returns true if VMMouse was successfully enabled.
pub fn init() -> bool {
    unsafe {
        // 1. Detect VMware via GetVersion backdoor
        let (_ax, bx, _cx, _dx) = backdoor(CMD_GETVERSION, 0);
        if bx != VMWARE_MAGIC {
            serial::write_str("[VMMOUSE] Not running in VMware (GetVersion check failed)\r\n");
            return false;
        }
        serial::write_str("[VMMOUSE] VMware hypervisor detected\r\n");

        // 2. Enable absolute pointer mode
        let (_ax, bx, _cx, _dx) = backdoor(CMD_ABSPOINTER_COMMAND, VMMOUSE_CMD_REQUEST_ABSOLUTE);

        // 3. Verify it worked: read status — expect non-error response
        let (_ax, bx_status, _cx, _dx) = backdoor(CMD_ABSPOINTER_STATUS, 0);
        if bx_status == 0xFFFF0000 {
            serial::write_str("[VMMOUSE] Absolute mode request rejected by hypervisor\r\n");
            return false;
        }

        // 4. Also send CMD_ENABLE_ABS
        backdoor(CMD_ABSPOINTER_COMMAND, VMMOUSE_CMD_ENABLE_ABS);
    }

    VMMOUSE_ENABLED.store(true, Ordering::Release);
    serial::write_str("[VMMOUSE] Absolute mouse enabled — virtual cursor active\r\n");
    true
}
