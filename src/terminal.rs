/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Linux-like terminal for AETERNA microkernel.
Features:
  - Clean prompt: root@ospab:~#
  - Command history with Up/Down arrows (16 entries)
  - Proper backspace (never deletes prompt)
  - Ctrl+C: cancel input, Ctrl+L: clear screen, Ctrl+D: EOF
  - White text on black background (colors only for errors/warnings/highlights)
  - Real system commands with live data
*/
use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};
use ospab_os::arch::x86_64::framebuffer;
use ospab_os::arch::x86_64::keyboard;
use ospab_os::klog;
use ospab_os::{fs, acpi, axon};

// Builtin command list for Tab completion (includes AXON)
const BUILTINS: &[&str] = &[
    "help", "echo", "clear", "ver", "version", "uname", "ls", "pwd", "cd",
    "cat", "mkdir", "touch", "rm", "save", "write", "whoami", "hostname",
    "date", "about", "meminfo", "free", "uptime", "dmesg", "lsmem",
    "lspci", "lsblk", "fdisk", "mkfs", "mount", "ping", "ifconfig", "ip", "ntpdate", "netdiag", "soundtest", "sync",
    "dump_disk", "reboot", "shutdown", "poweroff", "halt", "install", "history",
    "tutor", "grape", "tomato", "seed", "bash", "doom", "aai", "export", "alias",
    "unalias", "env", "set", "unset", "type", "source", "plum",
    "ps", "top",
];

extern crate alloc;

// ─── Global Ctrl+C flag ────────────────────────────────────────────────────
// Set when Ctrl+C is pressed during a running command.
// Long-running commands must check this and break out.
static CTRL_C: AtomicBool = AtomicBool::new(false);

/// Call from any running command to check if the user pressed Ctrl+C.
/// Returns true and prints "^C" if so.
fn check_ctrl_c() -> bool {
    // Non-blocking drain: consume only Ctrl+C, push others back
    while let Some(ch) = keyboard::try_read_key() {
        if ch == '\x03' {
            CTRL_C.store(true, Ordering::Relaxed);
            break;
        }
        // Not Ctrl+C — ignore (key is consumed but that's acceptable
        // in a running command context where input goes to the command)
    }
    CTRL_C.load(Ordering::Relaxed)
}

// ─── Colors — minimal palette (white on black, colors only where needed) ───
const FG: u32         = 0x00FFFFFF;   // Default: white
const FG_DIM: u32     = 0x00AAAAAA;   // Dim gray for secondary info
const FG_ERR: u32     = 0x000000FF;   // Red — errors only
const FG_WARN: u32    = 0x0000FFFF;   // Yellow — warnings only
const FG_OK: u32      = 0x0000FF00;   // Green — success indicators
const FG_PROMPT: u32  = 0x0000FF00;   // Green — prompt user@host
const FG_PATH: u32    = 0x00FF8844;   // Blue-ish — prompt path (BGR)
const FG_DIR: u32     = 0x00FF8844;   // Blue — directory names in ls
const BG: u32         = 0x00000000;   // Black background

// ─── Prompt ───
const PROMPT_USER: &str = "root@ospab";
const PROMPT_SEP: &str  = ":";
const PROMPT_PATH: &str = "~";
const PROMPT_HASH: &str = "# ";

// ─── Input buffer ───
const INPUT_BUFFER_SIZE: usize = 256;
static mut INPUT_BUFFER: [u8; INPUT_BUFFER_SIZE] = [0; INPUT_BUFFER_SIZE];
static mut INPUT_LEN: usize = 0;
static mut INPUT_CURSOR: usize = 0;

// ─── Command history ───
const HISTORY_SIZE: usize = 16;
static mut HISTORY: [[u8; INPUT_BUFFER_SIZE]; HISTORY_SIZE] = [[0; INPUT_BUFFER_SIZE]; HISTORY_SIZE];
static mut HISTORY_LENS: [usize; HISTORY_SIZE] = [0; HISTORY_SIZE];
static mut HISTORY_COUNT: usize = 0;
static mut HISTORY_POS: usize = 0;
static mut HISTORY_BROWSING: bool = false;

// ─── Prompt position tracking ───
static mut PROMPT_END_X: u64 = 0;
static mut PROMPT_END_Y: u64 = 0;

// ─── Current working directory ───
static mut CWD: [u8; 64] = [0; 64];
static mut CWD_LEN: usize = 0;

fn init_cwd() {
    unsafe {
        CWD[0] = b'/';
        CWD_LEN = 1;
    }
}

fn cwd_str() -> &'static str {
    ospab_os::plum::cwd()
}

/// Resolve a user-supplied path to an absolute path.
/// Delegates to plum shell's path resolution (uses PWD env).
fn resolve_path(path: &str) -> alloc::string::String {
    ospab_os::plum::resolve_path(path)
}

/// Return a slice of the current input up to cursor as &str
fn current_input_prefix() -> &'static str {
    unsafe { core::str::from_utf8_unchecked(&INPUT_BUFFER[..INPUT_CURSOR]) }
}

/// Attempt to complete the token before the cursor. Returns number of bytes inserted.
fn try_complete() -> Option<usize> {
    // Find token bounds (last space/tab)
    let prefix = current_input_prefix();
    let token_start = prefix.rfind(' ').map(|p| p + 1).unwrap_or(0);
    let token = &prefix[token_start..];

    // If empty token, nothing to do
    if token.is_empty() { return Some(0); }

    // If token contains '/', complete path
    let (dir, base) = if let Some(pos) = token.rfind('/') {
        let dir = &token[..pos];
        let base = &token[pos + 1..];
        let mut abs = resolve_path(dir);
        if abs.is_empty() { abs.push('/'); }
        (abs, base)
    } else {
        (alloc::string::String::from(cwd_str()), token)
    };

    // Collect matches from filesystem
    let mut matches: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();
    if let Some(entries) = fs::readdir(&dir) {
        for e in entries {
            if e.name.starts_with(base) {
                matches.push(e.name);
            }
        }
    }

    // If token has no slash and is first token, also complete builtins and axon commands
    let is_first_token = token_start == 0;
    if is_first_token && !token.contains('/') {
        for &b in BUILTINS {
            if b.starts_with(token) { matches.push(alloc::string::String::from(b)); }
        }
        for &a in axon::command_list() {
            if a.starts_with(token) { matches.push(alloc::string::String::from(a)); }
        }
    }

    // Deduplicate
    matches.sort();
    matches.dedup();

    if matches.is_empty() {
        return Some(0);
    }

    // Unique completion: insert remainder
    if matches.len() == 1 {
        let m = &matches[0];
        if m.len() >= base.len() {
            let suffix = &m[base.len()..];
            unsafe {
                let need = suffix.len();
                if INPUT_LEN + need >= INPUT_BUFFER_SIZE { return Some(0); }
                for i in (INPUT_CURSOR..INPUT_LEN).rev() {
                    INPUT_BUFFER[i + need] = INPUT_BUFFER[i];
                }
                for (k, b) in suffix.as_bytes().iter().enumerate() {
                    INPUT_BUFFER[INPUT_CURSOR + k] = *b;
                }
                INPUT_CURSOR += need;
                INPUT_LEN += need;
            }
            return Some(suffix.len());
        }
        return Some(0);
    }

    // Multiple matches: show list and redraw line
    puts("\n");
    for m in &matches {
        puts(m);
        puts("  ");
    }
    puts("\n");
    // Redraw prompt + input line
    redraw_line();
    Some(0)
}

/// Main terminal loop — never returns
pub fn run() -> ! {
    keyboard::init();
    init_cwd();

    framebuffer::clear(BG);
    framebuffer::set_cursor_pos(0, 0);

    // Minimal welcome — white text
    puts("AETERNA Microkernel ");
    puts(crate::version::VERSION_STR);
    puts(" (");
    puts(crate::version::ARCH);
    puts(")\n");
    puts("Type 'help' for available commands.\n\n");

    klog::record(klog::EventSource::Terminal, "Terminal started");

    loop {
        draw_prompt();
        let cmd = read_line();
        if !cmd.is_empty() {
            history_push(cmd);
            execute_command(cmd);
        }
    }
}

// ══════════════════════════════════════════════════════════════
// Prompt
// ══════════════════════════════════════════════════════════════

fn draw_prompt() {
    framebuffer::draw_string(PROMPT_USER, FG_PROMPT, BG);
    framebuffer::draw_string(PROMPT_SEP, FG, BG);
    // Show live PWD from plum shell
    let pwd = ospab_os::plum::cwd();
    if pwd == "/" {
        framebuffer::draw_string("~", FG_PATH, BG);
    } else {
        framebuffer::draw_string(pwd, FG_PATH, BG);
    }
    framebuffer::draw_string(PROMPT_HASH, FG, BG);
    unsafe {
        let (x, y) = framebuffer::cursor_pos();
        PROMPT_END_X = x;
        PROMPT_END_Y = y;
    }
}

// ══════════════════════════════════════════════════════════════
// Command history
// ══════════════════════════════════════════════════════════════

fn history_push(cmd: &str) {
    let bytes = cmd.as_bytes();
    if bytes.is_empty() { return; }
    unsafe {
        let slot = HISTORY_COUNT % HISTORY_SIZE;
        let len = bytes.len().min(INPUT_BUFFER_SIZE);
        HISTORY[slot][..len].copy_from_slice(&bytes[..len]);
        HISTORY_LENS[slot] = len;
        HISTORY_COUNT += 1;
        HISTORY_POS = HISTORY_COUNT;
        HISTORY_BROWSING = false;
    }
}

fn history_up() -> Option<&'static str> {
    unsafe {
        if HISTORY_COUNT == 0 { return None; }
        if !HISTORY_BROWSING {
            HISTORY_POS = HISTORY_COUNT;
            HISTORY_BROWSING = true;
        }
        if HISTORY_POS == 0 { return None; }
        let oldest = if HISTORY_COUNT > HISTORY_SIZE { HISTORY_COUNT - HISTORY_SIZE } else { 0 };
        if HISTORY_POS <= oldest { return None; }
        HISTORY_POS -= 1;
        let slot = HISTORY_POS % HISTORY_SIZE;
        let len = HISTORY_LENS[slot];
        Some(core::str::from_utf8_unchecked(&HISTORY[slot][..len]))
    }
}

fn history_down() -> Option<&'static str> {
    unsafe {
        if !HISTORY_BROWSING { return None; }
        if HISTORY_POS >= HISTORY_COUNT { return None; }
        HISTORY_POS += 1;
        if HISTORY_POS >= HISTORY_COUNT {
            HISTORY_BROWSING = false;
            return Some("");
        }
        let slot = HISTORY_POS % HISTORY_SIZE;
        let len = HISTORY_LENS[slot];
        Some(core::str::from_utf8_unchecked(&HISTORY[slot][..len]))
    }
}

fn replace_input_line(new_text: &str) {
    unsafe {
        // Clear old text on screen
        let (sx, sy) = input_screen_pos(0);
        framebuffer::set_cursor_pos(sx, sy);
        for _ in 0..INPUT_LEN {
            framebuffer::draw_char(' ', FG, BG);
        }
        framebuffer::draw_char(' ', FG, BG); // extra for old cursor block
        // Write new text
        framebuffer::set_cursor_pos(PROMPT_END_X, PROMPT_END_Y);
        let bytes = new_text.as_bytes();
        let len = bytes.len().min(INPUT_BUFFER_SIZE - 1);
        INPUT_BUFFER[..len].copy_from_slice(&bytes[..len]);
        INPUT_LEN = len;
        INPUT_CURSOR = len;
        for i in 0..len {
            framebuffer::draw_char(bytes[i] as char, FG, BG);
        }
    }
}

// ══════════════════════════════════════════════════════════════
// Cursor position helpers
// ══════════════════════════════════════════════════════════════

/// Compute the screen pixel position for a given buffer index.
fn input_screen_pos(idx: usize) -> (u64, u64) {
    unsafe {
        let cw = framebuffer::CHAR_WIDTH;
        let ch = framebuffer::CHAR_HEIGHT;
        let cols = framebuffer::screen_cols();
        let prompt_col = PROMPT_END_X / cw;
        let total_col = prompt_col + idx as u64;
        let row_offset = total_col / cols;
        let col = total_col % cols;
        (col * cw, PROMPT_END_Y + row_offset * ch)
    }
}

/// Redraw the input buffer from position `from` to end, clear trailing char,
/// then reposition framebuffer cursor to INPUT_CURSOR.
fn redraw_input_from(from: usize) {
    unsafe {
        let (x, y) = input_screen_pos(from);
        framebuffer::set_cursor_pos(x, y);
        for i in from..INPUT_LEN {
            framebuffer::draw_char(INPUT_BUFFER[i] as char, FG, BG);
        }
        // Clear one trailing char (covers deletions)
        framebuffer::draw_char(' ', FG, BG);
        // Reposition to cursor
        let (cx, cy) = input_screen_pos(INPUT_CURSOR);
        framebuffer::set_cursor_pos(cx, cy);
    }
}

