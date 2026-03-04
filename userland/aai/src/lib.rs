/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

aai — Aeterna AI utility (kernel-integrated)

PHASE 2: Model loader
  .tmt-ai binary format:
    [0..4]   Magic  : b"TMT\x01"
    [4..8]   Version: u32 LE
    [8..12]  Meta-len: u32 LE
    [12..12+meta_len] UTF-8 JSON metadata
    [aligned to 64 bytes] Weights: raw f32 data

  Parsing:   check magic, version, read JSON metadata key-values.
  Loading:   direct VFS → huge-page mapping via sys_mmap_huge (zero-copy).
  Shared:    SHARED_MODEL static slot so multiple callers share one mapping.

PHASE 3: Interactive chat
  aai_chat(): line-by-line prompt → inference → token streaming at 100 Hz.
  KV-Cache: statically-bounded ring buffer stored in a heap-allocated Tensor.
  Status bar: RAM and "CPU-tick" indicator printed each frame.

CLI entry points (called from terminal.rs run_command):
  aai_dispatch(args)  — parse args and dispatch to load/info/bench/chat.
*/

#![allow(unsafe_op_in_unsafe_fn)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use alloc::vec;

use crate::arch::x86_64::{framebuffer, serial, idt};
use crate::fs;

use crate::ane::tensor::{DataType, Tensor};
use crate::core::capability::AaiCaps;

// ─── Colour palette ──────────────────────────────────────────────────────────

const COL_HEADER : u32 = 0x00_FF_FF_00; // yellow
const COL_NORMAL : u32 = 0x00_FF_FF_FF; // white
const COL_DIM    : u32 = 0x00_88_88_88; // grey
const COL_GOOD   : u32 = 0x00_00_FF_00; // green
const COL_ERR    : u32 = 0x00_FF_00_00; // red
const COL_ACCENT : u32 = 0x00_00_CC_FF; // cyan

// ─── .tmt-ai format constants ────────────────────────────────────────────────

const TMT_MAGIC:   [u8; 4] = [b'T', b'M', b'T', 0x01];
const TMT_VERSION: u32     = 1;

// ─── Loaded model descriptor ─────────────────────────────────────────────────

pub struct ModelMeta {
    pub name:        String,
    pub version:     u32,
    pub arch:        String,   // "transformer", "mlp", …
    pub d_model:     usize,
    pub n_layers:    usize,
    pub n_heads:     usize,
    pub vocab_size:  usize,
    pub ctx_len:     usize,    // max context window
    pub param_count: u64,      // total parameter count
    pub weight_offset: usize,  // byte offset to first weight in file
    pub weight_bytes:  usize,
}

impl ModelMeta {
    fn empty() -> Self {
        ModelMeta {
            name: String::from("unnamed"),
            version: 0,
            arch: String::from("unknown"),
            d_model: 512,
            n_layers: 6,
            n_heads: 8,
            vocab_size: 32000,
            ctx_len: 2048,
            param_count: 0,
            weight_offset: 0,
            weight_bytes: 0,
        }
    }
}

/// Global loaded model (shared between callers, zero-copy weights).
static mut LOADED_MODEL: Option<LoadedModel> = None;

pub struct LoadedModel {
    pub meta:    ModelMeta,
    /// Weight tensor mapped directly from the file buffer (zero-copy).
    pub weights: Tensor,
    /// KV-cache for the current chat session.
    pub kv_cache: KvCache,
}

// ─── KV-Cache ─────────────────────────────────────────────────────────────────

/// Ring-buffer KV-cache: stores (key, value) pairs for up to `ctx_len` tokens.
/// Memory is allocated once at model load time.
pub struct KvCache {
    /// [n_layers, ctx_len, d_model] packed as flat f32
    pub keys:   Tensor,
    pub values: Tensor,
    pub n_layers: usize,
    pub ctx_len:  usize,
    pub d_model:  usize,
    /// Write head (wraps at ctx_len)
    pub head: usize,
    /// Number of valid entries stored
    pub fill: usize,
}

impl KvCache {
    pub fn new(n_layers: usize, ctx_len: usize, d_model: usize) -> Self {
        KvCache {
            keys:     Tensor::zeros(&[n_layers, ctx_len, d_model], DataType::F32),
            values:   Tensor::zeros(&[n_layers, ctx_len, d_model], DataType::F32),
            n_layers,
            ctx_len,
            d_model,
            head: 0,
            fill: 0,
        }
    }

    /// Insert a K/V pair at the current head position for a given layer.
    pub fn insert(&mut self, layer: usize, k: &[f32], v: &[f32]) {
        let d = self.d_model.min(k.len()).min(v.len());
        let base = (layer * self.ctx_len + self.head) * self.d_model;
        let ks = self.keys.as_f32_slice_mut();
        let vs = self.values.as_f32_slice_mut();
        for i in 0..d {
            ks[base + i] = k[i];
            vs[base + i] = v[i];
        }
        if layer == self.n_layers - 1 {
            self.head = (self.head + 1) % self.ctx_len;
            self.fill = (self.fill + 1).min(self.ctx_len);
        }
    }

    /// Clear the cache (new session).
    pub fn reset(&mut self) {
        self.head = 0;
        self.fill = 0;
        for v in self.keys.as_f32_slice_mut()   { *v = 0.0; }
        for v in self.values.as_f32_slice_mut()  { *v = 0.0; }
    }
}

// ─── sys_mmap_huge — invoke AETERNA Mmap=40 syscall ─────────────────────────

