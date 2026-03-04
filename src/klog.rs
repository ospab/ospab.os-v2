/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Kernel event log: lock-free ring buffer for last N events.
Used by panic handler and diagnostic tools.
Events are recorded from boot, interrupts, faults, terminal, scheduler, etc.
*/

use core::sync::atomic::{AtomicUsize, Ordering};

/// Maximum length of a single event message
const EVENT_MSG_LEN: usize = 80;
/// Number of events stored in the ring buffer
const EVENT_COUNT: usize = 32;

/// Single event entry
#[derive(Clone, Copy)]
pub struct Event {
    /// Event category / source
    pub source: EventSource,
    /// Human-readable message (null-terminated in buffer)
    pub msg: [u8; EVENT_MSG_LEN],
    pub msg_len: usize,
}

/// Event source categories
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EventSource {
    Boot = 0,
    Memory = 1,
    Interrupt = 2,
    Fault = 3,
    Scheduler = 4,
    Syscall = 5,
    Terminal = 6,
    Driver = 7,
    Panic = 8,
    Unknown = 255,
}

impl EventSource {
    pub fn label(&self) -> &'static str {
        match self {
            EventSource::Boot => "BOOT",
            EventSource::Memory => "MEM ",
            EventSource::Interrupt => "IRQ ",
            EventSource::Fault => "FAIL",
            EventSource::Scheduler => "SCHED",
            EventSource::Syscall => "SYSC",
            EventSource::Terminal => "TERM",
            EventSource::Driver => "DRV ",
            EventSource::Panic => "PANIC",
            EventSource::Unknown => "????",
        }
    }
}

impl Event {
    const fn empty() -> Self {
        Event {
            source: EventSource::Unknown,
            msg: [0u8; EVENT_MSG_LEN],
            msg_len: 0,
        }
    }

    /// Public empty constructor for use in panic handler stack buffers
    pub const fn empty_pub() -> Self {
        Self::empty()
    }

    pub fn message(&self) -> &str {
        unsafe { core::str::from_utf8_unchecked(&self.msg[..self.msg_len]) }
    }
}

/// Global event ring buffer (no heap, static allocation)
static mut EVENTS: [Event; EVENT_COUNT] = [Event::empty(); EVENT_COUNT];
/// Write cursor — always increments, modulo EVENT_COUNT gives slot
static WRITE_INDEX: AtomicUsize = AtomicUsize::new(0);

/// Record an event. Safe to call from any context (ISR, panic, boot).
/// Overwrites oldest event when buffer is full (ring behavior).
pub fn record(source: EventSource, message: &str) {
    let idx = WRITE_INDEX.fetch_add(1, Ordering::Relaxed) % EVENT_COUNT;
    unsafe {
        let event = &mut EVENTS[idx];
        event.source = source;
        let bytes = message.as_bytes();
        let len = bytes.len().min(EVENT_MSG_LEN);
        event.msg[..len].copy_from_slice(&bytes[..len]);
        event.msg_len = len;
    }
}

/// Get the last `count` events in chronological order (oldest first).
/// Returns slice reference and actual count available.
/// Caller provides a buffer to copy into.
pub fn last_events(buf: &mut [Event], count: usize) -> usize {
    let total_written = WRITE_INDEX.load(Ordering::Relaxed);
    if total_written == 0 {
        return 0;
    }

    let available = total_written.min(EVENT_COUNT);
    let requested = count.min(available).min(buf.len());

    // Start reading from (total_written - requested) in ring order
    let start = if total_written >= EVENT_COUNT {
        total_written - requested
    } else {
        total_written.saturating_sub(requested)
    };

    for i in 0..requested {
        let ring_idx = (start + i) % EVENT_COUNT;
        unsafe {
            buf[i] = EVENTS[ring_idx];
        }
    }

    requested
}

/// Total number of events ever recorded
pub fn total_count() -> usize {
    WRITE_INDEX.load(Ordering::Relaxed)
}

/// Convenience: record boot event
pub fn boot(msg: &str) {
    record(EventSource::Boot, msg);
}

/// Convenience: record memory event
pub fn memory(msg: &str) {
    record(EventSource::Memory, msg);
}

/// Convenience: record fault event
pub fn fault(msg: &str) {
    record(EventSource::Fault, msg);
}

/// Convenience: record terminal event
pub fn terminal(msg: &str) {
    record(EventSource::Terminal, msg);
}
