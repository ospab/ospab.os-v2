# ospab.os v2 — AETERNA Microkernel Operating System

> **Version 2.1.0** | **Kernel: AETERNA** | **Architecture: x86_64** | **Language: Rust (no_std)**

## About

**ospab.os** is a microkernel-based operating system written entirely in Rust. The core — the **AETERNA** microkernel — provides deterministic scheduling, capability-based security, and AI-native computing primitives. The system boots from a Live ISO via the Limine protocol and runs in Long Mode (x86_64).

> **Naming:** The project name **ospab** is always written in lowercase. The kernel is **AETERNA**.

---

## System Components

### Kernel: AETERNA
The microkernel handles hardware initialization (GDT, IDT, PIC, SSE/FPU), physical/virtual memory management, a Compute-First scheduler, and syscall dispatch via MSRs. It boots through the Limine protocol and renders output via a UEFI framebuffer (8×16 VGA font).

### Virtual Filesystem (VFS + RamFS)
A fully functional POSIX-compatible virtual filesystem. RamFS provides an in-memory filesystem mounted at `/` with standard directories (`/etc`, `/proc`, `/dev`, `/home`, `/tmp`, `/sys`, `/boot`, `/var`). All file operations (read, write, mkdir, touch, rm) are real — no stubs.

### Network Stack
- **RTL8139** PCI NIC driver with DMA ring buffer TX/RX
- **Ethernet** frame handling
- **ARP** request/reply with MAC resolution
- **IPv4** send/receive with checksum
- **ICMP** ping (echo request/reply)
- **UDP** + **SNTP** time synchronization

### Storage Drivers
- **ATA PIO** — IDE hard drive access
- **AHCI SATA** — modern SATA controllers via PCI BAR5

---

## Userland Tools

All userland tools are written in Rust and currently run as kernel-integrated modules (userspace process isolation is on the roadmap).

### grape — Text Editor
A full-screen **nano-like** text editor with VFS integration.

- Full-screen editing with title bar, status bar, and shortcut help bar
- Keyboard shortcuts: `Ctrl+X` exit, `Ctrl+O` save, `Ctrl+K` cut, `Ctrl+U` paste, `Ctrl+W` search, `Ctrl+G` help, `Ctrl+T` go-to-line
- Arrow keys, Home/End, PgUp/PgDn navigation
- Horizontal/vertical scrolling for large files
- Search forward with wrap-around
- Block cursor (inverted colors)
- Creates parent directories on save
- Opens files from VFS, saves back to VFS

### tomato — Package Manager
A **pacman-inspired** package manager with local binary package support.

- `-S <pkg>` — Install a package (with dependency resolution)
- `-R <pkg>` — Remove a package (cleans up files)
- `-Q` — List installed packages
- `-Qi <pkg>` — Show detailed package info
- `-Ss <query>` — Search available packages (case-insensitive)
- `-Sy` — Sync package database
- `-Syu` — Full system upgrade
- Built-in repository with 10 packages: `base`, `coreutils`, `grape`, `plum`, `net-tools`, `tutor`, `kernel-headers`, `ospab-libc`, `man-pages`, `seed`
- Tracks installed packages in `/var/lib/tomato/local/`
- Creates stub files in VFS on install

### plum — Command Shell
A POSIX-inspired shell with **bash script support**:

- **Environment variables**: `$VAR`, `${VAR}`, `$?` (last exit code)
- **`export VAR=value`** — set/show environment variables
- **`alias name=command`** — command aliases (`ll`, `la`, `cls`, `edit`, etc.)
- **`unalias`**, **`unset`**, **`set`**, **`env`** — variable management
- **`type <cmd>`** — show command type (builtin, alias, or binary)
- **`source <file>`** or **`. <file>`** — execute script files from VFS
- **`bash <script.sh>`** — execute bash scripts (new in v2.1.0)
- Variable expansion in all commands
- Alias expansion before execution
- Command chaining with `;`
- Startup config: `/etc/plum/plumrc`
- Default environment: `HOME`, `USER`, `SHELL`, `PATH`, `PS1`, `TERM`, `EDITOR`, etc.

**Bash script features** (v2.1.0+):
- Conditionals: `if`/`then`/`else`/`fi`
- Loops: `for`/`do`/`done`, `while`/`do`/`done`
- Functions: `function_name() { ... }`
- Variable expansion: `$var`, `${var}`
- Command substitution: `$(...)` (limited)
- Arithmetic: `$((expr))`

### seed — Init System
The first logical process (PID 1 equivalent), managing:

- **Service registration** — 9 core services (kernel, vfs, scheduler, console, serial, keyboard, network, storage, plum)
- **`seed status`** — display all services with status, PID, restart count, description
- **`seed start/stop/restart <svc>`** — control individual services
- **`seed enable/disable <svc>`** — change service activation policy
- **`seed log`** — show boot log with timestamps
- Restart policies: `always`, `once`, `manual`
- Config file: `/etc/seed/init.conf`
- Writes configuration to VFS on init

### tutor — Interactive Tutorial
Built-in interactive system guide with topics: `intro`, `fs`, `net`, `mem`, `kernel`, `commands`.

---

## Terminal / Shell

28+ built-in commands with full implementations:

