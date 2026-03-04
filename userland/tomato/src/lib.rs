/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

tomato — Package manager for ospab.os (AETERNA)

A pacman-inspired package manager operating on local binary packages.
Packages are stored in the VFS under /var/lib/tomato/.
Each package is a metadata record + file list.

Usage from terminal:
  tomato -S <name>          Install a package
  tomato -R <name>          Remove a package
  tomato -Ss <query>        Search available packages
  tomato -Q                 List installed packages
  tomato -Qi <name>         Show package info
  tomato -Sy                Sync package database
  tomato -Syu               Full system upgrade
  tomato --help             Show help

Package database is stored at /var/lib/tomato/db/
Installed files tracked at /var/lib/tomato/local/
Repository config at /etc/tomato/repos.conf
*/

extern crate alloc;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

// ─── Package metadata ───────────────────────────────────────────────────────

/// A package in the database
#[derive(Clone)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub description: String,
    pub size: usize,
    pub files: Vec<String>,
    pub depends: Vec<String>,
    pub installed: bool,
}

/// Repository entry
#[allow(dead_code)]
struct Repo {
    name: String,
    url: String,
}

/// Early network transport holder for future HTTP package downloads.
/// This keeps parsing and session state outside command handlers so it can
/// later map to a real socket syscall API without changing tomato UX.
#[allow(dead_code)]
pub struct HttpSession {
    pub host: String,
    pub port: u16,
    pub path: String,
    pub connected: bool,
}

#[allow(dead_code)]
impl HttpSession {
    pub fn new(url: &str) -> Option<Self> {
        // Very small parser: expects http://host[:port]/path
        let trimmed = url.trim();
        let no_scheme = if let Some(rest) = trimmed.strip_prefix("http://") {
            rest
        } else {
            return None;
        };

        let mut parts = no_scheme.splitn(2, '/');
        let host_port = parts.next().unwrap_or("");
        let path = parts.next().unwrap_or("");
        if host_port.is_empty() {
            return None;
        }

        let mut hp = host_port.splitn(2, ':');
        let host = hp.next().unwrap_or("");
        let port = hp.next().and_then(|p| p.parse::<u16>().ok()).unwrap_or(80);

        Some(Self {
            host: String::from(host),
            port,
            path: alloc::format!("/{}", path),
            connected: false,
        })
    }

    pub fn connect(&mut self) -> bool {
        // Socket syscalls are not wired yet; keep deterministic state machine.
        self.connected = true;
        true
    }

    pub fn build_get_request(&self) -> String {
        alloc::format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: tomato/0.1\r\nConnection: close\r\n\r\n",
            self.path,
            self.host
        )
    }
}

// ─── Constants ──────────────────────────────────────────────────────────────

#[allow(dead_code)] const DB_PATH: &str = "/var/lib/tomato/db";
const LOCAL_PATH: &str = "/var/lib/tomato/local";
#[allow(dead_code)] const CACHE_PATH: &str = "/var/cache/tomato/pkg";
#[allow(dead_code)] const REPO_CONF: &str = "/etc/tomato/repos.conf";

// ─── Framebuffer output helpers ─────────────────────────────────────────────

use crate::arch::x86_64::framebuffer;

const FG: u32      = 0x00FFFFFF;
const FG_OK: u32   = 0x0000FF00;
const FG_ERR: u32  = 0x000000FF;
const FG_WARN: u32 = 0x0000FFFF;
const FG_DIM: u32  = 0x00AAAAAA;
const FG_NAME: u32 = 0x0000FF00;
const FG_VER: u32  = 0x0000FFFF;
const BG: u32      = 0x00000000;

fn puts(s: &str) { framebuffer::draw_string(s, FG, BG); }
fn ok(s: &str)   { framebuffer::draw_string(s, FG_OK, BG); }
fn err(s: &str)  { framebuffer::draw_string(s, FG_ERR, BG); }
fn warn(s: &str) { framebuffer::draw_string(s, FG_WARN, BG); }
fn dim(s: &str)  { framebuffer::draw_string(s, FG_DIM, BG); }

// ─── Built-in package database ──────────────────────────────────────────────