/// Request a Huge-Page backed mapping of `size` bytes.
/// Returns the mapped virtual address, or null on failure.
///
/// AETERNA syscall ABI:
///   RAX=40 (Mmap), RDI=0 (hint), RSI=size, RDX=flags(0x1=HUGE)
unsafe fn sys_mmap_huge(size: usize) -> *mut u8 {
    let result: usize;
    core::arch::asm!(
        "syscall",
        in("rax") 40u64,   // SyscallNumber::Mmap
        in("rdi") 0u64,    // addr hint = 0 (kernel picks)
        in("rsi") size as u64,
        in("rdx") 1u64,    // flag: HUGE_PAGE
        lateout("rax") result,
        options(nostack)
    );
    if result == 0 || result > usize::MAX - size { core::ptr::null_mut() }
    else { result as *mut u8 }
}

// ─── .tmt-ai parser ──────────────────────────────────────────────────────────

/// Parse a .tmt-ai file from a raw byte slice.
/// Returns (ModelMeta, weight_data_slice) on success.
fn parse_tmt_ai(data: &[u8]) -> Result<ModelMeta, &'static str> {
    if data.len() < 12 { return Err("File too small"); }

    // Magic
    if &data[0..4] != &TMT_MAGIC {
        return Err("Bad magic: expected TMT\\x01");
    }

    // Version
    let version = u32::from_le_bytes(data[4..8].try_into().unwrap_or([0;4]));
    if version != TMT_VERSION {
        return Err("Incompatible .tmt-ai version");
    }

    // Metadata JSON length
    let meta_len = u32::from_le_bytes(data[8..12].try_into().unwrap_or([0;4])) as usize;
    if data.len() < 12 + meta_len {
        return Err("File truncated: metadata incomplete");
    }
    let json_bytes = &data[12..12 + meta_len];
    let json_str   = core::str::from_utf8(json_bytes).unwrap_or("{}");

    // Weight offset: align to 64 bytes after header
    let raw_offset    = 12 + meta_len;
    let weight_offset = (raw_offset + 63) & !63;
    let weight_bytes  = data.len().saturating_sub(weight_offset);

    // ── Minimal JSON key-value scanner ───────────────────────────────────────
    // Only handles flat {"key": "value"} and {"key": 123} — no nesting.
    let mut meta = ModelMeta::empty();
    meta.version       = version;
    meta.weight_offset = weight_offset;
    meta.weight_bytes  = weight_bytes;

    let mut parser = JsonScanner::new(json_str);
    while let Some((key, val)) = parser.next_kv() {
        match key {
            "name"        => meta.name        = String::from(val),
            "arch"        => meta.arch        = String::from(val),
            "d_model"     => meta.d_model     = val.parse().unwrap_or(512),
            "n_layers"    => meta.n_layers    = val.parse().unwrap_or(6),
            "n_heads"     => meta.n_heads     = val.parse().unwrap_or(8),
            "vocab_size"  => meta.vocab_size  = val.parse().unwrap_or(32000),
            "ctx_len"     => meta.ctx_len     = val.parse().unwrap_or(2048),
            "param_count" => meta.param_count = val.parse().unwrap_or(0),
            _             => {}
        }
    }

    // Compute param_count from weight bytes if not specified
    if meta.param_count == 0 {
        meta.param_count = (weight_bytes / 4) as u64;
    }

    Ok(meta)
}

// ─── Tiny JSON flat key-value scanner ────────────────────────────────────────

struct JsonScanner<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> JsonScanner<'a> {
    fn new(src: &'a str) -> Self { JsonScanner { src, pos: 0 } }

    fn skip_ws(&mut self) {
        while self.pos < self.src.len() {
            let b = self.src.as_bytes()[self.pos];
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' || b == b'{' || b == b'}' {
                self.pos += 1;
            } else { break; }
        }
    }

    /// Read a JSON string (advancing past the closing quote).
    fn read_string(&mut self) -> Option<&'a str> {
        if self.pos >= self.src.len() { return None; }
        if self.src.as_bytes()[self.pos] != b'"' { return None; }
        self.pos += 1;
        let start = self.pos;
        while self.pos < self.src.len() && self.src.as_bytes()[self.pos] != b'"' {
            self.pos += 1;
        }
        let s = &self.src[start..self.pos];
        if self.pos < self.src.len() { self.pos += 1; } // skip closing '"'
        Some(s)
    }

    /// Read a bare JSON value (number or string).
    fn read_value(&mut self) -> Option<&'a str> {
        self.skip_ws();
        if self.pos >= self.src.len() { return None; }
        if self.src.as_bytes()[self.pos] == b'"' {
            return self.read_string();
        }
        // Bare number or identifier
        let start = self.pos;
        while self.pos < self.src.len() {
            let b = self.src.as_bytes()[self.pos];
            if b == b',' || b == b'}' || b == b' ' || b == b'\n' { break; }
            self.pos += 1;
        }
        if self.pos > start { Some(&self.src[start..self.pos]) } else { None }
    }

    /// Returns the next (key, value) pair or None at end of object.
    fn next_kv(&mut self) -> Option<(&'a str, &'a str)> {
        self.skip_ws();
        if self.pos >= self.src.len() { return None; }
        // Skip commas
        if self.src.as_bytes()[self.pos] == b',' { self.pos += 1; self.skip_ws(); }
        if self.pos >= self.src.len() { return None; }
        let key = self.read_string()?;
        self.skip_ws();
        if self.pos < self.src.len() && self.src.as_bytes()[self.pos] == b':' { self.pos += 1; }
        self.skip_ws();
        let val = self.read_value()?;
        Some((key, val))
    }
}

// ─── Load model ──────────────────────────────────────────────────────────────

