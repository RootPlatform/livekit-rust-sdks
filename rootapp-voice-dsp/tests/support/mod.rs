#![allow(dead_code)]

/// Deterministic uniform noise for tests (xorshift64*).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in [-1, 1).
    pub fn symmetric(&mut self) -> f32 {
        (self.next_f64() * 2.0 - 1.0) as f32
    }
}

pub fn near(actual: f64, expected: f64, tolerance: f64) -> bool {
    (actual - expected).abs() <= tolerance
}

pub fn in_range(value: f64, low: f64, high: f64) -> bool {
    value >= low && value <= high
}

pub fn silent(samples: &[f32]) -> bool {
    samples.iter().all(|&v| v == 0.0)
}

/// A sine of `amplitude` at `hz`, sample `n` of a stream at `rate` (f64 phase).
pub fn sine(amplitude: f32, hz: f64, n: usize, rate: u32) -> f32 {
    amplitude * (2.0 * std::f64::consts::PI * hz * n as f64 / f64::from(rate)).sin() as f32
}
