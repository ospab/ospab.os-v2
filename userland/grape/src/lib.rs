/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

grape — Terminal text editor for ospab.os (AETERNA)

A nano-like text editor that runs in the kernel framebuffer console.
Full-screen editing with status bar, keyboard shortcuts, and VFS integration.

Keyboard shortcuts (nano-compatible):
  Ctrl+X  — Exit (prompts to save if modified)
  Ctrl+O  — Write Out (save file)
  Ctrl+K  — Cut current line
  Ctrl+U  — Uncut (paste last cut line)
  Ctrl+G  — Help screen
  Ctrl+W  — Search (Where Is)
  Ctrl+C  — Show cursor position
  Ctrl+T  — Go to line number

  Arrow keys — Move cursor
  Home/End   — Beginning/End of line
  PgUp/PgDn  — Scroll page
  Backspace  — Delete char before cursor
  Delete     — Delete char at cursor
*/

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

// ─── Editor state ───────────────────────────────────────────────────────────

/// Maximum number of lines in a file
const MAX_LINES: usize = 4096;

/// A single line of text
struct Line {
    /// Character data (no trailing newline stored)
    chars: Vec<u8>,
}

impl Line {
    fn new() -> Self {
        Self { chars: Vec::new() }
    }
    fn from_bytes(data: &[u8]) -> Self {
        Self { chars: Vec::from(data) }
    }
}

/// The editor state
struct EditorState {
    /// All lines in the buffer
    lines: Vec<Line>,
    /// Cursor X position (column in current line)
    cx: usize,
    /// Cursor Y position (line index in buffer)
    cy: usize,
    /// First visible line (vertical scroll offset)
    row_off: usize,
    /// First visible column (horizontal scroll offset)
    col_off: usize,
    /// Screen dimensions in characters
    screen_rows: usize,
    screen_cols: usize,
    /// Filename being edited (empty for new file)
    filename: String,
    /// Has the buffer been modified?
    dirty: bool,
    /// Status message (shown in status bar)
    status_msg: String,
    /// Cut buffer (for Ctrl+K / Ctrl+U)
    cut_buffer: Vec<u8>,
    /// Search query
    search_query: String,
    /// Should the editor exit?
    quit: bool,
}

// ─── Colors ─────────────────────────────────────────────────────────────────

const FG_TEXT: u32      = 0x00FFFFFF;  // White — normal text
const FG_STATUS: u32    = 0x00000000;  // Black text on status bar
const BG_STATUS: u32    = 0x00FFFFFF;  // White background for status bar
const FG_HELP: u32      = 0x00000000;  // Black text on help bar
const BG_HELP: u32      = 0x00AAAAAA;  // Gray background for help bar
const FG_SHORTCUT: u32  = 0x00FFFFFF;  // White for shortcut keys on help bar
const BG_SHORTCUT: u32  = 0x00555555;  // Dark gray bg for shortcut highlight
const FG_LINE_NUM: u32  = 0x00888888;  // Gray for line numbers
const FG_TITLE: u32     = 0x00FFFFFF;  // Title bar text
const BG_TITLE: u32     = 0x00005500;  // Dark green title bar
const BG: u32           = 0x00000000;  // Normal background

// ─── Framebuffer + keyboard imports ─────────────────────────────────────────

use crate::arch::x86_64::framebuffer;
use crate::arch::x86_64::keyboard;

fn fb_cols() -> usize { framebuffer::screen_cols() as usize }
fn fb_rows() -> usize { framebuffer::screen_rows() as usize }

/// Draw a character at a specific cell (col, row)
fn draw_at(col: usize, row: usize, ch: char, fg: u32, bg: u32) {
    let x = (col as u64) * framebuffer::CHAR_WIDTH;
    let y = (row as u64) * framebuffer::CHAR_HEIGHT;
    framebuffer::draw_char_at(x, y, ch, fg, bg);
}

/// Draw a string starting at cell (col, row), clipping at screen edge
fn draw_str_at(col: usize, row: usize, s: &str, fg: u32, bg: u32) {
    let max_col = fb_cols();
    let mut c = col;
    for ch in s.bytes() {
        if c >= max_col { break; }
        draw_at(c, row, ch as char, fg, bg);
        c += 1;
    }
}

