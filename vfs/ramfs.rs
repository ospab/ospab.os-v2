/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

RamFS — In-memory filesystem for AETERNA microkernel.

Design:
  - Uses BTreeMap<String, RamNode> to store the entire directory tree.
  - Every path is stored as an absolute path relative to the mount point.
  - Directories are just RamNode::Dir entries; their children are discovered
    by prefix-matching in the BTreeMap (sorted lexicographically).
  - Files store their data as Vec<u8>.
  - All operations are O(log n) lookup via BTreeMap.
  - Thread safety: uses a spin mutex (single-core for now).

This is NOT a stub — files created here persist in RAM until power-off.
*/

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use super::{DirEntry, FileSystem, NodeType};

// ─── RamFS node types ───────────────────────────────────────────────────────

/// A node in the RamFS tree (public for disk_sync.rs)
pub enum RamNode {
    /// A regular file with byte contents
    File(Vec<u8>),
    /// A directory (children discovered by prefix scan)
    Dir,
}

// ─── Spin lock for RamFS ────────────────────────────────────────────────────

/// Simple spin lock for single-core (no contention, just prevents reentrance)
static LOCK: AtomicBool = AtomicBool::new(false);

/// Set to true in main.rs to indicate the storage layer is ready.
/// Actual sync scheduling now uses IS_DIRTY + deferred_tick().
pub static AUTOSYNC_ENABLED: AtomicBool = AtomicBool::new(false);

/// True when RamFS has unflushed changes.
/// Set by every mutation; cleared by sync_filesystem() after a successful flush.
pub static IS_DIRTY: AtomicBool = AtomicBool::new(false);

fn lock() {
    while LOCK.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
        core::hint::spin_loop();
    }
}

fn unlock() {
    LOCK.store(false, Ordering::Release);
}

// ─── Global RamFS storage ───────────────────────────────────────────────────

/// The actual storage: path → node
/// Paths are absolute within the FS, e.g., "/", "/etc", "/etc/hostname"
static mut TREE: Option<BTreeMap<String, RamNode>> = None;
static mut RAMFS_INIT: bool = false;

/// The singleton RamFS instance (used as &'static dyn FileSystem)
static mut INSTANCE: RamFsInstance = RamFsInstance;

/// ZST that implements FileSystem trait
struct RamFsInstance;

// ─── Initialization ─────────────────────────────────────────────────────────

