/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

plum — Command shell for ospab.os (AETERNA)

A lightweight POSIX-inspired shell with:
  - Environment variables ($VAR, export VAR=VALUE)
  - Command aliases (alias name=command)
  - Command chaining (cmd1 ; cmd2)
  - Shell builtins: export, alias, unalias, set, source, type, env
  - VFS commands: ls, cat, touch, mkdir, rm, cd, pwd, echo, write, save
  - Startup script: /etc/plum/plumrc
  - Prompt customization via $PS1

ALL file operations use the VFS syscall layer (crate::fs::*).
No raw device or architecture access is used for I/O.
*/

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use alloc::collections::BTreeMap;

pub mod bash;

// ─── Shell state ────────────────────────────────────────────────────────────

/// The shell state — persists across commands
pub struct PlumShell {
    /// Environment variables
    env: BTreeMap<String, String>,
    /// Command aliases
    aliases: BTreeMap<String, String>,
    /// Last command exit code
    last_exit: i32,
    /// Is the shell initialized?
    initialized: bool,
}

// ─── Global singleton shell state ───────────────────────────────────────────

static mut SHELL: Option<PlumShell> = None;

/// Get the global shell instance, initializing if needed
fn shell() -> &'static mut PlumShell {
    unsafe {
        if SHELL.is_none() {
            let mut sh = PlumShell {
                env: BTreeMap::new(),
                aliases: BTreeMap::new(),
                last_exit: 0,
                initialized: false,
            };
            // Default environment
            sh.env.insert(String::from("HOME"), String::from("/home/root"));
            sh.env.insert(String::from("USER"), String::from("root"));
            sh.env.insert(String::from("SHELL"), String::from("/bin/plum"));
            sh.env.insert(String::from("PATH"), String::from("/bin:/sbin:/usr/bin"));
            sh.env.insert(String::from("PS1"), String::from("\\u@\\h:\\w# "));
            sh.env.insert(String::from("TERM"), String::from("aeterna-fb"));
            sh.env.insert(String::from("EDITOR"), String::from("grape"));
            sh.env.insert(String::from("HOSTNAME"), String::from("ospab"));
            sh.env.insert(String::from("OSTYPE"), String::from("ospab-os"));
            sh.env.insert(String::from("LANG"), String::from("en_US.UTF-8"));
            sh.env.insert(String::from("PWD"), String::from("/"));

            // Default aliases
            sh.aliases.insert(String::from("ll"), String::from("ls -l"));
            sh.aliases.insert(String::from("la"), String::from("ls -a"));
            sh.aliases.insert(String::from("cls"), String::from("clear"));
            sh.aliases.insert(String::from("q"), String::from("exit"));
            sh.aliases.insert(String::from("h"), String::from("history"));
            sh.aliases.insert(String::from("edit"), String::from("grape"));

            SHELL = Some(sh);
        }
        SHELL.as_mut().unwrap()
    }
}

// ─── Framebuffer output helpers ─────────────────────────────────────────────

use crate::arch::x86_64::framebuffer;

const FG: u32      = 0x00FFFFFF;
const FG_OK: u32   = 0x0000FF00;
const FG_ERR: u32  = 0x00FF4444;
const FG_WARN: u32 = 0x0000FFFF;
const FG_DIM: u32  = 0x00AAAAAA;
const FG_VAR: u32  = 0x0000FFFF;
const FG_DIR: u32  = 0x005555FF;
const BG: u32      = 0x00000000;

fn puts(s: &str) { framebuffer::draw_string(s, FG, BG); }
fn ok(s: &str)   { framebuffer::draw_string(s, FG_OK, BG); }
fn err(s: &str)  { framebuffer::draw_string(s, FG_ERR, BG); }
#[allow(dead_code)]
fn warn(s: &str) { framebuffer::draw_string(s, FG_WARN, BG); }
fn dim(s: &str)  { framebuffer::draw_string(s, FG_DIM, BG); }

