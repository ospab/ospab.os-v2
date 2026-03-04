/*
 * AETERNA XHCI (USB 3.0) Host Controller Driver
 *
 * Minimal XHCI driver focused on USB keyboard support for bare metal.
 *
 * Architecture:
 *   1. PCI scan for XHCI controller (class 0x0C, subclass 0x03, progif 0x30)
 *   2. MMIO BAR0 mapping via HHDM
 *   3. Controller reset and initialization
 *   4. Device Context Base Address Array (DCBAA)
 *   5. Command Ring, Event Ring, Transfer Ring setup
 *   6. Port detection and USB device enumeration
 *   7. HID keyboard endpoint discovery and interrupt transfers
 *   8. Scancode translation → keyboard ring buffer integration
 *
 * The driver feeds decoded keypresses into the same ring buffer used by
 * the PS/2 keyboard driver (arch/x86_64/idt.rs KB_BUFFER), so the
 * existing keyboard::poll_key()/try_read_key() API works transparently.
 */

// WIP driver — many constants and structs are reserved for future endpoint types.
#![allow(dead_code, unused_assignments, unused_parens)]

use core::arch::asm;
use core::ptr;
use crate::arch::x86_64::serial;
use crate::arch::x86_64::boot;

// ══════════════════════════════════════════════════════════════════════════════
// XHCI MMIO Register Sets
// ══════════════════════════════════════════════════════════════════════════════

/// XHCI Capability Registers (offset 0x00 from BAR0)
#[repr(C)]
struct XhciCaps {
    caplength:   u8,        // 0x00: Capability register length
    _rsvd:       u8,        // 0x01
    hci_version: u16,       // 0x02: Interface version (BCD)
    hcsparams1:  u32,       // 0x04: Structural params 1
    hcsparams2:  u32,       // 0x08: Structural params 2
    hcsparams3:  u32,       // 0x0C: Structural params 3
    hccparams1:  u32,       // 0x10: Capability params 1
    dboff:       u32,       // 0x14: Doorbell offset
    rtsoff:      u32,       // 0x18: Runtime register space offset
    hccparams2:  u32,       // 0x1C: Capability params 2
}

/// XHCI Operational Registers (offset caplength from BAR0)
#[repr(C)]
struct XhciOps {
    usbcmd:      u32,       // 0x00: USB Command
    usbsts:      u32,       // 0x04: USB Status
    pagesize:    u32,       // 0x08: Page Size
    _rsvd1:      [u32; 2],  // 0x0C-0x13
    dnctrl:      u32,       // 0x14: Device Notification Control
    crcr_lo:     u32,       // 0x18: Command Ring Control (low)
    crcr_hi:     u32,       // 0x1C: Command Ring Control (high)
    _rsvd2:      [u32; 4],  // 0x20-0x2F
    dcbaap_lo:   u32,       // 0x30: DCBA Pointer (low)
    dcbaap_hi:   u32,       // 0x34: DCBA Pointer (high)
    config:      u32,       // 0x38: Configure
}

/// XHCI Port Register Set (one per port, within Operational space)
#[repr(C)]
struct XhciPort {
    portsc:  u32,           // Port Status and Control
    portpmsc: u32,          // Port Power Management Status and Control
    portli:  u32,           // Port Link Info
    porthlpmc: u32,         // Port Hardware LPM Control
}

// USBCMD bits
const CMD_RUN:   u32 = 1 << 0;     // Run/Stop
const CMD_HCRST: u32 = 1 << 1;     // Host Controller Reset
const CMD_INTE:  u32 = 1 << 2;     // Interrupter Enable
const CMD_HSEE:  u32 = 1 << 3;     // Host System Error Enable

// USBSTS bits
const STS_HCH:   u32 = 1 << 0;     // HC Halted
const STS_HSE:   u32 = 1 << 2;     // Host System Error
const STS_CNR:   u32 = 1 << 11;    // Controller Not Ready

// PORTSC bits
const PORT_CCS:  u32 = 1 << 0;     // Current Connect Status
const PORT_PED:  u32 = 1 << 1;     // Port Enabled/Disabled
const PORT_PR:   u32 = 1 << 4;     // Port Reset
const PORT_PLS_MASK: u32 = 0xF << 5; // Port Link State
const PORT_PP:   u32 = 1 << 9;     // Port Power
const PORT_CSC:  u32 = 1 << 17;    // Connect Status Change
const PORT_PRC:  u32 = 1 << 21;    // Port Reset Change
const PORT_WRC:  u32 = 1 << 19;    // Warm Port Reset Change

// ══════════════════════════════════════════════════════════════════════════════
// Transfer Request Block (TRB) — 16 bytes
// ══════════════════════════════════════════════════════════════════════════════

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct Trb {
    param_lo: u32,
    param_hi: u32,
    status:   u32,
    control:  u32,
}

impl Trb {
    const fn zero() -> Self {
        Self { param_lo: 0, param_hi: 0, status: 0, control: 0 }
    }
}

// TRB types (in control field bits 15:10)
const TRB_TYPE_SHIFT: u32 = 10;
const TRB_NORMAL: u32              = 1 << TRB_TYPE_SHIFT;
const TRB_SETUP_STAGE: u32         = 2 << TRB_TYPE_SHIFT;
const TRB_DATA_STAGE: u32          = 3 << TRB_TYPE_SHIFT;
const TRB_STATUS_STAGE: u32        = 4 << TRB_TYPE_SHIFT;
const TRB_LINK: u32                = 6 << TRB_TYPE_SHIFT;
const TRB_NO_OP_CMD: u32           = 23 << TRB_TYPE_SHIFT;
const TRB_ENABLE_SLOT: u32         = 9 << TRB_TYPE_SHIFT;
const TRB_ADDRESS_DEVICE: u32      = 11 << TRB_TYPE_SHIFT;
const TRB_CONFIGURE_ENDPOINT: u32  = 12 << TRB_TYPE_SHIFT;
const TRB_EVALUATE_CONTEXT: u32    = 13 << TRB_TYPE_SHIFT;
const TRB_CMD_COMPLETION: u32      = 33 << TRB_TYPE_SHIFT;
const TRB_TRANSFER_EVENT: u32      = 32 << TRB_TYPE_SHIFT;
const TRB_PORT_STATUS_CHANGE: u32  = 34 << TRB_TYPE_SHIFT;

