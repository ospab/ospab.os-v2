/*
 * lspci — AXON hardware inventory utility for ospab.os (AETERNA)
 *
 * Queries the kernel PCI scan table (populated at boot by pci::enumerate())
 * and displays all detected PCI devices with vendor/class names.
 *
 * Output format (similar to Linux lspci -nn):
 *   00:00.0  Host Bridge [0600]: Intel 440FX Host Bridge [8086:1237]
 *   00:01.0  VGA Compatible Controller [0300]: QEMU/Bochs QEMU VGA Adapter [1234:1111]
 *   00:03.0  Ethernet Controller [0200]: Intel 82545EM Gigabit Ethernet [8086:100E]
 *
 * Flags:
 *   -v        verbose (add BAR info — future feature)
 *   -d XX:YY  filter by VendorID:DeviceID
 *   (no args) list all
 */

extern crate alloc;
use alloc::format;
use crate::arch::x86_64::framebuffer;

const FG:      u32 = 0x00FFFFFF;
const FG_OK:   u32 = 0x0000FF00;
const FG_DIM:  u32 = 0x00AAAAAA;
const FG_HL:   u32 = 0x00FFCC00;
const FG_BLU:  u32 = 0x006699FF;
const FG_CYN:  u32 = 0x0055FFFF;
const FG_ERR:  u32 = 0x00FF4444;
const BG:      u32 = 0x00000000;

fn puts(s: &str)   { framebuffer::draw_string(s, FG,     BG); }
fn dim(s: &str)    { framebuffer::draw_string(s, FG_DIM, BG); }
fn hl(s: &str)     { framebuffer::draw_string(s, FG_HL,  BG); }
fn ok(s: &str)     { framebuffer::draw_string(s, FG_OK,  BG); }
fn cyan(s: &str)   { framebuffer::draw_string(s, FG_CYN, BG); }
fn blue(s: &str)   { framebuffer::draw_string(s, FG_BLU, BG); }
fn err(s: &str)    { framebuffer::draw_string(s, FG_ERR, BG); }

/// Parse "XXXX:YYYY" vendor:device filter from args. Returns (vid, did) or (0xFFFF, 0xFFFF).
fn parse_filter(args: &str) -> (u16, u16) {
    // Look for -d XXXX:YYYY
    let args = args.trim();
    if let Some(rest) = args.strip_prefix("-d") {
        let rest = rest.trim();
        let parts: alloc::vec::Vec<&str> = rest.splitn(2, ':').collect();
        if parts.len() == 2 {
            let vid = u16::from_str_radix(parts[0].trim(), 16).unwrap_or(0xFFFF);
            let did = u16::from_str_radix(parts[1].trim(), 16).unwrap_or(0xFFFF);
            return (vid, did);
        }
    }
    (0xFFFF, 0xFFFF)
}

pub fn run(args: &str) {
    let (filter_vid, filter_did) = parse_filter(args);
    let filtering = filter_vid != 0xFFFF;

    let count = crate::pci::device_count();

    if count == 0 {
        err("lspci: PCI table is empty — run during boot with pci::enumerate()\n");
        return;
    }

    // Header
    dim(&format!("PCI devices ({} found):\n", count));

    let mut shown = 0u32;
    for i in 0..count {
        let d = match crate::pci::get_device(i) {
            Some(d) => d,
            None => continue,
        };

        // Apply filter
        if filtering {
            let vid_match = d.vendor_id == filter_vid;
            let did_match = filter_did == 0xFFFF || d.device_id == filter_did;
            if !vid_match || !did_match { continue; }
        }

        // Bus:Dev.Func  [colored yellow]
        hl(&format!("{:02x}:{:02x}.{}  ",
            d.bus, d.device, d.function));

        // Class description  [cyan]
        let cname = crate::pci::class_name(d.class, d.subclass);
        cyan(cname);

        // Class code  [dim brackets]
        dim(&format!(" [{:02x}{:02x}]: ", d.class, d.subclass));

        // Vendor name  [white]
        puts(crate::pci::vendor_name(d.vendor_id));
        puts(" ");

        // Device name
        let dname = crate::pci::device_name(d.vendor_id, d.device_id);
        if !dname.is_empty() {
            ok(dname);
        } else {
            dim("(unknown)");
        }

        // VID:DID  [dim brackets, blue hex]
        dim(" [");
        blue(&format!("{:04x}:{:04x}", d.vendor_id, d.device_id));
        dim("]");

        puts("\n");
        shown += 1;
    }

    if shown == 0 && filtering {
        puts(&format!("lspci: no devices matching {:04x}:{:04x}\n", filter_vid, filter_did));
    } else {
        dim(&format!("  {} device(s) listed.\n", shown));
    }

    // Special callouts for key OS-relevant devices
    puts("\n");
    print_callouts();
}