| Category | Commands |
|----------|----------|
| **Navigation** | `help`, `clear`, `history`, `tutor` |
| **Filesystem** | `ls`, `cd`, `pwd`, `cat`, `mkdir`, `touch`, `rm`, `echo` (with `>` / `>>` redirect) |
| **System Info** | `version`, `uname`, `about`, `whoami`, `hostname`, `date`, `uptime` |
| **Hardware** | `free`, `lsmem`, `lspci`, `lsblk`, `fdisk`, `dmesg` |
| **Networking** | `ifconfig`, `ping`, `ntpdate` |
| **Control** | `install`, `reboot`, `shutdown`/`poweroff`/`halt` |
| **Userland** | `grape`, `tomato`, `seed`, `plum` |
| **Shell** | `export`, `alias`, `unalias`, `env`, `set`, `unset`, `type`, `source` |

Features: command history (Up/Down), Ctrl+C cancel, Ctrl+L clear, Tab support, VFS-backed file operations.

---

## Project Structure

```
ospab.os-v2/
├── arch/x86_64/        # HAL: GDT, IDT, PIC, SSE, keyboard, framebuffer, serial
├── core/               # Scheduler, IPC, syscall dispatch
├── drivers/            # PCI, ATA, AHCI, VirtIO stubs
├── executive/          # Object manager, process, power management
├── hpc/                # Tensor unit, DMA engine, shared memory
├── mm/                 # Physical allocator, heap, VMM (4-level page tables)
├── src/
│   ├── main.rs         # Boot entry — 5-phase init sequence
│   ├── lib.rs          # Crate root — module declarations
│   ├── terminal.rs     # Interactive terminal with 28+ commands
│   ├── fs/             # VFS + RamFS (27+ nodes at boot)
│   ├── net/            # RTL8139, Ethernet, ARP, IPv4, ICMP, UDP, SNTP
│   └── drivers/        # ATA PIO, AHCI SATA
├── userland/
│   ├── grape/src/      # nano-like text editor
│   ├── tomato/src/     # pacman-like package manager
│   ├── plum/src/       # POSIX-inspired shell
│   └── seed/src/       # Init system (PID 1)
├── limine.conf         # Bootloader configuration
├── linker.ld           # Kernel linker script
├── x86_64-ospab.json   # Custom Rust target spec
└── build.sh            # Build script (cargo + xorriso)
```

---

## Boot Sequence

```
Phase 0: Hardware Init     — SSE, GDT, IDT, PIC
Phase 1: Limine Protocol   — bootloader verification
Phase 2: Memory            — physical allocator, 128 MiB heap, VMM
Phase 3: Kernel Services   — scheduler, syscall MSRs, VFS+RamFS, storage
Phase 4: Network           — RTL8139 auto-detect, ARP, ICMP self-test
Phase 4.5: Userland Init   — seed (services), plum (shell env + aliases)
Phase 5: Terminal           — interactive console
```

---

## Building

```bash
# Requires: Rust nightly, xorriso, mtools, limine
bash build.sh
```

## Running
```bash
qemu-system-x86_64 \
  -cdrom isos/ospab-os-v2-19.iso \
  -m 256M \
  -serial stdio \
  -device rtl8139,netdev=net0 \
  -netdev user,id=net0
```

---

## Development Roadmap

### Phase 1: Foundation ✅
- [x] AETERNA kernel entry point
- [x] GDT, IDT, PIC, SSE/FPU initialization
- [x] Physical memory allocator (bitmap)
- [x] Kernel heap (128 MiB linked-list allocator)
- [x] Framebuffer console (8×16 VGA font)
- [x] PS/2 keyboard driver (US QWERTY, Shift, Ctrl, CapsLock, arrows)
- [x] Serial port (COM1) logging

### Phase 2: Drivers & Networking ✅
- [x] PCI bus enumeration
- [x] RTL8139 NIC driver (TX/RX DMA)
- [x] Ethernet, ARP, IPv4, ICMP, UDP
- [x] SNTP time sync
- [x] ATA PIO + AHCI SATA storage drivers
- [x] Installer TUI

### Phase 3: VMM, VFS, Syscall ✅
- [x] Virtual Memory Manager (4-level page tables, CR3)
- [x] VFS with mount table and file descriptors
- [x] RamFS (BTreeMap-based, 27+ nodes at boot)
- [x] Syscall dispatch (sys_open, sys_read, sys_write, sys_close)
- [x] VFS-integrated terminal commands

### Phase 4: Userland Tools ✅
- [x] grape — nano-like text editor
- [x] tomato — pacman-like package manager
- [x] plum — shell with env vars, aliases, scripting
- [x] seed — init system with service management
- [x] tutor — interactive tutorial

### Phase 4.1: bash Integration ✅
- [x] bash script execution (`bash script.sh`)
- [x] Conditionals (if/then/else/fi)
- [x] Loops (for/while)
- [x] Function definitions
- [x] Variable expansion in scripts
- [x] Command substitution (limited)

### Phase 5: Process Isolation (Next)
- [ ] ELF loader for userspace binaries
- [ ] Ring 3 execution with syscall interface
- [ ] Process isolation and IPC
- [ ] plum as standalone userspace shell

---

*Copyright © 2026 ospab. Boost Software License 1.1.*