# AETERNA Microkernel — Copilot Instructions

## Overview
**ospab.os v2** (AETERNA) is a bare-metal microkernel operating system written in Rust (`#![no_std]`) for x86_64. It boots via Limine (BIOS+UEFI hybrid ISO), manages bare-metal hardware (GDT/IDT/PIC), provides a UNIX-like VFS layer, and embeds a DOOM port for testing.

---

## Architecture at a Glance

```
Terminal/Shell (plum)
    ↓
VFS + RamFS (in-memory files + persistent LBA 2048) + Syscall Layer
    ↓
Drivers (ATA PIO, AHCI SATA, PS/2, PCI, RTL8139)
    ↓
AETERNA Microkernel (Rust, no_std)
├─ x86_64 init: SSE, GDT, IDT, PIC
├─ Memory: linked_list_allocator (128 MiB heap)
├─ Display: Limine GOP framebuffer + 8×16 VGA font
├─ Input: PS/2 keyboard (IRQ 1, ring buffer)
├─ Timer: PIT IRQ 0 (100 Hz, 10ms ticks)
└─ Logging: klog (in-memory ring buffer) → serial (COM1) + framebuffer
```

**Key files:**
- **Kernel entry:** `src/main.rs` (5 boot phases)
- **Library root:** `src/lib.rs` (module re-exports)
- **Terminal:** `src/terminal.rs` (2200+ lines, shell commands)
- **VFS:** `src/fs/mod.rs`, `src/fs/ramfs.rs`, `src/fs/disk_sync.rs`
- **DOOM port:** `src/doom.rs` + `doom_engine/ospab_libc.c`
- **Architecture:** `arch/x86_64/{init.rs, gdt_simple.rs, idt.rs, pic.rs, serial.rs, framebuffer.rs, keyboard.rs}`

---

## Critical Workflows

### 1. **Build Process**
```bash
./build.sh              # Full build: cargo release + xorriso hybrid ISO
./build.sh kernel      # Kernel only (no ISO generation)
```
- **Output:** `isos/ospab-os-v2-${N}.iso` (auto-incrementing, xorriso hybrid)
- **Limine bins needed:** `tools/limine/bin/{BOOTX64.EFI, limine-bios.sys, limine-bios-cd.bin}`
- **Optional:** `bash doom_engine/build_doom.sh` rebuilds DOOM C code (requires doom1.wad in `src/`)

### 2. **Running & Debugging**
```bash
qemu-system-x86_64 -cdrom isos/ospab-os-v2-54.iso -serial stdio  # BIOS boot
qemu-system-x86_64 -cdrom isos/ospab-os-v2-54.iso -serial stdio -bios /path/to/OVMF.fd  # UEFI boot
```
- Serial output → COM1 (115200 baud) on stdout
- Framebuffer output → GUI window
- Press `Ctrl+L` in terminal for screen clear, `Ctrl+C` to break

### 3. **Checking Compiler Warnings**
```bash
cargo build --release 2>&1 | grep -E "warning:|error:"
```
- Most warnings suppressed via `#![allow(...)]` at module level (e.g., `src/xhci/mod.rs` for WIP USB driver)
- **No warnings policy:** ISO builds must have zero errors/warnings

---

## Rust & Project Conventions

### 1. **no_std + no_main**
- No `std` library (bare-metal)
- Entry point is `#[no_mangle] pub extern "C" fn _start() -> !` in `src/main.rs`
- Use `extern crate alloc` for collections (Vec, BTreeMap, String)
- Panic handler defined in `arch/x86_64/panic.rs` (logs to serial + halts)

### 2. **Modules & Visibility**
- `src/lib.rs` re-exports all major subsystems (arch, mm, fs, core, drivers, etc.)
- Path imports: `use ospab_os::fs` works from kernel code
- **Private by default:** Use `pub` only for cross-module APIs
- Userland modules are integrated as kernel modules (see `src/lib.rs`: `grape`, `tomato`, `seed`)

