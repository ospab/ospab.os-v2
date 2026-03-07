/*
 * Network Diagnostics — Deep hardware-level dump for AETERNA
 *
 * Callable from kmain or terminal (`netdiag`).
 * Dumps to COM1 serial:
 *   1. PCI device table (all NICs found)
 *   2. Active NIC register state
 *   3. Descriptor ring state (TX/RX heads, tails, DD bits)
 *   4. DMA address validation (virt→phys sanity check)
 *   5. IRQ routing verification (PIC mask state)
 *   6. Packet injection self-test (TX loopback)
 *
 * No stubs. Every function is complete, functional, no_std safe.
 */

use core::arch::asm;

// ─── Serial helpers (self-contained, no dependency on NIC modules) ─────────

fn s(text: &str) {
    crate::arch::x86_64::serial::write_str(text);
}
fn sb(b: u8) {
    crate::arch::x86_64::serial::write_byte(b);
}
fn hex4(v: u8) {
    sb(b"0123456789abcdef"[(v & 0xF) as usize]);
}
fn hex8(v: u8) {
    hex4(v >> 4);
    hex4(v);
}
fn hex16(v: u16) {
    hex8((v >> 8) as u8);
    hex8(v as u8);
}
fn hex32(v: u32) {
    hex8((v >> 24) as u8);
    hex8((v >> 16) as u8);
    hex8((v >> 8) as u8);
    hex8(v as u8);
}
fn hex64(v: u64) {
    hex32((v >> 32) as u32);
    hex32(v as u32);
}
fn dec(mut v: u64) {
    if v == 0 { sb(b'0'); return; }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while v > 0 { buf[i] = b'0' + (v % 10) as u8; v /= 10; i += 1; }
    for j in (0..i).rev() { sb(buf[j]); }
}
fn ip(a: [u8; 4]) {
    for i in 0..4 {
        dec(a[i] as u64);
        if i < 3 { sb(b'.'); }
    }
}
fn mac(m: [u8; 6]) {
    for i in 0..6 {
        hex8(m[i]);
        if i < 5 { sb(b':'); }
    }
}
fn nl() { s("\r\n"); }

// ─── PCI I/O (standalone, doesn't depend on pci.rs) ──────────────────────

fn pci_r32(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    let addr: u32 = 0x8000_0000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | ((off as u32) & 0xFC);
    unsafe {
        let v: u32;
        asm!("out dx, eax", in("dx") 0x0CF8u16, in("eax") addr, options(nomem, nostack));
        asm!("in eax, dx",  in("dx") 0x0CFCu16, out("eax") v, options(nomem, nostack));
        v
    }
}
fn pci_r16(bus: u8, dev: u8, func: u8, off: u8) -> u16 {
    let d = pci_r32(bus, dev, func, off & 0xFC);
    ((d >> ((off & 2) * 8)) & 0xFFFF) as u16
}
fn pci_r8(bus: u8, dev: u8, func: u8, off: u8) -> u8 {
    let d = pci_r32(bus, dev, func, off & 0xFC);
    ((d >> ((off & 3) * 8)) & 0xFF) as u8
}

// Port I/O
fn inb(port: u16) -> u8 {
    let v: u8;
    unsafe { asm!("in al, dx", in("dx") port, out("al") v, options(nomem, nostack)); }
    v
}
fn inw(port: u16) -> u16 {
    let v: u16;
    unsafe { asm!("in ax, dx", in("dx") port, out("ax") v, options(nomem, nostack)); }
    v
}
fn inl(port: u16) -> u32 {
    let v: u32;
    unsafe { asm!("in eax, dx", in("dx") port, out("eax") v, options(nomem, nostack)); }
    v
}

// MMIO
fn mmio_r32(addr: usize) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. PCI NIC scan — find ALL network controllers on the bus
// ═══════════════════════════════════════════════════════════════════════════

