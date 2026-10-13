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

//! The research's echo metrics (`echo/dtln/src/bin/eval.rs` `measure()` and
//! `dsp.rs`), for estimator-spec.md's 16 kHz scenes: 16 kHz signals in
//! -1..1, the output aligned to the mic, near and echo known.

use rustfft::num_complex::Complex32;
use rustfft::FftPlanner;

#[derive(Debug, Default, Clone, Copy)]
pub struct Metrics {
    /// Share of 50 ms windows of far-end-only echo (from 1 s on) more than
    /// 6 dB over the mic noise floor, and the loudest one (dB over it).
    pub leak_pct: f64,
    pub worst_db: f64,
    /// Output over mic level where neither talker is active (from 1 s on).
    pub idle_delta: f64,
    /// Output over mic level in double-talk time-frequency cells where the
    /// near voice is 10 dB above the echo.
    pub near_keep_dt: f64,
    pub stoi: f64,
    /// The near voice 1 s after the last non-silent render frame: spectral
    /// SNR against the clean near voice, and level change.
    pub neq_snr: f64,
    pub neq_level: f64,
}

pub fn db(p: f64) -> f64 {
    10.0 * p.max(1e-20).log10()
}

pub fn frame_power(x: &[f32], frame: usize) -> Vec<f64> {
    x.chunks(frame)
        .map(|c| c.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>() / c.len() as f64)
        .collect()
}

fn percentile(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn dilate(a: &[bool], r: usize) -> Vec<bool> {
    (0..a.len()).map(|i| (i.saturating_sub(r)..(i + r + 1).min(a.len())).any(|j| a[j])).collect()
}

/// 2nd-order Butterworth high-pass at 150 Hz (16 kHz), forward and backward.
#[allow(clippy::approx_constant)]
fn hpf150(x: &[f32]) -> Vec<f32> {
    let w = 2.0 * std::f64::consts::PI * 150.0 / 16000.0;
    let (s, c) = w.sin_cos();
    let alpha = s / (2.0 * 0.7071);
    let a0 = 1.0 + alpha;
    let b = [(1.0 + c) / 2.0 / a0, -(1.0 + c) / a0, (1.0 + c) / 2.0 / a0];
    let a = [-2.0 * c / a0, (1.0 - alpha) / a0];
    let run = |x: &[f64]| -> Vec<f64> {
        let mut z = [0.0f64; 2];
        x.iter()
            .map(|&i| {
                let o = b[0] * i + z[0];
                z[0] = b[1] * i - a[0] * o + z[1];
                z[1] = b[2] * i - a[1] * o;
                o
            })
            .collect()
    };
    let f = run(&x.iter().map(|&v| f64::from(v)).collect::<Vec<_>>());
    let r: Vec<f64> = run(&f.into_iter().rev().collect::<Vec<_>>());
    r.into_iter().rev().map(|v| v as f32).collect()
}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > 1e-12 * sum {
        term *= (x / (2.0 * k)) * (x / (2.0 * k));
        sum += term;
        k += 1.0;
    }
    sum
}

/// Windowed-sinc lowpass, cutoff in cycles per sample, Kaiser beta 9.
fn lowpass(taps: usize, cutoff: f64) -> Vec<f64> {
    let m = (taps - 1) as f64;
    let beta = 9.0;
    (0..taps)
        .map(|i| {
            let x = i as f64 - m / 2.0;
            let sinc = if x == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
            };
            let r = 2.0 * i as f64 / m - 1.0;
            sinc * bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / bessel_i0(beta)
        })
        .collect()
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// Rational resampling with a delay-compensated FIR.
pub fn resample(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to {
        return x.to_vec();
    }
    let g = gcd(from, to);
    let up = (to / g) as usize;
    let down = (from / g) as usize;
    let half = 24 * up.max(down);
    let taps = 2 * half + 1;
    let cutoff = 0.5 / up.max(down) as f64 * 0.94;
    let h: Vec<f64> = lowpass(taps, cutoff).into_iter().map(|v| v * up as f64).collect();
    let out_len = x.len() * up / down;
    let mut y = vec![0.0f32; out_len];
    for (n, yv) in y.iter_mut().enumerate() {
        let pos = n * down;
        let mut acc = 0.0f64;
        let k_min = pos.saturating_sub(half);
        let k_max = (pos + half).min(x.len() * up - 1);
        let mut k = k_min + (up - k_min % up) % up;
        while k <= k_max {
            acc += f64::from(x[k / up]) * h[k + half - pos];
            k += up;
        }
        *yv = acc as f32;
    }
    y
}

