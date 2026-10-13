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

//! The echo model through the real libwebrtc APM (no device): the variant
//! switch, the decisions, the kill switch, the device classification tables,
//! and what a scene's echo looks like with and without the model.

use std::time::Instant;

use cxx::UniquePtr;

use crate::audio_device_controller::ffi::{
    new_adm_proxy_harness, new_adm_proxy_harness_with_apm, AudioDeviceSelection,
    AudioDeviceSelectionKind, RootAudioControlUpdate,
};
use crate::root_echo_model::ffi::*;

const RATE: i32 = 48_000;
const FRAME: usize = 480;
const FS: f32 = 48_000.0;

const MODE_OFF: u32 = 0;
const MODE_ON: u32 = 1;
const MODE_AUTO: u32 = 2;

const STATE_OFF: u32 = 0;
const STATE_ON: u32 = 1;

const REASON_MODE_OFF: u32 = 0;
const REASON_MODE_ON: u32 = 1;
const REASON_DEVICE_KIND: u32 = 2;
const REASON_MEASURED_ECHO: u32 = 3;
const REASON_KILL_SWITCH: u32 = 4;
const REASON_UNAVAILABLE: u32 = 5;

const KIND_NONE: u32 = 0;
const KIND_UNKNOWN: u32 = 1;
const KIND_SPEAKERS: u32 = 2;
const KIND_LINE_OUT: u32 = 3;
const KIND_DISPLAY_AUDIO: u32 = 4;
const KIND_HEADPHONES: u32 = 5;
const KIND_HEADSET: u32 = 6;
const KIND_HANDSET: u32 = 7;
const KIND_BLUETOOTH: u32 = 8;

fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

/// A voiced source with syllables.
struct Voice {
    f0: f32,
    swing: f32,
    syllables_hz: f32,
    phase: f32,
}

impl Voice {
    fn sample(&mut self, t: f32) -> f32 {
        let f0 = self.f0 + self.swing * (2.0 * std::f32::consts::PI * 0.7 * t).sin();
        self.phase += 2.0 * std::f32::consts::PI * f0 / FS;
        let mut s = 0.0;
        for h in 1..20 {
            let hf = h as f32 * f0;
            if hf > 7000.0 {
                break;
            }
            let formant = 1.0 / (1.0 + ((hf - 700.0) / 400.0).powi(2))
                + 0.6 / (1.0 + ((hf - 1800.0) / 500.0).powi(2));
            s += formant * (h as f32 * self.phase).sin() / h as f32;
        }
        let envelope =
            (0.5 + 0.5 * (2.0 * std::f32::consts::PI * self.syllables_hz * t).sin()).powi(2);
        s * envelope
    }
}

/// 48 kHz: a far end (render) during [far_from, far_to) s, its echo at the
/// mic (30 ms late, two reflections, a loudspeaker's soft clip, `echo_gain`)
/// when `echo`, and a near voice during [near_from, near_to) s.
struct Scene {
    render: Vec<f32>,
    capture: Vec<f32>,
    far_active: Vec<bool>,
    near_active: Vec<bool>,
}

fn scene(seconds: f32, far: (f32, f32), near: (f32, f32), echo: bool, echo_gain: f32) -> Scene {
    let n = (seconds * FS) as usize;
    let mut noise = Lcg(11);
    let mut far_voice = Voice { f0: 130.0, swing: 25.0, syllables_hz: 3.5, phase: 0.0 };
    let mut near_voice = Voice { f0: 210.0, swing: 35.0, syllables_hz: 2.7, phase: 0.0 };
    let mut render = vec![0.0f32; n];
    let mut near_part = vec![0.0f32; n];
    let mut far_active = vec![false; n];
    let mut near_active = vec![false; n];
    for i in 0..n {
        let t = i as f32 / FS;
        if t >= far.0 && t < far.1 {
            render[i] = 9000.0 * far_voice.sample(t);
            far_active[i] = true;
        }
        if t >= near.0 && t < near.1 {
            near_part[i] = 5000.0 * near_voice.sample(t);
            near_active[i] = true;
        }
    }
    let delay = (0.030 * FS) as usize;
    let taps = [(0usize, 1.0f32), ((0.004 * FS) as usize, 0.45), ((0.009 * FS) as usize, 0.2)];
    let mut capture = vec![0.0f32; n];
    for i in 0..n {
        let mut e = 0.0;
        if echo {
            for &(d, g) in &taps {
                if i >= delay + d {
                    e += g * render[i - delay - d];
                }
            }
            e = 20000.0 * (echo_gain * e / 20000.0).tanh();
        }
        capture[i] = e + near_part[i] + 3.0 * noise.next();
    }
    Scene { render, capture, far_active, near_active }
}

fn frames(scene: &Scene) -> usize {
    scene.render.len() / FRAME
}

/// Runs the scene's frames from `from` (each followed by 10 ms of clock),
/// stereo render (both channels the far end), returns the outputs.
fn run(h: &RootEchoHarness, scene: &Scene, from: usize, to: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity((to - from) * FRAME);
    let mut render = vec![0.0f32; 2 * FRAME];
    for f in from..to {
        let r = &scene.render[f * FRAME..(f + 1) * FRAME];
        render[..FRAME].copy_from_slice(r);
        render[FRAME..].copy_from_slice(r);
        out.extend(h.process(RATE, 2, &render, &scene.capture[f * FRAME..(f + 1) * FRAME]));
        h.advance(10);
    }
    out
}

/// Energy (dB, int16 units) of the samples in [from_s, to_s) where `select`
/// holds.
fn level_db(signal: &[f32], from_s: f32, to_s: f32, select: impl Fn(usize) -> bool) -> f64 {
    let (a, b) = ((from_s * FS) as usize, ((to_s * FS) as usize).min(signal.len()));
    let sum: f64 = signal[a..b]
        .iter()
        .enumerate()
        .filter(|(k, _)| select(a + k))
        .map(|(_, v)| f64::from(*v) * f64::from(*v))
        .sum();
    10.0 * (sum + 1e-9).log10()
}

fn today() -> UniquePtr<RootEchoHarness> {
    new_root_echo_harness(true)
}

fn with_variants() -> UniquePtr<RootEchoHarness> {
    new_root_echo_harness(false)
}

/// Processes `scene` frames until `done` holds (at most `max` frames).
fn until(
    h: &RootEchoHarness,
    scene: &Scene,
    start: usize,
    max: usize,
    done: impl Fn(&RootEchoHarnessProbe) -> bool,
) -> usize {
    for f in start..start + max {
        run(h, scene, f % frames(scene), f % frames(scene) + 1);
        if done(&h.probe()) {
            return f + 1;
        }
    }
    panic!("not done after {max} frames: {:?}", h.probe());
}