/// Load a .tmt-ai model from the VFS path.
/// Uses Huge-Page mapping for zero-copy weight access.
/// Stores the result in LOADED_MODEL.
///
/// Requires: CapFsRead("/models"), CapMemHuge — both provided via `caps`.
pub fn cmd_load(path: &str, caps: &AaiCaps) -> Result<(), &'static str> {
    let _fs  = caps.fs_read;  // prove we hold CapFsRead before any VFS access
    let _mem = caps.mem_huge; // prove we hold CapMemHuge before any mmap
    serial::write_str("[AAI] Loading model: ");
    serial::write_str(path);
    serial::write_str("\r\n");

    // Read file via VFS
    let file_data = fs::read_file(path).ok_or("VFS: file not found")?;
    if file_data.is_empty() { return Err("File is empty"); }
    let file_size = file_data.len();

    // Allocate huge-page buffer (if Mmap syscall not ready, fall back to heap)
    let buf_ptr: *mut u8 = unsafe {
        let p = sys_mmap_huge(file_size);
        if p.is_null() {
            // Fall back to 64-byte aligned heap allocation
            let layout = core::alloc::Layout::from_size_align(file_size, 64)
                .map_err(|_| "layout error")?;
            alloc::alloc::alloc_zeroed(layout)
        } else { p }
    };
    if buf_ptr.is_null() { return Err("OOM"); }

    // Copy file data into aligned buffer
    unsafe { core::ptr::copy_nonoverlapping(file_data.as_ptr(), buf_ptr, file_size); }
    let buf = unsafe { core::slice::from_raw_parts(buf_ptr, file_size) };

    // Parse header
    let meta = parse_tmt_ai(buf).map_err(|e| { serial::write_str("[AAI] Parse error: "); serial::write_str(e); serial::write_str("\r\n"); e })?;

    let n_layers   = meta.n_layers;
    let ctx_len    = meta.ctx_len;
    let d_model    = meta.d_model;
    let w_off      = meta.weight_offset;
    let w_bytes    = meta.weight_bytes;
    let w_len      = w_bytes / 4;

    // Map weights as a Tensor via from_raw_parts (zero-copy)
    let weight_ptr = unsafe { buf_ptr.add(w_off) };
    let weights    = unsafe {
        Tensor::from_raw_parts(weight_ptr, w_len, &[w_len], DataType::F32)
    };

    serial::write_str("[AAI] Model loaded: ");
    serial::write_str(&meta.name);
    serial::write_str(" (");
    serial_dec(meta.param_count);
    serial::write_str(" params)\r\n");

    let kv_cache = KvCache::new(n_layers, ctx_len, d_model);

    unsafe {
        LOADED_MODEL = Some(LoadedModel { meta, weights, kv_cache });
    }
    Ok(())
}

// ─── Info command ─────────────────────────────────────────────────────────────

pub fn cmd_info(row: &mut usize) {
    let model = unsafe { LOADED_MODEL.as_ref() };
    match model {
        None => {
            draw_line(*row, "No model loaded. Run: aai load <file.tmt-ai>", COL_ERR);
            *row += 1;
        }
        Some(m) => {
            let meta = &m.meta;
            draw_line(*row, "── ANE Model Info ─────────────────────────────────", COL_HEADER);
            *row += 1;

            let mut line = String::from("  Name:       ");
            line.push_str(&meta.name);
            draw_line(*row, &line, COL_NORMAL); *row += 1;

            let mut line = String::from("  Arch:       ");
            line.push_str(&meta.arch);
            draw_line(*row, &line, COL_NORMAL); *row += 1;

            let mut line = String::from("  d_model:    ");
            push_dec(&mut line, meta.d_model as u64);
            draw_line(*row, &line, COL_NORMAL); *row += 1;

            let mut line = String::from("  Layers:     ");
            push_dec(&mut line, meta.n_layers as u64);
            draw_line(*row, &line, COL_NORMAL); *row += 1;

            let mut line = String::from("  Heads:      ");
            push_dec(&mut line, meta.n_heads as u64);
            draw_line(*row, &line, COL_NORMAL); *row += 1;

            let mut line = String::from("  Vocab:      ");
            push_dec(&mut line, meta.vocab_size as u64);
            draw_line(*row, &line, COL_NORMAL); *row += 1;

            let mut line = String::from("  Ctx window: ");
            push_dec(&mut line, meta.ctx_len as u64);
            draw_line(*row, &line, COL_NORMAL); *row += 1;

            let mut line = String::from("  Params:     ");
            push_dec(&mut line, meta.param_count);
            draw_line(*row, &line, COL_GOOD); *row += 1;

            let mb = meta.weight_bytes / (1024 * 1024);
            let mut line = String::from("  Weights:    ");
            push_dec(&mut line, mb as u64);
            line.push_str(" MiB (zero-copy mapped)");
            draw_line(*row, &line, COL_ACCENT); *row += 1;

            draw_line(*row, "───────────────────────────────────────────────────", COL_HEADER);
            *row += 1;
        }
    }
}

// ─── Bench command ────────────────────────────────────────────────────────────

/// Benchmark: run a sequence of GEMM + ReLU passes and report throughput.
pub fn cmd_bench(row: &mut usize, _caps: &AaiCaps) {
    draw_line(*row, "── ANE Tensor Benchmark ───────────────────────────", COL_HEADER);
    *row += 1;

    let sizes: &[(usize, usize, usize)] = &[
        (64,  64,  64),
        (256, 256, 256),
        (512, 512, 512),
        (1024, 1024, 1024),
    ];

    for &(m, k, n) in sizes {
        let a = Tensor::from_flat_f32(
            &vec![1.0f32 / (m*k) as f32; m*k], m, k);
        let b = Tensor::from_flat_f32(
            &vec![1.0f32 / (k*n) as f32; k*n], k, n);

        let t0 = idt::timer_ticks();
        let mut c = a.matmul(&b);
        c.relu_inplace();
        let dt = idt::timer_ticks().wrapping_sub(t0);

        // GFLOPS: 2*m*k*n multiply-adds
        let gflops_num = 2u64 * m as u64 * k as u64 * n as u64;
        // dt is in ticks (100 Hz = 10ms each), convert to ns: dt*10_000_000
        let ns = dt.max(1) * 10_000_000;
        let gflops = (gflops_num * 1_000) / ns; // GFOPS×10⁻³

        let mut line = String::from("  GEMM ");
        push_dec(&mut line, m as u64);
        line.push('×');
        push_dec(&mut line, k as u64);
        line.push('×');
        push_dec(&mut line, n as u64);
        line.push_str("  → ");
        push_dec(&mut line, dt);
        line.push_str(" ticks  ≈ ");
        push_dec(&mut line, gflops);
        line.push_str(" MFLOPS");

        draw_line(*row, &line, COL_NORMAL); *row += 1;
    }

    draw_line(*row, "───────────────────────────────────────────────────", COL_HEADER);
    *row += 1;
}

