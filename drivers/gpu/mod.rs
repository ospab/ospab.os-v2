/*
 * AETERNA GPU / Display Driver Subsystem
 *
 * Supported hardware:
 *   vmware_svga — VMware SVGA II virtual GPU (PCI 15AD:0405)
 *                 Provides resolution switching and FIFO command queue
 *   vmmouse     — VMware Backdoor Absolute Mouse (port 0x5658)
 *                 Delivers absolute cursor coordinates without PS/2 relative math
 */

pub mod vmware_svga;
pub mod vmmouse;

/// Probe all GPU/display accelerators — call after PCI enumeration.
/// This is **non-destructive**: it detects hardware and reads parameters but
/// does NOT touch SVGA_REG_ENABLE or CONFIG_DONE, so the Limine GOP
/// framebuffer keeps working undisturbed.
/// Returns true if any accelerated display was found.
pub fn init() -> bool {
    let svga = vmware_svga::init();
    if svga {
        // VMMouse is only useful alongside the SVGA adapter
        vmmouse::init();
    }
    svga
}

/// Re-export high-level accessors
pub use vmware_svga::{is_ready as svga_ready, set_mode, activate as svga_activate, flush_screen};
pub use vmmouse::{poll as mouse_poll, is_absolute};