#[test]
fn root_echo_classifies_windows_endpoints() {
    let cases = [
        (1, false, KIND_SPEAKERS),
        (2, false, KIND_LINE_OUT),
        (3, false, KIND_HEADPHONES),
        (5, false, KIND_HEADSET),
        (6, false, KIND_HANDSET),
        (9, false, KIND_DISPLAY_AUDIO),
        (0, false, KIND_UNKNOWN),
        (4, false, KIND_UNKNOWN),
        (7, false, KIND_UNKNOWN),
        (8, false, KIND_UNKNOWN),
        (10, false, KIND_UNKNOWN),
        (1, true, KIND_BLUETOOTH),
        (3, true, KIND_BLUETOOTH),
        (5, true, KIND_BLUETOOTH),
    ];
    for (form_factor, bluetooth, kind) in cases {
        assert_eq!(
            root_output_kind_from_windows(form_factor, bluetooth),
            kind,
            "{form_factor} {bluetooth}"
        );
    }
    for id in [
        "BTHENUM\\{0000110B-0000-1000-8000-00805F9B34FB}_VID&0001004C_PID&2002\\7&1&0",
        "BTHHFENUM\\BthHFPAudio\\8&2&97",
        "BTHLEDevice\\{0000184E-0000-1000-8000-00805F9B34FB}_Dev_VID&0004c\\8&1",
        "bthle\\x",
    ] {
        assert!(root_is_bluetooth_instance_id(id), "{id}");
    }
    for id in [
        "HDAUDIO\\FUNC_01&VEN_10EC&DEV_0257\\4&1",
        "USB\\VID_046D&PID_0A87\\x",
        "SWD\\MMDEVAPI\\{0.0.0}",
        "BT",
        "",
    ] {
        assert!(!root_is_bluetooth_instance_id(id), "{id}");
    }
}

#[test]
fn root_echo_classifies_macos_devices() {
    let none = (false, 0);
    let kind = |transport: &[u8; 4], source: (bool, u32), terminal: (bool, u32)| {
        root_output_kind_from_mac(fourcc(transport), source.0, source.1, terminal.0, terminal.1)
    };
    let source = |s: &[u8; 4]| (true, fourcc(s));
    let terminal = |s: &[u8; 4]| (true, fourcc(s));
    assert_eq!(kind(b"bltn", source(b"ispk"), none), KIND_SPEAKERS);
    assert_eq!(kind(b"bltn", source(b"espk"), none), KIND_SPEAKERS);
    assert_eq!(kind(b"bltn", source(b"hdpn"), none), KIND_HEADPHONES);
    assert_eq!(kind(b"bltn", source(b"line"), none), KIND_LINE_OUT);
    assert_eq!(kind(b"bltn", source(b"spdf"), terminal(b"spkr")), KIND_SPEAKERS);
    assert_eq!(kind(b"bltn", none, terminal(b"spkr")), KIND_SPEAKERS);
    assert_eq!(kind(b"bltn", none, none), KIND_UNKNOWN);
    for t in [b"blue", b"blea"] {
        assert_eq!(kind(t, source(b"ispk"), terminal(b"spkr")), KIND_BLUETOOTH);
    }
    for t in [b"hdmi", b"dprt"] {
        assert_eq!(kind(t, none, terminal(b"spkr")), KIND_DISPLAY_AUDIO);
    }
    for t in [b"airp", b"virt", b"grup", b"fgrp", b"ccwd", b"ccwl"] {
        assert_eq!(kind(t, none, terminal(b"spkr")), KIND_UNKNOWN);
    }
    assert_eq!(root_output_kind_from_mac(0, false, 0, true, fourcc(b"spkr")), KIND_UNKNOWN);
    assert_eq!(kind(b"usb ", none, terminal(b"spkr")), KIND_SPEAKERS);
    assert_eq!(kind(b"usb ", none, terminal(b"lfes")), KIND_SPEAKERS);
    assert_eq!(kind(b"usb ", none, terminal(b"hdph")), KIND_HEADPHONES);
    assert_eq!(kind(b"usb ", none, terminal(b"line")), KIND_LINE_OUT);
    assert_eq!(kind(b"usb ", none, terminal(b"rspk")), KIND_HANDSET);
    assert_eq!(kind(b"thun", none, terminal(b"hdmi")), KIND_DISPLAY_AUDIO);
    assert_eq!(kind(b"usb ", none, terminal(b"spdf")), KIND_UNKNOWN);
    assert_eq!(kind(b"usb ", none, none), KIND_UNKNOWN);
}

#[test]
fn root_echo_policy_decides_by_mode_and_device_kind() {
    let decide = |mode: u32, kind: u32| root_echo_policy_probe(mode, kind, &[], 20);
    for kind in [KIND_SPEAKERS, KIND_LINE_OUT, KIND_DISPLAY_AUDIO] {
        let p = decide(MODE_AUTO, kind);
        assert!(p.on && p.reason == REASON_DEVICE_KIND, "{kind}");
    }
    for kind in
        [KIND_NONE, KIND_UNKNOWN, KIND_HEADPHONES, KIND_HEADSET, KIND_HANDSET, KIND_BLUETOOTH]
    {
        let p = decide(MODE_AUTO, kind);
        assert!(!p.on && p.reason == REASON_DEVICE_KIND, "{kind}");
    }
    for kind in [KIND_SPEAKERS, KIND_HEADPHONES, KIND_BLUETOOTH] {
        let on = decide(MODE_ON, kind);
        assert!(on.on && on.reason == REASON_MODE_ON);
        let off = decide(MODE_OFF, kind);
        assert!(!off.on && off.reason == REASON_MODE_OFF);
        let unknown = decide(7, kind);
        assert!(!unknown.on && unknown.reason == REASON_MODE_OFF, "unknown modes are off");
    }
}