/// Print a decimal number in dim color
fn dim_dec(mut val: u64) {
    if val == 0 {
        dim("0");
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while val > 0 {
        buf[i] = b'0' + (val % 10) as u8;
        val /= 10;
        i += 1;
    }
    let mut tmp = [0u8; 20];
    for j in 0..i {
        tmp[j] = buf[i - 1 - j];
    }
    if let Ok(s) = core::str::from_utf8(&tmp[..i]) {
        dim(s);
    }
}

// ─── Initialization ─────────────────────────────────────────────────────────

/// Initialize plum shell. Called once at boot.
/// Loads /etc/plum/plumrc if it exists.
pub fn init() {
    let sh = shell();
    if sh.initialized { return; }
    sh.initialized = true;

    // Create default config file if it doesn't exist — VFS only
    if !crate::fs::exists("/etc/plum") {
        crate::fs::mkdir("/etc/plum");
    }
    if !crate::fs::exists("/etc/plum/plumrc") {
        let default_rc = concat!(
            "# plum shell configuration\n",
            "# Aliases\n",
            "alias ll='ls -l'\n",
            "alias la='ls -a'\n",
            "alias cls='clear'\n",
            "alias edit='grape'\n",
            "\n",
            "# Environment\n",
            "export EDITOR=grape\n",
            "export PAGER=cat\n",
        );
        crate::fs::write_file("/etc/plum/plumrc", default_rc.as_bytes());
    }

    // Execute plumrc
    source_file("/etc/plum/plumrc");
}

// ─── Public API ─────────────────────────────────────────────────────────────

/// Preprocess a command line: expand aliases, variables, handle builtins and
/// VFS commands.
///
/// Returns `None` if the command was fully handled by plum (builtin or
/// VFS command).
/// Returns `Some(expanded)` if the command should be dispatched to the
/// terminal for execution (external commands like help, dmesg, etc.).
pub fn preprocess(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() { return None; }

    // Handle command chaining (;)
    if input.contains(';') {
        let parts: Vec<&str> = input.split(';').collect();
        let mut final_result = None;
        for part in parts {
            final_result = preprocess(part.trim());
        }
        return final_result;
    }

    // Expand aliases
    let expanded = expand_aliases(input);
    let expanded = expanded.trim();

    // Expand environment variables
    let expanded = expand_variables(&expanded);
    let expanded = expanded.trim();

    if expanded.is_empty() { return None; }

    // Check if it's a shell builtin
    let (cmd, args) = split_first_word(&expanded);

    match cmd {
        // ── Shell builtins ──
        "export"  => { builtin_export(args); return None; }
        "set"     => { builtin_set(args); return None; }
        "unset"   => { builtin_unset(args); return None; }
        "alias"   => { builtin_alias(args); return None; }
        "unalias" => { builtin_unalias(args); return None; }
        "env"     => { builtin_env(); return None; }
        "source" | "." => { builtin_source(args); return None; }
        "type"    => { builtin_type(args); return None; }
        "bash"    => { builtin_bash(args); return None; }
        "plum"    => { builtin_plum_info(); return None; }

        // ── VFS commands — all go through crate::fs syscalls ──
        "ls"      => { cmd_ls(args); return None; }
        "cat"     => { cmd_cat(args); return None; }
        "touch"   => { cmd_touch(args); return None; }
        "mkdir"   => { cmd_mkdir(args); return None; }
        "rm"      => { cmd_rm(args); return None; }
        "write"   => { cmd_write(args); return None; }
        "save"    => { cmd_save(); return None; }
        "cd"      => { cmd_cd(args); return None; }
        "pwd"     => { cmd_pwd(); return None; }

        _ => {}
    }

    // Return the expanded command for the terminal to execute
    Some(String::from(expanded))
}

/// Get the value of an environment variable
pub fn getenv(name: &str) -> Option<&'static str> {
    let sh = shell();
    sh.env.get(name).map(|s| {
        // Safety: we need a &'static str but our String lives in the global SHELL
        // which is never deallocated, so this is safe
        unsafe { &*(s.as_str() as *const str) }
    })
}

/// Set an environment variable
pub fn setenv(name: &str, value: &str) {
    let sh = shell();
    sh.env.insert(String::from(name), String::from(value));
}

