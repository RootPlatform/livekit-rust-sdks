// Copyright 2026 LiveKit, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Dot products for the fully connected layers, picked once per model:
//! AVX2 with FMA when the CPU has both (x86-64), SSE2 otherwise, NEON on
//! aarch64, and a portable fallback.

/// The dot product kernel a model runs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kernel {
    Scalar,
    Sse2,
    Avx2Fma,
    Neon,
}

impl Kernel {
    /// The fastest kernel this CPU runs. AVX2 needs FMA too: some CPUs and
    /// hypervisors expose one without the other.
    pub fn detect() -> Kernel {
        #[cfg(target_arch = "x86_64")]
        {
            if std::arch::is_x86_feature_detected!("avx2")
                && std::arch::is_x86_feature_detected!("fma")
            {
                return Kernel::Avx2Fma;
            }
            Kernel::Sse2
        }
        #[cfg(target_arch = "aarch64")]
        {
            Kernel::Neon
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Kernel::Scalar
        }
    }

    /// Whether this CPU can run the kernel.
    pub fn available(self) -> bool {
        match self {
            Kernel::Scalar => true,
            #[cfg(target_arch = "x86_64")]
            Kernel::Sse2 => true,
            #[cfg(target_arch = "x86_64")]
            Kernel::Avx2Fma => {
                std::arch::is_x86_feature_detected!("avx2")
                    && std::arch::is_x86_feature_detected!("fma")
            }
            #[cfg(target_arch = "aarch64")]
            Kernel::Neon => true,
            #[allow(unreachable_patterns)]
            _ => false,
        }
    }

    /// The dot product of the common length of `a` and `b`. A kernel the CPU
    /// cannot run (only constructible through [`crate::Model::with_kernel`],
    /// which refuses it) computes with the portable one.
    #[inline]
    pub fn dot(self, a: &[f32], b: &[f32]) -> f32 {
        let n = a.len().min(b.len());
        let (a, b) = (&a[..n], &b[..n]);
        match self {
            #[cfg(target_arch = "x86_64")]
            // SAFETY: SSE2 is part of the x86-64 baseline.
            Kernel::Sse2 => unsafe { x86::dot_sse2(a, b) },
            #[cfg(target_arch = "x86_64")]
            // SAFETY: a model holds Avx2Fma only after detect()/available() saw
            // both AVX2 and FMA.
            Kernel::Avx2Fma => unsafe { x86::dot_avx2_fma(a, b) },
            #[cfg(target_arch = "aarch64")]
            // SAFETY: NEON is part of the aarch64 baseline.
            Kernel::Neon => unsafe { arm::dot_neon(a, b) },
            _ => dot_scalar(a, b),
        }
    }
}

fn dot_scalar(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0.0f32; 16];
    let mut chunks_a = a.chunks_exact(16);
    let mut chunks_b = b.chunks_exact(16);
    for (ca, cb) in (&mut chunks_a).zip(&mut chunks_b) {
        for ((s, &x), &y) in acc.iter_mut().zip(ca).zip(cb) {
            *s += x * y;
        }
    }
    let tail: f32 = chunks_a.remainder().iter().zip(chunks_b.remainder()).map(|(x, y)| x * y).sum();
    tail + acc.iter().sum::<f32>()
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use std::arch::x86_64::*;

    /// # Safety
    /// `a` and `b` have the same length; SSE2 is always present on x86-64.
    #[target_feature(enable = "sse2")]
    pub(super) unsafe fn dot_sse2(a: &[f32], b: &[f32]) -> f32 {
        let n = a.len();
        let (pa, pb) = (a.as_ptr(), b.as_ptr());
        let mut acc = [_mm_setzero_ps(); 4];
        let mut i = 0;
        while i + 16 <= n {
            for (k, s) in acc.iter_mut().enumerate() {
                let x = _mm_loadu_ps(pa.add(i + 4 * k));
                let y = _mm_loadu_ps(pb.add(i + 4 * k));
                *s = _mm_add_ps(*s, _mm_mul_ps(x, y));
            }
            i += 16;
        }
        while i + 4 <= n {
            acc[0] =
                _mm_add_ps(acc[0], _mm_mul_ps(_mm_loadu_ps(pa.add(i)), _mm_loadu_ps(pb.add(i))));
            i += 4;
        }
        let s = _mm_add_ps(_mm_add_ps(acc[0], acc[1]), _mm_add_ps(acc[2], acc[3]));
        let s = _mm_add_ps(s, _mm_movehl_ps(s, s));
        let s = _mm_add_ss(s, _mm_shuffle_ps::<1>(s, s));
        let mut sum = _mm_cvtss_f32(s);
        while i < n {
            sum += *pa.add(i) * *pb.add(i);
            i += 1;
        }
        sum
    }

    /// # Safety
    /// `a` and `b` have the same length, and the CPU has AVX2 and FMA.
    #[target_feature(enable = "avx2,fma")]
    pub(super) unsafe fn dot_avx2_fma(a: &[f32], b: &[f32]) -> f32 {
        let n = a.len();
        let (pa, pb) = (a.as_ptr(), b.as_ptr());
        let mut acc = [_mm256_setzero_ps(); 4];
        let mut i = 0;
        while i + 32 <= n {
            for (k, s) in acc.iter_mut().enumerate() {
                let x = _mm256_loadu_ps(pa.add(i + 8 * k));
                let y = _mm256_loadu_ps(pb.add(i + 8 * k));
                *s = _mm256_fmadd_ps(x, y, *s);
            }
            i += 32;
        }
        while i + 8 <= n {
            acc[0] =
                _mm256_fmadd_ps(_mm256_loadu_ps(pa.add(i)), _mm256_loadu_ps(pb.add(i)), acc[0]);
            i += 8;
        }
        let v = _mm256_add_ps(_mm256_add_ps(acc[0], acc[1]), _mm256_add_ps(acc[2], acc[3]));
        let s = _mm_add_ps(_mm256_castps256_ps128(v), _mm256_extractf128_ps::<1>(v));
        let s = _mm_add_ps(s, _mm_movehl_ps(s, s));
        let s = _mm_add_ss(s, _mm_shuffle_ps::<1>(s, s));
        let mut sum = _mm_cvtss_f32(s);
        while i < n {
            sum += *pa.add(i) * *pb.add(i);
            i += 1;
        }
        sum
    }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use std::arch::aarch64::*;

    /// # Safety
    /// `a` and `b` have the same length; NEON is always present on aarch64.
    #[target_feature(enable = "neon")]
    pub(super) unsafe fn dot_neon(a: &[f32], b: &[f32]) -> f32 {
        let n = a.len();
        let (pa, pb) = (a.as_ptr(), b.as_ptr());
        let mut acc = [vdupq_n_f32(0.0); 4];
        let mut i = 0;
        while i + 16 <= n {
            for (k, s) in acc.iter_mut().enumerate() {
                *s = vfmaq_f32(*s, vld1q_f32(pa.add(i + 4 * k)), vld1q_f32(pb.add(i + 4 * k)));
            }
            i += 16;
        }
        while i + 4 <= n {
            acc[0] = vfmaq_f32(acc[0], vld1q_f32(pa.add(i)), vld1q_f32(pb.add(i)));
            i += 4;
        }
        let v = vaddq_f32(vaddq_f32(acc[0], acc[1]), vaddq_f32(acc[2], acc[3]));
        let mut sum = vaddvq_f32(v);
        while i < n {
            sum += *pa.add(i) * *pb.add(i);
            i += 1;
        }
        sum
    }
}