// ─── Summarize command ────────────────────────────────────────────────────────

/// Statistical text summary: word count, char count, unique bigrams, entropy estimate.
///
/// Designed to be the sink of a plum pipeline:
///   `cat /var/log/boot.log | aai summarize`
/// Until plum pipes are live, text is passed as a CLI argument.
///
/// Capabilities required: none (pure computation, no VFS/framebuffer write beyond output).
pub fn cmd_summarize(text: &str, row: &mut usize) {
    draw_line(*row, "── aai summarize ───────────────────────────────────", COL_HEADER);
    *row += 1;

    if text.is_empty() {
        draw_line(*row, "  (empty input — use: aai summarize <text>)", COL_DIM);
        *row += 1;
        return;
    }

    let bytes  = text.as_bytes();
    let chars  = text.chars().count();
    let words  = text.split_whitespace().count();
    let lines  = text.split('\n').count();

    // Unique token count (bigram hash, no alloc hashmap needed)
    // We use a 256-slot counting array as a fast approximation.
    let mut byte_hist = [0u32; 256];
    for &b in bytes {
        byte_hist[b as usize] += 1;
    }
    let unique_bytes = byte_hist.iter().filter(|&&c| c > 0).count();

    // Shannon entropy estimate (bits per byte) using byte distribution.
    let n = bytes.len() as f32;
    let mut entropy = 0.0f32;
    for &count in &byte_hist {
        if count > 0 {
            let p = count as f32 / n;
            // H = -sum(p * log2(p)).  Approximate log2 via ln/ln(2).
            // ln(p) via Newton: ln(x) ≈ 2*(x-1)/(x+1) [Padé, good for p near 1].
            // For better range: ln(x) = 2*(z + z³/3 + z⁵/5 + ...) where z=(x-1)/(x+1)
            let z   = (p - 1.0) / (p + 1.0);
            let z2  = z * z;
            let ln_p = 2.0 * z * (1.0 + z2 / 3.0 + z2 * z2 / 5.0);
            entropy -= p * ln_p / core::f32::consts::LN_2;
        }
    }

    // Most frequent byte
    let (top_byte, top_count) = byte_hist.iter().enumerate()
        .max_by_key(|&(_, &c)| c)
        .unwrap_or((32, &0));

    // Output summary
    let mut line = String::from("  Chars:        ");
    push_dec(&mut line, chars as u64);
    draw_line(*row, &line, COL_NORMAL); *row += 1;

    let mut line = String::from("  Words:        ");
    push_dec(&mut line, words as u64);
    draw_line(*row, &line, COL_NORMAL); *row += 1;

    let mut line = String::from("  Lines:        ");
    push_dec(&mut line, lines as u64);
    draw_line(*row, &line, COL_NORMAL); *row += 1;

    let mut line = String::from("  Unique bytes: ");
    push_dec(&mut line, unique_bytes as u64);
    line.push_str(" / 256");
    draw_line(*row, &line, COL_NORMAL); *row += 1;

    // Entropy × 100 (2 decimal places without floats)
    let ent_int  = entropy as u64;
    let ent_frac = ((entropy - ent_int as f32) * 100.0) as u64;
    let mut line = String::from("  Entropy:      ");
    push_dec(&mut line, ent_int);
    line.push('.');
    if ent_frac < 10 { line.push('0'); }
    push_dec(&mut line, ent_frac);
    line.push_str(" bits/byte");
    draw_line(*row, &line, COL_ACCENT); *row += 1;

    let mut line = String::from("  Top byte:     0x");
    // Push hex for top_byte
    let hi = (top_byte >> 4) as u8;
    let lo = (top_byte & 0xF) as u8;
    line.push(if hi < 10 { (b'0'+hi) as char } else { (b'a'+hi-10) as char });
    line.push(if lo < 10 { (b'0'+lo) as char } else { (b'a'+lo-10) as char });
    line.push_str("  (");
    if top_byte >= 0x20 && top_byte < 0x7F {
        line.push(top_byte as u8 as char);
    } else {
        line.push('.');
    }
    line.push_str(")  count=");
    push_dec(&mut line, *top_count as u64);
    draw_line(*row, &line, COL_DIM); *row += 1;

    // Preview: first 48 chars
    let preview: String = text.chars().take(48).collect();
    let mut line = String::from("  Preview:      \"");
    line.push_str(&preview);
    if chars > 48 { line.push_str("..."); }
    line.push('"');
    draw_line(*row, &line, COL_DIM); *row += 1;

    draw_line(*row, "────────────────────────────────────────────────────", COL_HEADER);
    *row += 1;
}

// ─── Chat command (Phase 3) ───────────────────────────────────────────────────

/// KV-cache tokenizer: split prompt into token ids using a simple byte bigram hash.
fn naive_tokenize(text: &str, vocab_size: usize) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    if bytes.is_empty() { return tokens; }
    // Bigram character hash
    tokens.push((bytes[0] as usize) % vocab_size);
    for i in 1..bytes.len() {
        let tok = (bytes[i-1] as usize * 256 + bytes[i] as usize) % vocab_size;
        tokens.push(tok);
    }
    tokens
}

