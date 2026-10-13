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

//! AEC3's neural residual echo estimator slot (estimator-spec.md), fed one
//! 4 ms block (64 samples of band 0 at 16 kHz, int16-scaled) at a time:
//!
//! ```text
//! model[k] = far end active ? E2[k] * min(1, 1.5 * (1 - G2[k])) : 0
//! floor[k] = S2[k] / ERLE[k] / today's enr_suppress[k]
//!            max Y2[k] in a block whose capture peaks >= 32000 while y - e
//!              peaks > 20000 (saturated echo)
//!            max E2[k] for 5 blocks after the linear output carried 1.5x
//!              the mic power (diverged filter), and for the first blocks
//!              after the far end turns active (onset)
//! R2[k] = R2_unbounded[k] = max(model[k], floor[k])
//! ```
//!
//! G2 is DTLN-aec-128's per-bin power gain ([`ModelFraction`], every second
//! block); ERLE is tracked like AEC3's subband ERLE ([`Rule`]). The far-end
//! flag and the onset guard's length come from the caller (the APM's reverse
//! stream, not the block AEC3 hands the hook). Same arithmetic, in the same
//! order, as the research's reference (`echo_model_estimator.h`).

use std::sync::Arc;

use crate::dtln::{Dtln, GainTracker, GAIN_BINS, HOP};
use crate::Model;

/// Samples in one AEC3 block.
pub const AEC3_BLOCK: usize = 64;
/// Bins of AEC3's band-0 spectra.
pub const AEC3_BINS: usize = GAIN_BINS;
/// How much of the model's removed power counts as residual echo.
pub const SCALE: f32 = 1.5;

const INT16_SCALE: f32 = 32768.0;
const DIVERGE_RATIO: f32 = 1.5;
const DIVERGE_HOLD_BLOCKS: i32 = 4;
const ECHO_SATURATION_LEVEL: f32 = 20000.0;
const SATURATION_LEVEL: f32 = 32000.0;
const LEVEL_GATE: f32 = 64.0 * 64.0 * 30.0 * 30.0;
const X2_BAND_ENERGY_THRESHOLD: f32 = 44015068.0;
const DFT: usize = 2 * AEC3_BLOCK;

/// What AEC3's hook passes, for one block. `render`, `y` and `e` are channel
/// 0 (shorter slices are padded with zeros); `s2`, `y2` and `e2` hold 65 bins
/// per capture channel, one channel after the other.
pub struct Hook<'a> {
    pub render: &'a [f32],
    pub y: &'a [f32],
    pub e: &'a [f32],
    pub s2: &'a [f32],
    pub y2: &'a [f32],
    pub e2: &'a [f32],
    pub dominant_nearend: bool,
    pub far_end_active: bool,
    /// Blocks of full suppression after the far end turns active:
    /// max(60 ms, render-to-capture delay + 40 ms) in 4 ms blocks, rounded up.
    pub onset_guard_blocks: u32,
}

/// The suppressor's `enr_suppress` per state today (AEC3's config before
/// AdjustConfig): low band up to `last_lf_band`, high band from
/// `first_hf_band`, linear in between.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TodayMasks {
    pub normal_lf: f32,
    pub normal_hf: f32,
    pub nearend_lf: f32,
    pub nearend_hf: f32,
    pub last_lf_band: usize,
    pub first_hf_band: usize,
}

impl Default for TodayMasks {
    /// AEC3's defaults.
    fn default() -> Self {
        TodayMasks {
            normal_lf: 0.4,
            normal_hf: 0.1,
            nearend_lf: 1.1,
            nearend_hf: 0.3,
            last_lf_band: 5,
            first_hf_band: 8,
        }
    }
}

/// The model half: every second block runs DTLN-aec on the last two blocks
/// of AEC3's linear output (mic) and render (loopback), and the per-bin gain
/// it applied becomes `fraction = min(1, scale * (1 - G2))`, held until the
/// next hop. A gain that is not finite resets the model (fraction 1).
pub struct ModelFraction {
    dtln: Dtln,
    gain: GainTracker,
    scale: f32,
    e_acc: [f32; HOP],
    r_acc: [f32; HOP],
    filled: usize,
    fraction: [f32; AEC3_BINS],
    model_resets: u64,
}

impl ModelFraction {
    pub fn new(model: Arc<Model>, scale: f32) -> ModelFraction {
        let mut m = ModelFraction {
            dtln: Dtln::new(model),
            gain: GainTracker::new(),
            scale,
            e_acc: [0.0; HOP],
            r_acc: [0.0; HOP],
            filled: 0,
            fraction: [1.0; AEC3_BINS],
            model_resets: 0,
        };
        m.reset();
        m
    }