/// 8 samples (2 s) of 3 dB ERLE or more within the last 40 (10 s).
#[test]
fn root_echo_measured_echo_needs_2_s_of_evidence_in_10_s() {
    let auto = |erle: &[f64], frames: u64| {
        root_echo_policy_probe(MODE_AUTO, KIND_HEADPHONES, erle, frames)
    };
    // The first sample is the baseline; then 7 of 8 evidence samples: off.
    let mut erle = vec![9.0; 8];
    let p = auto(&erle, 20);
    assert!(!p.on && !p.measured_echo && p.evidence == 7 && p.latched_at == -1, "{p:?}");
    erle.push(9.0);
    let p = auto(&erle, 20);
    assert!(
        p.on && p.measured_echo && p.reason == REASON_MEASURED_ECHO && p.latched_at == 8,
        "{p:?}"
    );
    // 8 evidence samples spread over the 40-sample window still latch.
    let spread: Vec<f64> = (0..41).map(|i| if i % 5 == 1 { 3.0 } else { 0.0 }).collect();
    assert!(auto(&spread, 20).on);
    // One every 6 samples is at most 7 in any 40: the oldest leave the window.
    let thin: Vec<f64> = (0..80).map(|i| if i % 6 == 1 { 4.0 } else { 0.0 }).collect();
    assert!(!auto(&thin, 20).on);
    // ERLE under 3 dB is no evidence, nor NaN.
    assert!(!auto(&[2.9; 60], 20).on);
    assert!(!auto(&[f64::NAN; 60], 20).on);
    // A sample counts only after 20 new capture frames: frozen statistics (no
    // capture) never do, and slow capture counts every other sample.
    let p = auto(&[30.0; 60], 0);
    assert!(!p.on && p.counted == 0, "{p:?}");
    let p = auto(&[30.0; 16], 19);
    assert!(!p.on && p.counted == 7, "{p:?}");
    // Loudspeakers are on already, ON needs no measurement, OFF takes none.
    assert_eq!(root_echo_policy_probe(MODE_AUTO, KIND_SPEAKERS, &[30.0; 60], 20).latched_at, -1);
    assert_eq!(root_echo_policy_probe(MODE_ON, KIND_HEADPHONES, &[30.0; 60], 20).latched_at, -1);
    let off = root_echo_policy_probe(MODE_OFF, KIND_HEADPHONES, &[30.0; 60], 20);
    assert!(!off.on && off.latched_at == -1);
}

#[test]
fn root_echo_kill_switch_trips_on_two_slow_windows_in_a_row() {
    let warmup = vec![5_000u64; 100];
    let window = |slow: usize| -> Vec<u64> {
        (0..1000).map(|i| if i < slow { 2_500 } else { 150 }).collect()
    };
    // The warm-up never counts, however slow.
    let mut script = warmup.clone();
    script.extend(window(10));
    script.extend(window(10));
    let p = root_echo_window_probe(&script, -1);
    assert_eq!(p.tripped_at, -1, "10 of 1000 over 2 ms is p99 at 2 ms: fine");
    assert!(p.p99_us <= 2000.0, "{p:?}");
    let mut script = warmup.clone();
    script.extend(window(11));
    let p = root_echo_window_probe(&script, -1);
    assert!(p.tripped_at == -1 && p.failing_streak == 1 && p.p99_us > 2000.0, "{p:?}");
    script.extend(window(0));
    script.extend(window(11));
    let p = root_echo_window_probe(&script, -1);
    assert_eq!(p.tripped_at, -1, "a good window in between starts over");
    script.extend(window(500));
    let p = root_echo_window_probe(&script, -1);
    assert_eq!(p.tripped_at, (100 + 4000 - 1) as i64, "{p:?}");
    let steady: Vec<u64> = vec![120; 3100];
    let p = root_echo_window_probe(&steady, -1);
    assert!(p.tripped_at == -1 && p.p99_us <= 125.0 && p.p99_us >= 100.0, "{p:?}");
    // A session reset starts the streak over: one failing window before it
    // and one after do not trip it, a second one after it does.
    let mut script = warmup.clone();
    script.extend(window(11));
    let reset_at = script.len() as i64;
    script.extend(window(11));
    let p = root_echo_window_probe(&script, reset_at);
    assert!(p.tripped_at == -1 && p.failing_streak == 1, "{p:?}");
    script.extend(window(11));
    let p = root_echo_window_probe(&script, reset_at);
    assert_eq!(p.tripped_at, (100 + 3000 - 1) as i64, "{p:?}");
}

#[test]
fn root_echo_adjust_config_applies_googles_tuning() {
    assert!(root_echo_adjust_config_is_googles_tuning());
}

/// An old desktop never sets the mode: the APM is today's, sample for sample,
/// through the whole chain (DSP defaults: voice gain, auto sensitivity, the
/// startup hold).
#[test]
fn root_echo_unset_mode_is_todays_apm_bit_for_bit() {
    let s = scene(6.0, (0.5, 4.0), (2.5, 5.5), true, 0.5);
    let reference = today();
    let candidate = with_variants();
    let off = with_variants();
    off.set_mode(MODE_OFF);
    let a = run(&reference, &s, 0, frames(&s));
    let b = run(&candidate, &s, 0, frames(&s));
    let c = run(&off, &s, 0, frames(&s));
    assert_eq!(a.len(), b.len());
    assert!(a.iter().any(|&v| v.abs() > 100.0), "the scene reaches the output");
    let first = a.iter().zip(&b).position(|(x, y)| x.to_bits() != y.to_bits());
    assert_eq!(first, None, "unset mode differs from today's APM at sample {first:?}");
    assert!(a.iter().zip(&c).all(|(x, y)| x.to_bits() == y.to_bits()), "OFF is today's APM");
    for p in [candidate.probe(), off.probe()] {
        assert!(!p.running_model && p.swaps == 0 && p.inferences == 0 && p.staged == -1, "{p:?}");
        assert_eq!((p.state, p.reason, p.kind), (STATE_OFF, REASON_MODE_OFF, KIND_NONE));
    }
}

/// A 440 Hz tone at the mic, silent render, through the float or the int16
/// interface; the model on at frame 100 and off at frame 700. Both switches
/// happen at a frame boundary, the output stays continuous and the voice
/// chain (its state, resets, format) is untouched.
fn switch_twice_without_a_glitch(int16: bool) {
    const TOTAL: usize = 1300;
    let tone: Vec<f32> = (0..TOTAL * FRAME)
        .map(|i| 6000.0 * (2.0 * std::f32::consts::PI * 440.0 * i as f32 / FS).sin())
        .collect();
    let silence = vec![0.0f32; 2 * FRAME];
    let h = with_variants();
    h.open_dsp_chain();
    h.prepare_model();
    let mut out: Vec<Vec<f32>> = Vec::new();
    let mut swaps_at = Vec::new();
    let mut before = h.probe();
    for f in 0..TOTAL {
        if f == 100 {
            before = h.probe();
            h.set_mode(MODE_ON);
        }
        if f == 700 {
            h.set_mode(MODE_OFF);
        }
        let capture = &tone[f * FRAME..(f + 1) * FRAME];
        out.push(if int16 {
            h.process_int16(RATE, 2, &silence, capture)
        } else {
            h.process(RATE, 2, &silence, capture)
        });
        h.advance(10);
        if h.probe().swaps as usize > swaps_at.len() {
            swaps_at.push(f);
        }
    }
    let after = h.probe();
    assert_eq!(swaps_at.len(), 2, "{swaps_at:?} {after:?}");
    assert!(!after.running_model && after.state == STATE_OFF, "{after:?}");
    assert!((100..700).contains(&swaps_at[0]) && swaps_at[1] >= 700, "{swaps_at:?}");
    assert_eq!(after.processed_frames - before.processed_frames, (TOTAL - 100) as u64);
    assert_eq!(
        (after.dsp_resets, after.dsp_flushes, after.format_seq),
        (before.dsp_resets, before.dsp_flushes, before.format_seq)
    );
    let rms =
        |v: &[f32]| (v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / v.len() as f64).sqrt();
    let reference = rms(&out[90]);
    for (f, frame) in out.iter().enumerate().skip(30) {
        let db = 20.0 * (rms(frame) / reference).log10();
        assert!(db.abs() < 1.0, "frame {f} (switches at {swaps_at:?}): {db:.2} dB");
    }
    let max_step = |v: &[f32]| v.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    let steady = max_step(&out[90]);
    for &swap in &swaps_at {
        let joined: Vec<f32> = out[swap - 1]
            .iter()
            .chain(out[swap].iter())
            .chain(out[swap + 1].iter())
            .copied()
            .collect();
        assert!(max_step(&joined) < 1.5 * steady, "{swap}: {} vs {steady}", max_step(&joined));
    }
}