/// Get all environment variables as a Vec of (name, value) pairs
pub fn get_env() -> Vec<(String, String)> {
    let sh = shell();
    sh.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// Set the last exit code
pub fn set_exit_code(code: i32) {
    let sh = shell();
    sh.last_exit = code;
    let code_str = if code >= 0 {
        usize_to_string(code as usize)
    } else {
        let mut s = String::from("-");
        s.push_str(&usize_to_string((-code) as usize));
        s
    };
    sh.env.insert(String::from("?"), code_str);
}

/// Get current working directory from plum env
pub fn cwd() -> &'static str {
    getenv("PWD").unwrap_or("/")
}

/// Resolve a user-supplied path to an absolute path using the shell's PWD.
pub fn resolve_path(path: &str) -> String {
    if path == "~" {
        return String::from(getenv("HOME").unwrap_or("/"));
    }
    if path.starts_with('/') {
        // Already absolute
        let mut s = String::from(path);
        while s.len() > 1 && s.ends_with('/') {
            s.pop();
        }
        return s;
    }
    // Relative path: join with PWD
    let pwd = cwd();
    let mut abs = String::from(pwd);
    if !abs.ends_with('/') {
        abs.push('/');
    }
    abs.push_str(path);
    // Normalize: remove trailing slashes
    while abs.len() > 1 && abs.ends_with('/') {
        abs.pop();
    }
    abs
}

// ═══════════════════════════════════════════════════════════════════════════
// VFS commands — all use crate::fs::* syscall layer exclusively
// ═══════════════════════════════════════════════════════════════════════════

/// ls [path] — list directory contents via VFS readdir()
fn cmd_ls(args: &str) {
    let raw_path = if args.is_empty() { cwd() } else { args };
    let abs_path = resolve_path(raw_path);

    // Refresh /proc virtual files if listing /proc
    if abs_path.starts_with("/proc") {
        crate::fs::ramfs::refresh_proc_files();
    }

    let entries = match crate::fs::readdir(&abs_path) {
        Some(e) => e,
        None => {
            if !crate::fs::exists(&abs_path) {
                err("ls: cannot access '");
                err(raw_path);
                err("': No such file or directory\n");
            } else {
                dim("(empty directory)\n");
            }
            return;
        }
    };
    if entries.is_empty() {
        dim("(empty directory)\n");
        return;
    }
    for e in &entries {
        match e.node_type {
            crate::fs::NodeType::Directory => {
                framebuffer::draw_string("d ", FG_DIM, BG);
                framebuffer::draw_string(&e.name, FG_DIR, BG);
                framebuffer::draw_string("/\n", FG_DIR, BG);
            }
            crate::fs::NodeType::File | crate::fs::NodeType::CharDevice => {
                framebuffer::draw_string("- ", FG_DIM, BG);
                puts(&e.name);
                let pad = if e.name.len() < 16 { 16 - e.name.len() } else { 1 };
                for _ in 0..pad { puts(" "); }
                if e.size > 0 {
                    dim_dec(e.size as u64);
                } else {
                    dim("0");
                }
                puts("\n");
            }
        }
    }
}

/// cat <file> — read and print file contents via VFS read_file()
fn cmd_cat(args: &str) {
    if args.is_empty() {
        err("cat: missing operand\n");
        return;
    }

    let abs = resolve_path(args);

    // Refresh /proc before reading
    if abs.starts_with("/proc") {
        crate::fs::ramfs::refresh_proc_files();
    }

    match crate::fs::read_file(&abs) {
        Some(data) => {
            if data.is_empty() {
                return; // empty file (e.g. /dev/null)
            }
            if let Ok(text) = core::str::from_utf8(&data) {
                puts(text);
                if !text.ends_with('\n') {
                    puts("\n");
                }
            } else {
                err("cat: ");
                err(args);
                err(": binary file (");
                puts(&usize_to_string(data.len()));
                err(" bytes)\n");
            }
        }
        None => {
            err("cat: ");
            err(args);
            err(": No such file or directory\n");
        }
    }
}

/// touch <file> — create empty file via VFS touch()
fn cmd_touch(args: &str) {
    if args.is_empty() {
        err("touch: missing file operand\n");
        return;
    }
    let abs = resolve_path(args);
    if crate::fs::exists(&abs) {
        return; // touch existing file is a no-op
    }
    if !crate::fs::touch(&abs) {
        err("touch: cannot touch '");
        err(args);
        err("'\n");
    }
}