// TRB control bits
const TRB_CYCLE: u32   = 1 << 0;
const TRB_IOC: u32     = 1 << 5;   // Interrupt On Completion
const TRB_IDT: u32     = 1 << 6;   // Immediate Data
const TRB_BSR: u32     = 1 << 9;   // Block Set Address Request (Address Device)

// ══════════════════════════════════════════════════════════════════════════════
// Event Ring Segment Table Entry
// ══════════════════════════════════════════════════════════════════════════════

#[repr(C, align(64))]
#[derive(Clone, Copy)]
struct EventRingSegEntry {
    base_lo:  u32,
    base_hi:  u32,
    size:     u32,
    _rsvd:    u32,
}

// ══════════════════════════════════════════════════════════════════════════════
// Device Context / Input Context structures
// ══════════════════════════════════════════════════════════════════════════════

/// Slot Context (32 bytes)
#[repr(C, align(32))]
#[derive(Clone, Copy)]
struct SlotContext {
    fields: [u32; 8],
}

impl SlotContext {
    const fn zero() -> Self { Self { fields: [0; 8] } }
}

/// Endpoint Context (32 bytes)
#[repr(C, align(32))]
#[derive(Clone, Copy)]
struct EndpointContext {
    fields: [u32; 8],
}

impl EndpointContext {
    const fn zero() -> Self { Self { fields: [0; 8] } }
}

/// Device Context = Slot + 31 Endpoint Contexts (1024 bytes total for 32-byte contexts)
#[repr(C, align(64))]
struct DeviceContext {
    slot: SlotContext,
    endpoints: [EndpointContext; 31],
}

/// Input Context = Input Control Context + Device Context
#[repr(C, align(64))]
struct InputContext {
    control: [u32; 8],    // Input Control Context (drop/add flags)
    slot: SlotContext,
    endpoints: [EndpointContext; 31],
}

// ══════════════════════════════════════════════════════════════════════════════
// Static allocations (in BSS — zeroed at boot)
// ══════════════════════════════════════════════════════════════════════════════

const CMD_RING_SIZE:   usize = 64;   // Command Ring TRBs
const EVENT_RING_SIZE: usize = 64;   // Event Ring TRBs
const XFER_RING_SIZE:  usize = 64;   // Transfer Ring TRBs
const MAX_PORTS:       usize = 16;
const MAX_SLOTS:       usize = 8;

#[repr(C, align(4096))]
struct XhciStatic {
    // Command Ring
    cmd_ring:     [Trb; CMD_RING_SIZE],
    cmd_enqueue:  usize,
    cmd_cycle:    u32,

    // Event Ring (interrupter 0)
    event_ring:   [Trb; EVENT_RING_SIZE],
    event_dequeue: usize,
    event_cycle:  u32,
    event_seg_table: [EventRingSegEntry; 1],

    // Transfer Ring for keyboard endpoint
    xfer_ring:    [Trb; XFER_RING_SIZE],
    xfer_enqueue: usize,
    xfer_cycle:   u32,

    // DCBAA (Device Context Base Address Array) — must be 64-byte aligned
    dcbaa:        [u64; MAX_SLOTS + 1],

    // Device Contexts
    dev_contexts: [DeviceContextBlock; MAX_SLOTS],

    // Input Context (reusable)
    input_ctx:    InputContext,

    // Keyboard report buffer (8 bytes for boot protocol)
    kb_report:    [u8; 8],

    // Controller state
    bar_virt:     u64,     // BAR0 virtual address (via HHDM)
    cap_length:   u8,      // Capability registers length
    max_ports:    u8,
    max_slots:    u8,
    max_intrs:    u16,
    initialized:  bool,
    kb_slot:      u8,      // Slot ID of USB keyboard (0 = not found)
    kb_endpoint:  u8,      // Endpoint DCI for keyboard interrupt IN
    kb_port:      u8,      // Root port of keyboard device
    context_size: u8,      // 32 or 64 bytes per context entry
}

/// Per-slot device context storage
#[repr(C, align(4096))]
#[derive(Clone, Copy)]
struct DeviceContextBlock {
    data: [u8; 2048],
}

static mut XHCI: XhciStatic = XhciStatic {
    cmd_ring:     [Trb::zero(); CMD_RING_SIZE],
    cmd_enqueue:  0,
    cmd_cycle:    1,
    event_ring:   [Trb::zero(); EVENT_RING_SIZE],
    event_dequeue: 0,
    event_cycle:  1,
    event_seg_table: [EventRingSegEntry { base_lo: 0, base_hi: 0, size: 0, _rsvd: 0 }; 1],
    xfer_ring:    [Trb::zero(); XFER_RING_SIZE],
    xfer_enqueue: 0,
    xfer_cycle:   1,
    dcbaa:        [0u64; MAX_SLOTS + 1],
    dev_contexts: [DeviceContextBlock { data: [0u8; 2048] }; MAX_SLOTS],
    input_ctx:    InputContext {
        control: [0; 8],
        slot: SlotContext::zero(),
        endpoints: [EndpointContext::zero(); 31],
    },
    kb_report:    [0u8; 8],
    bar_virt:     0,
    cap_length:   0,
    max_ports:    0,
    max_slots:    0,
    max_intrs:    0,
    initialized:  false,
    kb_slot:      0,
    kb_endpoint:  0,
    kb_port:      0,
    context_size: 32,
};

// ══════════════════════════════════════════════════════════════════════════════
// MMIO helpers
// ══════════════════════════════════════════════════════════════════════════════

unsafe fn mmio_read32(addr: u64) -> u32 {
    ptr::read_volatile(addr as *const u32)
}

unsafe fn mmio_write32(addr: u64, val: u32) {
    ptr::write_volatile(addr as *mut u32, val);
}

unsafe fn mmio_read64(addr: u64) -> u64 {
    // Some hardware doesn't support atomic 64-bit reads, use two 32-bit reads
    let lo = ptr::read_volatile(addr as *const u32) as u64;
    let hi = ptr::read_volatile((addr + 4) as *const u32) as u64;
    (hi << 32) | lo
}

unsafe fn mmio_write64(addr: u64, val: u64) {
    ptr::write_volatile(addr as *mut u32, val as u32);
    ptr::write_volatile((addr + 4) as *mut u32, (val >> 32) as u32);
}