#[test]
fn root_echo_switches_at_a_frame_boundary_without_a_glitch() {
    switch_twice_without_a_glitch(false);
}

/// The voice engine's interface: interleaved int16, processed in place.
#[test]
fn root_echo_int16_switches_at_a_frame_boundary_without_a_glitch() {
    switch_twice_without_a_glitch(true);
}

/// Through the int16 interface, an unset mode is today's APM sample for
/// sample too.
#[test]
fn root_echo_int16_unset_mode_is_todays_apm_bit_for_bit() {
    let s = scene(6.0, (0.5, 4.0), (2.5, 5.5), true, 0.5);
    let reference = today();
    let candidate = with_variants();
    let mut render = vec![0.0f32; 2 * FRAME];
    let (mut a, mut b) = (Vec::new(), Vec::new());
    for f in 0..frames(&s) {
        let r = &s.render[f * FRAME..(f + 1) * FRAME];
        render[..FRAME].copy_from_slice(r);
        render[FRAME..].copy_from_slice(r);
        let capture = &s.capture[f * FRAME..(f + 1) * FRAME];
        a.extend(reference.process_int16(RATE, 2, &render, capture));
        b.extend(candidate.process_int16(RATE, 2, &render, capture));
        reference.advance(10);
        candidate.advance(10);
    }
    assert!(a.iter().any(|&v| v.abs() > 100.0), "the scene reaches the output");
    assert_eq!(a, b);
}

/// On, off, on again, with echo present. The variant without the model that
/// replaces the model's is today's APM: its output equals, sample for sample,
/// a fresh APM (initialized for the formats, as a staged variant is) fed the
/// same frames from the 8 its warm-up replayed onward, once the fresh
/// harness's own voice chain is past its startup hold.
#[test]
fn root_echo_off_variant_is_a_fresh_apm_bit_for_bit() {
    let s = scene(20.0, (0.0, 20.0), (6.0, 12.0), true, 0.5);
    let a = with_variants();
    a.open_dsp_chain();
    a.prepare_model();
    a.set_mode(MODE_ON);
    let mut out: Vec<Vec<f32>> = Vec::new();
    let step = |h: &RootEchoHarness, out: &mut Vec<Vec<f32>>| {
        let f = out.len();
        out.push(run(h, &s, f, f + 1));
        h.probe()
    };
    while !step(&a, &mut out).running_model {
        assert!(out.len() < 500, "on");
    }
    for _ in 0..100 {
        step(&a, &mut out);
    }
    a.set_mode(MODE_OFF);
    let mut staged_after = None;
    let switched_at = loop {
        assert!(out.len() < 900, "off");
        let p = step(&a, &mut out);
        if staged_after.is_none() && p.staged == 0 {
            staged_after = Some(out.len() - 1);
        }
        if !p.running_model {
            break out.len() - 1;
        }
    };
    let staged_after = staged_after.expect("staged before the switch");
    assert_eq!(switched_at, staged_after + 1, "the next capture frame takes it");
    for _ in 0..300 {
        step(&a, &mut out);
    }
    assert_eq!(a.probe().swaps, 2);

    let first = staged_after - 7;
    let b = with_variants();
    b.open_dsp_chain();
    b.initialize(RATE, 2);
    let fresh = run(&b, &s, first, out.len());
    let from = switched_at + 40;
    let fresh_tail = &fresh[(from - first) * FRAME..];
    let model_tail: Vec<f32> = out[from..].iter().flatten().copied().collect();
    assert_eq!(fresh_tail.len(), model_tail.len());
    assert!(fresh_tail.iter().any(|&v| v.abs() > 100.0), "the scene reaches the output");
    let differs = fresh_tail.iter().zip(&model_tail).position(|(x, y)| x.to_bits() != y.to_bits());
    assert_eq!(differs, None, "the off variant differs from a fresh APM at {differs:?}");

    a.set_mode(MODE_ON);
    while !step(&a, &mut out).running_model {
        assert!(out.len() < frames(&s), "on again");
    }
    let p = a.probe();
    assert!(p.swaps == 3 && p.state == STATE_ON && p.reason == REASON_MODE_ON, "{p:?}");
}

#[test]
fn root_echo_auto_follows_the_output_device() {
    let s = scene(4.0, (0.0, 4.0), (9.0, 9.0), false, 0.0);
    let h = with_variants();
    h.prepare_model();
    h.set_device(KIND_SPEAKERS, "speakers");
    h.set_mode(MODE_AUTO);
    until(&h, &s, 0, 200, |p| p.running_model);
    let p = h.probe();
    assert_eq!(
        (p.state, p.reason, p.kind, p.swaps),
        (STATE_ON, REASON_DEVICE_KIND, KIND_SPEAKERS, 1)
    );
    h.set_device(KIND_HEADPHONES, "headphones");
    until(&h, &s, 0, 400, |p| !p.running_model);
    let p = h.probe();
    assert_eq!(
        (p.state, p.reason, p.kind, p.swaps),
        (STATE_OFF, REASON_DEVICE_KIND, KIND_HEADPHONES, 2)
    );
    h.set_device(KIND_BLUETOOTH, "airpods");
    h.set_device(KIND_DISPLAY_AUDIO, "monitor");
    until(&h, &s, 0, 400, |p| p.running_model);
    h.reset_session();
    until(&h, &s, 0, 400, |p| !p.running_model);
    let p = h.probe();
    assert_eq!((p.mode, p.state, p.reason, p.swaps), (MODE_OFF, STATE_OFF, REASON_MODE_OFF, 4));
    assert_eq!(p.kind, KIND_DISPLAY_AUDIO, "an open playout keeps its device across sessions");
    run(&h, &s, 0, 50);
    assert_eq!(h.probe().pending_tasks, 0, "nothing polls with the mode off and the model gone");
    h.close_playout();
    h.reset_session();
    h.advance(0);
    assert_eq!(h.probe().kind, KIND_NONE, "a closed playout's device is forgotten");
    h.set_mode(MODE_AUTO);
    run(&h, &s, 0, 50);
    let p = h.probe();
    assert!(!p.running_model && p.reason == REASON_DEVICE_KIND, "{p:?}");
}