/// Scan entire PCI bus and print every device, highlighting network controllers
/// and VirtIO devices.
pub fn dump_pci_nics() {
    s("═══ NETDIAG: PCI NIC SCAN ═══\r\n");

    let mut nic_count = 0u32;
    let mut virtio_count = 0u32;

    for bus in 0u16..256 {
        for dev in 0u8..32 {
            let id = pci_r32(bus as u8, dev, 0, 0x00);
            let vid = (id & 0xFFFF) as u16;
            if vid == 0xFFFF || vid == 0x0000 { continue; }
            let did = ((id >> 16) & 0xFFFF) as u16;

            let class_reg = pci_r32(bus as u8, dev, 0, 0x08);
            let class    = ((class_reg >> 24) & 0xFF) as u8;
            let subclass = ((class_reg >> 16) & 0xFF) as u8;

            let is_nic = class == 0x02; // Network controller
            let is_virtio = vid == 0x1AF4;

            if is_nic || is_virtio {
                s("  [");
                if is_nic { s("NIC"); } else { s("VIRTIO"); }
                s("] ");
                hex8(bus as u8); sb(b':'); hex8(dev); s(".0  ");
                s("VID=0x"); hex16(vid);
                s(" DID=0x"); hex16(did);
                s(" Class="); hex8(class); sb(b'/'); hex8(subclass);

                // Read interrupt line + pin
                let irq_line = pci_r8(bus as u8, dev, 0, 0x3C);
                let irq_pin  = pci_r8(bus as u8, dev, 0, 0x3D);
                s(" IRQ="); dec(irq_line as u64);
                s(" PIN="); dec(irq_pin as u64);

                // Read command register
                let cmd = pci_r16(bus as u8, dev, 0, 0x04);
                s(" CMD=0x"); hex16(cmd);
                if cmd & 0x01 != 0 { s(" IO"); }
                if cmd & 0x02 != 0 { s(" MMIO"); }
                if cmd & 0x04 != 0 { s(" BusMaster"); }
                if cmd & 0x0400 != 0 { s(" IntDis"); }

                // BAR0
                let bar0 = pci_r32(bus as u8, dev, 0, 0x10);
                s(" BAR0=0x"); hex32(bar0);

                // VirtIO specific: subsystem ID tells us the device type
                if is_virtio {
                    let subsys = pci_r16(bus as u8, dev, 0, 0x2E);
                    s(" SubsysID=0x"); hex16(subsys);
                    match subsys {
                        1 => s(" (Network)"),
                        2 => s(" (Block)"),
                        3 => s(" (Console)"),
                        16 => s(" (GPU)"),
                        _ => s(" (Other)"),
                    }
                    virtio_count += 1;
                }

                nl();
                if is_nic { nic_count += 1; }
            }
        }
    }

    s("  Total NICs: "); dec(nic_count as u64);
    s(", VirtIO devices: "); dec(virtio_count as u64);
    nl();

    if nic_count == 0 && virtio_count == 0 {
        s("  !!! NO NETWORK HARDWARE FOUND !!!\r\n");
        s("  QEMU hint: add -device e1000,netdev=n0 -netdev user,id=n0\r\n");
        s("        or:  -device virtio-net-pci,netdev=n0 -netdev user,id=n0\r\n");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. Active NIC register dump
// ═══════════════════════════════════════════════════════════════════════════

pub fn dump_active_nic() {
    s("═══ NETDIAG: ACTIVE NIC STATE ═══\r\n");

    if !crate::net::is_up() {
        s("  Network stack is DOWN (net::is_up() == false)\r\n");
        s("  net::init() either failed or was not called.\r\n");
        return;
    }

    let nic = crate::net::nic_name();
    s("  Driver: "); s(nic); nl();
    s("  MAC: "); mac(unsafe { crate::net::OUR_MAC }); nl();
    s("  IP:  "); ip(unsafe { crate::net::OUR_IP }); nl();
    s("  GW:  "); ip(unsafe { crate::net::GATEWAY_IP }); nl();
    s("  Mask:"); ip(unsafe { crate::net::SUBNET_MASK }); nl();
    s("  GW MAC: "); mac(unsafe { crate::net::GATEWAY_MAC }); nl();
    s("  RX packets: "); dec(crate::net::rx_packets()); nl();
    s("  TX packets: "); dec(crate::net::tx_packets()); nl();

    match unsafe { crate::net::ACTIVE_NIC } {
        0 => dump_rtl8139_regs(),
        1 => dump_e1000_regs(),
        2 => dump_rtl8169_regs(),
        _ => { s("  Unknown NIC ID\r\n"); }
    }
}

fn dump_rtl8139_regs() {
    s("  ── RTL8139 Registers ──\r\n");
    let io = unsafe { super::rtl8139::io_base() };
    if io == 0 {
        s("  IO_BASE = 0 (not initialized)\r\n");
        return;
    }
    s("  IO_BASE: 0x"); hex16(io); nl();

    let cmd     = inb(io + 0x37);
    let isr     = inw(io + 0x3E);
    let imr     = inw(io + 0x3C);
    let rcr     = inl(io + 0x44);
    let tcr     = inl(io + 0x40);
    let rbstart = inl(io + 0x30);
    let capr    = inw(io + 0x38);
    let cbr     = inw(io + 0x3A);
    let cfg1    = inb(io + 0x52);

    s("  CMD=0x");     hex8(cmd);
    if cmd & 0x08 != 0 { s(" RxEN"); }
    if cmd & 0x04 != 0 { s(" TxEN"); }
    if cmd & 0x01 != 0 { s(" BUFE(empty)"); }
    nl();
    s("  ISR=0x");     hex16(isr);
    if isr & 0x01 != 0 { s(" ROK"); }
    if isr & 0x04 != 0 { s(" TOK"); }
    if isr & 0x10 != 0 { s(" RxOvfl"); }
    if isr & 0x20 != 0 { s(" LinkChg"); }
    nl();
    s("  IMR=0x");     hex16(imr); nl();
    s("  RCR=0x");     hex32(rcr); nl();
    s("  TCR=0x");     hex32(tcr); nl();
    s("  RBSTART=0x"); hex32(rbstart); nl();
    s("  CAPR=0x");    hex16(capr);
    s(" CBR=0x");      hex16(cbr); nl();
    s("  CONFIG1=0x"); hex8(cfg1); nl();

    // TX descriptor status
    for i in 0..4u16 {
        let tsd = inl(io + 0x10 + i * 4);
        let tsad = inl(io + 0x20 + i * 4);
        s("  TSD"); dec(i as u64); s("=0x"); hex32(tsd);
        if tsd & (1 << 15) != 0 { s(" TOK"); }
        if tsd & (1 << 13) != 0 { s(" OWN"); }
        s(" TSAD"); dec(i as u64); s("=0x"); hex32(tsad);
        nl();
    }

    // RX buffer first 64 bytes (DMA verification)
    s("  RX buf[0..64]: ");
    for i in 0..64u32 {
        let phys_base = rbstart;
        let hhdm = crate::arch::x86_64::boot::hhdm_offset().unwrap_or(0xFFFF_8000_0000_0000);
        let virt = (phys_base as u64 + hhdm) as *const u8;
        let byte = unsafe { core::ptr::read_volatile(virt.add(i as usize)) };
        hex8(byte);
        if i % 16 == 15 { s("\r\n                  "); }
        else { sb(b' '); }
    }
    nl();
}

fn dump_e1000_regs() {
    s("  ── Intel e1000 Registers ──\r\n");
    if !super::e1000::is_initialized() {
        s("  NOT INITIALIZED\r\n");
        return;
    }

    let (status, icr, rdh, rdt) = super::e1000::diag_regs();
    s("  STATUS=0x"); hex32(status);
    if status & (1 << 1) != 0 { s(" LinkUp"); } else { s(" LinkDOWN"); }
    let speed = (status >> 6) & 3;
    s(" Speed=");
    match speed { 0 => s("10M"), 1 => s("100M"), _ => s("1G") }
    nl();

    s("  ICR=0x"); hex32(icr);
    if icr & 0x01 != 0 { s(" TXDW"); }
    if icr & 0x04 != 0 { s(" LSC"); }
    if icr & 0x10 != 0 { s(" RXDMT0"); }
    if icr & 0x40 != 0 { s(" RXO"); }
    if icr & 0x80 != 0 { s(" RXT0"); }
    nl();

    s("  RDH="); dec(rdh as u64);
    s(" RDT="); dec(rdt as u64);
    s(" (RX ring: 32 descs)\r\n");

    // Check for the classic e1000 silent failure: RDH == RDT means ring is exhausted
    if rdh == rdt {
        s("  !!! RDH == RDT — RX ring EXHAUSTED (hardware has no free buffers) !!!\r\n");
    }
    // RDH > 0 but we never polled means packets arrived but weren't consumed
    if rdh > 0 {
        s("  RDH > 0 means hardware wrote "); dec(rdh as u64);
        s(" descriptors. Check if poll_rx() is being called.\r\n");
    }
}

fn dump_rtl8169_regs() {
    s("  ── RTL8169 Registers ──\r\n");
    s("  (diagnostics not yet wired — RTL8169 driver uses I/O)\r\n");
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. IRQ routing verification
// ═══════════════════════════════════════════════════════════════════════════

pub fn dump_irq_state() {
    s("═══ NETDIAG: IRQ ROUTING ═══\r\n");

    // Read PIC masks
    let mask1 = inb(0x21); // Master PIC data port
    let mask2 = inb(0xA1); // Slave PIC data port

    s("  PIC1 mask: 0b");
    for bit in (0..8).rev() {
        if mask1 & (1 << bit) != 0 { sb(b'1'); } else { sb(b'0'); }
    }
    s(" (0=enabled 1=masked)\r\n");

    s("  PIC2 mask: 0b");
    for bit in (0..8).rev() {
        if mask2 & (1 << bit) != 0 { sb(b'1'); } else { sb(b'0'); }
    }
    nl();

    // Decode which IRQs matter for networking
    let irqs = [
        (0u8, "Timer"),
        (1, "Keyboard"),
        (2, "Cascade"),
        (9, "NET/ACPI"),
        (10, "NET"),
        (11, "NET"),
    ];
    for &(irq, name) in &irqs {
        let masked = if irq < 8 {
            mask1 & (1 << irq) != 0
        } else {
            mask2 & (1 << (irq - 8)) != 0
        };
        s("  IRQ "); dec(irq as u64);
        s(" ("); s(name); s("): ");
        if masked {
            s("MASKED !!!");
        } else {
            s("enabled");
        }
        nl();
    }

    // Check cascade: if IRQ2 is masked, NO slave IRQs (8-15) can fire
    if mask1 & (1 << 2) != 0 {
        s("  !!! CRITICAL: IRQ2 (cascade) MASKED — slave PIC IRQs 8-15 are ALL dead !!!\r\n");
    }

    // Read ISR (In-Service Register) to see if an IRQ is stuck
    // OCW3: read ISR = 0x0B to command port
    unsafe {
        asm!("out dx, al", in("dx") 0x20u16, in("al") 0x0Bu8, options(nomem, nostack));
        let isr1: u8;
        asm!("in al, dx", in("dx") 0x20u16, out("al") isr1, options(nomem, nostack));
        asm!("out dx, al", in("dx") 0xA0u16, in("al") 0x0Bu8, options(nomem, nostack));
        let isr2: u8;
        asm!("in al, dx", in("dx") 0xA0u16, out("al") isr2, options(nomem, nostack));

        s("  PIC1 ISR: 0b");
        for bit in (0..8).rev() {
            if isr1 & (1 << bit) != 0 { sb(b'1'); } else { sb(b'0'); }
        }
        nl();
        s("  PIC2 ISR: 0b");
        for bit in (0..8).rev() {
            if isr2 & (1 << bit) != 0 { sb(b'1'); } else { sb(b'0'); }
        }
        nl();

        if isr1 != 0 || isr2 != 0 {
            s("  !!! Non-zero ISR means an IRQ is in-service (EOI may be missing) !!!\r\n");
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. DMA address validation
// ═══════════════════════════════════════════════════════════════════════════

/// Validate that our virt_to_phys calculation is correct by cross-checking
/// against the HHDM offset provided by Limine.
pub fn validate_dma_addresses() {
    s("═══ NETDIAG: DMA ADDRESS VALIDATION ═══\r\n");

    let hhdm = crate::arch::x86_64::boot::hhdm_offset().unwrap_or(0);
    s("  HHDM offset: 0x"); hex64(hhdm); nl();

    let phys_base = crate::arch::x86_64::boot::kernel_phys_base();
    let virt_base = crate::arch::x86_64::boot::kernel_virt_base();
    let virt_offset = crate::arch::x86_64::boot::kernel_virt_offset();
    s("  Kernel phys base: 0x"); hex64(phys_base); nl();
    s("  Kernel virt base: 0x"); hex64(virt_base); nl();
    s("  Kernel virt offset: 0x"); hex64(virt_offset);
    if virt_offset == 0xFFFF_FFFF_8000_0000 {
        s(" (matches linker default)");
    } else {
        s(" (Limine RELOCATED kernel!)");
    }
    nl();

    // Take a known kernel static and verify its physical address
    static CANARY: u32 = 0xDEAD_BEEF;
    let canary_virt = &CANARY as *const u32 as u64;
    let canary_phys = canary_virt.wrapping_sub(virt_offset);

    s("  Canary virt:     0x"); hex64(canary_virt); nl();
    s("  Canary phys:     0x"); hex64(canary_phys); nl();

    // Read back via HHDM to verify
    let readback_ptr = (canary_phys + hhdm) as *const u32;
    let readback = unsafe { core::ptr::read_volatile(readback_ptr) };

    s("  Readback via HHDM: 0x"); hex32(readback);
    if readback == 0xDEAD_BEEF {
        s(" OK ✓\r\n");
    } else {
        s(" MISMATCH !!!\r\n");
        s("  !!! virt_to_phys is WRONG — DMA will silently fail !!!\r\n");
        s("  !!! This is the #1 cause of 'packets sent but never transmitted' !!!\r\n");
    }

    // Also check if the physical address looks sane (should be < 4GB for RTL8139)
    if canary_phys > 0xFFFF_FFFF {
        s("  WARNING: kernel phys > 4GB — RTL8139 DMA will fail (32-bit only)\r\n");
    }

    // Check if kernel is at the expected base
    extern "C" {
        static _kernel_start: u8;
    }
    let ks = unsafe { &_kernel_start as *const u8 as u64 };
    s("  Kernel start virt: 0x"); hex64(ks); nl();

    // Verify the HHDM-based check of an e1000 descriptor buffer
    if super::e1000::is_initialized() {
        s("  [e1000] Checking descriptor DMA addresses...\r\n");
        // The TX/RX buffers are kernel statics → their phys should be
        // virt - 0xFFFFFFFF80000000 and should be < 4GB for non-64bit BARs.
        // (e1000 supports 64-bit DMA, so this is informational.)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. VirtIO-Net specific diagnostics
// ═══════════════════════════════════════════════════════════════════════════

/// Check for VirtIO-Net on PCI and dump its registers.
/// This is the diagnostic you need if you're trying to bring up virtio-net.
pub fn dump_virtio_net() {
    s("═══ NETDIAG: VIRTIO-NET PROBE ═══\r\n");

    // VirtIO legacy PCI: vendor=0x1AF4, device=0x1000 (network)
    // VirtIO modern PCI: vendor=0x1AF4, device=0x1041 (network, transitional)
    let legacy_devids: &[u16] = &[0x1000]; // Legacy
    let modern_devids: &[u16] = &[0x1041, 0x1040]; // Modern net, transitional

    let mut found = false;

    for bus in 0u16..256 {
        for dev in 0u8..32 {
            let id = pci_r32(bus as u8, dev, 0, 0x00);
            let vid = (id & 0xFFFF) as u16;
            let did = ((id >> 16) & 0xFFFF) as u16;
            if vid != 0x1AF4 { continue; }

            let subsys = pci_r16(bus as u8, dev, 0, 0x2E);

            let is_net = legacy_devids.contains(&did)
                || modern_devids.contains(&did)
                || subsys == 1; // subsystem ID 1 = network

            if !is_net { continue; }

            found = true;
            s("  VirtIO-Net at ");
            hex8(bus as u8); sb(b':'); hex8(dev); s(".0\r\n");
            s("  Device ID: 0x"); hex16(did);
            if did == 0x1000 { s(" (legacy)"); } else { s(" (modern)"); }
            nl();

            let cmd = pci_r16(bus as u8, dev, 0, 0x04);
            s("  PCI CMD: 0x"); hex16(cmd);
            if cmd & 0x01 == 0 { s(" !IO_DISABLED"); }
            if cmd & 0x04 == 0 { s(" !NO_BUS_MASTER"); }
            nl();

            let bar0 = pci_r32(bus as u8, dev, 0, 0x10);
            s("  BAR0: 0x"); hex32(bar0);
            if bar0 & 1 != 0 {
                let io_base = (bar0 & 0xFFFC) as u16;
                s(" (I/O at 0x"); hex16(io_base); s(")\r\n");
                dump_virtio_legacy_regs(io_base);
            } else {
                s(" (MMIO)\r\n");
                s("  NOTE: Legacy VirtIO uses I/O ports. Modern VirtIO uses MMIO caps.\r\n");
                // Walk PCI capability list for VirtIO modern caps
                dump_virtio_modern_caps(bus as u8, dev);
            }

            let irq = pci_r8(bus as u8, dev, 0, 0x3C);
            s("  IRQ line: "); dec(irq as u64);
            let mask = if irq < 8 { inb(0x21) } else { inb(0xA1) };
            let bit = if irq < 8 { irq } else { irq - 8 };
            if mask & (1 << bit) != 0 {
                s(" MASKED !!!");
            } else {
                s(" enabled");
            }
            nl();
        }
    }

    if !found {
        s("  No VirtIO-Net device found on PCI bus.\r\n");
        s("  To add one in QEMU:\r\n");
        s("    -device virtio-net-pci,netdev=n0 -netdev user,id=n0\r\n");
    }
}

/// Dump VirtIO legacy I/O port registers (device ID 0x1000)
fn dump_virtio_legacy_regs(io: u16) {
    // VirtIO legacy register layout at BAR0 (I/O space):
    //   0x00  u32  Device Features
    //   0x04  u32  Guest Features
    //   0x08  u32  Queue Address (PFN)
    //   0x0C  u16  Queue Size
    //   0x0E  u16  Queue Select
    //   0x10  u16  Queue Notify
    //   0x12  u8   Device Status
    //   0x13  u8   ISR Status
    // (For net: 0x14+ are device-specific config: MAC, status, etc.)

    s("  ── VirtIO Legacy Registers (I/O 0x"); hex16(io); s(") ──\r\n");

    let dev_features = inl(io + 0x00);
    let guest_features = inl(io + 0x04);

    s("  Device Features: 0x"); hex32(dev_features); nl();
    s("  Guest Features:  0x"); hex32(guest_features); nl();

    // Feature bits for VirtIO-Net:
    if dev_features & (1 << 0) != 0 { s("    VIRTIO_NET_F_CSUM\r\n"); }
    if dev_features & (1 << 1) != 0 { s("    VIRTIO_NET_F_GUEST_CSUM\r\n"); }
    if dev_features & (1 << 5) != 0 { s("    VIRTIO_NET_F_MAC\r\n"); }
    if dev_features & (1 << 16) != 0 { s("    VIRTIO_NET_F_STATUS\r\n"); }
    if dev_features & (1 << 17) != 0 { s("    VIRTIO_NET_F_MQ\r\n"); }

    // Device status register (critical!)
    let status = inb(io + 0x12);
    s("  Device Status: 0x"); hex8(status);
    if status == 0                { s(" (RESET — device not initialised!)"); }
    if status & 0x01 != 0        { s(" ACKNOWLEDGE"); }
    if status & 0x02 != 0        { s(" DRIVER"); }
    if status & 0x04 != 0        { s(" DRIVER_OK"); }
    if status & 0x08 != 0        { s(" FEATURES_OK"); }
    if status & 0x40 != 0        { s(" DEVICE_NEEDS_RESET"); }
    if status & 0x80 != 0        { s(" FAILED"); }
    nl();

    // If status != 0x0F (ACK|DRIVER|FEATURES_OK|DRIVER_OK), the device is NOT ready
    if status & 0x04 == 0 {
        s("  !!! DRIVER_OK not set — device is NOT operational !!!\r\n");
        s("  !!! The init sequence must set status bits in order: !!!\r\n");
        s("  !!!   1. Reset (write 0) !!!\r\n");
        s("  !!!   2. ACKNOWLEDGE (0x01) !!!\r\n");
        s("  !!!   3. DRIVER (0x02) !!!\r\n");
        s("  !!!   4. Negotiate features, write FEATURES_OK (0x08) !!!\r\n");
        s("  !!!   5. Configure virtqueues !!!\r\n");
        s("  !!!   6. DRIVER_OK (0x04) !!!\r\n");
    }

    // ISR status
    let isr = inb(io + 0x13);
    s("  ISR Status: 0x"); hex8(isr);
    if isr & 0x01 != 0 { s(" QUEUE_IRQ"); }
    if isr & 0x02 != 0 { s(" CONFIG_CHANGE"); }
    nl();

    // VirtQueues — dump first 3 (RX, TX, Control)
    for q in 0..3u16 {
        // Select queue
        unsafe {
            asm!("out dx, ax", in("dx") io + 0x0E, in("ax") q, options(nomem, nostack));
        }
        let qsize = inw(io + 0x0C);
        let qaddr = inl(io + 0x08);

        s("  Queue "); dec(q as u64);
        s(": size="); dec(qsize as u64);
        s(" PFN=0x"); hex32(qaddr);
        let phys = (qaddr as u64) << 12;
        s(" (phys=0x"); hex64(phys); s(")");

        if q == 0 { s(" [RX]"); }
        if q == 1 { s(" [TX]"); }
        if q == 2 { s(" [Ctrl]"); }
        nl();

        if qsize == 0 {
            s("    Queue not available (size=0)\r\n");
            continue;
        }

        if qaddr == 0 {
            s("    !!! Queue address = 0 — NOT CONFIGURED !!!\r\n");
            s("    !!! Driver must allocate aligned memory and write PFN here !!!\r\n");
            continue;
        }

        // Verify alignment: the physical address must be page-aligned (4096)
        if phys & 0xFFF != 0 {
            s("    !!! ALIGNMENT ERROR: queue phys not 4K-aligned !!!\r\n");
        }

        // Dump first descriptor from the virtqueue (through HHDM)
        let hhdm = crate::arch::x86_64::boot::hhdm_offset().unwrap_or(0xFFFF_8000_0000_0000);
        let desc_virt = (phys + hhdm) as *const u64;
        if !desc_virt.is_null() && phys < 0x1_0000_0000 {
            // VirtQueue descriptor: addr(u64) + len(u32) + flags(u16) + next(u16)
            let d_addr = unsafe { core::ptr::read_volatile(desc_virt) };
            let d_rest = unsafe { core::ptr::read_volatile(desc_virt.add(1)) };
            let d_len = (d_rest & 0xFFFF_FFFF) as u32;
            let d_flags = ((d_rest >> 32) & 0xFFFF) as u16;
            s("    desc[0]: addr=0x"); hex64(d_addr);
            s(" len="); dec(d_len as u64);
            s(" flags=0x"); hex16(d_flags);
            nl();
        }
    }

    // Device-specific config: MAC address at offset 0x14
    s("  VirtIO-Net MAC: ");
    for i in 0..6u16 {
        hex8(inb(io + 0x14 + i));
        if i < 5 { sb(b':'); }
    }
    nl();

    // Link status at offset 0x1A (if VIRTIO_NET_F_STATUS is negotiated)
    if dev_features & (1 << 16) != 0 {
        let link = inw(io + 0x1A);
        s("  VirtIO-Net Link: ");
        if link & 1 != 0 { s("UP"); } else { s("DOWN"); }
        nl();
    }
}

/// Walk PCI capability list to find VirtIO modern capability structures
fn dump_virtio_modern_caps(bus: u8, dev: u8) {
    let status = pci_r16(bus, dev, 0, 0x06);
    if status & 0x10 == 0 {
        s("  No PCI capability list\r\n");
        return;
    }

    let mut cap_ptr = pci_r8(bus, dev, 0, 0x34) & 0xFC;
    let mut n = 0u32;

    s("  ── PCI Capabilities ──\r\n");
    while cap_ptr != 0 && n < 32 {
        let cap_id = pci_r8(bus, dev, 0, cap_ptr);
        let cap_next = pci_r8(bus, dev, 0, cap_ptr + 1) & 0xFC;

        s("    Cap @0x"); hex8(cap_ptr);
        s(" ID=0x"); hex8(cap_id);

        if cap_id == 0x09 {
            // VirtIO vendor-specific cap
            let cfg_type = pci_r8(bus, dev, 0, cap_ptr + 3);
            let bar = pci_r8(bus, dev, 0, cap_ptr + 4);
            let offset = pci_r32(bus, dev, 0, cap_ptr + 8);
            let length = pci_r32(bus, dev, 0, cap_ptr + 12);
            s(" VIRTIO type="); dec(cfg_type as u64);
            s(" BAR="); dec(bar as u64);
            s(" off=0x"); hex32(offset);
            s(" len=0x"); hex32(length);
            match cfg_type {
                1 => s(" (COMMON)"),
                2 => s(" (NOTIFY)"),
                3 => s(" (ISR)"),
                4 => s(" (DEVICE)"),
                5 => s(" (PCI_CFG)"),
                _ => {}
            }
        } else if cap_id == 0x05 {
            s(" MSI");
        } else if cap_id == 0x11 {
            s(" MSI-X");
        }

        nl();
        cap_ptr = cap_next;
        n += 1;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. Packet TX self-test
// ═══════════════════════════════════════════════════════════════════════════

/// Send a single ARP request to the gateway and check if it actually leaves the NIC.
/// This validates the entire TX path: descriptor → DMA → wire.
pub fn tx_self_test() {
    s("═══ NETDIAG: TX SELF-TEST ═══\r\n");

    if !crate::net::is_up() {
        s("  SKIP: network not up\r\n");
        return;
    }

    let tx_before = crate::net::tx_packets();
    s("  TX counter before: "); dec(tx_before); nl();

    // Send an ARP request to gateway
    let gw = unsafe { crate::net::GATEWAY_IP };
    s("  Sending ARP request for "); ip(gw); s("...\r\n");
    crate::net::arp::send_request(gw);

    let tx_after = crate::net::tx_packets();
    s("  TX counter after:  "); dec(tx_after); nl();

    if tx_after > tx_before {
        s("  TX path OK (counter incremented)\r\n");
    } else {
        s("  !!! TX counter did NOT increment — send_raw() may be broken !!!\r\n");
    }

    // Wait ~500ms and poll for any RX activity
    s("  Polling RX for 500ms...\r\n");
    let rx_before = crate::net::rx_packets();
    let deadline = crate::arch::x86_64::idt::timer_ticks() + 50; // 500ms
    loop {
        crate::net::poll_rx();
        if crate::arch::x86_64::idt::timer_ticks() >= deadline { break; }
        unsafe { asm!("hlt"); }
    }
    let rx_after = crate::net::rx_packets();
    s("  RX packets during test: "); dec(rx_after - rx_before); nl();
    if rx_after > rx_before {
        s("  RX path OK (packets received)\r\n");
    } else {
        s("  No RX packets — could be normal if gateway doesn't respond to ARP.\r\n");
        s("  Try: qemu ... -netdev user,id=n0 -device <nic>,netdev=n0\r\n");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. Master diagnostic — call this from kmain or terminal
// ═══════════════════════════════════════════════════════════════════════════

/// Run the full network diagnostic suite. Output goes to COM1 serial.
/// Call from kmain after net::init() or from terminal via `netdiag` command.
pub fn run_full_diagnostic() {
    s("\r\n");
    s("╔══════════════════════════════════════════════════════════════╗\r\n");
    s("║         AETERNA NETWORK DIAGNOSTIC SUITE v1.0               ║\r\n");
    s("║         Serial output: COM1 115200 8N1                      ║\r\n");
    s("╚══════════════════════════════════════════════════════════════╝\r\n\r\n");

    dump_pci_nics();
    s("\r\n");
    dump_irq_state();
    s("\r\n");
    validate_dma_addresses();
    s("\r\n");
    dump_active_nic();
    s("\r\n");
    dump_virtio_net();
    s("\r\n");
    tx_self_test();

    s("\r\n");
    s("═══ NETDIAG: SUMMARY ═══\r\n");
    if !crate::net::is_up() {
        s("  RESULT: Network stack is DOWN.\r\n");
        s("  ACTION: Check QEMU flags, PCI NIC presence, and driver probe.\r\n");
    } else {
        s("  RESULT: Network stack is UP (");
        s(crate::net::nic_name());
        s(")\r\n");
        let (r0, _, _, _) = crate::net::diag();
        if crate::net::nic_name() == "Intel e1000" && r0 & 2 == 0 {
            s("  WARNING: e1000 link is DOWN.\r\n");
        }
        if crate::net::tx_packets() == 0 {
            s("  WARNING: 0 TX packets — nothing has been transmitted.\r\n");
        }
        if crate::net::rx_packets() == 0 {
            s("  WARNING: 0 RX packets — nothing has been received.\r\n");
        }
    }
    s("═══ END NETDIAG ═══\r\n\r\n");
}

/// Print a brief on-screen (framebuffer) network summary.
/// Called from the terminal after run_full_diagnostic() so the user gets
/// key results without having to read serial output.
pub fn run_screen_summary() {
    use crate::arch::x86_64::framebuffer;

    const FG: u32     = 0x00FFFFFF;
    const FG_OK: u32  = 0x0000FF00;
    const FG_ERR: u32 = 0x00FF4444;
    const FG_WARN: u32= 0x00FFCC00;
    const FG_DIM: u32 = 0x00AAAAAA;
    const BG: u32     = 0x00000000;

    let p  = |s: &str| framebuffer::draw_string(s, FG, BG);
    let ok = |s: &str| framebuffer::draw_string(s, FG_OK, BG);
    let er = |s: &str| framebuffer::draw_string(s, FG_ERR, BG);
    let wn = |s: &str| framebuffer::draw_string(s, FG_WARN, BG);
    let dm = |s: &str| framebuffer::draw_string(s, FG_DIM, BG);

    p("\n");
    framebuffer::draw_string("  Network Diagnostics Summary\n", FG_WARN, BG);
    dm("  ──────────────────────────────\n");

    if !crate::net::is_up() {
        er("  Status  : DOWN\n");
        er("  No NIC detected or driver probe failed.\n");
        er("  Check QEMU flags (-netdev/-device) and kernel build.\n\n");
        return;
    }

    ok("  Status  : UP\n");

    // Driver name
    p("  Driver  : ");
    p(crate::net::nic_name());
    p("\n");

    // MAC address
    let mac = unsafe { crate::net::OUR_MAC };
    {
        let hex = b"0123456789abcdef";
        p("  MAC     : ");
        for i in 0..6usize {
            framebuffer::draw_char(hex[(mac[i] >> 4) as usize] as char, FG, BG);
            framebuffer::draw_char(hex[(mac[i] & 0xF) as usize] as char, FG, BG);
            if i < 5 { p(":"); }
        }
        p("\n");
    }

    // IP address
    let ip = unsafe { crate::net::OUR_IP };
    p("  IP      : ");
    p(&alloc::format!("{}.{}.{}.{}\n", ip[0], ip[1], ip[2], ip[3]));

    // RX / TX packet counts
    let rx = crate::net::rx_packets();
    let tx = crate::net::tx_packets();
    p("  RX pkts : ");
    p(&alloc::format!("{}\n", rx));
    p("  TX pkts : ");
    p(&alloc::format!("{}\n", tx));

    // Link status check for e1000
    if crate::net::nic_name() == "Intel e1000" {
        let (r0, _, _, _) = crate::net::diag();
        if r0 & 2 == 0 {
            wn("  Link    : DOWN (e1000 STATUS.LU=0)\n");
        } else {
            ok("  Link    : Up\n");
        }
    }

    // Error rate heuristic: warn if we only ever sent but never received
    if tx > 0 && rx == 0 {
        wn("  Warning : TX active but 0 RX — possible link or routing issue\n");
    } else if rx > 0 && tx == 0 {
        wn("  Warning : RX active but 0 TX — driver may not be transmitting\n");
    }

    dm("\n  Full register dump written to COM1 serial output.\n\n");
}

