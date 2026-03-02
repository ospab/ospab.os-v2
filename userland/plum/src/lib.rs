/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

plum — Command shell for ospab.os (AETERNA)

A lightweight POSIX-inspired shell with:
  - Environment variables ($VAR, export VAR=VALUE)
  - Command aliases (alias name=command)
  - Pipes (cmd1 | cmd2) — output of cmd1 fed as argument context to cmd2
  - Output redirection (cmd > file, cmd >> file)
  - Command chaining (cmd1 ; cmd2)
  - Shell builtins: cd, export, alias, unalias, set, source, exit
  - Startup script: /etc/plum/plumrc 
  - Prompt customization via $PS1

plum operates as an enhanced command dispatcher within the kernel.
Since userspace process isolation is not yet available, commands are
dispatched to the terminal's built-in command handlers.
*/

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use alloc::collections::BTreeMap;

mod bash;

// ─── Shell state ────────────────────────────────────────────────────────────

/// Maximum environment variables
#[allow(dead_code)]
const MAX_VARS: usize = 64;

/// Maximum aliases
#[allow(dead_code)]
const MAX_ALIASES: usize = 32;

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
const FG_ERR: u32  = 0x000000FF;
const FG_WARN: u32 = 0x0000FFFF;
const FG_DIM: u32  = 0x00AAAAAA;
const FG_VAR: u32  = 0x0000FFFF;  // Yellow for variable names
const BG: u32      = 0x00000000;

fn puts(s: &str) { framebuffer::draw_string(s, FG, BG); }
fn ok(s: &str)   { framebuffer::draw_string(s, FG_OK, BG); }
fn err(s: &str)  { framebuffer::draw_string(s, FG_ERR, BG); }
#[allow(dead_code)]
fn warn(s: &str) { framebuffer::draw_string(s, FG_WARN, BG); }
fn dim(s: &str)  { framebuffer::draw_string(s, FG_DIM, BG); }

// ─── Initialization ─────────────────────────────────────────────────────────