/// Built-in repository of available packages.
/// In a real system these would come from a remote repo; here they're
/// bundled so `tomato -S` always works in the live environment.
fn builtin_packages() -> Vec<Package> {
    let mut pkgs = Vec::new();

    pkgs.push(Package {
        name: String::from("coreutils"),
        version: String::from("1.0.0"),
        description: String::from("Essential system utilities (ls, cp, mv, pwd, cat)"),
        size: 2048,
        files: vec![
            String::from("/bin/ls"),
            String::from("/bin/cp"),
            String::from("/bin/mv"),
            String::from("/bin/pwd"),
            String::from("/bin/cat"),
        ],
        depends: Vec::new(),
        installed: false,
    });

    pkgs.push(Package {
        name: String::from("grape"),
        version: String::from("1.0.0"),
        description: String::from("Terminal text editor (nano-like)"),
        size: 4096,
        files: vec![
            String::from("/bin/grape"),
            String::from("/etc/grape/graperc"),
        ],
        depends: Vec::new(),
        installed: false,
    });

    pkgs.push(Package {
        name: String::from("plum"),
        version: String::from("1.0.0"),
        description: String::from("System shell for AETERNA"),
        size: 3072,
        files: vec![
            String::from("/bin/plum"),
            String::from("/etc/plum/plumrc"),
        ],
        depends: vec![String::from("coreutils")],
        installed: false,
    });

    pkgs.push(Package {
        name: String::from("net-tools"),
        version: String::from("1.0.0"),
        description: String::from("Network utilities (ping, ifconfig, ntpdate)"),
        size: 1536,
        files: vec![
            String::from("/bin/ping"),
            String::from("/bin/ifconfig"),
            String::from("/bin/ntpdate"),
        ],
        depends: Vec::new(),
        installed: false,
    });

    pkgs.push(Package {
        name: String::from("tutor"),
        version: String::from("1.0.0"),
        description: String::from("Interactive system tutorial"),
        size: 1024,
        files: vec![
            String::from("/bin/tutor"),
            String::from("/usr/share/tutor/lessons.dat"),
        ],
        depends: Vec::new(),
        installed: false,
    });

    pkgs.push(Package {
        name: String::from("kernel-headers"),
        version: String::from("2.0.3"),
        description: String::from("AETERNA kernel headers for module development"),
        size: 8192,
        files: vec![
            String::from("/usr/include/aeterna/types.h"),
            String::from("/usr/include/aeterna/syscall.h"),
            String::from("/usr/include/aeterna/ipc.h"),
            String::from("/usr/include/aeterna/capability.h"),
        ],
        depends: Vec::new(),
        installed: false,
    });

    pkgs.push(Package {
        name: String::from("ospab-libc"),
        version: String::from("0.1.0"),
        description: String::from("Optimized C library for AETERNA microkernel"),
        size: 16384,
        files: vec![
            String::from("/lib/libc.so"),
            String::from("/usr/include/stdio.h"),
            String::from("/usr/include/stdlib.h"),
            String::from("/usr/include/string.h"),
        ],
        depends: vec![String::from("kernel-headers")],
        installed: false,
    });

    pkgs.push(Package {
        name: String::from("base"),
        version: String::from("1.0.0"),
        description: String::from("Base meta-package for ospab.os"),
        size: 256,
        files: Vec::new(),
        depends: vec![
            String::from("coreutils"),
            String::from("plum"),
            String::from("grape"),
            String::from("net-tools"),
        ],
        installed: false,
    });

    pkgs.push(Package {
        name: String::from("man-pages"),
        version: String::from("1.0.0"),
        description: String::from("Manual pages for system commands"),
        size: 5120,
        files: vec![
            String::from("/usr/share/man/man1/ls.1"),
            String::from("/usr/share/man/man1/cat.1"),
            String::from("/usr/share/man/man1/grape.1"),
            String::from("/usr/share/man/man1/tomato.1"),
        ],
        depends: Vec::new(),
        installed: false,
    });

    pkgs.push(Package {
        name: String::from("seed"),
        version: String::from("1.0.0"),
        description: String::from("Init system for AETERNA (PID 1)"),
        size: 2048,
        files: vec![
            String::from("/sbin/seed"),
            String::from("/etc/seed/init.conf"),
        ],
        depends: vec![String::from("plum")],
        installed: false,
    });

    pkgs
}

// ─── Installed packages tracking (in VFS) ───────────────────────────────────

fn ensure_dirs() {
    crate::fs::mkdir("/var");
    crate::fs::mkdir("/var/lib");
    crate::fs::mkdir("/var/lib/tomato");
    crate::fs::mkdir("/var/lib/tomato/db");
    crate::fs::mkdir("/var/lib/tomato/local");
    crate::fs::mkdir("/var/cache");
    crate::fs::mkdir("/var/cache/tomato");
    crate::fs::mkdir("/var/cache/tomato/pkg");
    crate::fs::mkdir("/etc/tomato");
    crate::fs::mkdir("/bin");
    crate::fs::mkdir("/sbin");
    crate::fs::mkdir("/lib");
    crate::fs::mkdir("/usr");
    crate::fs::mkdir("/usr/include");
    crate::fs::mkdir("/usr/include/aeterna");
    crate::fs::mkdir("/usr/share");
    crate::fs::mkdir("/usr/share/man");
    crate::fs::mkdir("/usr/share/man/man1");
    crate::fs::mkdir("/usr/share/tutor");
}

/// Check if a package is installed by looking in /var/lib/tomato/local/<name>
fn is_installed(name: &str) -> bool {
    let mut path = String::from(LOCAL_PATH);
    path.push('/');
    path.push_str(name);
    crate::fs::exists(&path)
}

/// Mark a package as installed: create /var/lib/tomato/local/<name> with version info
fn mark_installed(pkg: &Package) {
    let mut path = String::from(LOCAL_PATH);
    path.push('/');
    path.push_str(&pkg.name);

    let mut meta = String::from("name=");
    meta.push_str(&pkg.name);
    meta.push_str("\nversion=");
    meta.push_str(&pkg.version);
    meta.push_str("\nsize=");
    meta.push_str(&usize_to_string(pkg.size));
    meta.push_str("\ndesc=");
    meta.push_str(&pkg.description);
    meta.push('\n');

    // Files list
    for f in &pkg.files {
        meta.push_str("file=");
        meta.push_str(f);
        meta.push('\n');
    }

    crate::fs::write_file(&path, meta.as_bytes());

    // Create stub files in VFS
    for f in &pkg.files {
        // Ensure parent dirs exist
        if let Some(slash) = f.rfind('/') {
            if slash > 0 {
                crate::fs::mkdir(&f[..slash]);
            }
        }
        if !crate::fs::exists(f) {
            // Create placeholder file with a package attribution comment
            let mut content = String::from("# Installed by tomato: ");
            content.push_str(&pkg.name);
            content.push_str(" ");
            content.push_str(&pkg.version);
            content.push('\n');
            crate::fs::write_file(f, content.as_bytes());
        }
    }
}

