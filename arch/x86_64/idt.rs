/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Interrupt Descriptor Table (IDT) and exception handlers for AETERNA microkernel.
Rewritten for proper PIC support and diagnostic output (tech-manifest п.4, п.8).
*/

use core::arch::asm;
use core::arch::naked_asm;
use bitflags::bitflags;

pub const IDT_ENTRIES: usize = 256;

#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    type_attr: u8,
    offset_mid: u16,
    offset_high: u32,
    zero: u32,
}

impl IdtEntry {
    pub const fn new() -> Self {
        Self {
            offset_low: 0,
            selector: 0,
            ist: 0,
            type_attr: 0,
            offset_mid: 0,
            offset_high: 0,
            zero: 0,
        }
    }

    pub fn set_handler(&mut self, handler: u64, selector: u16, flags: &IdtFlags) {
        self.offset_low = (handler & 0xFFFF) as u16;
        self.offset_mid = ((handler >> 16) & 0xFFFF) as u16;
        self.offset_high = ((handler >> 32) & 0xFFFFFFFF) as u32;
        self.selector = selector;
        self.type_attr = flags.bits();
        self.ist = 0;
    }

    pub fn set_handler_ist(&mut self, handler: u64, selector: u16, flags: &IdtFlags, ist: u8) {
        self.set_handler(handler, selector, flags);
        self.ist = ist & 0x07;
    }
}

bitflags! {
    pub struct IdtFlags: u8 {
        const PRESENT = 1 << 7;
        const INTERRUPT_GATE = 0x0E;
        const TRAP_GATE = 0x0F;
        const TASK_GATE = 0x05;
        const RING_0 = 0x00;
        const RING_1 = 0x20;
        const RING_2 = 0x40;
        const RING_3 = 0x60;
        const SIZE = 1 << 3;
    }
}

#[repr(C, packed)]
pub struct Idt {
    entries: [IdtEntry; IDT_ENTRIES],
}

impl Idt {
    pub fn new() -> Self {
        Self {
            entries: [IdtEntry::new(); IDT_ENTRIES],
        }
    }

    pub fn set_handler(&mut self, index: usize, handler: u64, selector: u16, flags: &IdtFlags) {
        if index < IDT_ENTRIES {
            self.entries[index].set_handler(handler, selector, flags);
        }
    }

    pub fn load(&'static self) {
        let ptr = IdtPointer {
            limit: (core::mem::size_of::<Self>() - 1) as u16,
            base: self as *const _ as u64,
        };
        
        unsafe {
            asm!("lidt [{}]", in(reg) &ptr, options(readonly, nostack, preserves_flags));
        }
    }
}

#[repr(C, packed)]
struct IdtPointer {
    limit: u16,
    base: u64,
}

// ============================================================================
// Exception stack frames
// ============================================================================

