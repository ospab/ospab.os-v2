/*
Boost Software License - Version 1.1
Copyright (c) 2026 ospab

ANE Optimizers — AdamW and SGD weight-update kernels.

Both optimizers operate directly on raw f32 slices via SIMD to match
or exceed PyTorch overhead-free kernel performance.

API:
    let mut opt = AdamW::new(lr, beta1, beta2, eps, weight_decay);
    opt.step(param, grad);   // update param in-place
*/

extern crate alloc;

use alloc::vec::Vec;
use alloc::vec;

use super::tensor::Tensor;

/// Newton-Raphson square root for no_std environments.
#[inline(always)]
fn sqrtf(x: f32) -> f32 {
    if x <= 0.0 { return 0.0; }
    let mut y = f32::from_bits((x.to_bits().wrapping_add(0x3f80_0000)) >> 1);
    y = 0.5 * (y + x / y);
    y = 0.5 * (y + x / y);
    y
}

// ─── Optimizer trait ─────────────────────────────────────────────────────────

pub trait Optimizer {
    /// Update a parameter tensor given its gradient tensor (in-place).
    fn step(&mut self, param: &mut Tensor, grad: &Tensor);
    /// Reset internal moment buffers.
    fn reset(&mut self);
}

// ─── AdamW ───────────────────────────────────────────────────────────────────

/// AdamW: Adam with decoupled weight decay.
///
/// p_{t+1} = p_t − lr * [m̂_t / (√v̂_t + ε) + wd * p_t]
///
/// Moment buffers grow lazily when step() is called for the first time.
pub struct AdamW {
    pub lr:    f32,
    pub beta1: f32,
    pub beta2: f32,
    pub eps:   f32,
    pub weight_decay: f32,
    t:  u64,          // timestep count
    bc1_prod: f32,    // accumulated: beta1^t
    bc2_prod: f32,    // accumulated: beta2^t
    m:  Vec<f32>,     // 1st moment
    v:  Vec<f32>,     // 2nd moment
}

impl AdamW {
    pub fn new(lr: f32, beta1: f32, beta2: f32, eps: f32, weight_decay: f32) -> Self {
        AdamW { lr, beta1, beta2, eps, weight_decay, t: 0, bc1_prod: 1.0, bc2_prod: 1.0, m: Vec::new(), v: Vec::new() }
    }

    /// Convenience constructor with common defaults.
    pub fn default_lr(lr: f32) -> Self {
        Self::new(lr, 0.9, 0.999, 1e-8, 0.01)
    }
}

impl Optimizer for AdamW {
    fn step(&mut self, param: &mut Tensor, grad: &Tensor) {
        let n = param.len;
        // Lazy init moment buffers
        if self.m.len() != n {
            self.m = vec![0.0f32; n];
            self.v = vec![0.0f32; n];
        }
        self.t += 1;
        self.bc1_prod *= self.beta1;
        self.bc2_prod *= self.beta2;
        let b1 = self.beta1;
        let b2 = self.beta2;
        let lr = self.lr;
        let wd = self.weight_decay;
        let eps = self.eps;

        // Bias correction (iterative, no powf)
        let bc1 = 1.0 - self.bc1_prod;
        let bc2 = 1.0 - self.bc2_prod;

        let p  = param.as_f32_slice_mut();
        let g  = grad.as_f32_slice();
        let m  = self.m.as_mut_slice();
        let v  = self.v.as_mut_slice();

        #[cfg(target_arch = "x86_64")]
        unsafe {
            if crate::ane::has_avx2() {
                adamw_avx2(p, g, m, v, n, b1, b2, eps, lr, wd, bc1, bc2);
                return;
            }
        }
        adamw_scalar(p, g, m, v, n, b1, b2, eps, lr, wd, bc1, bc2);
    }

    fn reset(&mut self) {
        self.t = 0;
        self.bc1_prod = 1.0;
        self.bc2_prod = 1.0;
        self.m.clear();
        self.v.clear();
    }
}