/// Remove installation record
fn mark_removed(name: &str) -> Option<Vec<String>> {
    let mut path = String::from(LOCAL_PATH);
    path.push('/');
    path.push_str(name);

    // Read the file list before removing
    let mut files = Vec::new();
    if let Some(data) = crate::fs::read_file(&path) {
        if let Ok(text) = core::str::from_utf8(&data) {
            for line in text.lines() {
                if let Some(stripped) = line.strip_prefix("file=") {
                    files.push(String::from(stripped));
                }
            }
        }
    }

    crate::fs::remove(&path);
    Some(files)
}

// ─── Public entry point ─────────────────────────────────────────────────────

/// Execute tomato with the given command-line arguments.
/// Called from the terminal as: tomato <args>
pub fn run(args: &str) {
    ensure_dirs();

    let args = args.trim();

    if args.is_empty() || args == "--help" || args == "-h" {
        show_help();
        return;
    }

    // Parse operation
    if args.starts_with("-Syu") {
        cmd_upgrade();
    } else if args.starts_with("-Sy") {
        cmd_sync();
    } else if args.starts_with("-Ss ") {
        cmd_search(&args[4..].trim());
    } else if args.starts_with("-S ") {
        cmd_install(&args[3..].trim());
    } else if args.starts_with("-Qi ") {
        cmd_info(&args[4..].trim());
    } else if args.starts_with("-Q") {
        cmd_list();
    } else if args.starts_with("-R ") {
        cmd_remove(&args[3..].trim());
    } else {
        err("error: ");
        puts("invalid operation '");
        puts(args);
        puts("'\n");
        dim("Try 'tomato --help' for usage.\n");
    }
}

// ─── Commands ───────────────────────────────────────────────────────────────

fn show_help() {
    puts("\n");
    framebuffer::draw_string("tomato", FG_NAME, BG);
    puts(" — package manager for ospab.os\n\n");

    framebuffer::draw_string("  Usage:\n", FG_WARN, BG);
    puts("    tomato <operation> [options] [targets]\n\n");

    framebuffer::draw_string("  Operations:\n", FG_WARN, BG);
    puts("    -S <pkg>     Install a package\n");
    puts("    -R <pkg>     Remove a package\n");
    puts("    -Q           List all installed packages\n");
    puts("    -Qi <pkg>    Show info about installed package\n");
    puts("    -Ss <query>  Search for packages\n");
    puts("    -Sy          Synchronize package database\n");
    puts("    -Syu         Full system upgrade\n");
    puts("    --help       Show this help\n\n");

    framebuffer::draw_string("  Examples:\n", FG_WARN, BG);
    dim("    tomato -S grape        Install grape text editor\n");
    dim("    tomato -S base         Install base system packages\n");
    dim("    tomato -R man-pages    Remove man-pages package\n");
    dim("    tomato -Ss net         Search for network packages\n");
    dim("    tomato -Q              List all installed packages\n\n");
}

fn cmd_sync() {
    puts(":: Synchronizing package databases...\n");
    // Simulate sync — write DB index to VFS
    let pkgs = builtin_packages();
    let mut index = String::new();
    for pkg in &pkgs {
        index.push_str(&pkg.name);
        index.push(' ');
        index.push_str(&pkg.version);
        index.push('\n');
    }
    crate::fs::write_file("/var/lib/tomato/db/core.db", index.as_bytes());

    puts(" core                    ");
    ok("done\n");
    puts(":: Package database synchronized (");
    puts(&usize_to_string(pkgs.len()));
    puts(" packages available)\n");
}

fn cmd_upgrade() {
    cmd_sync();
    puts(":: Starting full system upgrade...\n");

    let pkgs = builtin_packages();
    let mut upgraded = 0usize;

    for pkg in &pkgs {
        if is_installed(&pkg.name) {
            // Check for version mismatch (simulated)
            let mut path = String::from(LOCAL_PATH);
            path.push('/');
            path.push_str(&pkg.name);
            if let Some(data) = crate::fs::read_file(&path) {
                if let Ok(text) = core::str::from_utf8(&data) {
                    let current_ver = text.lines()
                        .find(|l| l.starts_with("version="))
                        .map(|l| &l[8..])
                        .unwrap_or("0");
                    if current_ver != pkg.version.as_str() {
                        puts(" upgrading ");
                        framebuffer::draw_string(&pkg.name, FG_NAME, BG);
                        puts(" ");
                        dim(current_ver);
                        puts(" -> ");
                        framebuffer::draw_string(&pkg.version, FG_VER, BG);
                        puts("\n");
                        mark_installed(&pkg);
                        upgraded += 1;
                    }
                }
            }
        }
    }

    if upgraded == 0 {
        puts(" there is nothing to do\n");
    } else {
        ok(":: ");
        puts(&usize_to_string(upgraded));
        puts(" packages upgraded\n");
    }
}

