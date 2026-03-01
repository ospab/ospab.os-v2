/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab
Framebuffer output: pixel-level control + 8x16 VGA text rendering.
Supports scrolling, cursor tracking, and optimized row copy.
*/

use core::ptr;
use super::font;

/// Framebuffer hardware info
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Framebuffer {
    pub address: *mut u32,
    pub width: u64,
    pub height: u64,
    pub pitch: u64,   // bytes per scanline
    pub bpp: u16,
}

static mut FRAMEBUFFER: Option<Framebuffer> = None;

// Text cursor in pixel coordinates
static mut CURSOR_X: u64 = 0;
static mut CURSOR_Y: u64 = 0;

/// Character cell dimensions (8x16 VGA font)
pub const CHAR_WIDTH: u64 = font::FONT_WIDTH;
pub const CHAR_HEIGHT: u64 = font::FONT_HEIGHT;

/// Initialize framebuffer with raw parameters from bootloader
pub unsafe fn init(address: *mut u32, width: u64, height: u64, pitch: u64, bpp: u16) {
    FRAMEBUFFER = Some(Framebuffer {
        address,
        width,
        height,
        pitch,
        bpp,
    });
    CURSOR_X = 0;
    CURSOR_Y = 0;
}

/// Check if framebuffer is ready
pub fn is_initialized() -> bool {
    unsafe { FRAMEBUFFER.is_some() }
}

/// Plot single pixel at (x, y) with 32-bit color (0x00RRGGBB or BGR depending on mode)
#[inline(always)]
pub fn put_pixel(x: u64, y: u64, color: u32) {
    unsafe {
        if let Some(fb) = FRAMEBUFFER {
            if x < fb.width && y < fb.height {
                let offset = y * (fb.pitch / 4) + x;
                ptr::write_volatile(fb.address.add(offset as usize), color);
            }
        }
    }
}

/// Clear entire screen with solid color
pub fn clear(color: u32) {
    unsafe {
        if let Some(fb) = FRAMEBUFFER {
            let pixels_per_row = fb.pitch / 4;
            for y in 0..fb.height {
                let row_base = fb.address.add((y * pixels_per_row) as usize);
                for x in 0..fb.width {
                    ptr::write_volatile(row_base.add(x as usize), color);
                }
            }
            CURSOR_X = 0;
            CURSOR_Y = 0;
        }
    }
}

/// Draw filled rectangle
pub fn fill_rect(x: u64, y: u64, w: u64, h: u64, color: u32) {
    unsafe {
        if let Some(fb) = FRAMEBUFFER {
            let pixels_per_row = fb.pitch / 4;
            let x_end = (x + w).min(fb.width);
            let y_end = (y + h).min(fb.height);
            for py in y..y_end {
                let row_base = fb.address.add((py * pixels_per_row) as usize);
                for px in x..x_end {
                    ptr::write_volatile(row_base.add(px as usize), color);
                }
            }
        }
    }
}

/// Draw single character at pixel position (8x16 font, 1 byte per row, MSB left)
pub fn draw_char_at(x: u64, y: u64, c: char, fg_color: u32, bg_color: u32) {
    if let Some(bitmap) = font::get_char_bitmap(c) {
        for row in 0..16u64 {
            let row_data = bitmap[row as usize];
            for col in 0..8u64 {
                let pixel_x = x + col;
                let pixel_y = y + row;
                let color = if (row_data >> (7 - col)) & 1 == 1 {
                    fg_color
                } else {
                    bg_color
                };
                put_pixel(pixel_x, pixel_y, color);
            }
        }
    }
}

/// Draw character at current cursor, handle newline/CR, auto-wrap, auto-scroll
pub fn draw_char(c: char, fg_color: u32, bg_color: u32) {
    unsafe {
        if let Some(fb) = FRAMEBUFFER {
            match c {
                '\n' => {
                    CURSOR_X = 0;
                    CURSOR_Y += CHAR_HEIGHT;
                    // Scroll if cursor went past bottom
                    if CURSOR_Y + CHAR_HEIGHT > fb.height {
                        scroll_up();
                        CURSOR_Y = fb.height - CHAR_HEIGHT;
                    }
                }
                '\r' => {
                    CURSOR_X = 0;
                }
                _ => {
                    // Wrap if at right edge
                    if CURSOR_X + CHAR_WIDTH > fb.width {
                        CURSOR_X = 0;
                        CURSOR_Y += CHAR_HEIGHT;
                    }
                    // Scroll if at bottom
                    if CURSOR_Y + CHAR_HEIGHT > fb.height {
                        scroll_up();
                        CURSOR_Y = fb.height - CHAR_HEIGHT;
                    }
                    draw_char_at(CURSOR_X, CURSOR_Y, c, fg_color, bg_color);
                    CURSOR_X += CHAR_WIDTH;
                }
            }
        }
    }
}

/// Draw string at current cursor position
pub fn draw_string(s: &str, fg_color: u32, bg_color: u32) {
    for c in s.chars() {
        draw_char(c, fg_color, bg_color);
    }
}

/// Draw string starting at specific pixel position
pub fn draw_string_at(x: u64, y: u64, s: &str, fg_color: u32, bg_color: u32) {
    unsafe {
        CURSOR_X = x;
        CURSOR_Y = y;
    }
    draw_string(s, fg_color, bg_color);
}

/// Scroll screen content up by one text row (CHAR_HEIGHT pixels).
/// Uses optimized row-based copy via pitch arithmetic.
fn scroll_up() {
    unsafe {
        if let Some(fb) = FRAMEBUFFER {
            let pixels_per_row = fb.pitch / 4;

            // Copy rows upward (source row y -> dest row y - CHAR_HEIGHT)
            for y in CHAR_HEIGHT..fb.height {
                let src = fb.address.add((y * pixels_per_row) as usize);
                let dst = fb.address.add(((y - CHAR_HEIGHT) * pixels_per_row) as usize);
                // Copy full scanline (width pixels * 4 bytes each)
                ptr::copy(src, dst, fb.width as usize);
            }

            // Clear the bottom CHAR_HEIGHT rows
            let clear_start = fb.height - CHAR_HEIGHT;
            for y in clear_start..fb.height {
                let row_base = fb.address.add((y * pixels_per_row) as usize);
                for x in 0..fb.width {
                    ptr::write_volatile(row_base.add(x as usize), 0x00000000);
                }
            }
        }
    }
}

/// Get current cursor position in pixels
pub fn cursor_pos() -> (u64, u64) {
    unsafe { (CURSOR_X, CURSOR_Y) }
}

/// Set cursor position in pixels
pub fn set_cursor_pos(x: u64, y: u64) {
    unsafe {
        CURSOR_X = x;
        CURSOR_Y = y;
    }
}

/// Get framebuffer info (dimensions, address, etc.)
pub fn info() -> Option<Framebuffer> {
    unsafe { FRAMEBUFFER }
}

/// Get screen dimensions in character cells
pub fn screen_cols() -> u64 {
    unsafe {
        if let Some(fb) = FRAMEBUFFER {
            fb.width / CHAR_WIDTH
        } else {
            80
        }
    }
}

pub fn screen_rows() -> u64 {
    unsafe {
        if let Some(fb) = FRAMEBUFFER {
            fb.height / CHAR_HEIGHT
        } else {
            25
        }
    }
}