/// Fill a row from col to end with spaces
fn clear_row(row: usize, from_col: usize, fg: u32, bg: u32) {
    let max_col = fb_cols();
    for c in from_col..max_col {
        draw_at(c, row, ' ', fg, bg);
    }
}

/// Fill an entire row with a background color
fn fill_row(row: usize, fg: u32, bg: u32) {
    clear_row(row, 0, fg, bg);
}

// ─── Public entry point ─────────────────────────────────────────────────────

/// Open grape editor on a file. If path is empty, opens a new empty buffer.
/// Returns when the user exits (Ctrl+X).
pub fn edit(path: &str) {
    let mut state = EditorState {
        lines: Vec::new(),
        cx: 0,
        cy: 0,
        row_off: 0,
        col_off: 0,
        screen_rows: fb_rows().saturating_sub(3), // 1 title + 1 status + 1 help
        screen_cols: fb_cols(),
        filename: String::from(path),
        dirty: false,
        status_msg: String::new(),
        cut_buffer: Vec::new(),
        search_query: String::new(),
        quit: false,
    };

    // Load file from VFS if it exists
    if !path.is_empty() {
        if let Some(data) = crate::fs::read_file(path) {
            load_buffer(&mut state, &data);
            state.status_msg = format_status("Read {} bytes", data.len());
        } else {
            // New file
            state.lines.push(Line::new());
            state.status_msg = String::from("[ New File ]");
        }
    } else {
        state.lines.push(Line::new());
        state.status_msg = String::from("[ New Buffer ]");
    }

    // Main editor loop
    framebuffer::clear(BG);
    loop {
        refresh_screen(&state);
        let key = wait_key();
        process_key(&mut state, key);
        if state.quit {
            break;
        }
    }

    // Restore terminal: clear and reset cursor
    framebuffer::clear(BG);
    framebuffer::set_cursor_pos(0, 0);
}

// ─── Key handling ───────────────────────────────────────────────────────────

/// Read one keypress (blocking)
fn wait_key() -> char {
    loop {
        if let Some(ch) = keyboard::try_read_key() {
            return ch;
        }
        unsafe { core::arch::asm!("hlt"); }
    }
}