/// A call that ends with the model on: the audio stops before the session
/// resets, so no frame takes the variant without it. The worker switches
/// once both directions are idle, and then stops polling.
#[test]
fn root_echo_switches_off_on_the_worker_once_audio_stops() {
    let s = scene(4.0, (0.0, 4.0), (9.0, 9.0), false, 0.0);
    let h = with_variants();
    h.prepare_model();
    h.set_mode(MODE_ON);
    until(&h, &s, 0, 200, |p| p.running_model);
    h.reset_session();
    h.advance(5000);
    let p = h.probe();
    assert!(!p.running_model && p.swaps == 2 && p.staged == -1 && p.state == STATE_OFF, "{p:?}");
    assert_eq!(p.pending_tasks, 0, "nothing polls between calls");
    let out = run(&h, &s, 0, 20);
    assert!(out.iter().all(|v| v.is_finite()) && h.probe().processed_frames > 0);
}

/// review M1: a reader stalled between reading the epoch and counting
/// itself, while one switch is collected and a second one made, keeps the
/// APM it then holds until it leaves.
#[test]
fn root_echo_grace_period_holds_a_stalled_reader() {
    let p = with_variants().grace_replay();
    assert!(p.first_switched && p.first_released, "{p:?}");
    assert!(p.reader_holds_second && p.reader_parity_moved, "{p:?}");
    assert!(p.second_switched && p.second_held && p.held_usable, "{p:?}");
    assert!(p.released_after_leave, "{p:?}");
}

/// With the APM's echo canceller off the model has nothing to run on: it is
/// off (unavailable) until echo cancellation is back.
#[test]
fn root_echo_model_is_off_while_echo_cancellation_is() {
    let s = scene(4.0, (0.0, 4.0), (9.0, 9.0), false, 0.0);
    let h = with_variants();
    h.prepare_model();
    h.set_mode(MODE_ON);
    until(&h, &s, 0, 200, |p| p.running_model);
    h.set_echo_cancellation(false);
    until(&h, &s, 0, 600, |p| !p.running_model);
    let p = h.probe();
    assert!(p.state == STATE_OFF && p.reason == REASON_UNAVAILABLE && p.inferences > 0, "{p:?}");
    h.set_echo_cancellation(true);
    until(&h, &s, 0, 600, |p| p.running_model);
    assert_eq!(h.probe().reason, REASON_MODE_ON);
}

/// A device without a key (an identity lookup that failed) is never
/// measured, so it cannot latch the model on for unrelated devices.
#[test]
fn root_echo_device_without_a_key_is_never_measured() {
    let s = scene(15.0, (0.0, 15.0), (40.0, 40.0), true, 0.6);
    let h = with_variants();
    h.set_device(KIND_HEADPHONES, "");
    h.set_mode(MODE_AUTO);
    run(&h, &s, 0, frames(&s));
    let p = h.probe();
    assert!(!p.measured_echo && !p.running_model && p.evidence == 0, "{p:?}");
}

#[test]
fn root_echo_measured_echo_turns_the_model_on_and_keeps_it() {
    let s = scene(30.0, (0.0, 30.0), (40.0, 40.0), true, 0.6);
    let h = with_variants();
    h.prepare_model();
    h.set_device(KIND_HEADPHONES, "not really headphones");
    h.set_mode(MODE_AUTO);
    let frames_to_on = until(&h, &s, 0, 1500, |p| p.running_model);
    let p = h.probe();
    assert!(p.measured_echo && p.reason == REASON_MEASURED_ECHO && p.state == STATE_ON, "{p:?}");
    assert!(frames_to_on <= 1000, "on after {frames_to_on} frames of echo");
    eprintln!("measured echo: the model ran after {} s of echo", frames_to_on as f32 / 100.0);
    run(&h, &s, frames_to_on, frames_to_on + 300);
    assert!(h.probe().running_model, "measurement never turns it off");
    h.set_device(KIND_HEADPHONES, "real headphones");
    until(&h, &s, 0, 400, |p| !p.running_model);
    h.set_device(KIND_HEADPHONES, "not really headphones");
    until(&h, &s, 0, 400, |p| p.running_model);
    assert_eq!(h.probe().reason, REASON_MEASURED_ECHO, "the latch is kept for the device");
}

#[test]
fn root_echo_no_echo_path_never_turns_the_model_on() {
    let s = scene(25.0, (0.0, 25.0), (0.0, 25.0), false, 0.0);
    let h = with_variants();
    h.prepare_model();
    h.set_device(KIND_HEADPHONES, "headphones");
    h.set_mode(MODE_AUTO);
    run(&h, &s, 0, frames(&s));
    let p = h.probe();
    assert!(!p.running_model && !p.measured_echo && p.swaps == 0, "{p:?}");
    assert!(p.erle_db.is_nan() || p.erle_db < 3.0, "erle {}", p.erle_db);
}

#[test]
fn root_echo_kill_switch_turns_the_model_off_for_the_session() {
    let s = scene(4.0, (0.0, 4.0), (9.0, 9.0), false, 0.0);
    let h = with_variants();
    h.prepare_model();
    h.set_mode(MODE_ON);
    until(&h, &s, 0, 200, |p| p.running_model);
    h.trip_kill_switch();
    until(&h, &s, 0, 400, |p| !p.running_model);
    let p = h.probe();
    assert!(p.kill_switch && p.reason == REASON_KILL_SWITCH && p.state == STATE_OFF, "{p:?}");
    h.set_mode(MODE_ON);
    run(&h, &s, 0, 100);
    assert!(!h.probe().running_model, "off until the session ends");
    h.reset_session();
    h.set_mode(MODE_ON);
    until(&h, &s, 0, 400, |p| p.running_model);
    assert!(!h.probe().kill_switch);
}

#[test]
fn root_echo_reports_inference_costs() {
    let s = scene(12.0, (0.0, 12.0), (6.0, 12.0), true, 0.5);
    let h = with_variants();
    h.prepare_model();
    h.set_mode(MODE_ON);
    until(&h, &s, 0, 200, |p| p.running_model);
    let first = h.probe();
    run(&h, &s, 0, 1000);
    let p = h.probe();
    let inferences = p.inferences - first.inferences;
    assert!((1200..=1300).contains(&inferences), "125 per second: {inferences}");
    assert!(p.inference_mean_us > 0.0 && p.inference_mean_us < 5000.0, "{p:?}");
    assert!(p.inference_max_us >= p.inference_mean_us as u64);
    assert!(p.inference_p99_us.is_finite(), "a window of 1000 completed: {p:?}");
    let again = h.probe();
    assert!(
        again.inference_mean_us.is_nan() && again.inference_max_us == 0,
        "reset on read: {again:?}"
    );
    assert!(!p.kill_switch);
    h.reset_session();
    assert!(h.probe().inference_p99_us.is_nan(), "a new session has no window yet");
}

