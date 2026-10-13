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

//! Streaming DTLN-aec (16 kHz, 512-sample window, 128-sample hop) as the
//! authors' `run_aec.py` runs it, and the per-bin gain the full model applied
//! to the block it just finished.

use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use crate::plan::PlanState;
use crate::Model;

pub const BLOCK: usize = 512;
pub const HOP: usize = 128;
pub const BINS: usize = BLOCK / 2 + 1;
/// Bins of the gain on a 128-sample block (AEC3's band-0 spectrum).
pub const GAIN_BINS: usize = HOP / 2 + 1;

/// One stream of the full model (both stages).
pub struct Dtln {
    model: Arc<Model>,
    s1: PlanState,
    s2: PlanState,
    in_mic: [f32; BLOCK],
    in_lpb: [f32; BLOCK],
    out_buf: [f32; BLOCK],
    fwd: Arc<dyn Fft<f32>>,
    inv: Arc<dyn Fft<f32>>,
    scratch: Vec<Complex32>,
    spec: Vec<Complex32>,
    spec_lpb: Vec<Complex32>,
    mask: [f32; BINS],
    estimate: [f32; BLOCK],
}

impl Dtln {
    pub fn new(model: Arc<Model>) -> Dtln {
        let mut planner = FftPlanner::<f32>::new();
        let fwd = planner.plan_fft_forward(BLOCK);
        let inv = planner.plan_fft_inverse(BLOCK);
        let scratch_len = fwd.get_inplace_scratch_len().max(inv.get_inplace_scratch_len());
        let s1 = model.stage1.new_state();
        let s2 = model.stage2.new_state();
        Dtln {
            model,
            s1,
            s2,
            in_mic: [0.0; BLOCK],
            in_lpb: [0.0; BLOCK],
            out_buf: [0.0; BLOCK],
            fwd,
            inv,
            scratch: vec![Complex32::new(0.0, 0.0); scratch_len],
            spec: vec![Complex32::new(0.0, 0.0); BLOCK],
            spec_lpb: vec![Complex32::new(0.0, 0.0); BLOCK],
            mask: [1.0; BINS],
            estimate: [0.0; BLOCK],
        }
    }

    /// Back to the state of a new stream: windows, LSTM states and every
    /// activation zeroed.
    pub fn reset(&mut self) {
        self.in_mic = [0.0; BLOCK];
        self.in_lpb = [0.0; BLOCK];
        self.out_buf = [0.0; BLOCK];
        self.mask = [1.0; BINS];
        self.estimate = [0.0; BLOCK];
        for buf in self.s1.bufs.iter_mut().chain(self.s2.bufs.iter_mut()) {
            buf.iter_mut().for_each(|v| *v = 0.0);
        }
    }

    /// The oldest hop of the input window: the samples `hop`'s output lines
    /// up with.
    pub fn oldest_input(&self) -> &[f32] {
        &self.in_mic[..HOP]
    }

    /// Both stages: `HOP` samples of mic and loopback in, `HOP` samples out
    /// (32 ms of algorithmic latency, the hop included).
    pub fn hop(&mut self, mic: &[f32; HOP], lpb: &[f32; HOP], out: &mut [f32; HOP]) {
        self.in_mic.copy_within(HOP.., 0);
        self.in_mic[BLOCK - HOP..].copy_from_slice(mic);
        self.in_lpb.copy_within(HOP.., 0);
        self.in_lpb[BLOCK - HOP..].copy_from_slice(lpb);

        for (c, &x) in self.spec.iter_mut().zip(self.in_mic.iter()) {
            *c = Complex32::new(x, 0.0);
        }
        self.fwd.process_with_scratch(&mut self.spec, &mut self.scratch);
        for (c, &x) in self.spec_lpb.iter_mut().zip(self.in_lpb.iter()) {
            *c = Complex32::new(x, 0.0);
        }
        self.fwd.process_with_scratch(&mut self.spec_lpb, &mut self.scratch);

        let model = &*self.model;
        let p1 = &model.stage1;
        let [mic_in, state_in, lpb_in] = model.stage1_inputs;
        for (k, (m, l)) in self.spec.iter().zip(self.spec_lpb.iter()).enumerate().take(BINS) {
            self.s1.bufs[mic_in][k] = m.norm();
            self.s1.bufs[lpb_in][k] = l.norm();
        }
        p1.run(&mut self.s1, model.kernel);
        let [mask_out, state_out] = model.stage1_outputs;
        copy_state(&mut self.s1.bufs, state_out, state_in);
        self.mask.copy_from_slice(&self.s1.bufs[mask_out][..BINS]);

        for (c, &m) in self.spec.iter_mut().zip(self.mask.iter()) {
            *c *= m;
        }
        self.spec[0].im = 0.0;
        self.spec[BLOCK / 2].im = 0.0;
        for k in 1..BLOCK / 2 {
            self.spec[BLOCK - k] = self.spec[k].conj();
        }
        self.inv.process_with_scratch(&mut self.spec, &mut self.scratch);
        let scale = 1.0 / BLOCK as f32;
        for (e, c) in self.estimate.iter_mut().zip(self.spec.iter()) {
            *e = c.re * scale;
        }

        let p2 = &model.stage2;
        let [est_in, state2_in, lpb2_in] = model.stage2_inputs;
        self.s2.bufs[est_in].copy_from_slice(&self.estimate);
        self.s2.bufs[lpb2_in].copy_from_slice(&self.in_lpb);
        p2.run(&mut self.s2, model.kernel);
        let [block_out, state2_out] = model.stage2_outputs;
        copy_state(&mut self.s2.bufs, state2_out, state2_in);
        self.out_buf.copy_within(HOP.., 0);
        self.out_buf[BLOCK - HOP..].iter_mut().for_each(|v| *v = 0.0);
        for (o, &v) in self.out_buf.iter_mut().zip(self.s2.bufs[block_out].iter()) {
            *o += v;
        }
        out.copy_from_slice(&self.out_buf[..HOP]);
    }
}