### 3. **Unsafe & FFI**
- FFI for C libc (DOOM): Functions declared in `src/doom.rs` via `#[no_mangle] pub extern "C"`
- C returns invoke wrapped Rust functions (e.g., `rust_malloc()` delegates to kernel allocator)
- **Safety invariant:** Kernel is single-core (no thread races for now), but use `AtomicBool` for flags like `CTRL_C`
- All unsafe use documented with `// SAFETY: ...` comments

---

## VFS + RamFS Architecture

### 1. **Mount Table & Path Resolution**
- **VFS layer** (`src/fs/mod.rs`): Single `/` root mount
- **RamFS** (`src/fs/ramfs.rs`): In-memory BTreeMap<String, RamNode>, spin-locked
- **Persistence:** LBA 2048 on disk (via AHCI), synced on demand or timer

```rust
// Open a file
let fd = fs::open("/doom/doomsav0.dsg", fs::OpenMode::ReadWrite)?;
fs::write_slice(fd, &data)?;
fs::close(fd)?;

// List directory
if let Some(entries) = fs::readdir("/doom")? {
    for entry in entries {
        println!("{} ({})", entry.name, entry.size);
    }
}
```

### 2. **Deferred Sync Pattern**
- **IS_DIRTY flag** (`src/fs/ramfs.rs`): Set on every write
- **Deferred timer:** `DEFERRED_SYNC_TICKS = 182` (10s at 18.2 Hz)
- **Sync trigger:** Called from terminal idle loop (`read_line()` → `hlt` → `deferred_tick()`)
- **Benefit:** DOOM saves appear instant (RAM-only), disk I/O happens quietly later
- **Manual sync:** User can run `sync` command or `sys_sync()` syscall forces immediate flush

```rust
// From src/fs/disk_sync.rs
pub fn deferred_tick() {         // Called ~18.2x per second
    if elapsed_since_last_dirty > DEFERRED_SYNC_TICKS {
        if is_dirty() {
            sync_filesystem();
        }
    }
}
```

### 3. **DOOM Save Integration**
- Files stored at `/doom/doomsav{0-5}.dsg` (RamFS)
- C libc intercepts `fopen("temp.dsg")`, `rename("temp.dsg" → "doomsav0.dsg")`
- **Key VFS functions exported to C:**
  - `rust_vfs_open()`, `rust_vfs_read()`, `rust_vfs_write()`, `rust_vfs_close()`, `rust_vfs_seek()`
  - `rust_vfs_rename()` (read → write → remove atomically)
  - `rust_vfs_access()` (check if file exists)
  - `rust_vfs_opendir()`, `rust_vfs_readdir_next()`, `rust_vfs_closedir()`
- **Debug logging:** `[DOOM_DEBUG]` prefix in serial output for `.dsg` operations

---

## Terminal Shell Commands

The terminal (`src/terminal.rs`) is the primary user interface. It supports:

### 1. **Command Structure**
- Prompt: `root@ospab:~# `
- Tab completion of builtins (grep `BUILTINS` array for full list)
- Command history (Up/Down arrows, 16-entry ring buffer)
- Input validation: no destructive backspace past prompt

### 2. **Key Built-in Commands**
```bash
ls [path]          # List directory (VFS readdir)
cat [path]         # Print file contents
echo [text]        # Print to console
mkdir [path]       # Create directory in RamFS
touch [path]       # Create empty file
rm [path]          # Delete file
sync               # Force filesystem sync
meminfo            # Show memory stats (PMM + heap)
uptime             # Seconds since boot
dmesg              # Kernel log ring buffer (100 entries)
reboot             # Immediate reboot (sync first if dirty)
doom               # Launch DOOM engine (C-only, WASM coming)
ping [-c N] <ip>   # ICMP echo (if network stack ready)
```

### 3. **Adding New Commands**
1. Add command name to `BUILTINS` array (for Tab completion)
2. Implement in `match cmd_name { ... }` block in `read_line()` / `run_command()`
3. Use `check_ctrl_c()` to allow user interrupt
4. Output via `framebuffer::draw_string(x, y, text, color)` or `serial::write_str()` for dual output