fn phys_to_virt(phys: u64) -> u64 {
    let hhdm = boot::hhdm_offset().unwrap_or(0);
    phys + hhdm
}

fn virt_to_phys(virt: u64) -> u64 {
    let hhdm = boot::hhdm_offset().unwrap_or(0);
    virt.wrapping_sub(hhdm)
}

/// Spin-wait with timeout. Returns true if condition was met.
fn wait_for(timeout_us: u64, mut cond: impl FnMut() -> bool) -> bool {
    for _ in 0..timeout_us {
        if cond() { return true; }
        // ~1µs delay on modern CPUs
        for _ in 0..100 { unsafe { asm!("pause"); } }
    }
    false
}

// ══════════════════════════════════════════════════════════════════════════════
// Register access helpers
// ══════════════════════════════════════════════════════════════════════════════

unsafe fn ops_base() -> u64 {
    XHCI.bar_virt + XHCI.cap_length as u64
}

unsafe fn port_base(port: u8) -> u64 {
    ops_base() + 0x400 + (port as u64) * 16
}

unsafe fn runtime_base() -> u64 {
    let rtsoff = mmio_read32(XHCI.bar_virt + 0x18);
    XHCI.bar_virt + (rtsoff & 0xFFFFFFE0) as u64
}

unsafe fn doorbell(slot: u8) -> u64 {
    let dboff = mmio_read32(XHCI.bar_virt + 0x14);
    XHCI.bar_virt + (dboff & 0xFFFFFFFC) as u64 + (slot as u64) * 4
}

// ══════════════════════════════════════════════════════════════════════════════
// Initialization
// ══════════════════════════════════════════════════════════════════════════════

/// Initialize XHCI USB controller.
/// Returns true if a controller was found and initialized.
pub fn init() -> bool {
    serial::write_str("[XHCI] Scanning for USB 3.0 controller...\r\n");

    // Find XHCI controller via PCI: class 0x0C (Serial Bus), subclass 0x03 (USB), progif 0x30 (XHCI)
    let dev = match crate::pci::find_by_class(0x0C, 0x03, 0x30) {
        Some(d) => *d,
        None => {
            serial::write_str("[XHCI] No XHCI controller found\r\n");
            return false;
        }
    };

    serial::write_str("[XHCI] Found controller at PCI ");
    serial_dec(dev.bus as u64);
    serial::write_str(":");
    serial_dec(dev.device as u64);
    serial::write_str(".");
    serial_dec(dev.function as u64);
    serial::write_str(" (");
    serial_hex16(dev.vendor_id);
    serial::write_str(":");
    serial_hex16(dev.device_id);
    serial::write_str(")\r\n");

    // Enable bus mastering and memory space
    crate::pci::enable_bus_master(dev.bus, dev.device, dev.function);

    // Read BAR0 (MMIO base)
    let bar = crate::pci::read_bar(dev.bus, dev.device, dev.function, 0);
    let bar_phys = match bar {
        crate::pci::BarType::Mmio { base, size, .. } => {
            serial::write_str("[XHCI] BAR0: 0x");
            serial_hex(base);
            serial::write_str(" size=0x");
            serial_hex(size);
            serial::write_str("\r\n");
            base
        }
        _ => {
            serial::write_str("[XHCI] BAR0 is not MMIO!\r\n");
            return false;
        }
    };

    let bar_virt = phys_to_virt(bar_phys);

    unsafe {
        XHCI.bar_virt = bar_virt;

        // Read capability registers
        XHCI.cap_length = mmio_read32(bar_virt) as u8;
        let hci_ver = (mmio_read32(bar_virt) >> 16) as u16;
        let hcsparams1 = mmio_read32(bar_virt + 0x04);
        let hccparams1 = mmio_read32(bar_virt + 0x10);

        XHCI.max_slots = (hcsparams1 & 0xFF) as u8;
        XHCI.max_intrs = ((hcsparams1 >> 8) & 0x7FF) as u16;
        XHCI.max_ports = ((hcsparams1 >> 24) & 0xFF) as u8;

        // Context Size: bit 2 of HCCPARAMS1 (CSZ) — 0=32 bytes, 1=64 bytes
        XHCI.context_size = if hccparams1 & 0x04 != 0 { 64 } else { 32 };

        serial::write_str("[XHCI] HCI version: ");
        serial_hex16(hci_ver);
        serial::write_str(", Ports: ");
        serial_dec(XHCI.max_ports as u64);
        serial::write_str(", Slots: ");
        serial_dec(XHCI.max_slots as u64);
        serial::write_str(", CtxSize: ");
        serial_dec(XHCI.context_size as u64);
        serial::write_str("\r\n");

        // ── Step 1: Halt controller ──
        let ops = ops_base();
        let cmd = mmio_read32(ops);
        if cmd & CMD_RUN != 0 {
            mmio_write32(ops, cmd & !CMD_RUN);
            if !wait_for(20000, || mmio_read32(ops + 4) & STS_HCH != 0) {
                serial::write_str("[XHCI] Failed to halt controller\r\n");
                return false;
            }
        }

        // ── Step 2: Reset controller ──
        mmio_write32(ops, CMD_HCRST);
        if !wait_for(100000, || mmio_read32(ops) & CMD_HCRST == 0) {
            serial::write_str("[XHCI] Controller reset timeout\r\n");
            return false;
        }
        // Wait for CNR to clear
        if !wait_for(100000, || mmio_read32(ops + 4) & STS_CNR == 0) {
            serial::write_str("[XHCI] Controller not ready after reset\r\n");
            return false;
        }
        serial::write_str("[XHCI] Controller reset OK\r\n");

        // ── Step 3: Configure MaxSlots ──
        let max_slots = XHCI.max_slots.min(MAX_SLOTS as u8);
        XHCI.max_slots = max_slots;
        mmio_write32(ops + 0x38, max_slots as u32);

        // ── Step 4: Set up DCBAA ──
        let dcbaa_phys = virt_to_phys(&XHCI.dcbaa as *const _ as u64);
        for i in 0..=MAX_SLOTS {
            XHCI.dcbaa[i] = 0;
        }
        mmio_write64(ops + 0x30, dcbaa_phys);
        serial::write_str("[XHCI] DCBAA at phys 0x");
        serial_hex(dcbaa_phys);
        serial::write_str("\r\n");

        // ── Step 5: Set up Command Ring ──
        XHCI.cmd_enqueue = 0;
        XHCI.cmd_cycle = 1;
        for i in 0..CMD_RING_SIZE {
            XHCI.cmd_ring[i] = Trb::zero();
        }
        // Link TRB at the end
        let cmd_ring_phys = virt_to_phys(&XHCI.cmd_ring as *const _ as u64);
        XHCI.cmd_ring[CMD_RING_SIZE - 1] = Trb {
            param_lo: cmd_ring_phys as u32,
            param_hi: (cmd_ring_phys >> 32) as u32,
            status: 0,
            control: TRB_LINK | TRB_CYCLE | (1 << 1), // Toggle Cycle bit
        };
        // Set CRCR (Command Ring Control Register)
        mmio_write64(ops + 0x18, cmd_ring_phys | 1); // CS=1 (cycle state)
        serial::write_str("[XHCI] Command Ring OK\r\n");

        // ── Step 6: Set up Event Ring (interrupter 0) ──
        XHCI.event_dequeue = 0;
        XHCI.event_cycle = 1;
        for i in 0..EVENT_RING_SIZE {
            XHCI.event_ring[i] = Trb::zero();
        }
        let event_ring_phys = virt_to_phys(&XHCI.event_ring as *const _ as u64);
        XHCI.event_seg_table[0] = EventRingSegEntry {
            base_lo: event_ring_phys as u32,
            base_hi: (event_ring_phys >> 32) as u32,
            size: EVENT_RING_SIZE as u32,
            _rsvd: 0,
        };

        let rt = runtime_base();
        let ir0 = rt + 0x20; // Interrupter Register Set 0

        // ERSTSZ = 1 (one segment)
        mmio_write32(ir0 + 0x08, 1);
        // ERDP = event ring dequeue pointer
        mmio_write64(ir0 + 0x18, event_ring_phys);
        // ERSTBA = event ring segment table base
        let erst_phys = virt_to_phys(&XHCI.event_seg_table as *const _ as u64);
        mmio_write64(ir0 + 0x10, erst_phys);

        // Enable interrupter
        let iman = mmio_read32(ir0);
        mmio_write32(ir0, iman | 0x02); // IE bit

        serial::write_str("[XHCI] Event Ring OK\r\n");

        // ── Step 7: Start controller ──
        let cmd = mmio_read32(ops);
        mmio_write32(ops, cmd | CMD_RUN | CMD_INTE);
        if !wait_for(20000, || mmio_read32(ops + 4) & STS_HCH == 0) {
            serial::write_str("[XHCI] Failed to start controller\r\n");
            return false;
        }
        serial::write_str("[XHCI] Controller running\r\n");

        // ── Step 8: Detect connected ports and enumerate keyboard ──
        let found = detect_and_enumerate_keyboard();

        XHCI.initialized = true;

        if found {
            serial::write_str("[XHCI] USB keyboard detected and configured\r\n");
        } else {
            serial::write_str("[XHCI] No USB keyboard found (controller ready for hotplug)\r\n");
        }
    }

    true
}