/// Initialize the RamFS. Call after heap is ready.
/// Creates the root directory "/" and populates it with default files.
pub fn init() {
    lock();
    unsafe {
        let mut tree = BTreeMap::new();

        // Root directory
        tree.insert(String::from("/"), RamNode::Dir);

        // Standard directories
        tree.insert(String::from("/etc"), RamNode::Dir);
        tree.insert(String::from("/tmp"), RamNode::Dir);
        tree.insert(String::from("/home"), RamNode::Dir);
        tree.insert(String::from("/home/root"), RamNode::Dir);
        tree.insert(String::from("/var"), RamNode::Dir);
        tree.insert(String::from("/var/log"), RamNode::Dir);
        tree.insert(String::from("/dev"), RamNode::Dir);
        tree.insert(String::from("/boot"), RamNode::Dir);
        tree.insert(String::from("/proc"), RamNode::Dir);
        tree.insert(String::from("/sys"), RamNode::Dir);
        tree.insert(String::from("/sys/kernel"), RamNode::Dir);
        tree.insert(String::from("/sys/devices"), RamNode::Dir);
        tree.insert(String::from("/info"), RamNode::Dir);
        tree.insert(String::from("/doom"), RamNode::Dir);     // DOOM save dir — persists on disk

        // /etc/hostname
        tree.insert(
            String::from("/etc/hostname"),
            RamNode::File(Vec::from(b"ospab\n" as &[u8])),
        );

        // /etc/os-release
        tree.insert(
            String::from("/etc/os-release"),
            RamNode::File(Vec::from(
                b"NAME=\"ospab.os\"\nVERSION=\"2.0.3\"\nID=ospab\nPRETTY_NAME=\"ospab.os 2.0.3 (AETERNA)\"\nHOME_URL=\"https://github.com/nicorp/ospab-os\"\nBUILD_ID=nightly\n" as &[u8],
            )),
        );

        // /etc/motd (message of the day)
        tree.insert(
            String::from("/etc/motd"),
            RamNode::File(Vec::from(
                b"Welcome to ospab.os (AETERNA Microkernel)\nType 'help' for available commands.\n" as &[u8],
            )),
        );

        // /etc/hosts
        tree.insert(
            String::from("/etc/hosts"),
            RamNode::File(Vec::from(
                b"# AETERNA /etc/hosts\n127.0.0.1\tlocalhost\n10.0.2.2\tgateway router host\n10.0.2.3\tdns nameserver\n216.239.35.0\ttime1.google.com ntp\n216.239.35.4\ttime2.google.com\n162.159.200.1\ttime.cloudflare.com\n" as &[u8],
            )),
        );

        // /etc/timezone
        tree.insert(
            String::from("/etc/timezone"),
            RamNode::File(Vec::from(b"UTC\n" as &[u8])),
        );

        // /info/author.txt
        tree.insert(
            String::from("/info/author.txt"),
            RamNode::File(Vec::from(
                b"AETERNA Microkernel\nAuthor: ospab\nLanguage: Rust (no_std)\nTarget: x86_64\nLicense: BSL-1.1\nRepository: https://github.com/nicorp/ospab-os\n" as &[u8],
            )),
        );

        // /info/welcome.txt
        tree.insert(
            String::from("/info/welcome.txt"),
            RamNode::File(Vec::from(
                b"Welcome to AETERNA!\n\nThis is ospab.os v2.0.3, a microkernel operating system\nwritten from scratch in Rust. This file is stored in RamFS --\na real in-memory filesystem.\n\nTry:\n  ls /       -- directory listing\n  cat /etc/os-release\n  mkdir /tmp/test\n  touch /tmp/test/hello.txt\n  echo Hello, AETERNA! > /tmp/test/hello.txt\n  cat /tmp/test/hello.txt\n" as &[u8],
            )),
        );

        // /boot/KERNEL (metadata, not real binary)
        tree.insert(
            String::from("/boot/KERNEL"),
            RamNode::File(Vec::from(b"AETERNA 2.0.3 x86_64 ELF64\n" as &[u8])),
        );

        // /boot/limine.conf (copy of real config)
        tree.insert(
            String::from("/boot/limine.conf"),
            RamNode::File(Vec::from(
                b"TIMEOUT=0\n:ospab.os\n  PROTOCOL=limine\n  KERNEL_PATH=boot:///KERNEL\n" as &[u8],
            )),
        );

        // /dev entries (special files, size 0, content describes them)
        tree.insert(String::from("/dev/null"), RamNode::File(Vec::new()));
        tree.insert(String::from("/dev/zero"), RamNode::File(Vec::new()));
        tree.insert(String::from("/dev/console"), RamNode::File(Vec::new()));
        tree.insert(String::from("/dev/ttyS0"), RamNode::File(Vec::new()));
        tree.insert(String::from("/dev/fb0"), RamNode::File(Vec::new()));
        tree.insert(String::from("/dev/mem"), RamNode::File(Vec::new()));

        TREE = Some(tree);
        RAMFS_INIT = true;
    }
    unlock();
    crate::arch::x86_64::serial::write_str("[RAMFS] Initialized with default files\r\n");
}

/// Get a static reference to the RamFS instance for mounting
pub fn instance() -> &'static dyn FileSystem {
    unsafe { &INSTANCE }
}

/// Check if RamFS is initialized
pub fn is_initialized() -> bool {
    unsafe { RAMFS_INIT }
}

// ─── Helper: normalize path ─────────────────────────────────────────────────

/// Normalize a path: remove trailing slashes, ensure starts with "/"
fn normalize(path: &str) -> String {
    if path.is_empty() || path == "/" {
        return String::from("/");
    }
    let mut p = String::from(path);
    // Ensure starts with /
    if !p.starts_with('/') {
        p.insert(0, '/');
    }
    // Remove trailing slash
    while p.len() > 1 && p.ends_with('/') {
        p.pop();
    }
    p
}

