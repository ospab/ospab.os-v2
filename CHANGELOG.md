# Changelog

All notable changes to ospab.os / AETERNA are documented here.

---

## [1.0.0] — 2026-03-05 — First Public Release

The first stable release of the AETERNA microkernel. Boots from Live ISO
(BIOS + UEFI hybrid) via Limine, runs 30+ shell commands, includes
a working UEFI disk installer, DOOM port, and full userland tools.

### Kernel & Architecture
- **x86_64 bare-metal** — Long Mode, SSE/FPU, GDT, IDT, PIC, PIT (100 Hz)
- **Memory** — linked_list_allocator, 128 MiB heap, 4-level page tables (PML4)
- **Limine protocol** — BIOS + UEFI hybrid boot, framebuffer via GOP
- **Framebuffer** — 8x16 VGA font, 32-bit BGRA rendering
- **PS/2 keyboard** — IRQ 1, ring buffer, scancode translation
- **Serial** — COM1 115200 8N1, kernel log output

### Storage Drivers
- **ATA PIO** — IDE hard drive read/write
- **AHCI SATA** — DMA-based SATA via PCI BAR5, NCQ, flush cache
- **NVMe** — Admin + I/O queue setup, PRP-based transfers, flush

### Audio Drivers
- **AC97** — Intel ICH-compatible audio, DMA BDL ring, 44100 Hz VRA
- **ES1371** — Ensoniq AudioPCI / Creative CT5880 (VMware support)
- **Unified API** — `soundtest` command, 440 Hz test tone generation

### Network Stack
- **RTL8139** — PCI NIC with DMA ring buffer TX/RX
- **e1000** — Intel PRO/1000 Gigabit Ethernet
- **Protocols** — Ethernet, ARP, IPv4, ICMP, UDP, SNTP
- **Commands** — `ping` with TSC-based latency, `ifconfig`, `ntpdate`

### Virtual Filesystem
- **VFS** — POSIX-compatible, mount table, path resolution
- **RamFS** — In-memory BTreeMap with spin-lock
- **Disk persistence** — Deferred sync to LBA 2048 (10s timer)
- **Standard dirs** — `/etc`, `/proc`, `/dev`, `/home`, `/tmp`, `/sys`, `/boot`, `/var`

### UEFI Disk Installer
- **GPT** — Protective MBR + primary/backup GPT headers + CRC32 verification
- **FAT32 ESP** — Dynamic SPC for EDK2 compatibility (>= 65525 clusters)
- **LFN support** — Long File Name entries for `limine.conf`
- **Files installed** — `/EFI/BOOT/BOOTX64.EFI`, `/boot/KERNEL`, `/limine.conf`
- **NVMe + AHCI + ATA** — Works on VMware, QEMU, bare metal
- **Verification** — Read-back checks for MBR, GPT, VBR, FAT chains, EFI binary
- **Serial logging** — Full installer trace on COM1 for remote debugging

### Userland Tools
- **plum** — POSIX shell with bash scripting, variables, conditionals, loops,
  functions, `source`, aliases, pipes
- **grape** — Full-screen nano-style text editor with VFS integration
- **tomato** — pacman-inspired package manager (`-S`, `-R`, `-Q`, `-Ss`, `-Syu`)
- **seed** — Init system (PID 1), 9 core services, `start`/`stop`/`restart`
- **tutor** — Interactive tutorial (`intro`, `fs`, `net`, `mem`, `kernel`, `commands`)

### DOOM
- **doomgeneric** port running bare-metal on UEFI framebuffer (640x400)
- Freestanding C runtime bridged to Rust kernel allocator via FFI
- PS/2 keyboard input, F1-F10 menu, save/load via VFS

### Terminal
- 30+ built-in commands (`ls`, `cat`, `mkdir`, `ping`, `free`, `lspci`, `install`, ...)
- Command history (Up/Down), tab completion, Ctrl+C cancel, Ctrl+L clear
- Dual output: framebuffer + serial (COM1)

---

## Building

```bash
bash build.sh          # Full ISO: cargo + xorriso hybrid
bash build.sh kernel   # Kernel only
```

## Running

```bash
# QEMU (BIOS)
qemu-system-x86_64 -cdrom isos/ospab-os-v2-101.iso -m 256M -serial stdio

# QEMU (UEFI)
qemu-system-x86_64 -cdrom isos/ospab-os-v2-101.iso -m 256M -serial stdio \
  -bios /usr/share/OVMF/OVMF_CODE.fd

# VMware Workstation — attach ISO as CD-ROM, UEFI firmware
```