/// Redraw prompt and the whole input line (used after listing completions)
fn redraw_line() {
    // Move to new line, redraw prompt and buffer
    framebuffer::draw_char('\n', FG, BG);
    draw_prompt();
    unsafe {
        for i in 0..INPUT_LEN {
            framebuffer::draw_char(INPUT_BUFFER[i] as char, FG, BG);
        }
        let (cx, cy) = input_screen_pos(INPUT_CURSOR);
        framebuffer::set_cursor_pos(cx, cy);
    }
}

/// Draw a visible block cursor (inverted colors) at current INPUT_CURSOR.
fn draw_input_cursor() {
    unsafe {
        let (x, y) = input_screen_pos(INPUT_CURSOR);
        if INPUT_CURSOR < INPUT_LEN {
            framebuffer::draw_char_at(x, y, INPUT_BUFFER[INPUT_CURSOR] as char, BG, FG);
        } else {
            framebuffer::draw_char_at(x, y, ' ', BG, FG);
        }
    }
}

/// Erase the block cursor (restore normal rendering).
fn erase_input_cursor() {
    unsafe {
        let (x, y) = input_screen_pos(INPUT_CURSOR);
        if INPUT_CURSOR < INPUT_LEN {
            framebuffer::draw_char_at(x, y, INPUT_BUFFER[INPUT_CURSOR] as char, FG, BG);
        } else {
            framebuffer::draw_char_at(x, y, ' ', FG, BG);
        }
    }
}

// ══════════════════════════════════════════════════════════════
// Input line editor
// ══════════════════════════════════════════════════════════════

fn read_line() -> &'static str {
    unsafe {
        INPUT_LEN = 0;
        INPUT_CURSOR = 0;
        HISTORY_BROWSING = false;

        draw_input_cursor();

        loop {
            let c = keyboard::poll_key();

            if let Some(ch) = c {
                erase_input_cursor();

                match ch {
                    '\n' => {
                        // Move fb cursor past end of text before newline
                        let (ex, ey) = input_screen_pos(INPUT_LEN);
                        framebuffer::set_cursor_pos(ex, ey);
                        framebuffer::draw_char('\n', FG, BG);
                        INPUT_BUFFER[INPUT_LEN] = 0;
                        return core::str::from_utf8_unchecked(&INPUT_BUFFER[..INPUT_LEN]);
                    }

                    '\x08' => {
                        // Backspace: delete char before cursor
                        if INPUT_CURSOR > 0 {
                            for i in INPUT_CURSOR..INPUT_LEN {
                                INPUT_BUFFER[i - 1] = INPUT_BUFFER[i];
                            }
                            INPUT_CURSOR -= 1;
                            INPUT_LEN -= 1;
                            redraw_input_from(INPUT_CURSOR);
                        }
                    }

                    k if k == keyboard::KEY_DELETE => {
                        // Delete: remove char at cursor
                        if INPUT_CURSOR < INPUT_LEN {
                            for i in INPUT_CURSOR..INPUT_LEN - 1 {
                                INPUT_BUFFER[i] = INPUT_BUFFER[i + 1];
                            }
                            INPUT_LEN -= 1;
                            redraw_input_from(INPUT_CURSOR);
                        }
                    }

                    k if k == keyboard::KEY_LEFT => {
                        if INPUT_CURSOR > 0 {
                            INPUT_CURSOR -= 1;
                            let (x, y) = input_screen_pos(INPUT_CURSOR);
                            framebuffer::set_cursor_pos(x, y);
                        }
                    }

                    k if k == keyboard::KEY_RIGHT => {
                        if INPUT_CURSOR < INPUT_LEN {
                            INPUT_CURSOR += 1;
                            let (x, y) = input_screen_pos(INPUT_CURSOR);
                            framebuffer::set_cursor_pos(x, y);
                        }
                    }

                    k if k == keyboard::KEY_HOME => {
                        INPUT_CURSOR = 0;
                        framebuffer::set_cursor_pos(PROMPT_END_X, PROMPT_END_Y);
                    }

                    k if k == keyboard::KEY_END => {
                        INPUT_CURSOR = INPUT_LEN;
                        let (x, y) = input_screen_pos(INPUT_LEN);
                        framebuffer::set_cursor_pos(x, y);
                    }

                    k if k == keyboard::KEY_UP => {
                        if let Some(text) = history_up() {
                            replace_input_line(text);
                        }
                    }

                    k if k == keyboard::KEY_DOWN => {
                        if let Some(text) = history_down() {
                            replace_input_line(text);
                        }
                    }

                    '\x03' => {
                        // Ctrl+C: cancel input
                        let (ex, ey) = input_screen_pos(INPUT_LEN);
                        framebuffer::set_cursor_pos(ex, ey);
                        framebuffer::draw_string("^C", FG_DIM, BG);
                        framebuffer::draw_char('\n', FG, BG);
                        INPUT_LEN = 0;
                        INPUT_CURSOR = 0;
                        CTRL_C.store(false, Ordering::Relaxed);
                        return core::str::from_utf8_unchecked(&INPUT_BUFFER[..0]);
                    }

                    '\x0C' => {
                        // Ctrl+L: clear screen
                        framebuffer::clear(BG);
                        framebuffer::set_cursor_pos(0, 0);
                        INPUT_LEN = 0;
                        INPUT_CURSOR = 0;
                        return core::str::from_utf8_unchecked(&INPUT_BUFFER[..0]);
                    }

                    '\t' => {
                        // Tab: completion for commands and filesystem entries
                        if let Some(inserted) = try_complete() {
                            if inserted > 0 {
                                redraw_input_from(INPUT_CURSOR - inserted);
                            }
                        }
                    }

                    '\x1B' | '\x04' => {}
                    k if k == keyboard::KEY_PGUP || k == keyboard::KEY_PGDN => {}

                    c if c.is_ascii() && (c as u8) >= 0x20 => {
                        if INPUT_LEN < INPUT_BUFFER_SIZE - 1 {
                            // Insert character at cursor position
                            for i in (INPUT_CURSOR..INPUT_LEN).rev() {
                                INPUT_BUFFER[i + 1] = INPUT_BUFFER[i];
                            }
                            INPUT_BUFFER[INPUT_CURSOR] = c as u8;
                            INPUT_CURSOR += 1;
                            INPUT_LEN += 1;

                            if INPUT_CURSOR == INPUT_LEN {
                                // Appending at end — just draw the char
                                framebuffer::draw_char(c, FG, BG);
                            } else {
                                // Inserted in middle — redraw from insert point
                                redraw_input_from(INPUT_CURSOR - 1);
                            }
                        }
                    }

                    _ => {}
                }

                draw_input_cursor();
            } else {
                // No key — yield CPU, then check if a deferred write-back is due.
                unsafe { core::arch::asm!("hlt"); }
                ospab_os::fs::disk_sync::deferred_tick();
            }
        }
    }
}

// ══════════════════════════════════════════════════════════════
// Command dispatch
// ══════════════════════════════════════════════════════════════

fn execute_command(cmd: &str) {
    let cmd = cmd.trim();
    if cmd.is_empty() { return; }

    // Reset Ctrl+C before running any command so previous ^C doesn’t bleed
    CTRL_C.store(false, Ordering::Relaxed);

    klog::record(klog::EventSource::Terminal, cmd);

    let (command, args) = match cmd.find(' ') {
        Some(pos) => (&cmd[..pos], cmd[pos + 1..].trim()),
        None => (cmd, ""),
    };

    match command {
        "help"    => cmd_help(args),
        "echo"    => cmd_echo(args),
        "clear"   => cmd_clear(),
        "ver" | "version" => cmd_version(),
        "uname"   => cmd_uname(args),
        "ls"      => { ospab_os::plum::preprocess(&alloc::format!("ls {}", args)); }
        "pwd"     => { ospab_os::plum::preprocess("pwd"); }
        "cd"      => { ospab_os::plum::preprocess(&alloc::format!("cd {}", args)); }
        "cat"     => { ospab_os::plum::preprocess(&alloc::format!("cat {}", args)); }
        "mkdir"   => { ospab_os::plum::preprocess(&alloc::format!("mkdir {}", args)); }
        "touch"   => { ospab_os::plum::preprocess(&alloc::format!("touch {}", args)); }
        "rm"      => { ospab_os::plum::preprocess(&alloc::format!("rm {}", args)); }
        "save"    => { ospab_os::plum::preprocess("save"); }
        "write"   => { ospab_os::plum::preprocess(&alloc::format!("write {}", args)); }
        "whoami"  => cmd_whoami(),
        "hostname"=> cmd_hostname(),
        "date"    => cmd_date(),
        "about"   => cmd_about(),
        "meminfo" | "free" => cmd_meminfo(),
        "uptime"  => cmd_uptime(),
        "dmesg"   => cmd_dmesg(),
        "lsmem"   => cmd_lsmem(),
        "lspci"   => cmd_lspci(),
        "lsblk"   => ospab_os::axon::disk_tools::run_lsblk(args),
        "fdisk"   => ospab_os::axon::disk_tools::run_fdisk(args),
        "mkfs"    => ospab_os::axon::disk_tools::run_mkfs(args),
        "mount"   => ospab_os::axon::disk_tools::run_mount(args),
        "ping"    => cmd_ping(args),
        "ifconfig" | "ip" => cmd_ifconfig(),
        "netdiag"  => {
            puts("Running network diagnostics (output to serial COM1)...\n");
            ospab_os::net::diag::run_full_diagnostic();
            puts("Done. Check serial output for full report.\n");
        }
        "soundtest" => cmd_soundtest(),
        "ntpdate" => cmd_ntpdate(args),
        "sync"    => cmd_sync(),
        "dump_disk" => cmd_dump_disk(args),
        "reboot"  => cmd_reboot(),
        "shutdown" | "poweroff" | "halt" => cmd_shutdown(),
        "install" => cmd_install(),
        "history" => cmd_history(),
        "tutor"   => cmd_tutor(args),
        "grape"   => cmd_grape(args),
        "tomato"  => cmd_tomato(args),
        "seed"    => cmd_seed(args),
        "bash"    => cmd_bash(args),
        "doom"    => cmd_doom(args),
        "aai"     => cmd_aai(args),
        "export"  => { ospab_os::plum::preprocess(&alloc::format!("export {}", args)); }
        "alias"   => { ospab_os::plum::preprocess(&alloc::format!("alias {}", args)); }
        "unalias" => { ospab_os::plum::preprocess(&alloc::format!("unalias {}", args)); }
        "env"     => { ospab_os::plum::preprocess("env"); }
        "set"     => { ospab_os::plum::preprocess(&alloc::format!("set {}", args)); }
        "unset"   => { ospab_os::plum::preprocess(&alloc::format!("unset {}", args)); }
        "type"    => { ospab_os::plum::preprocess(&alloc::format!("type {}", args)); }
        "source"  => { ospab_os::plum::preprocess(&alloc::format!("source {}", args)); }
        "plum"    => { ospab_os::plum::preprocess("plum"); }
        _ => {
            // AXON coreutils
            if axon::dispatch(command, args) {
                return;
            }
            // Try plum shell preprocessing (alias/variable expansion).
            // preprocess() returns None if it handled the command itself
            // (plum builtin, alias expanding to a plum builtin, etc.).
            // It returns Some(expanded) if nobody recognised the command.
            let full_cmd = if args.is_empty() {
                alloc::string::String::from(command)
            } else {
                alloc::format!("{} {}", command, args)
            };
            if ospab_os::plum::preprocess(&full_cmd).is_some() {
                // Nothing handled it — show "command not found"
                err_print(command);
                err_print(": command not found\n");
                dim_print("  Type 'help' for available commands.\n");
            }
            // else: plum handled it internally (alias, builtin, etc.)
        }
    }
}

// ══════════════════════════════════════════════════════════════
// Commands
// ══════════════════════════════════════════════════════════════