/// Extract the filename (last component) from a path
fn filename(path: &str) -> &str {
    match path.rfind('/') {
        Some(pos) if pos < path.len() - 1 => &path[pos + 1..],
        _ => path,
    }
}

/// Extract the parent directory from a path
fn parent(path: &str) -> String {
    if path == "/" {
        return String::from("/");
    }
    match path.rfind('/') {
        Some(0) => String::from("/"),
        Some(pos) => String::from(&path[..pos]),
        None => String::from("/"),
    }
}

// ─── FileSystem trait implementation ────────────────────────────────────────

unsafe impl Send for RamFsInstance {}
unsafe impl Sync for RamFsInstance {}

impl FileSystem for RamFsInstance {
    fn name(&self) -> &str {
        "ramfs"
    }

    fn read_file(&self, path: &str) -> Option<Vec<u8>> {
        let path = normalize(path);
        lock();
        let result = unsafe {
            let tree = TREE.as_ref()?;
            match tree.get(&path) {
                Some(RamNode::File(data)) => Some(data.clone()),
                _ => None,
            }
        };
        unlock();
        result
    }

    fn write_file(&self, path: &str, data: &[u8]) -> bool {
        let path = normalize(path);
        lock();
        let ok = unsafe {
            match TREE.as_mut() {
                Some(tree) => {
                    // Ensure parent directory exists
                    let par = parent(&path);
                    if par != "/" && !tree.contains_key(&par) {
                        // Auto-create parent dirs
                        self.mkdir_internal(tree, &par);
                    }
                    tree.insert(path, RamNode::File(Vec::from(data)));
                    true
                }
                None => false,
            }
        };
        unlock();
        if ok { crate::fs::disk_sync::mark_dirty(); }
        ok
    }

    fn append_file(&self, path: &str, data: &[u8]) -> bool {
        let path = normalize(path);
        lock();
        let ok = unsafe {
            match TREE.as_mut() {
                Some(tree) => {
                    match tree.get_mut(&path) {
                        Some(RamNode::File(existing)) => {
                            existing.extend_from_slice(data);
                            true
                        }
                        _ => {
                            // File doesn't exist — create it
                            let par = parent(&path);
                            if par != "/" && !tree.contains_key(&par) {
                                self.mkdir_internal(tree, &par);
                            }
                            tree.insert(path, RamNode::File(Vec::from(data)));
                            true
                        }
                    }
                }
                None => false,
            }
        };
        unlock();
        if ok { crate::fs::disk_sync::mark_dirty(); }
        ok
    }

    fn readdir(&self, path: &str) -> Option<Vec<DirEntry>> {
        let path = normalize(path);
        lock();
        let result = unsafe {
            let tree = TREE.as_ref()?;

            // Check that the directory itself exists
            match tree.get(&path) {
                Some(RamNode::Dir) => {}
                None if path == "/" => {} // root always exists
                _ => { unlock(); return None; }
            }

            let mut entries = Vec::new();
            let prefix = if path == "/" {
                String::from("/")
            } else {
                let mut p = path.clone();
                p.push('/');
                p
            };

            for (key, node) in tree.iter() {
                if key == "/" { continue; }
                // Must be a direct child: starts with prefix and has no more '/' after that
                if key.starts_with(prefix.as_str()) {
                    let rest = &key[prefix.len()..];
                    // Direct child has no '/' in the remaining part
                    if !rest.is_empty() && !rest.contains('/') {
                        let (ntype, size) = match node {
                            RamNode::File(data) => (NodeType::File, data.len()),
                            RamNode::Dir => (NodeType::Directory, 0),
                        };
                        entries.push(DirEntry {
                            name: String::from(rest),
                            node_type: ntype,
                            size,
                        });
                    }
                }
            }

            // Sort entries: directories first, then alphabetical
            entries.sort_by(|a, b| {
                match (a.node_type, b.node_type) {
                    (NodeType::Directory, NodeType::File) => core::cmp::Ordering::Less,
                    (NodeType::File, NodeType::Directory) => core::cmp::Ordering::Greater,
                    _ => a.name.cmp(&b.name),
                }
            });

            Some(entries)
        };
        unlock();
        result
    }

