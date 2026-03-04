/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

ANE Tensor — N-dimensional tensor with 64-byte aligned storage.

Memory layout:
  - Raw bytes at 64-byte boundary (AVX-512 native alignment)
  - Row-major strides (C-order)
  - Owned vs. borrowed (huge-page mapping via sys_mmap_huge)

SIMD dispatch:
  - AVX-512: _mm512_fmadd_ps, 16×f32 per cycle
  - AVX2 fallback: _mm256_fmadd_ps, 8×f32 per cycle
  - Scalar fallback for non-x86_64 or old hardware

Autograd:
  - Reverse-mode tape (dynamic define-by-run)
  - GradFn per Variable stores backward closure
  - .backward() accumulates gradients leaf-to-root
*/

#![allow(unsafe_op_in_unsafe_fn)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::alloc::Layout;

// ─── DataType ────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DataType {
    /// 32-bit float — primary training dtype
    F32,
    /// 16-bit float (IEEE 754 half) — inference / reduced memory
    F16,
    /// 8-bit signed integer — quantised inference
    I8,
}

impl DataType {
    /// Bytes per element.
    #[inline]
    pub fn bytes(&self) -> usize {
        match self {
            DataType::F32 => 4,
            DataType::F16 => 2,
            DataType::I8  => 1,
        }
    }
}

// ─── Tensor ──────────────────────────────────────────────────────────────────

/// N-dimensional dense tensor.
pub struct Tensor {
    /// Raw byte buffer, 64-byte aligned.
    data:   *mut u8,
    shape:  Vec<usize>,
    strides: Vec<usize>,
    /// Number of logical elements.
    pub len: usize,
    pub dtype: DataType,
    /// If false the buffer is borrowed (e.g. huge-page mapping).
    owned: bool,
}

// SAFETY: Tensor is single-threaded in AETERNA (single-core kernel).
unsafe impl Send for Tensor {}
unsafe impl Sync for Tensor {}

impl Tensor {
    // ── Construction ─────────────────────────────────────────────────────────

    /// Allocate a zero-initialised tensor with the given shape and dtype.
    pub fn zeros(shape: &[usize], dtype: DataType) -> Self {
        let len: usize = shape.iter().product();
        let byte_size  = len * dtype.bytes();
        let layout = Layout::from_size_align(byte_size.max(1), 64)
            .expect("ANE: tensor layout");
        let data = unsafe {
            let p = alloc::alloc::alloc_zeroed(layout);
            assert!(!p.is_null(), "ANE: tensor OOM");
            p
        };
        let strides = Self::compute_strides(shape);
        Tensor { data, shape: shape.to_vec(), strides, len, dtype, owned: true }
    }

    /// Allocate a tensor filled with the given f32 value.
    pub fn full_f32(shape: &[usize], val: f32) -> Self {
        let mut t = Tensor::zeros(shape, DataType::F32);
        for i in 0..t.len {
            t.set_f32(i, val);
        }
        t
    }

    /// Create a 1-D tensor from a slice (copies data).
    pub fn from_slice_f32(data: &[f32]) -> Self {
        let mut t = Tensor::zeros(&[data.len()], DataType::F32);
        for (i, &v) in data.iter().enumerate() {
            t.set_f32(i, v);
        }
        t
    }

    /// Create a 2-D tensor from row-major flat slice.
    pub fn from_flat_f32(data: &[f32], rows: usize, cols: usize) -> Self {
        assert_eq!(data.len(), rows * cols);
        let mut t = Tensor::zeros(&[rows, cols], DataType::F32);
        for (i, &v) in data.iter().enumerate() {
            t.set_f32(i, v);
        }
        t
    }

    /// Map an existing buffer (e.g. Huge Page) as a tensor without copying.
    ///
    /// # Safety
    /// `ptr` must remain valid for the lifetime of this Tensor.
    /// `ptr` must be 64-byte aligned and point to `len * dtype.bytes()` bytes.
    pub unsafe fn from_raw_parts(
        ptr:   *mut u8,
        len:   usize,
        shape: &[usize],
        dtype: DataType,
    ) -> Self {
        debug_assert!(!ptr.is_null());
        debug_assert_eq!(ptr as usize % 64, 0, "ANE: huge-page ptr must be 64-byte aligned");
        let strides = Self::compute_strides(shape);
        Tensor { data: ptr, shape: shape.to_vec(), strides, len, dtype, owned: false }
    }

    // ── Strides ──────────────────────────────────────────────────────────────

    fn compute_strides(shape: &[usize]) -> Vec<usize> {
        let ndim = shape.len();
        let mut s = vec![1usize; ndim];
        for i in (0..ndim.saturating_sub(1)).rev() {
            s[i] = s[i + 1] * shape[i + 1];
        }
        s
    }

    // ── Shape accessors ──────────────────────────────────────────────────────

    #[inline] pub fn shape(&self)   -> &[usize] { &self.shape   }
    #[inline] pub fn strides(&self) -> &[usize] { &self.strides }
    #[inline] pub fn ndim(&self)    -> usize    { self.shape.len() }
    #[inline] pub fn rows(&self)    -> usize    { if self.ndim() >= 2 { self.shape[self.ndim()-2] } else { 1 } }
    #[inline] pub fn cols(&self)    -> usize    { if self.ndim() >= 1 { self.shape[self.ndim()-1] } else { 1 } }