/// Print OS-relevant device summary: GPU, NIC, audio, storage.
fn print_callouts() {
    dim("─── OS-relevant hardware ───────────────────────────────\n");

    // GPU
    if let Some(d) = crate::pci::find_by_class(0x03, 0x00, 0x00) {
        ok("  GPU     "); dim("→ ");
        ok(crate::pci::device_name(d.vendor_id, d.device_id));
        if crate::pci::device_name(d.vendor_id, d.device_id).is_empty() {
            dim(&format!("[{:04x}:{:04x}]", d.vendor_id, d.device_id));
        }
        puts("\n");
    }
    // VMware SVGA II
    if let Some(d) = crate::pci::find_by_vendor_device(0x15AD, 0x0405) {
        let ready = crate::drivers::gpu::svga_ready();
        ok("  SVGA II "); dim("→ VMware SVGA II  ");
        if ready { ok("[driver loaded]"); } else { dim("[driver not loaded]"); }
        puts("\n");
    }
    // NIC
    if let Some(d) = crate::pci::find_by_class(0x02, 0x00, 0x00) {
        ok("  NIC     "); dim("→ ");
        ok(crate::pci::device_name(d.vendor_id, d.device_id));
        if crate::pci::device_name(d.vendor_id, d.device_id).is_empty() {
            dim(&format!("[{:04x}:{:04x}]", d.vendor_id, d.device_id));
        }
        puts("\n");
    }
    // Audio
    if let Some(d) = crate::pci::find_by_class(0x04, 0x03, 0x00) {
        let ready = crate::drivers::audio::is_ready();
        ok("  Audio   "); dim("→ ");
        ok(crate::pci::device_name(d.vendor_id, d.device_id));
        if crate::pci::device_name(d.vendor_id, d.device_id).is_empty() {
            dim(&format!("[{:04x}:{:04x}]", d.vendor_id, d.device_id));
        }
        dim("  ");
        if ready { ok("[driver loaded]"); } else { dim("[driver not loaded]"); }
        puts("\n");
    }
    // SATA
    if let Some(d) = crate::pci::find_by_class(0x01, 0x06, 0x00) {
        ok("  SATA    "); dim("→ ");
        puts(crate::pci::vendor_name(d.vendor_id));
        puts(" ");
        ok(crate::pci::device_name(d.vendor_id, d.device_id));
        if crate::pci::device_name(d.vendor_id, d.device_id).is_empty() {
            dim(&format!("[{:04x}:{:04x}]", d.vendor_id, d.device_id));
        }
        puts("\n");
    }
    // IDE
    if let Some(d) = crate::pci::find_by_class(0x01, 0x01, 0x00) {
        ok("  IDE     "); dim("→ ");
        puts(crate::pci::vendor_name(d.vendor_id));
        puts(" ");
        ok(crate::pci::device_name(d.vendor_id, d.device_id));
        if crate::pci::device_name(d.vendor_id, d.device_id).is_empty() {
            dim(&format!("[{:04x}:{:04x}]", d.vendor_id, d.device_id));
        }
        puts("\n");
    }
}