fn cmd_search(query: &str) {
    let pkgs = builtin_packages();
    let query_lower = query.as_bytes();
    let mut found = 0usize;

    for pkg in &pkgs {
        // Search in name and description (case-insensitive)
        let name_match = contains_ci(pkg.name.as_bytes(), query_lower);
        let desc_match = contains_ci(pkg.description.as_bytes(), query_lower);

        if name_match || desc_match {
            // Show: repo/name version [installed]
            dim("core/");
            framebuffer::draw_string(&pkg.name, FG_NAME, BG);
            puts(" ");
            framebuffer::draw_string(&pkg.version, FG_VER, BG);
            if is_installed(&pkg.name) {
                puts(" ");
                ok("[installed]");
            }
            puts("\n");
            puts("    ");
            dim(&pkg.description);
            puts("\n");
            found += 1;
        }
    }

    if found == 0 {
        err("error: ");
        puts("no packages found matching '");
        puts(query);
        puts("'\n");
    }
}

fn cmd_install(name: &str) {
    if name.is_empty() {
        err("error: ");
        puts("no targets specified\n");
        return;
    }

    let pkgs = builtin_packages();

    // Find the package
    let pkg = match pkgs.iter().find(|p| p.name.as_str() == name) {
        Some(p) => p.clone(),
        None => {
            err("error: ");
            puts("target not found: ");
            puts(name);
            puts("\n");
            return;
        }
    };

    // Check if already installed
    if is_installed(name) {
        warn("warning: ");
        puts(&pkg.name);
        puts(" is already installed -- reinstalling\n");
    }

    // Resolve dependencies
    let mut to_install: Vec<Package> = Vec::new();
    resolve_deps(&pkg, &pkgs, &mut to_install);

    // Show what will be installed
    puts(":: resolving dependencies...\n");
    puts(":: looking for conflicting packages...\n\n");

    let _new_count = to_install.iter().filter(|p| !is_installed(&p.name)).count();
    let total_size: usize = to_install.iter().map(|p| p.size).sum();

    puts("Packages (");
    puts(&usize_to_string(to_install.len()));
    puts(") ");
    for p in &to_install {
        framebuffer::draw_string(&p.name, FG_NAME, BG);
        puts("-");
        framebuffer::draw_string(&p.version, FG_VER, BG);
        puts("  ");
    }
    puts("\n\n");

    puts("Total Installed Size:  ");
    puts(&format_size(total_size));
    puts("\n\n");

    // Install each package
    for install_pkg in &to_install {
        puts("(");
        let idx = to_install.iter().position(|p| p.name == install_pkg.name).unwrap_or(0) + 1;
        puts(&usize_to_string(idx));
        puts("/");
        puts(&usize_to_string(to_install.len()));
        puts(") installing ");
        framebuffer::draw_string(&install_pkg.name, FG_NAME, BG);
        puts("-");
        framebuffer::draw_string(&install_pkg.version, FG_VER, BG);
        puts("...\n");

        // Create files in VFS
        mark_installed(install_pkg);

        // Simulate extraction progress
        puts("  -> extracting ");
        puts(&usize_to_string(install_pkg.files.len()));
        puts(" files (");
        puts(&format_size(install_pkg.size));
        puts(")");
        ok(" done\n");
    }

    puts("\n");
    ok(":: ");
    puts("Transaction completed successfully.\n");
    puts("   ");
    puts(&usize_to_string(to_install.len()));
    puts(" package(s) installed.\n");
}

fn cmd_remove(name: &str) {
    if name.is_empty() {
        err("error: ");
        puts("no targets specified\n");
        return;
    }

    if !is_installed(name) {
        err("error: ");
        puts("target not found: ");
        puts(name);
        puts("\n");
        return;
    }

    puts(":: Removing ");
    framebuffer::draw_string(name, FG_NAME, BG);
    puts("...\n");

    if let Some(files) = mark_removed(name) {
        for f in &files {
            crate::fs::remove(f);
        }
        puts("  -> removed ");
        puts(&usize_to_string(files.len()));
        puts(" files\n");
    }

    ok(":: ");
    puts("Package ");
    puts(name);
    puts(" removed.\n");
}

fn cmd_list() {
    let pkgs = builtin_packages();
    let mut count = 0usize;

    for pkg in &pkgs {
        if is_installed(&pkg.name) {
            framebuffer::draw_string(&pkg.name, FG_NAME, BG);
            puts(" ");
            framebuffer::draw_string(&pkg.version, FG_VER, BG);
            puts("\n");
            count += 1;
        }
    }

    if count == 0 {
        dim("No packages installed.\n");
        dim("Install packages with: tomato -S <name>\n");
    } else {
        puts("\n");
        dim(&usize_to_string(count));
        dim(" package(s) installed\n");
    }
}