/// Process a single keypress
fn process_key(state: &mut EditorState, key: char) {
    match key {
        // ── Ctrl+X: Exit ──
        '\x18' => {
            if state.dirty {
                state.status_msg = String::from("Modified buffer! Ctrl+X again to discard, Ctrl+O to save");
                state.dirty = false; // Allow second Ctrl+X to quit
                return;
            }
            state.quit = true;
        }

        // ── Ctrl+O: Write Out (Save) ──
        '\x0F' => {
            if state.filename.is_empty() {
                // Prompt for filename
                if let Some(name) = prompt(state, "File Name to Write: ") {
                    if !name.is_empty() {
                        state.filename = name;
                    }
                }
            }
            if !state.filename.is_empty() {
                save_file(state);
            }
        }

        // ── Ctrl+K: Cut line ──
        '\x0B' => {
            if state.cy < state.lines.len() {
                state.cut_buffer = state.lines[state.cy].chars.clone();
                state.lines.remove(state.cy);
                if state.lines.is_empty() {
                    state.lines.push(Line::new());
                }
                if state.cy >= state.lines.len() {
                    state.cy = state.lines.len() - 1;
                }
                clamp_cx(state);
                state.dirty = true;
            }
        }

        // ── Ctrl+U: Uncut (paste) ──
        '\x15' => {
            if !state.cut_buffer.is_empty() {
                let line = Line::from_bytes(&state.cut_buffer);
                state.lines.insert(state.cy, line);
                state.dirty = true;
            }
        }

        // ── Ctrl+G: Help ──
        '\x07' => {
            show_help(state);
        }

        // ── Ctrl+W: Search (Where Is) ──
        '\x17' => {
            if let Some(query) = prompt(state, "Search: ") {
                if !query.is_empty() {
                    state.search_query = query;
                    search_forward(state);
                }
            }
        }

        // ── Ctrl+T: Go To Line ──
        '\x14' => {
            if let Some(num_str) = prompt(state, "Enter line number: ") {
                if let Some(n) = parse_usize(&num_str) {
                    if n > 0 && n <= state.lines.len() {
                        state.cy = n - 1;
                        state.cx = 0;
                        clamp_scroll(state);
                    }
                }
            }
        }

        // ── Ctrl+C: Show position ──
        '\x03' => {
            let line = state.cy + 1;
            let col = state.cx + 1;
            let total = state.lines.len();
            state.status_msg = format_pos(line, col, total);
        }

        // ── Arrow keys ──
        k if k == keyboard::KEY_UP    => move_up(state),
        k if k == keyboard::KEY_DOWN  => move_down(state),
        k if k == keyboard::KEY_LEFT  => move_left(state),
        k if k == keyboard::KEY_RIGHT => move_right(state),

        // ── Home / End ──
        k if k == keyboard::KEY_HOME => { state.cx = 0; }
        k if k == keyboard::KEY_END  => {
            if state.cy < state.lines.len() {
                state.cx = state.lines[state.cy].chars.len();
            }
        }

        // ── Page Up / Page Down ──
        k if k == keyboard::KEY_PGUP => {
            for _ in 0..state.screen_rows {
                move_up(state);
            }
        }
        k if k == keyboard::KEY_PGDN => {
            for _ in 0..state.screen_rows {
                move_down(state);
            }
        }

        // ── Delete ──
        k if k == keyboard::KEY_DELETE => {
            delete_char(state);
        }

        // ── Backspace ──
        '\x08' => {
            if state.cx > 0 {
                move_left(state);
                delete_char(state);
            } else if state.cy > 0 {
                // Join with previous line
                let current_data = state.lines[state.cy].chars.clone();
                state.cy -= 1;
                state.cx = state.lines[state.cy].chars.len();
                state.lines[state.cy].chars.extend_from_slice(&current_data);
                state.lines.remove(state.cy + 1);
                state.dirty = true;
            }
        }

        // ── Enter ──
        '\n' => {
            insert_newline(state);
        }

        // ── Tab ──
        '\t' => {
            // Insert 4 spaces
            for _ in 0..4 {
                insert_char(state, b' ');
            }
        }

        // ── Escape — ignore ──
        '\x1B' => {}

        // ── Ctrl+L — redraw ──
        '\x0C' => {
            framebuffer::clear(BG);
        }

        // ── Regular printable characters ──
        c if c.is_ascii() && (c as u8) >= 0x20 => {
            insert_char(state, c as u8);
        }

        _ => {}
    }
}

// ─── Cursor movement ────────────────────────────────────────────────────────

fn move_up(state: &mut EditorState) {
    if state.cy > 0 {
        state.cy -= 1;
        clamp_cx(state);
    }
    clamp_scroll(state);
}

fn move_down(state: &mut EditorState) {
    if state.cy < state.lines.len().saturating_sub(1) {
        state.cy += 1;
        clamp_cx(state);
    }
    clamp_scroll(state);
}

fn move_left(state: &mut EditorState) {
    if state.cx > 0 {
        state.cx -= 1;
    } else if state.cy > 0 {
        state.cy -= 1;
        state.cx = state.lines[state.cy].chars.len();
    }
    clamp_scroll(state);
}

fn move_right(state: &mut EditorState) {
    if state.cy < state.lines.len() {
        let line_len = state.lines[state.cy].chars.len();
        if state.cx < line_len {
            state.cx += 1;
        } else if state.cy < state.lines.len() - 1 {
            state.cy += 1;
            state.cx = 0;
        }
    }
    clamp_scroll(state);
}

fn clamp_cx(state: &mut EditorState) {
    if state.cy < state.lines.len() {
        let max = state.lines[state.cy].chars.len();
        if state.cx > max {
            state.cx = max;
        }
    }
}

fn clamp_scroll(state: &mut EditorState) {
    // Vertical scrolling
    if state.cy < state.row_off {
        state.row_off = state.cy;
    }
    if state.cy >= state.row_off + state.screen_rows {
        state.row_off = state.cy - state.screen_rows + 1;
    }
    // Horizontal scrolling
    if state.cx < state.col_off {
        state.col_off = state.cx;
    }
    if state.cx >= state.col_off + state.screen_cols {
        state.col_off = state.cx - state.screen_cols + 1;
    }
}

// ─── Text editing operations ────────────────────────────────────────────────