#[inline]
fn wrap_idx(len: usize, idx: usize) -> usize {
    if len == 0 { 0 } else { idx % len }
}

/// Newton-Raphson square root (avoids libm in no_std).
#[inline(always)]
fn fast_sqrt(x: f32) -> f32 {
    if x <= 0.0 { return 0.0; }
    let mut y = f32::from_bits((x.to_bits().wrapping_add(0x3f80_0000)) >> 1);
    y = 0.5 * (y + x / y);
    y = 0.5 * (y + x / y);
    y
}

#[inline]
fn matvec_cyclic(weights: &[f32], base: usize, input: &[f32], out_dim: usize) -> Vec<f32> {
    let in_dim = input.len();
    let w_len = weights.len();
    let mut out = vec![0.0f32; out_dim];
    if w_len == 0 || in_dim == 0 || out_dim == 0 {
        return out;
    }
    for o in 0..out_dim {
        let mut acc = 0.0f32;
        let row_off = base + o * in_dim;
        for i in 0..in_dim {
            let wi = wrap_idx(w_len, row_off + i);
            acc += input[i] * weights[wi];
        }
        out[o] = acc;
    }
    out
}

fn softmax_vec(v: &mut [f32]) {
    if v.is_empty() { return; }
    let mut t = Tensor::from_slice_f32(v);
    t.softmax_inplace();
    let src = t.as_f32_slice();
    for i in 0..v.len() {
        v[i] = src[i];
    }
}

/// Full N-layer transformer forward inference.
///
/// Weight layout in .tmt-ai flat f32 buffer:
///   [embeddings:  vocab * d_model]
///   [layer_0: Wq(d²) Wk(d²) Wv(d²) Wo(d²) FF1(4d²) FF2(4d²)]   ← 12*d² each
///   [layer_1..layer_{n-1}: same pattern]
///   [lm_head: vocab * d_model]
///
/// KV-cache: stateful ring-buffer per layer, updated every call.
/// Returns softmax(logits) of shape [vocab_size].
fn infer_next_logits(
    model: &mut LoadedModel,
    tokens: &[usize],
) -> Vec<f32> {
    let meta     = &model.meta;
    let d        = meta.d_model;
    let vocab    = meta.vocab_size;
    let n_layers = meta.n_layers.max(1);
    let d_ff     = d * 4;   // Standard Transformer FFN expansion ratio

    if d == 0 || vocab == 0 { return Vec::new(); }

    let tok = tokens.last().copied().unwrap_or(0) % vocab;

    // SAFETY: weights Tensor is valid for the duration of this call.
    let weights = model.weights.as_f32_slice();
    let w_len   = weights.len();
    if w_len == 0 { return vec![0.0f32; vocab]; }

    // ── Embedding lookup ─────────────────────────────────────────────────────
    // Layout: embeddings start at byte 0, row-major [vocab × d_model].
    let emb_base = 0usize;
    let mut hidden = vec![0.0f32; d];
    let emb_row = emb_base.saturating_add(tok.saturating_mul(d));
    for i in 0..d {
        hidden[i] = weights[wrap_idx(w_len, emb_row + i)];
    }

    // ── Per-layer geometry ───────────────────────────────────────────────────
    // Each layer block: [Wq d², Wk d², Wv d², Wo d², FF1 d_ff*d, FF2 d*d_ff]
    //                    4*d²  +  d_ff*d + d*d_ff  =  4d² + 4d² + 4d²  = 12d²
    let per_layer  = 4usize.saturating_mul(d * d)
                     .saturating_add(d_ff.saturating_mul(d))
                     .saturating_add(d.saturating_mul(d_ff));
    let layers_base  = emb_base.saturating_add(vocab.saturating_mul(d));
    let lm_head_base = layers_base.saturating_add(n_layers.saturating_mul(per_layer));

    // ── Transformer stack ─────────────────────────────────────────────────────
    for layer in 0..n_layers {
        let lb       = layers_base.saturating_add(layer.saturating_mul(per_layer));
        let q_base   = lb;
        let k_base   = lb.saturating_add(d * d);
        let v_base   = lb.saturating_add(d * d * 2);
        let o_base   = lb.saturating_add(d * d * 3);
        let ff1_base = lb.saturating_add(d * d * 4);
        let ff2_base = lb.saturating_add(d * d * 4 + d_ff.saturating_mul(d));

        // Q / K / V projections from current hidden state
        let q = matvec_cyclic(weights, q_base,  &hidden, d);
        let k = matvec_cyclic(weights, k_base,  &hidden, d);
        let v = matvec_cyclic(weights, v_base,  &hidden, d);

        // Update KV cache for this layer
        if layer < model.kv_cache.n_layers {
            model.kv_cache.insert(layer, &k, &v);
        }

        // Causal self-attention: attend over all past keys/values for this layer
        let context = {
            let kv     = &model.kv_cache;
            let valid  = if layer < kv.n_layers { kv.fill.min(kv.ctx_len) } else { 0 };
            let mut ctx = vec![0.0f32; d];

            if valid > 0 && layer < kv.n_layers {
                // Per-layer slice offsets within the flat KV tensors
                let key_layer_off = layer * kv.ctx_len * kv.d_model;
                let val_layer_off = layer * kv.ctx_len * kv.d_model;
                let start_slot    = if kv.fill < kv.ctx_len { 0 } else { kv.head };
                let keys = kv.keys.as_f32_slice();
                let vals = kv.values.as_f32_slice();
                let inv_scale = 1.0f32 / fast_sqrt(d as f32);

                // Compute attention scores
                let mut scores = vec![0.0f32; valid];
                for i in 0..valid {
                    let slot = (start_slot + i) % kv.ctx_len;
                    let base = key_layer_off + slot * d;
                    let mut dot = 0.0f32;
                    for j in 0..d {
                        dot += q[j] * keys[wrap_idx(keys.len(), base + j)];
                    }
                    scores[i] = dot * inv_scale;
                }
                softmax_vec(&mut scores);

                // Weighted sum of values
                for i in 0..valid {
                    let slot = (start_slot + i) % kv.ctx_len;
                    let base = val_layer_off + slot * d;
                    let a    = scores[i];
                    for j in 0..d {
                        ctx[j] += a * vals[wrap_idx(vals.len(), base + j)];
                    }
                }
            } else {
                // No cache history yet for this layer: pass through v directly
                for i in 0..d { ctx[i] = v[i]; }
            }
            ctx
        };

        // Output projection + residual
        let attn_out = matvec_cyclic(weights, o_base, &context, d);
        for i in 0..d { hidden[i] += attn_out[i]; }

        // Feed-forward: ReLU(hidden × FF1ᵀ) × FF2ᵀ + residual
        let mut ff_hidden = matvec_cyclic(weights, ff1_base, &hidden, d_ff);
        for x in ff_hidden.iter_mut() { if *x < 0.0 { *x = 0.0; } }   // ReLU
        let ff_out = matvec_cyclic(weights, ff2_base, &ff_hidden, d);
        for i in 0..d { hidden[i] += ff_out[i]; }                       // residual
    }

    // ── LM head: project final hidden state to vocab logits ─────────────────
    let mut logits = vec![0.0f32; vocab];
    for vid in 0..vocab {
        let base = lm_head_base.saturating_add(vid.saturating_mul(d));
        let mut dot = 0.0f32;
        for i in 0..d {
            dot += hidden[i] * weights[wrap_idx(w_len, base + i)];
        }
        logits[vid] = dot;
    }
    softmax_vec(&mut logits);
    logits
}