fn cmd_help(args: &str) {
    if !args.is_empty() {
        // Detailed help for a specific command
        match args {
            "ping" => {
                puts("ping <ip> [count]\n");
                dim_print("  Send ICMP echo requests to a host.\n");
                dim_print("  Default: 4 pings. Requires RTL8139 NIC.\n");
                dim_print("  Example: ping 10.0.2.2\n");
                dim_print("           ping 8.8.8.8 8\n");
                return;
            }
            "ntpdate" => {
                puts("ntpdate [server-ip]\n");
                dim_print("  Synchronize system clock via SNTP.\n");
                dim_print("  Default server: 10.0.2.2 (QEMU gateway).\n");
                dim_print("  Example: ntpdate\n");
                dim_print("           ntpdate 10.0.2.2\n");
                return;
            }
            "ifconfig" | "ip" => {
                puts("ifconfig\n");
                dim_print("  Show network interface configuration.\n");
                dim_print("  Displays IP, MAC, gateway, netmask.\n");
                return;
            }
            "tutor" => {
                puts("tutor [topic]\n");
                dim_print("  Interactive system tutorial.\n");
                dim_print("  Topics: intro, fs, net, mem, kernel, commands\n");
                dim_print("  Example: tutor intro\n");
                return;
            }
            "grape" => {
                puts("grape [file]\n");
                dim_print("  Open text editor (nano-like interface).\n");
                dim_print("  Ctrl+X: exit, Ctrl+O: save, Ctrl+K: cut line\n");
                dim_print("  Ctrl+W: search, Ctrl+G: help screen\n");
                dim_print("  Example: grape /etc/hostname\n");
                dim_print("           grape /tmp/notes.txt\n");
                return;
            }
            "tomato" => {
                puts("tomato <operation> [target]\n");
                dim_print("  Package manager (pacman-like).\n");
                dim_print("  -S <pkg>   Install package\n");
                dim_print("  -R <pkg>   Remove package\n");
                dim_print("  -Q         List installed packages\n");
                dim_print("  -Ss <q>    Search packages\n");
                dim_print("  -Syu       Full system upgrade\n");
                dim_print("  Example: tomato -S base\n");
                return;
            }
            "seed" => {
                puts("seed [command] [service]\n");
                dim_print("  Init system and service manager.\n");
                dim_print("  status           Show all services\n");
                dim_print("  start <svc>      Start a service\n");
                dim_print("  stop <svc>       Stop a service\n");
                dim_print("  restart <svc>    Restart a service\n");
                dim_print("  log              Show boot log\n");
                dim_print("  Example: seed status\n");
                return;
            }
            "doom" => {
                puts("doom\n");
                dim_print("  Run classic DOOM (shareware v1.9).\n");
                dim_print("  The legendary 1993 FPS on bare metal!\n");
                dim_print("  Controls: Arrow keys or WASD to move,\n");
                dim_print("            Ctrl for strafe, Space to use.\n");
                dim_print("  F1=help, F2=save, F3=load, ESC=quit.\n");
                return;
            }
            "soundtest" => {
                puts("soundtest\n");
                dim_print("  Audio driver diagnostics + test tone.\n");
                dim_print("  Reports active driver (AC97 / HDA / none),\n");
                dim_print("  DMA ring state, IRQ line, and all AC97\n");
                dim_print("  hardware registers to serial COM1.\n");
                dim_print("  Then plays a 440 Hz sine tone (0.5 s)\n");
                dim_print("  through all available audio outputs.\n");
                dim_print("  Use: soundtest\n");
                return;
            }
            "aai" => {
                puts("aai <subcommand> [args]\n");
                dim_print("  Aeterna AI utility — powered by ANE (Aeterna Neural Engine).\n\n");
                dim_print("  Subcommands:\n");
                dim_print("    aai load <path>   Load a .tmt-ai model from the VFS\n");
                dim_print("    aai info          Show loaded model metadata\n");
                dim_print("    aai bench         GEMM benchmark (SIMD perf report)\n");
                dim_print("    aai chat <prompt> Interactive inference with KV cache\n");
                dim_print("    aai summarize <text> Entropy + stats analysis of text\n\n");
                dim_print("  .tmt-ai format: TMT\x01 magic + JSON metadata + f32 weights\n");
                dim_print("  Weights are zero-copy mapped via Huge Pages (Mmap syscall).\n");
                dim_print("  SIMD auto-dispatched: AVX-512 → AVX2+FMA → scalar.\n\n");
                dim_print("  Example:\n");
                dim_print("    aai load /models/tiny.tmt-ai\n");
                dim_print("    aai info\n");
                dim_print("    aai chat Hello, world\n");
                dim_print("  See: tutor ai\n");
                return;
            }
            "cat" => {
                puts("cat <path>\n");
                dim_print("  Display file contents. Virtual files:\n");
                dim_print("  /proc/version  /proc/uptime  /proc/meminfo\n");
                dim_print("  /proc/cpuinfo  /etc/hostname  /etc/os-release\n");
                return;
            }
            "uname" => {
                puts("uname [-a | -r | -m]\n");
                dim_print("  -a  Full system info\n");
                dim_print("  -r  Kernel version\n");
                dim_print("  -m  Architecture\n");
                return;
            }
            _ => {
                dim_print("No detailed help for '");
                dim_print(args);
                dim_print("'.\n");
                return;
            }
        }
    }

    // Build help content as lines
    let mut lines = alloc::vec::Vec::new();
    lines.push(("header", "  AETERNA Shell Guide"));
    lines.push(("normal", "  ("));
    lines.push(("normal", crate::version::VERSION_STR));
    lines.push(("normal", ")"));
    lines.push(("normal", "  =========================================="));
    lines.push(("normal", ""));
    lines.push(("section", "  NAVIGATION & SHELL"));
    lines.push(("cmd", "  help       This guide. 'help <cmd>' for details"));
    lines.push(("cmd", "  help ping  Example: detailed ping help"));
    lines.push(("cmd", "  tutor      Interactive system tutorial"));
    lines.push(("cmd", "  history    Show command history"));
    lines.push(("cmd", "  clear      Clear screen (also Ctrl+L)"));
    lines.push(("dim", "    Tip: Up/Down arrows browse history, Ctrl+C cancels"));
    lines.push(("normal", ""));
    lines.push(("section", "  FILE SYSTEM"));
    lines.push(("cmd", "  ls         List directory contents"));
    lines.push(("cmd", "  cd         Change directory (cd /proc)"));
    lines.push(("cmd", "  pwd        Print working directory"));
    lines.push(("cmd", "  cat        Read file (cat /proc/meminfo)"));
    lines.push(("cmd", "  mkdir      Create directory (mkdir /tmp/test)"));
    lines.push(("cmd", "  touch      Create empty file (touch /tmp/hello)"));
    lines.push(("cmd", "  rm         Remove file (rm /tmp/hello)"));
    lines.push(("cmd", "  write      Write text to file (write /path text)"));
    lines.push(("cmd", "  echo       Print text, or echo text > file"));
    lines.push(("normal", ""));
    lines.push(("section", "  SYSTEM INFO"));
    lines.push(("cmd", "  version    Kernel and OS version"));
    lines.push(("cmd", "  uname      System info (-a for full)"));
    lines.push(("cmd", "  about      About AETERNA kernel"));
    lines.push(("cmd", "  whoami     Current user"));
    lines.push(("cmd", "  hostname   System hostname"));
    lines.push(("cmd", "  date       Date and uptime"));
    lines.push(("cmd", "  uptime     System uptime counter"));
    lines.push(("normal", ""));
    lines.push(("section", "  HARDWARE & MEMORY"));
    lines.push(("cmd", "  free       Memory usage (physical + heap)"));
    lines.push(("cmd", "  lsmem      Memory region details"));
    lines.push(("cmd", "  lspci      PCI device listing"));
    lines.push(("cmd", "  lsblk      List block storage devices"));
    lines.push(("cmd", "  fdisk      Show disk/partition info"));
    lines.push(("cmd", "  dmesg      Kernel event log"));
    lines.push(("normal", ""));
    lines.push(("section", "  NETWORKING"));
    lines.push(("cmd", "  ifconfig   Network interface status"));
    lines.push(("cmd", "  ping       ICMP ping (ping 10.0.2.2)"));
    lines.push(("cmd", "  netdiag    Full NIC/stack diagnostics → serial"));
    lines.push(("cmd", "  ntpdate    NTP time sync"));
    lines.push(("normal", ""));
    lines.push(("section", "  AUDIO"));
    lines.push(("cmd", "  soundtest  Audio driver diagnostics + 440 Hz tone"));
    lines.push(("normal", ""));
    lines.push(("section", "  SYSTEM CONTROL"));
    lines.push(("cmd", "  save       Save filesystem to disk (plum)"));
    lines.push(("cmd", "  sync       Flush VFS to disk"));
    lines.push(("cmd", "  dump_disk  Hex dump of sector 2048"));
    lines.push(("cmd", "  install    Launch system installer"));
    lines.push(("cmd", "  reboot     Reboot system"));
    lines.push(("cmd", "  shutdown   Shutdown (also poweroff, halt)"));
    lines.push(("normal", ""));
    lines.push(("section", "  USERLAND TOOLS"));
    lines.push(("cmd", "  grape      Text editor (nano-like). grape <file>"));
    lines.push(("cmd", "  tomato     Package manager. tomato --help"));
    lines.push(("cmd", "  seed       Init system / services. seed status"));
    lines.push(("cmd", "  doom       Classic DOOM (shareware v1.9)"));
    lines.push(("cmd", "  plum       Shell info. Also: export, alias, env"));
    lines.push(("normal", ""));
    lines.push(("section", "  AI — AETERNA NEURAL ENGINE (ANE)"));
    lines.push(("cmd", "  aai load <path>   Load .tmt-ai model (zero-copy)"));
    lines.push(("cmd", "  aai info          Model metadata + param count"));
    lines.push(("cmd", "  aai bench         SIMD GEMM benchmark"));
    lines.push(("cmd", "  aai chat <text>   Inference with 100 Hz streaming"));
    lines.push(("cmd", "  aai summarize <text> Entropy + text stats"));
    lines.push(("dim", "    Tip: help aai  or  tutor ai  for full details"));
    lines.push(("normal", ""));
    lines.push(("section", "  SHELL BUILTINS (plum)"));
    lines.push(("cmd", "  export     Set environment variable (export VAR=val)"));
    lines.push(("cmd", "  alias      Define command alias (alias name=cmd)"));
    lines.push(("cmd", "  env        Show all environment variables"));
    lines.push(("cmd", "  set        Show/set variables"));
    lines.push(("cmd", "  type       Show command type (type ls)"));
    lines.push(("cmd", "  source     Execute script file (source /path)"));
    lines.push(("cmd", "  bash       Execute bash scripts (bash script.sh)"));

    show_paged_output(&lines);
}

/// Display content with pagination (Space=next, b=back, q=quit)
fn show_paged_output(lines: &[(&str, &str)]) {
    let screen_rows = framebuffer::screen_rows() as usize;
    let lines_per_page = screen_rows.saturating_sub(3); // reserve 3 for prompt
    let total_pages = (lines.len() + lines_per_page - 1) / lines_per_page;
    let mut current_page = 0usize;

    loop {
        framebuffer::clear(BG);
        framebuffer::set_cursor_pos(0, 0);

        let start = current_page * lines_per_page;
        let end = (start + lines_per_page).min(lines.len());

        // Draw page content
        for i in start..end {
            let (typ, text) = lines[i];
            match typ {
                "header" => framebuffer::draw_string(text, FG_OK, BG),
                "section" => framebuffer::draw_string(text, FG_WARN, BG),
                "dim" => framebuffer::draw_string(text, FG_DIM, BG),
                "cmd" => framebuffer::draw_string(text, FG, BG),
                _ => framebuffer::draw_string(text, FG, BG),
            }
            puts("\n");
        }

        // Draw navigation prompt
        if total_pages > 1 {
            puts("\n");
            framebuffer::draw_string("  -- Page ", FG_DIM, BG);
            let pg_str = alloc::format!("{}/{}", current_page + 1, total_pages);
            framebuffer::draw_string(&pg_str, FG, BG);
            framebuffer::draw_string(" -- ", FG_DIM, BG);
            framebuffer::draw_string("[Space]", FG_WARN, BG);
            framebuffer::draw_string("=next ", FG_DIM, BG);
            if current_page > 0 {
                framebuffer::draw_string("[b]", FG_WARN, BG);
                framebuffer::draw_string("=back ", FG_DIM, BG);
            }
            framebuffer::draw_string("[q]", FG_WARN, BG);
            framebuffer::draw_string("=quit", FG_DIM, BG);

            // Wait for key
            loop {
                if let Some(key) = keyboard::poll_key() {
                    match key {
                        ' ' | '\n' => {
                            if current_page + 1 < total_pages {
                                current_page += 1;
                            } else {
                                return; // last page, exit on Space
                            }
                            break;
                        }
                        'b' | 'B' => {
                            if current_page > 0 {
                                current_page -= 1;
                            }
                            break;
                        }
                        'q' | 'Q' | '\x03' => {
                            framebuffer::clear(BG);
                            framebuffer::set_cursor_pos(0, 0);
                            return;
                        }
                        _ => {}
                    }
                }
            }
        } else {
            // Single page — wait for any key
            puts("\n");
            framebuffer::draw_string("  -- Press any key to continue --", FG_DIM, BG);
            keyboard::poll_key();
            framebuffer::clear(BG);
            framebuffer::set_cursor_pos(0, 0);
            return;
        }
    }
}

fn cmd_echo(args: &str) {
    // Support: echo text > file  and  echo text >> file
    if let Some(pos) = args.find(">>") {
        // Append mode
        let text = args[..pos].trim();
        let file = args[pos + 2..].trim();
        if file.is_empty() {
            err_print("echo: missing file after '>>'\n");
            return;
        }
        let abs = resolve_path(file);
        let mut data = alloc::vec::Vec::new();
        data.extend_from_slice(text.as_bytes());
        data.push(b'\n');
        if ospab_os::fs::append_file(&abs, &data) {
            // success — silent
        } else {
            err_print("echo: cannot write to '");
            err_print(file);
            err_print("'\n");
        }
        return;
    }
    if let Some(pos) = args.find('>') {
        // Write mode
        let text = args[..pos].trim();
        let file = args[pos + 1..].trim();
        if file.is_empty() {
            err_print("echo: missing file after '>'\n");
            return;
        }
        let abs = resolve_path(file);
        let mut data = alloc::vec::Vec::new();
        data.extend_from_slice(text.as_bytes());
        data.push(b'\n');
        if ospab_os::fs::write_file(&abs, &data) {
            // success — silent
        } else {
            err_print("echo: cannot write to '");
            err_print(file);
            err_print("'\n");
        }
        return;
    }
    // Normal echo
    puts(args);
    puts("\n");
}

