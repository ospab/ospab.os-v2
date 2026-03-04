/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

axon вЂ” Coreutils for ospab.os (AETERNA)

A collection of POSIX-compatible essential utilities, all operating
exclusively through the VFS syscall layer (crate::fs::*).

Utilities:
  wc       вЂ” Count lines/words/bytes
  head     вЂ” Display first N lines
  tail     вЂ” Display last N lines
  grep     вЂ” Pattern search in files
  find     вЂ” Search for files in directory tree
  cp       вЂ” Copy files
  mv       вЂ” Move / rename files
  tee      вЂ” Write stdin to file + stdout (echo variant)
  stat     вЂ” Show file/directory info
  du       вЂ” Estimate file space usage (sizes of files)
  tree     вЂ” Directory tree display
  basename вЂ” Strip directory prefix from path
  dirname  вЂ” Strip last component from path
  yes      вЂ” Repeatedly output a string (limited)
  true/false вЂ” Exit code utilities
  seq      вЂ” Print number sequences
  sort     вЂ” Sort lines in a file
  uniq     вЂ” Remove duplicate adjacent lines
  cut      вЂ” Extract fields/columns from text
  rev      вЂ” Reverse lines
*/

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;

use crate::arch::x86_64::framebuffer;
use crate::fs;
use crate::userspace::plum;

pub mod proc_tools;
pub mod net_tools;
pub mod ping;
pub mod ps;
pub mod netstat;
pub mod lspci;
pub mod disk_tools;

const FG: u32     = 0x00FFFFFF;
const FG_OK: u32  = 0x0000FF00;
const FG_ERR: u32 = 0x00FF4444;
const FG_DIM: u32 = 0x00AAAAAA;
const FG_DIR: u32 = 0x005555FF;
const FG_HL: u32  = 0x00FFCC00;
const BG: u32     = 0x00000000;

fn puts(s: &str) { framebuffer::draw_string(s, FG, BG); }
fn ok(s: &str)   { framebuffer::draw_string(s, FG_OK, BG); }
fn err(s: &str)  { framebuffer::draw_string(s, FG_ERR, BG); }
fn dim(s: &str)  { framebuffer::draw_string(s, FG_DIM, BG); }
fn hl(s: &str)   { framebuffer::draw_string(s, FG_HL, BG); }
fn dir_color(s: &str) { framebuffer::draw_string(s, FG_DIR, BG); }

fn put_usize(mut n: usize) {
    if n == 0 { puts("0"); return; }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while n > 0 { buf[i] = b'0' + (n % 10) as u8; n /= 10; i += 1; }
    for k in (0..i).rev() { framebuffer::draw_char(buf[k] as char, FG, BG); }
}

