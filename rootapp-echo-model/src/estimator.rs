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

//! AEC3's neural residual echo estimator slot, fed one 4 ms block (64 samples
//! of band 0 at 16 kHz) at a time. Every second block runs the full model on
//! AEC3's linear output and the render, and the per-bin gain it applied
//! becomes the residual fraction `min(1, scale * (1 - gain))` (the research's
//! `aec3+nree128-full-x2` with the default tuning).

use std::sync::Arc;

use crate::dtln::{Dtln, GainTracker, GAIN_BINS, HOP};
use crate::Model;

/// Samples in one AEC3 block.
pub const AEC3_BLOCK: usize = 64;
/// Bins of AEC3's band-0 spectra.
pub const AEC3_BINS: usize = GAIN_BINS;

/// Full scale of AEC3's int16-scaled float samples.
const INT16_SCALE: f32 = 32768.0;

/// The estimator's tunables.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tuning {
    /// How much of the model's removed power counts as residual echo (2 in
    /// the research's best variant).
    pub scale: f32,
    /// Render gate: with a value above 0, the estimate is 0 once the render
    /// has stayed under `render_gate_dbfs` for this many blocks (4 ms each).
    /// 0 turns the gate off (the baseline).
    pub render_gate_blocks: u32,
    /// The render level (mean square of a block, dBFS) the gate counts as
    /// activity.
    pub render_gate_dbfs: f32,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning { scale: 2.0, render_gate_blocks: 0, render_gate_dbfs: -70.0 }
    }
}

pub struct Estimator {
    dtln: Dtln,
    gain: GainTracker,
    tuning: Tuning,
    render_gate_power: f32,
    e_acc: [f32; HOP],
    r_acc: [f32; HOP],
    filled: usize,
    fraction: [f32; AEC3_BINS],
    render_hold: u32,
    model_resets: u64,
}

impl Estimator {
    /// Allocates the stream's state; [`Estimator::estimate_block`] and
    /// [`Estimator::reset`] never allocate.
    pub fn new(model: Arc<Model>, tuning: Tuning) -> Estimator {
        let scale = INT16_SCALE * INT16_SCALE;
        let render_gate_power = scale * 10f32.powf(tuning.render_gate_dbfs / 10.0);
        let mut estimator = Estimator {
            dtln: Dtln::new(model),
            gain: GainTracker::new(),
            tuning,
            render_gate_power,
            e_acc: [0.0; HOP],
            r_acc: [0.0; HOP],
            filled: 0,
            fraction: [1.0; AEC3_BINS],
            render_hold: 0,
            model_resets: 0,
        };
        estimator.reset();
        estimator
    }

    pub fn tuning(&self) -> Tuning {
        self.tuning
    }

    /// Times the model's state went non-finite and was reset.
    pub fn model_resets(&self) -> u64 {
        self.model_resets
    }

    pub fn reset(&mut self) {
        self.dtln.reset();
        self.gain.reset();
        self.e_acc = [0.0; HOP];
        self.r_acc = [0.0; HOP];
        self.filled = 0;
        self.fraction = [1.0; AEC3_BINS];
        self.render_hold = 0;
    }

    /// One AEC3 block: `render` and `linear_out` are band 0 (64 samples at
    /// 16 kHz, int16-scaled), each averaged over its channels. Writes the
    /// residual fraction of every bin (`R2 = E2 * fraction`) into the first
    /// 65 values of `fraction`. Returns whether this block ran an inference.
    /// Shorter inputs are padded with zeros; non-finite samples count as 0.
    pub fn estimate_block(
        &mut self,
        render: &[f32],
        linear_out: &[f32],
        fraction: &mut [f32],
    ) -> bool {
        let start = self.filled;
        let mut render_power = 0.0f32;
        for i in 0..AEC3_BLOCK {
            let r = finite(render.get(i).copied().unwrap_or(0.0));
            let e = finite(linear_out.get(i).copied().unwrap_or(0.0));
            render_power += r * r;
            if let (Some(rs), Some(es)) =
                (self.r_acc.get_mut(start + i), self.e_acc.get_mut(start + i))
            {
                *rs = r / INT16_SCALE;
                *es = e / INT16_SCALE;
            }
        }
        if render_power / AEC3_BLOCK as f32 > self.render_gate_power {
            self.render_hold = self.tuning.render_gate_blocks;
        } else {
            self.render_hold = self.render_hold.saturating_sub(1);
        }
        self.filled += AEC3_BLOCK;
        let inferred = self.filled >= HOP;
        if inferred {
            self.filled = 0;
            self.gain.hop(&mut self.dtln, &self.e_acc, &self.r_acc);
            let gain2 = self.gain.gain2();
            if gain2.iter().all(|g| g.is_finite()) {
                for (f, &g) in self.fraction.iter_mut().zip(gain2.iter()) {
                    *f = (self.tuning.scale * (1.0 - g)).clamp(0.0, 1.0);
                }
            } else {
                self.model_resets += 1;
                self.dtln.reset();
                self.gain.reset();
                self.fraction = [1.0; AEC3_BINS];
            }
        }
        let gated = self.tuning.render_gate_blocks > 0 && self.render_hold == 0;
        for (out, &f) in fraction.iter_mut().zip(self.fraction.iter()) {
            *out = if gated { 0.0 } else { f };
        }
        inferred
    }
}

fn finite(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}
