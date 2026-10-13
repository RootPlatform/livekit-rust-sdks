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

//! Wall time of the estimator per kernel, on one thread: the blocks that run
//! the model (every second 4 ms block) and the ones that only run the rule
//! (the spec's steps 2-8): `cargo run --release -p rootapp-echo-model
//! --example bench`.

#![allow(clippy::unwrap_used, clippy::disallowed_macros)]

#[path = "../tests/support/scene.rs"]
mod scene;

use std::sync::Arc;
use std::time::Instant;

use rootapp_echo_model::{Estimator, Hook, Kernel, Model, AEC3_BINS, AEC3_BLOCK};

fn stats(mut times: Vec<f64>) -> (f64, f64, f64, f64) {
    times.sort_by(f64::total_cmp);
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
    (mean, at(0.5), at(0.99), at(1.0))
}

fn main() {
    let blocks = 6000;
    let (render, linear) = scene::scene(blocks * AEC3_BLOCK);
    println!("| kernel | model block mean / median / p99 / max us | rule-only block mean / max us | per 10 ms us |");
    println!("|---|---|---|---|");
    for kernel in [Kernel::Scalar, Kernel::Sse2, Kernel::Avx2Fma, Kernel::Neon] {
        let Ok(model) = Model::embedded().unwrap().with_kernel(kernel) else { continue };
        let mut estimator = Estimator::new(Arc::new(model));
        let mut r2 = [0.0f32; AEC3_BINS];
        let mut y = [0.0f32; AEC3_BLOCK];
        let mut spectrum = [0.0f32; AEC3_BINS];
        let (mut model_times, mut rule_times) = (Vec::new(), Vec::new());
        for b in 0..blocks {
            let range = b * AEC3_BLOCK..(b + 1) * AEC3_BLOCK;
            for (i, v) in y.iter_mut().enumerate() {
                *v = linear[range.start + i] + 0.3 * render[range.start + i];
            }
            for (k, v) in spectrum.iter_mut().enumerate() {
                *v = 1e6 * (1.0 + ((b + k) % 5) as f32);
            }
            let hook = Hook {
                render: &render[range.clone()],
                y: &y,
                e: &linear[range],
                s2: &spectrum,
                y2: &spectrum,
                e2: &spectrum,
                dominant_nearend: false,
                far_end_active: true,
                onset_guard_blocks: 15,
            };
            let t = Instant::now();
            let inferred = estimator.estimate(&hook, &mut r2);
            let us = t.elapsed().as_secs_f64() * 1e6;
            if b > 200 {
                if inferred {
                    model_times.push(us)
                } else {
                    rule_times.push(us)
                }
            }
        }
        let (m_mean, m_median, m_p99, m_max) = stats(model_times);
        let (r_mean, _, _, r_max) = stats(rule_times);
        println!(
            "| {kernel:?} | {m_mean:.0} / {m_median:.0} / {m_p99:.0} / {m_max:.0} | {r_mean:.1} / {r_max:.1} | {:.0} |",
            (m_mean + r_mean) * 2.5 / 2.0
        );
    }
}
