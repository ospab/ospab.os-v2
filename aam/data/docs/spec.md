# AETERNA — Технічна Специфікація

## 1. Kernel Specification

### CPU Architecture
- **Target**: x86_64
- **Features**: SSE (required), AVX (optional)
- **Boot**: Limine protocol (v10.8.2), BIOS + UEFI hybrid
- **Execution Mode**: Long mode, 64-bit paging

### Memory Layout
```
┌─────────────────────────────────────────────────────────────┐
│ High Kernel Space (HHDM)                                     │
│ - Virtual address offset: bootloader-dependent               │
│ - Physical memory mapped 1:1                                 │
│ - Kernel stack: 4 MiB (autogrow)                            │
│ - Kernel heap: 128 MiB (linked_list_allocator)              │
│ - Driver buffers, page tables, etc.                         │
└─────────────────────────────────────────────────────────────┘
│ 0x0000_0000_0000_0000 — User space (if implemented)         │
└─────────────────────────────────────────────────────────────┘
```

### Boot Phases
1. **Stage 1**: CPU feature detection, GDT setup, interrupt handling (IDT)
2. **Stage 2**: Memory map parsing, heap initialization
3. **Stage 3**: Driver probe (ATA, AHCI, PS/2, PCI, NIC)
4. **Stage 4**: VFS mount, RamFS init, persistent storage sync
5. **Stage 5**: Shell prompt

### Interrupt Handling
- **PIC**: 8259A programmable interrupt controller
- **IRQ 0**: Timer (PIT 100 Hz, 10 ms ticks)
- **IRQ 1**: PS/2 keyboard
- **IRQ 6**: Floppy (unused)
- **IRQ 8-15**: SLAVE PIC (e1000, AHCI, XHCI)

### Timer & Scheduling
- **PIT**: Generates IRQ 0 every ~10 ms (100 Hz clock)
- **Ticks**: Monotonic counter (u64), never resets during runtime
- **Scheduler**: Currently static (seed.rs), future: preemptive with TCB per process

---

## 2. Disk Layout (AHCI / NVMe)

### Master Boot Record (MBB)
```
LBA 0:      MBR signature (0x55AA)
LBA 1:      Limine SPL (secondary program loader)
LBA 2-N:    Persistent RamFS snapshot (AeternaFS format)
LBA N+1-:   Free space / future partitions
```

### AeternaFS Snapshot Format
```
Header (64 bytes):
  - Magic: "AEFS" (0x53465341)
  - Version: u32 = 1
  - NumNodes: u32
  - Checksum: u32 (CRC32)
  - Reserved: 48 bytes

Node List (per node):
  - NameLen: u16
  - Name: [u8; NameLen]
  - Type: u8 (0=dir, 1=file, 2=symlink)
  - Size: u32 (file size in bytes)
  - Mtime: u32 (unix timestamp)
  - Data: [u8; Size]
```

### Sync Strategy
- **Trigger**: Deferred tick (every 182 timer ticks ≈ 10s)
- **Write**: Linear scan of RamFS tree, serialize to LBA 2
- **Safety**: CRC32 verification before commit

---

## 3. Virtual Filesystem (VFS)

### Mount Points
```
/ (root)
├── RamFS (in-memory)
│   ├── /doom
│   ├── /etc
│   ├── /var
│   └── /tmp
```

### File Operations
```rust
pub fn open(path: &str, mode: OpenMode) -> io::Result<Fd>;
pub fn read_slice(fd: Fd, buf: &mut [u8]) -> io::Result<usize>;
pub fn write_slice(fd: Fd, buf: &[u8]) -> io::Result<usize>;
pub fn close(fd: Fd) -> io::Result<()>;
pub fn seek(fd: Fd, offset: i32, whence: SeekWhence) -> io::Result<u32>;
pub fn readdir(path: &str) -> io::Result<Option<Vec<DirEntry>>>;
pub fn mkdir(path: &str) -> io::Result<()>;
pub fn rmdir(path: &str) -> io::Result<()>;
pub fn unlink(path: &str) -> io::Result<()>;
pub fn rename(old: &str, new: &str) -> io::Result<()>;
```

### Handle Multiplexing
- **Per-process FD table**: Vec<InodeHandle>
- **Ring buffer**: 256 max open file descriptors per process
- **Inode cache**: BTreeMap<Path, Inode> (in-memory)

---

## 4. Device Drivers

### ATA PIO (Legacy IDE)
- **Registers**: Primary 0x1F0, Secondary 0x170
- **Commands**: READ, WRITE (CHS/LBA mode)
- **Polling**: Status register wait loop
- **Max**: 2 drives per controller

### AHCI SATA
- **Discovery**: PCI device class 0x0106
- **Ports**: Up to 32 per controller
- **Commands**: Native Command Queue (NCQ)
- **Interrupts**: MSI or INTx

### PS/2 Keyboard
- **Port**: 0x60 (data), 0x64 (status/control)
- **IRQ**: 1
- **Encoding**: Scancode set 2
- **Features**: Auto-repeat, LED control

### e1000 (Intel 1GbE)
- **MAC**: PCIe device
- **RX/TX rings**: 256-entry ring buffers
- **MTU**: 1500 bytes
- **Features**: Checksums, VLAN tagging

### RTL8139 (Realtek 100Mbps)
- **MAC**: PCIe device
- **RX ring**: 8 KiB circular buffer
- **TX slots**: 4 × 2 KiB
- **Features**: Wake-on-LAN capable

---

## 5. AETERNA AI Model (AAM)