pub struct Stft {
    n: usize,
    hop: usize,
    frames: Vec<Vec<f32>>,
}

fn stft_power(x: &[f32], n: usize, hop: usize) -> Stft {
    let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
    let win: Vec<f32> = (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos())
        .collect();
    let mut frames = Vec::new();
    let mut buf = vec![Complex32::new(0.0, 0.0); n];
    let mut start = 0;
    while start + n <= x.len() {
        for i in 0..n {
            buf[i] = Complex32::new(x[start + i] * win[i], 0.0);
        }
        fft.process(&mut buf);
        frames.push(buf[..n / 2 + 1].iter().map(|c| c.norm_sqr()).collect());
        start += hop;
    }
    Stft { n, hop, frames }
}

/// Phase-insensitive SNR of the estimate's STFT magnitudes against the
/// reference's over the selected 10 ms frames, bins from 150 Hz up.
fn spectral_snr(reference: &Stft, estimate: &Stft, select: &[bool]) -> f64 {
    let (mut sig, mut err) = (0.0f64, 0.0f64);
    for k in 0..reference.frames.len().min(estimate.frames.len()) {
        let center = (k * reference.hop + reference.n / 2) / 160;
        if center >= select.len() || !select[center] {
            continue;
        }
        for b in 5..reference.frames[k].len() {
            let s = f64::from(reference.frames[k][b]).sqrt();
            let o = f64::from(estimate.frames[k][b]).sqrt();
            sig += s * s;
            err += (o - s) * (o - s);
        }
    }
    if sig <= 0.0 {
        return f64::NAN;
    }
    db(sig / err.max(1e-20))
}