    pub fn reset(&mut self) {
        self.dtln.reset();
        self.gain.reset();
        self.e_acc = [0.0; HOP];
        self.r_acc = [0.0; HOP];
        self.filled = 0;
        self.fraction = [1.0; AEC3_BINS];
    }

    /// One block (int16-scaled, padded with zeros, non-finite samples as 0);
    /// true when it ran the model.
    pub fn block(&mut self, render: &[f32], linear_out: &[f32]) -> bool {
        let start = self.filled;
        for i in 0..AEC3_BLOCK {
            let r = finite(render.get(i).copied().unwrap_or(0.0));
            let e = finite(linear_out.get(i).copied().unwrap_or(0.0));
            if let (Some(rs), Some(es)) =
                (self.r_acc.get_mut(start + i), self.e_acc.get_mut(start + i))
            {
                *rs = r * (1.0 / INT16_SCALE);
                *es = e * (1.0 / INT16_SCALE);
            }
        }
        self.filled += AEC3_BLOCK;
        if self.filled < HOP {
            return false;
        }
        self.filled = 0;
        self.gain.hop(&mut self.dtln, &self.e_acc, &self.r_acc);
        let gain2 = self.gain.gain2();
        if gain2.iter().all(|g| g.is_finite()) {
            for (f, &g) in self.fraction.iter_mut().zip(gain2.iter()) {
                *f = ((1.0 - g) * self.scale).min(1.0);
            }
        } else {
            self.model_resets += 1;
            self.dtln.reset();
            self.gain.reset();
            self.fraction = [1.0; AEC3_BINS];
        }
        true
    }

    pub fn fraction(&self) -> &[f32; AEC3_BINS] {
        &self.fraction
    }

    /// Times the model's state went non-finite and was reset.
    pub fn model_resets(&self) -> u64 {
        self.model_resets
    }
}

/// Steps 2-8 of the spec: the render spectrum, AEC3's subband ERLE, the
/// guards, and the per-bin maximum of the model's estimate and the floor.
/// Fixed-size state; [`Rule::apply`] never allocates.
pub struct Rule {
    window: [f32; DFT],
    cos: Box<[[f32; DFT]; AEC3_BINS]>,
    sin: Box<[[f32; DFT]; AEC3_BINS]>,
    max_erle: [f32; AEC3_BINS],
    floor_scale_normal: [f32; AEC3_BINS],
    floor_scale_nearend: [f32; AEC3_BINS],
    prev_render: [f32; AEC3_BLOCK],
    x2: [f32; AEC3_BINS],
    erle: [f32; AEC3_BINS],
    erle_oc: [f32; AEC3_BINS],
    hold: [i32; AEC3_BINS],
    acc_y: [f32; AEC3_BINS],
    acc_e: [f32; AEC3_BINS],
    low_render: [bool; AEC3_BINS],
    acc_n: i32,
    diverge_hold: i32,
    was_active: bool,
    onset_hold: u32,
}

impl Default for Rule {
    fn default() -> Self {
        Self::new()
    }
}

impl Rule {
    pub fn new() -> Rule {
        let mut window = [0.0f32; DFT];
        for (n, w) in window.iter_mut().enumerate() {
            *w = (0.5f32 - 0.5f32 * (2.0f32 * std::f32::consts::PI * n as f32 / 128.0f32).cos())
                .sqrt();
        }
        let mut cos = Box::new([[0.0f32; DFT]; AEC3_BINS]);
        let mut sin = Box::new([[0.0f32; DFT]; AEC3_BINS]);
        let mut max_erle = [0.0f32; AEC3_BINS];
        for k in 0..AEC3_BINS {
            for n in 0..DFT {
                let a = -2.0f32 * std::f32::consts::PI * ((k * n % 128) as f32) / 128.0f32;
                cos[k][n] = a.cos();
                sin[k][n] = a.sin();
            }
            max_erle[k] = if k < 32 { 4.0 } else { 1.5 };
        }
        let mut rule = Rule {
            window,
            cos,
            sin,
            max_erle,
            floor_scale_normal: [1.0; AEC3_BINS],
            floor_scale_nearend: [1.0; AEC3_BINS],
            prev_render: [0.0; AEC3_BLOCK],
            x2: [0.0; AEC3_BINS],
            erle: [1.0; AEC3_BINS],
            erle_oc: [1.0; AEC3_BINS],
            hold: [0; AEC3_BINS],
            acc_y: [0.0; AEC3_BINS],
            acc_e: [0.0; AEC3_BINS],
            low_render: [false; AEC3_BINS],
            acc_n: 0,
            diverge_hold: 0,
            was_active: false,
            onset_hold: 0,
        };
        rule.set_today_masks(&TodayMasks::default());
        rule.reset();
        rule
    }