fn insert_char(state: &mut EditorState, ch: u8) {
    if state.cy >= state.lines.len() {
        state.lines.push(Line::new());
    }
    let line = &mut state.lines[state.cy];
    if state.cx > line.chars.len() {
        state.cx = line.chars.len();
    }
    line.chars.insert(state.cx, ch);
    state.cx += 1;
    state.dirty = true;
    clamp_scroll(state);
}

fn delete_char(state: &mut EditorState) {
    if state.cy >= state.lines.len() { return; }
    let line_len = state.lines[state.cy].chars.len();
    if state.cx < line_len {
        state.lines[state.cy].chars.remove(state.cx);
        state.dirty = true;
    } else if state.cy + 1 < state.lines.len() {
        // Join with next line
        let next_data = state.lines[state.cy + 1].chars.clone();
        state.lines[state.cy].chars.extend_from_slice(&next_data);
        state.lines.remove(state.cy + 1);
        state.dirty = true;
    }
}

fn insert_newline(state: &mut EditorState) {
    if state.cy >= state.lines.len() {
        state.lines.push(Line::new());
        state.cy += 1;
        state.cx = 0;
    } else {
        let line = &state.lines[state.cy];
        let tail = if state.cx < line.chars.len() {
            Vec::from(&line.chars[state.cx..])
        } else {
            Vec::new()
        };
        state.lines[state.cy].chars.truncate(state.cx);
        state.cy += 1;
        state.lines.insert(state.cy, Line::from_bytes(&tail));
        state.cx = 0;
    }
    state.dirty = true;
    clamp_scroll(state);
}

// ─── File I/O ───────────────────────────────────────────────────────────────

fn load_buffer(state: &mut EditorState, data: &[u8]) {
    state.lines.clear();

    let mut start = 0;
    for i in 0..data.len() {
        if data[i] == b'\n' {
            state.lines.push(Line::from_bytes(&data[start..i]));
            start = i + 1;
        }
    }
    // Last line (may not end with \n)
    if start <= data.len() {
        state.lines.push(Line::from_bytes(&data[start..]));
    }
    if state.lines.is_empty() {
        state.lines.push(Line::new());
    }
}

fn save_file(state: &mut EditorState) {
    // Serialize buffer to bytes
    let mut data: Vec<u8> = Vec::new();
    for (i, line) in state.lines.iter().enumerate() {
        data.extend_from_slice(&line.chars);
        if i + 1 < state.lines.len() {
            data.push(b'\n');
        }
    }

    let path = state.filename.as_str();
    // Ensure parent directory exists
    if let Some(last_slash) = path.rfind('/') {
        if last_slash > 0 {
            let parent = &path[..last_slash];
            crate::fs::mkdir(parent);
        }
    }

    if crate::fs::write_file(path, &data) {
        state.dirty = false;
        state.status_msg = format_status("Wrote {} bytes to ", data.len());
        state.status_msg.push_str(path);
    } else {
        state.status_msg = String::from("[ Error writing file! ]");
    }
}

// ─── Search ─────────────────────────────────────────────────────────────────

fn search_forward(state: &mut EditorState) {
    let query = state.search_query.as_bytes();
    if query.is_empty() { return; }

    // Search from current position forward
    let start_line = state.cy;
    let start_col = state.cx + 1;

    for offset in 0..state.lines.len() {
        let line_idx = (start_line + offset) % state.lines.len();
        let line = &state.lines[line_idx].chars;
        let search_start = if offset == 0 { start_col } else { 0 };

        if search_start < line.len() {
            if let Some(pos) = find_in_slice(&line[search_start..], query) {
                state.cy = line_idx;
                state.cx = search_start + pos;
                clamp_scroll(state);
                state.status_msg = String::from("Found");
                return;
            }
        }
    }

    state.status_msg = String::from("[ Not Found ]");
}

/// Simple substring search
fn find_in_slice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    for i in 0..=(haystack.len() - needle.len()) {
        if &haystack[i..i + needle.len()] == needle {
            return Some(i);
        }
    }
    None
}

// ─── Screen rendering ───────────────────────────────────────────────────────