/// Short-Time Objective Intelligibility (Taal et al. 2011) at 10 kHz.
fn stoi(clean: &[f32], processed: &[f32], rate: u32) -> f64 {
    let x = resample(clean, rate, 10000);
    let y = resample(processed, rate, 10000);
    let len = x.len().min(y.len());
    let (x, y) = (&x[..len], &y[..len]);
    let n = 256;
    let hop = 128;
    let win: Vec<f32> = (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * (i + 1) as f32 / (n + 1) as f32).cos())
        .collect();
    let mut energies = Vec::new();
    let mut start = 0;
    while start + n <= len {
        let e: f64 = (0..n).map(|i| f64::from(x[start + i] * win[i]).powi(2)).sum();
        energies.push(db(e));
        start += hop;
    }
    let max_e = energies.iter().copied().fold(f64::MIN, f64::max);
    let keep: Vec<usize> =
        energies.iter().enumerate().filter(|(_, &e)| e > max_e - 40.0).map(|(i, _)| i).collect();
    let mut xs = vec![0.0f32; (keep.len() + 1) * hop];
    let mut ys = vec![0.0f32; (keep.len() + 1) * hop];
    for (j, &f) in keep.iter().enumerate() {
        for i in 0..n {
            xs[j * hop + i] += x[f * hop + i] * win[i];
            ys[j * hop + i] += y[f * hop + i] * win[i];
        }
    }
    let nfft = 512;
    let fs = 10000.0f64;
    let num_bands = 15;
    let min_freq = 150.0f64;
    let freqs: Vec<f64> = (0..=nfft / 2).map(|k| k as f64 * fs / nfft as f64).collect();
    let nearest = |f: f64| -> usize {
        freqs
            .iter()
            .enumerate()
            .min_by(|a, b| (a.1 - f).abs().total_cmp(&(b.1 - f).abs()))
            .map_or(0, |(i, _)| i)
    };
    let bands: Vec<(usize, usize)> = (0..num_bands)
        .map(|b| {
            let cf = min_freq * 2f64.powf(b as f64 / 3.0);
            (nearest(cf * 2f64.powf(-1.0 / 6.0)), nearest(cf * 2f64.powf(1.0 / 6.0)))
        })
        .collect();
    let fft = FftPlanner::<f32>::new().plan_fft_forward(nfft);
    let tob = |s: &[f32]| -> Vec<Vec<f64>> {
        let mut out = Vec::new();
        let mut start = 0;
        let mut buf = vec![Complex32::new(0.0, 0.0); nfft];
        while start + n <= s.len() {
            for i in 0..nfft {
                buf[i] = Complex32::new(if i < n { s[start + i] * win[i] } else { 0.0 }, 0.0);
            }
            fft.process(&mut buf);
            out.push(
                bands
                    .iter()
                    .map(|&(lo, hi)| {
                        (lo..hi).map(|k| f64::from(buf[k].norm_sqr())).sum::<f64>().sqrt()
                    })
                    .collect(),
            );
            start += hop;
        }
        out
    };
    let xb = tob(&xs);
    let yb = tob(&ys);
    let frames = xb.len().min(yb.len());
    let seg = 30;
    if frames < seg {
        return f64::NAN;
    }
    let clip = 10f64.powf(15.0 / 20.0);
    let (mut total, mut count) = (0.0, 0);
    for m in seg..=frames {
        for b in 0..num_bands {
            let xv: Vec<f64> = (m - seg..m).map(|t| xb[t][b]).collect();
            let yv: Vec<f64> = (m - seg..m).map(|t| yb[t][b]).collect();
            let nx = xv.iter().map(|v| v * v).sum::<f64>().sqrt();
            let ny = yv.iter().map(|v| v * v).sum::<f64>().sqrt().max(1e-20);
            let alpha = nx / ny;
            let yc: Vec<f64> =
                yv.iter().zip(&xv).map(|(&y, &x)| (y * alpha).min(x * (1.0 + clip))).collect();
            let mx = xv.iter().sum::<f64>() / seg as f64;
            let my = yc.iter().sum::<f64>() / seg as f64;
            let (mut num, mut dx, mut dy) = (0.0, 0.0, 0.0);
            for t in 0..seg {
                num += (xv[t] - mx) * (yc[t] - my);
                dx += (xv[t] - mx).powi(2);
                dy += (yc[t] - my).powi(2);
            }
            total += num / (dx.sqrt() * dy.sqrt()).max(1e-20);
            count += 1;
        }
    }
    total / f64::from(count)
}

