# Contributing to ospab.os

Thank you for your interest in contributing to **ospab.os v2.1.0**! This document describes the guidelines and workflow for contributing to the project.

---

## Getting Started

### Prerequisites
- **Rust nightly** toolchain (`rustup default nightly`)
- **xorriso** and **mtools** for ISO generation
- **QEMU** for testing (`qemu-system-x86_64`)
- **WSL** or Linux environment for building

### Building
```bash
git clone https://github.com/user/ospab.os-v2.git
cd ospab.os-v2
bash build.sh
```

### Running
```bash
qemu-system-x86_64 \
  -cdrom isos/ospab-os-v2-19.iso \
  -m 256M \
  -serial stdio \
  -device rtl8139,netdev=net0 \
  -netdev user,id=net0
```

---

## Project Architecture

The codebase is organized into the following key areas:

| Area | Path | Description |
|------|------|-------------|
| Kernel core | `src/main.rs`, `src/lib.rs` | Boot sequence, module declarations |
| HAL | `arch/x86_64/` | GDT, IDT, PIC, keyboard, framebuffer |
| Memory | `mm/` | Physical allocator, heap, VMM |
| Filesystem | `src/fs/` | VFS + RamFS implementation |
| Networking | `src/net/` | RTL8139, Ethernet, ARP, IPv4, ICMP, UDP |
| Terminal | `src/terminal.rs` | Command dispatch, 28+ commands |
| Userland | `userland/` | grape, tomato, plum, seed |
| Drivers | `src/drivers/`, `drivers/` | ATA, AHCI, PCI |

---

## Contribution Areas

### High Priority
- **Process isolation** — ELF loader, Ring 3 execution, IPC
- **RTL8139 RX fix** — receive path returns CBR=0, needs DMA alignment investigation
- **TCP/IP stack** — currently only UDP is implemented
- **ext2/FAT filesystem** — persistent storage beyond RamFS

### Medium Priority
- **Additional NIC drivers** — e1000, VirtIO-net
- **USB stack** — UHCI/EHCI/xHCI
- **Sound driver** — AC97 or Intel HDA
- **Graphics** — framebuffer double buffering, window manager prototype

### Good First Issues
- Add new terminal commands
- Improve `tutor` topics with more content
- Add new packages to `tomato` repository
- Improve `plum` shell scripting (loops, conditionals)
- Write unit tests for VFS operations

---

## Code Style

### Rust Guidelines
- Use `#![no_std]` — no standard library
- All allocations go through `alloc` crate (`Vec`, `String`, `BTreeMap`)
- Use `crate::` for internal references (not `ospab_os::`)
- Unsafe code is acceptable for hardware access, but must be documented
- All public functions need doc comments

### Naming Conventions
- **Modules**: `snake_case` (e.g., `src/net/rtl8139.rs`)
- **Types**: `PascalCase` (e.g., `PacketBuffer`, `ServiceStatus`)
- **Functions**: `snake_case` (e.g., `init_driver`, `send_packet`)
- **Constants**: `SCREAMING_SNAKE_CASE` (e.g., `MAX_PACKET_SIZE`)

### File Organization
- Each subsystem has its own directory with `mod.rs`
- Userland tools live in `userland/<tool>/src/lib.rs`
- Userland modules are included via `#[path]` in `src/lib.rs`
- Trait definitions go in separate `trait.rs` files

---

## Workflow

### 1. Fork and branch
```bash
git checkout -b feature/my-feature
```

### 2. Make changes
- Write code following the style guide
- Test with QEMU
- Ensure `bash build.sh` succeeds without errors

### 3. Commit
Use clear, descriptive commit messages:
```
feat(net): add TCP SYN/ACK handshake
fix(vfs): handle nested directory creation
refactor(terminal): extract command dispatch to separate module
docs: update README with new commands
```

### 4. Submit
Open a pull request with:
- Description of changes
- Testing steps
- Screenshots/logs if applicable

---

## Testing

Since ospab.os is a bare-metal OS, testing is done through QEMU:

```bash
# Build and run
bash build.sh
qemu-system-x86_64 -cdrom isos/ospab-os-v2-19.iso -m 256M -serial stdio

# With networking
qemu-system-x86_64 \
  -cdrom isos/ospab-os-v2-19.iso \
  -m 256M \
  -serial stdio \
  -device rtl8139,netdev=net0 \
  -netdev user,id=net0

# With storage
qemu-system-x86_64 \
  -cdrom isos/ospab-os-v2-19.iso \
  -m 256M \
  -serial stdio \
  -drive file=disk.img,format=raw,if=ide
```

### What to test
- Boot completes through all 5 phases
- Terminal responds to commands
- `grape <file>` — editor opens, can type, save, exit
- `tomato -S base` — package installs
- `seed status` — shows services
- `export VAR=test && echo $VAR` — shell variables work

---

## Communication

- Issues: Use GitHub Issues for bug reports and feature requests
- PRs: All contributions go through pull requests
- Documentation: Update README/docs when adding features

---

## License

By contributing to ospab.os, you agree that your contributions will be licensed under the **Boost Software License 1.1** (see [LICENSE](LICENSE)).

---

*Thank you for helping build ospab.os!*