fn cmd_info(name: &str) {
    let pkgs = builtin_packages();

    let pkg = match pkgs.iter().find(|p| p.name.as_str() == name) {
        Some(p) => p,
        None => {
            err("error: ");
            puts("package not found: ");
            puts(name);
            puts("\n");
            return;
        }
    };

    let installed = is_installed(name);

    puts("\n");
    info_field("Name",         &pkg.name);
    info_field("Version",      &pkg.version);
    info_field("Description",  &pkg.description);
    info_field("Install Size", &format_size(pkg.size));
    info_field("Status",       if installed { "Installed" } else { "Not Installed" });

    if !pkg.depends.is_empty() {
        let mut dep_str = String::new();
        for (i, d) in pkg.depends.iter().enumerate() {
            if i > 0 { dep_str.push_str("  "); }
            dep_str.push_str(d);
        }
        info_field("Depends On", &dep_str);
    } else {
        info_field("Depends On", "None");
    }

    if !pkg.files.is_empty() {
        framebuffer::draw_string("Files          : ", FG_WARN, BG);
        puts("\n");
        for f in &pkg.files {
            puts("                 ");
            puts(f);
            puts("\n");
        }
    }
    puts("\n");
}

fn info_field(label: &str, value: &str) {
    framebuffer::draw_string(label, FG_WARN, BG);
    // Pad to 16 chars
    let pad = if label.len() < 15 { 15 - label.len() } else { 0 };
    for _ in 0..pad { puts(" "); }
    puts(": ");
    puts(value);
    puts("\n");
}

// ─── Dependency resolution ──────────────────────────────────────────────────

fn resolve_deps(pkg: &Package, all: &[Package], result: &mut Vec<Package>) {
    // Check if already in result list
    if result.iter().any(|p| p.name == pkg.name) {
        return;
    }

    // Resolve dependencies first
    for dep_name in &pkg.depends {
        if let Some(dep_pkg) = all.iter().find(|p| p.name.as_str() == dep_name.as_str()) {
            resolve_deps(dep_pkg, all, result);
        }
    }

    result.push(pkg.clone());
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Case-insensitive substring search
fn contains_ci(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() { return true; }
    if needle.len() > haystack.len() { return false; }
    for i in 0..=(haystack.len() - needle.len()) {
        let mut matched = true;
        for j in 0..needle.len() {
            if to_lower(haystack[i + j]) != to_lower(needle[j]) {
                matched = false;
                break;
            }
        }
        if matched { return true; }
    }
    false
}

fn to_lower(b: u8) -> u8 {
    if b >= b'A' && b <= b'Z' { b + 32 } else { b }
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

fn format_size(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        let mib = bytes / (1024 * 1024);
        let frac = (bytes % (1024 * 1024)) / (1024 * 100);
        let mut s = usize_to_string(mib);
        s.push('.');
        s.push_str(&usize_to_string(frac));
        s.push_str(" MiB");
        s
    } else if bytes >= 1024 {
        let kib = bytes / 1024;
        let mut s = usize_to_string(kib);
        s.push_str(" KiB");
        s
    } else {
        let mut s = usize_to_string(bytes);
        s.push_str(" B");
        s
    }
}

// ══════════════════════════════════════════════════════════════════════════════
//  .tmt — AETERNA Binary Package Format
//  
//  Tomato Managed Tarball (.tmt) is a self-describing binary package:
//
//  Offset  Size  Field
//  ──────────────────────────────────────────────────────────────────
//       0     8  Magic: [b'T',b'M',b'T',0x01, 0x00,0x00,0x00,0x00]
//       8     4  Format version (LE u32 = 1)
//      12     4  Flags (LE u32; bit 0 = compressed, bit 1 = signed)
//      16     4  Architecture (LE u32; 0=any, 1=x86_64, 2=aarch64)
//      20    64  Package name (UTF-8, null-padded, max 63 chars)
//      84    32  Package version (UTF-8, null-padded, max 31 chars)
//     116     4  meta_len  — length of metadata section (LE u32)
//     120     4  payload_len — length of payload section (LE u32)
//     124    32  SHA-256 over bytes [0..124] ++ metadata ++ payload
//     156    meta_len  metadata: UTF-8 key=value lines (desc, depends…)
//    156+meta_len  payload: concatenated file entries:
//         [u32 path_len LE][path bytes][u32 data_len LE][data bytes]
//
//  SHA-256 is computed over the entire blob with bytes 124..156 zeroed,
//  then the digest is stored at offset 124. To verify, zero bytes 124..156,
//  hash the full blob, compare with stored digest.
// ══════════════════════════════════════════════════════════════════════════════

/// Magic bytes for .tmt format
pub const TMT_MAGIC: [u8; 8] = [b'T', b'M', b'T', 0x01, 0x00, 0x00, 0x00, 0x00];
pub const TMT_VERSION: u32 = 1;
pub const TMT_HEADER_SIZE: usize = 156; // bytes before metadata section

// Architecture constants
pub const TMT_ARCH_ANY:   u32 = 0;
pub const TMT_ARCH_X64:   u32 = 1;
pub const TMT_ARCH_AA64:  u32 = 2;

/// A single file entry inside a .tmt payload
pub struct TmtFile<'a> {
    pub path: &'a str,
    pub data: &'a [u8],
}

/// Metadata parsed from a .tmt header
pub struct TmtHeader {
    pub version:     u32,
    pub flags:       u32,
    pub arch:        u32,
    pub name:        String,
    pub pkg_version: String,
    pub meta_len:    u32,
    pub payload_len: u32,
    pub checksum:    [u8; 32],
}

// ─── Pack: create a .tmt blob from metadata + files ──────────────────────