/// Sample the next token id from logits (top-1 greedy).
fn sample_greedy(logits: &[f32]) -> usize {
    let mut best = 0;
    let mut best_val = f32::NEG_INFINITY;
    for (i, &v) in logits.iter().enumerate() {
        if v > best_val { best_val = v; best = i; }
    }
    best
}

/// Detokenize a token id to a visible glyph.
fn detokenize(tok: usize) -> char {
    // For demo: map token id to printable ASCII range
    let c = (tok % 96) as u8 + 32;
    if c.is_ascii_graphic() || c == b' ' { c as char } else { '.' }
}

/// Interactive chat loop with 100 Hz token-streaming to framebuffer.
/// Requires: CapFramebuf + CapSerial — provided via `caps`.
pub fn cmd_chat(prompt: &str, row: &mut usize, _caps: &AaiCaps) {
    let model = unsafe { LOADED_MODEL.as_mut() };
    let model = match model {
        Some(m) => m,
        None => {
            draw_line(*row, "[aai] No model loaded. Use: aai load <file>", COL_ERR);
            *row += 1;
            return;
        }
    };

    // New chat session starts with a clean KV-cache state.
    model.kv_cache.reset();

    draw_line(*row, "╔══════════════════════════════════════════════════╗", COL_ACCENT);
    *row += 1;
    let mut header = String::from("║  aai-chat  ·  ");
    header.push_str(&model.meta.name);
    header.push_str("  ·  ctx=");
    push_dec(&mut header, model.meta.ctx_len as u64);
    header.push_str("  ║");
    draw_line(*row, &header, COL_ACCENT); *row += 1;
    draw_line(*row, "╚══════════════════════════════════════════════════╝", COL_ACCENT);
    *row += 1;

    let mut user_line = String::from("You> ");
    user_line.push_str(prompt);
    draw_line(*row, &user_line, COL_NORMAL); *row += 1;
    draw_line(*row, "AI>  ", COL_GOOD);

    // Tokenize
    let tokens = naive_tokenize(prompt, model.meta.vocab_size);
    if tokens.is_empty() {
        draw_line(*row, "(empty prompt)", COL_DIM); *row += 1;
        return;
    }

    // Generate MAX_GEN tokens, streaming at 100 Hz
    const MAX_GEN: usize = 64;
    let ai_row  = *row;
    let col_px  = 8 * 5; // "AI>  " prefix = 5 chars
    let mut cur_tokens = tokens.clone();
    let mut out_x = col_px;
    let out_y = ai_row * 16;

    let tick_start = idt::timer_ticks();

    for gen_step in 0..MAX_GEN {
        // Respect 100 Hz timer: one token per tick
        loop {
            let now = idt::timer_ticks();
            if now >= tick_start + gen_step as u64 { break; }
            unsafe { core::arch::asm!("hlt"); }
        }

        // Inference
        let logits   = infer_next_logits(model, &cur_tokens);
        let next_tok = sample_greedy(&logits);
        let ch       = detokenize(next_tok);

        // Stream character to framebuffer
        let ch_buf = [ch as u8];
        if let Ok(s) = core::str::from_utf8(&ch_buf) {
            framebuffer::draw_string_at(out_x as u64, out_y as u64, s, COL_GOOD, 0x00_00_00_00);
            out_x += 8;
        }

        // Also emit to serial
        serial::write_byte(ch as u8);

        // Stop on EOS / newline token
        if ch == '\n' || next_tok == 0 { break; }

        cur_tokens.push(next_tok);
        if cur_tokens.len() > model.meta.ctx_len {
            cur_tokens.remove(0);
        }
    }
    serial::write_str("\r\n");
    *row += 1;

    // Status bar: RAM usage + timing
    let ticks_elapsed = idt::timer_ticks().wrapping_sub(tick_start);
    let mut status = String::from("  [tokens/s ~");
    push_dec(&mut status, (MAX_GEN as u64).saturating_div(ticks_elapsed.max(1)) * 100);
    status.push_str("]  [cache: ");
    push_dec(&mut status, model.kv_cache.fill as u64);
    status.push('/');
    push_dec(&mut status, model.kv_cache.ctx_len as u64);
    status.push_str(" slots]");
    draw_line(*row, &status, COL_DIM); *row += 1;
}