fn adamw_scalar(
    p: &mut [f32], g: &[f32], m: &mut [f32], v: &mut [f32],
    n: usize, b1: f32, b2: f32, eps: f32, lr: f32, wd: f32,
    bc1: f32, bc2: f32,
) {
    for i in 0..n {
        m[i] = b1 * m[i] + (1.0 - b1) * g[i];
        v[i] = b2 * v[i] + (1.0 - b2) * g[i] * g[i];
        let m_hat = m[i] / bc1;
        let v_hat = v[i] / bc2;
        p[i] -= lr * (m_hat / (sqrtf(v_hat) + eps) + wd * p[i]);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn adamw_avx2(
    p: &mut [f32], g: &[f32], m: &mut [f32], v: &mut [f32],
    n: usize, b1: f32, b2: f32, eps: f32, lr: f32, wd: f32,
    bc1: f32, bc2: f32,
) {
    use core::arch::x86_64::*;
    let v_b1        = _mm256_set1_ps(b1);
    let v_1mb1      = _mm256_set1_ps(1.0 - b1);
    let v_b2        = _mm256_set1_ps(b2);
    let v_1mb2      = _mm256_set1_ps(1.0 - b2);
    let v_bc1_inv   = _mm256_set1_ps(1.0 / bc1);
    let v_bc2_inv   = _mm256_set1_ps(1.0 / bc2);
    let v_eps       = _mm256_set1_ps(eps);
    let v_lr        = _mm256_set1_ps(lr);
    let v_wd        = _mm256_set1_ps(wd);

    let full = n / 8 * 8;
    for i in (0..full).step_by(8) {
        let gi = _mm256_loadu_ps(g.as_ptr().add(i));
        let pi = _mm256_loadu_ps(p.as_ptr().add(i));

        // m = b1*m + (1-b1)*g
        let mi = _mm256_fmadd_ps(v_b1, _mm256_loadu_ps(m.as_ptr().add(i)), _mm256_mul_ps(v_1mb1, gi));
        // v = b2*v + (1-b2)*g²
        let vi = _mm256_fmadd_ps(v_b2, _mm256_loadu_ps(v.as_ptr().add(i)), _mm256_mul_ps(v_1mb2, _mm256_mul_ps(gi, gi)));

        _mm256_storeu_ps(m.as_mut_ptr().add(i), mi);
        _mm256_storeu_ps(v.as_mut_ptr().add(i), vi);

        // m_hat = m / bc1,  v_hat = v / bc2
        let m_hat = _mm256_mul_ps(mi, v_bc1_inv);
        let v_hat = _mm256_mul_ps(vi, v_bc2_inv);

        // update = m_hat / (sqrt(v_hat) + eps) + wd*p
        let denom  = _mm256_add_ps(_mm256_sqrt_ps(v_hat), v_eps);
        let update = _mm256_fmadd_ps(v_wd, pi, _mm256_div_ps(m_hat, denom));
        let new_pi = _mm256_fnmadd_ps(v_lr, update, pi);
        _mm256_storeu_ps(p.as_mut_ptr().add(i), new_pi);
    }
    // Scalar tail
    for i in full..n {
        m[i] = b1 * m[i] + (1.0 - b1) * g[i];
        v[i] = b2 * v[i] + (1.0 - b2) * g[i] * g[i];
        let m_hat = m[i] / bc1;
        let v_hat = v[i] / bc2;
        p[i] -= lr * (m_hat / (sqrtf(v_hat) + eps) + wd * p[i]);
    }
}

// ─── SGD ─────────────────────────────────────────────────────────────────────

/// SGD with optional momentum and weight decay.
///
/// buf_{t}  = momentum * buf_{t-1} + g + wd * p
/// p_{t+1}  = p_t − lr * buf_t
pub struct Sgd {
    pub lr:           f32,
    pub momentum:     f32,
    pub weight_decay: f32,
    buf: Vec<f32>,
}

impl Sgd {
    pub fn new(lr: f32, momentum: f32, weight_decay: f32) -> Self {
        Sgd { lr, momentum, weight_decay, buf: Vec::new() }
    }
    pub fn vanilla(lr: f32) -> Self { Self::new(lr, 0.0, 0.0) }
}

impl Optimizer for Sgd {
    fn step(&mut self, param: &mut Tensor, grad: &Tensor) {
        let n  = param.len;
        if self.buf.len() != n { self.buf = vec![0.0f32; n]; }
        let p  = param.as_f32_slice_mut();
        let g  = grad.as_f32_slice();
        let lr = self.lr;
        let mu = self.momentum;
        let wd = self.weight_decay;
        let b  = self.buf.as_mut_slice();

        #[cfg(target_arch = "x86_64")]
        unsafe {
            if crate::ane::has_avx2() {
                sgd_avx2(p, g, b, n, lr, mu, wd);
                return;
            }
        }
        sgd_scalar(p, g, b, n, lr, mu, wd);
    }

    fn reset(&mut self) { self.buf.clear(); }
}

fn sgd_scalar(p: &mut [f32], g: &[f32], buf: &mut [f32], n: usize, lr: f32, mu: f32, wd: f32) {
    for i in 0..n {
        buf[i] = mu * buf[i] + g[i] + wd * p[i];
        p[i]  -= lr * buf[i];
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
unsafe fn sgd_avx2(p: &mut [f32], g: &[f32], buf: &mut [f32], n: usize, lr: f32, mu: f32, wd: f32) {
    use core::arch::x86_64::*;
    let v_lr = _mm256_set1_ps(lr);
    let v_mu = _mm256_set1_ps(mu);
    let v_wd = _mm256_set1_ps(wd);
    let full = n / 8 * 8;
    for i in (0..full).step_by(8) {
        let pi = _mm256_loadu_ps(p.as_ptr().add(i));
        let gi = _mm256_loadu_ps(g.as_ptr().add(i));
        let bi = _mm256_loadu_ps(buf.as_ptr().add(i));
        // buf = mu*buf + g + wd*p
        let new_b = _mm256_fmadd_ps(v_wd, pi, _mm256_fmadd_ps(v_mu, bi, gi));
        _mm256_storeu_ps(buf.as_mut_ptr().add(i), new_b);
        // p = p - lr*buf
        let new_p = _mm256_fnmadd_ps(v_lr, new_b, pi);
        _mm256_storeu_ps(p.as_mut_ptr().add(i), new_p);
    }
    for i in full..n {
        buf[i] = mu * buf[i] + g[i] + wd * p[i];
        p[i]  -= lr * buf[i];
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tensor::{DataType, Tensor};

    // ── sqrtf helper ─────────────────────────────────────────────────────────

    #[test]
    fn test_sqrtf_known_values() {
        assert!((sqrtf(4.0)  - 2.0).abs() < 1e-4);
        assert!((sqrtf(9.0)  - 3.0).abs() < 1e-4);
        assert!((sqrtf(2.0)  - 1.41421356).abs() < 1e-4);
        assert_eq!(sqrtf(0.0), 0.0);
    }

    // ── AdamW ────────────────────────────────────────────────────────────────

    #[test]
    fn test_adamw_decreases_loss() {
        let mut opt = AdamW::default_lr(0.01);
        let mut param = Tensor::from_slice_f32(&[1.0f32; 4]);
        let grad  = Tensor::from_slice_f32(&[0.5f32; 4]);
        let initial = param.get_f32(0);
        for _ in 0..10 {
            opt.step(&mut param, &grad);
        }
        let after = param.get_f32(0);
        assert!(after < initial, "AdamW should decrease param: {initial} \u{2192} {after}");
    }

    #[test]
    fn test_adamw_zero_gradient_weight_decay() {
        let mut opt = AdamW::new(0.001, 0.9, 0.999, 1e-8, 0.01);
        let mut param = Tensor::from_slice_f32(&[2.0f32; 4]);
        let grad  = Tensor::from_slice_f32(&[0.0f32; 4]);
        for _ in 0..100 {
            opt.step(&mut param, &grad);
        }
        for i in 0..4 {
            let v = param.get_f32(i);
            assert!(v >= 0.0 && v < 2.0, "weight decay should shrink param, got {v}");
        }
    }

    #[test]
    fn test_adamw_reset() {
        let mut opt = AdamW::default_lr(0.01);
        let mut param = Tensor::from_slice_f32(&[1.0f32; 2]);
        let grad  = Tensor::from_slice_f32(&[0.5f32; 2]);
        opt.step(&mut param, &grad);
        opt.reset();
        assert_eq!(opt.t, 0);
        assert!(opt.m.is_empty());
        assert!(opt.v.is_empty());
    }

    #[test]
    fn test_adamw_bias_correction_bc1_prod() {
        // After 1 step with beta1=0.9: bc1_prod should be 0.9
        let mut opt = AdamW::new(0.01, 0.9, 0.999, 1e-8, 0.0);
        let mut param = Tensor::from_slice_f32(&[1.0f32]);
        let grad  = Tensor::from_slice_f32(&[1.0f32]);
        opt.step(&mut param, &grad);
        assert!((opt.bc1_prod - 0.9).abs() < 1e-6, "bc1_prod after 1 step = {}", opt.bc1_prod);
        opt.step(&mut param, &grad);
        assert!((opt.bc1_prod - 0.81).abs() < 1e-6, "bc1_prod after 2 steps = {}", opt.bc1_prod);
    }

    // ── SGD ──────────────────────────────────────────────────────────────────

    #[test]
    fn test_sgd_vanilla() {
        // p=1.0, g=0.5, lr=0.1, mu=0, wd=0 \u2192 p -= 0.1*0.5 = 0.95
        let mut opt = Sgd::new(0.1, 0.0, 0.0);
        let mut param = Tensor::from_slice_f32(&[1.0f32]);
        let grad  = Tensor::from_slice_f32(&[0.5f32]);
        opt.step(&mut param, &grad);
        assert!((param.get_f32(0) - 0.95).abs() < 1e-5);
    }

    #[test]
    fn test_sgd_momentum_accelerates() {
        let mut opt = Sgd::new(0.1, 0.9, 0.0);
        let mut param = Tensor::from_slice_f32(&[1.0f32]);
        let grad  = Tensor::from_slice_f32(&[1.0f32]);
        opt.step(&mut param, &grad);
        let delta1 = 1.0 - param.get_f32(0);   // step 1
        opt.step(&mut param, &grad);
        let p2 = param.get_f32(0);
        let delta2 = p2 + delta1 - 1.0;        // step 2 delta = param_after_1 - param_after_2
        assert!(delta2 > delta1, "momentum should accelerate: d1={delta1}, d2={delta2}");
    }

    #[test]
    fn test_sgd_weight_decay() {
        // Weight decay adds wd*p to gradient, so decays param even with g=0
        let mut opt = Sgd::new(0.1, 0.0, 0.1);
        let mut param = Tensor::from_slice_f32(&[2.0f32]);
        let grad  = Tensor::from_slice_f32(&[0.0f32]);
        for _ in 0..10 {
            opt.step(&mut param, &grad);
        }
        assert!(param.get_f32(0) < 2.0, "weight decay should shrink param");
    }
}
