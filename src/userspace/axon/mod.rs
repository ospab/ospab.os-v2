/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

axon — Coreutils for ospab.os (AETERNA)

A collection of POSIX-compatible essential utilities, all operating
exclusively through the VFS syscall layer (crate::fs::*).

Utilities:
  wc       — Count lines/words/bytes
  head     — Display first N lines
  tail     — Display last N lines
  grep     — Pattern search in files
  find     — Search for files in directory tree
  cp       — Copy files
  mv       — Move / rename files
  tee      — Write stdin to file + stdout (echo variant)
  stat     — Show file/directory info
  du       — Estimate file space usage (sizes of files)
  tree     — Directory tree display
  basename — Strip directory prefix from path
  dirname  — Strip last component from path
  yes      — Repeatedly output a string (limited)
  true/false — Exit code utilities
  seq      — Print number sequences
  sort     — Sort lines in a file
  uniq     — Remove duplicate adjacent lines
  cut      — Extract fields/columns from text
  rev      — Reverse lines
*/

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;

use crate::arch::x86_64::framebuffer;
use crate::fs;
use crate::userspace::plum;

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
        _          => false,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// wc — Count lines/words/bytes
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// head — Display first N lines (default 10)
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// tail — Display last N lines (default 10)
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// grep — Pattern search (exact substring match)
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// find — Search for files in directory tree
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// cp — Copy file
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// mv — Move/rename file
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// tee — Write text to file + display
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// stat — Show file/directory info
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// du — Estimate file space usage
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// tree — Directory tree display
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// basename / dirname — Path manipulation
// ═══════════════════════════════════════════════════════════════════════════

fn cmd_basename(args: &str) {
    if args.is_empty() {
        err("basename: missing operand\n");
        return;
    }
    let path = args.trim().trim_end_matches('/');
    if let Some(pos) = path.rfind('/') {
        puts(&path[pos + 1..]); puts("\n");
    } else {
        puts(path); puts("\n");
    }
}

fn cmd_dirname(args: &str) {
    if args.is_empty() {
        err("dirname: missing operand\n");
        return;
    }
    let path = args.trim().trim_end_matches('/');
    if let Some(pos) = path.rfind('/') {
        if pos == 0 { puts("/\n"); } else { puts(&path[..pos]); puts("\n"); }
    } else {
        puts(".\n");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// yes — Output string repeatedly (limited to 100 lines)
// ═══════════════════════════════════════════════════════════════════════════

fn cmd_yes(args: &str) {
    let text = if args.is_empty() { "y" } else { args.trim() };
    for _ in 0..100 {
        puts(text); puts("\n");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// seq — Print number sequence
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// sort — Sort lines of a file
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// uniq — Remove duplicate adjacent lines
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// cut — Extract columns/fields
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// rev — Reverse each line
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// xxd — Hex dump of file
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// nl — Number lines of a file
// ═══════════════════════════════════════════════════════════════════════════

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

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

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

/// Return list of all AXON command names (for Tab completion)
pub fn command_list() -> &'static [&'static str] {
    &[
        "wc", "head", "tail", "grep", "find", "cp", "mv", "tee",
        "stat", "du", "tree", "basename", "dirname", "yes", "true",
        "false", "seq", "sort", "uniq", "cut", "rev", "xxd", "nl",
    ]
}