fn cmd_clear() {
    framebuffer::clear(BG);
    framebuffer::set_cursor_pos(0, 0);
}

fn cmd_version() {
    puts(crate::version::OS_VERSION);
    puts(" (");
    puts(crate::version::KERNEL_VERSION);
    puts(")\n");
    dim_print("Build: ");
    puts(crate::version::BUILD_DATE);
    puts(" nightly\n");
    dim_print("Arch:  ");
    puts(crate::version::ARCH);
    puts("\n");
}

fn cmd_uname(args: &str) {
    if args.contains("-a") || args.contains("--all") {
        puts(crate::version::UNAME_FULL);
        puts("\n");
    } else if args.contains("-r") {
        puts(crate::version::VERSION_STR);
        puts("\n");
    } else if args.contains("-m") {
        puts(crate::version::ARCH);
        puts("\n");
    } else {
        puts("AETERNA\n");
    }
}

// VFS commands (ls, cat, touch, mkdir, rm, cd, pwd) are now handled by
// plum shell in src/userspace/plum/mod.rs — using VFS syscalls only.

fn cmd_whoami() {
    puts("root\n");
}

fn cmd_hostname() {
    // Read from VFS
    match ospab_os::fs::read_file("/etc/hostname") {
        Some(data) => {
            if let Ok(text) = core::str::from_utf8(&data) {
                puts(text.trim());
                puts("\n");
            } else {
                puts("ospab\n");
            }
        }
        None => puts("ospab\n"),
    }
}

fn cmd_date() {
    // If NTP time is synced, show real date
    if ospab_os::net::sntp::is_synced() {
        let unix_ts = ospab_os::net::sntp::unix_time();
        let mut buf = [0u8; 32];
        let len = ospab_os::net::sntp::format_datetime(unix_ts, &mut buf);
        for i in 0..len {
            framebuffer::draw_char(buf[i] as char, FG, BG);
        }
        puts("\n");
        return;
    }

    // Fallback: uptime-based
    let ticks = ospab_os::arch::x86_64::idt::timer_ticks();
    let total_secs = ticks / 100;
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    puts("Sat Mar  1 00:");
    if hours < 10 { puts("0"); }
    print_dec(hours);
    puts(":");
    if mins < 10 { puts("0"); }
    print_dec(mins);
    puts(":");
    if secs < 10 { puts("0"); }
    print_dec(secs);
    puts(" UTC 2026  (uptime: ");
    print_dec(total_secs);
    puts("s)\n");
    dim_print("  (run 'ntpdate' to sync with NTP server)\n");
}

fn cmd_about() {
    puts("\n");
    puts("    _   ___ _____ _____ ___ _  _   _\n");
    puts("   /_\\ | __|_   _| __| _ | \\| | /_\\\n");
    puts("  / _ \\| _|  | | | _||   | .` |/ _ \\\n");
    puts(" /_/ \\_|___| |_| |___|_|_|_|\\_/_/ \\_\\\n");
    puts("\n");
    puts("  AETERNA Microkernel - ");
    puts(crate::version::OS_VERSION);
    puts("\n");
    dim_print("  Deterministic, capability-based, AI-native\n");
    dim_print("  Compute-First scheduler | NUMA-aware memory\n");
    dim_print("  License: BSL-1.1\n");
    puts("\n");
}

fn cmd_meminfo() {
    let stats = ospab_os::mm::physical::stats();

    puts("           total       usable      reserved\n");
    puts("Phys:  ");
    print_size_padded(stats.total_bytes, 12);
    print_size_padded(stats.usable_bytes, 12);
    print_size_padded(stats.reserved_bytes, 12);
    puts("\n");
    puts("Regions: ");
    print_dec(stats.region_count as u64);
    puts("\n");

    if ospab_os::mm::heap::is_initialized() {
        let (used, free) = ospab_os::mm::heap::stats();
        let heap_total = ospab_os::mm::heap::heap_size() as u64;
        puts("Heap:  ");
        print_size_padded(used as u64, 12);
        print_size_padded(heap_total - used as u64, 12);
        print_size_padded(heap_total, 12);
        puts("\n");
    }
}

fn cmd_uptime() {
    let ticks = ospab_os::arch::x86_64::idt::timer_ticks();
    let seconds = ticks / 100;
    let minutes = seconds / 60;
    let hours = minutes / 60;

    puts("up ");
    if hours > 0 {
        print_dec(hours);
        puts("h ");
    }
    print_dec(minutes % 60);
    puts("m ");
    print_dec(seconds % 60);
    puts("s");
    dim_print(" (");
    print_dec(ticks);
    dim_print(" ticks)\n");
}

fn cmd_dmesg() {
    puts("--- kernel event log ---\n");
    let mut buf = [klog::Event::empty_pub(); 32];
    let count = klog::last_events(&mut buf, 32);
    if count == 0 {
        dim_print("(no events recorded)\n");
        return;
    }
    for i in 0..count {
        framebuffer::draw_string("[", FG_DIM, BG);
        let label = buf[i].source.label();
        let color = match buf[i].source {
            klog::EventSource::Boot => FG_OK,
            klog::EventSource::Fault | klog::EventSource::Panic => FG_ERR,
            _ => FG_DIM,
        };
        framebuffer::draw_string(label, color, BG);
        framebuffer::draw_string("] ", FG_DIM, BG);
        puts(buf[i].message());
        puts("\n");
    }
}

fn cmd_lsmem() {
    let stats = ospab_os::mm::physical::stats();
    puts("Memory regions:\n");
    puts("  Total:    ");
    print_size(stats.total_bytes);
    puts("\n");
    puts("  Usable:   ");
    print_size(stats.usable_bytes);
    puts("\n");
    puts("  Reserved: ");
    print_size(stats.reserved_bytes);
    puts("\n");
    puts("  Regions:  ");
    print_dec(stats.region_count as u64);
    puts("\n");

    if ospab_os::mm::heap::is_initialized() {
        let (used, _free) = ospab_os::mm::heap::stats();
        puts("  Heap used: ");
        print_size(used as u64);
        puts(" / ");
        print_size(ospab_os::mm::heap::heap_size() as u64);
        puts("\n");
    }
}

fn cmd_lspci() {
    puts("PCI devices:\n");
    let mut found = 0u32;
    for bus in 0u16..8 {
        for dev in 0u16..32 {
            let vendor = pci_read_vendor(bus as u8, dev as u8, 0);
            if vendor != 0xFFFF && vendor != 0x0000 {
                let device_id = pci_read_device(bus as u8, dev as u8, 0);
                let class = pci_read_class(bus as u8, dev as u8, 0);
                let subclass = pci_read_subclass(bus as u8, dev as u8, 0);
                puts("  ");
                print_hex_byte(bus as u8);
                puts(":");
                print_hex_byte(dev as u8);
                puts(".0  ");
                print_pci_class(class, subclass);
                puts("  [");
                print_hex_u16(vendor);
                puts(":");
                print_hex_u16(device_id);
                puts("]\n");
                found += 1;
            }
        }
    }
    if found == 0 {
        dim_print("  No PCI devices found\n");
    }
}

fn cmd_lsblk() {
    let n = ospab_os::drivers::disk_count();
    if n == 0 {
        dim_print("  No block devices detected.\n");
        dim_print("  To add a disk in QEMU:\n");
        dim_print("    # Create image first (once):\n");
        dim_print("    qemu-img create -f raw disk.img 4G\n");
        dim_print("    # Then boot with:\n");
        dim_print("    -drive file=disk.img,format=raw,if=none,id=d0 -device ahci,id=ahci -device ide-hd,drive=d0,bus=ahci.0\n");
        return;
    }
    puts("NAME    TYPE   SIZE      MODEL\n");
    dim_print("────────────────────────────────────────────────────\n");
    for i in 0..n {
        if let Some(d) = ospab_os::drivers::disk_info(i) {
            let name = match d.kind {
                ospab_os::drivers::DiskKind::Ahci => {
                    let idx = ospab_os::drivers::disk_info_count_before(i, ospab_os::drivers::DiskKind::Ahci);
                    match idx { 0 => "sda  ", 1 => "sdb  ", 2 => "sdc  ", _ => "sdX  " }
                }
                ospab_os::drivers::DiskKind::Ata => {
                    let idx = ospab_os::drivers::disk_info_count_before(i, ospab_os::drivers::DiskKind::Ata);
                    match idx { 0 => "hda  ", 1 => "hdb  ", 2 => "hdc  ", _ => "hdX  " }
                }
            };
            puts("  ");
            puts(name);
            let kind_s = match d.kind {
                ospab_os::drivers::DiskKind::Ahci => " SATA  ",
                ospab_os::drivers::DiskKind::Ata  => " IDE   ",
            };
            puts(kind_s);
            let gib = d.size_mb / 1024;
            let rem = (d.size_mb % 1024) * 10 / 1024;
            print_u64(gib);
            puts(".");
            print_u64(rem);
            puts(" GiB  ");
            puts(ospab_os::drivers::model_str(d));
            puts("\n");
        }
    }
}

fn cmd_fdisk(args: &str) {
    if args.is_empty() || args == "-l" {
        let n = ospab_os::drivers::disk_count();
        if n == 0 {
            dim_print("  No disks found.\n");
            dim_print("  Tip: attach a disk with -device ahci or -device piix3-ide in QEMU.\n");
            return;
        }
        for i in 0..n {
            if let Some(d) = ospab_os::drivers::disk_info(i) {
                let name = match d.kind {
                    ospab_os::drivers::DiskKind::Ahci => "sda",
                    ospab_os::drivers::DiskKind::Ata  => "hda",
                };
                puts("Disk /dev/");
                puts(name);
                puts(": ");
                let gib = d.size_mb / 1024;
                let rem = (d.size_mb % 1024) * 10 / 1024;
                print_u64(gib);
                puts(".");
                print_u64(rem);
                puts(" GiB, ");
                print_u64(d.size_mb as u64 * 1024 * 1024);
                puts(" bytes, ");
                print_u64(d.sectors);
                puts(" sectors\n");
                puts("Disk model: ");
                puts(ospab_os::drivers::model_str(d));
                puts("\n");
                dim_print("Units: sectors of 512 bytes\n");
                dim_print("Partition table not available (raw disk)\n\n");
            }
        }
    } else {
        dim_print("Usage: fdisk -l\n");
    }
}

fn print_u64(mut n: u64) {
    if n == 0 { puts("0"); return; }
    let mut buf = [0u8; 20];
    let mut pos = 0;
    while n > 0 { buf[pos] = b'0' + (n % 10) as u8; n /= 10; pos += 1; }
    for i in (0..pos).rev() {
        framebuffer::draw_char(buf[i] as char, FG, BG);
    }
}

fn pci_config_addr(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    0x80000000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | ((offset as u32) & 0xFC)
}

fn pci_read_u32(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    let addr = pci_config_addr(bus, dev, func, offset);
    unsafe {
        let val: u32;
        asm!(
            "out dx, eax",
            in("dx") 0x0CF8u16,
            in("eax") addr,
            options(nomem, nostack)
        );
        asm!(
            "in eax, dx",
            in("dx") 0x0CFCu16,
            out("eax") val,
            options(nomem, nostack)
        );
        val
    }
}

fn pci_read_vendor(bus: u8, dev: u8, func: u8) -> u16 {
    (pci_read_u32(bus, dev, func, 0) & 0xFFFF) as u16
}

fn pci_read_device(bus: u8, dev: u8, func: u8) -> u16 {
    ((pci_read_u32(bus, dev, func, 0) >> 16) & 0xFFFF) as u16
}

fn pci_read_class(bus: u8, dev: u8, func: u8) -> u8 {
    ((pci_read_u32(bus, dev, func, 8) >> 24) & 0xFF) as u8
}

fn pci_read_subclass(bus: u8, dev: u8, func: u8) -> u8 {
    ((pci_read_u32(bus, dev, func, 8) >> 16) & 0xFF) as u8
}

fn print_pci_class(class: u8, subclass: u8) {
    match (class, subclass) {
        (0x00, _)     => puts("Unclassified        "),
        (0x01, 0x00)  => puts("SCSI Controller     "),
        (0x01, 0x01)  => puts("IDE Controller      "),
        (0x01, 0x06)  => puts("SATA Controller     "),
        (0x01, _)     => puts("Storage Controller  "),
        (0x02, 0x00)  => puts("Ethernet Controller "),
        (0x02, _)     => puts("Network Controller  "),
        (0x03, 0x00)  => puts("VGA Controller      "),
        (0x03, _)     => puts("Display Controller  "),
        (0x04, _)     => puts("Multimedia Device   "),
        (0x05, _)     => puts("Memory Controller   "),
        (0x06, 0x00)  => puts("Host Bridge         "),
        (0x06, 0x01)  => puts("ISA Bridge          "),
        (0x06, 0x04)  => puts("PCI Bridge          "),
        (0x06, _)     => puts("Bridge Device       "),
        (0x07, _)     => puts("Communication Ctrl  "),
        (0x08, _)     => puts("System Peripheral   "),
        (0x0C, 0x03)  => puts("USB Controller      "),
        (0x0C, _)     => puts("Serial Bus Ctrl     "),
        _             => puts("Unknown Device      "),
    }
}

