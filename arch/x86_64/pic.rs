/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
8259 PIC (Programmable Interrupt Controller) driver for AETERNA microkernel.
Remaps IRQ 0-15 to IDT vectors 32-47.
*/

use core::arch::asm;

// PIC ports
const PIC1_COMMAND: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_COMMAND: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

// ICW1 flags
const ICW1_INIT: u8 = 0x10;
const ICW1_ICW4: u8 = 0x01;

// ICW4 flags
const ICW4_8086: u8 = 0x01;

// OCW2 - End of Interrupt
const PIC_EOI: u8 = 0x20;

// IRQ vector offsets
pub const PIC1_OFFSET: u8 = 32;
pub const PIC2_OFFSET: u8 = 40;

/// Write a byte to a port
#[inline(always)]
unsafe fn outb(port: u16, value: u8) {
    asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
}

/// Read a byte from a port
#[inline(always)]
unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack));
    value
}

/// Small I/O delay (wait for PIC to process)
#[inline(always)]
unsafe fn io_wait() {
    // Port 0x80 is used for POST codes, writing to it causes a small delay
    outb(0x80, 0);
}

/// Initialize both 8259 PICs with standard IRQ remapping.
/// Master PIC: IRQ 0-7 -> IDT 32-39
/// Slave PIC:  IRQ 8-15 -> IDT 40-47
pub fn init() {
    unsafe {
        // Save masks
        let _mask1 = inb(PIC1_DATA);
        let _mask2 = inb(PIC2_DATA);

        // ICW1: start initialization sequence (cascade mode, ICW4 needed)
        outb(PIC1_COMMAND, ICW1_INIT | ICW1_ICW4);
        io_wait();
        outb(PIC2_COMMAND, ICW1_INIT | ICW1_ICW4);
        io_wait();

        // ICW2: vector offsets
        outb(PIC1_DATA, PIC1_OFFSET);
        io_wait();
        outb(PIC2_DATA, PIC2_OFFSET);
        io_wait();

        // ICW3: tell PICs about each other
        outb(PIC1_DATA, 0x04); // Master: slave on IRQ2 (bit 2)
        io_wait();
        outb(PIC2_DATA, 0x02); // Slave: cascade identity = 2
        io_wait();

        // ICW4: 8086 mode
        outb(PIC1_DATA, ICW4_8086);
        io_wait();
        outb(PIC2_DATA, ICW4_8086);
        io_wait();

        // Mask all IRQs except timer (IRQ0), keyboard (IRQ1), and cascade (IRQ2)
        // CASCADE (IRQ2) MUST be unmasked for any slave PIC IRQs (8-15) to work!
        // 0xF8 = 11111000: bits 0,1,2 clear = IRQ 0,1,2 enabled
        outb(PIC1_DATA, 0xF8); // Enable IRQ0 (timer), IRQ1 (keyboard), IRQ2 (cascade)
        io_wait();
        outb(PIC2_DATA, 0xFF); // Mask all slave IRQs for now (enabled individually later)
        io_wait();
    }

    crate::arch::x86_64::serial::write_str("[AETERNA] PIC initialized (IRQ 0-15 -> IDT 32-47)\r\n");
}

/// Send End-of-Interrupt to the appropriate PIC(s)
pub fn send_eoi(irq: u8) {
    unsafe {
        if irq >= 8 {
            // IRQ came from slave PIC, send EOI to both
            outb(PIC2_COMMAND, PIC_EOI);
        }
        outb(PIC1_COMMAND, PIC_EOI);
    }
}

/// Enable a specific IRQ line.
/// For slave IRQs (8-15), also ensures the cascade (IRQ2) on the master is enabled.
pub fn enable_irq(irq: u8) {
    unsafe {
        if irq < 8 {
            let mask = inb(PIC1_DATA);
            outb(PIC1_DATA, mask & !(1 << irq));
        } else {
            // Ensure cascade IRQ2 is enabled on the master PIC
            let master_mask = inb(PIC1_DATA);
            if master_mask & (1 << 2) != 0 {
                outb(PIC1_DATA, master_mask & !(1 << 2));
            }
            // Enable the specific slave IRQ
            let mask = inb(PIC2_DATA);
            outb(PIC2_DATA, mask & !(1 << (irq - 8)));
        }
    }
}

/// Disable a specific IRQ line
pub fn disable_irq(irq: u8) {
    unsafe {
        if irq < 8 {
            let mask = inb(PIC1_DATA);
            outb(PIC1_DATA, mask | (1 << irq));
        } else {
            let mask = inb(PIC2_DATA);
            outb(PIC2_DATA, mask | (1 << (irq - 8)));
        }
    }
}

/// Disable both PICs (mask all IRQs)
pub fn disable() {
    unsafe {
        outb(PIC1_DATA, 0xFF);
        outb(PIC2_DATA, 0xFF);
    }
}

/// Read the IRR (Interrupt Request Register) - pending IRQs
pub fn read_irr() -> u16 {
    unsafe {
        outb(PIC1_COMMAND, 0x0A);
        outb(PIC2_COMMAND, 0x0A);
        let lo = inb(PIC1_COMMAND) as u16;
        let hi = inb(PIC2_COMMAND) as u16;
        (hi << 8) | lo
    }
}

/// Read the ISR (In-Service Register) - currently being serviced
pub fn read_isr() -> u16 {
    unsafe {
        outb(PIC1_COMMAND, 0x0B);
        outb(PIC2_COMMAND, 0x0B);
        let lo = inb(PIC1_COMMAND) as u16;
        let hi = inb(PIC2_COMMAND) as u16;
        (hi << 8) | lo
    }
}

/// Program PIT channel 0 for 100 Hz preemptive timer interrupts.
/// Call this after pic::init() and before enable_interrupts().
///
/// Divisor = 1_193_182 / 100 = 11_931 (0x2E9B)
/// Command byte 0x36 = channel 0, lobyte/hibyte access, mode 3 (square wave), binary.
pub fn init_pit_100hz() {
    const DIVISOR: u16 = 11931; // 1_193_182 / 100
    unsafe {
        outb(0x43, 0x36);                            // channel 0, lobyte/hibyte, mode 3
        outb(0x40, (DIVISOR & 0xFF) as u8);          // divisor low byte
        outb(0x40, ((DIVISOR >> 8) & 0xFF) as u8);   // divisor high byte
    }
    crate::arch::x86_64::serial::write_str("[PIT] Channel 0 → 100 Hz (divisor=11931)\r\n");
}