    /// The floor's scale per bin and state: 1 / today's enr_suppress, so
    /// AdjustConfig's rule (full suppression at residual = nearend)
    /// suppresses at least where today's would.
    pub fn set_today_masks(&mut self, masks: &TodayMasks) {
        let (lf, hf) = (masks.last_lf_band, masks.first_hf_band);
        for k in 0..AEC3_BINS {
            let a = if k <= lf {
                0.0f32
            } else if k >= hf {
                1.0f32
            } else {
                (k - lf) as f32 / (hf - lf) as f32
            };
            let normal = masks.normal_lf + a * (masks.normal_hf - masks.normal_lf);
            let nearend = masks.nearend_lf + a * (masks.nearend_hf - masks.nearend_lf);
            self.floor_scale_normal[k] = inverse(normal);
            self.floor_scale_nearend[k] = inverse(nearend);
        }
    }

    pub fn reset(&mut self) {
        self.prev_render = [0.0; AEC3_BLOCK];
        self.x2 = [0.0; AEC3_BINS];
        self.erle = [1.0; AEC3_BINS];
        self.erle_oc = [1.0; AEC3_BINS];
        self.hold = [0; AEC3_BINS];
        self.acc_y = [0.0; AEC3_BINS];
        self.acc_e = [0.0; AEC3_BINS];
        self.low_render = [false; AEC3_BINS];
        self.acc_n = 0;
        self.diverge_hold = 0;
        self.was_active = false;
        self.onset_hold = 0;
    }

    /// One block with the model's current `fraction`: writes R2 for every
    /// capture channel that `r2` and the spectra hold (65 bins each).
    pub fn apply(&mut self, x: &Hook, fraction: &[f32; AEC3_BINS], r2: &mut [f32]) {
        let render = block64(x.render);
        let y = block64(x.y);
        let e = block64(x.e);
        let y2 = bins65(x.y2);
        let e2 = bins65(x.e2);
        let (mut y_peak, mut s_peak) = (0.0f32, 0.0f32);
        for i in 0..AEC3_BLOCK {
            y_peak = y_peak.max(y[i].abs());
            s_peak = s_peak.max((y[i] - e[i]).abs());
        }
        self.update_render_spectrum(&render);
        self.update_erle(&y2, &e2);
        let saturated_echo = y_peak >= SATURATION_LEVEL && s_peak > ECHO_SATURATION_LEVEL;

        let active = x.far_end_active;
        if active && !self.was_active {
            self.onset_hold = x.onset_guard_blocks;
        }
        self.was_active = active;
        let onset_guard = self.onset_hold > 0 && !x.dominant_nearend;
        if self.onset_hold > 0 {
            self.onset_hold -= 1;
        }

        let (mut sy, mut se) = (0.0f32, 0.0f32);
        for k in 0..AEC3_BINS {
            sy += y2[k];
            se += e2[k];
        }
        let diverged = active && !x.dominant_nearend && se > DIVERGE_RATIO * sy && sy > LEVEL_GATE;
        self.diverge_hold =
            if diverged { DIVERGE_HOLD_BLOCKS + 1 } else { (self.diverge_hold - 1).max(0) };

        let (erle, scale) = if x.dominant_nearend {
            (&self.erle, &self.floor_scale_nearend)
        } else {
            (&self.erle_oc, &self.floor_scale_normal)
        };
        let guard = self.diverge_hold > 0 || onset_guard;
        let channels = x.s2.chunks_exact(AEC3_BINS).zip(x.y2.chunks_exact(AEC3_BINS));
        let channels = channels.zip(x.e2.chunks_exact(AEC3_BINS));
        for (out, ((s2, y2), e2)) in r2.chunks_exact_mut(AEC3_BINS).zip(channels) {
            for k in 0..AEC3_BINS {
                let model = if active { e2[k] * fraction[k] } else { 0.0 };
                let mut floor = s2[k] / erle[k] * scale[k];
                if saturated_echo {
                    floor = floor.max(y2[k]);
                }
                if guard {
                    floor = floor.max(e2[k]);
                }
                out[k] = if floor > model { floor } else { model };
            }
        }
    }

    /// AEC3's render spectrum: sqrt-Hann over [previous block, current
    /// block], 128-point DFT.
    #[allow(clippy::needless_range_loop)]
    fn update_render_spectrum(&mut self, r: &[f32; AEC3_BLOCK]) {
        let mut v = [0.0f32; DFT];
        for n in 0..AEC3_BLOCK {
            v[n] = self.prev_render[n] * self.window[n];
            v[AEC3_BLOCK + n] = r[n] * self.window[AEC3_BLOCK + n];
            self.prev_render[n] = r[n];
        }
        for k in 0..AEC3_BINS {
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for n in 0..DFT {
                re += v[n] * self.cos[k][n];
                im += v[n] * self.sin[k][n];
            }
            self.x2[k] = re * re + im * im;
        }
    }

