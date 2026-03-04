/*
 * Audio subsystem for AETERNA microkernel
 *
 * Currently implements:
 *   hda — Intel High Definition Audio (QEMU ICH6-HDA, Intel ICH8+)
 *
 * /dev/audio write path:
 *   Any code that writes to /dev/audio goes through audio::write_pcm().
 *   The HDA driver picks it up from the DMA ring buffer.
 */

pub mod hda;

/// Initialize audio subsystem during boot.
/// Returns true if any audio device was found and initialized.
pub fn init() -> bool {
    hda::init()
}

/// Write raw PCM samples to the audio output.
/// Format must match the initialized stream: 44100 Hz, 16-bit LE, 2-channel.
/// Silently drops data if no driver is ready.
pub fn write_pcm(data: &[u8]) {
    hda::write_pcm(data);
}

/// Returns true if the audio driver is initialized and streaming.
pub fn is_ready() -> bool {
    hda::is_ready()
}