fn refresh_screen(state: &EditorState) {
    let rows = fb_rows();
    let cols = fb_cols();

    // Row 0: title bar
    draw_title_bar(state, cols);

    // Rows 1..screen_rows+1: text content
    for screen_row in 0..state.screen_rows {
        let file_row = state.row_off + screen_row;
        let draw_row = screen_row + 1; // +1 for title bar

        if file_row < state.lines.len() {
            let line = &state.lines[file_row];
            let mut col = 0;
            let line_start = state.col_off;

            // Draw visible portion of line
            for i in line_start..line.chars.len() {
                if col >= cols { break; }
                draw_at(col, draw_row, line.chars[i] as char, FG_TEXT, BG);
                col += 1;
            }
            // Clear rest of row
            clear_row(draw_row, col, FG_TEXT, BG);
        } else {
            // Empty line — show tilde like nano
            draw_at(0, draw_row, ' ', FG_LINE_NUM, BG);
            clear_row(draw_row, 1, FG_TEXT, BG);
        }
    }

    // Status bar (2nd to last row)
    let status_row = rows - 2;
    draw_status_bar(state, status_row, cols);

    // Help bar (last row)
    let help_row = rows - 1;
    draw_help_bar(help_row, cols);

    // Position physical cursor
    let cursor_screen_y = (state.cy - state.row_off + 1) as u64;
    let cursor_screen_x = (state.cx - state.col_off) as u64;
    let px = cursor_screen_x * framebuffer::CHAR_WIDTH;
    let py = cursor_screen_y * framebuffer::CHAR_HEIGHT;

    // Draw cursor block (inverted)
    if state.cy < state.lines.len() {
        let line = &state.lines[state.cy];
        let ch = if state.cx < line.chars.len() {
            line.chars[state.cx] as char
        } else {
            ' '
        };
        draw_at(
            (state.cx - state.col_off),
            (state.cy - state.row_off + 1),
            ch,
            BG, // Inverted: black text
            FG_TEXT, // on white background
        );
    }
}

fn draw_title_bar(state: &EditorState, cols: usize) {
    // Title: "  grape  filename  [Modified]"
    fill_row(0, FG_TITLE, BG_TITLE);
    draw_str_at(1, 0, "grape", FG_TITLE, BG_TITLE);
    draw_str_at(7, 0, " - ", FG_TITLE, BG_TITLE);

    if state.filename.is_empty() {
        draw_str_at(10, 0, "[New Buffer]", FG_TITLE, BG_TITLE);
    } else {
        draw_str_at(10, 0, &state.filename, FG_TITLE, BG_TITLE);
    }

    if state.dirty {
        let offset = 10 + if state.filename.is_empty() { 12 } else { state.filename.len() };
        draw_str_at(offset + 1, 0, "[Modified]", 0x0000FFFF, BG_TITLE);
    }

    // Right side: line count
    let info = format_line_info(state.cy + 1, state.lines.len());
    let info_col = if cols > info.len() + 2 { cols - info.len() - 2 } else { 0 };
    draw_str_at(info_col, 0, &info, FG_TITLE, BG_TITLE);
}

fn draw_status_bar(state: &EditorState, row: usize, cols: usize) {
    fill_row(row, FG_STATUS, BG_STATUS);
    if !state.status_msg.is_empty() {
        draw_str_at(1, row, &state.status_msg, FG_STATUS, BG_STATUS);
    } else {
        let default_msg = if state.dirty { "Modified" } else { "Ready" };
        draw_str_at(1, row, default_msg, FG_STATUS, BG_STATUS);
    }
}

fn draw_help_bar(row: usize, cols: usize) {
    fill_row(row, FG_HELP, BG_HELP);
    // nano-style shortcut bar
    let shortcuts = [
        ("^X", "Exit"),
        ("^O", "Save"),
        ("^K", "Cut"),
        ("^U", "Paste"),
        ("^W", "Search"),
        ("^G", "Help"),
    ];

    let mut col = 0;
    for (key, desc) in shortcuts.iter() {
        if col + key.len() + desc.len() + 2 >= cols { break; }
        draw_str_at(col, row, key, FG_SHORTCUT, BG_SHORTCUT);
        col += key.len();
        draw_str_at(col, row, " ", FG_HELP, BG_HELP);
        col += 1;
        draw_str_at(col, row, desc, FG_HELP, BG_HELP);
        col += desc.len() + 2;
    }
}

// ─── Mini-prompt (for save filename, search, go-to-line) ────────────────────

