<div align="center">

# ospab.os v2 — AETERNA

**A bare-metal microkernel operating system written in Rust**

[![lang](https://img.shields.io/badge/language-Rust%20(no__std)-orange?style=flat-square)](https://www.rust-lang.org/)
[![arch](https://img.shields.io/badge/arch-x86__64-blue?style=flat-square)](#)
[![kernel](https://img.shields.io/badge/kernel-AETERNA%20v2.1.0-darkblue?style=flat-square)](#)
[![license](https://img.shields.io/badge/license-BSL%201.1-green?style=flat-square)](LICENSE)

<br/>

[English](README.md) &nbsp;|&nbsp; [Русский](README_ru.md)

</div>

---

## Overview

**ospab.os** is a microkernel-based operating system written entirely in Rust with no standard library. The core — the **AETERNA** microkernel — provides deterministic scheduling, capability-based security, and AI-native computing primitives. The system boots from a Live ISO via the Limine protocol and runs in Long Mode (x86_64).

> The project name **ospab** is always written in lowercase. The kernel is **AETERNA**.

---

## Architecture

```
┌───────────────────────────────────────────────────────┐
│                      Terminal / Shell                  │
├─────────────┬─────────────┬──────────────┬────────────┤
│    grape    │    tomato   │     plum     │    seed    │
│  (editor)   │  (pkg mgr)  │   (shell)    │   (init)   │
├─────────────┴─────────────┴──────────────┴────────────┤
│                 VFS + RamFS + Syscall Layer             │
├──────────────────────┬────────────────────────────────┤
│     Network Stack    │       Storage Drivers          │
│  RTL8139 / ARP /     │    ATA PIO  /  AHCI SATA       │
│  IPv4 / ICMP / SNTP  │                                │
├──────────────────────┴────────────────────────────────┤
│             AETERNA Microkernel (Rust, no_std)         │
│    GDT · IDT · PIC · SSE · Heap · VMM · Scheduler     │
├───────────────────────────────────────────────────────┤
│              x86_64 Hardware / QEMU / KVM              │
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
├── drivers/            PCI, ATA, AHCI, VirtIO
├── mm/                 Physical allocator, heap, VMM (4-level page tables)
├── src/
│   ├── main.rs         Boot entry — 5-phase init sequence
│   ├── lib.rs          Crate root
│   ├── terminal.rs     Interactive terminal
│   ├── doom.rs         DOOM engine Rust FFI layer
│   ├── fs/             VFS + RamFS + disk persistence
│   ├── net/            RTL8139, Ethernet, ARP, IPv4, ICMP, UDP, SNTP
│   └── drivers/        ATA PIO, AHCI SATA
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
  -cdrom isos/ospab-os-v2-24.iso \
  -m 256M \
  -serial stdio \
  -device rtl8139,netdev=net0 \
  -netdev user,id=net0
```

---

## Roadmap

**Foundation** — GDT, IDT, PIC, SSE, heap, framebuffer, serial, keyboard `[done]`  
**Drivers & Networking** — PCI, RTL8139, ARP, IPv4, ICMP, UDP, SNTP, ATA, AHCI `[done]`  
**VMM + VFS + Syscall** — 4-level page tables, RamFS, sys_open/read/write/close `[done]`  
**Userland Tools** — grape, tomato, plum, seed, tutor, bash scripting `[done]`  
**DOOM Port** — doomgeneric bare-metal, freestanding C runtime, Rust FFI `[done]`  
**Disk Persistence** — RamFS serialization to LBA 2048, boot recovery `[done]`  
**Process Isolation** — ELF loader, Ring 3, syscall ABI, real IPC `[next]`  
**Driver Expansion** — VirtIO block/net, NVMe, RTL8169, e1000 `[next]`  

---

<div align="center">

Copyright &copy; 2026 ospab &nbsp;&middot;&nbsp; Boost Software License 1.1

</div>