fn copy_state(bufs: &mut [Vec<f32>], from: usize, to: usize) {
    if from == to {
        return;
    }
    let (lo, hi) = (from.min(to), from.max(to));
    let (left, right) = bufs.split_at_mut(hi);
    let (low, high) = (&mut left[lo], &mut right[0]);
    if from < to {
        high.copy_from_slice(low);
    } else {
        low.copy_from_slice(high);
    }
}

/// Runs the full model and returns, per 65-bin band of a 128-point Hann
/// window, the power gain it applied to the input block it just finished
/// (384 samples, 24 ms, older than the newest input): output power over
/// input power, each summed over the bin and its neighbours, in 0..1.
pub struct GainTracker {
    fft: Arc<dyn Fft<f32>>,
    buf: Vec<Complex32>,
    scratch: Vec<Complex32>,
    win: [f32; HOP],
    gain2: [f32; GAIN_BINS],
}

impl GainTracker {
    pub fn new() -> GainTracker {
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(HOP);
        let scratch_len = fft.get_inplace_scratch_len();
        let mut win = [0.0f32; HOP];
        for (i, w) in win.iter_mut().enumerate() {
            *w = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / HOP as f32).cos();
        }
        GainTracker {
            fft,
            buf: vec![Complex32::new(0.0, 0.0); HOP],
            scratch: vec![Complex32::new(0.0, 0.0); scratch_len],
            win,
            gain2: [1.0; GAIN_BINS],
        }
    }

    pub fn reset(&mut self) {
        self.gain2 = [1.0; GAIN_BINS];
    }

    pub fn gain2(&self) -> &[f32; GAIN_BINS] {
        &self.gain2
    }

    fn power(&mut self, x: &[f32], out: &mut [f32; GAIN_BINS]) {
        for ((b, &v), &w) in self.buf.iter_mut().zip(x).zip(self.win.iter()) {
            *b = Complex32::new(v * w, 0.0);
        }
        self.fft.process_with_scratch(&mut self.buf, &mut self.scratch);
        for (o, b) in out.iter_mut().zip(self.buf.iter()) {
            *o = b.norm_sqr();
        }
    }

    pub fn hop(&mut self, dtln: &mut Dtln, mic: &[f32; HOP], lpb: &[f32; HOP]) {
        let mut out = [0.0f32; HOP];
        dtln.hop(mic, lpb, &mut out);
        let mut x = [0.0f32; HOP];
        x.copy_from_slice(dtln.oldest_input());
        let mut px = [0.0f32; GAIN_BINS];
        let mut po = [0.0f32; GAIN_BINS];
        self.power(&x, &mut px);
        self.power(&out, &mut po);
        for k in 0..GAIN_BINS {
            let lo = k.saturating_sub(1);
            let hi = (k + 1).min(GAIN_BINS - 1);
            let (mut sx, mut so) = (1e-12f32, 0.0f32);
            for j in lo..=hi {
                sx += px[j];
                so += po[j];
            }
            self.gain2[k] = (so / sx).clamp(0.0, 1.0);
        }
    }
}

impl Default for GainTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// The authors' offline processing (`run_aec.py`): 384 zeros of padding on
/// both ends, the latency trimmed away, the result scaled back under full
/// scale when it clips. For the reference test.
pub fn process_offline(dtln: &mut Dtln, mic: &[f32], lpb: &[f32]) -> Vec<f32> {
    let n = mic.len().min(lpb.len());
    let pad = BLOCK - HOP;
    let mut a = vec![0.0f32; pad];
    a.extend_from_slice(&mic[..n]);
    a.extend(std::iter::repeat_n(0.0, pad));
    let mut l = vec![0.0f32; pad];
    l.extend_from_slice(&lpb[..n]);
    l.extend(std::iter::repeat_n(0.0, pad));
    let blocks = (a.len() - pad) / HOP;
    let mut out = vec![0.0f32; a.len()];
    let mut o = [0.0f32; HOP];
    let mut m = [0.0f32; HOP];
    let mut p = [0.0f32; HOP];
    for i in 0..blocks {
        m.copy_from_slice(&a[i * HOP..(i + 1) * HOP]);
        p.copy_from_slice(&l[i * HOP..(i + 1) * HOP]);
        dtln.hop(&m, &p, &mut o);
        out[i * HOP..(i + 1) * HOP].copy_from_slice(&o);
    }
    let mut res = out[pad..pad + n].to_vec();
    let peak = res.iter().fold(0.0f32, |m, &v| m.max(v));
    if peak > 1.0 {
        res.iter_mut().for_each(|v| *v = *v / peak * 0.99);
    }
    res
}