/// Check if XHCI is initialized
pub fn is_available() -> bool {
    unsafe { XHCI.initialized }
}

// ══════════════════════════════════════════════════════════════════════════════
// Port detection and keyboard enumeration
// ══════════════════════════════════════════════════════════════════════════════

unsafe fn detect_and_enumerate_keyboard() -> bool {
    let nports = XHCI.max_ports.min(MAX_PORTS as u8);

    for port in 0..nports {
        let portsc = mmio_read32(port_base(port));

        // Check if device is connected
        if portsc & PORT_CCS == 0 { continue; }

        serial::write_str("[XHCI] Port ");
        serial_dec(port as u64 + 1);
        serial::write_str(" connected, PORTSC=0x");
        serial_hex(portsc as u64);
        serial::write_str("\r\n");

        // Power the port if needed
        if portsc & PORT_PP == 0 {
            mmio_write32(port_base(port), portsc | PORT_PP);
            wait_for(50000, || false); // 50ms delay for power stabilization
        }

        // Reset the port (USB2 ports need this; USB3 ports auto-train)
        let portsc = mmio_read32(port_base(port));
        let pls = (portsc & PORT_PLS_MASK) >> 5;

        if pls != 0 { // Not in U0 state
            // Write to reset (preserve RW bits, clear RW1C bits)
            let preserve = portsc & 0x0E01C3E0; // RW bits mask
            mmio_write32(port_base(port), preserve | PORT_PR);

            // Wait for reset to complete
            if !wait_for(200000, || {
                let sc = mmio_read32(port_base(port));
                sc & PORT_PRC != 0
            }) {
                serial::write_str("[XHCI] Port reset timeout\r\n");
                continue;
            }

            // Clear port reset change
            let portsc = mmio_read32(port_base(port));
            mmio_write32(port_base(port), portsc | PORT_PRC);
        }

        // Wait for port to be enabled
        if !wait_for(50000, || mmio_read32(port_base(port)) & PORT_PED != 0) {
            serial::write_str("[XHCI] Port not enabled after reset\r\n");
            continue;
        }

        // Attempt to enumerate this device
        if enumerate_device(port) {
            return true; // Found a keyboard
        }
    }
    false
}