/// mkdir <dir> — create directory via VFS mkdir()
fn cmd_mkdir(args: &str) {
    if args.is_empty() {
        err("mkdir: missing operand\n");
        return;
    }
    let abs = resolve_path(args);
    if crate::fs::exists(&abs) {
        err("mkdir: cannot create directory '");
        err(args);
        err("': File exists\n");
        return;
    }
    if !crate::fs::mkdir(&abs) {
        err("mkdir: cannot create directory '");
        err(args);
        err("'\n");
    }
}

/// rm <file> — remove file via VFS remove()
fn cmd_rm(args: &str) {
    if args.is_empty() {
        err("rm: missing operand\n");
        return;
    }
    let abs = resolve_path(args);
    if !crate::fs::exists(&abs) {
        err("rm: cannot remove '");
        err(args);
        err("': No such file or directory\n");
        return;
    }
    if !crate::fs::remove(&abs) {
        err("rm: cannot remove '");
        err(args);
        err("': Is a directory or not empty\n");
    }
}

/// write <file> <content> — write text to a file via VFS write_file()
fn cmd_write(args: &str) {
    if args.is_empty() {
        err("write: usage: write <file> <content>\n");
        return;
    }
    let (path, content) = split_first_word(args);
    if content.is_empty() {
        err("write: missing content\n");
        return;
    }
    let abs = resolve_path(path);
    if crate::fs::write_file(&abs, content.as_bytes()) {
        ok("Written ");
        puts(&usize_to_string(content.len()));
        puts(" bytes to ");
        puts(path);
        puts("\n");
    } else {
        err("write: cannot write to '");
        err(path);
        err("'\n");
    }
}

/// save — sync the entire RamFS to disk via disk_sync
fn cmd_save() {
    if !crate::fs::disk_sync::is_dirty() {
        ok("[OK] ");
        puts("Filesystem is already clean — nothing to save.\n");
        return;
    }
    puts("Syncing filesystem to disk...\n");
    crate::klog::record(crate::klog::EventSource::Boot, "save: sync requested");

    if crate::fs::disk_sync::sync_filesystem() {
        // Report bytes written: serialize again to get size (or use node count)
        let count = crate::fs::ramfs::node_count();
        ok("[OK] ");
        puts("Filesystem saved to disk (");
        puts(&usize_to_string(count));
        puts(" nodes synchronized).\n");
    } else {
        err("[FAIL] ");
        err("Failed to save filesystem to disk.\n");
        puts("Possible causes: no storage device, or filesystem empty.\n");
    }
}

/// cd [dir] — change working directory, verified via VFS exists() + stat()
fn cmd_cd(args: &str) {
    if args.is_empty() || args == "~" || args == "/" {
        setenv("PWD", "/");
        return;
    }
    if args == ".." {
        let pwd = String::from(cwd());
        if pwd.len() > 1 {
            if let Some(pos) = pwd.rfind('/') {
                if pos == 0 {
                    setenv("PWD", "/");
                } else {
                    setenv("PWD", &pwd[..pos]);
                }
            }
        }
        return;
    }
    let abs = resolve_path(args);
    if crate::fs::exists(&abs) {
        if let Some(stat) = crate::fs::stat(&abs) {
            match stat.node_type {
                crate::fs::NodeType::Directory => {
                    setenv("PWD", &abs);
                    return;
                }
                _ => {
                    err("cd: ");
                    err(args);
                    err(": Not a directory\n");
                    return;
                }
            }
        }
    }
    err("cd: ");
    err(args);
    err(": No such directory\n");
}

/// pwd — print working directory from env
fn cmd_pwd() {
    puts(cwd());
    puts("\n");
}

// ═══════════════════════════════════════════════════════════════════════════
// Shell builtins
// ═══════════════════════════════════════════════════════════════════════════

