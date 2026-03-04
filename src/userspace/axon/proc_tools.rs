/*
 * axon/proc_tools — Process management commands: ps, top, kill
 *
 * Self-contained logical unit: every dependency accessed through crate:: paths.
 * Output helpers are inlined so this module compiles independently.
 */

extern crate alloc;
use alloc::format;

use crate::arch::x86_64::framebuffer;

const FG: u32    = 0x00FFFFFF;
const FG_OK: u32 = 0x0000FF00;
const FG_ERR: u32 = 0x00FF4444;
const FG_DIM: u32 = 0x00AAAAAA;
const FG_HL: u32  = 0x00FFCC00;
const BG: u32    = 0x00000000;

fn puts(s: &str)  { framebuffer::draw_string(s, FG, BG); }
fn ok(s: &str)    { framebuffer::draw_string(s, FG_OK, BG); }
fn err(s: &str)   { framebuffer::draw_string(s, FG_ERR, BG); }
fn dim(s: &str)   { framebuffer::draw_string(s, FG_DIM, BG); }
fn hl(s: &str)    { framebuffer::draw_string(s, FG_HL, BG); }

fn put_usize(mut n: usize) {
    if n == 0 { puts("0"); return; }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while n > 0 { buf[i] = b'0' + (n % 10) as u8; n /= 10; i += 1; }
    for k in (0..i).rev() { framebuffer::draw_char(buf[k] as char, FG, BG); }
}

// ─── ps ─────────────────────────────────────────────────────────────────────

pub fn cmd_ps(_args: &str) {
    use crate::core::scheduler::{get_tasks, TaskSnapshot, TaskState, name_from_snapshot};
    let mut snap = [TaskSnapshot {
        pid: 0,
        priority: crate::core::scheduler::Priority::Idle,
        state: TaskState::Dead,
        cr3: 0,
        cpu_ticks: 0,
        memory_bytes: 0,
        name: [0; 24],
        name_len: 0,
    }; 64];
    let count = get_tasks(&mut snap);

    hl("  PID   STAT   MEM(KiB)   COMMAND\n");
    dim("  -------------------------------------\n");
    for i in 0..count {
        let t = &snap[i];
        puts("  ");
        let pid_s = format!("{}", t.pid);
        puts(&pid_s);
        for _ in 0..(6usize.saturating_sub(pid_s.len())) { puts(" "); }
        let st = match t.state {
            TaskState::Running => "R",
            TaskState::Ready   => "S",
            TaskState::Waiting => "W",
            TaskState::Dead    => "Z",
        };
        match t.state {
            TaskState::Running => ok(st),
            TaskState::Dead    => err(st),
            _                  => dim(st),
        }
        puts("      ");
        let mem_s = format!("{}", t.memory_bytes / 1024);
        puts(&mem_s);
        for _ in 0..(11usize.saturating_sub(mem_s.len())) { puts(" "); }
        hl(name_from_snapshot(t));
        puts("\n");
    }
    if count == 0 { dim("  (no tasks)\n"); }
}

// ─── top ─────────────────────────────────────────────────────────────────────

pub fn cmd_top(_args: &str) {
    use crate::core::scheduler::{get_tasks, TaskSnapshot, TaskState, name_from_snapshot};
    let mut snap = [TaskSnapshot {
        pid: 0,
        priority: crate::core::scheduler::Priority::Idle,
        state: TaskState::Dead,
        cr3: 0,
        cpu_ticks: 0,
        memory_bytes: 0,
        name: [0; 24],
        name_len: 0,
    }; 64];
    let count = get_tasks(&mut snap);

    let total_ticks: u64 = snap[..count].iter().map(|t| t.cpu_ticks).sum();

    hl("  top \u{2014} task snapshot\n");
    dim("  PID   CPU%   MEM(KiB)   TICKS      STATE    COMMAND\n");
    dim("  ---------------------------------------------------\n");
    for i in 0..count {
        let t = &snap[i];
        puts("  ");
        let pid_s = format!("{}", t.pid);
        puts(&pid_s);
        for _ in 0..(6usize.saturating_sub(pid_s.len())) { puts(" "); }
        let cpu_pct = if total_ticks > 0 { t.cpu_ticks * 100 / total_ticks } else { 0 };
        let cpu_s = format!("{}%", cpu_pct);
        puts(&cpu_s);
        for _ in 0..(7usize.saturating_sub(cpu_s.len())) { puts(" "); }
        let mem_s = format!("{}", t.memory_bytes / 1024);
        puts(&mem_s);
        for _ in 0..(11usize.saturating_sub(mem_s.len())) { puts(" "); }
        let tick_s = format!("{}", t.cpu_ticks);
        puts(&tick_s);
        for _ in 0..(11usize.saturating_sub(tick_s.len())) { puts(" "); }
        let st = match t.state {
            TaskState::Running => "Running",
            TaskState::Ready   => "Ready  ",
            TaskState::Waiting => "Waiting",
            TaskState::Dead    => "Dead   ",
        };
        match t.state {
            TaskState::Running => ok(st),
            TaskState::Dead    => err(st),
            _                  => dim(st),
        }
        puts("  ");
        hl(name_from_snapshot(t));
        puts("\n");
    }
    if count == 0 { dim("  (no tasks)\n"); }
}

// ─── kill ─────────────────────────────────────────────────────────────────────

pub fn cmd_kill(args: &str) {
    let args = args.trim();
    if args.is_empty() {
        err("kill: missing PID\n");
        dim("Usage: kill [-SIGNAL] <pid>\n");
        dim("       kill -l    (list signals)\n");
        return;
    }
    if args == "-l" {
        hl("  Signals available:\n");
        puts("   1) SIGHUP    2) SIGINT    3) SIGQUIT   4) SIGILL\n");
        puts("   9) SIGKILL  15) SIGTERM  17) SIGCHLD  18) SIGCONT\n");
        puts("  19) SIGSTOP  20) SIGTSTP\n");
        return;
    }
    let (sig, pid_str) = if args.starts_with('-') {
        let rest = &args[1..];
        if let Some(pos) = rest.find(' ') {
            (&rest[..pos], rest[pos + 1..].trim())
        } else {
            (rest, "")
        }
    } else { ("15", args) };

    let pid: usize = pid_str.parse().unwrap_or(usize::MAX);
    if pid == usize::MAX {
        err("kill: invalid PID\n");
        return;
    }
    if pid <= 1 {
        err("kill: cannot kill kernel thread\n");
        return;
    }
    if crate::core::scheduler::signal_pid(pid as u32, sig.parse().unwrap_or(15)) {
        ok("kill: sent SIG"); ok(sig); ok(" to PID "); ok(pid_str); ok("\n");
    } else {
        err("kill: ("); err(pid_str); err(") No such process\n");
    }
}