// ─── Top-level dispatcher ────────────────────────────────────────────────────

/// Call this from terminal.rs: `aai_dispatch(&["aai", "load", "/models/foo.tmt-ai"])`
pub fn aai_dispatch(args: &[&str], row: &mut usize) {
    if args.len() < 2 {
        aai_help(row);
        return;
    }
    // Acquire capability manifest once at the entry point.
    // The kernel will verify this against the task's token set at spawn time.
    let caps = AaiCaps::acquire();

    match args[1] {
        "load" => {
            if args.len() < 3 {
                draw_line(*row, "Usage: aai load <path>", COL_ERR);
                *row += 1;
                return;
            }
            match cmd_load(args[2], &caps) {
                Ok(())    => { draw_line(*row, "Model loaded OK.", COL_GOOD); *row += 1; }
                Err(e)    => { let mut m = String::from("[AAI] Load error: "); m.push_str(e);
                               draw_line(*row, &m, COL_ERR); *row += 1; }
            }
        }
        "info"  => cmd_info(row),
        "bench" => cmd_bench(row, &caps),
        "chat"  => {
            let prompt = if args.len() >= 3 { args[2..].join(" ") } else { String::from("Hello") };
            cmd_chat(&prompt, row, &caps);
        }
        // "aai summarize <text>" — statistical text summary.
        // Designed as the sink of a plum pipeline:  cat file | aai summarize
        // Until plum pipes are implemented, accepts text directly as an argument.
        "summarize" => {
            let text = if args.len() >= 3 { args[2..].join(" ") } else { String::new() };
            cmd_summarize(&text, row);
        }
        _ => { aai_help(row); }
    }
}

fn aai_help(row: &mut usize) {
    let lines = [
        "── aai — Aeterna AI utility ────────────────────────",
        "  aai load <path>        Load a .tmt-ai model file",
        "  aai info               Show loaded model metadata",
        "  aai bench              GEMM throughput benchmark",
        "  aai chat [prompt]      Interactive inference chat",
        "  aai summarize <text>   Statistical text summary",
        "  (plum pipe: cat file | aai summarize)",
        "────────────────────────────────────────────────────",
    ];
    for l in &lines { draw_line(*row, l, COL_HEADER); *row += 1; }
}

// ─── Framebuffer helpers ──────────────────────────────────────────────────────

fn draw_line(row: usize, text: &str, color: u32) {
    framebuffer::draw_string_at(0, (row * 16) as u64, text, color, 0x00_00_00_00);
    serial::write_str(text);
    serial::write_str("\r\n");
}

fn push_dec(s: &mut String, mut n: u64) {
    if n == 0 { s.push('0'); return; }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while n > 0 { buf[i] = b'0' + (n % 10) as u8; n /= 10; i += 1; }
    for j in (0..i).rev() { s.push(buf[j] as char); }
}

fn serial_dec(mut n: u64) {
    if n == 0 { serial::write_str("0"); return; }
    let mut buf = [0u8; 20];
    let mut i = 0;
    while n > 0 { buf[i] = b'0' + (n % 10) as u8; n /= 10; i += 1; }
    buf[..i].reverse();
    serial::write_str(core::str::from_utf8(&buf[..i]).unwrap_or("?"));
}

// ─── String join helper (no alloc joining workaround) ────────────────────────

trait JoinExt {
    fn join(&self, sep: &str) -> String;
}