/// Stack frame pushed by CPU for exceptions without error code
#[derive(Debug)]
#[repr(C)]
pub struct ExceptionStackFrame {
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

/// Saved general-purpose registers (pushed by our handler stubs)
#[derive(Debug)]
#[repr(C)]
pub struct SavedRegisters {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
}

// ============================================================================
// Exception handler stubs (naked functions)
// ============================================================================

// Exception without error code: CPU pushes SS, RSP, RFLAGS, CS, RIP
macro_rules! exception_no_error {
    ($name:ident, $vector:expr) => {
        #[unsafe(naked)]
        pub extern "C" fn $name() {
            naked_asm!(
                "push 0",         // fake error code for uniform stack layout
                "push rax",
                "push rbx",
                "push rcx",
                "push rdx",
                "push rsi",
                "push rdi",
                "push rbp",
                "push r8",
                "push r9",
                "push r10",
                "push r11",
                "push r12",
                "push r13",
                "push r14",
                "push r15",
                "mov rdi, rsp",   // arg1: pointer to saved state
                "mov rsi, {}",    // arg2: vector number
                "call {}",
                "pop r15",
                "pop r14",
                "pop r13",
                "pop r12",
                "pop r11",
                "pop r10",
                "pop r9",
                "pop r8",
                "pop rbp",
                "pop rdi",
                "pop rsi",
                "pop rdx",
                "pop rcx",
                "pop rbx",
                "pop rax",
                "add rsp, 8",    // skip error code
                "iretq",
                const $vector,
                sym exception_dispatch,
            );
        }
    };
}

// Exception with error code: CPU pushes SS, RSP, RFLAGS, CS, RIP, ERROR_CODE
macro_rules! exception_with_error {
    ($name:ident, $vector:expr) => {
        #[unsafe(naked)]
        pub extern "C" fn $name() {
            naked_asm!(
                // error code already on stack from CPU
                "push rax",
                "push rbx",
                "push rcx",
                "push rdx",
                "push rsi",
                "push rdi",
                "push rbp",
                "push r8",
                "push r9",
                "push r10",
                "push r11",
                "push r12",
                "push r13",
                "push r14",
                "push r15",
                "mov rdi, rsp",
                "mov rsi, {}",
                "call {}",
                "pop r15",
                "pop r14",
                "pop r13",
                "pop r12",
                "pop r11",
                "pop r10",
                "pop r9",
                "pop r8",
                "pop rbp",
                "pop rdi",
                "pop rsi",
                "pop rdx",
                "pop rcx",
                "pop rbx",
                "pop rax",
                "add rsp, 8",   // skip error code
                "iretq",
                const $vector,
                sym exception_dispatch,
            );
        }
    };
}

// IRQ handler stub: no error code, sends EOI
macro_rules! irq_handler {
    ($name:ident, $irq:expr) => {
        #[unsafe(naked)]
        pub extern "C" fn $name() {
            naked_asm!(
                "push 0",
                "push rax",
                "push rbx",
                "push rcx",
                "push rdx",
                "push rsi",
                "push rdi",
                "push rbp",
                "push r8",
                "push r9",
                "push r10",
                "push r11",
                "push r12",
                "push r13",
                "push r14",
                "push r15",
                "mov rdi, rsp",
                "mov rsi, {}",
                "call {}",
                "pop r15",
                "pop r14",
                "pop r13",
                "pop r12",
                "pop r11",
                "pop r10",
                "pop r9",
                "pop r8",
                "pop rbp",
                "pop rdi",
                "pop rsi",
                "pop rdx",
                "pop rcx",
                "pop rbx",
                "pop rax",
                "add rsp, 8",
                "iretq",
                const $irq,
                sym irq_dispatch,
            );
        }
    };
}

// ============================================================================
// Exception handlers (vectors 0-31)
// ============================================================================

exception_no_error!(exc_divide_error, 0u64);
exception_no_error!(exc_debug, 1u64);
exception_no_error!(exc_nmi, 2u64);
exception_no_error!(exc_breakpoint, 3u64);
exception_no_error!(exc_overflow, 4u64);
exception_no_error!(exc_bound_range, 5u64);
exception_no_error!(exc_invalid_opcode, 6u64);
exception_no_error!(exc_device_not_available, 7u64);
exception_with_error!(exc_double_fault, 8u64);
exception_with_error!(exc_invalid_tss, 10u64);
exception_with_error!(exc_segment_not_present, 11u64);
exception_with_error!(exc_stack_segment, 12u64);
exception_with_error!(exc_general_protection, 13u64);
exception_with_error!(exc_page_fault, 14u64);
exception_no_error!(exc_x87_fpu, 16u64);
exception_with_error!(exc_alignment_check, 17u64);
exception_no_error!(exc_machine_check, 18u64);
exception_no_error!(exc_simd_fp, 19u64);
exception_no_error!(exc_virtualization, 20u64);
exception_with_error!(exc_control_protection, 21u64);
exception_no_error!(exc_hypervisor_injection, 28u64);
exception_with_error!(exc_vmm_communication, 29u64);
exception_with_error!(exc_security, 30u64);

// ============================================================================
// IRQ handlers (vectors 32-47 = IRQ 0-15)
// ============================================================================

irq_handler!(irq_timer, 0u64);
irq_handler!(irq_keyboard, 1u64);
irq_handler!(irq_cascade, 2u64);

// ── APIC one-shot timer: vector 48, NOT routed through PIC ──────────────────
/// Raw APIC timer ISR stub — saves GPRs, calls apic_timer_dispatch, ACKs APIC, restores, iretq.
#[unsafe(naked)]
pub extern "C" fn apic_timer_isr() {
    naked_asm!(
        "push 0",
        "push rax",
        "push rbx",
        "push rcx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push rbp",
        "push r8",
        "push r9",
        "push r10",
        "push r11",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "mov rdi, rsp",
        "call {}",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rbp",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rbx",
        "pop rax",
        "add rsp, 8",
        "iretq",
        sym apic_timer_dispatch,
    );
}

/// Dispatcher called from `apic_timer_isr`.
/// Re-arms a 1 ms one-shot, drives scheduler preemption, ACKs APIC.
extern "C" fn apic_timer_dispatch(saved_state: *mut u8) {
    // ACK the APIC *first* so the CPU can take the next interrupt.
    crate::arch::x86_64::apic::send_eoi();

    // Re-arm: fire again in 1 ms (gives 1 kHz preemption rate when idle,
    // but NIC IRQs will still land on separate PIC-routed vectors).
    crate::arch::x86_64::apic::one_shot_us(1_000);

    // Drive scheduler context switch — same interface as PIT handler.
    crate::core::scheduler::on_timer_irq(saved_state as *mut u8);
}
irq_handler!(irq_com2, 3u64);
irq_handler!(irq_com1, 4u64);
irq_handler!(irq_lpt2, 5u64);
irq_handler!(irq_floppy, 6u64);
irq_handler!(irq_lpt1, 7u64);
irq_handler!(irq_rtc, 8u64);
irq_handler!(irq_free1, 9u64);
irq_handler!(irq_free2, 10u64);
irq_handler!(irq_free3, 11u64);
irq_handler!(irq_mouse, 12u64);
irq_handler!(irq_fpu, 13u64);
irq_handler!(irq_primary_ata, 14u64);
irq_handler!(irq_secondary_ata, 15u64);

// ============================================================================
// Dispatch functions (called from assembly stubs)
// ============================================================================

/// Exception names for diagnostic output
static EXCEPTION_NAMES: [&str; 32] = [
    "Divide Error (#DE)",
    "Debug (#DB)",
    "NMI",
    "Breakpoint (#BP)",
    "Overflow (#OF)",
    "Bound Range (#BR)",
    "Invalid Opcode (#UD)",
    "Device Not Available (#NM)",
    "Double Fault (#DF)",
    "Coprocessor Segment Overrun",
    "Invalid TSS (#TS)",
    "Segment Not Present (#NP)",
    "Stack-Segment Fault (#SS)",
    "General Protection (#GP)",
    "Page Fault (#PF)",
    "Reserved",
    "x87 FPU Error (#MF)",
    "Alignment Check (#AC)",
    "Machine Check (#MC)",
    "SIMD FP Exception (#XM)",
    "Virtualization (#VE)",
    "Control Protection (#CP)",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Hypervisor Injection (#HV)",
    "VMM Communication (#VC)",
    "Security Exception (#SX)",
    "Reserved",
];

/// Main exception dispatcher - handles ring-3 faults gracefully, panics on ring-0.
/// Stack layout at entry: [r15..rax] [error_code] [rip] [cs] [rflags] [rsp] [ss]
/// Offsets (u64): r15=0..rax=14, error_code=15, RIP=16, CS=17, RFLAGS=18, RSP=19, SS=20
extern "C" fn exception_dispatch(saved_state: *mut u8, vector: u64) {
    let name = if (vector as usize) < EXCEPTION_NAMES.len() {
        EXCEPTION_NAMES[vector as usize]
    } else {
        "Unknown Exception"
    };

    // Extract fault context from saved stack frame
    let error_code: u64;
    let fault_rip: u64;
    let fault_rsp: u64;
    let fault_cs: u64;
    unsafe {
        let base = saved_state as *const u64;
        error_code = *base.add(15); // error code at offset 15
        fault_rip  = *base.add(16); // RIP at offset 16
        fault_cs   = *base.add(17); // CS  at offset 17
        fault_rsp  = *base.add(19); // RSP at offset 19
    }

    // ── Ring-3 process fault: kill process, yield to next ready task ──
    if fault_cs & 3 != 0 {
        let pid = crate::core::scheduler::current_task_id();

        // Page fault (#PF, vector 14): CR2 holds the faulting virtual address.
        // All other exceptions: use the faulting RIP as the address.
        let fault_addr = if vector == 14 {
            let cr2: u64;
            unsafe { core::arch::asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack)); }
            cr2
        } else {
            fault_rip
        };

        crate::arch::x86_64::serial::write_str("[CRITICAL] Process ");
        serial_dec(pid as u64);
        crate::arch::x86_64::serial::write_str(" ");
        crate::arch::x86_64::serial::write_str(name);
        crate::arch::x86_64::serial::write_str(" at 0x");
        serial_hex(fault_addr);
        crate::arch::x86_64::serial::write_str(". Terminating.\r\n");

        crate::core::scheduler::exit_pid(pid);

        // Patch saved_state so iretq resumes the next ready task instead of
        // returning into the now-dead process's faulting instruction.
        crate::core::scheduler::on_timer_irq(saved_state);
        return;
    }