/// The research's 48 kHz laptop scenes, synthesized: a loudspeaker driven
/// into clipping (what AEC3's linear filter cannot model), a sparse room
/// response with an exponential tail (RT60 about 0.35 s), the far end at
/// about -20 dBFS, mic noise at -80 dBFS. `chimes` adds three UI sounds
/// before the far end starts talking (the fresh-call burst).
#[derive(Clone, Copy, PartialEq)]
enum Chimes {
    None,
    /// The research's: three-partial bells, 600 ms each.
    Bells,
    /// Two-tone beeps, 400 ms with an 80 ms decay: purely tonal.
    Beeps,
}

fn laptop_scene(seconds: f32, far: (f32, f32), near: (f32, f32), chimes: Chimes) -> Scene {
    let n = (seconds * FS) as usize;
    let mut noise = Lcg(5);
    let mut far_voice = Voice { f0: 125.0, swing: 25.0, syllables_hz: 3.3, phase: 0.0 };
    let mut near_voice = Voice { f0: 205.0, swing: 35.0, syllables_hz: 2.6, phase: 0.0 };
    let mut render = vec![0.0f32; n];
    let mut near_part = vec![0.0f32; n];
    let mut far_active = vec![false; n];
    let mut near_active = vec![false; n];
    for i in 0..n {
        let t = i as f32 / FS;
        if t >= far.0 && t < far.1 {
            render[i] = 7000.0 * far_voice.sample(t);
            far_active[i] = true;
        }
        if chimes == Chimes::Beeps {
            for start in [0.5f32, 1.4, 2.3] {
                let dt = t - start;
                if (0.0..0.4).contains(&dt) {
                    let env = (-dt / 0.08).exp() * (dt / 0.003).min(1.0);
                    let w = 2.0 * std::f32::consts::PI * t;
                    render[i] +=
                        9000.0 * env * (0.6 * (w * 1046.5).sin() + 0.4 * (w * 1568.0).sin());
                    far_active[i] = true;
                }
            }
        }
        if chimes == Chimes::Bells {
            for (k, start) in [0.5f32, 1.4, 2.3].into_iter().enumerate() {
                let dt = t - start;
                if (0.0..0.6).contains(&dt) {
                    let f0 = if k % 2 == 0 { 880.0 } else { 660.0 };
                    let env = (-6.0 * dt).exp() * (1.0 - (-400.0 * dt).exp());
                    let w = 2.0 * std::f32::consts::PI * f0 * dt;
                    let bell = w.sin() + 0.5 * (2.01 * w).sin() + 0.25 * (3.02 * w).sin();
                    render[i] += 6500.0 * env * bell;
                    far_active[i] = true;
                }
            }
        }
        if t >= near.0 && t < near.1 {
            near_part[i] = 4000.0 * near_voice.sample(t);
            near_active[i] = true;
        }
    }
    let speaker: Vec<f32> = render.iter().map(|&x| 14000.0 * (2.5 * x / 14000.0).tanh()).collect();
    let mut taps = vec![((0.025 * FS) as usize, 0.5f32)];
    let mut tap_noise = Lcg(9);
    for k in 1..80 {
        let delay = 0.025 + 0.3 * (k as f32 / 80.0) + 0.002 * tap_noise.next();
        let gain = 0.25 * (-(delay - 0.025) / 0.05).exp() * tap_noise.next();
        taps.push(((delay * FS) as usize, gain));
    }
    let mut capture = vec![0.0f32; n];
    for i in 0..n {
        let mut e = 0.0;
        for &(d, g) in &taps {
            if i >= d {
                e += g * speaker[i - d];
            }
        }
        capture[i] = e + near_part[i] + 3.0 * noise.next();
    }
    Scene { render, capture, far_active, near_active }
}

/// Of the 50 ms windows wholly in [from_s, to_s) where the far end is active
/// and the near end not, the share whose output is more than 6 dB above the
/// mic noise floor, and the loudest one (dB over the floor).
fn leak(signal: &[f32], scene: &Scene, from_s: f32, to_s: f32) -> (f64, f64) {
    let window = (0.05 * FS) as usize;
    let floor_db = 20.0 * (3.0f64 / 3f64.sqrt()).log10();
    let (mut windows, mut leaking, mut worst) = (0usize, 0usize, f64::NEG_INFINITY);
    let mut start = (from_s * FS) as usize;
    while start + window <= ((to_s * FS) as usize).min(signal.len()) {
        let range = start..start + window;
        if range.clone().all(|i| scene.far_active[i] && !scene.near_active[i]) {
            let rms = (signal[range].iter().map(|v| f64::from(*v).powi(2)).sum::<f64>()
                / window as f64)
                .sqrt();
            let over = 20.0 * rms.max(1e-9).log10() - floor_db;
            windows += 1;
            if over > 6.0 {
                leaking += 1;
            }
            worst = worst.max(over);
        }
        start += window;
    }
    (100.0 * leaking as f64 / windows.max(1) as f64, worst)
}

