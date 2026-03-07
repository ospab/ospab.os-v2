/*
 * ps — process status from /sys/status
 *
 * Standalone logical unit.  Does NOT call scheduler APIs directly.
 * Instead it reads /sys/status from VFS and parses the output \u2014
 * exactly like a real userspace binary would via /proc.
 *
 * Usage: ps
 */

extern crate alloc;
use crate::arch::x86_64::framebuffer;

const FG: u32     = 0x00FFFFFF;
const FG_OK: u32  = 0x0000FF00;
const FG_ERR: u32 = 0x00FF4444;
const FG_DIM: u32 = 0x00AAAAAA;
const FG_HL: u32  = 0x00FFCC00;
const BG: u32     = 0x00000000;

fn puts(s: &str)  { framebuffer::draw_string(s, FG, BG); }
fn hl(s: &str)    { framebuffer::draw_string(s, FG_HL, BG); }
fn dim(s: &str)   { framebuffer::draw_string(s, FG_DIM, BG); }

fn state_color(state: &str, s: &str) {
    let color = if state.starts_with("running") {
        FG_OK
    } else if state.starts_with("dead") {
        FG_ERR
    } else {
        FG_DIM
    };
    framebuffer::draw_string(s, color, BG);
}

/// Refresh /sys/status by reading the live task table and writing it to VFS.
/// This is the in-kernel equivalent of what a real /proc subsystem would do.
fn write_sys_status() {
    use crate::core::scheduler::{get_tasks, TaskSnapshot, TaskState, Priority, name_from_snapshot};
    use alloc::vec::Vec;

    let mut snap = [TaskSnapshot {
        pid: 0, priority: Priority::Idle, state: TaskState::Dead,
        cr3: 0, cpu_ticks: 0, memory_bytes: 0, name: [0; 24], name_len: 0,
    }; 64];
    let n = get_tasks(&mut snap);

    let mut out: Vec<u8> = Vec::with_capacity(n * 48 + 64);
    for &b in b"PID   TICKS      STATE    NAME\n" { out.push(b); }
    for &b in b"---   -----      -----    ----\n" { out.push(b); }

    for s in snap.iter().take(n) {
        // PID
        let pid_s = dec_u64(s.pid as u64);
        push_str(&mut out, &pid_s);
        pad(&mut out, pid_s.len(), 6);

        // Ticks
        let tick_s = dec_u64(s.cpu_ticks);
        push_str(&mut out, &tick_s);
        pad(&mut out, tick_s.len(), 11);

        // State (9 chars fixed)
        let state: &[u8] = match s.state {
            TaskState::Running => b"Running  ",
            TaskState::Ready   => b"Ready    ",
            TaskState::Waiting => b"Waiting  ",
            TaskState::Dead    => b"Dead     ",
        };
        for &b in state { out.push(b); }

        // Name
        let name = name_from_snapshot(s);
        for b in name.bytes() { out.push(b); }
        out.push(b'\n');
    }

    crate::fs::write_file("/sys/status", &out);
}

fn dec_u64(mut n: u64) -> alloc::string::String {
    if n == 0 { return alloc::string::String::from("0"); }
    let mut buf = [0u8; 20];
    let mut i = 0usize;
    while n > 0 { buf[i] = b'0' + (n % 10) as u8; n /= 10; i += 1; }
    buf[..i].reverse();
    alloc::string::String::from(core::str::from_utf8(&buf[..i]).unwrap_or("?"))
}

fn push_str(v: &mut alloc::vec::Vec<u8>, s: &str) {
    for b in s.bytes() { v.push(b); }
}

fn pad(v: &mut alloc::vec::Vec<u8>, cur: usize, target: usize) {
    for _ in cur..target { v.push(b' '); }
}

/// Entry point: refreshes /sys/status via scheduler APIs, then reads and
/// displays it \u2014 exactly like a real userspace binary reading /proc/ps.
pub fn run(_args: &str) {
    // Step 1: refresh /sys/status \u2014 write current task snapshot to VFS.
    // In true userspace this would be a kernel syscall; here it's an in-kernel
    // call to the same function used by the boot sequence.
    write_sys_status();

    // Step 2: read /sys/status from VFS and parse it.
    let data = match crate::fs::read_file("/sys/status") {
        Some(d) => d,
        None => {
            framebuffer::draw_string("ps: /sys/status not found\n", FG_ERR, BG);
            return;
        }
    };

    let text = match core::str::from_utf8(&data) {
        Ok(t) => t,
        Err(_) => {
            framebuffer::draw_string("ps: /sys/status is not valid UTF-8\n", FG_ERR, BG);
            return;
        }
    };

    // Column header (our own formatting, nicer than raw file)
    hl("  PID   TICKS      STATE    NAME\n");
    dim("  --------------------------------\n");

    // Skip the two header lines from /sys/status, parse data lines
    let mut line_num = 0usize;
    for line in text.lines() {
        line_num += 1;
        if line_num <= 2 { continue; } // skip PID header + dash line

        // Format is: PID(6) TICKS(11) STATE(9) NAME
        // Fields are fixed-width padded with spaces.
        let pid   = line.get(0..6).map(|s| s.trim()).unwrap_or("-");
        let ticks = line.get(6..17).map(|s| s.trim()).unwrap_or("-");
        let state = line.get(17..26).map(|s| s.trim()).unwrap_or("-");
        let name  = line.get(26..).map(|s| s.trim()).unwrap_or("?");

        // PID
        puts("  ");
        puts(pid);
        for _ in pid.len()..6 { puts(" "); }

        // Ticks
        puts(ticks);
        for _ in ticks.len()..11 { puts(" "); }

        // State (coloured)
        state_color(state, state);
        for _ in state.len()..9 { puts(" "); }

        // Name
        hl(name.trim());
        puts("\n");
    }

    if line_num <= 2 {
        dim("  (no tasks)\n");
    }
}