    fn mkdir(&self, path: &str) -> bool {
        let path = normalize(path);
        lock();
        let ok = unsafe {
            match TREE.as_mut() {
                Some(tree) => self.mkdir_internal(tree, &path),
                None => false,
            }
        };
        unlock();
        if ok { crate::fs::disk_sync::mark_dirty(); }
        ok
    }

    fn touch(&self, path: &str) -> bool {
        let path = normalize(path);
        lock();
        let ok = unsafe {
            match TREE.as_mut() {
                Some(tree) => {
                    if tree.contains_key(&path) {
                        true // already exists
                    } else {
                        let par = parent(&path);
                        if par != "/" && !tree.contains_key(&par) {
                            self.mkdir_internal(tree, &par);
                        }
                        tree.insert(path, RamNode::File(Vec::new()));
                        true
                    }
                }
                None => false,
            }
        };
        unlock();
        if ok { crate::fs::disk_sync::mark_dirty(); }
        ok
    }

    fn exists(&self, path: &str) -> bool {
        let path = normalize(path);
        lock();
        let result = unsafe {
            match TREE.as_ref() {
                Some(tree) => tree.contains_key(&path),
                None => false,
            }
        };
        unlock();
        result
    }

    fn stat(&self, path: &str) -> Option<DirEntry> {
        let path = normalize(path);
        lock();
        let result = unsafe {
            let tree = TREE.as_ref()?;
            match tree.get(&path) {
                Some(RamNode::File(data)) => Some(DirEntry {
                    name: String::from(filename(&path)),
                    node_type: NodeType::File,
                    size: data.len(),
                }),
                Some(RamNode::Dir) => Some(DirEntry {
                    name: String::from(filename(&path)),
                    node_type: NodeType::Directory,
                    size: 0,
                }),
                None => None,
            }
        };
        unlock();
        result
    }

    fn remove(&self, path: &str) -> bool {
        let path = normalize(path);
        if path == "/" {
            return false; // Can't remove root
        }
        lock();
        let ok = unsafe {
            match TREE.as_mut() {
                Some(tree) => {
                    // If it's a directory, check it's empty
                    if let Some(RamNode::Dir) = tree.get(&path) {
                        let prefix = {
                            let mut p = path.clone();
                            p.push('/');
                            p
                        };
                        let has_children = tree.keys().any(|k| k.starts_with(prefix.as_str()));
                        if has_children {
                            unlock();
                            return false; // Directory not empty
                        }
                    }
                    tree.remove(&path).is_some()
                }
                None => false,
            }
        };
        unlock();
        if ok { crate::fs::disk_sync::mark_dirty(); }
        ok
    }
}

impl RamFsInstance {
    /// Create directory and all missing parents (internal, must hold lock)
    unsafe fn mkdir_internal(&self, tree: &mut BTreeMap<String, RamNode>, path: &str) -> bool {
        if path == "/" || path.is_empty() {
            return true;
        }
        // Create parents first
        let par = parent(path);
        if par != "/" && !tree.contains_key(&par) {
            self.mkdir_internal(tree, &par);
        }
        if !tree.contains_key(path) {
            tree.insert(String::from(path), RamNode::Dir);
        }
        true
    }
}

// ─── Proc filesystem data generator ─────────────────────────────────────────
// These are synthetic files: their content is generated on each read.
// We inject them into the VFS mount at /proc by using a separate ProcFS-like
// pattern, OR we can use the RamFS and refresh /proc/* files periodically.
//
// For now, we provide a function to refresh /proc files with live data.