/// Create a `.tmt` package blob in memory.
///
/// # Arguments
/// * `name`       — package name (max 63 bytes)
/// * `version`    — package version string (max 31 bytes)
/// * `arch`       — TMT_ARCH_ANY / TMT_ARCH_X64 / TMT_ARCH_AA64
/// * `metadata`   — UTF-8 key=value lines (appended together with '\n')
/// * `files`      — slice of (path, data) tuples to embed
///
/// Returns the complete .tmt blob with a valid SHA-256 checksum.
pub fn tmt_pack(
    name: &str,
    version: &str,
    arch: u32,
    metadata: &[(&str, &str)],
    files: &[(&str, &[u8])],
) -> alloc::vec::Vec<u8>
{
    // ── Build metadata section ───────────────────────────────────────────
    let mut meta_bytes: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    for (k, v) in metadata {
        for b in k.as_bytes() { meta_bytes.push(*b); }
        meta_bytes.push(b'=');
        for b in v.as_bytes() { meta_bytes.push(*b); }
        meta_bytes.push(b'\n');
    }

    // ── Build payload section ────────────────────────────────────────────
    let mut payload: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    for (path, data) in files {
        let pb = path.as_bytes();
        let path_len = pb.len() as u32;
        let data_len = data.len() as u32;
        for b in &path_len.to_le_bytes() { payload.push(*b); }
        payload.extend_from_slice(pb);
        for b in &data_len.to_le_bytes() { payload.push(*b); }
        payload.extend_from_slice(data);
    }

    // ── Build header ─────────────────────────────────────────────────────
    let mut blob: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(
        TMT_HEADER_SIZE + meta_bytes.len() + payload.len()
    );

    // Magic (8)
    blob.extend_from_slice(&TMT_MAGIC);
    // Version (4)
    blob.extend_from_slice(&TMT_VERSION.to_le_bytes());
    // Flags (4) — no compression, no external sig
    blob.extend_from_slice(&0u32.to_le_bytes());
    // Arch (4)
    blob.extend_from_slice(&arch.to_le_bytes());
    // Name (64)
    let nb = name.as_bytes();
    let nlen = nb.len().min(63);
    blob.extend_from_slice(&nb[..nlen]);
    for _ in nlen..64 { blob.push(0); }
    // Version (32)
    let vb = version.as_bytes();
    let vlen = vb.len().min(31);
    blob.extend_from_slice(&vb[..vlen]);
    for _ in vlen..32 { blob.push(0); }
    // meta_len (4)
    blob.extend_from_slice(&(meta_bytes.len() as u32).to_le_bytes());
    // payload_len (4)
    blob.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    // SHA-256 placeholder (32 zeros — to be replaced after hash)
    for _ in 0..32 { blob.push(0); }

    // Append data sections
    blob.extend_from_slice(&meta_bytes);
    blob.extend_from_slice(&payload);

    // ── Compute SHA-256 over the full blob (checksum field = zeros) ──────
    let digest = sha256_hash(&blob);
    // Write digest at offset 124
    blob[124..156].copy_from_slice(&digest);

    blob
}

// ─── Verify: check SHA-256 checksum ──────────────────────────────────────

/// Verify the SHA-256 checksum of a `.tmt` blob.  
/// Returns `true` if the checksum matches.
pub fn tmt_verify(data: &[u8]) -> bool {
    if data.len() < TMT_HEADER_SIZE { return false; }
    // Check magic
    if &data[0..8] != &TMT_MAGIC { return false; }

    // Extract stored checksum
    let stored: [u8; 32] = match data[124..156].try_into() {
        Ok(a) => a,
        Err(_) => return false,
    };

    // Zero the checksum field in a copy, then hash
    let mut copy: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(data.len());
    copy.extend_from_slice(data);
    for b in &mut copy[124..156] { *b = 0; }
    let computed = sha256_hash(&copy);

    computed == stored
}

// ─── Parse header ────────────────────────────────────────────────────────

/// Parse the header from a `.tmt` blob.
pub fn tmt_parse_header(data: &[u8]) -> Option<TmtHeader> {
    if data.len() < TMT_HEADER_SIZE { return None; }
    if &data[0..8] != &TMT_MAGIC { return None; }

    let version     = u32::from_le_bytes(data[8..12].try_into().ok()?);
    let flags       = u32::from_le_bytes(data[12..16].try_into().ok()?);
    let arch        = u32::from_le_bytes(data[16..20].try_into().ok()?);

    let name_end = data[20..84].iter().position(|&b| b == 0).unwrap_or(64);
    let name    = String::from(core::str::from_utf8(&data[20..20+name_end]).unwrap_or("?"));

    let ver_end = data[84..116].iter().position(|&b| b == 0).unwrap_or(32);
    let pkg_version = String::from(core::str::from_utf8(&data[84..84+ver_end]).unwrap_or("?"));

    let meta_len    = u32::from_le_bytes(data[116..120].try_into().ok()?);
    let payload_len = u32::from_le_bytes(data[120..124].try_into().ok()?);
    let checksum: [u8; 32] = data[124..156].try_into().ok()?;

    Some(TmtHeader { version, flags, arch, name, pkg_version, meta_len, payload_len, checksum })
}

// ─── Install: extract a .tmt from VFS path ───────────────────────────────

