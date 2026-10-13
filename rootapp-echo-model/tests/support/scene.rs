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

//! A deterministic 16 kHz scene in AEC3's int16-scaled floats: a voiced far
//! end with syllables, its echo (40 samples late, 0.3, soft-clipped) as AEC3's
//! linear output would leave it, and a second voice at the near end from the
//! second half on. Also what generated `tests/data/golden-x2.f32` with the
//! research runtime.

pub const RATE: f32 = 16_000.0;

pub struct Lcg(u64);

impl Lcg {
    pub fn new(seed: u64) -> Lcg {
        Lcg(seed)
    }

    /// Uniform in -1..1.
    pub fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

fn voice(t: f32, f0_base: f32, f0_swing: f32, rate_hz: f32, phase: &mut f32) -> f32 {
    let f0 = f0_base + f0_swing * (2.0 * std::f32::consts::PI * 0.7 * t).sin();
    *phase += 2.0 * std::f32::consts::PI * f0 / RATE;
    let mut s = 0.0;
    for h in 1..12 {
        let formant = 1.0 / (1.0 + ((h as f32 * f0 - 700.0) / 400.0).powi(2))
            + 0.6 / (1.0 + ((h as f32 * f0 - 1800.0) / 500.0).powi(2));
        s += formant * (h as f32 * *phase).sin() / h as f32;
    }
    let envelope = (0.5 + 0.5 * (2.0 * std::f32::consts::PI * rate_hz * t).sin()).powi(2);
    s * envelope
}

/// `samples` samples of (render, linear_out).
pub fn scene(samples: usize) -> (Vec<f32>, Vec<f32>) {
    let mut noise = Lcg::new(7);
    let (mut far_phase, mut near_phase) = (0.0f32, 0.0f32);
    let mut far = Vec::with_capacity(samples);
    for i in 0..samples {
        let t = i as f32 / RATE;
        far.push(8000.0 * voice(t, 140.0, 30.0, 4.0, &mut far_phase) + 30.0 * noise.next());
    }
    let mut linear = Vec::with_capacity(samples);
    for i in 0..samples {
        let t = i as f32 / RATE;
        let echo = if i >= 40 { 0.3 * far[i - 40] } else { 0.0 };
        let echo = 12000.0 * (echo / 12000.0).tanh();
        let near = if i >= samples / 2 {
            6000.0 * voice(t, 220.0, 40.0, 3.0, &mut near_phase)
        } else {
            0.0
        };
        linear.push(0.5 * echo + near + 20.0 * noise.next());
    }
    (far, linear)
}
