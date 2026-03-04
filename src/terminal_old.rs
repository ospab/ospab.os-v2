/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Minimalist terminal - white text only, red for errors.
*/
#![no_std]

use core::arch::asm;
use ospab_os::arch::x86_64::framebuffer;
use ospab_os::arch::x86_64::keyboard;

const PROMPT: &str = "ospab.os # ";
const INPUT_BUFFER_SIZE: usize = 256;

// Colors - minimal palette
const COLOR_TEXT: u32 = 0x00FFFFFF;      // White
const COLOR_ERROR: u32 = 0x000000FF;     // Red (BGR)
const COLOR_BG: u32 = 0x00000000;        // Black

// Margins
const MARGIN_X: u64 = 10;
const MARGIN_Y: u64 = 10;

static mut INPUT_BUFFER: [u8; INPUT_BUFFER_SIZE] = [0; INPUT_BUFFER_SIZE];
static mut INPUT_LEN: usize = 0;

pub fn run() -> ! {
    keyboard::init();
    
    // Clear and set position
    framebuffer::clear(COLOR_BG);
    framebuffer::set_cursor_pos(MARGIN_X, MARGIN_Y);
    
    // Welcome message
    print_line("AETERNA Microkernel v2.0");
    print_line("Type 'help' for commands.");
    print_line("");
    
    loop {
        draw_prompt();
        let cmd = read_line();
        execute_command(cmd);
        print_line("");
    }
}

fn draw_prompt() {
    for c in PROMPT.chars() {
        framebuffer::draw_char(c, COLOR_TEXT, COLOR_BG);
    }
}

fn read_line() -> &'static str {
    unsafe {
        INPUT_LEN = 0;
        
        loop {
            let c = keyboard::poll_key();
            
            if let Some(ch) = c {
                match ch {
                    '\n' => {
                        print_line("");
                        INPUT_BUFFER[INPUT_LEN] = 0;
                        return core::str::from_utf8_unchecked(&INPUT_BUFFER[..INPUT_LEN]);
                    }
                    '\x08' => {
                        if INPUT_LEN > 0 {
                            INPUT_LEN -= 1;
                            let (x, y) = framebuffer::cursor_pos();
                            if x >= 8 {
                                framebuffer::set_cursor_pos(x - 8, y);
                                framebuffer::draw_char(' ', COLOR_TEXT, COLOR_BG);
                                framebuffer::set_cursor_pos(x - 8, y);
                            }
                        }
                    }
                    '\x03' => {
                        // Ctrl+C
                        print_line("");
                        print_error("^C");
                        print_line("");
                        INPUT_LEN = 0;
                        INPUT_BUFFER[0] = 0;
                        return core::str::from_utf8_unchecked(&INPUT_BUFFER[..0]);
                    }
                    c => {
                        if INPUT_LEN < INPUT_BUFFER_SIZE - 1 && c.is_ascii() {
                            INPUT_BUFFER[INPUT_LEN] = c as u8;
                            INPUT_LEN += 1;
                            framebuffer::draw_char(c, COLOR_TEXT, COLOR_BG);
                        }
                    }
                }
            }
        }
    }
}

fn print_line(s: &str) {
    for c in s.chars() {
        framebuffer::draw_char(c, COLOR_TEXT, COLOR_BG);
    }
    framebuffer::draw_char('\n', COLOR_TEXT, COLOR_BG);
}

fn print_error(s: &str) {
    for c in s.chars() {
        framebuffer::draw_char(c, COLOR_ERROR, COLOR_BG);
    }
}

fn execute_command(cmd: &str) {
    let cmd = cmd.trim();
    
    if cmd.is_empty() {
        return;
    }
    
    let mut parts = cmd.split_whitespace();
    let command = parts.next().unwrap_or("");
    
    match command {
        "help" => cmd_help(),
        "echo" => cmd_echo(cmd.strip_prefix("echo ").unwrap_or("")),
        "clear" => cmd_clear(),
        "ver" | "version" => cmd_version(),
        "ls" => cmd_ls(),
        "about" => cmd_about(),
        "meminfo" => cmd_meminfo(),
        "uptime" => cmd_uptime(),
        "uname" => cmd_uname(),
        "reboot" => cmd_reboot(),
        "" => {}
        _ => {
            print_error("Unknown: ");
            print_line(command);
        }
    }
}

fn cmd_help() {
    print_line("Commands:");
    print_line("  help     Show help");
    print_line("  echo     Print text");
    print_line("  clear    Clear screen");
    print_line("  ver      Version info");
    print_line("  ls       List files");
    print_line("  about    About system");
    print_line("  meminfo  Memory info");
    print_line("  uptime   System uptime");
    print_line("  uname    System name");
    print_line("  reboot   Reboot");
}

fn cmd_echo(args: &str) {
    print_line(args);
}

fn cmd_clear() {
    framebuffer::clear(COLOR_BG);
    framebuffer::set_cursor_pos(MARGIN_X, MARGIN_Y);
}

fn cmd_version() {
    print_line("ospab.os v2.0 (AETERNA)");
    print_line("2026-03-01");
}

fn cmd_ls() {
    print_line("(no files)");
}

fn cmd_about() {
    print_line("ospab.os / AETERNA");
    print_line("Deterministic microkernel");
    print_line("Compute-First Scheduler");
    print_line("Capability-based security");
}

fn cmd_meminfo() {
    let stats = ospab_os::mm::physical::stats();
    print_str("Total:    ");
    print_size(stats.total_bytes);
    print_line("");
    print_str("Usable:   ");
    print_size(stats.usable_bytes);
    print_line("");
    print_str("Reserved: ");
    print_size(stats.reserved_bytes);
    print_line("");
    print_str("Regions:  ");
    print_dec(stats.region_count as u64);
    print_line("");
    if ospab_os::mm::heap::is_initialized() {
        let (used, free) = ospab_os::mm::heap::stats();
        print_str("Heap used: ");
        print_size(used as u64);
        print_str(" / ");
        print_size(ospab_os::mm::heap::heap_size() as u64);
        print_line("");
    }
}

fn cmd_uptime() {
    let ticks = ospab_os::arch::x86_64::idt::timer_ticks();
    // PIT default rate: ~18.2 Hz, so ticks / 18 ~= seconds
    let seconds = ticks / 18;
    let minutes = seconds / 60;
    print_str("Uptime: ");
    print_dec(minutes);
    print_str("m ");
    print_dec(seconds % 60);
    print_str("s (");
    print_dec(ticks);
    print_line(" ticks)");
}

fn cmd_uname() {
    print_line("AETERNA 2.0.0 x86_64 ospab.os");
}

fn print_str(s: &str) {
    for c in s.chars() {
        framebuffer::draw_char(c, COLOR_TEXT, COLOR_BG);
    }
}

fn print_dec(mut val: u64) {
    if val == 0 {
        framebuffer::draw_char('0', COLOR_TEXT, COLOR_BG);
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
        framebuffer::draw_char(buf[j] as char, COLOR_TEXT, COLOR_BG);
    }
}

fn print_size(bytes: u64) {
    if bytes >= 1024 * 1024 * 1024 {
        print_dec(bytes / (1024 * 1024 * 1024));
        print_str(" GiB");
    } else if bytes >= 1024 * 1024 {
        print_dec(bytes / (1024 * 1024));
        print_str(" MiB");
    } else if bytes >= 1024 {
        print_dec(bytes / 1024);
        print_str(" KiB");
    } else {
        print_dec(bytes);
        print_str(" B");
    }
}

fn cmd_reboot() {
    print_line("Rebooting...");
    unsafe {
        asm!("int 3", options(noreturn));
    }
}