fn builtin_export(args: &str) {
    if args.is_empty() {
        let sh = shell();
        for (k, v) in sh.env.iter() {
            puts("export ");
            framebuffer::draw_string(k, FG_VAR, BG);
            puts("=\"");
            puts(v);
            puts("\"\n");
        }
        return;
    }

    if let Some(eq) = args.find('=') {
        let name = args[..eq].trim();
        let val = args[eq + 1..].trim().trim_matches('"').trim_matches('\'');
        setenv(name, val);
    } else {
        let name = args.trim();
        if shell().env.get(name).is_none() {
            setenv(name, "");
        }
    }
}

fn builtin_set(args: &str) {
    if args.is_empty() {
        let sh = shell();
        for (k, v) in sh.env.iter() {
            framebuffer::draw_string(k, FG_VAR, BG);
            puts("=");
            puts(v);
            puts("\n");
        }
        return;
    }

    if let Some(eq) = args.find('=') {
        let name = args[..eq].trim();
        let val = args[eq + 1..].trim();
        setenv(name, val);
    }
}

fn builtin_unset(args: &str) {
    if args.is_empty() {
        err("unset: not enough arguments\n");
        return;
    }
    let sh = shell();
    sh.env.remove(args.trim());
}

fn builtin_alias(args: &str) {
    let sh = shell();

    if args.is_empty() {
        for (k, v) in sh.aliases.iter() {
            puts("alias ");
            framebuffer::draw_string(k, FG_VAR, BG);
            puts("='");
            puts(v);
            puts("'\n");
        }
        return;
    }

    if let Some(eq) = args.find('=') {
        let name = args[..eq].trim();
        let val = args[eq + 1..].trim().trim_matches('\'').trim_matches('"');
        sh.aliases.insert(String::from(name), String::from(val));
    } else {
        let name = args.trim();
        if let Some(val) = sh.aliases.get(name) {
            puts("alias ");
            framebuffer::draw_string(name, FG_VAR, BG);
            puts("='");
            puts(val);
            puts("'\n");
        } else {
            err("plum: alias not found: ");
            puts(name);
            puts("\n");
        }
    }
}

fn builtin_unalias(args: &str) {
    if args.is_empty() {
        err("unalias: not enough arguments\n");
        return;
    }
    let sh = shell();
    if sh.aliases.remove(args.trim()).is_some() {
        ok("Alias removed.\n");
    } else {
        err("plum: alias not found: ");
        puts(args.trim());
        puts("\n");
    }
}

fn builtin_env() {
    let sh = shell();
    for (k, v) in sh.env.iter() {
        framebuffer::draw_string(k, FG_VAR, BG);
        puts("=");
        puts(v);
        puts("\n");
    }
}

fn builtin_source(args: &str) {
    if args.is_empty() {
        err("source: filename argument required\n");
        return;
    }
    source_file(args.trim());
}

fn builtin_type(args: &str) {
    if args.is_empty() {
        err("type: not enough arguments\n");
        return;
    }

    let name = args.trim();
    let sh = shell();

    // Check aliases
    if let Some(val) = sh.aliases.get(name) {
        puts(name);
        puts(" is aliased to '");
        puts(val);
        puts("'\n");
        return;
    }

    // Check builtins
    let builtins = [
        "export", "set", "unset", "alias", "unalias", "env",
        "source", "type", "plum", "cd", "pwd", "echo", "exit",
        "ls", "cat", "touch", "mkdir", "rm", "write", "save", "bash",
    ];
    for b in builtins.iter() {
        if *b == name {
            puts(name);
            puts(" is a shell builtin\n");
            return;
        }
    }

    // Known terminal commands
    let commands = [
        "help", "clear", "ver", "version", "uname", "whoami", "hostname",
        "date", "about", "free", "meminfo", "uptime", "dmesg", "lsmem",
        "lspci", "lsblk", "fdisk", "ping", "ifconfig", "ip", "ntpdate",
        "reboot", "shutdown", "poweroff", "halt", "install", "history",
        "tutor", "grape", "tomato", "sync", "dump_disk", "doom",
    ];
    for c in commands.iter() {
        if *c == name {
            puts(name);
            puts(" is /bin/");
            puts(name);
            puts("\n");
            return;
        }
    }

    err("plum: not found: ");
    puts(name);
    puts("\n");
}