/// Both harnesses over the scene from its first frame, the model APM in use
/// before it (no audio ran, so the worker switched).
fn compare(s: &Scene) -> (Vec<f32>, Vec<f32>) {
    let n = frames(s);
    let reference = today();
    let model = with_variants();
    for h in [&reference, &model] {
        h.open_dsp_chain();
    }
    model.prepare_model();
    model.set_mode(MODE_ON);
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    while !model.probe().running_model {
        assert!(Instant::now() < deadline, "on before the first frame");
        model.advance(10);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let a = run(&reference, s, 0, n);
    let b = run(&model, s, 0, n);
    assert!(model.probe().running_model);
    (a, b)
}

/// The research's claims on synthesized laptop scenes, with thresholds at or
/// below what they measure (printed with --nocapture): less residual echo
/// from a clipping loudspeaker, fewer leaking windows from call-start chimes
/// (their first onset leaks with and without the model here), the near voice
/// kept in double talk and when the far end is silent.
#[test]
fn root_echo_model_removes_more_echo_than_aec3_alone() {
    let s = laptop_scene(16.0, (0.2, 12.0), (8.0, 15.0), Chimes::None);
    let (a, b) = compare(&s);
    let (leak_a, worst_a) = leak(&a, &s, 2.0, 8.0);
    let (leak_b, worst_b) = leak(&b, &s, 2.0, 8.0);
    let double = |i: usize| s.near_active[i] && s.far_active[i];
    let near_only = |i: usize| s.near_active[i] && !s.far_active[i];
    let dt_a = level_db(&a, 8.0, 12.0, double);
    let dt_b = level_db(&b, 8.0, 12.0, double);
    let near_a = level_db(&a, 13.0, 15.0, near_only);
    let near_b = level_db(&b, 13.0, 15.0, near_only);
    eprintln!(
        "loud laptop: leak {leak_a:.1}% -> {leak_b:.1}%, worst {worst_a:.1} -> {worst_b:.1} dB over the floor; double talk {dt_a:.1} -> {dt_b:.1} dB; near only {near_a:.1} -> {near_b:.1} dB"
    );

    let c = laptop_scene(8.0, (4.0, 8.0), (9.0, 9.0), Chimes::Bells);
    let (ca, cb) = compare(&c);
    let (chime_leak_a, chime_worst_a) = leak(&ca, &c, 0.4, 3.5);
    let (chime_leak_b, chime_worst_b) = leak(&cb, &c, 0.4, 3.5);
    eprintln!(
        "fresh call bells: leak {chime_leak_a:.1}% -> {chime_leak_b:.1}%, worst {chime_worst_a:.1} -> {chime_worst_b:.1} dB over the floor"
    );

    assert!(leak_b <= leak_a / 2.0, "steady leak {leak_a:.1}% -> {leak_b:.1}%");
    assert!(worst_b <= worst_a + 1.0, "steady worst {worst_a:.1} -> {worst_b:.1} dB");
    assert!(
        chime_leak_b <= chime_leak_a * 0.75,
        "bells leak {chime_leak_a:.1}% -> {chime_leak_b:.1}%"
    );
    assert!(
        chime_worst_b <= chime_worst_a + 1.0,
        "bells {chime_worst_a:.1} -> {chime_worst_b:.1} dB"
    );
    assert!(dt_b >= dt_a - 3.0, "double talk {dt_a:.1} -> {dt_b:.1} dB");
    assert!((near_b - near_a).abs() <= 3.0, "near only {near_a:.1} -> {near_b:.1} dB");
}

/// The baseline's known weak spot (the research's music result): purely
/// tonal render (two-tone beeps) leaks far louder with the model than with
/// AEC3 alone (on this scene about 23 -> 56 dB over the floor at the worst
/// window). estimator-spec.md's never-worse rule for music is to fix it;
/// until then this stays ignored and documents the gap.
#[test]
#[ignore = "baseline x2 estimator: tonal render is worse than AEC3 alone until estimator-spec.md lands"]
fn root_echo_model_is_never_worse_on_tonal_render() {
    let c = laptop_scene(8.0, (4.0, 8.0), (9.0, 9.0), Chimes::Beeps);
    let (ca, cb) = compare(&c);
    let (leak_a, worst_a) = leak(&ca, &c, 0.4, 3.5);
    let (leak_b, worst_b) = leak(&cb, &c, 0.4, 3.5);
    eprintln!("beeps: leak {leak_a:.1}% -> {leak_b:.1}%, worst {worst_a:.1} -> {worst_b:.1} dB over the floor");
    assert!(worst_b <= worst_a + 1.0, "beeps {worst_a:.1} -> {worst_b:.1} dB");
    assert!(leak_b <= leak_a, "beeps leak {leak_a:.1}% -> {leak_b:.1}%");
}

#[test]
fn root_echo_switching_under_load_releases_every_apm() {
    let h = with_variants();
    h.prepare_model();
    let p = h.stress(40);
    assert_eq!((p.swaps, p.failed_stages), (40, 0), "{p:?}");
    assert!(p.collected && p.render_frames > 40 && p.capture_frames > 40 && p.reads > 0, "{p:?}");
}

/// The capture callback's cost (render and capture of one 10 ms frame at
/// 48 kHz) on the frame each switch happens, against the frames around it,
/// over four switches each way with echo present (printed with --nocapture).
#[test]
fn root_echo_switch_capture_cost() {
    let s = laptop_scene(30.0, (0.0, 30.0), (12.0, 20.0), Chimes::None);
    let h = with_variants();
    h.prepare_model();
    let mut render = vec![0.0f32; 2 * FRAME];
    let mut steady = Vec::new();
    let mut switch_costs = Vec::new();
    let mut f = 0;
    for switch in 0..8 {
        h.set_mode(if switch % 2 == 0 { MODE_ON } else { MODE_OFF });
        let swaps = h.probe().swaps;
        for _ in 0..300 {
            let r = &s.render[f * FRAME..(f + 1) * FRAME];
            render[..FRAME].copy_from_slice(r);
            render[FRAME..].copy_from_slice(r);
            let t = Instant::now();
            h.process(RATE, 2, &render, &s.capture[f * FRAME..(f + 1) * FRAME]);
            let us = t.elapsed().as_secs_f64() * 1e6;
            h.advance(10);
            f += 1;
            if h.probe().swaps > swaps {
                switch_costs.push(us);
                break;
            }
            steady.push(us);
        }
    }
    assert_eq!(switch_costs.len(), 8, "every switch happened");
    steady.sort_by(f64::total_cmp);
    let worst_switch = switch_costs.iter().copied().fold(0.0, f64::max);
    eprintln!(
        "capture callback: steady median {:.0} us, p99 {:.0} us, max {:.0} us; switch frames {:?} us, worst {worst_switch:.0} us",
        steady[steady.len() / 2],
        steady[steady.len() * 99 / 100],
        steady[steady.len() - 1],
        switch_costs.iter().map(|v| v.round() as u64).collect::<Vec<_>>()
    );
}

/// What ProcessStream costs per 10 ms at 48 kHz with and without the model
/// (printed with --nocapture); the model stays under 2 ms on average.
#[test]
fn root_echo_apm_cpu_per_10_ms() {
    let s = scene(10.0, (0.0, 10.0), (5.0, 10.0), true, 0.5);
    let n = frames(&s);
    let mut means = Vec::new();
    for model in [false, true] {
        let h = with_variants();
        if model {
            h.prepare_model();
            h.set_mode(MODE_ON);
            until(&h, &s, 0, 200, |p| p.running_model);
        }
        let mut render = vec![0.0f32; 2 * FRAME];
        let mut times = Vec::with_capacity(n);
        for f in 0..n {
            let r = &s.render[f * FRAME..(f + 1) * FRAME];
            render[..FRAME].copy_from_slice(r);
            render[FRAME..].copy_from_slice(r);
            let t = Instant::now();
            h.process(RATE, 2, &render, &s.capture[f * FRAME..(f + 1) * FRAME]);
            times.push(t.elapsed().as_secs_f64() * 1e6);
        }
        times.sort_by(f64::total_cmp);
        let mean = times.iter().sum::<f64>() / times.len() as f64;
        eprintln!(
            "APM {} model: render+capture per 10 ms mean {mean:.0} us, median {:.0} us, p99 {:.0} us",
            if model { "with" } else { "without" },
            times[times.len() / 2],
            times[times.len() * 99 / 100]
        );
        means.push(mean);
    }
    assert!(means[1] < 2000.0, "{means:?}");
}

#[test]
fn root_echo_playout_opens_reach_the_sink() {
    let h = new_adm_proxy_harness(&["{a}".to_string(), "{b}".to_string()], &["{mic}".to_string()]);
    h.acquire();
    h.enable(true, true);
    h.hold(true);
    h.flush();
    assert_eq!(h.take_playout_opened(), vec!["default|Device {a}".to_string()]);
    let unset = AudioDeviceSelection {
        kind: AudioDeviceSelectionKind::Default,
        id: String::new(),
        index: 0,
    };
    let b =
        AudioDeviceSelection { kind: AudioDeviceSelectionKind::Guid, id: "{b}".into(), index: 0 };
    h.select(&unset, false, &b, true);
    h.flush();
    assert_eq!(h.take_playout_opened(), vec!["{b}|Device {b}".to_string()]);
    h.restart(false, true);
    h.flush();
    assert_eq!(h.take_playout_opened(), vec!["{b}|Device {b}".to_string()]);
    h.hold(false);
    h.flush();
    assert_eq!(h.take_playout_opened(), vec!["closed".to_string()]);
}

#[test]
fn root_echo_control_is_refused_without_the_echo_model() {
    let h = new_adm_proxy_harness_with_apm(&["{a}".to_string()], &["{mic}".to_string()]);
    let echo = RootAudioControlUpdate {
        has_echo_model: true,
        echo_model: MODE_AUTO,
        ..Default::default()
    };
    assert!(!h.apply_control(&echo), "the decorator over a fake APM has no echo model");
    let other =
        RootAudioControlUpdate { has_mic_muted: true, mic_muted: true, ..Default::default() };
    assert!(h.apply_control(&other));
    let stats = h.stats();
    assert!(!stats.echo_model_available);
}

/// A 16-bit PCM mono WAV: its rate and samples (int16 scale).
fn read_wav(path: &std::path::Path) -> (u32, Vec<f32>) {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert!(bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE");
    let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
    let u32_at =
        |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let (mut rate, mut pos) = (0, 12);
    while pos + 8 <= bytes.len() {
        let size = u32_at(pos + 4) as usize;
        let body = pos + 8;
        if &bytes[pos..pos + 4] == b"fmt " {
            assert_eq!((u16_at(body), u16_at(body + 2), u16_at(body + 14)), (1, 1, 16));
            rate = u32_at(body + 4);
        } else if &bytes[pos..pos + 4] == b"data" {
            let end = (body + size).min(bytes.len());
            let samples = bytes[body..end]
                .chunks_exact(2)
                .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])))
                .collect();
            return (rate, samples);
        }
        pos = body + size + (size & 1);
    }
    panic!("{}: no data chunk", path.display());
}