fn cmd_sync() {
    if !ospab_os::fs::disk_sync::is_dirty() {
        dim_print("Nothing to sync — filesystem is already clean.\n");
        return;
    }
    puts("Syncing filesystem...\n");
    klog::record(klog::EventSource::Boot, "Sync requested");
    if ospab_os::fs::disk_sync::sync_filesystem() {
        dim_print("Filesystem synchronized to disk.\n");
    } else {
        err_print("Failed to sync filesystem.\n");
    }
}

fn cmd_dump_disk(_args: &str) {
    use alloc::format;
    puts("Reading sector 2048 (LBA 0x800)...\n");
    let mut buf = [0u8; 512];
    if ospab_os::drivers::read(0, 2048, 1, &mut buf) {
        // Print first 16 bytes in hex
        let mut hex = alloc::string::String::with_capacity(80);
        for i in 0..16 {
            let b = buf[i];
            let hi = b >> 4;
            let lo = b & 0x0F;
            if i > 0 { hex.push(' '); }
            hex.push(if hi < 10 { (b'0' + hi) as char } else { (b'a' + hi - 10) as char });
            hex.push(if lo < 10 { (b'0' + lo) as char } else { (b'a' + lo - 10) as char });
        }
        hex.push('\n');
        puts(&hex);
        // Also print ASCII interpretation
        let mut ascii = alloc::string::String::with_capacity(32);
        ascii.push_str("ASCII: ");
        for i in 0..16 {
            let c = buf[i];
            if c >= 0x20 && c < 0x7f {
                ascii.push(c as char);
            } else {
                ascii.push('.');
            }
        }
        ascii.push('\n');
        dim_print(&ascii);
    } else {
        err_print("Failed to read sector 2048.\n");
    }
}

// ─── soundtest — audio subsystem diagnostics + 440 Hz test tone ──────────────

fn cmd_soundtest() {
    extern crate alloc;
    use ospab_os::arch::x86_64::serial;
    use ospab_os::drivers::audio;

    // ── header ───────────────────────────────────────────────────────────────
    serial::write_str("[SOUND] === Audio Subsystem Diagnostics ===\r\n");
    puts("=== Audio Subsystem Diagnostics ===\n");

    let driver = audio::active_driver_name();
    let ready  = audio::is_ready();
    let mut nbuf = [0u8; 8];

    serial::write_str("[SOUND] Active driver : "); serial::write_str(driver); serial::write_str("\r\n");
    serial::write_str("[SOUND] Ready         : ");
    serial::write_str(if ready { "yes" } else { "no" }); serial::write_str("\r\n");

    puts("Active driver : "); puts(driver); puts("\n");
    if ready {
        framebuffer::draw_string("Status        : ready\n", FG_OK, BG);
    } else {
        framebuffer::draw_string("Status        : NOT READY\n", FG_ERR, BG);
    }

    // ── IRQ line ─────────────────────────────────────────────────────────────
    let irq = audio::irq_line();
    serial::write_str("[SOUND] IRQ           : ");
    serial::write_str(ospab_os::format_u64(&mut nbuf, irq as u64));
    serial::write_str("\r\n");
    puts("IRQ line      : "); print_dec(irq as u64); puts("\n");

    // ── AC97-specific DMA state ───────────────────────────────────────────────
    if driver == "AC97" && ready {
        let (civ, fill, in_flight) = audio::ac97::dma_status();
        serial::write_str("[SOUND] CIV=");
        serial::write_str(ospab_os::format_u64(&mut nbuf, civ as u64));
        serial::write_str("  FILL_IDX=");
        serial::write_str(ospab_os::format_u64(&mut nbuf, fill as u64));
        serial::write_str("  in_flight=");
        serial::write_str(ospab_os::format_u64(&mut nbuf, in_flight as u64));
        serial::write_str("\r\n");
    }

    // ── full register dump (serial only, driver-agnostic) ────────────────────
    puts("Full register dump → serial COM1.\n");
    audio::dump_status();
    audio::dump_mem_map();

    // ── no driver — abort ────────────────────────────────────────────────────
    if !ready {
        puts("No audio driver ready — test tone skipped.\n");
        serial::write_str("[SOUND] No driver — test tone SKIPPED\r\n");
        serial::write_str("[SOUND] === End Diagnostics ===\r\n");
        return;
    }

    // ── test tone ─────────────────────────────────────────────────────────────
    //
    // Use the driver's native sample rate (44100 Hz for AC97/HDA, 48000 Hz for ES1371).
    // 16-sample sine table: sin(i·2π/16) × 16384  (50 % amplitude, integer-only)
    let sample_rate = audio::sample_rate();

    puts("Generating 440 Hz test tone (0.5 s, ");
    print_dec(sample_rate as u64);
    puts(" Hz, stereo 16-bit)...\n");
    serial::write_str("[SOUND] Generating 440 Hz test tone @ ");
    serial::write_str(ospab_os::format_u64(&mut nbuf, sample_rate as u64));
    serial::write_str(" Hz...\r\n");

    const SIN16: [i16; 16] = [
        0, 6270, 11585, 15137, 16384, 15137, 11585, 6270,
        0, -6270, -11585, -15137, -16384, -15137, -11585, -6270,
    ];

    // Phase increment Q16.16 = 440 × 16 × 65536 / sample_rate
    let phase_inc: u32 = ((440u64 * 16u64 * 65536u64) / sample_rate as u64) as u32;

    // 0.5 seconds of frames
    let total_frames: usize = (sample_rate / 2) as usize;
    const CHUNK_FRAMES: usize = 1024;
    const CHUNK_BYTES:  usize = CHUNK_FRAMES * 4;

    let mut phase:       u32  = 0;
    let mut total_bytes: usize = 0;
    let mut remaining:   usize = total_frames;
    let mut pcm: alloc::vec::Vec<u8> = alloc::vec![0u8; CHUNK_BYTES];

    while remaining > 0 {
        let n = if remaining > CHUNK_FRAMES { CHUNK_FRAMES } else { remaining };
        for i in 0..n {
            let idx = ((phase >> 16) as usize) & 0xF;
            let s   = SIN16[idx];
            let lo  = (s as u16 & 0xFF) as u8;
            let hi  = ((s as u16) >> 8) as u8;
            pcm[i * 4    ] = lo;
            pcm[i * 4 + 1] = hi;
            pcm[i * 4 + 2] = lo;
            pcm[i * 4 + 3] = hi;
            phase = phase.wrapping_add(phase_inc);
        }
        let bytes = n * 4;
        audio::write_pcm(&pcm[..bytes]);
        total_bytes += bytes;
        remaining   -= n;
    }

    serial::write_str("[SOUND] Bytes submitted : ");
    serial::write_str(ospab_os::format_u64(&mut nbuf, total_bytes as u64));
    serial::write_str("\r\n");
    serial::write_str("[SOUND] === End Diagnostics ===\r\n");

    puts("Bytes submitted : "); print_dec(total_bytes as u64); puts("\n");
    framebuffer::draw_string("Test tone queued to audio driver.\n", FG_OK, BG);
    dim_print("Full register dump on serial COM1 (115200 baud).\n");
}

fn cmd_reboot() {
    puts("Syncing and rebooting...\n");

    // Only flush if there are pending writes — no-op otherwise.
    if ospab_os::fs::disk_sync::is_dirty() {
        ospab_os::fs::disk_sync::sync_filesystem();
    }

    puts("Rebooting...\n");
    klog::record(klog::EventSource::Boot, "Reboot requested");
    ospab_os::arch::x86_64::serial::write_str("[AETERNA] Rebooting...\r\n");
    // Use ACPI reboot; falls back internally to 0xCF9/0x64/triple fault
    acpi::reboot();
}

fn cmd_shutdown() {
    puts("System shutting down...\n");
    klog::record(klog::EventSource::Boot, "Shutdown requested");
    ospab_os::arch::x86_64::serial::write_str("[AETERNA] Shutting down...\r\n");
    // ACPI shutdown writes PM1a_CNT SLP_TYP/SLP_EN and falls back to emulator ports
    acpi::shutdown();
    dim_print("\nSystem halted. You may turn off your computer.\n");
    loop { unsafe { asm!("cli; hlt"); } }
}

fn cmd_history() {
    unsafe {
        if HISTORY_COUNT == 0 {
            dim_print("(no history)\n");
            return;
        }
        let start = if HISTORY_COUNT > HISTORY_SIZE { HISTORY_COUNT - HISTORY_SIZE } else { 0 };
        for i in start..HISTORY_COUNT {
            let slot = i % HISTORY_SIZE;
            let len = HISTORY_LENS[slot];
            puts("  ");
            print_dec((i + 1) as u64);
            puts("  ");
            puts(core::str::from_utf8_unchecked(&HISTORY[slot][..len]));
            puts("\n");
        }
    }
}

fn cmd_install() {
    crate::installer::run();
}

// ══════════════════════════════════════════════════════════════
// Network commands
// ══════════════════════════════════════════════════════════════

fn cmd_ifconfig() {
    if !ospab_os::net::is_up() {
        err_print("Network not available. Start QEMU with: -netdev user,id=n0 -device rtl8139,netdev=n0\n");
        return;
    }
    let ip = unsafe { ospab_os::net::OUR_IP };
    let gw = unsafe { ospab_os::net::GATEWAY_IP };
    let mask = unsafe { ospab_os::net::SUBNET_MASK };
    let mac = unsafe { ospab_os::net::OUR_MAC };
    let gw_mac = unsafe { ospab_os::net::GATEWAY_MAC };
    let driver = ospab_os::net::nic_name(); // "RTL8139" / "Intel e1000" / "RTL8169/8111"

    // Interface name line with driver
    puts("eth0      Link encap:Ethernet  Driver:");
    puts(driver);
    puts("\n");
    puts("          HWaddr ");
    print_mac(mac);
    puts("\n");
    puts("          inet addr:");
    print_ip(ip);
    puts("  Mask:");
    print_ip(mask);
    puts("\n");
    puts("          Gateway:");
    print_ip(gw);
    puts("  GW MAC:");
    print_mac(gw_mac);
    puts("\n");
    puts("          UP BROADCAST RUNNING MULTICAST  MTU:1500\n");
}

