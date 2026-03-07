/*
Business Source License 1.1
Copyright (c) 2026 ospab

seed — Init system for ospab.os (AETERNA)

The first logical process (PID 1 equivalent) in AETERNA.
Responsible for:
  - Mounting filesystems
  - Starting system services (drivers, daemons)
  - Launching the plum shell
  - Service supervision (restart on crash)
  - Clean shutdown sequence

Configuration: /etc/seed/init.conf

Service entries format (one per line):
  service:<name>:<command>:<restart_policy>
  mount:<device>:<mountpoint>:<fstype>

Restart policies:
  always    — restart immediately on exit
  once      — run once at boot, don't restart
  manual    — only start when explicitly requested

Since AETERNA doesn't yet have userspace process isolation,
seed operates as a kernel-level init module that coordinates
the boot sequence and manages service lifecycle state.
*/

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

// ─── Service state ──────────────────────────────────────────────────────────

/// Service status
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatus {
    Stopped,
    Running,
    Failed,
    Disabled,
}

/// Restart policy
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RestartPolicy {
    Always,
    Once,
    Manual,
}

/// A managed service
#[derive(Clone)]
pub struct Service {
    pub name: String,
    pub command: String,
    pub policy: RestartPolicy,
    pub status: ServiceStatus,
    pub pid: u32,
    pub restarts: u32,
    pub description: String,
}

/// Maximum services
const MAX_SERVICES: usize = 32;

/// Global service table
static mut SERVICES: Option<Vec<Service>> = None;
static mut INIT_DONE: bool = false;
static mut BOOT_TIME_TICKS: u64 = 0;

// ─── Framebuffer output helpers ─────────────────────────────────────────────

use crate::arch::x86_64::framebuffer;
use crate::arch::x86_64::serial;

const FG: u32       = 0x00FFFFFF;
const FG_OK: u32    = 0x0000FF00;
const FG_ERR: u32   = 0x000000FF;
const FG_WARN: u32  = 0x0000FFFF;
const FG_DIM: u32   = 0x00AAAAAA;
const FG_SVC: u32   = 0x0000FF00;
const BG: u32       = 0x00000000;

fn putc(s: &str) { framebuffer::draw_string(s, FG, BG); }
fn ok(s: &str)   { framebuffer::draw_string(s, FG_OK, BG); }
fn errf(s: &str)  { framebuffer::draw_string(s, FG_ERR, BG); }
fn warnf(s: &str) { framebuffer::draw_string(s, FG_WARN, BG); }
fn dim(s: &str)  { framebuffer::draw_string(s, FG_DIM, BG); }

// ─── Initialization ─────────────────────────────────────────────────────────