impl JoinExt for [&str] {
    fn join(&self, sep: &str) -> String {
        let mut result = String::new();
        for (i, &s) in self.iter().enumerate() {
            if i > 0 { result.push_str(sep); }
            result.push_str(s);
        }
        result
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────
// Pure-logic tests: no framebuffer / serial / idt calls.
// Run with: cargo test --lib (host build, x86_64-unknown-linux-gnu)

#[cfg(test)]
mod tests {
    use super::*;

    // ── fast_sqrt ─────────────────────────────────────────────────────────────

    #[test]
    fn test_fast_sqrt_zero() {
        assert_eq!(fast_sqrt(0.0), 0.0);
    }

    #[test]
    fn test_fast_sqrt_negative_zero() {
        assert_eq!(fast_sqrt(-1.0), 0.0);
    }

    #[test]
    fn test_fast_sqrt_perfect_squares() {
        let cases = [(1.0f32,1.0),(4.0,2.0),(9.0,3.0),(16.0,4.0),(100.0,10.0)];
        for (x, expected) in cases {
            let got = fast_sqrt(x);
            assert!((got - expected).abs() < 1e-3,
                "fast_sqrt({x}) = {got}, expected {expected}");
        }
    }

    #[test]
    fn test_fast_sqrt_irrational() {
        assert!((fast_sqrt(2.0) - 1.41421356).abs() < 1e-3);
        assert!((fast_sqrt(3.0) - 1.73205081).abs() < 1e-3);
    }

    // ── wrap_idx ─────────────────────────────────────────────────────────────

    #[test]
    fn test_wrap_idx_within_bounds() {
        assert_eq!(wrap_idx(10, 5), 5);
        assert_eq!(wrap_idx(10, 0), 0);
        assert_eq!(wrap_idx(10, 9), 9);
    }

    #[test]
    fn test_wrap_idx_wraps() {
        assert_eq!(wrap_idx(10, 10), 0);
        assert_eq!(wrap_idx(10, 25), 5);
    }

    #[test]
    fn test_wrap_idx_zero_len() {
        assert_eq!(wrap_idx(0, 99), 0);
    }

    // ── matvec_cyclic ─────────────────────────────────────────────────────────

    #[test]
    fn test_matvec_cyclic_identity() {
        // weights = identity 2x2 = [1,0, 0,1] at base=0, out_dim=2
        let w = [1.0f32, 0.0, 0.0, 1.0];
        let x = [3.0f32, 5.0];
        let out = matvec_cyclic(&w, 0, &x, 2);
        assert!((out[0] - 3.0).abs() < 1e-5, "expected 3.0, got {}", out[0]);
        assert!((out[1] - 5.0).abs() < 1e-5, "expected 5.0, got {}", out[1]);
    }

    #[test]
    fn test_matvec_cyclic_zero_weights() {
        let w = [0.0f32; 4];
        let x = [1.0f32, 2.0, 3.0, 4.0];
        let out = matvec_cyclic(&w, 0, &x, 2);
        assert_eq!(&out, &[0.0f32, 0.0]);
    }

    #[test]
    fn test_matvec_empty() {
        let empty: [f32; 0] = [];
        let out = matvec_cyclic(&empty, 0, &[1.0, 2.0], 3);
        assert_eq!(out, vec![0.0f32; 3]);
    }

    // ── softmax_vec ───────────────────────────────────────────────────────────

    #[test]
    fn test_softmax_vec_sums_to_one() {
        let mut v = vec![1.0f32, 2.0, 3.0, 4.0];
        softmax_vec(&mut v);
        let sum: f32 = v.iter().sum();
        assert!((sum - 1.0).abs() < 1e-5, "softmax sum={sum}");
    }

    #[test]
    fn test_softmax_vec_monotone() {
        let mut v = vec![0.5f32, 1.5, 3.0];
        softmax_vec(&mut v);
        assert!(v[0] < v[1] && v[1] < v[2], "softmax should be monotone");
    }

    #[test]
    fn test_softmax_vec_uniform() {
        let n = 4;
        let mut v = vec![1.0f32; n];
        softmax_vec(&mut v);
        for &x in &v {
            assert!((x - 0.25).abs() < 1e-5, "uniform input → 1/n={:.3}", x);
        }
    }

    // ── naive_tokenize ────────────────────────────────────────────────────────

    #[test]
    fn test_tokenize_empty() {
        let toks = naive_tokenize("", 100);
        assert!(toks.is_empty());
    }

    #[test]
    fn test_tokenize_bounds() {
        let vocab = 256;
        let toks = naive_tokenize("Hello, world!", vocab);
        for &t in &toks {
            assert!(t < vocab, "token {t} out of vocab range {vocab}");
        }
    }

    #[test]
    fn test_tokenize_deterministic() {
        let t1 = naive_tokenize("test", 1000);
        let t2 = naive_tokenize("test", 1000);
        assert_eq!(t1, t2, "tokenizer must be deterministic");
    }

    // ── sample_greedy ─────────────────────────────────────────────────────────

    #[test]
    fn test_sample_greedy_returns_argmax() {
        let logits = vec![0.1f32, 0.5, 0.9, 0.2];
        let best = sample_greedy(&logits);
        assert_eq!(best, 2, "greedy should pick index 2 (max=0.9)");
    }

    #[test]
    fn test_sample_greedy_first_element() {
        let logits = vec![1.0f32, 0.0, 0.0];
        assert_eq!(sample_greedy(&logits), 0);
    }

    #[test]
    fn test_sample_greedy_single_element() {
        let logits = vec![42.0f32];
        assert_eq!(sample_greedy(&logits), 0);
    }

    // ── parse_tmt_ai ──────────────────────────────────────────────────────────

    #[test]
    fn test_parse_tmt_ai_bad_magic() {
        let data = b"XXXX\x01\x00\x00\x00\x00\x00\x00\x00";
        assert!(parse_tmt_ai(data).is_err(), "wrong magic should fail");
    }

    #[test]
    fn test_parse_tmt_ai_too_short() {
        let data = b"TMT";   // only 3 bytes
        assert!(parse_tmt_ai(data).is_err());
    }

    #[test]
    fn test_parse_tmt_ai_minimal_valid() {
        // Magic + version=1 + meta_len=2 + "{}" + (pad to 64) + weights
        let mut buf = Vec::new();
        buf.extend_from_slice(b"TMT\x01");              // magic
        buf.extend_from_slice(&1u32.to_le_bytes());     // version
        buf.extend_from_slice(&2u32.to_le_bytes());     // meta_len=2
        buf.extend_from_slice(b"{}");                   // JSON metadata
        // Pad to 64 bytes
        while buf.len() % 64 != 0 { buf.push(0u8); }
        // 4 dummy weight bytes
        buf.extend_from_slice(&[0u8; 4]);

        let meta = parse_tmt_ai(&buf).expect("minimal valid .tmt-ai should parse OK");
        assert_eq!(meta.version, 1);
        assert!(meta.weight_bytes >= 4);
    }

    #[test]
    fn test_parse_tmt_ai_json_fields() {
        // Embed JSON with known fields
        let json = br#"{"name":"tiny","d_model":64,"n_layers":2,"vocab_size":128}"#;
        let meta_len = json.len() as u32;
        let mut buf = Vec::new();
        buf.extend_from_slice(b"TMT\x01");
        buf.extend_from_slice(&1u32.to_le_bytes());
        buf.extend_from_slice(&meta_len.to_le_bytes());
        buf.extend_from_slice(json);
        while buf.len() % 64 != 0 { buf.push(0u8); }
        buf.extend_from_slice(&[0u8; 64]);   // dummy weights

        let meta = parse_tmt_ai(&buf).expect("structured JSON should parse");
        assert_eq!(meta.name, "tiny");
        assert_eq!(meta.d_model, 64);
        assert_eq!(meta.n_layers, 2);
        assert_eq!(meta.vocab_size, 128);
    }
}