unsafe fn enumerate_device(port: u8) -> bool {
    // ── Enable Slot ──
    let slot_id = match send_command(Trb {
        param_lo: 0, param_hi: 0, status: 0,
        control: TRB_ENABLE_SLOT,
    }) {
        Some(trb) => {
            let slot = ((trb.control >> 24) & 0xFF) as u8;
            if slot == 0 || slot > XHCI.max_slots {
                serial::write_str("[XHCI] Enable Slot failed\r\n");
                return false;
            }
            serial::write_str("[XHCI] Slot ");
            serial_dec(slot as u64);
            serial::write_str(" allocated\r\n");
            slot
        }
        None => {
            serial::write_str("[XHCI] Enable Slot command timeout\r\n");
            return false;
        }
    };

    // Set up device context
    let slot_idx = (slot_id - 1) as usize;
    let dev_ctx_phys = virt_to_phys(&XHCI.dev_contexts[slot_idx] as *const _ as u64);
    XHCI.dcbaa[slot_id as usize] = dev_ctx_phys;

    // Zero out device context
    for b in &mut XHCI.dev_contexts[slot_idx].data {
        *b = 0;
    }

    // ── Prepare Input Context for Address Device ──
    // Clear input context
    for b in 0..8 { XHCI.input_ctx.control[b] = 0; }
    XHCI.input_ctx.slot = SlotContext::zero();
    for i in 0..31 { XHCI.input_ctx.endpoints[i] = EndpointContext::zero(); }

    // Add flags: A0 (Slot) + A1 (EP0)
    XHCI.input_ctx.control[1] = 0x03; // Add Slot Context + Endpoint 0

    // Slot Context:
    //   DW0: Route String=0, Speed (get from port), Context Entries=1
    //   DW1: Root Hub Port Number
    let portsc = mmio_read32(port_base(port));
    let speed = ((portsc >> 10) & 0x0F) as u8; // Port Speed
    let speed_name = match speed {
        1 => "Full-Speed (12 Mbps)",
        2 => "Low-Speed (1.5 Mbps)",
        3 => "High-Speed (480 Mbps)",
        4 => "SuperSpeed (5 Gbps)",
        5 => "SuperSpeedPlus (10 Gbps)",
        _ => "Unknown",
    };
    serial::write_str("[XHCI] Device speed: ");
    serial::write_str(speed_name);
    serial::write_str("\r\n");

    // Slot Context DW0: Context Entries (bits 31:27) = 1, Speed (bits 23:20)
    XHCI.input_ctx.slot.fields[0] = (1u32 << 27) | ((speed as u32) << 20);
    // Slot Context DW1: Root Hub Port (bits 23:16)
    XHCI.input_ctx.slot.fields[1] = ((port + 1) as u32) << 16;

    // Endpoint 0 Context:
    //   Max Packet Size depends on speed
    let max_packet = match speed {
        1 => 64,   // Full-Speed
        2 => 8,    // Low-Speed
        3 => 64,   // High-Speed
        4 => 512,  // SuperSpeed
        _ => 64,
    };

    // Set up transfer ring for EP0
    XHCI.xfer_enqueue = 0;
    XHCI.xfer_cycle = 1;
    for i in 0..XFER_RING_SIZE {
        XHCI.xfer_ring[i] = Trb::zero();
    }
    let xfer_phys = virt_to_phys(&XHCI.xfer_ring as *const _ as u64);
    // Link TRB at end
    XHCI.xfer_ring[XFER_RING_SIZE - 1] = Trb {
        param_lo: xfer_phys as u32,
        param_hi: (xfer_phys >> 32) as u32,
        status: 0,
        control: TRB_LINK | TRB_CYCLE | (1 << 1),
    };

    // EP0 Context:
    //   DW0: (reserved)
    //   DW1: CErr=3 (bits 2:1), EP Type=4 (Control Bi-directional, bits 5:3), MaxPktSize (bits 31:16)
    XHCI.input_ctx.endpoints[0].fields[1] =
        (3u32 << 1) | (4u32 << 3) | ((max_packet as u32) << 16);
    //   DW2-3: TR Dequeue Pointer (EP0 transfer ring) | DCS=1
    XHCI.input_ctx.endpoints[0].fields[2] = (xfer_phys as u32) | 1;
    XHCI.input_ctx.endpoints[0].fields[3] = (xfer_phys >> 32) as u32;
    //   DW4: Average TRB Length = 8
    XHCI.input_ctx.endpoints[0].fields[4] = 8;

    // ── Address Device (BSR=1 first to set address without data stage) ──
    let input_phys = virt_to_phys(&XHCI.input_ctx as *const _ as u64);
    let result = send_command(Trb {
        param_lo: input_phys as u32,
        param_hi: (input_phys >> 32) as u32,
        status: 0,
        control: TRB_ADDRESS_DEVICE | ((slot_id as u32) << 24),
    });

    match result {
        Some(trb) => {
            let cc = (trb.status >> 24) & 0xFF;
            if cc != 1 { // 1 = Success
                serial::write_str("[XHCI] Address Device failed, cc=");
                serial_dec(cc as u64);
                serial::write_str("\r\n");
                return false;
            }
            serial::write_str("[XHCI] Device addressed\r\n");
        }
        None => {
            serial::write_str("[XHCI] Address Device timeout\r\n");
            return false;
        }
    }

    // ── Get Device Descriptor (8 bytes first to learn real max packet size) ──
    let mut desc_buf = [0u8; 18];
    if !control_transfer(slot_id, 0x80, 0x06, 0x0100, 0, &mut desc_buf[..8]) {
        serial::write_str("[XHCI] Failed to get device descriptor\r\n");
        return false;
    }

    let dev_class = desc_buf[4];
    let dev_subclass = desc_buf[5];
    let dev_protocol = desc_buf[6];

    serial::write_str("[XHCI] Device: class=");
    serial_dec(dev_class as u64);
    serial::write_str(" subclass=");
    serial_dec(dev_subclass as u64);
    serial::write_str(" protocol=");
    serial_dec(dev_protocol as u64);
    serial::write_str("\r\n");

    // ── Get full device descriptor ──
    control_transfer(slot_id, 0x80, 0x06, 0x0100, 0, &mut desc_buf);

    // ── Get Configuration Descriptor ──
    let mut config_buf = [0u8; 128];
    if !control_transfer(slot_id, 0x80, 0x06, 0x0200, 0, &mut config_buf[..9]) {
        serial::write_str("[XHCI] Failed to get config descriptor\r\n");
        return false;
    }

    let total_len = u16::from_le_bytes([config_buf[2], config_buf[3]]) as usize;
    let read_len = total_len.min(128);
    if !control_transfer(slot_id, 0x80, 0x06, 0x0200, 0, &mut config_buf[..read_len]) {
        serial::write_str("[XHCI] Failed to get full config descriptor\r\n");
        return false;
    }

    // Parse configuration descriptor for HID keyboard interface
    let mut offset = 0;
    let mut is_keyboard = false;
    let mut kb_ep_addr: u8 = 0;
    let mut kb_ep_interval: u8 = 0;
    let mut kb_ep_max_packet: u16 = 0;

    while offset + 2 <= read_len {
        let desc_len = config_buf[offset] as usize;
        let desc_type = config_buf[offset + 1];

        if desc_len == 0 { break; }

        match desc_type {
            0x04 => {
                // Interface descriptor
                if offset + 9 <= read_len {
                    let iface_class = config_buf[offset + 5];
                    let iface_subclass = config_buf[offset + 6];
                    let iface_protocol = config_buf[offset + 7];
                    // HID class=0x03, subclass=0x01 (Boot), protocol=0x01 (Keyboard)
                    is_keyboard = iface_class == 0x03 && iface_subclass == 0x01 && iface_protocol == 0x01;
                    if is_keyboard {
                        serial::write_str("[XHCI] Found HID Boot Keyboard interface\r\n");
                    }
                }
            }
            0x05 => {
                // Endpoint descriptor
                if is_keyboard && offset + 7 <= read_len {
                    kb_ep_addr = config_buf[offset + 2];
                    let attrs = config_buf[offset + 3];
                    kb_ep_max_packet = u16::from_le_bytes([config_buf[offset + 4], config_buf[offset + 5]]);
                    kb_ep_interval = config_buf[offset + 6];

                    // Must be Interrupt IN endpoint
                    if (kb_ep_addr & 0x80 != 0) && (attrs & 0x03 == 0x03) {
                        serial::write_str("[XHCI] Keyboard endpoint: 0x");
                        serial_hex16(kb_ep_addr as u16);
                        serial::write_str(" interval=");
                        serial_dec(kb_ep_interval as u64);
                        serial::write_str(" maxpkt=");
                        serial_dec(kb_ep_max_packet as u64);
                        serial::write_str("\r\n");
                    }
                }
            }
            _ => {}
        }
        offset += desc_len;
    }

    if !is_keyboard || kb_ep_addr == 0 {
        serial::write_str("[XHCI] Not a keyboard device\r\n");
        return false;
    }

    // ── Set Configuration ──
    let config_value = config_buf[5]; // bConfigurationValue
    if !control_transfer_out(slot_id, 0x00, 0x09, config_value as u16, 0) {
        serial::write_str("[XHCI] Set Configuration failed\r\n");
        return false;
    }

    // ── Set Boot Protocol ──
    // SET_PROTOCOL request: bmRequestType=0x21, bRequest=0x0B, wValue=0 (Boot), wIndex=iface
    control_transfer_out(slot_id, 0x21, 0x0B, 0x0000, 0);

    // ── Set Idle (rate=0, ID=0 — report only on change) ──
    control_transfer_out(slot_id, 0x21, 0x0A, 0x0000, 0);

    // Store keyboard info
    XHCI.kb_slot = slot_id;
    XHCI.kb_port = port;
    // DCI = (endpoint_number * 2) + direction (1=IN, 0=OUT)
    let ep_num = kb_ep_addr & 0x0F;
    XHCI.kb_endpoint = ep_num * 2 + 1; // IN endpoint

    serial::write_str("[XHCI] Keyboard configured on slot ");
    serial_dec(slot_id as u64);
    serial::write_str(", DCI=");
    serial_dec(XHCI.kb_endpoint as u64);
    serial::write_str("\r\n");

    true
}

