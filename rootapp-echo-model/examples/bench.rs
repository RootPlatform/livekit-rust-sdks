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

//! Wall time of one inference (every second 4 ms block) per kernel, on one
//! thread: `cargo run --release -p rootapp-echo-model --example bench`.

#![allow(clippy::unwrap_used, clippy::disallowed_macros)]

#[path = "../tests/support/scene.rs"]
mod scene;

use std::sync::Arc;
use std::time::Instant;

use rootapp_echo_model::{Estimator, Kernel, Model, Tuning, AEC3_BINS, AEC3_BLOCK};

fn main() {
    let blocks = 6000;
    let (render, linear) = scene::scene(blocks * AEC3_BLOCK);
    println!("| kernel | mean us | median us | p99 us | max us | % of a core |");
    println!("|---|---|---|---|---|---|");
    for kernel in [Kernel::Scalar, Kernel::Sse2, Kernel::Avx2Fma, Kernel::Neon] {
        let Ok(model) = Model::embedded().unwrap().with_kernel(kernel) else { continue };
        let mut estimator = Estimator::new(Arc::new(model), Tuning::default());
        let mut fraction = [0.0f32; AEC3_BINS];
        let mut times = Vec::new();
        for b in 0..blocks {
            let range = b * AEC3_BLOCK..(b + 1) * AEC3_BLOCK;
            let t = Instant::now();
            let inferred =
                estimator.estimate_block(&render[range.clone()], &linear[range], &mut fraction);
            if inferred && b > 200 {
                times.push(t.elapsed().as_secs_f64() * 1e6);
            }
        }
        times.sort_by(f64::total_cmp);
        let mean = times.iter().sum::<f64>() / times.len() as f64;
        let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
        println!(
            "| {kernel:?} | {mean:.0} | {:.0} | {:.0} | {:.0} | {:.2} |",
            at(0.5),
            at(0.99),
            at(1.0),
            mean / 8000.0 * 100.0
        );
    }
}