    /// AEC3's subband ERLE: 6-block accumulation while the linear filter
    /// works (sum E2 < 0.5 sum Y2), smoothed (0.05 up, 0.1 down, no decrease
    /// in a bin that had low render), bounded to [1, 4] below bin 32 and
    /// [1, 1.5] from it. The onset-compensated copy decays back to 1 (0.97
    /// per block) 400 ms after the bin last had render energy.
    fn update_erle(&mut self, y2: &[f32; AEC3_BINS], e2: &[f32; AEC3_BINS]) {
        let (mut sy, mut se) = (0.0f32, 0.0f32);
        for k in 0..AEC3_BINS {
            sy += y2[k];
            se += e2[k];
        }
        if se < 0.5f32 * sy && sy > LEVEL_GATE {
            if self.acc_n == 6 {
                self.acc_n = 0;
                self.acc_y = [0.0; AEC3_BINS];
                self.acc_e = [0.0; AEC3_BINS];
                self.low_render = [false; AEC3_BINS];
            }
            for k in 0..AEC3_BINS {
                self.acc_y[k] += y2[k];
                self.acc_e[k] += e2[k];
                self.low_render[k] = self.low_render[k] || self.x2[k] < X2_BAND_ENERGY_THRESHOLD;
            }
            self.acc_n += 1;
            if self.acc_n == 6 {
                for k in 1..AEC3_BINS - 1 {
                    if self.acc_e[k] <= 0.0 {
                        continue;
                    }
                    let fresh = self.acc_y[k] / self.acc_e[k];
                    if !self.low_render[k] {
                        self.hold[k] = 250;
                    }
                    let low = self.low_render[k];
                    let max = self.max_erle[k];
                    let update = |v: f32| -> f32 {
                        let mut alpha = 0.05f32;
                        if fresh < v {
                            alpha = if low { 0.0 } else { 0.1 };
                        }
                        (v + alpha * (fresh - v)).clamp(1.0, max)
                    };
                    self.erle[k] = update(self.erle[k]);
                    self.erle_oc[k] = update(self.erle_oc[k]);
                }
            }
        }
        for k in 1..AEC3_BINS - 1 {
            self.hold[k] -= 1;
            if self.hold[k] <= 150 {
                self.erle_oc[k] = (0.97f32 * self.erle_oc[k]).max(1.0);
                if self.hold[k] < 0 {
                    self.hold[k] = 0;
                }
            }
        }
        self.erle[0] = self.erle[1];
        self.erle[AEC3_BINS - 1] = self.erle[AEC3_BINS - 2];
        self.erle_oc[0] = self.erle_oc[1];
        self.erle_oc[AEC3_BINS - 1] = self.erle_oc[AEC3_BINS - 2];
    }
}

/// The spec's estimator: [`ModelFraction`] at [`SCALE`] and the [`Rule`].
/// [`Estimator::estimate`] and [`Estimator::reset`] never allocate.
pub struct Estimator {
    model: ModelFraction,
    rule: Rule,
}

impl Estimator {
    pub fn new(model: Arc<Model>) -> Estimator {
        Estimator { model: ModelFraction::new(model, SCALE), rule: Rule::new() }
    }

    pub fn set_today_masks(&mut self, masks: &TodayMasks) {
        self.rule.set_today_masks(masks);
    }

    pub fn reset(&mut self) {
        self.model.reset();
        self.rule.reset();
    }

    /// One block; writes R2 (65 bins per capture channel) and returns
    /// whether the model ran.
    pub fn estimate(&mut self, x: &Hook, r2: &mut [f32]) -> bool {
        let inferred = self.model.block(x.render, x.e);
        self.rule.apply(x, self.model.fraction(), r2);
        inferred
    }

    /// Times the model's state went non-finite and was reset.
    pub fn model_resets(&self) -> u64 {
        self.model.model_resets()
    }
}

fn finite(v: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

fn inverse(v: f32) -> f32 {
    if v > 0.0 {
        1.0 / v
    } else {
        1.0
    }
}

fn block64(x: &[f32]) -> [f32; AEC3_BLOCK] {
    let mut out = [0.0f32; AEC3_BLOCK];
    for (o, v) in out.iter_mut().zip(x) {
        *o = *v;
    }
    out
}

fn bins65(x: &[f32]) -> [f32; AEC3_BINS] {
    let mut out = [0.0f32; AEC3_BINS];
    for (o, v) in out.iter_mut().zip(x) {
        *o = *v;
    }
    out
}