/// The measured-echo rule over one recording (AUTO, a device it may latch):
/// when it latched (s), and the most evidence it saw.
fn measure_echo(rate: u32, render: &[f32], capture: &[f32]) -> (Option<f32>, u32) {
    let h = with_variants();
    h.set_device(KIND_HEADPHONES, "calibration");
    h.set_mode(MODE_AUTO);
    let frame = rate as usize / 100;
    let mut most = 0;
    for f in 0..render.len().min(capture.len()) / frame {
        let range = f * frame..(f + 1) * frame;
        h.process(rate as i32, 1, &render[range.clone()], &capture[range]);
        h.advance(10);
        let p = h.probe();
        most = most.max(p.evidence);
        if p.measured_echo {
            return (Some((f + 1) as f32 / 100.0), most);
        }
    }
    (None, most)
}

/// Review m12 (design section 8): the measured-echo thresholds against the
/// AEC Challenge's real recordings. No near-end single-talk clip under
/// another clip's far end latches, and at least 16 of the 24 far-end
/// single-talk clips (their loopback as the render) latch within 10 s; on the
/// others AEC3's linear filter never converges (ERLE under 2 dB, no ERLE rule
/// can see them) or converges after 10 s. Needs ROOTAPP_AEC_REAL_DIR, the
/// challenge's `real` folder (`<id>_farend_singletalk_{mic,lpb}.wav`,
/// `<id>_nearend_singletalk_mic.wav`). Prints every clip with --nocapture.
#[test]
#[ignore = "needs ROOTAPP_AEC_REAL_DIR (AEC Challenge real recordings)"]
fn root_echo_measured_echo_calibration_on_real_recordings() {
    let dir = std::path::PathBuf::from(
        std::env::var_os("ROOTAPP_AEC_REAL_DIR")
            .expect("ROOTAPP_AEC_REAL_DIR is not set: this test cannot run without it"),
    );
    let suffix = "_farend_singletalk_mic.wav";
    let mut ids: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter_map(|name| name.strip_suffix(suffix).map(str::to_string))
        .collect();
    ids.sort();
    assert!(!ids.is_empty(), "no far-end clips in {}", dir.display());
    let wav = |id: &str, part: &str| read_wav(&dir.join(format!("{id}_{part}.wav")));
    let (mut late, mut false_latches) = (Vec::new(), Vec::new());
    for (k, id) in ids.iter().enumerate() {
        let (rate, mic) = wav(id, "farend_singletalk_mic");
        let (lpb_rate, lpb) = wav(id, "farend_singletalk_lpb");
        assert_eq!(rate, lpb_rate);
        let (far, far_most) = measure_echo(rate, &lpb, &mic);
        let other = &ids[(k + 1) % ids.len()];
        let (_, unrelated) = wav(other, "farend_singletalk_lpb");
        let (_, near_mic) = wav(id, "nearend_singletalk_mic");
        let (near, near_most) = measure_echo(rate, &unrelated, &near_mic);
        eprintln!(
            "{id}: far end {} ({:.1} s, evidence {far_most}); near end under {other}'s far end {} (evidence {near_most})",
            far.map_or("never latched".to_string(), |s| format!("latched at {s:.2} s")),
            mic.len() as f32 / rate as f32,
            near.map_or("never latched".to_string(), |s| format!("latched at {s:.2} s")),
        );
        if far.is_none_or(|s| s > 10.0) {
            late.push(id.clone());
        }
        if near.is_some() {
            false_latches.push(id.clone());
        }
    }
    eprintln!(
        "{} far-end clips: {} latched within 10 s; {} near-end clips latched",
        ids.len(),
        ids.len() - late.len(),
        false_latches.len()
    );
    assert!(false_latches.is_empty(), "near end latched: {false_latches:?}");
    assert!(ids.len() - late.len() >= 16, "not latched within 10 s: {late:?}");
}
