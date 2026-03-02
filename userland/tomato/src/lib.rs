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