### 4. **Ctrl+C Handling**
- Set `CTRL_C` atomic flag on `\x03` key press
- Long-running commands should poll `check_ctrl_c()` periodically
- Terminal automatically clears flag after command returns

---

## Hardware Integration

### 1. **Serial Output (COM1)**
```rust
use ospab_os::arch::x86_64::serial;
serial::write_byte(ch);      // Write single char
serial::write_str(text);     // Write null-terminated string (Rust should enforce this)
```
- **115200 baud, 8N1** (fixed, set by bootloader)
- Used for kernel logs + debug output (always-on)

### 2. **Framebuffer Output**
```rust
use ospab_os::arch::x86_64::framebuffer;
framebuffer::put_pixel(x, y, color);                      // 32bpp BGRA
framebuffer::draw_char(x, y, ch, fg_color, bg_color);    // 8×16 font
framebuffer::draw_string(x, y, text, color);             // Multi-char
framebuffer::clear(color);                                // Fill screen
```
- **Resolution:** From Limine GOP (typically 1024×768)
- **Colors:** 32-bit BGRA (e.g., `0x00FF0000` = blue)
- **Font:** 8×16 VGA bitmap (96 glyphs, ASCII 32..127)

### 3. **Keyboard Input (PS/2)**
```rust
use ospab_os::arch::x86_64::keyboard;
if let Some(ch) = keyboard::poll_key() {    // Non-blocking
    // Process character (0..9, a..z, Enter, Backspace, Escape, etc.)
}
```
- **IRQ 1 handler:** Fills ring buffer, no polling needed for real input
- **Special keys:** `\n` (Enter), `\x08` (Backspace), `\x03` (Ctrl+C), `\x0C` (Ctrl+L)

### 4. **PIT Timer (IRQ 0)**
```rust
use ospab_os::arch::x86_64::idt;
let ticks = idt::timer_ticks();    // Monotonic, increments every ~10ms (100Hz)
```
- Hardwired at 18.2 Hz (Linux legacy compatible)
- Used for timestamp logging, deferred sync scheduling, uptime calculation

---

## Memory Management

### 1. **Allocator & Heap**
- **Allocator:** `linked_list_allocator` (simple, first-fit)
- **Heap size:** 128 MiB (hardcoded in `src/main.rs` boot phase)
- **HHDM (Higher Half Direct Map):** Physical memory mapped at fixed virtual offset (set by bootloader)

### 2. **Global Allocator**
```rust
extern crate alloc;
use alloc::vec::Vec;
use alloc::collections::BTreeMap;
fn example() {
    let v: Vec<u32> = Vec::new();     // Allocates from kernel heap
    v.push(42);                        // Works as usual, uses global allocator
}
```
- Kernel is single-core, no need for thread-safe allocator contention yet

---

## Logging & Debug Output

### 1. **Kernel Log (klog)**
```rust
use ospab_os::klog;
klog::boot("Message at boot");    // 100-entry ring buffer, displayed by `dmesg` command
klog::err("Error message");
klog::warn("Warning");
```
- Stored in circular buffer, accessible via terminal `dmesg` command
- Also emitted to serial + framebuffer during boot phase

### 2. **Serial Debug Logging**
```rust
serial::write_str("[NET] Starting NIC scan\r\n");
serial::write_str("[ATA] Drive detected\r\n");
```
- **Prefix convention:** `[SUBSYSTEM]` for log routing (NET, ATA, AHCI, XHCI, etc.)
- Always terminated with `\r\n` (DOS-style for serial)

---

## Common Patterns & Anti-Patterns

### ✅ **DO:**
1. **Use `spin::Mutex` for shared mutable state** (not `parking_lot` yet — no threads)
2. **Check `is_dirty()` before expensive I/O** — deferred sync pattern
3. **Log subsystem prefix** — `[NET]`, `[FS]`, `[DOOM]` for easy grep
4. **Graceful fallback** — missing WAD? DOOM prints error, doesn't panic
5. **Validate user input** — terminal commands should check path traversal, bounds, etc.