    // ── Ring-0 exception: fatal kernel panic ──
    crate::arch::x86_64::serial::write_str("\r\n[FATAL] CPU Exception: ");
    crate::arch::x86_64::serial::write_str(name);
    crate::arch::x86_64::serial::write_str(" (vector ");
    serial_dec(vector);
    crate::arch::x86_64::serial::write_str(")\r\n");

    crate::arch::x86_64::serial::write_str("  Error code: 0x");
    serial_hex(error_code);
    crate::arch::x86_64::serial::write_str("\r\n");

    crate::arch::x86_64::serial::write_str("  Fault RIP:  0x");
    serial_hex(fault_rip);
    crate::arch::x86_64::serial::write_str("\r\n");

    crate::arch::x86_64::serial::write_str("  Fault RSP:  0x");
    serial_hex(fault_rsp);
    crate::arch::x86_64::serial::write_str("\r\n");

    panic!("CPU Exception");
}

/// IRQ dispatcher - handles hardware interrupts, sends EOI
extern "C" fn irq_dispatch(saved_state: *const u8, irq: u64) {
    match irq {
        0 => {
            // Timer interrupt - tick counter
            unsafe {
                TIMER_TICKS += 1;
            }
            crate::core::scheduler::on_timer_irq(saved_state as *mut u8);
        }
        1 => {
            // Keyboard interrupt - read scancode and buffer it
            unsafe {
                let scancode: u8;
                asm!("in al, dx", in("dx") 0x60u16, out("al") scancode, options(nomem, nostack));
                
                // Store in ring buffer for keyboard driver to pick up
                let write_idx = KB_BUFFER_WRITE & (KB_BUFFER_SIZE - 1);
                KB_BUFFER[write_idx] = scancode;
                KB_BUFFER_WRITE = KB_BUFFER_WRITE.wrapping_add(1);
            }
        }
        5 => {
            // AC97 Audio Controller IRQ (PCI IRQ line 5, default for QEMU AC97)
            crate::drivers::audio::handle_ac97_irq();
        }
        9 | 10 | 11 => {
            // Network IRQ — dispatch to the active NIC driver
            crate::net::handle_net_irq();
            unsafe { NET_IRQ_PENDING = true; }
        }
        14 => {
            // Primary ATA (IRQ 14) — MUST read status register to de-assert the IRQ line.
            // Without this, the drive keeps the line high → interrupt storm → CPU starved.
            unsafe {
                asm!("in al, dx", in("dx") 0x1F7u16, out("al") _, options(nomem, nostack));
            }
        }
        15 => {
            // Secondary ATA (IRQ 15) — same: read status to clear interrupt.
            unsafe {
                asm!("in al, dx", in("dx") 0x177u16, out("al") _, options(nomem, nostack));
            }
        }
        _ => {
            // Other IRQs - ignore for now
        }
    }

    // Send EOI to PIC
    crate::arch::x86_64::pic::send_eoi(irq as u8);
}

