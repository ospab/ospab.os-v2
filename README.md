<div align="center">

<br/>

# AETERNA

### A Research Microkernel for x86-64, Written in Rust

<br/>

[![Language](https://img.shields.io/badge/Language-Rust%20%28no__std%29-f74c00?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Architecture](https://img.shields.io/badge/Architecture-x86__64-0366d6?style=flat-square)](#architecture)
[![Version](https://img.shields.io/badge/Kernel-AETERNA%20v1.0.0-1a1a2e?style=flat-square)](#)
[![License](https://img.shields.io/badge/License-BSL%201.1-2ea44f?style=flat-square)](LICENSE)
[![Build](https://img.shields.io/badge/Build-xorriso%20%2B%20cargo-555?style=flat-square)](#building)

<br/>

[English](README.md) &nbsp;·&nbsp; [Русский](README_ru.md)

<br/>

</div>

**AETERNA** is a bare-metal microkernel operating system written entirely in Rust (`#![no_std]`). It targets x86-64, boots via the [Limine](https://github.com/limine-bootloader/limine) protocol from a hybrid BIOS/UEFI ISO, and provides a complete userland environment — shell, editor, package manager, init system, and a DOOM port — all running at ring 0 without a host OS.

The project is not a toy kernel. It implements a real memory hierarchy (physical allocator, 128 MiB kernel heap, 4-level page tables), a POSIX-compatible virtual filesystem with disk persistence, a working TCP/IP-adjacent network stack, storage drivers for ATA/AHCI/NVMe, audio drivers (AC97, ES1371), a UEFI disk installer with GPT/FAT32/LFN support, and a capability-based security model in design.

> The project name is **ospab.os** (lowercase). The kernel is **AETERNA**.

---

## Table of Contents

- [Architecture](#architecture)
- [Kernel Bootstrap](#kernel-bootstrap)
- [Subsystems](#subsystems)
- [Userland](#userland)
- [Terminal](#terminal)
- [Project Layout](#project-layout)
- [Building](#building)
- [Running](#running)
- [Roadmap](#roadmap)

---

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│                    User Session (ring 0)                 │
│          plum (shell)   grape (editor)   doom            │
├──────────┬───────────────┬───────────────────────────────┤
│   seed   │    tomato     │           tutor               │
│  (init)  │  (pkg mgr)    │      (interactive guide)      │
├──────────┴───────────────┴───────────────────────────────┤
│               VFS  ·  RamFS  ·  Syscall layer            │
├───────────────────────┬──────────────────────────────────┤
│     Network Stack     │         Storage Drivers          │
│  RTL8139 · e1000      │   ATA PIO · AHCI SATA · NVMe     │
│  ARP · IPv4 · ICMP    │                                  │
│  UDP · SNTP           │   Audio: AC97 · ES1371           │
├───────────────────────┴──────────────────────────────────┤
│              AETERNA Microkernel  (Rust, no_std)         │
│                                                          │
│  Hardware Abstraction                                    │
│    GDT · IDT · PIC · SSE/FPU · PIT (100 Hz)              │
│                                                          │
│  Memory                                                  │
│    Physical allocator · 128 MiB heap · 4-level PML4      │
│                                                          │
│  Execution                                               │
│    Compute-First scheduler · MSR-based syscall dispatch  │
│    Capability model (in progress)                        │
├──────────────────────────────────────────────────────────┤
│              x86-64 Hardware  /  QEMU  /  VMware         │
└──────────────────────────────────────────────────────────┘
```

---

## Kernel Bootstrap

The kernel initialises in five sequential phases, each logged to both the serial port (COM1, 115200 baud) and the UEFI framebuffer:

| Phase | Name | What happens |
|---|---|---|
| 0 | Hardware | SSE/FPU enable (CR4 `0x660`), GDT with TSS, IDT with 256 handlers, PIC remapped to IRQ 32–47 |
| 1 | Boot protocol | Limine response validation, framebuffer descriptor, memory map acquisition |
| 2 | Memory | Physical frame allocator (bitmap), 128 MiB kernel heap (`linked_list_allocator`), 4-level page table walk |
| 3 | Kernel services | Compute-First scheduler, MSR syscall dispatch, VFS + RamFS (27 nodes at boot), ATA/AHCI/NVMe detection |
| 4 | Networking | RTL8139 / e1000 PCI probe, ARP table init, ICMP self-test, UDP socket layer, SNTP sync |
| 4.5 | Userland | `seed` service manager (9 services), `plum` shell environment and startup aliases |
| 5 | Terminal | Interactive console — command dispatch, history, tab completion |

All phases must complete with `[OK]` before the shell prompt appears. Any failure halts with a structured error to both framebuffer and serial.

---

## Subsystems

### Virtual Filesystem

A fully POSIX-compatible virtual filesystem layer. `RamFS` provides an in-memory `BTreeMap`-backed filesystem mounted at `/`, protected by a `spin::Mutex`. Standard directories are created at boot: `/etc`, `/proc`, `/dev`, `/home`, `/tmp`, `/sys`, `/boot`, `/var`.

All file operations — `open`, `read`, `write`, `mkdir`, `unlink`, `rename`, `readdir` — are complete implementations. A deferred sync mechanism serialises the RamFS image to LBA 2048 on the boot disk every 10 seconds (182 PIT ticks), ensuring DOOM save files and shell state survive reboots without blocking the user.

### Network Stack

| Layer | Implementation |
|---|---|
| Drivers | RTL8139 (DMA ring buffer TX/RX), Intel e1000 (PRO/1000 Gigabit) |
| Ethernet | Frame parsing and dispatch table |
| ARP | Request / reply with TTL-based resolution cache |
| IPv4 | Transmit / receive with header checksum validation |
| ICMP | Echo request / reply — `ping -c N <ip>` with TSC-based RTT in µs |
| UDP | Datagram send / receive |
| SNTP | One-shot time synchronisation (pool.ntp.org via UDP/123) |

### Storage

| Driver | Interface | Notes |
|---|---|---|
| ATA PIO | IDE / ISA | Polling, 28-bit LBA, read/write/identify |
| AHCI SATA | PCI BAR5 | NCQ, DMA, flush cache, HBA reset |
| NVMe | PCI BAR0 | Admin + I/O queue pairs, PRP-based transfers, namespace identify |

The UEFI disk installer writes a valid GPT partition table, creates a FAT32 ESP with Long File Name entries for `limine.conf`, copies `BOOTX64.EFI`, the kernel binary, and the bootloader config, then verifies every written sector before reporting success.

### Audio

| Driver | Hardware | Notes |
|---|---|---|
| AC97 | Intel ICH (VMware, QEMU) | DMA BDL ring, 44100 Hz VRA, `soundtest` command |
| ES1371 | Ensoniq AudioPCI / Creative CT5880 | VMware Workstation native device |

### Capability Security Model *(in design)*

The kernel defines capability tokens as typed, non-forgeable handles granting access to specific kernel objects (filesystem subtrees, network sockets, DMA regions). The type system is defined in `core/capability/` and will be enforced at the syscall boundary in v1.1.

---

## Userland

All userland components are written in Rust and currently execute as kernel-mode modules. Process isolation (ELF loader, Ring 3 execution, syscall ABI) is the primary focus of the next development phase.

### plum — Shell

A POSIX-compatible shell with full bash scripting support.

- Environment variables: `$VAR`, `${VAR}`, `$?` (exit code)
- `export`, `alias`/`unalias`, `unset`, `set`, `env`, `type`
- `source <file>` / `. <file>` — execute scripts from the VFS
- Conditionals: `if` / `then` / `else` / `fi`
- Loops: `for` / `while` / `do` / `done`
- Functions, arithmetic expansion `$(( ))`, command substitution `$( )`
- Startup configuration at `/etc/plum/plumrc`

### grape — Text Editor

A full-screen editor modelled after GNU nano, integrated with the VFS.

- Title bar, status bar, keybinding help bar
- `Ctrl+O` save, `Ctrl+X` exit, `Ctrl+K` cut, `Ctrl+U` paste, `Ctrl+W` search, `Ctrl+G` help, `Ctrl+T` go-to-line
- Arrow keys, Home/End, PgUp/PgDn, horizontal and vertical scrolling
- Search with wrap-around, block cursor rendering (inverted colours)
- Automatically creates parent directories on save

### tomato — Package Manager

A pacman-inspired package manager with local binary package support.

| Flag | Action |
|---|---|
| `-S <pkg>` | Install with dependency resolution |
| `-R <pkg>` | Remove and purge installed files |
| `-Q` | List installed packages |
| `-Qi <pkg>` | Show detailed package metadata |
| `-Ss <query>` | Search available packages (case-insensitive) |
| `-Sy` | Synchronise package database |
| `-Syu` | Full system upgrade |

Built-in repository: `base` · `coreutils` · `grape` · `plum` · `net-tools` · `tutor` · `kernel-headers` · `ospab-libc` · `man-pages` · `seed`

### seed — Init System

The logical PID 1, managing nine core system services.

| Command | Description |
|---|---|
| `seed status` | Show all services — state, PID, restart count, description |
| `seed start/stop/restart <svc>` | Manage individual services |
| `seed enable/disable <svc>` | Toggle activation policy |
| `seed log` | Display timestamped boot log |

Services: `kernel` · `vfs` · `scheduler` · `console` · `serial` · `keyboard` · `network` · `storage` · `plum`

### tutor — Interactive Guide

A built-in guided tour of the system. Topics: `intro` · `fs` · `net` · `mem` · `kernel` · `commands`

---

## Terminal

30+ fully implemented built-in commands:

| Category | Commands |
|---|---|
| Navigation | `help`, `clear`, `history`, `tutor` |
| Filesystem | `ls`, `cd`, `pwd`, `cat`, `mkdir`, `touch`, `rm`, `echo` (`>` / `>>`) |
| System info | `version`, `uname`, `about`, `whoami`, `hostname`, `date`, `uptime` |
| Hardware | `free`, `lsmem`, `lspci`, `lsblk`, `fdisk`, `dmesg` |
| Networking | `ifconfig`, `ping`, `ntpdate` |
| Control | `install`, `reboot`, `shutdown`, `poweroff`, `halt`, `sync` |
| Userland | `grape`, `tomato`, `seed`, `plum`, `doom` |
| Shell | `export`, `alias`, `unalias`, `env`, `set`, `unset`, `type`, `source` |

Input features: command history (Up/Down arrows), Ctrl+C interrupt, Ctrl+L clear screen, tab completion of builtins. All output is mirrored to COM1 serial.

---

## DOOM

The [doomgeneric](https://github.com/ozkl/doomgeneric) port runs the original 1993 DOOM engine directly on the UEFI framebuffer without any host OS.

- 640×400 rendering at the native framebuffer pixel format (32-bit BGRA)
- PS/2 keyboard input with scancode-to-DOOM key translation
- F1–F10 menu navigation
- Save / load via VFS (`/doom/doomsavN.dsg`), persisted to disk via deferred sync
- A freestanding C runtime (`malloc`, `printf`, `memcpy`, `qsort`, …) bridged entirely to the Rust kernel heap via `#[no_mangle]` FFI

Run with: `doom`

---

## Project Layout

```
ospab.os-v2/
├── arch/x86_64/         Hardware abstraction — GDT, IDT, PIC, SSE, keyboard,
│                        framebuffer, serial, panic handler
├── core/                Scheduler, IPC primitives, syscall dispatch, capabilities
├── drivers/             PCI bus enumeration, storage trait, video, VirtIO stubs
├── executive/           Object manager, process table, power management
├── hpc/                 Tensor unit, DMA engine, shared memory interfaces
├── mm/                  Physical allocator, kernel heap, VMM (PML4 page tables)
├── net/                 Network stack trait definitions
├── vfs/                 VFS trait, cache, filesystem format interfaces
├── src/
│   ├── main.rs          Boot entry — 5-phase init sequence
│   ├── lib.rs           Crate root — all module declarations
│   ├── terminal.rs      Interactive terminal and command dispatch
│   ├── installer.rs     UEFI disk installer (GPT + FAT32 + LFN + Limine)
│   ├── doom.rs          DOOM engine Rust FFI layer
│   ├── fs/              VFS + RamFS + deferred disk persistence
│   ├── net/             RTL8139, e1000, Ethernet, ARP, IPv4, ICMP, UDP, SNTP
│   └── drivers/         ATA PIO, AHCI SATA, NVMe, AC97, ES1371
├── doom_engine/         doomgeneric C source + freestanding libc shim
├── userland/
│   ├── grape/           Text editor
│   ├── tomato/          Package manager
│   ├── plum/            Shell
│   └── seed/            Init system
├── limine.conf          Bootloader configuration
├── linker.ld            Kernel linker script
├── x86_64-ospab.json    Custom Rust target specification
└── build.sh             Build entry point (cargo → strip → xorriso)
```

---

## Building

**Requirements:** Rust nightly (`rustup default nightly`), `xorriso`, `mtools`, LLVM toolchain

```bash
git clone https://github.com/ospab/aeterna.git
cd aeterna
bash build.sh
```

The script compiles the DOOM C engine with `gcc -nostdlib`, builds the Rust kernel for `x86_64-ospab`, strips debug symbols with `llvm-objcopy`, and produces a hybrid BIOS+UEFI ISO under `isos/`.

To build the kernel only (no ISO):

```bash
bash build.sh kernel
```

---

## Running

```bash
# QEMU — BIOS boot, with networking and serial output
qemu-system-x86_64 \
  -cdrom isos/ospab-os-v2-103.iso \
  -m 256M \
  -serial stdio \
  -device rtl8139,netdev=net0 \
  -netdev user,id=net0

# QEMU — UEFI boot
qemu-system-x86_64 \
  -cdrom isos/ospab-os-v2-103.iso \
  -m 256M \
  -serial stdio \
  -bios /usr/share/OVMF/OVMF_CODE.fd \
  -device rtl8139,netdev=net0 \
  -netdev user,id=net0

# VMware Workstation — attach ISO as CD/DVD, set firmware to UEFI
```

Serial output on stdout shows the full structured boot log and all kernel diagnostic messages.

---

## Roadmap

| Milestone | Status |
|---|---|
| Hardware init (GDT, IDT, PIC, SSE, PIT) | ✅ Done |
| Memory management (physical allocator, heap, VMM) | ✅ Done |
| POSIX VFS + RamFS + disk persistence | ✅ Done |
| Network stack (RTL8139, e1000, ARP, IPv4, ICMP, UDP, SNTP) | ✅ Done |
| Storage drivers (ATA PIO, AHCI SATA, NVMe) | ✅ Done |
| Audio drivers (AC97, ES1371) | ✅ Done |
| Userland tools (plum, grape, tomato, seed, tutor) | ✅ Done |
| DOOM port (bare-metal, VFS saves, freestanding C runtime) | ✅ Done |
| UEFI disk installer (GPT, FAT32, LFN, multi-controller) | ✅ Done |
| Process isolation (ELF loader, Ring 3, syscall ABI) | 🔷 Next |
| Capability enforcement in syscall dispatcher | 🔷 Next |
| TCP/IP stack (SYN/ACK, connection state machine) | 🔷 Planned |
| Preemptive scheduling with TCB/PID table | 🔷 Planned |
| USB stack (xHCI) | 🔷 Planned |

See [ROADMAP.md](ROADMAP.md) for the full phased plan.

---

<div align="center">

Copyright &copy; 2026 ospab &nbsp;·&nbsp; <a href="LICENSE">Boost Software License 1.1</a>

</div> The core — the **AETERNA** microkernel — provides deterministic scheduling, capability-based security, and AI-native computing primitives. The system boots from a Live ISO via the Limine protocol and runs in Long Mode (x86_64).

> The project name **ospab** is always written in lowercase. The kernel is **AETERNA**.

---

## Architecture

```
┌───────────────────────────────────────────────────────┐
│                      Terminal / Shell                 │
├─────────────┬─────────────┬──────────────┬────────────┤
│    grape    │    tomato   │     plum     │    seed    │
│  (editor)   │  (pkg mgr)  │   (shell)    │   (init)   │
├─────────────┴─────────────┴──────────────┴────────────┤
│                 VFS + RamFS + Syscall Layer           │
├──────────────────────┬────────────────────────────────┤
│     Network Stack    │       Storage Drivers          │
│  RTL8139 / ARP /     │    ATA PIO  /  AHCI SATA       │
│  IPv4 / ICMP / SNTP  │                                │
├──────────────────────┴────────────────────────────────┤
│             AETERNA Microkernel (Rust, no_std)        │
│    GDT · IDT · PIC · SSE · Heap · VMM · Scheduler     │
├───────────────────────────────────────────────────────┤
│              x86_64 Hardware / QEMU / KVM             │
└───────────────────────────────────────────────────────┘
```

---

## Kernel: AETERNA

The microkernel handles hardware initialization (GDT, IDT, PIC, SSE/FPU), physical and virtual memory management, a Compute-First scheduler, and syscall dispatch via MSRs. It boots through the Limine protocol and renders output via a UEFI framebuffer with an 8×16 VGA font.

**Boot sequence:**

```
Phase 0   Hardware Init      SSE, GDT, IDT, PIC
Phase 1   Limine Protocol    Bootloader + memory map
Phase 2   Memory             Physical allocator, 128 MiB heap, VMM
Phase 3   Kernel Services    Scheduler, syscalls, VFS + RamFS, storage
Phase 4   Network            RTL8139 auto-detect, ARP, ICMP self-test
Phase 4.5 Userland Init      seed (services), plum (shell env + aliases)
Phase 5   Terminal           Interactive console
```

---

## Subsystems

### Virtual Filesystem

A fully functional POSIX-compatible virtual filesystem. RamFS provides an in-memory filesystem mounted at `/` with standard directories (`/etc`, `/proc`, `/dev`, `/home`, `/tmp`, `/sys`, `/boot`, `/var`). All file operations — read, write, mkdir, touch, rm — are real implementations, not stubs.

### Network Stack

| Layer | Implementation |
|---|---|
| Driver | RTL8139 PCI NIC with DMA ring buffer TX/RX |
| Ethernet | Frame parsing and dispatch |
| ARP | Request / reply with MAC resolution table |
| IPv4 | Send / receive with checksum |
| ICMP | Echo request / reply (ping) |
| UDP | Datagram send / receive |
| SNTP | Network time synchronization |

### Storage Drivers

| Driver | Protocol |
|---|---|
| ATA PIO | IDE hard drive access |
| AHCI SATA | Modern SATA controllers via PCI BAR5 |
| NVMe | NVMe SSD via PCI BAR0 (admin + I/O queues) |

### Audio Drivers

| Driver | Protocol |
|---|---|
| AC97 | Intel AC97 Audio (ICH compatible) |
| ES1371 | Ensoniq AudioPCI / Creative CT5880 |

### Network Drivers

| Driver | Protocol |
|---|---|
| RTL8139 | Realtek RTL8139 PCI NIC |
| e1000 | Intel PRO/1000 (Gigabit Ethernet) |

---

## Userland Tools

All userland tools are written in Rust and run as kernel-integrated modules. Userspace process isolation is on the roadmap.

### grape — Text Editor

A full-screen nano-style editor with VFS integration.

- Title bar, status bar, and shortcut help bar
- `Ctrl+X` exit, `Ctrl+O` save, `Ctrl+K` cut, `Ctrl+U` paste, `Ctrl+W` search, `Ctrl+G` help, `Ctrl+T` go-to-line
- Arrow keys, Home/End, PgUp/PgDn, horizontal/vertical scrolling
- Search with wrap-around, block cursor
- Creates parent directories automatically on save

### tomato — Package Manager

A pacman-inspired package manager with local binary package support.

| Flag | Action |
|---|---|
| `-S <pkg>` | Install with dependency resolution |
| `-R <pkg>` | Remove and clean up files |
| `-Q` | List installed packages |
| `-Qi <pkg>` | Show detailed package info |
| `-Ss <query>` | Search available packages |
| `-Sy` | Sync package database |
| `-Syu` | Full system upgrade |

Built-in repository: `base`, `coreutils`, `grape`, `plum`, `net-tools`, `tutor`, `kernel-headers`, `ospab-libc`, `man-pages`, `seed`.

### plum — Shell

A POSIX-inspired shell with bash script execution.

- Environment variables: `$VAR`, `${VAR}`, `$?`
- `export`, `alias`, `unalias`, `unset`, `set`, `env`, `type`
- `source <file>` / `. <file>` — execute scripts from VFS
- `bash <script.sh>` — bash script execution
- Conditionals: `if` / `then` / `else` / `fi`
- Loops: `for` / `while` / `do` / `done`
- Function definitions, variable expansion, arithmetic `$((expr))`
- Startup config at `/etc/plum/plumrc`

### seed — Init System

The PID 1 equivalent managing 9 core services.

| Command | Description |
|---|---|
| `seed status` | Show all services with status and restart count |
| `seed start/stop/restart <svc>` | Control a service |
| `seed enable/disable <svc>` | Change activation policy |
| `seed log` | Show boot log with timestamps |

### tutor — Interactive Tutorial

Built-in interactive guide with topics: `intro`, `fs`, `net`, `mem`, `kernel`, `commands`.

---

## Terminal Commands

30+ built-in commands, all fully implemented:

| Category | Commands |
|---|---|
| Navigation | `help`, `clear`, `history`, `tutor` |
| Filesystem | `ls`, `cd`, `pwd`, `cat`, `mkdir`, `touch`, `rm`, `echo` (with `>` / `>>`) |
| System Info | `version`, `uname`, `about`, `whoami`, `hostname`, `date`, `uptime` |
| Hardware | `free`, `lsmem`, `lspci`, `lsblk`, `fdisk`, `dmesg` |
| Networking | `ifconfig`, `ping`, `ntpdate` |
| Control | `install`, `reboot`, `shutdown` / `poweroff` / `halt`, `sync` |
| Userland | `grape`, `tomato`, `seed`, `plum`, `doom` |
| Shell | `export`, `alias`, `unalias`, `env`, `set`, `unset`, `type`, `source` |

Features: command history (Up/Down), Ctrl+C cancel, Ctrl+L clear, VFS-backed file operations.

---

## DOOM

The [doomgeneric](https://github.com/ozkl/doomgeneric) port runs the 1993 DOOM engine bare-metal.

- Full-screen 640×400 rendering to UEFI framebuffer
- PS/2 keyboard input with scancode translation
- F1–F10 keys for menu navigation
- Shareware WAD embedded via `include_bytes!`
- C runtime (malloc, printf, string ops) bridged to the Rust kernel allocator

Run with: `doom`

---

## Project Structure

```
ospab.os-v2/
├── arch/x86_64/        HAL: GDT, IDT, PIC, SSE, keyboard, framebuffer, serial
├── core/               Scheduler, IPC, syscall dispatch
├── drivers/            PCI, ATA, AHCI, NVMe, VirtIO, AC97, ES1371
├── mm/                 Physical allocator, heap, VMM (4-level page tables)
├── src/
│   ├── main.rs         Boot entry — 5-phase init sequence
│   ├── lib.rs          Crate root
│   ├── terminal.rs     Interactive terminal
│   ├── installer.rs    UEFI disk installer (GPT + FAT32 + Limine)
│   ├── doom.rs         DOOM engine Rust FFI layer
│   ├── fs/             VFS + RamFS + disk persistence
│   ├── net/            RTL8139, e1000, Ethernet, ARP, IPv4, ICMP, UDP, SNTP
│   └── drivers/        ATA PIO, AHCI SATA, NVMe, AC97, ES1371
├── doom_engine/        doomgeneric C source (95 files) + freestanding libc
├── userland/
│   ├── grape/          nano-like text editor
│   ├── tomato/         pacman-like package manager
│   ├── plum/           POSIX shell with bash scripting
│   └── seed/           Init system (PID 1)
├── limine.conf         Bootloader configuration
├── linker.ld           Kernel linker script
├── x86_64-ospab.json   Custom Rust target spec
└── build.sh            Build script (cargo + xorriso)
```

---

## Building

**Requirements:** Rust nightly, `xorriso`, `mtools`, LLVM/Clang

```bash
bash build.sh
```

The script compiles the DOOM C engine, builds the Rust kernel, assembles the ISO, and writes it to `isos/`.

## Running

```bash
qemu-system-x86_64 \
  -cdrom isos/ospab-os-v2-101.iso \
  -m 256M \
  -serial stdio \
  -device rtl8139,netdev=net0 \
  -netdev user,id=net0
```

---

## Roadmap

**Foundation** -- GDT, IDT, PIC, SSE, heap, framebuffer, serial, keyboard `[done]`  
**Drivers & Networking** -- PCI, RTL8139, e1000, ARP, IPv4, ICMP, UDP, SNTP, ATA, AHCI, NVMe `[done]`  
**Audio** -- AC97, ES1371/AudioPCI `[done]`  
**VMM + VFS + Syscall** -- 4-level page tables, RamFS, sys_open/read/write/close `[done]`  
**Userland Tools** -- grape, tomato, plum, seed, tutor, bash scripting `[done]`  
**DOOM Port** -- doomgeneric bare-metal, freestanding C runtime, Rust FFI `[done]`  
**Disk Persistence** -- RamFS serialization to LBA 2048, boot recovery `[done]`  
**UEFI Installer** -- GPT + FAT32 ESP + Limine, installs to NVMe/AHCI/ATA `[done]`  
**Process Isolation** -- ELF loader, Ring 3, syscall ABI, real IPC `[next]`  

---

<div align="center">

Copyright &copy; 2026 ospab &nbsp;&middot;&nbsp; Boost Software License 1.1

</div>