// ══════════════════════════════════════════════════════════════════════════════
// Command Ring operations
// ══════════════════════════════════════════════════════════════════════════════

/// Send a command TRB and wait for completion event.
/// Returns the completion event TRB or None on timeout.
unsafe fn send_command(mut trb: Trb) -> Option<Trb> {
    let idx = XHCI.cmd_enqueue;
    if idx >= CMD_RING_SIZE - 1 {
        // Wrap around via link TRB
        XHCI.cmd_enqueue = 0;
        XHCI.cmd_cycle ^= 1;
        return send_command(trb);
    }

    // Set cycle bit
    trb.control = (trb.control & !1) | XHCI.cmd_cycle;

    XHCI.cmd_ring[idx] = trb;

    // Memory fence
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

    XHCI.cmd_enqueue = idx + 1;

    // Ring doorbell 0 (host controller command)
    mmio_write32(doorbell(0), 0);

    // Wait for completion event
    wait_for_event(500000)
}

/// Wait for an event on the event ring. Returns the event TRB.
unsafe fn wait_for_event(timeout: u64) -> Option<Trb> {
    for _ in 0..timeout {
        let idx = XHCI.event_dequeue;
        let trb = &XHCI.event_ring[idx];

        // Check cycle bit matches expected
        if (trb.control & 1) == XHCI.event_cycle {
            let result = *trb;

            // Advance dequeue
            XHCI.event_dequeue += 1;
            if XHCI.event_dequeue >= EVENT_RING_SIZE {
                XHCI.event_dequeue = 0;
                XHCI.event_cycle ^= 1;
            }

            // Update ERDP (Event Ring Dequeue Pointer)
            let rt = runtime_base();
            let ir0 = rt + 0x20;
            let event_ring_phys = virt_to_phys(&XHCI.event_ring as *const _ as u64);
            let new_erdp = event_ring_phys + (XHCI.event_dequeue as u64) * 16;
            mmio_write64(ir0 + 0x18, new_erdp | (1 << 3)); // EHB bit

            return Some(result);
        }

        for _ in 0..100 { asm!("pause"); }
    }
    None
}

// ══════════════════════════════════════════════════════════════════════════════
// Control Transfers (USB Setup/Data/Status stages)
// ══════════════════════════════════════════════════════════════════════════════

/// Perform a control IN transfer (Setup → Data IN → Status OUT).
unsafe fn control_transfer(
    slot: u8, request_type: u8, request: u8,
    value: u16, index: u16, buf: &mut [u8],
) -> bool {
    let len = buf.len() as u16;

    // Setup stage
    let setup_lo = (request_type as u32) | ((request as u32) << 8) | ((value as u32) << 16);
    let setup_hi = (index as u32) | ((len as u32) << 16);

    enqueue_xfer_trb(Trb {
        param_lo: setup_lo,
        param_hi: setup_hi,
        status: 8, // TRB Transfer Length = 8 (setup packet is always 8 bytes)
        control: TRB_SETUP_STAGE | TRB_IDT | (3 << 16), // TRT=3 (IN Data Stage)
    });

    // Data stage (if any)
    if len > 0 {
        let buf_phys = virt_to_phys(buf.as_ptr() as u64);
        enqueue_xfer_trb(Trb {
            param_lo: buf_phys as u32,
            param_hi: (buf_phys >> 32) as u32,
            status: len as u32,
            control: TRB_DATA_STAGE | (1 << 16), // DIR=1 (IN)
        });
    }

    // Status stage
    enqueue_xfer_trb(Trb {
        param_lo: 0,
        param_hi: 0,
        status: 0,
        control: TRB_STATUS_STAGE | TRB_IOC, // DIR=0 (OUT) for IN transfer status
    });

    // Ring doorbell for EP0 (DCI=1)
    mmio_write32(doorbell(slot), 1);

    // Wait for transfer event
    match wait_for_event(500000) {
        Some(trb) => {
            let cc = (trb.status >> 24) & 0xFF;
            cc == 1 || cc == 13 // Success or Short Packet (OK for descriptors)
        }
        None => false,
    }
}