/// Initialize the seed init system. Called during kernel boot.
/// Sets up default services and reads init.conf.
pub fn init() {
    unsafe {
        if INIT_DONE { return; }

        BOOT_TIME_TICKS = crate::arch::x86_64::idt::timer_ticks();

        let mut services = Vec::with_capacity(MAX_SERVICES);

        let seed_pid = crate::core::scheduler::spawn_named(
            "seed",
            crate::core::scheduler::Priority::System,
            0,
            0,
            0,
        ).unwrap_or(1);

        services.push(Service {
            name: String::from("seed"),
            command: String::from("/sbin/seed"),
            policy: RestartPolicy::Always,
            status: ServiceStatus::Running,
            pid: seed_pid,
            restarts: 0,
            description: String::from("Init system (PID 1)"),
        });

        // Core system services — registered by default
        services.push(Service {
            name: String::from("kernel"),
            command: String::from("aeterna"),
            policy: RestartPolicy::Once,
            status: ServiceStatus::Running,
            pid: 0,
            restarts: 0,
            description: String::from("AETERNA microkernel"),
        });

        services.push(Service {
            name: String::from("vfs"),
            command: String::from("vfs-mount"),
            policy: RestartPolicy::Once,
            status: ServiceStatus::Stopped,
            pid: 0,
            restarts: 0,
            description: String::from("Virtual filesystem (RamFS at /)"),
        });

        services.push(Service {
            name: String::from("scheduler"),
            command: String::from("sched"),
            policy: RestartPolicy::Always,
            status: ServiceStatus::Stopped,
            pid: 0,
            restarts: 0,
            description: String::from("Compute-First task scheduler"),
        });

        services.push(Service {
            name: String::from("console"),
            command: String::from("fbconsole"),
            policy: RestartPolicy::Always,
            status: ServiceStatus::Stopped,
            pid: 0,
            restarts: 0,
            description: String::from("Framebuffer console driver"),
        });

        services.push(Service {
            name: String::from("serial"),
            command: String::from("serial-log"),
            policy: RestartPolicy::Always,
            status: ServiceStatus::Stopped,
            pid: 0,
            restarts: 0,
            description: String::from("Serial port logger (COM1)"),
        });

        services.push(Service {
            name: String::from("keyboard"),
            command: String::from("kbd-driver"),
            policy: RestartPolicy::Always,
            status: ServiceStatus::Stopped,
            pid: 0,
            restarts: 0,
            description: String::from("PS/2 keyboard driver"),
        });

        services.push(Service {
            name: String::from("network"),
            command: String::from("net-stack"),
            policy: RestartPolicy::Always,
            status: ServiceStatus::Stopped,
            pid: 0,
            restarts: 0,
            description: String::from("Network stack (RTL8139, IPv4)"),
        });

        services.push(Service {
            name: String::from("storage"),
            command: String::from("storage-drv"),
            policy: RestartPolicy::Once,
            status: ServiceStatus::Stopped,
            pid: 0,
            restarts: 0,
            description: String::from("ATA/AHCI storage driver"),
        });

        services.push(Service {
            name: String::from("plum"),
            command: String::from("/bin/plum"),
            policy: RestartPolicy::Always,
            status: ServiceStatus::Stopped,
            pid: 0,
            restarts: 0,
            description: String::from("plum command shell"),
        });

        SERVICES = Some(services);
        INIT_DONE = true;
    }

    // Create configuration directory and default config
    crate::fs::mkdir("/etc/seed");
    if !crate::fs::exists("/etc/seed/init.conf") {
        let default_conf = concat!(
            "# seed init configuration\n",
            "# Format: service:<name>:<command>:<policy>\n",
            "# Policies: always, once, manual\n",
            "\n",
            "service:kernel:aeterna:once\n",
            "service:vfs:vfs-mount:once\n",
            "service:scheduler:sched:always\n",
            "service:console:fbconsole:always\n",
            "service:serial:serial-log:always\n",
            "service:keyboard:kbd-driver:always\n",
            "service:network:net-stack:always\n",
            "service:storage:storage-drv:once\n",
            "service:plum:/bin/plum:always\n",
        );
        crate::fs::write_file("/etc/seed/init.conf", default_conf.as_bytes());
    }

    // Dynamically spawn all non-manual services after registration.
    unsafe {
        if let Some(services) = SERVICES.as_mut() {
            for svc in services.iter_mut() {
                // skip kernel (not spawnable) and seed (already spawned above)
                if svc.name.as_str() == "kernel" || svc.name.as_str() == "seed" {
                    continue;
                }
                if svc.policy != RestartPolicy::Manual {
                    let prio = if svc.name.as_str() == "plum" {
                        crate::core::scheduler::Priority::Normal
                    } else {
                        crate::core::scheduler::Priority::System
                    };
                    if let Some(pid) = crate::core::scheduler::spawn_named(
                        svc.name.as_str(),
                        prio,
                        0,
                        0,
                        0,
                    ) {
                        svc.pid = pid;
                        svc.status = ServiceStatus::Running;
                    } else {
                        svc.status = ServiceStatus::Failed;
                    }
                }
            }
        }
    }

    serial::write_str("[seed] Init system ready (");
    serial_u32(service_count() as u32);
    serial::write_str(" services)\r\n");
}

// ─── Service management ─────────────────────────────────────────────────────

/// Get the total number of services
pub fn service_count() -> usize {
    unsafe {
        SERVICES.as_ref().map(|s| s.len()).unwrap_or(0)
    }
}

/// Get a service by index
pub fn get_service(index: usize) -> Option<&'static Service> {
    unsafe {
        SERVICES.as_ref()?.get(index)
    }
}

/// Find a service by name
pub fn find_service(name: &str) -> Option<&'static Service> {
    unsafe {
        SERVICES.as_ref()?.iter().find(|s| s.name.as_str() == name)
    }
}