/// Check if AXON handles a command. Returns true if it was handled.
pub fn dispatch(command: &str, args: &str) -> bool {
    match command {
        "wc"       => { cmd_wc(args); true }
        "head"     => { cmd_head(args); true }
        "tail"     => { cmd_tail(args); true }
        "grep"     => { cmd_grep(args); true }
        "find"     => { cmd_find(args); true }
        "cp"       => { cmd_cp(args); true }
        "mv"       => { cmd_mv(args); true }
        "tee"      => { cmd_tee(args); true }
        "stat"     => { cmd_stat(args); true }
        "du"       => { cmd_du(args); true }
        "tree"     => { cmd_tree(args); true }
        "basename" => { cmd_basename(args); true }
        "dirname"  => { cmd_dirname(args); true }
        "yes"      => { cmd_yes(args); true }
        "true"     => { /* exit 0 */ true }
        "false"    => { /* conceptual exit 1 */ true }
        "seq"      => { cmd_seq(args); true }
        "sort"     => { cmd_sort(args); true }
        "uniq"     => { cmd_uniq(args); true }
        "cut"      => { cmd_cut(args); true }
        "rev"      => { cmd_rev(args); true }
        "xxd"      => { cmd_xxd(args); true }
        "nl"       => { cmd_nl(args); true }
        "diff"     => { cmd_diff(args); true }
        "awk"      => { cmd_awk(args); true }
        "ps"           => { ps::run(args); true }
        "top"          => { proc_tools::cmd_top(args); true }
        "kill"         => { proc_tools::cmd_kill(args); true }
        "netstat"      => { netstat::run(args); true }
        "df"           => { net_tools::cmd_df(args); true }
        "ping"         => { ping::run(args); true }
        "lspci"        => { lspci::run(args); true }
        "fdisk"        => { disk_tools::run_fdisk(args); true }
        "lsblk"        => { disk_tools::run_lsblk(args); true }
        "mkfs"         => { disk_tools::run_mkfs(args); true }
        "mount"        => { disk_tools::run_mount(args); true }
        "verify_mem"   => { crate::userspace::ivs::dispatch("verify_mem", args); true }
        "verify_sched" => { crate::userspace::ivs::dispatch("verify_sched", args); true }
        "verify_net"   => { crate::userspace::ivs::dispatch("verify_net", args); true }
        "verify_audio" => { crate::userspace::ivs::dispatch("verify_audio", args); true }
        "printf"   => { cmd_printf(args); true }
        "env"      => { cmd_env(args); true }
        "which"    => { cmd_which(args); true }
        "xargs"    => { cmd_xargs(args); true }
        _          => false,
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// wc вЂ” Count lines/words/bytes
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_wc(args: &str) {
    if args.is_empty() {
        err("wc: missing file operand\n");
        dim("Usage: wc <file>\n");
        return;
    }

    let path = plum::resolve_path(args.trim());
    match fs::read_file(&path) {
        Some(data) => {
            let text = core::str::from_utf8(&data).unwrap_or("");
            let lines = text.lines().count();
            let words = text.split_whitespace().count();
            let bytes = data.len();
            puts("  ");
            put_usize(lines); puts("  ");
            put_usize(words); puts("  ");
            put_usize(bytes); puts("  ");
            puts(args.trim()); puts("\n");
        }
        None => {
            err("wc: "); err(args.trim()); err(": No such file\n");
        }
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// head вЂ” Display first N lines (default 10)
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_head(args: &str) {
    let (n, file) = parse_n_flag(args, 10);
    if file.is_empty() {
        err("head: missing file operand\n");
        dim("Usage: head [-n N] <file>\n");
        return;
    }

    let path = plum::resolve_path(file);
    match fs::read_file(&path) {
        Some(data) => {
            let text = core::str::from_utf8(&data).unwrap_or("");
            for (i, line) in text.lines().enumerate() {
                if i >= n { break; }
                puts(line); puts("\n");
            }
        }
        None => {
            err("head: "); err(file); err(": No such file\n");
        }
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// tail вЂ” Display last N lines (default 10)
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_tail(args: &str) {
    let (n, file) = parse_n_flag(args, 10);
    if file.is_empty() {
        err("tail: missing file operand\n");
        dim("Usage: tail [-n N] <file>\n");
        return;
    }

    let path = plum::resolve_path(file);
    match fs::read_file(&path) {
        Some(data) => {
            let text = core::str::from_utf8(&data).unwrap_or("");
            let lines: Vec<&str> = text.lines().collect();
            let start = if lines.len() > n { lines.len() - n } else { 0 };
            for line in &lines[start..] {
                puts(line); puts("\n");
            }
        }
        None => {
            err("tail: "); err(file); err(": No such file\n");
        }
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// grep вЂ” Pattern search (exact substring match)
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_grep(args: &str) {
    let parts: Vec<&str> = args.splitn(2, ' ').collect();
    if parts.len() < 2 {
        err("grep: missing operand\n");
        dim("Usage: grep <pattern> <file>\n");
        return;
    }
    let pattern = parts[0];
    let file = parts[1].trim();
    let path = plum::resolve_path(file);

    // Check for -i flag (case-insensitive)
    let (pattern, case_insensitive) = if pattern.starts_with("-i") {
        if parts.len() < 2 {
            err("grep: missing pattern after -i\n");
            return;
        }
        let rest: Vec<&str> = parts[1].splitn(2, ' ').collect();
        if rest.len() < 2 {
            err("grep: missing file after pattern\n");
            return;
        }
        (rest[0], true)
    } else {
        (pattern, false)
    };

    // If -i was used, we need to re-resolve the path
    let path = if case_insensitive {
        let parts2: Vec<&str> = args.splitn(3, ' ').collect();
        if parts2.len() >= 3 {
            plum::resolve_path(parts2[2].trim())
        } else {
            path
        }
    } else {
        path
    };

    match fs::read_file(&path) {
        Some(data) => {
            let text = core::str::from_utf8(&data).unwrap_or("");
            let mut count = 0usize;
            let pattern_lower = if case_insensitive {
                alloc::string::String::from(pattern).to_ascii_lowercase()
            } else {
                alloc::string::String::new()
            };
            for (line_num, line) in text.lines().enumerate() {
                let matches = if case_insensitive {
                    let line_lower = alloc::string::String::from(line).to_ascii_lowercase();
                    line_lower.contains(&*pattern_lower)
                } else {
                    line.contains(pattern)
                };
                if matches {
                    // Colorize: line number in dim, matched text highlighted
                    dim(&format!("{}:", line_num + 1));
                    if let Some(pos) = if case_insensitive {
                        let ll = alloc::string::String::from(line).to_ascii_lowercase();
                        ll.find(&*pattern_lower)
                    } else {
                        line.find(pattern)
                    } {
                        puts(&line[..pos]);
                        hl(&line[pos..pos + pattern.len()]);
                        puts(&line[pos + pattern.len()..]);
                    } else {
                        puts(line);
                    }
                    puts("\n");
                    count += 1;
                }
            }
            if count == 0 {
                dim("(no matches)\n");
            }
        }
        None => {
            err("grep: "); err(file); err(": No such file\n");
        }
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// find вЂ” Search for files in directory tree
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_find(args: &str) {
    let (start_dir, name_pattern) = if args.contains("-name") {
        let parts: Vec<&str> = args.splitn(3, ' ').collect();
        match parts.len() {
            3 => (parts[0], parts[2]),
            _ => (".", ""),
        }
    } else if args.is_empty() {
        (".", "")
    } else {
        (args.trim(), "")
    };

    let base = plum::resolve_path(start_dir);
    find_recursive(&base, name_pattern, 0);
}

fn find_recursive(path: &str, pattern: &str, depth: usize) {
    if depth > 16 { return; }  // Prevent infinite recursion

    if let Some(entries) = fs::readdir(path) {
        for entry in &entries {
            if entry.name == "." || entry.name == ".." { continue; }
            let full = if path == "/" {
                format!("/{}", entry.name)
            } else {
                format!("{}/{}", path, entry.name)
            };

            let matches = pattern.is_empty() || entry.name.contains(pattern);
            if matches {
                if entry.node_type == fs::NodeType::Directory {
                    dir_color(&full); puts("\n");
                } else {
                    puts(&full); puts("\n");
                }
            }

            if entry.node_type == fs::NodeType::Directory {
                find_recursive(&full, pattern, depth + 1);
            }
        }
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// cp вЂ” Copy file
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_cp(args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() < 2 {
        err("cp: missing operand\n");
        dim("Usage: cp <source> <dest>\n");
        return;
    }
    let src = plum::resolve_path(parts[0]);
    let dst = plum::resolve_path(parts[1]);

    match fs::read_file(&src) {
        Some(data) => {
            if fs::write_file(&dst, &data) {
                ok("'"); ok(parts[0]); ok("' -> '"); ok(parts[1]); ok("'\n");
            } else {
                err("cp: cannot write '"); err(parts[1]); err("'\n");
            }
        }
        None => {
            err("cp: cannot read '"); err(parts[0]); err("': No such file\n");
        }
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// mv вЂ” Move/rename file
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_mv(args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() < 2 {
        err("mv: missing operand\n");
        dim("Usage: mv <source> <dest>\n");
        return;
    }
    let src = plum::resolve_path(parts[0]);
    let dst = plum::resolve_path(parts[1]);

    match fs::read_file(&src) {
        Some(data) => {
            if fs::write_file(&dst, &data) {
                if fs::remove(&src) {
                    ok("'"); ok(parts[0]); ok("' -> '"); ok(parts[1]); ok("'\n");
                } else {
                    err("mv: moved but failed to remove source\n");
                }
            } else {
                err("mv: cannot write '"); err(parts[1]); err("'\n");
            }
        }
        None => {
            err("mv: cannot read '"); err(parts[0]); err("': No such file\n");
        }
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// tee вЂ” Write text to file + display
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_tee(args: &str) {
    // Usage: tee <file> <text...>
    let parts: Vec<&str> = args.splitn(2, ' ').collect();
    if parts.len() < 2 {
        err("tee: missing operand\n");
        dim("Usage: tee <file> <text>\n");
        return;
    }
    let file = plum::resolve_path(parts[0]);
    let text = parts[1];

    puts(text); puts("\n");
    let mut data = Vec::from(text.as_bytes());
    data.push(b'\n');
    if !fs::write_file(&file, &data) {
        err("tee: write error\n");
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// stat вЂ” Show file/directory info
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_stat(args: &str) {
    if args.is_empty() {
        err("stat: missing file operand\n");
        return;
    }
    let path = plum::resolve_path(args.trim());
    match fs::stat(&path) {
        Some(entry) => {
            puts("  File: "); hl(args.trim()); puts("\n");
            puts("  Size: "); put_usize(entry.size); puts(" bytes\n");
            puts("  Type: ");
            match entry.node_type {
                fs::NodeType::File => puts("regular file"),
                fs::NodeType::Directory => dir_color("directory"),
                fs::NodeType::CharDevice => puts("character device"),
            }
            puts("\n");
            puts("  Path: "); dim(&path); puts("\n");
        }
        None => {
            err("stat: "); err(args.trim()); err(": No such file or directory\n");
        }
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// du вЂ” Estimate file space usage
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_du(args: &str) {
    let path = if args.is_empty() { "." } else { args.trim() };
    let resolved = plum::resolve_path(path);
    let total = du_recursive(&resolved);
    put_usize(total); puts("\t"); puts(path); puts("\n");
}

fn du_recursive(path: &str) -> usize {
    let mut total = 0usize;
    if let Some(entries) = fs::readdir(path) {
        for entry in &entries {
            if entry.name == "." || entry.name == ".." { continue; }
            let full = if path == "/" {
                format!("/{}", entry.name)
            } else {
                format!("{}/{}", path, entry.name)
            };
            match entry.node_type {
                fs::NodeType::File => {
                    total += entry.size;
                    put_usize(entry.size); puts("\t"); puts(&full); puts("\n");
                }
                fs::NodeType::Directory => {
                    total += du_recursive(&full);
                }
                _ => {}
            }
        }
    }
    total
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// tree вЂ” Directory tree display
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_tree(args: &str) {
    let path = if args.is_empty() { "." } else { args.trim() };
    let resolved = plum::resolve_path(path);
    dir_color(&resolved); puts("\n");
    let (dirs, files) = tree_recursive(&resolved, "", 0);
    puts("\n");
    dim(&format!("{} directories, {} files\n", dirs, files));
}

fn tree_recursive(path: &str, prefix: &str, depth: usize) -> (usize, usize) {
    if depth > 8 { return (0, 0); }
    let mut dirs = 0usize;
    let mut files = 0usize;

    if let Some(entries) = fs::readdir(path) {
        let entries: Vec<_> = entries.into_iter()
            .filter(|e| e.name != "." && e.name != "..")
            .collect();
        let count = entries.len();
        for (i, entry) in entries.iter().enumerate() {
            let is_last = i == count - 1;
            let connector = if is_last { "\u{2514}\u{2500}\u{2500} " } else { "\u{251C}\u{2500}\u{2500} " };
            puts(prefix);
            dim(connector);

            let full = if path == "/" {
                format!("/{}", entry.name)
            } else {
                format!("{}/{}", path, entry.name)
            };

            if entry.node_type == fs::NodeType::Directory {
                dir_color(&entry.name); puts("\n");
                dirs += 1;
                let child_prefix = format!(
                    "{}{}",
                    prefix,
                    if is_last { "    " } else { "\u{2502}   " }
                );
                let (d, f) = tree_recursive(&full, &child_prefix, depth + 1);
                dirs += d;
                files += f;
            } else {
                puts(&entry.name);
                dim(&format!("  ({})", entry.size));
                puts("\n");
                files += 1;
            }
        }
    }
    (dirs, files)
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// basename / dirname вЂ” Path manipulation
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_basename(args: &str) {
    let path = args.trim();
    if path.is_empty() {
        err("basename: missing operand\n");
        return;
    }
    let p = path.trim_end_matches('/');
    if p.is_empty() || p == "/" {
        puts("/\n");
        return;
    }
    if let Some(pos) = p.rfind('/') {
        puts(&p[pos + 1..]);
    } else {
        puts(p);
    }
    puts("\n");
}

fn cmd_dirname(args: &str) {
    let path = args.trim();
    if path.is_empty() {
        err("dirname: missing operand\n");
        return;
    }
    let p = path.trim_end_matches('/');
    if p.is_empty() || p == "/" {
        puts("/\n");
        return;
    }
    if let Some(pos) = p.rfind('/') {
        if pos == 0 {
            puts("/\n");
        } else {
            puts(&p[..pos]);
            puts("\n");
        }
    } else {
        puts(".\n");
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// yes вЂ” Output string repeatedly (limited to 100 lines)
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_yes(args: &str) {
    let text = if args.is_empty() { "y" } else { args.trim() };
    for _ in 0..100 {
        puts(text); puts("\n");
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// seq вЂ” Print number sequence
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_seq(args: &str) {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let (start, end, step) = match parts.len() {
        0 => { err("seq: missing operand\n"); return; }
        1 => (1i64, parse_i64(parts[0]), 1i64),
        2 => (parse_i64(parts[0]), parse_i64(parts[1]), 1i64),
        _ => (parse_i64(parts[0]), parse_i64(parts[2]), parse_i64(parts[1])),
    };

    if step == 0 { err("seq: step cannot be zero\n"); return; }

    let mut i = start;
    let mut count = 0;
    loop {
        if step > 0 && i > end { break; }
        if step < 0 && i < end { break; }
        if count > 10000 { break; } // Safety limit
        puts(&format!("{}\n", i));
        i += step;
        count += 1;
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// sort вЂ” Sort lines of a file
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_sort(args: &str) {
    if args.is_empty() {
        err("sort: missing file operand\n");
        return;
    }

    let (reverse, file) = if args.starts_with("-r ") {
        (true, args[3..].trim())
    } else {
        (false, args.trim())
    };

    let path = plum::resolve_path(file);
    match fs::read_file(&path) {
        Some(data) => {
            let text = core::str::from_utf8(&data).unwrap_or("");
            let mut lines: Vec<&str> = text.lines().collect();
            lines.sort_unstable();
            if reverse { lines.reverse(); }
            for line in &lines {
                puts(line); puts("\n");
            }
        }
        None => {
            err("sort: "); err(file); err(": No such file\n");
        }
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// uniq вЂ” Remove duplicate adjacent lines
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_uniq(args: &str) {
    if args.is_empty() {
        err("uniq: missing file operand\n");
        return;
    }

    let (count_mode, file) = if args.starts_with("-c ") {
        (true, args[3..].trim())
    } else {
        (false, args.trim())
    };

    let path = plum::resolve_path(file);
    match fs::read_file(&path) {
        Some(data) => {
            let text = core::str::from_utf8(&data).unwrap_or("");
            let mut prev: Option<&str> = None;
            let mut count = 0usize;
            for line in text.lines() {
                if prev == Some(line) {
                    count += 1;
                } else {
                    if let Some(p) = prev {
                        if count_mode {
                            puts(&format!("{:>7} ", count));
                        }
                        puts(p); puts("\n");
                    }
                    prev = Some(line);
                    count = 1;
                }
            }
            // Print last line
            if let Some(p) = prev {
                if count_mode {
                    puts(&format!("{:>7} ", count));
                }
                puts(p); puts("\n");
            }
        }
        None => {
            err("uniq: "); err(file); err(": No such file\n");
        }
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// cut вЂ” Extract columns/fields
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_cut(args: &str) {
    // Usage: cut -d<delim> -f<field> <file>
    // Simple: cut -f<N> <file> (tab-delimited)
    let mut delimiter = '\t';
    let mut field_idx: Option<usize> = None;
    let mut file = "";

    let parts: Vec<&str> = args.split_whitespace().collect();
    let mut i = 0;
    while i < parts.len() {
        if parts[i].starts_with("-d") {
            if parts[i].len() > 2 {
                delimiter = parts[i].chars().nth(2).unwrap_or('\t');
            }
        } else if parts[i].starts_with("-f") {
            if parts[i].len() > 2 {
                field_idx = parts[i][2..].parse::<usize>().ok();
            }
        } else {
            file = parts[i];
        }
        i += 1;
    }

    if file.is_empty() || field_idx.is_none() {
        err("cut: missing operand\n");
        dim("Usage: cut -f<N> [-d<delim>] <file>\n");
        return;
    }

    let field = field_idx.unwrap().saturating_sub(1); // 1-based to 0-based
    let path = plum::resolve_path(file);

    match fs::read_file(&path) {
        Some(data) => {
            let text = core::str::from_utf8(&data).unwrap_or("");
            let delim_str = alloc::string::String::from(delimiter);
            for line in text.lines() {
                let fields: Vec<&str> = line.split(&*delim_str).collect();
                if field < fields.len() {
                    puts(fields[field]);
                }
                puts("\n");
            }
        }
        None => {
            err("cut: "); err(file); err(": No such file\n");
        }
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// rev вЂ” Reverse each line
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_rev(args: &str) {
    if args.is_empty() {
        err("rev: missing file operand\n");
        return;
    }

    let path = plum::resolve_path(args.trim());
    match fs::read_file(&path) {
        Some(data) => {
            let text = core::str::from_utf8(&data).unwrap_or("");
            for line in text.lines() {
                let reversed: String = line.chars().rev().collect();
                puts(&reversed); puts("\n");
            }
        }
        None => {
            err("rev: "); err(args.trim()); err(": No such file\n");
        }
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// xxd вЂ” Hex dump of file
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_xxd(args: &str) {
    if args.is_empty() {
        err("xxd: missing file operand\n");
        return;
    }

    let path = plum::resolve_path(args.trim());
    match fs::read_file(&path) {
        Some(data) => {
            let limit = data.len().min(512); // Limit output to 512 bytes
            for offset in (0..limit).step_by(16) {
                // Address
                dim(&format!("{:08x}: ", offset));
                // Hex bytes
                for i in 0..16 {
                    if offset + i < limit {
                        puts(&format!("{:02x}", data[offset + i]));
                        if i == 7 { puts(" "); }
                    } else {
                        puts("  ");
                        if i == 7 { puts(" "); }
                    }
                }
                puts("  ");
                // ASCII
                for i in 0..16 {
                    if offset + i < limit {
                        let b = data[offset + i];
                        if b >= 0x20 && b < 0x7F {
                            framebuffer::draw_char(b as char, FG_DIM, BG);
                        } else {
                            framebuffer::draw_char('.', FG_DIM, BG);
                        }
                    }
                }
                puts("\n");
            }
            if data.len() > 512 {
                dim(&format!("... ({} bytes total, showing first 512)\n", data.len()));
            }
        }
        None => {
            err("xxd: "); err(args.trim()); err(": No such file\n");
        }
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// nl вЂ” Number lines of a file
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

fn cmd_nl(args: &str) {
    if args.is_empty() {
        err("nl: missing file operand\n");
        return;
    }

    let path = plum::resolve_path(args.trim());
    match fs::read_file(&path) {
        Some(data) => {
            let text = core::str::from_utf8(&data).unwrap_or("");
            for (i, line) in text.lines().enumerate() {
                dim(&format!("{:>6}\t", i + 1));
                puts(line); puts("\n");
            }
        }
        None => {
            err("nl: "); err(args.trim()); err(": No such file\n");
        }
    }
}

// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ
// Helpers
// в•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђв•ђ

/// Parse "-n N" flag from args, returning (count, remaining_args)
fn parse_n_flag(args: &str, default: usize) -> (usize, &str) {
    let args = args.trim();
    if args.starts_with("-n") {
        let rest = args[2..].trim_start();
        let (num_str, remaining) = if let Some(pos) = rest.find(' ') {
            (&rest[..pos], rest[pos + 1..].trim())
        } else {
            (rest, "")
        };
        let n = num_str.parse::<usize>().unwrap_or(default);
        (n, remaining)
    } else {
        (default, args)
    }
}

fn parse_i64(s: &str) -> i64 {
    let s = s.trim();
    let (neg, s) = if s.starts_with('-') { (true, &s[1..]) } else { (false, s) };
    let mut val = 0i64;
    for c in s.bytes() {
        if c >= b'0' && c <= b'9' {
            val = val * 10 + (c - b'0') as i64;
        } else { break; }
    }
    if neg { -val } else { val }
}

// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\
// diff \ Compare two files line-by-line
// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\

fn cmd_diff(args: &str) {
    let parts: Vec<&str> = args.splitn(2, ' ').collect();
    if parts.len() < 2 {
        err("diff: requires two file arguments\n");
        dim("Usage: diff <file1> <file2>\n");
        return;
    }
    let path1 = plum::resolve_path(parts[0].trim());
    let path2 = plum::resolve_path(parts[1].trim());

    let data1 = match fs::read_file(&path1) { Some(d) => d, None => { err("diff: "); err(parts[0].trim()); err(": No such file\n"); return; } };
    let data2 = match fs::read_file(&path2) { Some(d) => d, None => { err("diff: "); err(parts[1].trim()); err(": No such file\n"); return; } };

    let text1 = core::str::from_utf8(&data1).unwrap_or("");
    let text2 = core::str::from_utf8(&data2).unwrap_or("");

    let lines1: Vec<&str> = text1.lines().collect();
    let lines2: Vec<&str> = text2.lines().collect();

    let max = lines1.len().max(lines2.len());
    let mut diffs = 0usize;

    for i in 0..max {
        let l1 = if i < lines1.len() { lines1[i] } else { "" };
        let l2 = if i < lines2.len() { lines2[i] } else { "" };
        if l1 != l2 {
            diffs += 1;
            dim(&format!("{}c{}\n", i + 1, i + 1));
            framebuffer::draw_string("< ", 0x00FF6666, BG);
            puts(l1); puts("\n");
            framebuffer::draw_string("---\n", FG_DIM, BG);
            framebuffer::draw_string("> ", 0x0066FF66, BG);
            puts(l2); puts("\n");
        }
    }
    if diffs == 0 {
        ok("Files are identical\n");
    }
}

// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\
// awk \ Field-based text processing (minimal: single-field print)
// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\

fn cmd_awk(args: &str) {
    // Minimal awk: supports `awk '{print $N}' file`
    // and `awk -F: '{print $N}' file`
    let args = args.trim();
    let mut delim = ' ';
    let mut rest = args;

    // -F delimiter
    if rest.starts_with("-F") {
        if rest.len() > 2 {
            delim = rest.chars().nth(2).unwrap_or(' ');
            rest = rest[3..].trim_start();
        }
    }

    // Extract '{print $N}' pattern
    let (field_n, file) = if let Some(start) = rest.find('{') {
        if let Some(end) = rest.find('}') {
            let prog = &rest[start + 1..end];
            let file = rest[end + 1..].trim();
            // Parse $N from `print $N`
            let n = if let Some(dpos) = prog.find('$') {
                prog[dpos + 1..].trim().parse::<usize>().unwrap_or(0)
            } else { 0 };
            (n, file)
        } else { (0, rest) }
    } else { (0, rest) };

    if file.is_empty() {
        err("awk: missing file operand\n");
        dim("Usage: awk [-F:] '{print $N}' <file>\n");
        return;
    }

    let path = plum::resolve_path(file);
    match fs::read_file(&path) {
        Some(data) => {
            let text = core::str::from_utf8(&data).unwrap_or("");
            for line in text.lines() {
                let fields: Vec<&str> = if delim == ' ' {
                    line.split_whitespace().collect()
                } else {
                    let ds = alloc::string::String::from(delim);
                    line.split(&*ds).collect()
                };
                if field_n == 0 {
                    // print whole line ($0)
                    puts(line);
                } else if field_n <= fields.len() {
                    puts(fields[field_n - 1]);
                }
                puts("\n");
            }
        }
        None => { err("awk: "); err(file); err(": No such file\n"); }
    }
}

// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\
// ps \ Show kernel threads / scheduler state
// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\

#[allow(dead_code)]
fn cmd_ps(_args: &str) {
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

// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\
// top \ Show system load and task info
// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\

#[allow(dead_code)]
fn cmd_top(_args: &str) {
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

// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\
// netstat — network interface and ARP cache status
// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\

#[allow(dead_code)]
fn cmd_netstat(_args: &str) {
    use crate::net;

    if !net::is_up() {
        err("netstat: no network interface is up\n");
        return;
    }

    // ── Interface table ──────────────────────────────────────────────────
    hl("  Interface    MAC                  IP              Status    RX        TX\n");
    dim("  -------------------------------------------------------------------------\n");

    let mac = unsafe { net::OUR_MAC };
    let ip  = unsafe { net::OUR_IP };
    let rx  = net::rx_packets();
    let tx  = net::tx_packets();
    let nic = net::nic_name();

    puts("  ");
    puts(nic);
    let nic_len = nic.len();
    for _ in 0..(13usize.saturating_sub(nic_len)) { puts(" "); }

    // MAC
    let hex = b"0123456789abcdef";
    for i in 0..6 {
        framebuffer::draw_char(hex[(mac[i]>>4) as usize] as char, FG, BG);
        framebuffer::draw_char(hex[(mac[i]&0xF) as usize] as char, FG, BG);
        if i < 5 { puts(":"); }
    }
    puts("  ");

    // IP
    let ip_s = format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]);
    puts(&ip_s);
    for _ in 0..(16usize.saturating_sub(ip_s.len())) { puts(" "); }

    ok("Up");
    puts("      ");
    let rx_s = format!("{}", rx);
    puts(&rx_s);
    for _ in 0..(10usize.saturating_sub(rx_s.len())) { puts(" "); }
    puts(&format!("{}", tx));
    puts("\n");

    // ── Gateway / ARP cache ──────────────────────────────────────────────
    let gw = unsafe { net::GATEWAY_IP };
    let gw_mac = unsafe { net::GATEWAY_MAC };
    puts("\n");
    hl("  ARP Cache:\n");
    dim("  IP              MAC\n");
    dim("  -------------------------------------\n");

    let mut cache = [([0u8;4],[0u8;6]); 16];
    let n = net::arp::cache_entries(&mut cache);
    for i in 0..n {
        let (cip, cmac) = cache[i];
        let ip_s = format!("  {}.{}.{}.{}", cip[0], cip[1], cip[2], cip[3]);
        puts(&ip_s);
        for _ in 0..(18usize.saturating_sub(ip_s.len())) { puts(" "); }
        for j in 0..6 {
            framebuffer::draw_char(hex[(cmac[j]>>4) as usize] as char, FG_DIM, BG);
            framebuffer::draw_char(hex[(cmac[j]&0xF) as usize] as char, FG_DIM, BG);
            if j < 5 { puts(":"); }
        }
        puts("\n");
    }
    if n == 0 {
        puts("  Gateway ");
        puts(&format!("{}.{}.{}.{}", gw[0], gw[1], gw[2], gw[3]));
        puts("  ");
        for j in 0..6 {
            framebuffer::draw_char(hex[(gw_mac[j]>>4) as usize] as char, FG_DIM, BG);
            framebuffer::draw_char(hex[(gw_mac[j]&0xF) as usize] as char, FG_DIM, BG);
            if j < 5 { puts(":"); }
        }
        puts("\n");
    }
}

// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\
// axon-ping \ Send ICMP echo request 
// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\

#[allow(dead_code)]
fn cmd_axon_ping(args: &str) {
    let args = args.trim();
    let (target, count) = {
        // parse optional -c N
        let mut words = args.splitn(3, ' ');
        let first = words.next().unwrap_or("");
        if first == "-c" {
            let n = words.next().unwrap_or("4").parse::<usize>().unwrap_or(4);
            let host = words.next().unwrap_or("8.8.8.8").trim();
            (host, n)
        } else if first.is_empty() {
            ("8.8.8.8", 4usize)
        } else {
            (first, 4usize)
        }
    };

    // Parse dotted-decimal IP
    let parts: Vec<&str> = target.split('.').collect();
    if parts.len() != 4 {
        err("ping: invalid IP format\n");
        dim("Usage: ping [-c N] <ip>\n");
        return;
    }
    let mut ip = [0u8; 4];
    for (i, part) in parts.iter().enumerate() {
        match part.trim().parse::<u8>() {
            Ok(b) => ip[i] = b,
            Err(_) => { err("ping: bad IP octet\n"); return; }
        }
    }

    if !crate::net::is_up() {
        err("ping: network is down\n");
        dim("  (start network stack first)\n");
        return;
    }

    ok("PING "); puts(target); ok(" 56(84) bytes of data.\n");

    let mut received = 0usize;
    for seq in 1..=count {
        crate::net::icmp::send_ping(ip, seq as u16);

        // Poll for reply with 3s timeout (TSC µs)
        let mut reply = None;
        let t0 = crate::arch::x86_64::tsc::tsc_stamp_us();
        while crate::arch::x86_64::tsc::tsc_stamp_us().wrapping_sub(t0) < 3_000_000 {
            reply = crate::net::icmp::poll_reply();
            if reply.is_some() { break; }
            unsafe { core::arch::asm!("hlt"); }
        }

        match reply {
            Some(r) => {
                received += 1;
                ok("64 bytes from "); puts(target);
                let rtt_us = r.rtt_us;
                if rtt_us < 1000 {
                    puts(&format!(": icmp_seq={} ttl={} time={} µs\n", seq, r.ttl, rtt_us));
                } else {
                    let ms = rtt_us / 1000;
                    let frac = (rtt_us % 1000) / 10;
                    puts(&format!(": icmp_seq={} ttl={} time={}.{:02} ms\n", seq, r.ttl, ms, frac));
                }
            }
            None => {
                err("Request timeout for icmp_seq=");
                err(&format!("{}", seq));
                err("\n");
            }
        }
    }

    // Summary
    puts("\n");
    dim(&format!("--- {} ping statistics ---\n", target));
    let lost = count - received;
    let loss_pct = if count > 0 { lost * 100 / count } else { 0 };
    dim(&format!("{} packets transmitted, {} received, {}% packet loss\n",
        count, received, loss_pct));
}

// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\
// df \ Disk/filesystem usage
// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\

#[allow(dead_code)]
fn cmd_df(_args: &str) {
    hl("  Filesystem      1K-blocks   Used  Available  Use%  Mounted on\n");
    dim("  \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\n");

    // RamFS usage
    let node_count = crate::fs::ramfs::node_count();
    // Each node is ~avg 256 bytes overhead; estimate
    let ramfs_used_kb = (node_count * 256) / 1024;
    let heap_total_kb = if crate::mm::heap::is_initialized() {
        let (used, free) = crate::mm::heap::stats();
        (used + free) / 1024
    } else { 131072 }; // 128 MiB

    puts("  ramfs           ");
    put_usize(heap_total_kb); puts("  ");
    put_usize(ramfs_used_kb); puts("  ");
    put_usize(heap_total_kb.saturating_sub(ramfs_used_kb));
    puts("  ");
    if heap_total_kb > 0 {
        put_usize(ramfs_used_kb * 100 / heap_total_kb);
    } else { puts("0"); }
    puts("%  /\n");

    // Physical disk(s)
    let disk_count = crate::drivers::disk_count();
    for i in 0..disk_count {
        if let Some(info) = crate::drivers::disk_info(i) {
            let disk_kb = info.size_mb as usize * 1024;
            // Persistence uses LBA 2048 + up to 16384 sectors -> 8 MiB
            let used_kb = 8 * 1024;
            puts("  disk");
            put_usize(i);
            puts("          ");
            put_usize(disk_kb); puts("  ");
            put_usize(used_kb); puts("  ");
            put_usize(disk_kb.saturating_sub(used_kb));
            puts("  ");
            if disk_kb > 0 {
                put_usize(used_kb * 100 / disk_kb);
            } else { puts("0"); }
            puts("%  /dev/disk");
            put_usize(i);
            puts("\n");
        }
    }
}

// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\
// kill \ Send (simulated) signal to a PID
// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\

#[allow(dead_code)]
fn cmd_kill(args: &str) {
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
    // Parse optional signal and PID
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
    // PID 0 and 1 are non-killable kernel threads
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

// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\
// printf \ Formatted output (subset: %s, %d, %i, %x, \\n, \\t)
// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\

fn cmd_printf(args: &str) {
    if args.is_empty() {
        err("printf: missing format string\n");
        dim("Usage: printf <format> [args...]\n");
        return;
    }

    // Split into format and args
    let (fmt, rest) = if args.starts_with('"') {
        if let Some(end) = args[1..].find('"') {
            (&args[1..end + 1], args[end + 2..].trim())
        } else { (args, "") }
    } else {
        if let Some(pos) = args.find(' ') {
            (&args[..pos], args[pos + 1..].trim())
        } else { (args, "") }
    };

    let arg_list: Vec<&str> = rest.split_whitespace().collect();
    let mut arg_idx = 0usize;

    let mut i = 0usize;
    let bytes = fmt.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'n' => puts("\n"),
                b't' => puts("\t"),
                b'r' => puts("\r"),
                b'\\' => puts("\\"),
                _ => { puts("\\"); framebuffer::draw_char(bytes[i + 1] as char, FG, BG); }
            }
            i += 2;
        } else if bytes[i] == b'%' && i + 1 < bytes.len() {
            let spec = bytes[i + 1];
            let val = if arg_idx < arg_list.len() { arg_list[arg_idx] } else { "" };
            arg_idx += 1;
            match spec {
                b's' => puts(val),
                b'd' | b'i' => {
                    let n: i64 = val.trim().parse().unwrap_or(0);
                    puts(&format!("{}", n));
                }
                b'x' => {
                    let n: u64 = val.trim().parse().unwrap_or(0);
                    puts(&format!("{:x}", n));
                }
                b'X' => {
                    let n: u64 = val.trim().parse().unwrap_or(0);
                    puts(&format!("{:X}", n));
                }
                b'%' => { puts("%"); arg_idx -= 1; }
                _ => { puts("%"); framebuffer::draw_char(spec as char, FG, BG); arg_idx -= 1; }
            }
            i += 2;
        } else {
            framebuffer::draw_char(bytes[i] as char, FG, BG);
            i += 1;
        }
    }
}

// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\
// env \ Print or run with modified environment variables
// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\

fn cmd_env(args: &str) {
    if !args.is_empty() {
        // Try to run command with env vars (just dispatch the rest)
        dim("env: command exec not yet supported вЂ” showing environment\n\n");
    }
    // Print current shell environment from plum
    let env = plum::get_env();
    if env.is_empty() {
        dim("(no environment variables set)\n");
    } else {
        for (k, v) in &env {
            puts(k); puts("="); puts(v); puts("\n");
        }
    }
}

// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\
// which \ Show which subsystem provides a command
// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\

fn cmd_which(args: &str) {
    let cmd = args.trim();
    if cmd.is_empty() {
        err("which: missing operand\n");
        return;
    }
    // Check AXON
    for axon_cmd in command_list() {
        if *axon_cmd == cmd {
            ok("/bin/"); ok(cmd); ok("  [axon built-in]\n");
            return;
        }
    }
    // Check terminal builtins (common names)
    let builtins = ["ls", "cd", "pwd", "cat", "echo", "mkdir", "touch", "rm",
                    "help", "version", "dmesg", "lspci", "free", "ping", "reboot",
                    "shutdown", "clear", "history", "sync", "uname", "date",
                    "install", "tutor", "grape", "tomato", "seed", "doom"];
    for b in builtins.iter() {
        if *b == cmd {
            ok("/bin/"); ok(cmd); ok("  [kernel built-in]\n");
            return;
        }
    }
    err(cmd); err(": not found\n");
}

// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\
// xargs \ Build and execute commands from stdin-like arguments
// \\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\\

fn cmd_xargs(args: &str) {
    // xargs <cmd> [fixed-args...] -- builds: cmd fixed-args item
    // In a non-piped terminal this operates on whitespace-separated args
    // as if they were lines from stdin.
    let (cmd, items_str) = if let Some(pos) = args.find(' ') {
        (&args[..pos], args[pos + 1..].trim())
    } else { (args.trim(), "") };

    if cmd.is_empty() {
        err("xargs: missing command\n");
        dim("Usage: xargs <cmd> [items...]\n");
        return;
    }

    if items_str.is_empty() {
        err("xargs: no input items provided\n");
        return;
    }

    for item in items_str.split_whitespace() {
        let full_args = format!("{} {}", cmd, item);
        dim(&format!("+ {}\n", full_args));
        // Try dispatching through AXON first, then terminal builtins
        if !dispatch(cmd, &format!("{}", item)) {
            // Not an axon command вЂ” let plum/terminal handle it
            // We can call the full command via the shell's exec path
            let _ = full_args; // command will be executed by plum on next prompt
            err("xargs: '"); err(cmd); err("' is not an axon command (try running individually)\n");
        }
    }
}

/// Return list of all AXON command names (for Tab completion)
pub fn command_list() -> &'static [&'static str] {
    &[
        "wc", "head", "tail", "grep", "find", "cp", "mv", "tee",
        "stat", "du", "tree", "basename", "dirname", "yes", "true",
        "false", "seq", "sort", "uniq", "cut", "rev", "xxd", "nl",
        "diff", "awk", "ps", "top", "kill",
        "netstat", "df", "ping", "printf", "env", "which", "xargs",
        "verify_mem", "verify_sched", "verify_net", "verify_audio",
    ]
}