/// `measure()` for scenes with known near voice and echo (16 kHz, -1..1,
/// `out` aligned to `mic`).
pub fn measure(mic: &[f32], out: &[f32], near: &[f32], echo: &[f32], reference: &[f32]) -> Metrics {
    let f = 160;
    let n = mic.len().min(out.len()).min(near.len()).min(echo.len());
    let pm = frame_power(&mic[..n], f);
    let po = frame_power(&out[..n], f);
    let frames = pm.len().min(po.len());
    let mut floor_v: Vec<f64> = pm.iter().copied().filter(|&p| p > 1e-14).collect();
    let floor = percentile(&mut floor_v, 0.05).max(1e-13);
    let pn = frame_power(&near[..n], f);
    let pe = frame_power(&echo[..n], f);
    let mn = pn.iter().copied().fold(0.0, f64::max);
    let me = pe.iter().copied().fold(0.0, f64::max);
    let near_act: Vec<bool> =
        pn.iter().map(|&p| mn > 0.0 && p > mn * 1e-4 && p > floor * 3.0).collect();
    let echo_act: Vec<bool> =
        pe.iter().map(|&p| me > 0.0 && p > me * 1e-4 && p > floor * 3.0).collect();
    let near_wide = dilate(&near_act, 3);
    let echo_wide = dilate(&echo_act, 3);
    let skip = 100;
    let mut m = Metrics::default();
    let (mut leaks, mut wins, mut worst) = (0, 0, f64::MIN);
    let mut t = skip;
    while t + 5 <= frames {
        if (t..t + 5).all(|k| echo_act[k] && !near_wide[k]) {
            let p: f64 = (t..t + 5).map(|k| po[k]).sum::<f64>() / 5.0;
            wins += 1;
            if p > floor * 4.0 {
                leaks += 1;
            }
            worst = worst.max(db(p / floor));
        }
        t += 5;
    }
    m.leak_pct = if wins > 0 { 100.0 * f64::from(leaks) / f64::from(wins) } else { f64::NAN };
    m.worst_db = if wins > 0 { worst } else { f64::NAN };
    let idle: Vec<usize> = (skip..frames).filter(|&t| !near_wide[t] && !echo_wide[t]).collect();
    let (im, io) = idle.iter().fold((0.0, 0.0), |a, &t| (a.0 + pm[t], a.1 + po[t]));
    m.idle_delta = if idle.len() > 20 { db(io / im.max(1e-20)) } else { f64::NAN };
    let dt: Vec<bool> = (0..frames).map(|t| near_act[t] && echo_act[t]).collect();
    let ss = stft_power(&near[..n], 512, 160);
    let se = stft_power(&echo[..n], 512, 160);
    let sy = stft_power(&mic[..n], 512, 160);
    let so = stft_power(&out[..n], 512, 160);
    let cells_floor = floor * 512.0 * 0.375 / 257.0;
    let (mut nk_o, mut nk_y) = (0.0f64, 0.0f64);
    for k in skip..ss.frames.len().min(so.frames.len()) {
        let center = (k * 160 + 256) / 160;
        if center >= frames {
            break;
        }
        if !dt[center] {
            continue;
        }
        for b in 2..257 {
            let s = f64::from(ss.frames[k][b]);
            let e = f64::from(se.frames[k][b]);
            if s > 10.0 * e && s > 10.0 * cells_floor {
                nk_o += f64::from(so.frames[k][b]);
                nk_y += f64::from(sy.frames[k][b]);
            }
        }
    }
    m.near_keep_dt = if nk_y > 0.0 { db(nk_o / nk_y.max(1e-20)) } else { f64::NAN };
    m.stoi = stoi(&hpf150(&near[..n]), &hpf150(&out[..n]), 16000);
    let pr = frame_power(&reference[..n.min(reference.len())], f);
    let mut last_far = -1000i64;
    let mut neq = vec![false; frames];
    for t in 0..frames {
        if t < pr.len() && pr[t] > 1e-6 {
            last_far = t as i64;
        }
        neq[t] = near_act[t] && !echo_wide[t] && (t as i64 - last_far) > 100;
    }
    let (qa, qb) =
        (0..frames).filter(|&t| neq[t]).fold((0.0, 0.0), |a, t| (a.0 + pm[t], a.1 + po[t]));
    m.neq_level = if qa > 0.0 { db(qb / qa) } else { f64::NAN };
    m.neq_snr = if neq.iter().any(|&v| v) { spectral_snr(&ss, &so, &neq) } else { f64::NAN };
    m
}

/// The loudest 10 ms frame of `x` (-1..1) in [from, to) samples, dBFS.
pub fn loudest_frame_dbfs(x: &[f32], from: usize, to: usize) -> f64 {
    let to = to.min(x.len());
    db(frame_power(&x[from.min(to)..to], 160).into_iter().fold(0.0, f64::max))
}
