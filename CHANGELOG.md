# Changelog

All notable changes to ospab.os / AETERNA are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---

## [1.1.0] — 2026-03-07 — TCP Stack & Network Fixes

### Added

- **TCP stack** (`net/tcp.rs`) — complete RFC 793 implementation:
  - Full state machine: Closed → SynSent / Listen → SynReceived → Established → FinWait1/2 → TimeWait / CloseWait → LastAck → Closed
  - 3-way handshake (SYN / SYN-ACK / ACK), data transfer with seq/ack tracking
  - FIN teardown (both active and passive close), RST handling in all states
  - Retransmit timer (3 s), per-connection 8 KiB ring-buffer receive queue
  - 16-slot connection table; no heap allocation — fully static
  - Public API: `tcp_connect`, `tcp_listen`, `tcp_accept`, `tcp_send`, `tcp_recv`, `tcp_recv_nb`, `tcp_close`
- **IPv4 TCP dispatch** — protocol 6 now routed to `tcp::handle_tcp()` in `ipv4::handle_ipv4()`
- **Hardware link detection** — real register reads instead of always-true:
  - RTL8139: MediaStatusRegister (0x58) bit 2 (clear = link OK)
  - Intel e1000: STATUS register bit 1 (LU — Link Up)
  - RTL8169: PHY Status register (0x6C) bit 1
- **`net::link_up()`** — dispatches to active NIC's hardware check
- **RX/TX byte counters** — `net::rx_bytes()` / `net::tx_bytes()` atomics updated on every frame
- **`traceroute`** (`axon net_tools`) — ICMP TTL-based route discovery with `-m`/`-w` flags

### Fixed

- **`ifconfig` / `ip`** — no longer prints hardcoded `UP BROADCAST RUNNING MULTICAST`:
  - Interface name is the real driver name (RTL8139 / Intel e1000 / RTL8169/8111)
  - UP / DOWN flag derived from hardware link register
  - Broadcast-MAC gateway fallback annotated as `(broadcast fallback)` when ARP times out
  - RX/TX packet and byte counters displayed
- **`netstat`** — Status column now reflects actual hardware link state (was always "Up")
- **`signal_pid` / `signal_thread`** — dispatch on signal number: SIGCONT→Ready, SIGSTOP→Waiting, others→Dead
- **`env CMD`** — correctly parses `KEY=VAL` overrides, dispatches command, restores environment
- **`tomato --tmt pack`** — prints `[SIMULATION MODE]` warning for empty package manifests
- **Various display stubs** — removed false indicators from `seed log`, `df`, `top`, `netdiag`

### Changed

- `net/mod.rs` — added `pub mod tcp`, `link_up()`, `rx_bytes()`, `tx_bytes()`
- Version bump: 1.0.0 → 1.1.0

---

## [1.0.0] — 2026-03-05 — First Public Release

The first stable release of the AETERNA microkernel. Boots from a hybrid BIOS/UEFI Live ISO via
Limine, provides a complete interactive shell environment with 30+ commands, a working UEFI disk
installer, a DOOM port, and full userland tooling.

### Kernel & Architecture

- **x86-64 bare-metal** — Long Mode, SSE/FPU (CR4 `0x660`), GDT with TSS, IDT with 256 handlers
- **PIC** — 8259 remapped to IRQ 32–47; PIT IRQ 0 at 100 Hz; PS/2 keyboard IRQ 1
- **Memory** — bitmap physical allocator, 128 MiB kernel heap (`linked_list_allocator`), 4-level PML4 page tables
- **Boot protocol** — Limine 10.8.2, framebuffer via GOP, HHDM, memory map
- **Framebuffer** — 8×16 VGA bitmap font, 32-bit BGRA pixel format
- **Serial** — COM1 115200 8N1, structured boot log with `[OK]`/`[FAIL]` markers
- **FPU save/restore** — `fxsave`/`fxrstor` in all interrupt stubs

### Storage Drivers

- **ATA PIO** — polling, 28-bit LBA, read/write/identify, error recovery
- **AHCI SATA** — PCI BAR5, HBA reset, NCQ port init, DMA read/write, flush cache command
- **NVMe** — PCI BAR0, admin queue setup, I/O queue pair, PRP-based transfers, namespace identify, flush

### Audio Drivers

- **AC97** — Intel ICH-compatible, DMA BDL ring, 44100 Hz VRA, mixer volume control
- **ES1371** — Ensoniq AudioPCI / Creative CT5880 (VMware Workstation native)
- **Unified API** — `soundtest` command, 440 Hz sine wave test tone

### Network Stack