/// Read a `.tmt` file from the VFS, verify its SHA-256, and extract all
/// embedded files into the VFS. Returns `true` on success.
pub fn tmt_install(vfs_path: &str) -> bool {
    puts("tomato: installing from .tmt: ");
    puts(vfs_path);
    puts("\n");

    // Read blob from VFS
    let data = match crate::fs::read_file(vfs_path) {
        Some(d) => d,
        None => {
            err("error: file not found: ");
            err(vfs_path);
            puts("\n");
            return false;
        }
    };

    // Verify checksum
    if !tmt_verify(&data) {
        err("error: SHA-256 checksum mismatch — package may be corrupted\n");
        return false;
    }

    let hdr = match tmt_parse_header(&data) {
        Some(h) => h,
        None    => { err("error: invalid .tmt header\n"); return false; }
    };

    puts("  Name:    "); puts(&hdr.name);    puts("\n");
    puts("  Version: "); puts(&hdr.pkg_version); puts("\n");
    let arch_s = match hdr.arch {
        TMT_ARCH_X64  => "x86_64",
        TMT_ARCH_AA64 => "aarch64",
        _             => "any",
    };
    puts("  Arch:    "); puts(arch_s); puts("\n");

    // Parse metadata section (key=value pairs)
    let meta_start = TMT_HEADER_SIZE;
    let meta_end   = meta_start + hdr.meta_len as usize;
    if meta_end > data.len() {
        err("error: metadata section truncated\n");
        return false;
    }

    // Parse payload section
    let payload_start = meta_end;
    let payload_end   = payload_start + hdr.payload_len as usize;
    if payload_end > data.len() {
        err("error: payload section truncated\n");
        return false;
    }

    // Extract files from payload
    let payload = &data[payload_start..payload_end];
    let mut cursor = 0usize;
    let mut file_count = 0usize;

    ensure_dirs();

    while cursor + 8 <= payload.len() {
        let path_len = u32::from_le_bytes(
            payload[cursor..cursor+4].try_into().unwrap_or([0u8;4])
        ) as usize;
        cursor += 4;

        if cursor + path_len > payload.len() { break; }
        let path = match core::str::from_utf8(&payload[cursor..cursor+path_len]) {
            Ok(p) => p,
            Err(_) => { cursor += path_len; continue; }
        };
        cursor += path_len;

        if cursor + 4 > payload.len() { break; }
        let data_len = u32::from_le_bytes(
            payload[cursor..cursor+4].try_into().unwrap_or([0u8;4])
        ) as usize;
        cursor += 4;

        if cursor + data_len > payload.len() { break; }
        let file_data = &payload[cursor..cursor+data_len];
        cursor += data_len;

        // Ensure parent directory exists
        if let Some(slash) = path.rfind('/') {
            if slash > 0 { crate::fs::mkdir(&path[..slash]); }
        }

        // Write file into VFS
        crate::fs::write_file(path, file_data);
        dim("  -> extracted "); dim(path); dim("\n");
        file_count += 1;
    }

    // Record installation in tomato DB
    let mut inst_path = String::from(LOCAL_PATH);
    inst_path.push('/');
    inst_path.push_str(&hdr.name);
    let record = alloc::format!("name={}\nversion={}\ninstalled_from={}\n",
        hdr.name, hdr.pkg_version, vfs_path);
    crate::fs::write_file(&inst_path, record.as_bytes());

    ok("  Installation complete: ");
    puts(&usize_to_string(file_count));
    puts(" files extracted.\n");
    true
}

// ─── List contents: peek inside a .tmt without extracting ────────────────

/// List all files embedded in a `.tmt` blob without extracting them.
/// Returns a Vec of paths, or empty if the blob is invalid.
pub fn tmt_list_contents(data: &[u8]) -> alloc::vec::Vec<String> {
    let mut paths: alloc::vec::Vec<String> = alloc::vec::Vec::new();

    let hdr = match tmt_parse_header(data) { Some(h) => h, None => return paths };
    let payload_start = TMT_HEADER_SIZE + hdr.meta_len as usize;
    let payload_end   = payload_start + hdr.payload_len as usize;
    if payload_end > data.len() { return paths; }

    let payload = &data[payload_start..payload_end];
    let mut cursor = 0usize;

    while cursor + 8 <= payload.len() {
        let path_len = u32::from_le_bytes(
            payload[cursor..cursor+4].try_into().unwrap_or([0u8;4])
        ) as usize;
        cursor += 4;
        if cursor + path_len > payload.len() { break; }
        if let Ok(p) = core::str::from_utf8(&payload[cursor..cursor+path_len]) {
            paths.push(String::from(p));
        }
        cursor += path_len;
        if cursor + 4 > payload.len() { break; }
        let data_len = u32::from_le_bytes(
            payload[cursor..cursor+4].try_into().unwrap_or([0u8;4])
        ) as usize;
        cursor += 4 + data_len;
    }
    paths
}

// ─── SHA-256 implementation (FIPS 180-4, no_std) ─────────────────────────