/// Update /proc files with live kernel data.
/// Call this before reading /proc/* to get fresh numbers.
pub fn refresh_proc_files() {
    lock();
    unsafe {
        if let Some(tree) = TREE.as_mut() {
            // /proc/version
            tree.insert(
                String::from("/proc/version"),
                RamNode::File(Vec::from(b"AETERNA 2.0.3 ospab.os x86_64 AETERNA/Microkernel\n" as &[u8])),
            );

            // /proc/cpuinfo
            tree.insert(
                String::from("/proc/cpuinfo"),
                RamNode::File(Vec::from(
                    b"processor\t: 0\nvendor\t\t: AETERNA\nmodel name\t: QEMU Virtual CPU\ncpu MHz\t\t: 0.000\ncache size\t: 0 KB\n" as &[u8],
                )),
            );

            // /proc/meminfo — uses real data from PMM and heap
            {
                let stats = crate::mm::physical::stats();
                let (heap_used, heap_free) = if crate::mm::heap::is_initialized() {
                    crate::mm::heap::stats()
                } else {
                    (0, 0)
                };
                let mut buf = Vec::with_capacity(256);
                buf.extend_from_slice(b"MemTotal:        ");
                push_dec(&mut buf, stats.total_bytes / 1024);
                buf.extend_from_slice(b" kB\nMemUsable:       ");
                push_dec(&mut buf, stats.usable_bytes / 1024);
                buf.extend_from_slice(b" kB\nMemReserved:     ");
                push_dec(&mut buf, stats.reserved_bytes / 1024);
                buf.extend_from_slice(b" kB\nHeapUsed:        ");
                push_dec(&mut buf, heap_used as u64 / 1024);
                buf.extend_from_slice(b" kB\nHeapFree:        ");
                push_dec(&mut buf, heap_free as u64 / 1024);
                buf.extend_from_slice(b" kB\n");
                tree.insert(String::from("/proc/meminfo"), RamNode::File(buf));
            }

            // /proc/uptime
            {
                let ticks = crate::arch::x86_64::idt::timer_ticks();
                let secs = ticks / 100;
                let mut buf = Vec::with_capacity(32);
                push_dec(&mut buf, secs);
                buf.extend_from_slice(b".00 ");
                push_dec(&mut buf, secs);
                buf.extend_from_slice(b".00\n");
                tree.insert(String::from("/proc/uptime"), RamNode::File(buf));
            }

            // /proc/net (if network is up)
            if crate::net::is_up() {
                let ip = crate::net::OUR_IP;
                let mut buf = Vec::with_capacity(128);
                buf.extend_from_slice(b"Interface: eth0\nDriver: ");
                buf.extend_from_slice(crate::net::nic_name().as_bytes());
                buf.extend_from_slice(b"\nIP: ");
                push_ip(&mut buf, ip);
                buf.extend_from_slice(b"\nStatus: UP\n");
                tree.insert(String::from("/proc/net"), RamNode::File(buf));
            }
        }
    }
    unlock();
}

// ─── Helpers ────────────────────────────────────────────────────────────────

/// Push a decimal number as ASCII into a Vec
fn push_dec(buf: &mut Vec<u8>, val: u64) {
    if val == 0 {
        buf.push(b'0');
        return;
    }
    let mut digits = [0u8; 20];
    let mut n = val;
    let mut i = 0;
    while n > 0 {
        digits[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    for j in (0..i).rev() {
        buf.push(digits[j]);
    }
}

/// Push an IP address as ASCII into a Vec
fn push_ip(buf: &mut Vec<u8>, ip: [u8; 4]) {
    for i in 0..4 {
        push_dec(buf, ip[i] as u64);
        if i < 3 {
            buf.push(b'.');
        }
    }
}

/// Get number of files/dirs in the RamFS
pub fn node_count() -> usize {
    lock();
    let count = unsafe {
        match TREE.as_ref() {
            Some(tree) => tree.len(),
            None => 0,
        }
    };
    unlock();
    count
}
/// Get a copy of the entire RamFS tree (for serialization)
pub fn get_tree_copy() -> Option<BTreeMap<String, RamNode>> {
    lock();
    let result = unsafe {
        match TREE.as_ref() {
            Some(tree) => {
                let mut copy = BTreeMap::new();
                for (path, node) in tree.iter() {
                    let node_copy = match node {
                        RamNode::Dir => RamNode::Dir,
                        RamNode::File(data) => RamNode::File(data.clone()),
                    };
                    copy.insert(alloc::string::String::from(path), node_copy);
                }
                Some(copy)
            }
            None => None,
        }
    };
    unlock();
    result
}

/// Restore RamFS tree from serialized data (boot recovery)
pub fn restore_from_tree(tree: BTreeMap<String, RamNode>) {
    lock();
    unsafe {
        // Clear existing tree (keeping root)
        if let Some(old_tree) = TREE.take() {
            drop(old_tree);
        }
        
        // Restore from provided tree
        TREE = Some(tree);
    }
    unlock();
}