- **RTL8139** — PCI NIC, DMA TX/RX ring buffers, interrupt-driven receive
- **Intel e1000** — PRO/1000 Gigabit Ethernet, descriptor rings, EEPROM MAC read
- **Protocols** — Ethernet, ARP (TTL cache), IPv4 (header checksum), ICMP, UDP, SNTP
- **`ping -c N <ip>`** — TSC-based round-trip time in microseconds
- **`ntpdate`** — one-shot SNTP sync over UDP/123

### Virtual Filesystem

- **VFS** — POSIX-compatible trait layer, mount table, path resolution
- **RamFS** — in-memory `BTreeMap<String, RamNode>`, `spin::Mutex` protected
- **Deferred sync** — dirty flag + 182-tick PIT timer → serialise to LBA 2048 on boot disk
- **Boot recovery** — deserialise RamFS from LBA 2048 if magic header present
- **Standard dirs** — `/etc`, `/proc`, `/dev`, `/home`, `/tmp`, `/sys`, `/boot`, `/var`
- **27+ nodes** populated at boot: `/etc/hosts`, `/etc/motd`, plumrc, seed config, …
- **C FFI** — `rust_vfs_open/read/write/close/seek/rename/access/opendir` for DOOM

### UEFI Disk Installer

- **GPT** — protective MBR, primary + backup GPT headers, CRC32 per spec
- **FAT32 ESP** — dynamic sectors-per-cluster formula (≥ 65525 clusters, EDK2 compatible)
- **LFN support** — `lfn_checksum()`, `lfn_entry()`, UCS-2LE encoding; short name `LIMINE~1CON`
- **Files installed** — `/EFI/BOOT/BOOTX64.EFI`, `/boot/KERNEL`, `/limine.conf`
- **Multi-controller** — auto-detects NVMe → AHCI → ATA in priority order
- **Verification** — read-back of MBR, GPT, VBR, FAT[0..7], EFI MZ magic before declaring success
- **Disk flush** — three explicit `disk_flush()` calls: after GPT, after ESP files, before verify
- **Serial trace** — every installer step logged to COM1 with hex dump helpers

### Userland Tools

- **plum** — POSIX shell: variables (`$VAR`, `${VAR}`, `$?`), `export`, `alias`, `source`,
  `if/then/else/fi`, `for/while/do/done`, functions, `$(( ))`, `$( )`, `/etc/plum/plumrc`
- **grape** — full-screen nano-style editor: title/status/help bars, `Ctrl+O/X/K/U/W/G/T`,
  arrow keys, PgUp/PgDn, search with wrap-around, block cursor, auto-mkdir on save
- **tomato** — package manager: `-S/-R/-Q/-Qi/-Ss/-Sy/-Syu`; 10-package built-in repo;
  installation tracking in `/var/lib/tomato/`
- **seed** — init system: 9 services (`kernel`, `vfs`, `scheduler`, `console`, `serial`,
  `keyboard`, `network`, `storage`, `plum`); policies `always/once/manual`; `/etc/seed/init.conf`
- **tutor** — interactive tutorial: `intro`, `fs`, `net`, `mem`, `kernel`, `commands`

### Terminal

- 30+ built-in commands: `ls`, `cat`, `cd`, `pwd`, `mkdir`, `touch`, `rm`, `echo` (`>` / `>>`),
  `ping`, `ifconfig`, `ntpdate`, `free`, `lsmem`, `lspci`, `lsblk`, `fdisk`, `dmesg`,
  `version`, `uname`, `about`, `whoami`, `hostname`, `date`, `uptime`,
  `install`, `reboot`, `shutdown`, `poweroff`, `halt`, `sync`,
  `export`, `alias`, `unalias`, `env`, `set`, `unset`, `type`, `source`
- Command history (16-entry ring, Up/Down), tab completion, Ctrl+C interrupt, Ctrl+L clear
- All output mirrored to COM1 serial

### DOOM

- doomgeneric port — 640×400 rendering on UEFI framebuffer, 32-bit BGRA
- Freestanding C runtime (`malloc`, `free`, `printf`, `sprintf`, `memcpy`, `memmove`,
  `memset`, `qsort`, `sqrt`, `atan2`, …) bridged to kernel heap via `#[no_mangle]` FFI
- PS/2 keyboard with scancode-to-DOOM translation; F1–F10 menu navigation
- Save / load at `/doom/doomsav{0-5}.dsg` via VFS; persisted by deferred disk sync

### Build System

- `build.sh` — DOOM C compile (`gcc -nostdlib`) → kernel (`cargo build --release`) →
  debug strip (`llvm-objcopy --strip-debug`, output ≈ 6 MB) → hybrid ISO (`xorriso`)
- `bash build.sh kernel` — kernel-only build, skips ISO generation
- Zero-warning policy enforced on every commit

---

## [0.x] — Pre-release Development

Internal development series. Not publicly tagged.