// ============================================================================
// Timer
// ============================================================================

static mut TIMER_TICKS: u64 = 0;

/// Get current timer tick count
pub fn timer_ticks() -> u64 {
    unsafe { TIMER_TICKS }
}

// ============================================================================
// Network IRQ flag
// ============================================================================

static mut NET_IRQ_PENDING: bool = false;

/// Check and clear network IRQ pending flag.
/// Returns `true` if at least one NIC IRQ arrived since the last call.
pub fn net_irq_pending() -> bool {
    unsafe {
        let p = NET_IRQ_PENDING;
        NET_IRQ_PENDING = false;
        p
    }
}

/// Alias consumed by the network wait loops — see `net::icmp::wait_reply()`.
#[inline(always)]
pub fn take_net_irq() -> bool { net_irq_pending() }

// ============================================================================
// Keyboard IRQ buffer
// ============================================================================

const KB_BUFFER_SIZE: usize = 256; // must be power of 2
static mut KB_BUFFER: [u8; KB_BUFFER_SIZE] = [0; KB_BUFFER_SIZE];
static mut KB_BUFFER_WRITE: usize = 0;
static mut KB_BUFFER_READ: usize = 0;

/// Read a scancode from the IRQ keyboard buffer (non-blocking)
pub fn kb_irq_read() -> Option<u8> {
    unsafe {
        if KB_BUFFER_READ == KB_BUFFER_WRITE {
            None
        } else {
            let idx = KB_BUFFER_READ & (KB_BUFFER_SIZE - 1);
            let sc = KB_BUFFER[idx];
            KB_BUFFER_READ = KB_BUFFER_READ.wrapping_add(1);
            Some(sc)
        }
    }
}