### Architecture
```
Input token (u32)
    ↓
Embedding layer (256 tokens × 64 dims)
    ↓
Attention block:
  - Query/Key/Value projections (64×64 matmul)
  - Scaled dot-product: Q·K^T / sqrt(d_k)
  - Softmax (numerically stable)
  - Value weighting
    ↓
Residual connection (elem-wise add)
    ↓
Feed-forward network:
  - Hidden: 64→256 (ReLU)
  - Output: 256→64 (linear)
    ↓
Output projection (64→256 logits)
    ↓
Top-k sampling (k=5, argmax with jitter)
    ↓
Next token (u32)
```

### Tokenizer
- **Type**: Byte-level (256 tokens)
- **Encoding**: UTF-8 byte stream
- **Decoding**: Byte→char with fallback '?'

### Weight Storage
- **Format**: Binary (f32 per weight)
- **Sizes**:
  - Embedding: 256 × 64 × 4 = 64 KiB
  - Attention QKV: 3 × 64 × 64 × 4 = 48 KiB
  - FFN: (64 × 256 + 256 × 64) × 4 = 130 KiB
  - Total: ~280 KiB (fits in L3 cache)

### Inference Ops
- **Matrix multiply**: Manual loop unroll
- **Activation**: ReLU = max(x, 0)
- **Normalization**: Numerically-stable softmax (max-subtract)
- **Sampling**: Linear congruential RNG for randomness

### API
```rust
pub struct AeternaAiModel { ... }

impl AeternaAiModel {
    pub fn new(vocab_size, hidden_dim) -> Self;
    pub fn generate_step(prompt: &str) -> String;
    pub fn generate_sequence(prompt: &str, length) -> String;
    pub fn encode(input: &str) -> Vec<u32>;
    pub fn decode(tokens: &[u32]) -> String;
}
```

---

## 6. Retrieval-Augmented Generation (RAG)

### Data Sources
```
/aam/data/docs/
  - manifesto.txt     (Core philosophy, tokenized at startup)
  - spec.md          (This file, parsed as context)
  - custom.txt       (User-provided knowledge base)

/aam/data/training/
  - dataset.jsonl    (Prompt/completion pairs for fine-tuning)
```

### Chunking Strategy
- **Chunk size**: 128 tokens (≈ 100 bytes UTF-8)
- **Overlap**: 32 tokens
- **Indexing**: Simple BM25 (term frequency weighting)

### Query Flow
1. User prompt → tokenize
2. BM25 search in chunks (top-3 results)
3. Append relevant chunks to context window
4. AAM forward pass on expanded prompt
5. Output next-token probability

### Memory Footprint
- Dataset: ~10 MB docs, 256 chunks
- Index: 2 KB (chunk metadata)
- Runtime cache: 64 KiB (sliding window)

---

## 7. Shell Command Set

### File Operations
```
ls [PATH]               - List directory
cat [FILE]              - Print file contents
cat [FILE] | less       - Paged output (WIP)
echo [TEXT]             - Print text
echo [TEXT] > FILE      - Redirect to file
mkdir [-p] [PATH]       - Create directory
touch [FILE]            - Create empty file
rm [FILE]               - Delete file
rmdir [DIR]             - Delete empty directory
cp [SRC] [DST]          - Copy file (WIP)
mv [SRC] [DST]          - Move/rename file (WIP)
```

### System Info
```
meminfo                 - Memory statistics
uptime                  - System uptime
dmesg                   - Kernel log
ps                      - Process listing (WIP)
top                     - Process monitor (WIP)
```

### Misc
```
clear                   - Clear screen
sync                    - Force filesystem sync
reboot                  - Restart system
doom                    - Launch DOOM port
aai [PROMPT]            - AI assistant (WIP)
```

---

## 8. Syscall Interface (Future)

### Process Management
```
sys_exec(path, argv) -> pid
sys_exit(code)
sys_wait(pid) -> exit_code
sys_fork() -> pid
sys_get_pid() -> pid
```

### Memory
```
sys_mmap(addr, size, prot, flags) -> ptr
sys_munmap(ptr, size)
sys_brk(new_brk) -> old_brk
```

### Filesystem
```
sys_open(path, flags, mode)
sys_read(fd, buf, count)
sys_write(fd, buf, count)
sys_close(fd)
sys_stat(path) -> stat_t
sys_readdir(fd)
```

### I/O
```
sys_ioctl(fd, cmd, arg)
sys_poll(fds, timeout)
```

### Capability-based Security
```
sys_grant_cap(pid, cap_id)
sys_revoke_cap(pid, cap_id)
sys_check_cap(cap_id) -> bool
```

---

## 9. Performance Targets

| Metric | Target | Status |
|--------|--------|--------|
| Boot time | < 2 s | ✓ |
| Shell prompt latency | < 100 ms | ✓ |
| File read (1 MB) | < 50 ms | ✓ |
| AAM inference (1 token) | < 10 ms | ⚠️ (64 hidden, 100 Hz timer) |
| DOOM frame rate | 30+ FPS | ✓ |
| Memory usage | < 256 MiB | ✓ |

---

## 10. Known Limitations & Future Work

### Current
- Single-core only (scheduler is static)
- No preemption (cooperative multitasking)
- RamFS only (no persistent partitions yet)
- AAM is tiny (64-dim), for demo purposes

### Planned
- **Preemptive scheduler**: Context switch on IRQ 0
- **Multi-core**: SMP via APIC
- **TCP/IP stack**: Full network support
- **Userland processes**: Capability-based security model
- **GPU support**: NVIDIA PTX offload for larger models
- **Persistent storage**: GPT partition table, ext4-like filesystem

---

**Last Updated**: 2026-03-04
**Author**: ospab team
