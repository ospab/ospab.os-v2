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

extern crate alloc;

// ─── Global Ctrl+C flag ────────────────────────────────────────────────────
// Set when Ctrl+C is pressed during a running command.
// Long-running commands must check this and break out.
static CTRL_C: AtomicBool = AtomicBool::new(false);

/// Call from any running command to check if the user pressed Ctrl+C.
/// Returns true and prints "^C" if so.
fn check_ctrl_c() -> bool {
    // Also drain keyboard for Ctrl+C that arrives during execution
    if let Some(ch) = keyboard::poll_key() {
        if ch == '\x03' {
            CTRL_C.store(true, Ordering::Relaxed);
        }
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
    unsafe { core::str::from_utf8_unchecked(&CWD[..CWD_LEN]) }
}

/// Resolve a user-supplied path to an absolute path.
/// - If path starts with '/', it's already absolute.
/// - If path is "~", return "/".
/// - Otherwise, join with CWD.
fn resolve_path(path: &str) -> alloc::string::String {
    if path == "~" {
        return alloc::string::String::from("/");
    }
    if path.starts_with('/') {
        // Already absolute — normalize trailing slash
        let mut s = alloc::string::String::from(path);
        while s.len() > 1 && s.ends_with('/') {
            s.pop();
        }
        return s;
    }
    // Relative path: join with CWD
    let cwd = cwd_str();
    let mut abs = alloc::string::String::from(cwd);
    if !abs.ends_with('/') {
        abs.push('/');
    }
    abs.push_str(path);
    while abs.len() > 1 && abs.ends_with('/') {
        abs.pop();
    }
    abs
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
    framebuffer::draw_string(PROMPT_PATH, FG_PATH, BG);
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
                        // Tab: insert 4 spaces at cursor
                        let spaces = 4usize.min(INPUT_BUFFER_SIZE - 1 - INPUT_LEN);
                        if spaces > 0 {
                            for i in (INPUT_CURSOR..INPUT_LEN).rev() {
                                INPUT_BUFFER[i + spaces] = INPUT_BUFFER[i];
                            }
                            for s in 0..spaces {
                                INPUT_BUFFER[INPUT_CURSOR + s] = b' ';
                            }
                            INPUT_CURSOR += spaces;
                            INPUT_LEN += spaces;
                            redraw_input_from(INPUT_CURSOR - spaces);
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
        "ls"      => cmd_ls(args),
        "pwd"     => cmd_pwd(),
        "cd"      => cmd_cd(args),
        "cat"     => cmd_cat(args),
        "mkdir"   => cmd_mkdir(args),
        "touch"   => cmd_touch(args),
        "rm"      => cmd_rm(args),
        "whoami"  => cmd_whoami(),
        "hostname"=> cmd_hostname(),
        "date"    => cmd_date(),
        "about"   => cmd_about(),
        "meminfo" | "free" => cmd_meminfo(),
        "uptime"  => cmd_uptime(),
        "dmesg"   => cmd_dmesg(),
        "lsmem"   => cmd_lsmem(),
        "lspci"   => cmd_lspci(),
        "lsblk"   => cmd_lsblk(),
        "fdisk"   => cmd_fdisk(args),
        "ping"    => cmd_ping(args),
        "ifconfig" | "ip" => cmd_ifconfig(),
        "ntpdate" => cmd_ntpdate(args),
        "sync"    => cmd_sync(),
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
            // Try plum shell preprocessing (alias/variable expansion)
            let full_cmd = if args.is_empty() {
                alloc::string::String::from(command)
            } else {
                alloc::format!("{} {}", command, args)
            };
            if let Some(_expanded) = ospab_os::plum::preprocess(&full_cmd) {
                // If plum returned an expanded command that's different,
                // it was already handled by a builtin
            } else {
                puts(command);
                err_print(": command not found\n");
                dim_print("Type 'help' for available commands.\n");
            }
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
    lines.push(("cmd", "  ntpdate    NTP time sync"));
    lines.push(("normal", ""));
    lines.push(("section", "  SYSTEM CONTROL"));
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

fn cmd_ls(args: &str) {
    let raw_path = if args.is_empty() { cwd_str() } else { args };

    // Build absolute path
    let abs_path = resolve_path(raw_path);

    // Refresh /proc files if listing /proc
    if abs_path.starts_with("/proc") {
        ospab_os::fs::ramfs::refresh_proc_files();
    }

    let entries = match ospab_os::fs::readdir(&abs_path) {
        Some(e) => e,
        None => {
            if !ospab_os::fs::exists(&abs_path) {
                err_print("ls: cannot access '");
                err_print(raw_path);
                err_print("': No such file or directory\n");
            } else {
                dim_print("(empty directory)\n");
            }
            return;
        }
    };
    if entries.is_empty() {
        dim_print("(empty directory)\n");
        return;
    }
    for e in &entries {
        match e.node_type {
            ospab_os::fs::NodeType::Directory => {
                framebuffer::draw_string("d ", FG_DIM, BG);
                framebuffer::draw_string(&e.name, FG_DIR, BG);
                framebuffer::draw_string("/\n", FG_DIR, BG);
            }
            ospab_os::fs::NodeType::File | ospab_os::fs::NodeType::CharDevice => {
                framebuffer::draw_string("- ", FG_DIM, BG);
                puts(&e.name);
                let pad = if e.name.len() < 16 { 16 - e.name.len() } else { 1 };
                for _ in 0..pad { puts(" "); }
                if e.size > 0 {
                    dim_dec(e.size as u64);
                } else {
                    dim_print("0");
                }
                puts("\n");
            }
        }
    }
}

/// Print a decimal number in dim color
fn dim_dec(mut val: u64) {
    if val == 0 {
        dim_print("0");
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
        dim_print(s);
    }
}

fn cmd_pwd() {
    puts(cwd_str());
    puts("\n");
}

fn cmd_cd(args: &str) {
    if args.is_empty() || args == "~" || args == "/" {
        unsafe { CWD[0] = b'/'; CWD_LEN = 1; }
        return;
    }
    if args == ".." {
        // Go to parent
        unsafe {
            if CWD_LEN > 1 {
                // Find last '/' (excluding trailing)
                let s = core::str::from_utf8_unchecked(&CWD[..CWD_LEN]);
                if let Some(pos) = s[..s.len()].rfind('/') {
                    if pos == 0 {
                        CWD_LEN = 1; // root
                    } else {
                        CWD_LEN = pos;
                    }
                }
            }
        }
        return;
    }

    // Build absolute path
    let abs = resolve_path(args);

    // Check if it exists as a directory in VFS
    if ospab_os::fs::exists(&abs) {
        // Verify it's a directory
        if let Some(stat) = ospab_os::fs::stat(&abs) {
            match stat.node_type {
                ospab_os::fs::NodeType::Directory => {
                    let bytes = abs.as_bytes();
                    unsafe {
                        let len = bytes.len().min(63);
                        CWD[..len].copy_from_slice(&bytes[..len]);
                        CWD_LEN = len;
                    }
                    return;
                }
                _ => {
                    err_print("cd: ");
                    err_print(args);
                    err_print(": Not a directory\n");
                    return;
                }
            }
        }
    }
    err_print("cd: ");
    err_print(args);
    err_print(": No such directory\n");
}

fn cmd_cat(args: &str) {
    if args.is_empty() {
        err_print("cat: missing operand\n");
        return;
    }

    let abs = resolve_path(args);

    // Refresh /proc before reading
    if abs.starts_with("/proc") {
        ospab_os::fs::ramfs::refresh_proc_files();
    }

    match ospab_os::fs::read_file(&abs) {
        Some(data) => {
            if data.is_empty() {
                return; // no content (e.g. /dev/null)
            }
            // Print as UTF-8 text
            if let Ok(text) = core::str::from_utf8(&data) {
                puts(text);
                // Ensure trailing newline
                if !text.ends_with('\n') {
                    puts("\n");
                }
            } else {
                // Binary data — show size
                err_print("cat: ");
                err_print(args);
                err_print(": binary file (");
                print_dec(data.len() as u64);
                err_print(" bytes)\n");
            }
        }
        None => {
            err_print("cat: ");
            err_print(args);
            err_print(": No such file or directory\n");
        }
    }
}

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

fn cmd_mkdir(args: &str) {
    if args.is_empty() {
        err_print("mkdir: missing operand\n");
        return;
    }
    let abs = resolve_path(args);
    if ospab_os::fs::exists(&abs) {
        err_print("mkdir: cannot create directory '");
        err_print(args);
        err_print("': File exists\n");
        return;
    }
    if ospab_os::fs::mkdir(&abs) {
        // success — silent (like real mkdir)
    } else {
        err_print("mkdir: cannot create directory '");
        err_print(args);
        err_print("'\n");
    }
}

fn cmd_touch(args: &str) {
    if args.is_empty() {
        err_print("touch: missing file operand\n");
        return;
    }
    let abs = resolve_path(args);
    if ospab_os::fs::exists(&abs) {
        return; // touch existing file is a no-op (update timestamp; we have none)
    }
    if ospab_os::fs::touch(&abs) {
        // success — silent
    } else {
        err_print("touch: cannot touch '");
        err_print(args);
        err_print("'\n");
    }
}

fn cmd_rm(args: &str) {
    if args.is_empty() {
        err_print("rm: missing operand\n");
        return;
    }
    let abs = resolve_path(args);
    if !ospab_os::fs::exists(&abs) {
        err_print("rm: cannot remove '");
        err_print(args);
        err_print("': No such file or directory\n");
        return;
    }
    if ospab_os::fs::remove(&abs) {
        // success
    } else {
        err_print("rm: cannot remove '");
        err_print(args);
        err_print("': Is a directory or not empty\n");
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
    let total_secs = ticks / 18;
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
        let heap_total = ospab_os::mm::heap::HEAP_SIZE as u64;
        puts("Heap:  ");
        print_size_padded(used as u64, 12);
        print_size_padded(heap_total - used as u64, 12);
        print_size_padded(heap_total, 12);
        puts("\n");
    }
}

fn cmd_uptime() {
    let ticks = ospab_os::arch::x86_64::idt::timer_ticks();
    let seconds = ticks / 18;
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
        print_size(ospab_os::mm::heap::HEAP_SIZE as u64);
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
    puts("Syncing filesystem...\n");
    klog::record(klog::EventSource::Boot, "Sync requested");
    
    if ospab_os::fs::disk_sync::sync_filesystem() {
        dim_print("Filesystem synchronized to disk.\n");
    } else {
        err_print("Failed to sync filesystem.\n");
    }
}

fn cmd_reboot() {
    puts("Syncing and rebooting...\n");
    
    // Sync filesystem to disk before reboot
    ospab_os::fs::disk_sync::sync_filesystem();

    puts("Rebooting...\n");
    klog::record(klog::EventSource::Boot, "Reboot requested");
    ospab_os::arch::x86_64::serial::write_str("[AETERNA] Rebooting...\r\n");
    unsafe {
        asm!("cli");
        let mut timeout = 100000u32;
        loop {
            let status: u8;
            asm!("in al, dx", in("dx") 0x64u16, out("al") status, options(nomem, nostack));
            if status & 0x02 == 0 || timeout == 0 { break; }
            timeout -= 1;
        }
        asm!("out dx, al", in("dx") 0x64u16, in("al") 0xFEu8, options(nomem, nostack));
        for _ in 0..1000000u32 { asm!("pause"); }
        let null_idt: [u8; 6] = [0; 6];
        asm!("lidt [{}]", in(reg) &null_idt, options(noreturn));
    }
}

fn cmd_shutdown() {
    puts("System shutting down...\n");
    klog::record(klog::EventSource::Boot, "Shutdown requested");
    ospab_os::arch::x86_64::serial::write_str("[AETERNA] Shutting down...\r\n");
    unsafe {
        asm!("cli");
        asm!("out dx, ax", in("dx") 0x604u16, in("ax") 0x2000u16, options(nomem, nostack));
        asm!("out dx, ax", in("dx") 0xB004u16, in("ax") 0x2000u16, options(nomem, nostack));
        asm!("out dx, ax", in("dx") 0x4004u16, in("ax") 0x3400u16, options(nomem, nostack));
    }
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
        err_print("Usage: ping <ip-address> [count]\n");
        dim_print("  Example: ping 10.0.2.2\n");
        return;
    }

    // Parse IP and optional count
    let (ip_str, count_str) = match args.find(' ') {
        Some(pos) => (&args[..pos], args[pos + 1..].trim()),
        None => (args, "4"),
    };

    let ip = match parse_ip(ip_str) {
        Some(ip) => ip,
        None => {
            err_print("Invalid IP address: ");
            err_print(ip_str);
            puts("\n");
            return;
        }
    };

    let count = parse_u32(count_str).unwrap_or(4).min(100);

    puts("PING ");
    print_ip(ip);
    puts(" ");
    print_dec(count as u64);
    puts(" packets (press Ctrl+C to stop)\n");

    // Prime the receive queue
    ospab_os::net::poll_rx();

    let mut sent = 0u32;
    let mut received = 0u32;
    let mut interrupted = false;

    'outer: for seq in 0..count {
        // Check Ctrl+C before sending
        if check_ctrl_c() { interrupted = true; break 'outer; }

        ospab_os::net::icmp::send_ping(ip, seq as u16);
        sent += 1;

        // ─── Wait up to ~2 s for reply, polling keyboard each tick ───
        let deadline = ospab_os::arch::x86_64::idt::timer_ticks() + 36;
        let mut reply: Option<(u16, u64)> = None;
        loop {
            if check_ctrl_c() { interrupted = true; break; }
            if let Some(r) = ospab_os::net::icmp::poll_reply() {
                reply = Some(r);
                break;
            }
            if ospab_os::arch::x86_64::idt::timer_ticks() >= deadline {
                ospab_os::net::icmp::cancel_wait();
                break;
            }
            unsafe { asm!("hlt"); }
        }

        if interrupted { break 'outer; }

        match reply {
            Some((rseq, rtt_ms)) => {
                received += 1;
                puts("  Reply from ");
                print_ip(ip);
                puts(": seq=");
                print_dec(rseq as u64);
                puts(" time=");
                if rtt_ms == 0 { puts("<1"); } else { print_dec(rtt_ms); }
                puts("ms\n");
            }
            None => {
                puts("  Request timed out (seq=");
                print_dec(seq as u64);
                puts(")\n");
            }
        }

        // ─── 1-second inter-ping delay, interruptible ───
        if seq + 1 < count {
            let wait_until = ospab_os::arch::x86_64::idt::timer_ticks() + 18;
            loop {
                if check_ctrl_c() { interrupted = true; break; }
                if ospab_os::arch::x86_64::idt::timer_ticks() >= wait_until { break; }
                ospab_os::net::poll_rx();
                unsafe { asm!("hlt"); }
            }
            if interrupted { break 'outer; }
        }
    }

    if interrupted {
        framebuffer::draw_string("^C\n", FG_DIM, BG);
    }
    puts("--- ping statistics ---\n");
    print_dec(sent as u64);
    puts(" transmitted, ");
    print_dec(received as u64);
    puts(" received, ");
    if sent > 0 {
        print_dec(((sent - received) * 100 / sent) as u64);
    } else {
        puts("0");
    }
    puts("% packet loss\n");
}

fn cmd_ntpdate(args: &str) {
    if !ospab_os::net::is_up() {
        err_print("Network not available.\n");
        return;
    }

    // Parse server IP or use QEMU gateway
    let server_ip = if args.is_empty() {
        unsafe { ospab_os::net::GATEWAY_IP }
    } else {
        match parse_ip(args) {
            Some(ip) => ip,
            None => {
                err_print("Invalid IP: ");
                err_print(args);
                puts("\n");
                return;
            }
        }
    };

    puts("Querying NTP server ");
    print_ip(server_ip);
    puts("...\n");

    match ospab_os::net::sntp::sync_time(server_ip) {
        Some(unix_ts) => {
            framebuffer::draw_string("[  OK  ] ", FG_OK, BG);
            puts("Time synchronized: ");
            let mut buf = [0u8; 32];
            let len = ospab_os::net::sntp::format_datetime(unix_ts, &mut buf);
            for i in 0..len {
                framebuffer::draw_char(buf[i] as char, FG, BG);
            }
            puts("\n");
        }
        None => {
            err_print("NTP request timed out. Server may not support NTP.\n");
            dim_print("Tip: QEMU gateway (10.0.2.2) doesn't always respond to NTP.\n");
            dim_print("     Try: ntpdate <external-ntp-server-ip>\n");
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
    ospab_os::tomato::run(args);
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

// ══════════════════════════════════════════════════════════════
// Tutor — Interactive system tutorial
// ══════════════════════════════════════════════════════════════

fn cmd_tutor(args: &str) {
    let topic = if args.is_empty() { "intro" } else { args };

    match topic {
        "intro" => tutor_intro(),
        "fs"    => tutor_fs(),
        "net"   => tutor_net(),
        "mem"   => tutor_mem(),
        "kernel" => tutor_kernel(),
        "commands" | "cmd" => tutor_commands(),
        _ => {
            puts("Unknown topic: ");
            puts(topic);
            puts("\n\nAvailable topics:\n");
            dim_print("  intro     — Welcome and system overview\n");
            dim_print("  fs        — Virtual filesystem guide\n");
            dim_print("  net       — Networking tutorial\n");
            dim_print("  mem       — Memory subsystem\n");
            dim_print("  kernel    — AETERNA kernel architecture\n");
            dim_print("  commands  — All commands explained\n");
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
    puts("  1. Explore the virtual filesystem      ");
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
    dim_print("(install)\n\n");

    framebuffer::draw_string("  Quick start:\n", FG_WARN, BG);
    puts("  Type 'help' for all commands\n");
    puts("  Type 'tutor <topic>' to learn more ");
    dim_print("(fs, net, mem, kernel)\n");
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