/// Perform a control OUT transfer with no data stage (Setup → Status IN).
unsafe fn control_transfer_out(
    slot: u8, request_type: u8, request: u8,
    value: u16, index: u16,
) -> bool {
    let setup_lo = (request_type as u32) | ((request as u32) << 8) | ((value as u32) << 16);
    let setup_hi = (index as u32);

    enqueue_xfer_trb(Trb {
        param_lo: setup_lo,
        param_hi: setup_hi,
        status: 8,
        control: TRB_SETUP_STAGE | TRB_IDT, // TRT=0 (No Data Stage)
    });

    // Status stage (IN for no-data control)
    enqueue_xfer_trb(Trb {
        param_lo: 0,
        param_hi: 0,
        status: 0,
        control: TRB_STATUS_STAGE | TRB_IOC | (1 << 16), // DIR=1 (IN)
    });

    mmio_write32(doorbell(slot), 1);

    match wait_for_event(500000) {
        Some(trb) => {
            let cc = (trb.status >> 24) & 0xFF;
            cc == 1
        }
        None => false,
    }
}

/// Enqueue a TRB on the transfer ring (EP0).
unsafe fn enqueue_xfer_trb(mut trb: Trb) {
    let idx = XHCI.xfer_enqueue;
    if idx >= XFER_RING_SIZE - 1 {
        // Wrap around
        XHCI.xfer_enqueue = 0;
        XHCI.xfer_cycle ^= 1;
        return enqueue_xfer_trb(trb);
    }

    trb.control = (trb.control & !1) | XHCI.xfer_cycle;
    XHCI.xfer_ring[idx] = trb;
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    XHCI.xfer_enqueue = idx + 1;
}

// ══════════════════════════════════════════════════════════════════════════════
// Keyboard polling (called periodically from keyboard driver or timer)
// ══════════════════════════════════════════════════════════════════════════════

/// USB HID Boot Protocol keyboard report layout (8 bytes):
///   byte 0: modifier keys (Ctrl, Shift, Alt, etc.)
///   byte 1: reserved (OEM)
///   byte 2-7: up to 6 simultaneous key codes (USB HID usage IDs)
static mut PREV_KEYS: [u8; 6] = [0; 6];

/// Poll the USB keyboard for new keypresses.
/// Translates USB HID usage IDs to PS/2 scancodes and pushes them
/// into the keyboard IRQ ring buffer.
pub fn poll_keyboard() {
    unsafe {
        if !XHCI.initialized || XHCI.kb_slot == 0 { return; }

        // Read keyboard report via interrupt IN endpoint
        let mut report = [0u8; 8];
        if !poll_interrupt_endpoint(XHCI.kb_slot, &mut report) {
            return;
        }

        let modifiers = report[0];
        let keys = &report[2..8];

        // Detect newly pressed keys (in current report but not in previous)
        for &key in keys.iter() {
            if key == 0 { continue; }
            let was_pressed = PREV_KEYS.iter().any(|&k| k == key);
            if !was_pressed {
                // New keypress — translate to PS/2 scancode and inject
                if let Some(scancode) = hid_to_scancode(key) {
                    inject_scancode(scancode);
                }
            }
        }

        // Detect released keys (in previous but not in current)
        for &key in PREV_KEYS.iter() {
            if key == 0 { continue; }
            let still_pressed = keys.iter().any(|&k| k == key);
            if !still_pressed {
                // Key released — inject release scancode (scancode | 0x80)
                if let Some(scancode) = hid_to_scancode(key) {
                    inject_scancode(scancode | 0x80);
                }
            }
        }

        // Handle modifier changes
        static mut PREV_MODS: u8 = 0;
        let mod_changes = modifiers ^ PREV_MODS;
        if mod_changes != 0 {
            // Left Ctrl
            if mod_changes & 0x01 != 0 {
                inject_scancode(if modifiers & 0x01 != 0 { 0x1D } else { 0x9D });
            }
            // Left Shift
            if mod_changes & 0x02 != 0 {
                inject_scancode(if modifiers & 0x02 != 0 { 0x2A } else { 0xAA });
            }
            // Left Alt
            if mod_changes & 0x04 != 0 {
                inject_scancode(if modifiers & 0x04 != 0 { 0x38 } else { 0xB8 });
            }
            // Right Ctrl
            if mod_changes & 0x10 != 0 {
                inject_scancode(0xE0);
                inject_scancode(if modifiers & 0x10 != 0 { 0x1D } else { 0x9D });
            }
            // Right Shift
            if mod_changes & 0x20 != 0 {
                inject_scancode(if modifiers & 0x20 != 0 { 0x36 } else { 0xB6 });
            }
            // Right Alt
            if mod_changes & 0x40 != 0 {
                inject_scancode(0xE0);
                inject_scancode(if modifiers & 0x40 != 0 { 0x38 } else { 0xB8 });
            }
            PREV_MODS = modifiers;
        }

        // Update previous keys
        PREV_KEYS.copy_from_slice(keys);
    }
}