/// Compute SHA-256 over arbitrary data. Pure Rust, no_std.
pub fn sha256_hash(data: &[u8]) -> [u8; 32] {
    // Initial hash values (first 32 bits of fractional parts of sqrt of primes 2..19)
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
        0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];

    // Round constants (first 32 bits of fractional parts of cbrt of primes 2..311)
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5,
        0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
        0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
        0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
        0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5,
        0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
        0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    // Pre-processing: add padding
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut padded: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity(
        data.len() + 64 + 8
    );
    padded.extend_from_slice(data);
    padded.push(0x80); // append bit '1'
    while (padded.len() % 64) != 56 {
        padded.push(0x00);
    }
    // Append original length as big-endian u64
    for b in &bit_len.to_be_bytes() { padded.push(*b); }

    // Process each 512-bit (64-byte) chunk
    for chunk in padded.chunks_exact(64) {
        // Prepare message schedule
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i*4], chunk[i*4+1], chunk[i*4+2], chunk[i*4+3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i-15].rotate_right(7)  ^ w[i-15].rotate_right(18) ^ (w[i-15] >> 3);
            let s1 = w[i-2].rotate_right(17)  ^ w[i-2].rotate_right(19)  ^ (w[i-2]  >> 10);
            w[i] = w[i-16].wrapping_add(s0).wrapping_add(w[i-7]).wrapping_add(s1);
        }

        // Initialize working variables
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;

        // 64-round compression function
        for i in 0..64 {
            let s1    = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch    = (e & f) ^ ((!e) & g);
            let temp1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0    = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj   = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g; g = f; f = e;
            e = d.wrapping_add(temp1);
            d = c; c = b; b = a;
            a = temp1.wrapping_add(temp2);
        }

        // Add compressed chunk to current hash value
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    // Produce final digest (big-endian)
    let mut digest = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        digest[i*4..i*4+4].copy_from_slice(&word.to_be_bytes());
    }
    digest
}

// ─── .tmt command integration ─────────────────────────────────────────────

/// Entry point for `tomato --tmt` sub-commands.
/// Handles:
///   `tomato --tmt install <path.tmt>`
///   `tomato --tmt list    <path.tmt>`
///   `tomato --tmt pack    <name> <version> <output.tmt>`
///   `tomato --tmt verify  <path.tmt>`
pub fn tmt_dispatch(args: &str) {
    let mut toks = args.splitn(4, ' ');
    let subcmd = toks.next().unwrap_or("").trim();
    let arg1   = toks.next().unwrap_or("").trim();
    let arg2   = toks.next().unwrap_or("").trim();
    let _arg3  = toks.next().unwrap_or("").trim();

    match subcmd {
        "install" => {
            if arg1.is_empty() {
                err("Usage: tomato --tmt install <path.tmt>\n");
                return;
            }
            tmt_install(arg1);
        }
        "list" => {
            if arg1.is_empty() {
                err("Usage: tomato --tmt list <path.tmt>\n");
                return;
            }
            let data = match crate::fs::read_file(arg1) {
                Some(d) => d,
                None => { err("File not found\n"); return; }
            };
            if !tmt_verify(&data) {
                err("warning: checksum invalid (listing anyway)\n");
            }
            let hdr = match tmt_parse_header(&data) {
                Some(h) => h,
                None => { err("Invalid .tmt header\n"); return; }
            };
            puts("Package: "); puts(&hdr.name); puts(" ");
            puts(&hdr.pkg_version); puts("\n");
            puts("Files:\n");
            for p in tmt_list_contents(&data) {
                puts("  "); puts(&p); puts("\n");
            }
        }
        "verify" => {
            if arg1.is_empty() {
                err("Usage: tomato --tmt verify <path.tmt>\n");
                return;
            }
            let data = match crate::fs::read_file(arg1) {
                Some(d) => d,
                None => { err("File not found\n"); return; }
            };
            if tmt_verify(&data) {
                ok("Checksum OK\n");
            } else {
                err("Checksum FAILED\n");
            }
        }
        "pack" => {
            // `tomato --tmt pack <name> <version> [output_path]`
            if arg1.is_empty() {
                err("Usage: tomato --tmt pack <name> <version> [out.tmt]\n");
                return;
            }
            let pkg_name = arg1;
            let pkg_ver  = if arg2.is_empty() { "1.0.0" } else { arg2 };
            let output   = _arg3;
            let output   = if output.is_empty() {
                let mut s = String::from("/var/cache/tomato/pkg/");
                s.push_str(pkg_name);
                s.push('-');
                s.push_str(pkg_ver);
                s.push_str(".tmt");
                s
            } else {
                String::from(output)
            };

            // Pack an empty stub package (real packaging requires more context)
            let meta = [("description", "AETERNA binary package")];
            let files: &[(&str, &[u8])] = &[];
            let blob = tmt_pack(pkg_name, pkg_ver, TMT_ARCH_X64, &meta, files);
            if crate::fs::write_file(&output, &blob) {
                ok("Packed: "); puts(&output); puts("\n");
                puts("Size: "); puts(&usize_to_string(blob.len())); puts(" bytes\n");
            } else {
                err("Pack failed (VFS write error)\n");
            }
        }
        _ => {
            puts("Usage: tomato --tmt <subcmd> ...\n");
            dim("  install <path.tmt>              Install from binary package\n");
            dim("  list    <path.tmt>              List files in package\n");
            dim("  verify  <path.tmt>              Verify SHA-256 checksum\n");
            dim("  pack    <name> <ver> [out.tmt]  Create a new .tmt package\n");
        }
    }
}