    // ── Element access (F32) ─────────────────────────────────────────────────

    #[inline]
    pub fn get_f32(&self, idx: usize) -> f32 {
        debug_assert_eq!(self.dtype, DataType::F32);
        unsafe { *(self.data.add(idx * 4) as *const f32) }
    }

    #[inline]
    pub fn set_f32(&mut self, idx: usize, val: f32) {
        debug_assert_eq!(self.dtype, DataType::F32);
        unsafe { *(self.data.add(idx * 4) as *mut f32) = val; }
    }

    #[inline]
    pub fn as_f32_slice(&self) -> &[f32] {
        debug_assert_eq!(self.dtype, DataType::F32);
        unsafe { core::slice::from_raw_parts(self.data as *const f32, self.len) }
    }

    #[inline]
    pub fn as_f32_slice_mut(&mut self) -> &mut [f32] {
        debug_assert_eq!(self.dtype, DataType::F32);
        unsafe { core::slice::from_raw_parts_mut(self.data as *mut f32, self.len) }
    }

    /// Raw byte pointer (for FFI / SIMD).
    #[inline] pub fn as_ptr(&self) -> *const u8 { self.data }
    #[inline] pub fn as_mut_ptr(&self) -> *mut u8 { self.data }

    // ── Deep copy ────────────────────────────────────────────────────────────

    pub fn clone_tensor(&self) -> Tensor {
        let mut t = Tensor::zeros(&self.shape, self.dtype);
        unsafe {
            core::ptr::copy_nonoverlapping(
                self.data, t.data,
                self.len * self.dtype.bytes(),
            );
        }
        t
    }

    // ── Reshape (zero-copy if ndim fits) ─────────────────────────────────────

    pub fn reshape(&self, new_shape: &[usize]) -> Tensor {
        let new_len: usize = new_shape.iter().product();
        assert_eq!(self.len, new_len, "ANE: reshape size mismatch");
        // Borrow the buffer (not owned by the result)
        unsafe {
            Tensor::from_raw_parts(self.data, self.len, new_shape, self.dtype)
        }
    }

    // ── Elementwise ops ──────────────────────────────────────────────────────