/// Poll the interrupt IN endpoint for keyboard data.
/// This is a non-blocking poll — returns false if no data available.
unsafe fn poll_interrupt_endpoint(slot: u8, buf: &mut [u8; 8]) -> bool {
    // Check event ring for any pending transfer events
    let idx = XHCI.event_dequeue;
    let trb = &XHCI.event_ring[idx];

    if (trb.control & 1) != XHCI.event_cycle {
        // No event pending — schedule a new interrupt transfer if needed
        let buf_phys = virt_to_phys(buf.as_ptr() as u64);
        let xfer_idx = XHCI.xfer_enqueue;
        if xfer_idx < XFER_RING_SIZE - 1 {
            XHCI.xfer_ring[xfer_idx] = Trb {
                param_lo: buf_phys as u32,
                param_hi: (buf_phys >> 32) as u32,
                status: 8, // Transfer length = 8 bytes
                control: TRB_NORMAL | TRB_IOC | XHCI.xfer_cycle,
            };
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            XHCI.xfer_enqueue = xfer_idx + 1;

            // Ring doorbell for the keyboard endpoint DCI
            mmio_write32(doorbell(slot), XHCI.kb_endpoint as u32);
        }
        return false;
    }

    // Process the event
    let result = *trb;
    XHCI.event_dequeue += 1;
    if XHCI.event_dequeue >= EVENT_RING_SIZE {
        XHCI.event_dequeue = 0;
        XHCI.event_cycle ^= 1;
    }

    // Update ERDP
    let rt = runtime_base();
    let ir0 = rt + 0x20;
    let event_ring_phys = virt_to_phys(&XHCI.event_ring as *const _ as u64);
    let new_erdp = event_ring_phys + (XHCI.event_dequeue as u64) * 16;
    mmio_write64(ir0 + 0x18, new_erdp | (1 << 3));

    let cc = (result.status >> 24) & 0xFF;
    cc == 1 || cc == 13 // Success or Short Packet
}

/// Inject a PS/2 scancode into the keyboard ring buffer
/// (same buffer used by the PS/2 keyboard IRQ handler).
fn inject_scancode(scancode: u8) {
    crate::arch::x86_64::idt::kb_buffer_write(scancode);
}

// ══════════════════════════════════════════════════════════════════════════════
// USB HID Usage ID → PS/2 Scancode Set 1 translation
// ══════════════════════════════════════════════════════════════════════════════

/// Convert USB HID keyboard usage ID to PS/2 Scancode Set 1.
/// Returns None for unmapped keys.
fn hid_to_scancode(usage: u8) -> Option<u8> {
    // USB HID Usage ID → PS/2 Scancode Set 1 mapping
    match usage {
        0x04 => Some(0x1E), // A
        0x05 => Some(0x30), // B
        0x06 => Some(0x2E), // C
        0x07 => Some(0x20), // D
        0x08 => Some(0x12), // E
        0x09 => Some(0x21), // F
        0x0A => Some(0x22), // G
        0x0B => Some(0x23), // H
        0x0C => Some(0x17), // I
        0x0D => Some(0x24), // J
        0x0E => Some(0x25), // K
        0x0F => Some(0x26), // L
        0x10 => Some(0x32), // M
        0x11 => Some(0x31), // N
        0x12 => Some(0x18), // O
        0x13 => Some(0x19), // P
        0x14 => Some(0x10), // Q
        0x15 => Some(0x13), // R
        0x16 => Some(0x1F), // S
        0x17 => Some(0x14), // T
        0x18 => Some(0x16), // U
        0x19 => Some(0x2F), // V
        0x1A => Some(0x11), // W
        0x1B => Some(0x2D), // X
        0x1C => Some(0x15), // Y
        0x1D => Some(0x2C), // Z
        0x1E => Some(0x02), // 1
        0x1F => Some(0x03), // 2
        0x20 => Some(0x04), // 3
        0x21 => Some(0x05), // 4
        0x22 => Some(0x06), // 5
        0x23 => Some(0x07), // 6
        0x24 => Some(0x08), // 7
        0x25 => Some(0x09), // 8
        0x26 => Some(0x0A), // 9
        0x27 => Some(0x0B), // 0
        0x28 => Some(0x1C), // Enter
        0x29 => Some(0x01), // Escape
        0x2A => Some(0x0E), // Backspace
        0x2B => Some(0x0F), // Tab
        0x2C => Some(0x39), // Space
        0x2D => Some(0x0C), // -
        0x2E => Some(0x0D), // =
        0x2F => Some(0x1A), // [
        0x30 => Some(0x1B), // ]
        0x31 => Some(0x2B), // backslash
        0x33 => Some(0x27), // ;
        0x34 => Some(0x28), // '
        0x35 => Some(0x29), // `
        0x36 => Some(0x33), // ,
        0x37 => Some(0x34), // .
        0x38 => Some(0x35), // /
        0x39 => Some(0x3A), // CapsLock
        0x3A => Some(0x3B), // F1
        0x3B => Some(0x3C), // F2
        0x3C => Some(0x3D), // F3
        0x3D => Some(0x3E), // F4
        0x3E => Some(0x3F), // F5
        0x3F => Some(0x40), // F6
        0x40 => Some(0x41), // F7
        0x41 => Some(0x42), // F8
        0x42 => Some(0x43), // F9
        0x43 => Some(0x44), // F10
        // Extended keys (need 0xE0 prefix in PS/2)
        0x4F => Some(0x4D), // Right Arrow (would need 0xE0 prefix)
        0x50 => Some(0x4B), // Left Arrow
        0x51 => Some(0x50), // Down Arrow
        0x52 => Some(0x48), // Up Arrow
        0x4A => Some(0x47), // Home
        0x4B => Some(0x49), // Page Up
        0x4C => Some(0x53), // Delete
        0x4D => Some(0x4F), // End
        0x4E => Some(0x51), // Page Down
        _ => None,
    }
}

// ══════════════════════════════════════════════════════════════════════════════
// Serial helpers
// ══════════════════════════════════════════════════════════════════════════════

fn serial_hex(val: u64) {
    const HEX: [u8; 16] = *b"0123456789ABCDEF";
    let mut buf = [0u8; 16];
    let mut v = val;
    for i in (0..16).rev() {
        buf[i] = HEX[(v & 0xF) as usize];
        v >>= 4;
    }
    for b in buf {
        serial::write_byte(b);
    }
}

fn serial_hex16(val: u16) {
    const HEX: [u8; 16] = *b"0123456789ABCDEF";
    let mut buf = [0u8; 4];
    let mut v = val;
    for i in (0..4).rev() {
        buf[i] = HEX[(v & 0xF) as usize];
        v >>= 4;
    }
    for b in buf {
        serial::write_byte(b);
    }
}

fn serial_dec(mut val: u64) {
    if val == 0 {
        serial::write_byte(b'0');
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
        serial::write_byte(buf[j]);
    }
}