/// Display a prompt in the status bar and read a line of input.
/// Returns None if the user pressed Escape/Ctrl+C.
fn prompt(state: &mut EditorState, msg: &str) -> Option<String> {
    let mut input = String::new();
    let rows = fb_rows();
    let cols = fb_cols();
    let status_row = rows - 2;

    loop {
        // Draw prompt
        fill_row(status_row, FG_STATUS, BG_STATUS);
        draw_str_at(1, status_row, msg, FG_STATUS, BG_STATUS);
        draw_str_at(1 + msg.len(), status_row, &input, FG_STATUS, BG_STATUS);

        // Draw cursor
        let cursor_col = 1 + msg.len() + input.len();
        if cursor_col < cols {
            draw_at(cursor_col, status_row, '_', FG_STATUS, BG_STATUS);
        }

        let key = wait_key();
        match key {
            '\n' => return Some(input),
            '\x1B' | '\x03' => return None, // Escape or Ctrl+C: cancel
            '\x08' => { input.pop(); }
            c if c.is_ascii() && (c as u8) >= 0x20 => {
                if input.len() < 200 {
                    input.push(c);
                }
            }
            _ => {}
        }
    }
}

// ─── Help screen ────────────────────────────────────────────────────────────

fn show_help(state: &mut EditorState) {
    framebuffer::clear(BG);
    let cols = fb_cols();

    fill_row(0, FG_TITLE, BG_TITLE);
    draw_str_at(2, 0, "grape Help", FG_TITLE, BG_TITLE);

    let help_lines = [
        "",
        "  grape is the text editor for ospab.os (AETERNA).",
        "  Keyboard shortcuts (nano-compatible):",
        "",
        "  Ctrl+X      Exit editor (prompts to save if modified)",
        "  Ctrl+O      Write Out — save current file",
        "  Ctrl+K      Cut — remove current line to cut buffer",
        "  Ctrl+U      Uncut — paste cut buffer at cursor",
        "  Ctrl+W      Where Is — search for text",
        "  Ctrl+T      Go To Line — jump to line number",
        "  Ctrl+C      Cursor Pos — show current line/column",
        "  Ctrl+G      Help — this screen",
        "  Ctrl+L      Refresh — redraw screen",
        "",
        "  Up/Down     Move cursor up/down",
        "  Left/Right  Move cursor left/right",
        "  Home/End    Beginning/End of line",
        "  PgUp/PgDn   Scroll one page up/down",
        "  Backspace   Delete character before cursor",
        "  Delete      Delete character at cursor",
        "  Enter       Insert new line",
        "  Tab         Insert 4 spaces",
        "",
        "  Press any key to return to editing...",
    ];

    for (i, line) in help_lines.iter().enumerate() {
        if i + 1 >= fb_rows() { break; }
        draw_str_at(0, i + 1, line, FG_TEXT, BG);
        clear_row(i + 1, line.len(), FG_TEXT, BG);
    }

    wait_key();
    framebuffer::clear(BG);
}

// ─── Formatting helpers (no format! macro in no_std) ────────────────────────

fn format_status(msg: &str, num: usize) -> String {
    let mut s = String::from(msg);
    // Replace {} with number
    if let Some(pos) = s.find("{}") {
        let num_s = usize_to_string(num);
        s.replace_range(pos..pos + 2, &num_s);
    } else {
        s.push_str(&usize_to_string(num));
    }
    s
}

fn format_pos(line: usize, col: usize, total: usize) -> String {
    let mut s = String::from("line ");
    s.push_str(&usize_to_string(line));
    s.push_str("/");
    s.push_str(&usize_to_string(total));
    s.push_str(", col ");
    s.push_str(&usize_to_string(col));
    s
}

fn format_line_info(current: usize, total: usize) -> String {
    let mut s = String::from("L");
    s.push_str(&usize_to_string(current));
    s.push_str("/");
    s.push_str(&usize_to_string(total));
    s
}

fn usize_to_string(mut n: usize) -> String {
    if n == 0 {
        return String::from("0");
    }
    let mut buf = [0u8; 20];
    let mut pos = 20;
    while n > 0 {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    String::from(core::str::from_utf8(&buf[pos..]).unwrap_or("0"))
}

fn parse_usize(s: &str) -> Option<usize> {
    let mut result: usize = 0;
    for b in s.bytes() {
        if b < b'0' || b > b'9' { return None; }
        result = result.checked_mul(10)?.checked_add((b - b'0') as usize)?;
    }
    Some(result)
}
