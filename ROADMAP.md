# AETERNA — Development Roadmap

This document tracks the phased development plan for the AETERNA microkernel and ospab.os userland. Each phase has a defined scope and measurable completion criteria.

---

## Phase 0 — Hardware Foundation ✅ Complete

- [x] Limine boot protocol — BIOS + UEFI hybrid ISO
- [x] x86-64 Long Mode, CR4 SSE/FPU enable (`0x660`)
- [x] GDT with TSS, IDT with 256 handlers, PIC remapped to IRQ 32–47
- [x] PIT IRQ 0 at 100 Hz, PS/2 keyboard IRQ 1
- [x] Serial COM1 (115200 8N1), UEFI framebuffer (8×16 VGA font, 32-bit BGRA)
- [x] Build system — `build.sh` (cargo + xorriso hybrid ISO)

---

## Phase 1 — Memory Management ✅ Complete

- [x] Physical frame allocator — bitmap, reads Limine memory map
- [x] 128 MiB kernel heap — `linked_list_allocator`, `#[global_allocator]`
- [x] 4-level page tables (PML4) — walk, map, unmap
- [x] Higher-half direct map (HHDM) from Limine physical offset
- [x] `alloc` crate — `Vec`, `BTreeMap`, `String` throughout kernel

---

## Phase 2 — Storage Drivers ✅ Complete

- [x] ATA PIO — polling, 28-bit LBA, read/write/identify
- [x] AHCI SATA — PCI BAR5, HBA reset, NCQ, DMA, flush cache
- [x] NVMe — PCI BAR0, admin + I/O queues, PRP, namespace identify, flush
- [x] PCI bus enumeration — type 0/1 headers, BAR decode

---

## Phase 3 — Virtual Filesystem ✅ Complete

- [x] VFS trait — open, read, write, mkdir, unlink, rename, readdir, stat
- [x] RamFS — `BTreeMap<String, RamNode>`, `spin::Mutex`
- [x] Standard directories — /etc, /proc, /dev, /home, /tmp, /sys, /boot, /var
- [x] 27+ nodes at boot (hosts, motd, plumrc, seed config, …)
- [x] Deferred disk sync — dirty flag + 182-tick PIT timer → LBA 2048
- [x] Boot recovery — deserialise RamFS from LBA 2048 on next boot
- [x] C FFI — rust_vfs_open/read/write/close/seek/rename/access for DOOM

---

## Phase 4 — Network Stack ✅ Complete

- [x] RTL8139 — PCI NIC, DMA TX/RX ring buffers, interrupt receive
- [x] Intel e1000 — PRO/1000 Gigabit, descriptor rings, EEPROM MAC
- [x] Ethernet frame dispatch, ARP with TTL cache, IPv4 with checksum
- [x] ICMP echo — `ping -c N` with TSC-based RTT in microseconds
- [x] UDP datagrams, SNTP one-shot sync over UDP/123

---

## Phase 5 — Audio Drivers ✅ Complete

- [x] AC97 — Intel ICH, DMA BDL ring, 44100 Hz VRA, `soundtest` command
- [x] ES1371 — Ensoniq AudioPCI / Creative CT5880 (VMware Workstation)

---

## Phase 6 — Userland Tools ✅ Complete

- [x] **plum** — POSIX shell: variables, export, alias, source, if/for/while, functions, arithmetic, command substitution
- [x] **grape** — full-screen nano-style editor: search, cut/paste, scroll, block cursor
- [x] **tomato** — pacman-like package manager: -S/-R/-Q/-Qi/-Ss/-Sy/-Syu, 10-package repo
- [x] **seed** — init system (logical PID 1), 9 services, start/stop/restart/enable/disable/log
- [x] **tutor** — interactive guide: intro, fs, net, mem, kernel, commands
- [x] 30+ terminal built-ins, history, tab completion, Ctrl+C, Ctrl+L

---

## Phase 7 — DOOM Port ✅ Complete

- [x] doomgeneric bare-metal — 640×400 on UEFI framebuffer, 32-bit BGRA
- [x] Freestanding C runtime (malloc/free/printf/memcpy/qsort) via `#[no_mangle]` FFI
- [x] PS/2 keyboard input, F1–F10 menu navigation
- [x] VFS save/load at /doom/doomsavN.dsg, persisted by deferred sync

---

## Phase 8 — UEFI Disk Installer ✅ Complete

- [x] Protective MBR + GPT (primary + backup) with CRC32 verification
- [x] FAT32 ESP — dynamic SPC (≥ 65525 clusters for EDK2)
- [x] FAT32 LFN entries — UCS-2LE for `limine.conf`, short name LIMINE~1CON
- [x] Installs: BOOTX64.EFI, /boot/KERNEL, /limine.conf
- [x] Auto-detects NVMe → AHCI → ATA in priority order
- [x] Read-back verification at every stage, full serial trace on COM1

---

## Phase 9 — Process Isolation 🔷 In Progress

- [ ] ELF64 loader — PT_LOAD segments mapped to Ring 3 virtual address space
- [ ] Ring 3 execution — sysret/iretq, separate user stack per process
- [ ] Syscall ABI — SYSCALL/SYSRET via MSR, register argument convention
- [ ] Process Control Block — PID allocator, parent/child tree, exit status
- [ ] fork / exec / wait — POSIX-compatible process lifecycle
- [ ] Signal delivery — SIGKILL, SIGTERM, SIGCHLD
- [ ] plum moves to Ring 3 — kernel access through syscalls only

---

## Phase 10 — Capability Enforcement 🔷 Planned

- [ ] Capability token type system — typed, non-forgeable handles
- [ ] Per-process capability table — inherited on fork, explicitly revocable
- [ ] Syscall dispatcher rewrite — all paths validate capability before servicing
- [ ] Scoped capabilities: filesystem subtrees, network ports, DMA regions

---

## Phase 11 — TCP/IP 🔷 Planned

- [ ] TCP state machine — SYN, SYN-ACK, ACK, FIN, RST, retransmit timer
- [ ] Per-connection send/receive buffers, sliding window
- [ ] DNS resolver — UDP/53, TTL-based cache

---

## Phase 12 — Preemptive Scheduler 🔷 Planned

- [ ] TCB (Thread Control Block) — GP + FPU register save area, kernel stack
- [ ] PID/TID HashMap, IRQ 0 context switch via PIT
- [ ] Priority classes — real-time, interactive, background
- [ ] Timer wheel, nanosleep syscall

---

## Phase 13 — USB Stack 🔷 Planned

- [ ] xHCI host controller — register init, TRB command ring
- [ ] USB HID class — keyboard as PS/2 fallback
- [ ] USB Mass Storage — BOT protocol, block device interface

---

## Design Invariants

1. **No `std`** — kernel and all userland code compile with `#![no_std]`. All heap allocation via `alloc`.
2. **No panics in interrupt handlers** — fault paths log and halt; no unwinding in ring-0 interrupt context.
3. **Deferred I/O** — RamFS writes are buffered; callers must not assume disk persistence until the next sync cycle or an explicit `sys_sync()`.
4. **Capability-first** — no subsystem is designed around ambient authority. All cross-boundary access will require an explicit capability token.
5. **Zero warnings** — every commit must pass `cargo build --release` with zero warnings. WIP modules suppress warnings at module scope, not crate scope.
