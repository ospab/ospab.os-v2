/*
 * Audio subsystem for AETERNA microkernel
 *
 * Supports two audio controllers, probed in priority order:
 *
 *   1. ac97 — Intel AC'97 Audio Controller  (QEMU -soundhw ac97 / PCI 8086:2415)
 *              I/O-port DMA, BDL ring, 44100 Hz / 48000 Hz, 16-bit stereo
 *   2. hda  — Intel High Definition Audio   (QEMU ICH6-HDA, Intel ICH8+)
 *              MMIO DMA, CORB/RIRB codecs, 44100 Hz, 16-bit stereo
 *
 * /dev/audio write path:
 *   Any code calling audio::write_pcm() is dispatched to whichever driver
 *   was successfully initialized during boot.
 *
 * DOOM integration:
 *   Call audio::play_sample(data) per mix callback (~35 Hz at 44100 Hz, stereo).
 *   The non-blocking ring buffer absorbs bursts without stalling the game loop.
 */

pub mod ac97;
pub mod hda;
pub mod es1371;

use core::sync::atomic::{AtomicU8, Ordering};

// 0 = none, 1 = AC97, 2 = HDA, 3 = ES1371
static ACTIVE_DRIVER: AtomicU8 = AtomicU8::new(0);

/// Initialize audio subsystem during boot.
///
/// Probe order:
///   1. Intel AC97 (QEMU `-soundhw ac97`, PCI 8086:2415)
///   2. Intel HDA  (QEMU `-device intel-hda`, PCI 8086:2668)
///   3. Ensoniq ES1371/ES1373 (VMware default, PCI 1274:1371)
///
/// Returns true if any audio device was found and initialized.
pub fn init() -> bool {
    if ac97::init() {
        ACTIVE_DRIVER.store(1, Ordering::Relaxed);
        ac97::enable_interrupts();
        return true;
    }
    if hda::init() {
        ACTIVE_DRIVER.store(2, Ordering::Relaxed);
        return true;
    }
    if es1371::init() {
        ACTIVE_DRIVER.store(3, Ordering::Relaxed);
        return true;
    }
    false
}

/// Write raw PCM samples to the audio output.
///
/// Format: 44100 Hz (or 48000 Hz on VRA-less codecs), 16-bit LE, stereo.
/// Non-blocking: excess data is silently dropped if the DMA ring is full.
/// Write raw PCM samples to the audio output.
///
/// Format: 48000 Hz or 44100 Hz, 16-bit LE, stereo.
/// Non-blocking: excess data is silently dropped if the DMA ring is full.
pub fn write_pcm(data: &[u8]) {
    match ACTIVE_DRIVER.load(Ordering::Relaxed) {
        1 => { ac97::write_pcm(data); }
        2 => { hda::write_pcm(data); }
        3 => { es1371::write_pcm(data); }
        _ => {}
    }
}

/// Submit one audio frame from DOOM (or any userland audio source).
pub fn play_sample(data: &[u8]) -> bool {
    match ACTIVE_DRIVER.load(Ordering::Relaxed) {
        1 => ac97::play_sample(data),
        2 => { hda::write_pcm(data); true }
        3 => es1371::play_sample(data),
        _ => false,
    }
}

/// Returns true if any audio driver is initialized and streaming.
pub fn is_ready() -> bool {
    match ACTIVE_DRIVER.load(Ordering::Relaxed) {
        1 => ac97::is_ready(),
        2 => hda::is_ready(),
        3 => es1371::is_ready(),
        _ => false,
    }
}

/// Called from `idt::irq_dispatch()` when an AC97 IRQ fires.
pub fn handle_ac97_irq() {
    if ACTIVE_DRIVER.load(Ordering::Relaxed) == 1 {
        ac97::handle_irq();
    }
}

/// Called from `idt::irq_dispatch()` when the ES1371 PCI IRQ fires.
pub fn handle_es1371_irq() {
    if ACTIVE_DRIVER.load(Ordering::Relaxed) == 3 {
        es1371::handle_irq();
    }
}

/// Returns a static string identifying the active audio driver.
pub fn active_driver_name() -> &'static str {
    match ACTIVE_DRIVER.load(Ordering::Relaxed) {
        1 => "AC97",
        2 => "HDA",
        3 => "ES1371",
        _ => "none",
    }
}

/// Dump driver-specific diagnostics to serial.
/// Called by `soundtest` command.
pub fn dump_status() {
    match ACTIVE_DRIVER.load(Ordering::Relaxed) {
        1 => ac97::dump_status(),
        3 => es1371::dump_status(),
        _ => {}
    }
}

/// Dump driver memory map to serial (where applicable).
pub fn dump_mem_map() {
    match ACTIVE_DRIVER.load(Ordering::Relaxed) {
        1 => ac97::dump_mem_map(),
        _ => {}
    }
}

/// Returns the PCI IRQ line used by the active audio driver.
pub fn irq_line() -> u8 {
    match ACTIVE_DRIVER.load(Ordering::Relaxed) {
        1 => ac97::irq_line(),
        3 => es1371::irq_line(),
        _ => 0,
    }
}

/// Returns the PCM sample rate used by the active driver (Hz).
pub fn sample_rate() -> u32 {
    match ACTIVE_DRIVER.load(Ordering::Relaxed) {
        3 => es1371::sample_rate(), // actual rate programmed into the ES1371 codec
        _ => 44100,                 // AC97 / HDA default to 44100 Hz
    }
}