fn builtin_bash(args: &str) {
    let args = args.trim();
    if args.is_empty() {
        err("bash: usage: bash [OPTIONS] script.sh [ARGS]\n");
        return;
    }

    let (file_or_code, remaining) = split_first_word(args);

    if file_or_code == "-c" {
        bash::execute_code(remaining);
    } else {
        let exit_code = bash::execute_script(file_or_code);
        set_exit_code(exit_code);
    }
}

fn builtin_plum_info() {
    puts("\n");
    framebuffer::draw_string("plum", FG_OK, BG);
    puts(" — command shell for ospab.os\n");
    puts("Version 2.0.0\n\n");

    framebuffer::draw_string("Features:\n", FG_WARN, BG);
    puts("  - Environment variables ($VAR, export)\n");
    puts("  - Command aliases (alias name=command)\n");
    puts("  - Variable expansion in commands\n");
    puts("  - Command chaining (cmd1 ; cmd2)\n");
    puts("  - Output redirection (>, >>)\n");
    puts("  - VFS-only filesystem commands\n");
    puts("  - Disk persistence (save command)\n");
    puts("  - Startup config: /etc/plum/plumrc\n\n");

    framebuffer::draw_string("Shell builtins:\n", FG_WARN, BG);
    puts("  export    Set/show environment variables\n");
    puts("  set       Show all variables\n");
    puts("  unset     Remove a variable\n");
    puts("  alias     Define/show command aliases\n");
    puts("  unalias   Remove an alias\n");
    puts("  env       Print all environment variables\n");
    puts("  source    Execute commands from file\n");
    puts("  type      Show command type\n\n");

    framebuffer::draw_string("VFS commands:\n", FG_WARN, BG);
    puts("  ls        List directory (VFS readdir)\n");
    puts("  cat       Read file (VFS read_file)\n");
    puts("  touch     Create empty file (VFS touch)\n");
    puts("  mkdir     Create directory (VFS mkdir)\n");
    puts("  rm        Remove file (VFS remove)\n");
    puts("  cd        Change directory\n");
    puts("  pwd       Print working directory\n");
    puts("  write     Write text to file (VFS write_file)\n");
    puts("  save      Sync filesystem to disk\n\n");

    dim("Shell config: /etc/plum/plumrc\n");
    dim("Edit with: grape /etc/plum/plumrc\n\n");
}

// ─── Source (execute) a script file ─────────────────────────────────────────

fn source_file(path: &str) {
    // Read file via VFS
    if let Some(data) = crate::fs::read_file(path) {
        if let Ok(text) = core::str::from_utf8(&data) {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') { continue; }
                preprocess(line);
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Variable expansion
// ═══════════════════════════════════════════════════════════════════════════

fn expand_variables(input: &str) -> String {
    let sh = shell();
    let bytes = input.as_bytes();
    let mut result = String::new();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() {
            let start = i + 1;
            let mut end = start;

            if bytes[start] == b'{' {
                // ${VAR} syntax
                end = start + 1;
                while end < bytes.len() && bytes[end] != b'}' {
                    end += 1;
                }
                let name = &input[start + 1..end];
                if let Some(val) = sh.env.get(name) {
                    result.push_str(val);
                }
                i = end + 1;
            } else if bytes[start] == b'?' {
                // $?
                if let Some(val) = sh.env.get("?") {
                    result.push_str(val);
                } else {
                    result.push('0');
                }
                i = start + 1;
            } else {
                // $VAR syntax
                while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                    end += 1;
                }
                let name = &input[start..end];
                if let Some(val) = sh.env.get(name) {
                    result.push_str(val);
                }
                i = end;
            }
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }

    result
}

// ─── Alias expansion ───────────────────────────────────────────────────────

fn expand_aliases(input: &str) -> String {
    let sh = shell();
    let (cmd, args) = split_first_word(input);

    if let Some(expansion) = sh.aliases.get(cmd) {
        let mut result = expansion.clone();
        if !args.is_empty() {
            result.push(' ');
            result.push_str(args);
        }
        result
    } else {
        String::from(input)
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn split_first_word(s: &str) -> (&str, &str) {
    let s = s.trim();
    match s.find(' ') {
        Some(pos) => (&s[..pos], s[pos + 1..].trim()),
        None => (s, ""),
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