### ❌ **DON'T:**
1. **Call `sync_filesystem()` directly in write loops** — defeats deferred sync optimization
2. **Use `println!` or `std::io`** — no std library
3. **Panic in interrupt handlers** — always log + graceful recovery
4. **Assume thread-safety** — single-core for now, use `unsafe { static mut }` carefully
5. **Hard-code file paths** — use VFS trait for abstraction

---

## Working with DOOM C Engine

### 1. **Building DOOM**
```bash
cd doom_engine
bash build_doom.sh    # Invokes gcc → .o files, linked into kernel
```
- Requires: `doom1.wad` in `src/` (not in repo)
- Output: `doom_engine/doom.c.o` (static library)
- Linked at kernel build time via Cargo.toml `[build-script-build]`

### 2. **FFI Bridge Pattern**
```rust
// In src/doom.rs
#[no_mangle]
pub extern "C" fn rust_malloc(size: usize) -> *mut u8 {
    unsafe { alloc(Layout::from_size_align_unchecked(size, 1)) }
}

// In doom_engine/ospab_libc.c
extern void *rust_malloc(size_t size);
void *malloc(size_t size) {
    // ... alloc_header setup ...
    return (void *)(hdr + 1);
}
```
- C calls Rust via `extern` declarations in `.c` file
- Rust implements `#[no_mangle] pub extern "C"` functions
- **Type mapping:** `u8` → `uint8_t`, `usize` → `size_t`, etc.

### 3. **Debugging DOOM Issues**
- DOOM saves trigger `[DOOM_DEBUG]` serial log entries
- File operations (open, read, write, rename, unlink) are logged
- Use `qemu ... -serial stdio` to see real-time debug output

---

## Testing & Validation Checklist

When making changes:

1. ✅ **No compiler warnings:** `cargo build --release 2>&1 | grep -i warning`
2. ✅ **ISO builds:** `./build.sh` produces `isos/ospab-os-v2-N.iso`
3. ✅ **Boot sequence:** Verify all 5 phases succeed (hardware → memory → kernel → storage → terminal)
4. ✅ **Serial output:** Check that boot log has `[OK]` for each phase
5. ✅ **Terminal prompt:** Kernel reaches shell (green `#` prompt appears)
6. ✅ **DOOM integration (if modified):** Saves still appear in `/doom/`, rename works
7. ✅ **VFS operations:** `ls /`, `mkdir /test`, `cat /test/file` all work

---

## Key Dependencies & External Resources

| Component | Source | Version | Purpose |
|-----------|--------|---------|---------|
| **Limine** | `limine-10.8.2/` | 10.8.2 | Bootloader protocol + EFI loader |
| **linked_list_allocator** | Cargo | 0.10 | Kernel heap management |
| **spin** | Cargo | 0.10 | Spin-lock mutex (no-std) |
| **DOOM** | `doom_engine/` | Vanilla port | Test harness for WASI/graphics |
| **Rust nightly** | rustup | Latest | For x86_64-ospab target + asm! |

---

## Next Development Frontiers (from ROADMAP.md)

1. **Preemptive scheduler** — Replace `seed.rs` static list with TCB/PID HashMap, context-switch via IRQ 0
2. **Network stack** — e1000 driver, ARP, IPv4, ICMP (real `ping` command)
3. **Userland utilities** — `axon-ps`, `axon-top` invoking `sys_get_tasks()`
4. **Capability security** — Tokens for process resource access (GPU, network, filesystem subtrees)

---

## References

- **Tech Manifest:** `tech-manifest.md` (architecture vision + AI-native features)
- **Review:** `review.md` (detailed status of all subsystems)
- **ROADMAP:** `ROADMAP.md` (phased development plan)
- **Boot log:** Run `dmesg` in terminal to see kernel initialization log