    /// In-place ReLU: max(0, x).
    pub fn relu_inplace(&mut self) {
        assert_eq!(self.dtype, DataType::F32);
        #[cfg(target_arch = "x86_64")]
        unsafe {
            if crate::ane::has_avx512f() {
                relu_avx512(self.data as *mut f32, self.len);
            } else if crate::ane::has_avx2() {
                relu_avx2(self.data as *mut f32, self.len);
            } else {
                relu_scalar(self.data as *mut f32, self.len);
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        unsafe { relu_scalar(self.data as *mut f32, self.len); }
    }

    /// Out-of-place ReLU: returns new tensor.
    pub fn relu(&self) -> Tensor {
        let mut out = self.clone_tensor();
        out.relu_inplace();
        out
    }

    /// In-place Softmax along the last axis (row-wise for 2D).
    pub fn softmax_inplace(&mut self) {
        assert_eq!(self.dtype, DataType::F32);
        let cols = self.cols();
        let rows = self.len / cols;
        let data = unsafe { core::slice::from_raw_parts_mut(self.data as *mut f32, self.len) };
        for r in 0..rows {
            let row = &mut data[r*cols..(r+1)*cols];
            softmax_row(row);
        }
    }

    pub fn softmax(&self) -> Tensor {
        let mut out = self.clone_tensor();
        out.softmax_inplace();
        out
    }

    /// Elementwise add (returns new tensor).
    pub fn add(&self, other: &Tensor) -> Tensor {
        assert_eq!(self.len, other.len);
        assert_eq!(self.dtype, DataType::F32);
        let mut out = Tensor::zeros(&self.shape, DataType::F32);
        let a = self.as_f32_slice();
        let b = other.as_f32_slice();
        let c = out.as_f32_slice_mut();
        for i in 0..self.len { c[i] = a[i] + b[i]; }
        out
    }

    /// Elementwise multiply (hadamard).
    pub fn mul(&self, other: &Tensor) -> Tensor {
        assert_eq!(self.len, other.len);
        assert_eq!(self.dtype, DataType::F32);
        let mut out = Tensor::zeros(&self.shape, DataType::F32);
        let a = self.as_f32_slice();
        let b = other.as_f32_slice();
        let c = out.as_f32_slice_mut();
        for i in 0..self.len { c[i] = a[i] * b[i]; }
        out
    }

    /// Scale by scalar.
    pub fn scale(&self, s: f32) -> Tensor {
        let mut out = self.clone_tensor();
        for v in out.as_f32_slice_mut() { *v *= s; }
        out
    }

    // ── Transpose ────────────────────────────────────────────────────────────

    /// Transpose a 2-D matrix (returns new tensor).
    pub fn t(&self) -> Tensor {
        assert_eq!(self.ndim(), 2, "ANE: transpose requires 2D tensor");
        let (r, c) = (self.rows(), self.cols());
        let mut out = Tensor::zeros(&[c, r], DataType::F32);
        let src = self.as_f32_slice();
        let dst = out.as_f32_slice_mut();
        for i in 0..r {
            for j in 0..c {
                dst[j*r + i] = src[i*c + j];
            }
        }
        out
    }

    // ── GEMM: A (m×k) × B (k×n) → C (m×n) ──────────────────────────────────

    /// General matrix multiply.  A must be `[m, k]`, B must be `[k, n]`.
    pub fn matmul(&self, b: &Tensor) -> Tensor {
        assert_eq!(self.ndim(), 2);
        assert_eq!(b.ndim(),    2);
        let m = self.rows();
        let k = self.cols();
        assert_eq!(b.rows(), k, "ANE: matmul inner dim mismatch");
        let n = b.cols();

        let mut c = Tensor::zeros(&[m, n], DataType::F32);

        #[cfg(target_arch = "x86_64")]
        unsafe {
            if crate::ane::has_avx512f() {
                gemm_avx512(self.as_ptr() as *const f32, b.as_ptr() as *const f32,
                            c.as_mut_ptr() as *mut f32, m, k, n);
                return c;
            } else if crate::ane::has_avx2() {
                gemm_avx2(self.as_ptr() as *const f32, b.as_ptr() as *const f32,
                          c.as_mut_ptr() as *mut f32, m, k, n);
                return c;
            }
        }
        gemm_scalar(
            self.as_f32_slice(),
            b.as_f32_slice(),
            c.as_f32_slice_mut(),
            m, k, n,
        );
        c
    }

    // ── Reduction ────────────────────────────────────────────────────────────

    pub fn sum(&self) -> f32 {
        self.as_f32_slice().iter().copied().fold(0.0f32, |a, x| a + x)
    }

    pub fn mean(&self) -> f32 {
        if self.len == 0 { return 0.0; }
        self.sum() / self.len as f32
    }

    /// Sum over last axis, returns tensor of remaining shape.
    pub fn sum_last_axis(&self) -> Tensor {
        let cols = self.cols();
        let rows = self.len / cols;
        let mut out = Tensor::zeros(&[rows, 1], DataType::F32);
        let src = self.as_f32_slice();
        let dst = out.as_f32_slice_mut();
        for r in 0..rows {
            let mut s = 0.0f32;
            for c in 0..cols { s += src[r*cols + c]; }
            dst[r] = s;
        }
        out
    }

    // ── Layer-norm helpers ───────────────────────────────────────────────────

    /// Compute mean and variance along last axis for each row.
    pub fn mean_var_last(&self) -> (Vec<f32>, Vec<f32>) {
        let cols = self.cols();
        let rows = self.len / cols;
        let src = self.as_f32_slice();
        let mut means = vec![0.0f32; rows];
        let mut vars  = vec![0.0f32; rows];
        for r in 0..rows {
            let mut s  = 0.0f32;
            let mut s2 = 0.0f32;
            for c in 0..cols {
                let v = src[r*cols + c];
                s  += v;
                s2 += v * v;
            }
            let m = s / cols as f32;
            means[r] = m;
            vars[r]  = (s2 / cols as f32) - m * m;
        }
        (means, vars)
    }
}

impl Drop for Tensor {
    fn drop(&mut self) {
        if self.owned && !self.data.is_null() {
            let byte_size = (self.len * self.dtype.bytes()).max(1);
            let layout = Layout::from_size_align(byte_size, 64).unwrap();
            unsafe { alloc::alloc::dealloc(self.data, layout); }
        }
    }
}

// ─── Scalar GEMM with 16×16 tiling ───────────────────────────────────────────

/// Scalar GEMM: C = A(m×k) × B(k×n), row-major, 16×16 cache tiles.
fn gemm_scalar(a: &[f32], b: &[f32], c: &mut [f32], m: usize, k: usize, n: usize) {
    const TM: usize = 16;
    const TN: usize = 16;
    const TK: usize = 16;

    for i0 in (0..m).step_by(TM) {
        for j0 in (0..n).step_by(TN) {
            for p0 in (0..k).step_by(TK) {
                let i_end = (i0 + TM).min(m);
                let j_end = (j0 + TN).min(n);
                let p_end = (p0 + TK).min(k);
                for i in i0..i_end {
                    for p in p0..p_end {
                        let a_ip = a[i*k + p];
                        for j in j0..j_end {
                            c[i*n + j] += a_ip * b[p*n + j];
                        }
                    }
                }
            }
        }
    }
}

// ─── AVX2 GEMM ───────────────────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn gemm_avx2(a: *const f32, b: *const f32, c: *mut f32, m: usize, k: usize, n: usize) {
    use core::arch::x86_64::*;
    const TM: usize = 8;
    const TN: usize = 8;
    const TK: usize = 8;

    for i0 in (0..m).step_by(TM) {
        for j0 in (0..n).step_by(TN) {
            for p0 in (0..k).step_by(TK) {
                let i_end = (i0 + TM).min(m);
                let j_end = (j0 + TN).min(n);
                let p_end = (p0 + TK).min(k);
                for i in i0..i_end {
                    let j_full_end = j0 + ((j_end - j0) / 8) * 8;
                    for j in (j0..j_full_end).step_by(8) {
                        let mut acc = _mm256_loadu_ps(c.add(i*n + j));
                        for p in p0..p_end {
                            let a_v = _mm256_set1_ps(*a.add(i*k + p));
                            let b_v = _mm256_loadu_ps(b.add(p*n + j));
                            acc = _mm256_fmadd_ps(a_v, b_v, acc);
                        }
                        _mm256_storeu_ps(c.add(i*n + j), acc);
                    }
                    // Scalar tail
                    for j in j_full_end..j_end {
                        for p in p0..p_end {
                            *c.add(i*n + j) += *a.add(i*k + p) * *b.add(p*n + j);
                        }
                    }
                }
            }
        }
    }
}

// ─── AVX-512 GEMM ────────────────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
unsafe fn gemm_avx512(a: *const f32, b: *const f32, c: *mut f32, m: usize, k: usize, n: usize) {
    use core::arch::x86_64::*;
    const TM: usize = 16;
    const TN: usize = 16;
    const TK: usize = 16;

    for i0 in (0..m).step_by(TM) {
        for j0 in (0..n).step_by(TN) {
            for p0 in (0..k).step_by(TK) {
                let i_end = (i0 + TM).min(m);
                let j_end = (j0 + TN).min(n);
                let p_end = (p0 + TK).min(k);
                for i in i0..i_end {
                    let j_full_end = j0 + ((j_end - j0) / 16) * 16;
                    for j in (j0..j_full_end).step_by(16) {
                        let mut acc = _mm512_loadu_ps(c.add(i*n + j));
                        for p in p0..p_end {
                            let a_v = _mm512_set1_ps(*a.add(i*k + p));
                            let b_v = _mm512_loadu_ps(b.add(p*n + j));
                            acc = _mm512_fmadd_ps(a_v, b_v, acc);
                        }
                        _mm512_storeu_ps(c.add(i*n + j), acc);
                    }
                    for j in j_full_end..j_end {
                        for p in p0..p_end {
                            *c.add(i*n + j) += *a.add(i*k + p) * *b.add(p*n + j);
                        }
                    }
                }
            }
        }
    }
}

// ─── ReLU kernels ─────────────────────────────────────────────────────────────

unsafe fn relu_scalar(p: *mut f32, n: usize) {
    for i in 0..n {
        let v = *p.add(i);
        *p.add(i) = if v > 0.0 { v } else { 0.0 };
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn relu_avx2(p: *mut f32, n: usize) {
    use core::arch::x86_64::*;
    let zero = _mm256_setzero_ps();
    let full = n / 8 * 8;
    for i in (0..full).step_by(8) {
        let v = _mm256_loadu_ps(p.add(i));
        _mm256_storeu_ps(p.add(i), _mm256_max_ps(v, zero));
    }
    for i in full..n {
        let v = *p.add(i);
        *p.add(i) = if v > 0.0 { v } else { 0.0 };
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
unsafe fn relu_avx512(p: *mut f32, n: usize) {
    use core::arch::x86_64::*;
    let zero = _mm512_setzero_ps();
    let full = n / 16 * 16;
    for i in (0..full).step_by(16) {
        let v = _mm512_loadu_ps(p.add(i));
        _mm512_storeu_ps(p.add(i), _mm512_max_ps(v, zero));
    }
    for i in full..n {
        let v = *p.add(i);
        *p.add(i) = if v > 0.0 { v } else { 0.0 };
    }
}

// ─── Softmax helpers ─────────────────────────────────────────────────────────

/// Numerically stable softmax over a single row (in-place).
fn softmax_row(row: &mut [f32]) {
    // Fast exp approximation: e^x ≈ (1 + x/256)^256 via repeated squaring
    // For kernel use, we use a 5-term minimax polynomial approximation.
    let max = row.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut sum = 0.0f32;
    for v in row.iter_mut() {
        *v = fast_exp(*v - max);
        sum += *v;
    }
    let inv = 1.0 / sum;
    for v in row.iter_mut() { *v *= inv; }
}

/// Fast f32 exp approximation via Schraudolph's method.
/// Max relative error ≈ 2.4% — sufficient for softmax in inference.
#[inline]
fn fast_exp(x: f32) -> f32 {
    // Clamp to avoid overflow/underflow
    let x = if x < -87.3365_f32 { -87.3365_f32 }
             else if x > 88.7228_f32 { 88.7228_f32 }
             else { x };
    // Uses integer bit trick: 2^x ≈ reinterpret((x/ln2 + 127) * 2^23) as f32
    let v = (x * 1.4426950409_f32 + 127.0_f32) * (1u32 << 23) as f32;
    let bits = v as u32;
    f32::from_bits(bits)
}

// ─── Autograd ─────────────────────────────────────────────────────────────────

/// Backward function: takes upstream gradient, returns downstream gradients
/// in the same order as the inputs to the forward op.
type BackwardFn = Box<dyn Fn(&Tensor) -> Vec<Tensor>>;

/// A differentiable variable: wraps a Tensor with an optional gradient
/// and backward function for reverse-mode AD.
pub struct Variable {
    pub data:  Tensor,
    pub grad:  Option<Tensor>,
    /// Whether gradient is accumulated for this variable.
    pub requires_grad: bool,
    /// Recorded backward pass (None for leaf variables).
    grad_fn: Option<BackwardFn>,
}

impl Variable {
    /// Leaf variable (no parents).
    pub fn new(data: Tensor, requires_grad: bool) -> Self {
        Variable { data, grad: None, requires_grad, grad_fn: None }
    }

    /// Wrap a scalar (1-element) f32.
    pub fn scalar(val: f32, requires_grad: bool) -> Self {
        Variable::new(Tensor::from_slice_f32(&[val]), requires_grad)
    }

    /// Zero the accumulated gradient.
    pub fn zero_grad(&mut self) {
        if let Some(ref mut g) = self.grad {
            for v in g.as_f32_slice_mut() { *v = 0.0; }
        }
    }

    /// Set an explicit gradient function (used internally by ops).
    pub(crate) fn with_grad_fn(mut self, f: BackwardFn) -> Self {
        self.grad_fn = Some(f);
        self.requires_grad = true;
        self
    }

    /// Accumulate upstream gradient into self.grad.
    fn accumulate_grad(&mut self, g: Tensor) {
        match &mut self.grad {
            Some(existing) => {
                let e = existing.as_f32_slice_mut();
                let u = g.as_f32_slice();
                for i in 0..e.len() { e[i] += u[i]; }
            }
            None => { self.grad = Some(g); }
        }
    }
}

// ─── Differentiable ops ──────────────────────────────────────────────────────

/// Add two Variables: z = a + b.
/// dL/da = dL/dz,  dL/db = dL/dz
pub fn var_add(a: &Variable, b: &Variable) -> Variable {
    let result = a.data.add(&b.data);
    let a_shape = a.data.shape().to_vec();
    let b_shape = b.data.shape().to_vec();
    let a_rg = a.requires_grad;
    let b_rg = b.requires_grad;

    let grad_fn: BackwardFn = Box::new(move |grad: &Tensor| {
        let mut grads = Vec::new();
        if a_rg {
            grads.push(Tensor::from_flat_f32(grad.as_f32_slice(), a_shape[0], if a_shape.len() > 1 { a_shape[1] } else { 1 }));
        } else {
            grads.push(Tensor::zeros(&a_shape, DataType::F32));
        }
        if b_rg {
            grads.push(Tensor::from_flat_f32(grad.as_f32_slice(), b_shape[0], if b_shape.len() > 1 { b_shape[1] } else { 1 }));
        } else {
            grads.push(Tensor::zeros(&b_shape, DataType::F32));
        }
        grads
    });

    let mut v = Variable::new(result, a.requires_grad || b.requires_grad);
    v.grad_fn = Some(grad_fn);
    v
}

/// Elementwise multiply two Variables: z = a * b.
/// dL/da = dL/dz * b,  dL/db = dL/dz * a
pub fn var_mul(a: &Variable, b: &Variable) -> Variable {
    let result = a.data.mul(&b.data);
    let a_data_copy = a.data.clone_tensor();
    let b_data_copy = b.data.clone_tensor();
    let a_rg = a.requires_grad;
    let b_rg = b.requires_grad;

    let grad_fn: BackwardFn = Box::new(move |grad: &Tensor| {
        let ga = if a_rg { grad.mul(&b_data_copy) } else { Tensor::zeros(b_data_copy.shape(), DataType::F32) };
        let gb = if b_rg { grad.mul(&a_data_copy) } else { Tensor::zeros(a_data_copy.shape(), DataType::F32) };
        vec![ga, gb]
    });

    let mut v = Variable::new(result, a.requires_grad || b.requires_grad);
    v.grad_fn = Some(grad_fn);
    v
}

/// Matrix-multiply two Variables: z = a @ b.
/// dL/da = dL/dz @ b^T,  dL/db = a^T @ dL/dz
pub fn var_matmul(a: &Variable, b: &Variable) -> Variable {
    let result = a.data.matmul(&b.data);
    let a_data_copy = a.data.clone_tensor();
    let b_data_copy = b.data.clone_tensor();
    let a_rg = a.requires_grad;
    let b_rg = b.requires_grad;
    let a_shape = a.data.shape().to_vec();
    let b_shape = b.data.shape().to_vec();

    let grad_fn: BackwardFn = Box::new(move |grad: &Tensor| {
        let ga = if a_rg { grad.matmul(&b_data_copy.t()) } else { Tensor::zeros(&a_shape, DataType::F32) };
        let gb = if b_rg { a_data_copy.t().matmul(grad)  } else { Tensor::zeros(&b_shape, DataType::F32) };
        vec![ga, gb]
    });

    let mut v = Variable::new(result, a.requires_grad || b.requires_grad);
    v.grad_fn = Some(grad_fn);
    v
}

/// ReLU on a Variable.
/// dL/dx = dL/dz * (x > 0)
pub fn var_relu(a: &Variable) -> Variable {
    let result = a.data.relu();
    let a_data_copy = a.data.clone_tensor();
    let a_rg = a.requires_grad;
    let a_shape = a.data.shape().to_vec();

    let grad_fn: BackwardFn = Box::new(move |grad: &Tensor| {
        let g = if a_rg {
            let src  = a_data_copy.as_f32_slice();
            let gsrc = grad.as_f32_slice();
            let mut out = Tensor::zeros(&a_shape, DataType::F32);
            {
                let dst = out.as_f32_slice_mut();
                for i in 0..dst.len() {
                    dst[i] = if src[i] > 0.0 { gsrc[i] } else { 0.0 };
                }
            }
            out
        } else {
            Tensor::zeros(&a_shape, DataType::F32)
        };
        vec![g]
    });

    let mut v = Variable::new(result, a.requires_grad);
    v.grad_fn = Some(grad_fn);
    v
}

// ─── Tape & backward ─────────────────────────────────────────────────────────

/// Represents a node in the backward graph held on the Tape.
pub struct TapeNode {
    /// Gradient function
    pub grad_fn: BackwardFn,
    /// Index of output variable (receives upstream grad)
    pub output_id: usize,
    /// Indices of input variable(s) (receive downstream grads)
    pub input_ids: Vec<usize>,
}

/// Dynamic reverse-mode autograd tape.
///
/// Usage:
/// ```
/// let mut tape = Tape::new();
/// let a = tape.var(Tensor::from_slice_f32(&[1.0, 2.0, 3.0]), true);
/// let b = tape.var(Tensor::from_slice_f32(&[4.0, 5.0, 6.0]), true);
/// let c = tape.add(a, b);
/// tape.backward(c, Tensor::full_f32(&[3], 1.0));
/// // tape.grad(a) → [1,1,1]
/// ```
pub struct Tape {
    /// All Variables ever created on this tape.
    pub vars: Vec<Variable>,
    /// Backward graph nodes.
    pub nodes: Vec<TapeNode>,
}

impl Tape {
    pub fn new() -> Self {
        Tape { vars: Vec::new(), nodes: Vec::new() }
    }

    /// Create a new Variable on the tape, returning its index.
    pub fn var(&mut self, data: Tensor, requires_grad: bool) -> usize {
        let id = self.vars.len();
        self.vars.push(Variable::new(data, requires_grad));
        id
    }

    /// Add two tape variables.
    pub fn add(&mut self, a: usize, b: usize) -> usize {
        let result = {
            let va = &self.vars[a];
            let vb = &self.vars[b];
            va.data.add(&vb.data)
        };
        let out_id = self.vars.len();
        let rg = self.vars[a].requires_grad || self.vars[b].requires_grad;
        self.vars.push(Variable::new(result, rg));

        let a_rg = self.vars[a].requires_grad;
        let b_rg = self.vars[b].requires_grad;
        let a_shape = self.vars[a].data.shape().to_vec();
        let b_shape = self.vars[b].data.shape().to_vec();

        self.nodes.push(TapeNode {
            grad_fn: Box::new(move |g: &Tensor| {
                let ga = if a_rg { g.clone_tensor()  } else { Tensor::zeros(&a_shape, DataType::F32) };
                let gb = if b_rg { g.clone_tensor()  } else { Tensor::zeros(&b_shape, DataType::F32) };
                vec![ga, gb]
            }),
            output_id: out_id,
            input_ids: vec![a, b],
        });
        out_id
    }

    /// Elementwise multiply two tape variables.
    pub fn mul(&mut self, a: usize, b: usize) -> usize {
        let (result, a_clone, b_clone) = {
            let va = &self.vars[a];
            let vb = &self.vars[b];
            (va.data.mul(&vb.data), va.data.clone_tensor(), vb.data.clone_tensor())
        };
        let out_id = self.vars.len();
        let rg = self.vars[a].requires_grad || self.vars[b].requires_grad;
        self.vars.push(Variable::new(result, rg));

        let a_rg = self.vars[a].requires_grad;
        let b_rg = self.vars[b].requires_grad;

        self.nodes.push(TapeNode {
            grad_fn: Box::new(move |g: &Tensor| {
                let ga = if a_rg { g.mul(&b_clone) } else { Tensor::zeros(b_clone.shape(), DataType::F32) };
                let gb = if b_rg { g.mul(&a_clone) } else { Tensor::zeros(a_clone.shape(), DataType::F32) };
                vec![ga, gb]
            }),
            output_id: out_id,
            input_ids: vec![a, b],
        });
        out_id
    }

    /// Matrix multiply two tape variables.
    pub fn matmul(&mut self, a: usize, b: usize) -> usize {
        let (result, a_clone, b_clone) = {
            let va = &self.vars[a];
            let vb = &self.vars[b];
            (va.data.matmul(&vb.data), va.data.clone_tensor(), vb.data.clone_tensor())
        };
        let out_id = self.vars.len();
        let rg = self.vars[a].requires_grad || self.vars[b].requires_grad;
        self.vars.push(Variable::new(result, rg));

        let a_rg = self.vars[a].requires_grad;
        let b_rg = self.vars[b].requires_grad;
        let a_shape = a_clone.shape().to_vec();
        let b_shape = b_clone.shape().to_vec();

        self.nodes.push(TapeNode {
            grad_fn: Box::new(move |g: &Tensor| {
                let ga = if a_rg { g.matmul(&b_clone.t()) } else { Tensor::zeros(&a_shape, DataType::F32) };
                let gb = if b_rg { a_clone.t().matmul(g)  } else { Tensor::zeros(&b_shape, DataType::F32) };
                vec![ga, gb]
            }),
            output_id: out_id,
            input_ids: vec![a, b],
        });
        out_id
    }

    /// ReLU of a tape variable.
    pub fn relu(&mut self, a: usize) -> usize {
        let (result, a_clone) = {
            let va = &self.vars[a];
            (va.data.relu(), va.data.clone_tensor())
        };
        let out_id = self.vars.len();
        let rg = self.vars[a].requires_grad;
        self.vars.push(Variable::new(result, rg));
        let a_shape = a_clone.shape().to_vec();

        self.nodes.push(TapeNode {
            grad_fn: Box::new(move |g: &Tensor| {
                let mut out = Tensor::zeros(&a_shape, DataType::F32);
                {
                    let src  = a_clone.as_f32_slice();
                    let gsrc = g.as_f32_slice();
                    let dst  = out.as_f32_slice_mut();
                    for i in 0..dst.len() {
                        dst[i] = if src[i] > 0.0 { gsrc[i] } else { 0.0 };
                    }
                }
                vec![out]
            }),
            output_id: out_id,
            input_ids: vec![a],
        });
        out_id
    }

    /// Run reverse-mode backward pass starting from `output_id`.
    /// `initial_grad` is typically an all-ones tensor (dL/dL = 1).
    pub fn backward(&mut self, output_id: usize, initial_grad: Tensor) {
        // Accumulate initial gradient
        self.vars[output_id].accumulate_grad(initial_grad);

        // Traverse nodes in reverse order (topological reverse)
        for ni in (0..self.nodes.len()).rev() {
            let out_id = self.nodes[ni].output_id;
            if out_id != output_id && self.vars[out_id].grad.is_none() {
                continue; // Not reachable from output
            }
            let upstream = match &self.vars[out_id].grad {
                Some(g) => g.clone_tensor(),
                None => continue,
            };
            let input_grads = (self.nodes[ni].grad_fn)(&upstream);
            for (k, &inp_id) in self.nodes[ni].input_ids.iter().enumerate() {
                if k < input_grads.len() && self.vars[inp_id].requires_grad {
                    let g = input_grads[k].clone_tensor();
                    self.vars[inp_id].accumulate_grad(g);
                }
            }
        }
    }

    /// Get gradient of a variable. Returns None if not computed yet.
    pub fn grad(&self, id: usize) -> Option<&Tensor> {
        self.vars[id].grad.as_ref()
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────
// Run with: cargo test --lib (on x86_64 host, std-backed alloc)

#[cfg(test)]
mod tests {
    use super::*;

    // ── Tensor allocation + element access ───────────────────────────────────

    #[test]
    fn test_zeros_shape_and_dtype() {
        let t = Tensor::zeros(&[3, 4], DataType::F32);
        assert_eq!(t.shape(), &[3, 4]);
        assert_eq!(t.len, 12);
        assert_eq!(t.dtype, DataType::F32);
    }

    #[test]
    fn test_zeros_all_zero() {
        let t = Tensor::zeros(&[8], DataType::F32);
        for i in 0..8 {
            assert_eq!(t.get_f32(i), 0.0, "element {i} should be zero");
        }
    }

    #[test]
    fn test_set_and_get_f32() {
        let mut t = Tensor::zeros(&[4], DataType::F32);
        t.set_f32(0, 1.0);
        t.set_f32(3, -7.5);
        assert_eq!(t.get_f32(0), 1.0);
        assert_eq!(t.get_f32(1), 0.0);
        assert_eq!(t.get_f32(3), -7.5);
    }

    #[test]
    fn test_from_slice_f32() {
        let data = [1.0f32, 2.0, 3.0, 4.0];
        let t = Tensor::from_slice_f32(&data);
        assert_eq!(t.len, 4);
        for (i, &v) in data.iter().enumerate() {
            assert_eq!(t.get_f32(i), v);
        }
    }

    #[test]
    fn test_full_f32() {
        let t = Tensor::full_f32(&[5], 3.14);
        for i in 0..5 {
            assert!((t.get_f32(i) - 3.14).abs() < 1e-5);
        }
    }

    #[test]
    fn test_strides_row_major() {
        let t = Tensor::zeros(&[2, 3, 4], DataType::F32);
        assert_eq!(t.strides(), &[12, 4, 1]);
    }

    // ── Clone ────────────────────────────────────────────────────────────────

    #[test]
    fn test_clone_tensor() {
        let mut a = Tensor::from_slice_f32(&[1.0, 2.0, 3.0]);
        let b = a.clone_tensor();
        a.set_f32(0, 99.0);       // mutate original
        assert_eq!(b.get_f32(0), 1.0);   // clone is independent
    }

    // ── ReLU ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_relu_inplace() {
        let mut t = Tensor::from_slice_f32(&[-2.0, 0.0, 3.0, -0.5, 1.0]);
        t.relu_inplace();
        let s = t.as_f32_slice();
        assert_eq!(s, &[0.0, 0.0, 3.0, 0.0, 1.0]);
    }

    // ── Softmax ──────────────────────────────────────────────────────────────

    #[test]
    fn test_softmax_sums_to_one() {
        let mut t = Tensor::from_slice_f32(&[1.0, 2.0, 3.0, 4.0]);
        t.softmax_inplace();
        let sum: f32 = t.as_f32_slice().iter().sum();
        assert!((sum - 1.0).abs() < 1e-5, "softmax sum={sum}, expected ~1.0");
    }

    #[test]
    fn test_softmax_monotone() {
        let mut t = Tensor::from_slice_f32(&[1.0, 2.0, 3.0]);
        t.softmax_inplace();
        let s = t.as_f32_slice();
        // Higher input → higher probability
        assert!(s[0] < s[1] && s[1] < s[2], "softmax must be monotone");
    }

    // ── GEMM ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_matmul_identity() {
        // A × I = A
        let a = Tensor::from_flat_f32(&[1.0, 2.0, 3.0, 4.0], 2, 2);
        let eye = Tensor::from_flat_f32(&[1.0, 0.0, 0.0, 1.0], 2, 2);
        let c = a.matmul(&eye);
        let s = c.as_f32_slice();
        assert!((s[0] - 1.0).abs() < 1e-5);
        assert!((s[1] - 2.0).abs() < 1e-5);
        assert!((s[2] - 3.0).abs() < 1e-5);
        assert!((s[3] - 4.0).abs() < 1e-5);
    }

    #[test]
    fn test_matmul_2x2() {
        // [[1,2],[3,4]] × [[5,6],[7,8]] = [[19,22],[43,50]]
        let a = Tensor::from_flat_f32(&[1.0, 2.0, 3.0, 4.0], 2, 2);
        let b = Tensor::from_flat_f32(&[5.0, 6.0, 7.0, 8.0], 2, 2);
        let c = a.matmul(&b);
        let s = c.as_f32_slice();
        assert!((s[0] - 19.0).abs() < 1e-4, "expected 19, got {}", s[0]);
        assert!((s[1] - 22.0).abs() < 1e-4, "expected 22, got {}", s[1]);
        assert!((s[2] - 43.0).abs() < 1e-4, "expected 43, got {}", s[2]);
        assert!((s[3] - 50.0).abs() < 1e-4, "expected 50, got {}", s[3]);
    }

    #[test]
    fn test_matmul_rectangular() {
        // (2×3) × (3×2) → (2×2)
        let a = Tensor::from_flat_f32(&[1.0,0.0,0.0, 0.0,1.0,0.0], 2, 3);
        let b = Tensor::from_flat_f32(&[1.0,2.0, 3.0,4.0, 5.0,6.0], 3, 2);
        let c = a.matmul(&b);
        assert_eq!(c.shape(), &[2, 2]);
        let s = c.as_f32_slice();
        // Row 0 = first row of A × B = [1,0,0]×B = [1,2]
        assert!((s[0]-1.0).abs() < 1e-5 && (s[1]-2.0).abs() < 1e-5);
        // Row 1 = [0,1,0]×B = [3,4]
        assert!((s[2]-3.0).abs() < 1e-5 && (s[3]-4.0).abs() < 1e-5);
    }

    // ── Add / sub ────────────────────────────────────────────────────────────

    #[test]
    fn test_add_elementwise() {
        let a = Tensor::from_slice_f32(&[1.0, 2.0, 3.0]);
        let b = Tensor::from_slice_f32(&[4.0, 5.0, 6.0]);
        let c = a.add(&b);
        let s = c.as_f32_slice();
        assert_eq!(s, &[5.0, 7.0, 9.0]);
    }

    // ── Autograd: var_add, backward ───────────────────────────────────────────

    #[test]
    fn test_autograd_add_backward() {
        let mut tape = Tape::new();
        let x_data = Tensor::from_slice_f32(&[2.0, 3.0]);
        let y_data = Tensor::from_slice_f32(&[1.0, 1.0]);
        let x = tape.leaf(x_data, true);
        let y = tape.leaf(y_data, false);
        let z = tape.add(x, y);   // z = x + y
        let ones = Tensor::full_f32(&[2], 1.0);
        tape.backward(z, ones);
        let dx = tape.grad(x).expect("x should have grad");
        // d(x+y)/dx = 1 everywhere
        assert!((dx.get_f32(0) - 1.0).abs() < 1e-5);
        assert!((dx.get_f32(1) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_autograd_relu_backward() {
        let mut tape = Tape::new();
        let x_data = Tensor::from_slice_f32(&[1.0, -2.0, 3.0]);
        let x = tape.leaf(x_data, true);
        let y = tape.relu(x);
        let ones = Tensor::full_f32(&[3], 1.0);
        tape.backward(y, ones);
        let dx = tape.grad(x).expect("x should have grad");
        // d(relu)/dx = 1 if x>0, else 0
        assert!((dx.get_f32(0) - 1.0).abs() < 1e-5, "relu backward: x=1 → 1");
        assert!((dx.get_f32(1)).abs()       < 1e-5, "relu backward: x=-2 → 0");
        assert!((dx.get_f32(2) - 1.0).abs() < 1e-5, "relu backward: x=3 → 1");
    }

    // ── DataType bytes ────────────────────────────────────────────────────────

    #[test]
    fn test_dtype_bytes() {
        assert_eq!(DataType::F32.bytes(), 4);
        assert_eq!(DataType::F16.bytes(), 2);
        assert_eq!(DataType::I8.bytes(),  1);
    }
}
