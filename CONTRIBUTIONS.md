# Contributing to AETERNA / ospab.os

Thank you for your interest in contributing. This document covers the build environment,
code conventions, and contribution workflow.

---

## Getting Started

### Prerequisites

| Tool | Purpose | Install |
|---|---|---|
| Rust nightly | Kernel and userland compilation | `rustup default nightly` |
| `xorriso` | Hybrid ISO generation | `apt install xorriso` |
| `mtools` | FAT32 image manipulation | `apt install mtools` |
| LLVM toolchain | `llvm-objcopy` for strip, `clang` for C | `apt install llvm clang` |
| QEMU | Testing | `apt install qemu-system-x86` |

The kernel targets the custom `x86_64-ospab` Rust target defined in `x86_64-ospab.json`.
Run `rustup target add` is not needed — cargo picks up the JSON target automatically with the
`-Z build-std` flag in `.cargo/config.toml`.

### Clone and Build

```bash
git clone https://github.com/ospab/aeterna.git
cd aeterna
bash build.sh
```

### Run

```bash
# Minimal QEMU boot
qemu-system-x86_64 -cdrom isos/ospab-os-v2-103.iso -m 256M -serial stdio

# With RTL8139 networking
qemu-system-x86_64 \
  -cdrom isos/ospab-os-v2-103.iso \
  -m 256M -serial stdio \
  -device rtl8139,netdev=net0 \
  -netdev user,id=net0

# With IDE storage
qemu-system-x86_64 \
  -cdrom isos/ospab-os-v2-103.iso \
  -m 256M -serial stdio \
  -drive file=disk.img,format=raw,if=ide
```

---

## Codebase Layout

| Path | Description |
|---|---|
| `src/main.rs` | Boot entry point — 5-phase init sequence |
| `src/lib.rs` | Crate root — all module declarations |
| `src/terminal.rs` | Interactive terminal, command dispatch |
| `src/installer.rs` | UEFI disk installer (GPT + FAT32 + LFN) |
| `src/doom.rs` | DOOM engine Rust FFI layer |
| `src/fs/` | VFS + RamFS + deferred disk persistence |
| `src/net/` | RTL8139, e1000, Ethernet, ARP, IPv4, ICMP, UDP, SNTP |
| `src/drivers/` | ATA PIO, AHCI SATA, NVMe, AC97, ES1371 |
| `arch/x86_64/` | GDT, IDT, PIC, keyboard, framebuffer, serial, panic |
| `core/` | Scheduler, IPC, syscall dispatch, capability model |
| `mm/` | Physical allocator, heap, VMM |
| `drivers/` | PCI enumeration, storage/video/VirtIO trait stubs |
| `userland/grape/` | Text editor |
| `userland/tomato/` | Package manager |
| `userland/plum/` | Shell |
| `userland/seed/` | Init system |
| `doom_engine/` | doomgeneric C source + freestanding libc shim |

---

## Code Conventions

### Rust

- `#![no_std]` everywhere — no standard library. Use `extern crate alloc` for `Vec`, `String`, `BTreeMap`.
- All public functions and types require doc comments (`///`).
- Unsafe blocks require a `// SAFETY:` comment explaining the invariant being upheld.
- Use `crate::` for internal paths, not `ospab_os::` (except from outside the crate).
- Hardware access functions should be marked `unsafe` and called from a single initialisation site.

### Naming

| Item | Style | Example |
|---|---|---|
| Modules, functions, variables | `snake_case` | `init_driver`, `send_packet` |
| Types, traits, enums | `PascalCase` | `PacketBuffer`, `ServiceState` |
| Constants | `SCREAMING_SNAKE_CASE` | `MAX_PACKET_SIZE` |
| Files | `snake_case.rs` | `rtl8139.rs`, `ramfs.rs` |

### File Organisation

- Each subsystem has its own directory with a `mod.rs` entry point.
- Trait definitions go in a separate `trait.rs` within the same directory.
- Userland tools live in `userland/<tool>/src/lib.rs`.
- Architecture-specific code belongs in `arch/x86_64/`.

### Logging

- All subsystem log messages use a prefix: `[NET]`, `[FS]`, `[ATA]`, `[DOOM]`, etc.
- Messages are terminated with `\r\n` for serial compatibility.
- Boot-phase messages use `klog::boot()` / `klog::err()` for the `dmesg` ring buffer.
- Debug-only traces use `serial::write_str()` directly and should be removed before merging.

---

## Contribution Areas

### High Priority

- **Process isolation** — ELF loader, Ring 3 execution, syscall ABI, `fork`/`exec`/`wait`
- **TCP/IP** — SYN/ACK state machine, connection table, retransmit timer
- **Capability enforcement** — capability table per process, syscall dispatcher integration

### Medium Priority

- **Preemptive scheduler** — TCB, IRQ 0 context switch, priority classes, timer wheel
- **USB stack** — xHCI, HID keyboard, Mass Storage / BOT
- **ext2/FAT32 mount** — read existing filesystems from disk into the VFS

### Good First Contributions

- Add a new terminal command (see `src/terminal.rs` — search for the `match cmd_name` block)
- Improve `tutor` topics with more technical depth
- Add packages to the `tomato` built-in repository
- Write VFS unit tests (no hardware required — RamFS is pure Rust)
- Fix linting: `cargo clippy` and address any new warnings

---

## Workflow

### 1. Fork and create a branch

```bash
git checkout -b feat/my-feature
```

### 2. Develop

- Test with QEMU after every meaningful change.
- Verify `bash build.sh` completes with zero warnings and zero errors.
- Check `dmesg` in the running OS for unexpected log output.

### 3. Commit messages

Use the [Conventional Commits](https://www.conventionalcommits.org/) format:

```
feat(net): implement TCP SYN/ACK state machine
fix(vfs): handle concurrent readdir during deferred sync
refactor(terminal): extract command dispatch to dedicated module
docs: update ROADMAP with Phase 9 progress
chore(build): add strip step with llvm-objcopy
```

Scope examples: `kernel`, `net`, `vfs`, `fs`, `mm`, `arch`, `installer`, `doom`, `terminal`,
`plum`, `grape`, `tomato`, `seed`, `drivers`, `build`, `docs`.

### 4. Open a pull request

Include:
- A clear description of what the change does and why.
- Steps to reproduce / test the behaviour.
- Serial log output (`-serial stdio`) showing the relevant boot phases succeeding.
- Screenshots if the framebuffer output changes.

---

## Testing

Since AETERNA is a bare-metal kernel, all testing is done through QEMU:

```bash
# Build and run
bash build.sh
qemu-system-x86_64 -cdrom isos/ospab-os-v2-103.iso -m 256M -serial stdio
```

**Minimum acceptance checklist before submitting a PR:**

- [ ] All 5 boot phases complete with `[OK]`
- [ ] Shell prompt appears (`root@ospab:~# `)
- [ ] `version` shows correct kernel version
- [ ] `ls /` returns the standard directory tree
- [ ] `ping 10.0.2.2` returns RTT in microseconds (if network changed)
- [ ] `grape /tmp/test.txt` opens, accepts text, saves, exits cleanly (if editor changed)
- [ ] `seed status` shows all 9 services (if init changed)
- [ ] `bash build.sh` produces zero warnings

---

## License

By contributing to AETERNA / ospab.os, you agree that your contributions will be
licensed under the **Boost Software License 1.1** (see [LICENSE](LICENSE)).