/// Start a service
pub fn start_service(name: &str) -> bool {
    unsafe {
        if let Some(services) = SERVICES.as_mut() {
            if let Some(svc) = services.iter_mut().find(|s| s.name.as_str() == name) {
                if svc.status == ServiceStatus::Stopped || svc.status == ServiceStatus::Failed {
                    let prio = if svc.name.as_str() == "plum" {
                        crate::core::scheduler::Priority::Normal
                    } else {
                        crate::core::scheduler::Priority::System
                    };
                    if let Some(pid) = crate::core::scheduler::spawn_named(
                        svc.name.as_str(),
                        prio,
                        0,
                        0,
                        0,
                    ) {
                        svc.pid = pid;
                        svc.status = ServiceStatus::Running;
                        svc.restarts += 1;
                        return true;
                    }
                    svc.status = ServiceStatus::Failed;
                }
            }
        }
    }
    false
}

/// Stop a service
pub fn stop_service(name: &str) -> bool {
    unsafe {
        if let Some(services) = SERVICES.as_mut() {
            if let Some(svc) = services.iter_mut().find(|s| s.name.as_str() == name) {
                if svc.status == ServiceStatus::Running {
                    if svc.pid > 1 {
                        let _ = crate::core::scheduler::exit_pid(svc.pid);
                    }
                    svc.status = ServiceStatus::Stopped;
                    return true;
                }
            }
        }
    }
    false
}

/// Restart a service
pub fn restart_service(name: &str) -> bool {
    stop_service(name);
    start_service(name)
}

/// Enable a disabled service
pub fn enable_service(name: &str) -> bool {
    unsafe {
        if let Some(services) = SERVICES.as_mut() {
            if let Some(svc) = services.iter_mut().find(|s| s.name.as_str() == name) {
                if svc.status == ServiceStatus::Disabled {
                    svc.status = ServiceStatus::Stopped;
                    return true;
                }
            }
        }
    }
    false
}

/// Disable a service
pub fn disable_service(name: &str) -> bool {
    unsafe {
        if let Some(services) = SERVICES.as_mut() {
            if let Some(svc) = services.iter_mut().find(|s| s.name.as_str() == name) {
                svc.status = ServiceStatus::Disabled;
                return true;
            }
        }
    }
    false
}

// ─── Terminal interface ─────────────────────────────────────────────────────

/// Execute a seed command from the terminal.
/// Usage:
///   seed status              — show all services
///   seed start <service>     — start a service
///   seed stop <service>      — stop a service
///   seed restart <service>   — restart a service
///   seed enable <service>    — enable a service
///   seed disable <service>   — disable a service
///   seed log                 — show init log
pub fn run(args: &str) {
    let args = args.trim();

    if args.is_empty() || args == "status" {
        cmd_status();
    } else if args.starts_with("start ") {
        let name = args[6..].trim();
        if start_service(name) {
            ok("  Started: ");
            putc(name);
            putc("\n");
        } else {
            errf("  Failed to start: ");
            putc(name);
            putc("\n");
        }
    } else if args.starts_with("stop ") {
        let name = args[5..].trim();
        if stop_service(name) {
            warnf("  Stopped: ");
            putc(name);
            putc("\n");
        } else {
            errf("  Failed to stop: ");
            putc(name);
            putc("\n");
        }
    } else if args.starts_with("restart ") {
        let name = args[8..].trim();
        if restart_service(name) {
            ok("  Restarted: ");
            putc(name);
            putc("\n");
        } else {
            errf("  Failed to restart: ");
            putc(name);
            putc("\n");
        }
    } else if args.starts_with("enable ") {
        let name = args[7..].trim();
        if enable_service(name) {
            ok("  Enabled: ");
            putc(name);
            putc("\n");
        } else {
            errf("  Cannot enable: ");
            putc(name);
            putc("\n");
        }
    } else if args.starts_with("disable ") {
        let name = args[8..].trim();
        if disable_service(name) {
            warnf("  Disabled: ");
            putc(name);
            putc("\n");
        } else {
            errf("  Cannot disable: ");
            putc(name);
            putc("\n");
        }
    } else if args == "log" {
        cmd_log();
    } else if args == "--help" || args == "help" {
        cmd_help();
    } else {
        errf("seed: ");
        putc("unknown command '");
        putc(args);
        putc("'\n");
        dim("Try 'seed --help' for usage.\n");
    }
}