/// Initialize plum shell. Called once at boot.
/// Loads /etc/plum/plumrc if it exists.
pub fn init() {
    let sh = shell();
    if sh.initialized { return; }
    sh.initialized = true;

    // Create default config file if it doesn't exist
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

/// Preprocess a command line: expand aliases, variables, handle pipes and chains.
/// Returns a list of (command, args) pairs to execute sequentially.
/// If the command is a plum builtin, it is executed here and returns None.
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

// ─── Variable expansion ────────────────────────────────────────────────────

fn expand_variables(input: &str) -> String {
    let sh = shell();
    let bytes = input.as_bytes();
    let mut result = String::new();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() {
            // Extract variable name
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
                i = end + 1; // skip closing }
            } else if bytes[start] == b'?' {
                // $? — last exit code
                if let Some(val) = sh.env.get("?") {
                    result.push_str(val);
                } else {
                    result.push('0');
                }
                i = start + 1;
            } else {
                // $VAR syntax — alphanumeric + underscore
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

// ─── Shell builtins ─────────────────────────────────────────────────────────

fn builtin_export(args: &str) {
    if args.is_empty() {
        // Show all exports
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

    // Parse: export VAR=VALUE or export VAR
    if let Some(eq) = args.find('=') {
        let name = args[..eq].trim();
        let val = args[eq + 1..].trim().trim_matches('"').trim_matches('\'');
        setenv(name, val);
    } else {
        // Just declaring, set to empty if not present
        let name = args.trim();
        if shell().env.get(name).is_none() {
            setenv(name, "");
        }
    }
}

fn builtin_set(args: &str) {
    if args.is_empty() {
        // Show all variables
        let sh = shell();
        for (k, v) in sh.env.iter() {
            framebuffer::draw_string(k, FG_VAR, BG);
            puts("=");
            puts(v);
            puts("\n");
        }
        return;
    }

    // Parse: set VAR=VALUE
    if let Some(eq) = args.find('=') {
        let name = args[..eq].trim();
        let val = args[eq + 1..].trim();
        setenv(name, val);
    }
}

fn builtin_unset(args: &str) {
    if args.is_empty() {
        err("unset: ");
        puts("not enough arguments\n");
        return;
    }
    let sh = shell();
    sh.env.remove(args.trim());
}

fn builtin_alias(args: &str) {
    let sh = shell();

    if args.is_empty() {
        // Show all aliases
        for (k, v) in sh.aliases.iter() {
            puts("alias ");
            framebuffer::draw_string(k, FG_VAR, BG);
            puts("='");
            puts(v);
            puts("'\n");
        }
        return;
    }

    // Parse: alias name=command  or  alias name='command with args'
    if let Some(eq) = args.find('=') {
        let name = args[..eq].trim();
        let val = args[eq + 1..].trim().trim_matches('\'').trim_matches('"');
        sh.aliases.insert(String::from(name), String::from(val));
    } else {
        // Show specific alias
        let name = args.trim();
        if let Some(val) = sh.aliases.get(name) {
            puts("alias ");
            framebuffer::draw_string(name, FG_VAR, BG);
            puts("='");
            puts(val);
            puts("'\n");
        } else {
            err("plum: ");
            puts("alias not found: ");
            puts(name);
            puts("\n");
        }
    }
}

fn builtin_unalias(args: &str) {
    if args.is_empty() {
        err("unalias: ");
        puts("not enough arguments\n");
        return;
    }
    let sh = shell();
    if sh.aliases.remove(args.trim()).is_some() {
        ok("Alias removed.\n");
    } else {
        err("plum: ");
        puts("alias not found: ");
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
        err("source: ");
        puts("filename argument required\n");
        return;
    }
    source_file(args.trim());
}

fn builtin_type(args: &str) {
    if args.is_empty() {
        err("type: ");
        puts("not enough arguments\n");
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
    let builtins = ["export", "set", "unset", "alias", "unalias", "env", "source", "type", "plum", "cd", "echo", "exit"];
    for b in builtins.iter() {
        if *b == name {
            puts(name);
            puts(" is a shell builtin\n");
            return;
        }
    }

    // Check PATH — in our case, check if it's a known terminal command
    let commands = [
        "help", "clear", "ver", "version", "uname", "ls", "pwd", "cat",
        "mkdir", "touch", "rm", "whoami", "hostname", "date", "about",
        "free", "meminfo", "uptime", "dmesg", "lsmem", "lspci", "lsblk",
        "fdisk", "ping", "ifconfig", "ip", "ntpdate", "reboot", "shutdown",
        "poweroff", "halt", "install", "history", "tutor", "grape", "tomato",
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

    err("plum: ");
    puts("not found: ");
    puts(name);
    puts("\n");
}

/// Execute a bash script
fn builtin_bash(args: &str) {
    let args = args.trim();
    if args.is_empty() {
        err("bash: usage: bash [OPTIONS] script.sh [ARGS]\n");
        return;
    }

    // Parse options (simplified: just look for -c and filename)
    let (file_or_code, remaining) = split_first_word(args);

    if file_or_code == "-c" {
        // bash -c 'code here'
        bash::execute_code(remaining);
    } else {
        // bash script.sh
        let exit_code = bash::execute_script(file_or_code);
        set_exit_code(exit_code);
    }
}

fn builtin_plum_info() {
    puts("\n");
    framebuffer::draw_string("plum", FG_OK, BG);
    puts(" — command shell for ospab.os\n");
    puts("Version 1.0.0\n\n");

    framebuffer::draw_string("Features:\n", FG_WARN, BG);
    puts("  - Environment variables ($VAR, export)\n");
    puts("  - Command aliases (alias name=command)\n");
    puts("  - Variable expansion in commands\n");
    puts("  - Command chaining (cmd1 ; cmd2)\n");
    puts("  - Output redirection (>, >>)\n");
    puts("  - Startup config: /etc/plum/plumrc\n\n");

    framebuffer::draw_string("Builtins:\n", FG_WARN, BG);
    puts("  export    Set/show environment variables\n");
    puts("  set       Show all variables\n");
    puts("  unset     Remove a variable\n");
    puts("  alias     Define/show command aliases\n");
    puts("  unalias   Remove an alias\n");
    puts("  env       Print all environment variables\n");
    puts("  source    Execute commands from file\n");
    puts("  type      Show command type\n\n");

    dim("Shell config: /etc/plum/plumrc\n");
    dim("Edit with: grape /etc/plum/plumrc\n\n");
}

// ─── Source (execute) a script file ─────────────────────────────────────────

fn source_file(path: &str) {
    if let Some(data) = crate::fs::read_file(path) {
        if let Ok(text) = core::str::from_utf8(&data) {
            for line in text.lines() {
                let line = line.trim();
                // Skip comments and empty lines
                if line.is_empty() || line.starts_with('#') { continue; }
                // Process the line through plum
                preprocess(line);
            }
        }
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
