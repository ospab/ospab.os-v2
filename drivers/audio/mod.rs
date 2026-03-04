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

use core::sync::atomic::{AtomicU8, Ordering};

// 0 = none, 1 = AC97, 2 = HDA
static ACTIVE_DRIVER: AtomicU8 = AtomicU8::new(0);

/// Initialize audio subsystem during boot.
///
/// Tries AC97 first (QEMU default with `-soundhw ac97`), then HDA.
/// Returns true if any audio device was found and initialized.
pub fn init() -> bool {
    if ac97::init() {
        ACTIVE_DRIVER.store(1, Ordering::Relaxed);
        ac97::enable_interrupts(); // Enable GIE — must happen AFTER IDT is set up
        return true;
    }
    if hda::init() {
        ACTIVE_DRIVER.store(2, Ordering::Relaxed);
        return true;
    }
    false
}

/// Write raw PCM samples to the audio output.
///
/// Format: 44100 Hz (or 48000 Hz on VRA-less codecs), 16-bit LE, stereo.
/// Non-blocking: excess data is silently dropped if the DMA ring is full.
pub fn write_pcm(data: &[u8]) {
    match ACTIVE_DRIVER.load(Ordering::Relaxed) {
        1 => { ac97::write_pcm(data); }
        2 => { hda::write_pcm(data); }
        _ => {}
    }
}

/// Submit one audio frame from DOOM (or any userland audio source).
///
/// Identical to write_pcm but returns true on full acceptance.
/// Call this from the DOOM audio mix callback.
pub fn play_sample(data: &[u8]) -> bool {
    match ACTIVE_DRIVER.load(Ordering::Relaxed) {
        1 => ac97::play_sample(data),
        2 => { hda::write_pcm(data); true }
        _ => false,
    }
}

/// Returns true if any audio driver is initialized and streaming.
pub fn is_ready() -> bool {
    match ACTIVE_DRIVER.load(Ordering::Relaxed) {
        1 => ac97::is_ready(),
        2 => hda::is_ready(),
        _ => false,
    }
}

/// Called from `idt::irq_dispatch()` when an AC97 IRQ fires.
/// Forwarded only if AC97 is the active driver.
pub fn handle_ac97_irq() {
    if ACTIVE_DRIVER.load(Ordering::Relaxed) == 1 {
        ac97::handle_irq();
    }
}

/// Returns a static string identifying the active audio driver.
pub fn active_driver_name() -> &'static str {
    match ACTIVE_DRIVER.load(Ordering::Relaxed) {
        1 => "AC97",
        2 => "HDA",
        _ => "none",
    }
}