fn cmd_status() {
    putc("\n");
    framebuffer::draw_string("  seed", FG_SVC, BG);
    putc(" — AETERNA init system\n");
    putc("  ═══════════════════════════════════════\n\n");

    // Header
    framebuffer::draw_string("  SERVICE      STATUS      PID  RESTARTS  DESCRIPTION\n", FG_WARN, BG);
    framebuffer::draw_string("  ─────────────────────────────────────────────────────\n", FG_DIM, BG);

    let count = service_count();
    for i in 0..count {
        if let Some(svc) = get_service(i) {
            putc("  ");
            // Name (padded to 13 chars)
            framebuffer::draw_string(&svc.name, FG_SVC, BG);
            let pad = if svc.name.len() < 13 { 13 - svc.name.len() } else { 1 };
            for _ in 0..pad { putc(" "); }

            // Status
            match svc.status {
                ServiceStatus::Running  => ok("running "),
                ServiceStatus::Stopped  => dim("stopped "),
                ServiceStatus::Failed   => errf("failed  "),
                ServiceStatus::Disabled => dim("disabled"),
            }
            putc("    ");

            // PID
            putc(&usize_to_string(svc.pid as usize));
            let pid_len = usize_to_string(svc.pid as usize).len();
            let pid_pad = if pid_len < 5 { 5 - pid_len } else { 1 };
            for _ in 0..pid_pad { putc(" "); }

            // Restarts
            putc(&usize_to_string(svc.restarts as usize));
            let rst_len = usize_to_string(svc.restarts as usize).len();
            let rst_pad = if rst_len < 10 { 10 - rst_len } else { 1 };
            for _ in 0..rst_pad { putc(" "); }

            // Description
            dim(&svc.description);
            putc("\n");
        }
    }

    putc("\n");
    let running = (0..count).filter(|&i| {
        get_service(i).map(|s| s.status == ServiceStatus::Running).unwrap_or(false)
    }).count();
    dim("  ");
    dim(&usize_to_string(running));
    dim(" of ");
    dim(&usize_to_string(count));
    dim(" services running\n\n");
}

fn cmd_log() {
    putc("\n");
    framebuffer::draw_string("  kernel boot log\n", FG_WARN, BG);
    putc("  ═════════════════\n\n");

    let mut buf = [crate::klog::Event::empty_pub(); 32];
    let n = crate::klog::last_events(&mut buf, 32);

    if n == 0 {
        framebuffer::draw_string("  (no log entries)\n", FG_DIM, BG);
    } else {
        for i in 0..n {
            let ev = &buf[i];
            putc("  [");
            framebuffer::draw_string(ev.source.label(), FG_DIM, BG);
            putc("] ");
            framebuffer::draw_string(ev.message(), FG, BG);
            putc("\n");
        }
    }
    putc("\n");
}

fn cmd_help() {
    putc("\n");
    framebuffer::draw_string("seed", FG_SVC, BG);
    putc(" — init system for ospab.os (PID 1)\n\n");

    framebuffer::draw_string("  Usage:\n", FG_WARN, BG);
    putc("    seed status              Show all services\n");
    putc("    seed start <service>     Start a service\n");
    putc("    seed stop <service>      Stop a service\n");
    putc("    seed restart <service>   Restart a service\n");
    putc("    seed enable <service>    Enable a disabled service\n");
    putc("    seed disable <service>   Disable a service\n");
    putc("    seed log                 Show boot log\n");
    putc("    seed --help              Show this help\n\n");

    framebuffer::draw_string("  Config:\n", FG_WARN, BG);
    dim("    /etc/seed/init.conf — service definitions\n\n");
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn serial_u32(mut n: u32) {
    if n == 0 { serial::write_byte(b'0'); return; }
    let mut buf = [0u8; 10];
    let mut pos = 0;
    while n > 0 {
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
        pos += 1;
    }
    for i in (0..pos).rev() {
        serial::write_byte(buf[i]);
    }
}

fn usize_to_string(mut n: usize) -> String {
    if n == 0 { return String::from("0"); }
    let mut buf = [0u8; 20];
    let mut pos = 20;
    while n > 0 {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    String::from(core::str::from_utf8(&buf[pos..]).unwrap_or("0"))
}