/// Write a scancode into the keyboard ring buffer (for USB HID injection).
/// Called by the XHCI driver to feed translated scancodes into the same
/// pipeline used by the PS/2 keyboard IRQ handler.
pub fn kb_buffer_write(scancode: u8) {
    unsafe {
        let write_idx = KB_BUFFER_WRITE & (KB_BUFFER_SIZE - 1);
        KB_BUFFER[write_idx] = scancode;
        KB_BUFFER_WRITE = KB_BUFFER_WRITE.wrapping_add(1);
    }
}

// ============================================================================
// Helper functions
// ============================================================================

fn serial_hex(val: u64) {
    const HEX: [u8; 16] = *b"0123456789ABCDEF";
    let mut buf = [0u8; 16];
    let mut v = val;
    for i in (0..16).rev() {
        buf[i] = HEX[(v & 0xF) as usize];
        v >>= 4;
    }
    for b in buf {
        crate::arch::x86_64::serial::write_byte(b);
    }
}

fn serial_dec(mut val: u64) {
    if val == 0 {
        crate::arch::x86_64::serial::write_byte(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while val > 0 {
        buf[i] = b'0' + (val % 10) as u8;
        val /= 10;
        i += 1;
    }
    for j in (0..i).rev() {
        crate::arch::x86_64::serial::write_byte(buf[j]);
    }
}

#[allow(dead_code)]
fn draw_hex_fb(val: u64) {
    const HEX: [u8; 16] = *b"0123456789ABCDEF";
    let mut v = val;
    let mut buf = [0u8; 16];
    for i in (0..16).rev() {
        buf[i] = HEX[(v & 0xF) as usize];
        v >>= 4;
    }
    for b in buf {
        crate::arch::x86_64::framebuffer::draw_char(b as char, 0x00FFFFFF, 0x00000000);
    }
}

// ============================================================================
// IDT initialization
// ============================================================================

pub fn init() {
    crate::arch::x86_64::serial::write_str("[AETERNA] IDT init...\r\n");
    
    static mut IDT: Option<Idt> = None;
    
    let flags = IdtFlags::PRESENT | IdtFlags::INTERRUPT_GATE | IdtFlags::RING_0;
    
    unsafe {
        IDT = Some(Idt::new());
        let idt = IDT.as_mut().unwrap();
        
        // Exception handlers (vectors 0-31)
        idt.set_handler(0, exc_divide_error as *const () as u64, 0x08, &flags);
        idt.set_handler(1, exc_debug as *const () as u64, 0x08, &flags);
        idt.set_handler(2, exc_nmi as *const () as u64, 0x08, &flags);
        idt.set_handler(3, exc_breakpoint as *const () as u64, 0x08, &flags);
        idt.set_handler(4, exc_overflow as *const () as u64, 0x08, &flags);
        idt.set_handler(5, exc_bound_range as *const () as u64, 0x08, &flags);
        idt.set_handler(6, exc_invalid_opcode as *const () as u64, 0x08, &flags);
        idt.set_handler(7, exc_device_not_available as *const () as u64, 0x08, &flags);
        idt.set_handler(8, exc_double_fault as *const () as u64, 0x08, &flags);
        idt.set_handler(10, exc_invalid_tss as *const () as u64, 0x08, &flags);
        idt.set_handler(11, exc_segment_not_present as *const () as u64, 0x08, &flags);
        idt.set_handler(12, exc_stack_segment as *const () as u64, 0x08, &flags);
        idt.set_handler(13, exc_general_protection as *const () as u64, 0x08, &flags);
        idt.set_handler(14, exc_page_fault as *const () as u64, 0x08, &flags);
        idt.set_handler(16, exc_x87_fpu as *const () as u64, 0x08, &flags);
        idt.set_handler(17, exc_alignment_check as *const () as u64, 0x08, &flags);
        idt.set_handler(18, exc_machine_check as *const () as u64, 0x08, &flags);
        idt.set_handler(19, exc_simd_fp as *const () as u64, 0x08, &flags);
        idt.set_handler(20, exc_virtualization as *const () as u64, 0x08, &flags);
        idt.set_handler(21, exc_control_protection as *const () as u64, 0x08, &flags);
        idt.set_handler(28, exc_hypervisor_injection as *const () as u64, 0x08, &flags);
        idt.set_handler(29, exc_vmm_communication as *const () as u64, 0x08, &flags);
        idt.set_handler(30, exc_security as *const () as u64, 0x08, &flags);
        
        // IRQ handlers (IRQ 0-15 -> IDT 32-47)
        idt.set_handler(32, irq_timer as *const () as u64, 0x08, &flags);
        idt.set_handler(33, irq_keyboard as *const () as u64, 0x08, &flags);
        idt.set_handler(34, irq_cascade as *const () as u64, 0x08, &flags);
        idt.set_handler(35, irq_com2 as *const () as u64, 0x08, &flags);
        idt.set_handler(36, irq_com1 as *const () as u64, 0x08, &flags);
        idt.set_handler(37, irq_lpt2 as *const () as u64, 0x08, &flags);
        idt.set_handler(38, irq_floppy as *const () as u64, 0x08, &flags);
        idt.set_handler(39, irq_lpt1 as *const () as u64, 0x08, &flags);
        idt.set_handler(40, irq_rtc as *const () as u64, 0x08, &flags);
        idt.set_handler(41, irq_free1 as *const () as u64, 0x08, &flags);
        idt.set_handler(42, irq_free2 as *const () as u64, 0x08, &flags);
        idt.set_handler(43, irq_free3 as *const () as u64, 0x08, &flags);
        idt.set_handler(44, irq_mouse as *const () as u64, 0x08, &flags);
        idt.set_handler(45, irq_fpu as *const () as u64, 0x08, &flags);
        idt.set_handler(46, irq_primary_ata as *const () as u64, 0x08, &flags);
        idt.set_handler(47, irq_secondary_ata as *const () as u64, 0x08, &flags);

        // APIC one-shot timer (vector 48) — set here; only fires after apic::init_timer()
        idt.set_handler(48, apic_timer_isr as *const () as u64, 0x08, &flags);

        // Load IDT
        idt.load();
    }
    
    crate::arch::x86_64::serial::write_str("[AETERNA] IDT loaded (exceptions 0-31, IRQ 0-15)\r\n");
}