fn cmd_ping(args: &str) {
    if !ospab_os::net::is_up() {
        err_print("Network not available.\n");
        dim_print("  Start QEMU with one of:\n");
        dim_print("    -netdev user,id=n0 -device rtl8139,netdev=n0\n");
        dim_print("    -netdev user,id=n0 -device e1000,netdev=n0\n");
        return;
    }
    if args.is_empty() {
        err_print("Usage: ping [-c count] [-i interval] [-s size] [-W timeout] <destination>\n");
        dim_print("  Example: ping 10.0.2.2\n");
        dim_print("  Example: ping -c 5 gateway\n");
        return;
    }

    // ── Parse arguments ─────────────────────────────────────────────────────
    let mut count: Option<u32> = None;       // None = infinite (until Ctrl+C)
    let mut interval_us: u64 = 1_000_000;    // 1 second default
    let mut payload: usize = 56;             // Linux default
    let mut timeout_us: u64 = 3_000_000;     // 3 seconds default
    let mut target_str: &str = "";

    {
        let mut words = args.split_whitespace();
        while let Some(w) = words.next() {
            match w {
                "-c" => {
                    if let Some(v) = words.next() {
                        count = parse_u32(v);
                        if count == Some(0) {
                            err_print("ping: bad value for -c\n");
                            return;
                        }
                    } else {
                        err_print("ping: -c requires a value\n");
                        return;
                    }
                }
                "-i" => {
                    if let Some(v) = words.next() {
                        interval_us = match parse_seconds_us(v) {
                            Some(us) if us > 0 => us,
                            _ => { err_print("ping: bad value for -i\n"); return; }
                        };
                    } else {
                        err_print("ping: -i requires a value\n");
                        return;
                    }
                }
                "-s" => {
                    if let Some(v) = words.next() {
                        payload = match parse_u32(v) {
                            Some(n) if n <= 1458 => n as usize,
                            _ => { err_print("ping: -s must be 0..1458\n"); return; }
                        };
                    } else {
                        err_print("ping: -s requires a value\n");
                        return;
                    }
                }
                "-W" => {
                    if let Some(v) = words.next() {
                        timeout_us = match parse_seconds_us(v) {
                            Some(us) if us > 0 => us,
                            _ => { err_print("ping: bad value for -W\n"); return; }
                        };
                    } else {
                        err_print("ping: -W requires a value\n");
                        return;
                    }
                }
                _ if w.starts_with('-') => {
                    err_print("ping: unknown option: ");
                    err_print(w);
                    puts("\n");
                    return;
                }
                _ => {
                    target_str = w;
                }
            }
        }
    }

    if target_str.is_empty() {
        err_print("ping: missing destination\n");
        return;
    }

    // ── Resolve destination ─────────────────────────────────────────────────
    let ip = match parse_ip(target_str) {
        Some(ip) => ip,
        None => match ospab_os::net::resolver::resolve_host(target_str) {
            Ok(ip) => {
                puts("Resolved ");
                puts(target_str);
                puts(" -> ");
                print_ip(ip);
                puts("\n");
                ip
            }
            Err(_) => {
                err_print("Cannot resolve: ");
                err_print(target_str);
                puts("\n");
                return;
            }
        },
    };

    // ── ARP warm-up ─────────────────────────────────────────────────────────
    if ospab_os::net::arp::cache_lookup(ip).is_none() {
        ospab_os::net::arp::send_request(ip);
        let arp_end = ospab_os::arch::x86_64::tsc::tsc_stamp_us() + 500_000;
        while ospab_os::arch::x86_64::tsc::tsc_stamp_us() < arp_end {
            ospab_os::net::poll_rx();
            if ospab_os::net::arp::cache_lookup(ip).is_some() { break; }
            ospab_os::core::scheduler::sys_yield();
        }
    }

    // ── Header ──────────────────────────────────────────────────────────────
    // PING 10.0.2.2 (10.0.2.2) 56(84) bytes of data.
    let total_ip = 20 + 8 + payload;
    puts("PING ");
    print_ip(ip);
    puts(" (");
    print_ip(ip);
    puts(") ");
    print_dec(payload as u64);
    puts("(");
    print_dec(total_ip as u64);
    puts(") bytes of data.\n");

    ospab_os::net::poll_rx();
    CTRL_C.store(false, Ordering::Relaxed);

    let mut sent: u64      = 0;
    let mut received: u64  = 0;
    let mut seq: u16       = 1;
    let mut interrupted    = false;

    // RTT statistics in µs
    let mut rtt_min: u64   = u64::MAX;
    let mut rtt_max: u64   = 0;
    let mut rtt_sum: u64   = 0;
    let mut rtt_sum_sq: u64 = 0;

    loop {
        // Respect -c count
        if let Some(max) = count {
            if sent >= max as u64 { break; }
        }
        if check_ctrl_c() { interrupted = true; break; }

        // Send
        ospab_os::net::icmp::send_ping_sized(ip, seq, payload);
        sent += 1;

        // Wait for reply (TSC-based)
        let mut reply = None;
        let wait_start = ospab_os::arch::x86_64::tsc::tsc_stamp_us();
        loop {
            if check_ctrl_c() { interrupted = true; break; }
            ospab_os::net::poll_rx();
            if let Some(r) = ospab_os::net::icmp::poll_reply() {
                reply = Some(r);
                break;
            }
            let elapsed = ospab_os::arch::x86_64::tsc::tsc_stamp_us().saturating_sub(wait_start);
            if elapsed >= timeout_us {
                ospab_os::net::icmp::cancel_wait();
                break;
            }
            ospab_os::core::scheduler::sys_yield();
        }
        if interrupted { break; }

        // Display
        match reply {
            Some(r) => {
                received += 1;
                let rtt = r.rtt_us;
                if rtt < rtt_min { rtt_min = rtt; }
                if rtt > rtt_max { rtt_max = rtt; }
                rtt_sum += rtt;
                rtt_sum_sq += rtt.saturating_mul(rtt);

                // "64 bytes from 10.0.2.2: icmp_seq=1 ttl=64 time=1.23 ms"
                print_dec(r.nbytes);
                puts(" bytes from ");
                print_ip(ip);
                puts(": icmp_seq=");
                print_dec(seq as u64);
                puts(" ttl=");
                print_dec(r.ttl as u64);
                puts(" time=");
                print_rtt_ms(rtt);
                puts(" ms\n");
            }
            None => {
                err_print("Request timeout for icmp_seq ");
                print_dec(seq as u64);
                puts("\n");
            }
        }

        seq = seq.wrapping_add(1);

        // Check count limit after display
        if let Some(max) = count {
            if sent >= max as u64 { break; }
        }
        if check_ctrl_c() { interrupted = true; break; }

        // Inter-packet delay
        let delay_start = ospab_os::arch::x86_64::tsc::tsc_stamp_us();
        while ospab_os::arch::x86_64::tsc::tsc_stamp_us().saturating_sub(delay_start) < interval_us {
            if check_ctrl_c() { interrupted = true; break; }
            ospab_os::net::poll_rx();
            ospab_os::core::scheduler::sys_yield();
        }
        if interrupted { break; }
    }

    // ── Summary ─────────────────────────────────────────────────────────────
    if interrupted {
        framebuffer::draw_string("^C\n", FG_DIM, BG);
    }

    let lost = sent.saturating_sub(received);
    let loss_pct = if sent > 0 { lost * 100 / sent } else { 0 };

    puts("\n--- ");
    print_ip(ip);
    puts(" ping statistics ---\n");
    print_dec(sent);
    puts(" packets transmitted, ");
    print_dec(received);
    puts(" received, ");
    print_dec(loss_pct);
    puts("% packet loss\n");

    if received > 0 {
        let avg = rtt_sum / received;
        let mean_sq = rtt_sum_sq / received;
        let sq_mean = avg.saturating_mul(avg);
        let variance = mean_sq.saturating_sub(sq_mean);
        let mdev = isqrt_u64(variance);
        if rtt_min == u64::MAX { rtt_min = 0; }

        puts("rtt min/avg/max/mdev = ");
        print_rtt_ms(rtt_min);
        puts("/");
        print_rtt_ms(avg);
        puts("/");
        print_rtt_ms(rtt_max);
        puts("/");
        print_rtt_ms(mdev);
        puts(" ms\n");
    }
}

/// Print RTT in microseconds as "X.XX" milliseconds.
fn print_rtt_ms(rtt_us: u64) {
    let ms_int  = rtt_us / 1_000;
    let ms_frac = (rtt_us % 1_000) / 10; // 2 decimal places
    print_dec(ms_int);
    framebuffer::draw_char('.', FG, BG);
    framebuffer::draw_char((b'0' + ((ms_frac / 10) % 10) as u8) as char, FG, BG);
    framebuffer::draw_char((b'0' + (ms_frac % 10) as u8) as char, FG, BG);
}

/// Parse "N" or "N.NNN" seconds → microseconds.
fn parse_seconds_us(s: &str) -> Option<u64> {
    let bytes = s.as_bytes();
    let mut int_part: u64 = 0;
    let mut frac: u64 = 0;
    let mut frac_digits: u32 = 0;
    let mut in_frac = false;
    let mut has = false;
    for &b in bytes {
        if b == b'.' && !in_frac {
            in_frac = true;
        } else if b >= b'0' && b <= b'9' {
            has = true;
            if in_frac {
                if frac_digits < 6 { frac = frac * 10 + (b - b'0') as u64; frac_digits += 1; }
            } else {
                int_part = int_part * 10 + (b - b'0') as u64;
            }
        } else { break; }
    }
    if !has { return None; }
    while frac_digits < 6 { frac *= 10; frac_digits += 1; }
    Some(int_part * 1_000_000 + frac)
}

/// Integer square root.
fn isqrt_u64(n: u64) -> u64 {
    if n == 0 { return 0; }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x { x = y; y = (x + n / x) / 2; }
    x
}

fn cmd_ntpdate(args: &str) {
    if !ospab_os::net::is_up() {
        err_print("Network not available.\n");
        return;
    }

    // Determine which server(s) to try
    let result = if args.is_empty() {
        // No argument: try all fallbacks (QEMU gateway first, then real NTP servers)
        puts("Querying NTP servers...\n");
        ospab_os::net::sntp::sync_system_time()
    } else {
        // Explicit server IP or hostname
        let server_ip = match parse_ip(args) {
            Some(ip) => ip,
            None => match ospab_os::net::resolver::resolve_host(args) {
                Ok(ip) => {
                    puts("Resolved ");
                    puts(args);
                    puts(" -> ");
                    print_ip(ip);
                    puts("\n");
                    ip
                }
                Err(_) => {
                    err_print("Cannot resolve: ");
                    err_print(args);
                    puts("\n");
                    return;
                }
            },
        };
        puts("Querying NTP server ");
        print_ip(server_ip);
        puts("...\n");
        ospab_os::net::sntp::sync_time(server_ip)
    };

    match result {
        Ok(unix_ts) => {
            framebuffer::draw_string("[  OK  ] ", FG_OK, BG);
            puts("Time synchronized: ");
            let mut buf = [0u8; 32];
            let len = ospab_os::net::sntp::format_datetime_with_tz(
                unix_ts,
                ospab_os::net::sntp::read_timezone_offset(),
                &mut buf,
            );
            for i in 0..len {
                framebuffer::draw_char(buf[i] as char, FG, BG);
            }
            puts("\n");
        }
        Err(e) => {
            err_print("NTP sync failed: ");
            err_print(e.as_str());
            puts("\n");
            dim_print("Tip: Make sure QEMU is started with -netdev user,...\n");
            dim_print("     Try: ntpdate time1.google.com\n");
        }
    }
}

// ─── IP parsing helpers ───

fn parse_ip(s: &str) -> Option<[u8; 4]> {
    let bytes = s.as_bytes();
    let mut ip = [0u8; 4];
    let mut octet = 0u32;
    let mut octet_idx = 0usize;
    let mut has_digit = false;

    for &b in bytes {
        if b == b'.' {
            if !has_digit || octet > 255 || octet_idx >= 3 { return None; }
            ip[octet_idx] = octet as u8;
            octet_idx += 1;
            octet = 0;
            has_digit = false;
        } else if b >= b'0' && b <= b'9' {
            octet = octet * 10 + (b - b'0') as u32;
            has_digit = true;
        } else {
            return None;
        }
    }
    if !has_digit || octet > 255 || octet_idx != 3 { return None; }
    ip[3] = octet as u8;
    Some(ip)
}

fn parse_u32(s: &str) -> Option<u32> {
    let mut val = 0u32;
    let mut has = false;
    for &b in s.as_bytes() {
        if b >= b'0' && b <= b'9' {
            val = val * 10 + (b - b'0') as u32;
            has = true;
        } else {
            break;
        }
    }
    if has { Some(val) } else { None }
}

fn print_ip(ip: [u8; 4]) {
    for i in 0..4 {
        print_dec(ip[i] as u64);
        if i < 3 { puts("."); }
    }
}

fn print_mac(mac: [u8; 6]) {
    for i in 0..6 {
        print_hex_byte(mac[i]);
        if i < 5 { puts(":"); }
    }
}

// ══════════════════════════════════════════════════════════════
// grape — Text editor (nano-like)
// ══════════════════════════════════════════════════════════════

fn cmd_grape(args: &str) {
    let path = args.trim();
    if path.is_empty() {
        // Open empty buffer
        ospab_os::grape::edit("");
    } else {
        // Resolve path relative to CWD
        let abs = resolve_path(path);
        ospab_os::grape::edit(&abs);
    }
    // After editor exits, redraw prompt area
    framebuffer::set_cursor_pos(0, 0);
    puts("AETERNA Microkernel ");
    puts(crate::version::VERSION_STR);
    puts(" (");
    puts(crate::version::ARCH);
    puts(")\n\n");
}

// ══════════════════════════════════════════════════════════════
// tomato — Package manager
// ══════════════════════════════════════════════════════════════

fn cmd_tomato(args: &str) {
    // `tomato --tmt <subcmd>` routes to the binary package (.tmt) subsystem
    if let Some(tmt_args) = args.strip_prefix("--tmt") {
        ospab_os::tomato::tmt_dispatch(tmt_args.trim());
    } else {
        ospab_os::tomato::run(args);
    }
}

// ══════════════════════════════════════════════════════════════
// seed — Init system / service manager
// ══════════════════════════════════════════════════════════════

fn cmd_seed(args: &str) {
    ospab_os::seed::run(args);
}

fn cmd_bash(args: &str) {
    ospab_os::plum::preprocess(&alloc::format!("bash {}", args));
}

// ══════════════════════════════════════════════════════════════
// DOOM — The Classic Game (1993)
// ══════════════════════════════════════════════════════════════

fn cmd_doom(_args: &str) {
    puts("\n");
    dim_print("Loading DOOM engine... (shareware v1.9)\n");
    
    // Run DOOM and block until exit
    ospab_os::doom::run();
    
    // After exit, we're back in the terminal
    puts("\n");
    dim_print("Welcome back to AETERNA.\n");
}

fn cmd_aai(args: &str) {
    let mut row = 5usize;
    let parts: alloc::vec::Vec<&str> = args.split_whitespace().collect();
    let mut dispatch_args: alloc::vec::Vec<&str> = alloc::vec!["aai"];
    for p in &parts { dispatch_args.push(p); }
    ospab_os::aai::aai_dispatch(&dispatch_args, &mut row);
}

// ══════════════════════════════════════════════════════════════
// Tutor — Interactive system tutorial
// ══════════════════════════════════════════════════════════════

fn cmd_tutor(args: &str) {
    let topic = if args.is_empty() { "intro" } else { args };

    match topic {
        "intro"       => tutor_intro(),
        "fs"          => tutor_fs(),
        "net"         => tutor_net(),
        "mem"         => tutor_mem(),
        "kernel"      => tutor_kernel(),
        "commands" | "cmd" => tutor_commands(),
        "disk"        => tutor_disk(),
        "persistence" => tutor_persistence(),
        "axon"        => tutor_axon(),
        "shell"       => tutor_shell(),
        "ai" | "ane" | "aai" => tutor_ai(),
        "topics" | "help" | "?" => tutor_topics(),
        _ => {
            puts("Unknown topic: ");
            puts(topic);
            puts("\n\nType '");
            framebuffer::draw_string("tutor topics", FG_OK, BG);
            puts("' for a list of all topics.\n");
        }
    }
}

fn tutor_intro() {
    puts("\n");
    framebuffer::draw_string("  ┌─────────────────────────────────────────┐\n", FG_DIM, BG);
    framebuffer::draw_string("  │", FG_DIM, BG);
    framebuffer::draw_string("  Welcome to ospab.os (AETERNA)          ", FG_OK, BG);
    framebuffer::draw_string("│\n", FG_DIM, BG);
    framebuffer::draw_string("  └─────────────────────────────────────────┘\n\n", FG_DIM, BG);

    puts("  ospab.os is an experimental operating system built from\n");
    puts("  scratch in Rust. It runs on the AETERNA microkernel —\n");
    puts("  a capability-based, AI-native kernel designed for\n");
    puts("  deterministic, high-performance computing.\n\n");

    framebuffer::draw_string("  What you can do right now:\n\n", FG_WARN, BG);
    puts("  1. Welcome & system overview         ");
    dim_print("(ls, cd, cat)\n");
    puts("  2. Check hardware and memory            ");
    dim_print("(lspci, free, lsmem)\n");
    puts("  3. View kernel events                   ");
    dim_print("(dmesg)\n");
    puts("  4. Ping network hosts                   ");
    dim_print("(ping 10.0.2.2)\n");
    puts("  5. Sync time from the internet          ");
    dim_print("(ntpdate)\n");
    puts("  6. Install the OS to virtual disk       ");
    dim_print("(install)\n");
    puts("  7. Run neural inference on bare metal   ");
    dim_print("(aai chat ...)\n\n");

    framebuffer::draw_string("  Quick start:\n", FG_WARN, BG);
    puts("  Type 'help' for all commands\n");
    puts("  Type 'tutor <topic>' to learn more ");
    dim_print("(fs, net, mem, kernel, ai)\n");
    puts("  Use Up/Down arrows to browse command history\n");
    puts("  Press Ctrl+L to clear the screen\n\n");
}

fn tutor_fs() {
    puts("\n");
    framebuffer::draw_string("  Filesystem Tutorial\n", FG_OK, BG);
    puts("  ════════════════════\n\n");

    puts("  AETERNA uses a virtual filesystem (VFS). Files like\n");
    puts("  /proc/meminfo are generated live by the kernel.\n\n");

    framebuffer::draw_string("  Directory Structure:\n\n", FG_WARN, BG);
    puts("  /           Root directory\n");
    puts("  /boot       Kernel and bootloader files\n");
    puts("  /dev        Device files (null, zero, console, fb0)\n");
    puts("  /etc        Configuration (hostname, os-release)\n");
    puts("  /proc       Process and system info (live data)\n");
    puts("  /sys        Sysfs — kernel objects\n");
    puts("  /tmp        Temporary files\n");
    puts("  /home       User home directories\n\n");

    framebuffer::draw_string("  Try these commands:\n\n", FG_WARN, BG);
    dim_print("  ls /proc           — see available proc files\n");
    dim_print("  cat /proc/meminfo  — live memory statistics\n");
    dim_print("  cat /proc/cpuinfo  — CPU information\n");
    dim_print("  cat /etc/os-release — OS identity\n\n");
}

fn tutor_net() {
    puts("\n");
    framebuffer::draw_string("  Networking Tutorial\n", FG_OK, BG);
    puts("  ═══════════════════\n\n");

    puts("  AETERNA has a built-in TCP/IP stack (IPv4) with:\n");
    puts("  - RTL8139 NIC driver (PCI auto-detected)\n");
    puts("  - Ethernet, ARP, IPv4, ICMP, UDP\n");
    puts("  - SNTP time synchronization\n\n");

    framebuffer::draw_string("  QEMU Setup:\n\n", FG_WARN, BG);
    puts("  Start QEMU with networking enabled:\n");
    dim_print("  qemu-system-x86_64 -cdrom ospab.iso -m 512M \\\n");
    dim_print("    -netdev user,id=n0 -device rtl8139,netdev=n0\n\n");

    puts("  QEMU user-mode networking gives you:\n");
    puts("  - IP:      10.0.2.15 (your VM)\n");
    puts("  - Gateway: 10.0.2.2  (NAT to host)\n");
    puts("  - DNS:     10.0.2.3\n\n");

    framebuffer::draw_string("  Available Commands:\n\n", FG_WARN, BG);
    dim_print("  ifconfig          — show network configuration\n");
    dim_print("  ping 10.0.2.2     — ping the gateway\n");
    dim_print("  ping 8.8.8.8      — ping Google DNS (via NAT)\n");
    dim_print("  ntpdate           — sync time via NTP\n\n");

    puts("  Note: QEMU SLIRP NAT allows outbound connections.\n");
    puts("  Ping may not work to all hosts (QEMU limitation).\n\n");
}

fn tutor_mem() {
    puts("\n");
    framebuffer::draw_string("  Memory Subsystem Tutorial\n", FG_OK, BG);
    puts("  ══════════════════════════\n\n");

    puts("  AETERNA manages memory in several layers:\n\n");

    framebuffer::draw_string("  1. Physical Memory Manager\n", FG_WARN, BG);
    puts("     Bitmap allocator managing 4K frames.\n");
    puts("     Regions from Limine bootloader memory map.\n\n");

    framebuffer::draw_string("  2. Kernel Heap (128 MiB)\n", FG_WARN, BG);
    puts("     Linked-list allocator for dynamic allocation.\n");
    puts("     Used by alloc::Vec, alloc::String, etc.\n\n");

    framebuffer::draw_string("  3. HHDM (Higher Half Direct Map)\n", FG_WARN, BG);
    puts("     All physical memory mapped at offset\n");
    puts("     0xFFFF800000000000. No page table walks needed.\n\n");

    framebuffer::draw_string("  Commands to explore:\n\n", FG_WARN, BG);
    dim_print("  free              — physical + heap overview\n");
    dim_print("  lsmem             — detailed region info\n");
    dim_print("  cat /proc/meminfo — meminfo like Linux\n\n");
}

fn tutor_kernel() {
    puts("\n");
    framebuffer::draw_string("  AETERNA Kernel Architecture\n", FG_OK, BG);
    puts("  ═══════════════════════════\n\n");

    puts("  AETERNA is a microkernel written in Rust (no_std).\n\n");

    framebuffer::draw_string("  Core Principles:\n", FG_WARN, BG);
    puts("  - Capability-based security\n");
    puts("  - Deterministic scheduling (Compute-First)\n");
    puts("  - AI-native primitives (tensor, DMA engine)\n");
    puts("  - NUMA-aware memory allocation\n");
    puts("  - Single address space (SASOS)\n\n");

    framebuffer::draw_string("  Boot Sequence:\n", FG_WARN, BG);
    puts("  1. Limine loads kernel ELF at high address\n");
    puts("  2. _start() → GDT, IDT, PIC, SSE init\n");
    puts("  3. Memory map → physical allocator → heap\n");
    puts("  4. Scheduler + syscall interface\n");
    puts("  5. Network stack (if NIC present)\n");
    puts("  6. Terminal — interactive shell\n\n");

    framebuffer::draw_string("  Module Map:\n", FG_WARN, BG);
    puts("  arch/x86_64/   — platform code (GDT, IDT, PIC)\n");
    puts("  mm/            — memory management\n");
    puts("  core/          — scheduler, IPC, syscall\n");
    puts("  net/           — network stack (RTL8139, IPv4)\n");
    puts("  executive/     — object manager, processes\n");
    puts("  hpc/           — high-perf computing units\n\n");
}

fn tutor_commands() {
    puts("\n");
    framebuffer::draw_string("  Complete Command Reference\n", FG_OK, BG);
    puts("  ═══════════════════════════\n\n");

    framebuffer::draw_string("  Shell Features:\n", FG_WARN, BG);
    puts("  - Up/Down arrows: browse command history\n");
    puts("  - Ctrl+C: cancel current input line\n");
    puts("  - Ctrl+L: clear screen\n");
    puts("  - Tab: insert 4 spaces\n");
    puts("  - Backspace: delete last character\n\n");

    framebuffer::draw_string("  File Commands:\n", FG_WARN, BG);
    dim_print("  ls [path]         List directory (/proc, /etc, etc.)\n");
    dim_print("  cd <path>         Change directory\n");
    dim_print("  pwd               Print working directory\n");
    dim_print("  cat <file>        Display file contents\n\n");

    framebuffer::draw_string("  System Info:\n", FG_WARN, BG);
    dim_print("  version           OS and kernel version\n");
    dim_print("  uname -a          Full system identification\n");
    dim_print("  about             About AETERNA (ASCII art)\n");
    dim_print("  whoami            Current user (root)\n");
    dim_print("  hostname          System name (ospab)\n");
    dim_print("  date              Current date and uptime\n");
    dim_print("  uptime            Detailed uptime counter\n\n");

    framebuffer::draw_string("  Hardware:\n", FG_WARN, BG);
    dim_print("  free / meminfo    Memory usage overview\n");
    dim_print("  lsmem             Memory region details\n");
    dim_print("  lspci             PCI device listing\n");
    dim_print("  dmesg             Kernel event ring buffer\n\n");

    framebuffer::draw_string("  Networking:\n", FG_WARN, BG);
    dim_print("  ifconfig          Network interface config\n");
    dim_print("  ping <ip> [n]     ICMP echo request\n");
    dim_print("  ntpdate [ip]      NTP time synchronization\n\n");

    framebuffer::draw_string("  System Control:\n", FG_WARN, BG);
    dim_print("  echo <text>       Print text to console\n");
    dim_print("  history           Show command history\n");
    dim_print("  install           Launch OS installer TUI\n");
    dim_print("  reboot            Reboot the machine\n");
    dim_print("  shutdown          Power off the system\n\n");

    framebuffer::draw_string("  Userland & AI:\n", FG_WARN, BG);
    dim_print("  doom              Classic DOOM (shareware v1.9)\n");
    dim_print("  aai load <path>   Load .tmt-ai model file\n");
    dim_print("  aai info          Show loaded model metadata\n");
    dim_print("  aai bench         SIMD GEMM performance benchmark\n");
    dim_print("  aai chat <text>   Run inference + stream tokens\n");
    dim_print("  aai summarize <text> Entropy + stats (chars/words/uniq-bytes/H)\n");
    dim_print("  See 'tutor ai' for ANE deep dive\n\n");
}

// ══════════════════════════════════════════════════════════════
// Output helpers
// ══════════════════════════════════════════════════════════════

fn tutor_ai() {
    puts("\n");
    framebuffer::draw_string("  ANE — Aeterna Neural Engine\n", FG_OK, BG);
    puts("  ════════════════════════════\n\n");

    puts("  ANE is a no_std, bare-metal neural inference library\n");
    puts("  built into the AETERNA kernel. No Python, no framework —\n");
    puts("  tensors and GEMM kernels running directly on the CPU.\n\n");

    framebuffer::draw_string("  Architecture:\n", FG_WARN, BG);
    puts("  lib/ane/tensor.rs    — N-D Tensor, GEMM, ReLU, Softmax\n");
    puts("  lib/ane/layers.rs    — Linear, LayerNorm, MHA, Embedding\n");
    puts("  lib/ane/optimizers.rs— AdamW + SGD with SIMD kernels\n");
    puts("  lib/ane/compiler.rs  — Op-fusion graph compiler\n\n");

    framebuffer::draw_string("  SIMD Dispatch (auto-detected at runtime):\n", FG_WARN, BG);
    dim_print("  AVX-512   — 16-wide f32 FMADD (best performance)\n");
    dim_print("  AVX2+FMA  — 8-wide f32 FMADD   (standard modern CPU)\n");
    dim_print("  Scalar    — 16×16 tiled GEMM    (always available)\n\n");

    framebuffer::draw_string("  aai — command-line frontend:\n", FG_WARN, BG);
    dim_print("  aai load /models/tiny.tmt-ai   Load model file\n");
    dim_print("  aai info                       Show metadata\n");
    dim_print("  aai bench                      GEMM perf report\n");
    dim_print("  aai chat Hello, AETERNA!       Inference + streaming\n");
    dim_print("  aai summarize <text>           Entropy + text statistics\n\n");

    framebuffer::draw_string("  .tmt-ai model format:\n", FG_WARN, BG);
    puts("  Offset 0:    Magic  b\"TMT\\x01\"\n");
    puts("  Offset 4:    Version u32 LE\n");
    puts("  Offset 8:    Meta-len u32 LE\n");
    puts("  Offset 12:   UTF-8 JSON {name, arch, d_model, n_layers,\n");
    puts("                           n_heads, vocab_size, ctx_len}\n");
    puts("  Offset ~64:  Raw f32 weights (64-byte aligned)\n\n");

    framebuffer::draw_string("  KV-Cache (Phase 3):\n", FG_WARN, BG);
    puts("  Ring-buffer holding key/value pairs per layer.\n");
    puts("  Allocated once at model load (no GC pressure).\n");
    puts("  Token streaming at 100 Hz via PIT IRQ 0.\n\n");

    framebuffer::draw_string("  Try it:\n", FG_WARN, BG);
    puts("  1. Create a model:   ");
    dim_print("(place .tmt-ai in /models/)\n");
    puts("  2. Load it:          ");
    dim_print("aai load /models/tiny.tmt-ai\n");
    puts("  3. Inspect:          ");
    dim_print("aai info\n");
    puts("  4. Benchmark SIMD:   ");
    dim_print("aai bench\n");
    puts("  5. Chat:             ");
    dim_print("aai chat Tell me about AETERNA\n");
    puts("  6. Analyse text:     ");
    dim_print("aai summarize Hello, AETERNA!\n\n");
}

fn tutor_topics() {
    puts("\n");
    framebuffer::draw_string("  AETERNA Interactive Tutorial\n", FG_OK, BG);
    puts("  ═══════════════════════════════\n\n");
    framebuffer::draw_string("  Usage: ", FG_WARN, BG);
    puts("tutor <topic>\n\n");
    framebuffer::draw_string("  Topics:\n\n", FG_WARN, BG);
    dim_print("  intro       — Welcome & system overview\n");
    dim_print("  fs          — Virtual filesystem (VFS + RamFS)\n");
    dim_print("  disk        — Disk I/O, AHCI/ATA, LBA layout\n");
    dim_print("  persistence — How files survive reboots\n");
    dim_print("  net         — Networking (IP, ICMP, UDP)\n");
    dim_print("  mem         — Physical + heap memory\n");
    dim_print("  kernel      — AETERNA architecture\n");
    dim_print("  ai          — ANE neural engine + aai commands\n");
    dim_print("  axon        — AXON userland coreutils\n");
    dim_print("  shell       — Shell features (plum)\n");
    dim_print("  commands    — Full command reference\n\n");
    framebuffer::draw_string("  Quick examples:\n", FG_WARN, BG);
    dim_print("  tutor fs          — filesystem walkthrough\n");
    dim_print("  tutor ai          — neural engine & aai tool\n");
    dim_print("  tutor persistence — see how sync/recovery works\n");
    dim_print("  tutor axon        — learn new coreutils commands\n\n");
}

fn tutor_disk() {
    puts("\n");
    framebuffer::draw_string("  Disk I/O Tutorial\n", FG_OK, BG);
    puts("  ══════════════════\n\n");

    puts("  AETERNA supports two storage backends:\n\n");

    framebuffer::draw_string("  1. ATA PIO (IDE)\n", FG_WARN, BG);
    puts("     Port I/O based. Sector read/write via ports 0x1F0–0x3F6.\n");
    puts("     Max 128 sectors per request (u8 count field).\n\n");

    framebuffer::draw_string("  2. AHCI SATA (DMA)\n", FG_WARN, BG);
    puts("     Memory-mapped ABAR. Zero-copy DMA write via PRD tables.\n");
    puts("     Supports 48-bit LBA. AHCI preferred over ATA.\n\n");

    framebuffer::draw_string("  Disk Layout:\n", FG_WARN, BG);
    puts("     LBA 0–2047   — Boot area (MBR, GPT, ISO)\n");
    puts("     LBA 2048+    — AETERNA_FS persistence data\n\n");

    framebuffer::draw_string("  Commands:\n\n", FG_WARN, BG);
    dim_print("  dump_disk         — Hex dump of LBA 2048 superblock\n");
    dim_print("  sync              — Force flush RamFS to disk\n");
    dim_print("  df                — Show disk/filesystem usage\n\n");

    framebuffer::draw_string("  Try it:\n", FG_WARN, BG);
    puts("  1. Create a file:   ");
    dim_print("touch /tmp/test.txt\n");
    puts("  2. Write to it:     ");
    dim_print("echo hello > /tmp/test.txt\n");
    puts("  3. Force sync:      ");
    dim_print("sync\n");
    puts("  4. Check superblock: ");
    dim_print("dump_disk\n\n");
}

fn tutor_persistence() {
    puts("\n");
    framebuffer::draw_string("  Filesystem Persistence Tutorial\n", FG_OK, BG);
    puts("  ═════════════════════════════════\n\n");

    puts("  AETERNA persists the entire RamFS to disk using\n");
    puts("  the AETERNA_FS binary format.\n\n");

    framebuffer::draw_string("  Binary Format:\n", FG_WARN, BG);
    puts("  Offset  Size  Field\n");
    dim_print("  0       8     SUPER_MAGIC  = 0x41455445524E41\n");
    dim_print("  8       4     sector_count (u32 LE)\n");
    dim_print("  12      10    \"AETERNA_FS\" marker\n");
    dim_print("  22      4     VERSION = 1\n");
    dim_print("  26      4     COUNT (number of entries)\n");
    dim_print("  30+     var   path_len(2) + path + type(1) + data...\n\n");

    framebuffer::draw_string("  Auto-sync:\n", FG_WARN, BG);
    puts("  Every write/mkdir/touch/rm automatically syncs\n");
    puts("  to disk when storage is available.\n\n");

    framebuffer::draw_string("  Boot Recovery:\n", FG_WARN, BG);
    puts("  On boot:\n");
    puts("  1. Read 1 sector at LBA 2048\n");
    puts("  2. Validate SUPER_MAGIC\n");
    puts("  3. Extract sector_count from bytes 8-11\n");
    puts("  4. Read sector_count sectors in 128-sector batches\n");
    puts("  5. Deserialize → restore RamFS tree\n\n");

    framebuffer::draw_string("  Try it:\n", FG_WARN, BG);
    puts("  Write a file and reboot:\n");
    dim_print("  echo 'hello world' > /home/root/note.txt\n");
    dim_print("  reboot\n");
    puts("  After reboot:\n");
    dim_print("  cat /home/root/note.txt   (should show 'hello world')\n\n");
}

fn tutor_axon() {
    puts("\n");
    framebuffer::draw_string("  AXON Coreutils Tutorial\n", FG_OK, BG);
    puts("  ════════════════════════\n\n");

    puts("  AXON is the AETERNA coreutils — a complete set of\n");
    puts("  POSIX-inspired file and text utilities.\n\n");

    framebuffer::draw_string("  Text Processing:\n", FG_WARN, BG);
    dim_print("  wc <file>          — count lines/words/bytes\n");
    dim_print("  head [-n N] <file> — show first N lines\n");
    dim_print("  tail [-n N] <file> — show last N lines\n");
    dim_print("  grep <pat> <file>  — search for pattern\n");
    dim_print("  sort <file>        — sort lines\n");
    dim_print("  uniq <file>        — remove duplicate lines\n");
    dim_print("  cut -f1 -d: <file> — extract field N\n");
    dim_print("  awk '{print $2}' <file>  — field-based processing\n");
    dim_print("  diff <file1> <file2>    — compare files\n\n");

    framebuffer::draw_string("  File Utilities:\n", FG_WARN, BG);
    dim_print("  cp <src> <dst>     — copy file\n");
    dim_print("  mv <src> <dst>     — move/rename file\n");
    dim_print("  find <dir> <name>  — search for files\n");
    dim_print("  du <dir>           — disk usage by file\n");
    dim_print("  tree <dir>         — visual directory tree\n");
    dim_print("  stat <path>        — file info\n");
    dim_print("  xxd <file>         — hex dump\n");
    dim_print("  nl <file>          — number lines\n\n");

    framebuffer::draw_string("  System Utils:\n", FG_WARN, BG);
    dim_print("  ps                 — show processes\n");
    dim_print("  df                 — disk/fs usage\n");
    dim_print("  kill [-N] <pid>    — send signal to process\n");
    dim_print("  which <cmd>        — find where a command lives\n");
    dim_print("  env                — show environment variables\n");
    dim_print("  printf <fmt> ...   — formatted output\n");
    dim_print("  xargs <cmd> items  — build and run commands\n\n");

    framebuffer::draw_string("  Try this pipeline:\n", FG_WARN, BG);
    dim_print("  cat /etc/os-release\n");
    dim_print("  wc /etc/os-release\n");
    dim_print("  grep VERSION /etc/os-release\n");
    dim_print("  awk '{print $1}' /etc/os-release\n\n");
}

fn tutor_shell() {
    puts("\n");
    framebuffer::draw_string("  Shell (plum) Tutorial\n", FG_OK, BG);
    puts("  ══════════════════════\n\n");

    puts("  ospab.os uses 'plum' as its interactive shell.\n");
    puts("  It supports environment variables, aliases, and\n");
    puts("  command chaining with ';'.\n\n");

    framebuffer::draw_string("  Key Bindings:\n", FG_WARN, BG);
    puts("  Up/Down      Browse command history\n");
    puts("  Ctrl+L       Clear screen\n");
    puts("  Ctrl+C       Cancel current input\n");
    puts("  Tab          Insert 4 spaces\n");
    puts("  Backspace    Delete last character\n\n");

    framebuffer::draw_string("  Environment Variables:\n", FG_WARN, BG);
    dim_print("  export KEY=VALUE   — set a variable\n");
    dim_print("  echo $KEY          — expand a variable\n");
    dim_print("  env                — show all variables\n\n");

    framebuffer::draw_string("  Aliases:\n", FG_WARN, BG);
    dim_print("  alias ll='ls -la'  — create an alias\n");
    dim_print("  unalias ll         — remove an alias\n\n");

    framebuffer::draw_string("  Command Chaining:\n", FG_WARN, BG);
    dim_print("  mkdir /tmp/t ; touch /tmp/t/f.txt ; ls /tmp/t\n\n");

    framebuffer::draw_string("  Redirection:\n", FG_WARN, BG);
    dim_print("  echo hello > /tmp/out.txt  — write to file\n");
    dim_print("  echo more >> /tmp/out.txt  — append to file\n\n");

    framebuffer::draw_string("  Startup script:\n", FG_WARN, BG);
    puts("  /etc/plum/plumrc is sourced at shell start.\n");
    puts("  Write your aliases and ENV there with:\n");
    dim_print("  echo 'export PS1=\"\\$> \"' >> /etc/plum/plumrc\n\n");
}

// ══════════════════════════════════════════════════════════════
// Output helpers
// ══════════════════════════════════════════════════════════════

fn puts(s: &str) {
    framebuffer::draw_string(s, FG, BG);
}

fn dim_print(s: &str) {
    framebuffer::draw_string(s, FG_DIM, BG);
}

fn err_print(s: &str) {
    framebuffer::draw_string(s, FG_ERR, BG);
}

fn print_dec(mut val: u64) {
    if val == 0 {
        framebuffer::draw_char('0', FG, BG);
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while val > 0 {
        buf[i] = b'0' + (val % 10) as u8;
        val /= 10;
        i += 1;
    }
    for j in (0..i).rev() {
        framebuffer::draw_char(buf[j] as char, FG, BG);
    }
}

fn print_hex_byte(val: u8) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    framebuffer::draw_char(HEX[(val >> 4) as usize] as char, FG, BG);
    framebuffer::draw_char(HEX[(val & 0xF) as usize] as char, FG, BG);
}

fn print_hex_u16(val: u16) {
    print_hex_byte((val >> 8) as u8);
    print_hex_byte(val as u8);
}

fn print_size(bytes: u64) {
    let mut tmp = [0u8; 20];
    let len = format_size(bytes, &mut tmp);
    for i in 0..len {
        framebuffer::draw_char(tmp[i] as char, FG, BG);
    }
}

fn print_size_padded(bytes: u64, pad: usize) {
    let mut tmp = [0u8; 20];
    let len = format_size(bytes, &mut tmp);
    for i in 0..len {
        framebuffer::draw_char(tmp[i] as char, FG, BG);
    }
    if len < pad {
        for _ in 0..(pad - len) {
            framebuffer::draw_char(' ', FG, BG);
        }
    }
}

fn format_size(bytes: u64, buf: &mut [u8; 20]) -> usize {
    let (val, suffix) = if bytes >= 1024 * 1024 * 1024 {
        (bytes / (1024 * 1024 * 1024), b" GiB")
    } else if bytes >= 1024 * 1024 {
        (bytes / (1024 * 1024), b" MiB")
    } else if bytes >= 1024 {
        (bytes / 1024, b" KiB")
    } else {
        (bytes, b" B\0\0")
    };

    let mut pos = 0;
    if val == 0 {
        buf[pos] = b'0';
        pos += 1;
    } else {
        let mut digits = [0u8; 10];
        let mut n = val;
        let mut dcount = 0;
        while n > 0 {
            digits[dcount] = b'0' + (n % 10) as u8;
            n /= 10;
            dcount += 1;
        }
        for j in (0..dcount).rev() {
            buf[pos] = digits[j];
            pos += 1;
        }
    }
    for &b in suffix.iter() {
        if b == 0 { break; }
        buf[pos] = b;
        pos += 1;
    }
    pos
}
