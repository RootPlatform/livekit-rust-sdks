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

//! Native screen audio: the pipe (pacing, drift, gaps, trim) on a manual
//! clock, and the capture (control thread, restarts, attach to a real queued
//! source) with scripted fake backends. Device-free.

use std::time::{Duration, Instant};

use cxx::UniquePtr;

use crate::root_screen_audio::ffi::{
    capture_error_for_source, new_root_screen_audio_capture_harness,
    new_root_screen_audio_pipe_harness, screen_audio_ancestors, RootScreenAudioCaptureHarness,
    RootScreenAudioEventData, RootScreenAudioPipeHarness, RootScreenAudioTargetSpec,
};

const FRAME: usize = 480;
const FADE: usize = 144;
const TARGET: i32 = 960;
const STARTED: u32 = 1;
const FAILED: u32 = 2;
const STOPPED: u32 = 3;
const RUNNING: i32 = 2;

fn pipe() -> UniquePtr<RootScreenAudioPipeHarness> {
    new_root_screen_audio_pipe_harness()
}

/// A tone, continuous across packets.
struct Tone {
    rate: u32,
    channels: usize,
    frequency: f64,
    amplitude: f64,
    position: u64,
}

impl Tone {
    fn new(rate: u32, channels: usize, frequency: f64, amplitude: f64) -> Self {
        Self { rate, channels, frequency, amplitude, position: 0 }
    }

    fn packet(&mut self, frames: usize) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * self.channels);
        for i in 0..frames as u64 {
            let t = (self.position + i) as f64 / f64::from(self.rate);
            let v =
                (self.amplitude * (2.0 * std::f64::consts::PI * self.frequency * t).sin()) as f32;
            for _ in 0..self.channels {
                out.push(v);
            }
        }
        self.position += frames as u64;
        out
    }
}

/// Producer packets of `packet` frames every `interval_us`, consumer pulls
/// every 10 ms, in time order on the harness clock; `deliver(t)` may hold a
/// packet back (returns false) to model a late or paused capture.
struct Sim {
    producer_next: f64,
    consumer_next: f64,
    interval_us: f64,
    packet: usize,
    now: f64,
}

impl Sim {
    fn new(packet: usize, interval_us: f64) -> Self {
        Self { producer_next: 0.0, consumer_next: 0.0, interval_us, packet, now: 0.0 }
    }

    fn run(
        &mut self,
        h: &mut UniquePtr<RootScreenAudioPipeHarness>,
        tone: &mut Tone,
        until_us: f64,
        mut deliver: impl FnMut(f64) -> bool,
        mut pulled: impl FnMut(f64, &[i16]),
    ) {
        while self.producer_next.min(self.consumer_next) < until_us {
            let next = self.producer_next.min(self.consumer_next);
            let step = (next - self.now).round() as i64;
            if step > 0 {
                h.pin_mut().advance_us(step);
                self.now += step as f64;
            }
            if self.producer_next <= self.consumer_next {
                if deliver(self.producer_next) {
                    let packet = tone.packet(self.packet);
                    h.pin_mut().write(&packet);
                }
                self.producer_next += self.interval_us;
            } else {
                let out = h.pin_mut().pull(2);
                pulled(self.consumer_next, &out);
                self.consumer_next += 10_000.0;
            }
        }
    }
}

/// `frames` frames in 10 ms packets (a capture that delivers that way).
fn write_packets(h: &mut UniquePtr<RootScreenAudioPipeHarness>, samples: &[f32], channels: usize) {
    for packet in samples.chunks(FRAME * channels) {
        h.pin_mut().write(packet);
    }
}

fn rms(samples: &[i16]) -> f64 {
    let sum: f64 = samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum / samples.len().max(1) as f64).sqrt()
}

fn max_step(samples: &[i16], channels: usize) -> i32 {
    samples
        .chunks(channels)
        .collect::<Vec<_>>()
        .windows(2)
        .map(|w| (i32::from(w[1][0]) - i32::from(w[0][0])).abs())
        .max()
        .unwrap_or(0)
}

#[test]
fn screen_audio_pipe_sends_silence_until_primed() {
    let mut h = pipe();
    for _ in 0..10 {
        h.pin_mut().advance_us(10_000);
        let out = h.pin_mut().pull(2);
        assert_eq!(out.len(), FRAME * 2);
        assert!(out.iter().all(|&s| s == 0));
    }
    // Less than target + need never starts.
    h.pin_mut().write(&vec![0.5; (TARGET as usize + FRAME) * 2]);
    let out = h.pin_mut().pull(2);
    assert!(out.iter().all(|&s| s == 0));
    let stats = h.pin_mut().stats();
    assert_eq!(stats.state, 1);
    assert_eq!(stats.silence_frames, 11 * FRAME as u64);
    assert_eq!(stats.sent_frames, 0);
    assert_eq!(stats.pulls, 11);
    assert_eq!(stats.captured_frames, (TARGET as u64 + FRAME as u64));
}

#[test]
fn screen_audio_pipe_passes_audio_through_unchanged_at_ratio_one() {
    let mut h = pipe();
    // Distinct samples: left n, right -n (in int16 units).
    let sample = |n: usize| (n % 20_000) as f32 / 32767.0;
    let mut written = 0usize;
    let mut write = |h: &mut UniquePtr<RootScreenAudioPipeHarness>, frames: usize| {
        let mut packet = Vec::with_capacity(frames * 2);
        for i in written..written + frames {
            packet.push(sample(i));
            packet.push(-sample(i));
        }
        written += frames;
        h.pin_mut().write(&packet);
    };
    // Three 10 ms packets and 3 frames: exactly target + 483 (one pull with
    // the resampler's look-ahead), so it starts at once and nothing is skipped.
    for frames in [FRAME, FRAME, FRAME, 3] {
        write(&mut h, frames);
    }
    let mut output = Vec::new();
    for _ in 0..50 {
        h.pin_mut().advance_us(10_000);
        output.extend(h.pin_mut().pull(2));
        write(&mut h, FRAME);
    }
    let stats = h.pin_mut().stats();
    assert_eq!(stats.skipped_frames, 0);
    let first = 0;
    assert_eq!(stats.underruns, 0);
    assert_eq!(stats.level_frames, TARGET);
    assert_eq!(h.ratio(), 1.0);
    for (k, frame) in output.chunks(2).enumerate().skip(FADE) {
        let expected = (sample(first + k) * 32767.0) as i16;
        assert_eq!(frame[0], expected, "frame {k}");
        assert_eq!(frame[1], -expected, "frame {k}");
    }
    assert_eq!(stats.sent_frames, 50 * FRAME as u64);
    assert_eq!(stats.silence_frames, 0);
    assert_eq!(stats.state, RUNNING);
}

#[test]
fn screen_audio_pipe_downmixes_and_feeds_mono_sources() {
    // Mono capture is copied to both channels.
    let mut h = pipe();
    assert!(h.pin_mut().configure(48_000, 1));
    write_packets(&mut h, &vec![0.25; 4 * FRAME], 1);
    let out = h.pin_mut().pull(2);
    let tail = &out[FADE * 2..];
    assert!(tail.chunks(2).all(|f| f[0] == 8191 && f[1] == 8191), "{:?}", &tail[..4]);

    // Six channels: the first two are kept.
    let mut h = pipe();
    assert!(h.pin_mut().configure(48_000, 6));
    let mut packet = Vec::new();
    for _ in 0..4 * FRAME {
        packet.extend_from_slice(&[0.5, -0.25, 1.0, 1.0, 1.0, 1.0]);
    }
    write_packets(&mut h, &packet, 6);
    let out = h.pin_mut().pull(2);
    assert!(out[FADE * 2..].chunks(2).all(|f| f[0] == 16383 && f[1] == -8191));

    // A mono source gets the average of left and right.
    let mut h = pipe();
    let mut packet = Vec::new();
    for _ in 0..4 * FRAME {
        packet.extend_from_slice(&[0.5, 0.0]);
    }
    write_packets(&mut h, &packet, 2);
    let out = h.pin_mut().pull(1);
    assert_eq!(out.len(), FRAME);
    assert!(out[FADE..].iter().all(|&s| s == 8191));
    assert_eq!(h.pin_mut().stats().sent_samples, FRAME as u64);

    // Formats the converter cannot take are refused.
    let mut h = pipe();
    assert!(!h.pin_mut().configure(22_050, 2));
    assert!(!h.pin_mut().configure(48_000, 9));
    assert!(!h.pin_mut().configure(4_000, 2));
    assert!(h.pin_mut().configure(44_100, 2));
}

#[test]
fn screen_audio_pipe_converts_44100_to_48000() {
    let mut h = pipe();
    assert!(h.pin_mut().configure(44_100, 2));
    let mut tone = Tone::new(44_100, 2, 1000.0, 0.5);
    let mut sim = Sim::new(441, 10_000.0);
    let mut output = Vec::new();
    sim.run(
        &mut h,
        &mut tone,
        3_000_000.0,
        |_| true,
        |t, out| {
            if t >= 1_000_000.0 {
                output.extend_from_slice(out);
            }
        },
    );
    let stats = h.pin_mut().stats();
    assert_eq!(stats.underruns, 0);
    assert_eq!(stats.overflow_frames, 0);
    // 1 kHz at 48 kHz: 2000 zero crossings per second on the left channel.
    let left: Vec<i16> = output.chunks(2).map(|f| f[0]).collect();
    let crossings = left.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
    assert!((3990..=4010).contains(&crossings), "{crossings} crossings in 2 s");
    let level = rms(&left);
    let expected = 0.5 * 32767.0 / 2f64.sqrt();
    assert!((level - expected).abs() / expected < 0.02, "rms {level} vs {expected}");
    assert!(max_step(&output, 2) < 2200);
}

/// Three minutes of a capture clock `ppm` fast (10 ms packets that often):
/// the mean rate correction over the last two minutes (the instantaneous one
/// follows the packet phase, which beats against the pulls), the lowest level
/// seen then, and the underruns, trimmed and overflow frames.
fn drift(ppm: f64) -> (f64, i32, u64) {
    let mut h = pipe();
    let mut tone = Tone::new(48_000, 2, 440.0, 0.3);
    let mut sim = Sim::new(480, 10_000.0 / (1.0 + ppm * 1e-6));
    sim.run(&mut h, &mut tone, 60_000_000.0, |_| true, |_, _| {});
    let mut ratios = 0.0;
    let mut lowest = i32::MAX;
    for second in 61..=180 {
        sim.run(&mut h, &mut tone, f64::from(second) * 1e6, |_| true, |_, _| {});
        ratios += h.ratio();
        lowest = lowest.min(h.pin_mut().stats().level_frames);
    }
    let stats = h.pin_mut().stats();
    (ratios / 120.0, lowest, stats.underruns + stats.trimmed_frames + stats.overflow_frames)
}

#[test]
fn screen_audio_pipe_follows_slow_and_fast_capture_clocks() {
    for ppm in [-2000.0, -500.0, -100.0, 100.0, 500.0, 2000.0] {
        let (ratio, level, faults) = drift(ppm);
        assert_eq!(faults, 0, "{ppm} ppm: underruns, trims or overflow");
        let measured = (ratio - 1.0) * 1e6;
        assert!(
            (measured - ppm).abs() < 0.2 * ppm.abs() + 50.0,
            "{ppm} ppm: corrected at {measured:.0} ppm on average"
        );
        // The lowest level stays above what the law's correction needs below
        // the target (the 2 ms deadband plus ppm / 2000 of the 10 ms full
        // scale) less one packet of phase: never near empty.
        let law = 96.0 + ppm.abs() / 2000.0 * 480.0;
        assert!(
            f64::from(level) >= f64::from(TARGET) - law - 480.0 && level > 0,
            "{ppm} ppm: lowest level {level} frames"
        );
    }
}

#[test]
fn screen_audio_pipe_counts_a_capture_pause_as_a_gap() {
    let mut h = pipe();
    let mut tone = Tone::new(48_000, 2, 440.0, 0.3);
    let mut sim = Sim::new(480, 10_000.0);
    let mut output = Vec::new();
    // Nothing from 2.0 s to 2.5 s.
    sim.run(
        &mut h,
        &mut tone,
        5_000_000.0,
        |t| !(2_000_000.0..2_500_000.0).contains(&t),
        |t, out| {
            if (1_900_000.0..2_700_000.0).contains(&t) {
                output.extend_from_slice(out);
            }
        },
    );
    let stats = h.pin_mut().stats();
    assert_eq!(stats.underruns, 1);
    assert_eq!(stats.gaps, 1);
    assert_eq!(stats.target_frames, TARGET);
    assert_eq!(stats.capture_stalls, 1);
    assert!(stats.write_max_gap_us >= 500_000);
    // About 0.5 s of silence went out, faded both ways (no step larger than
    // the tone's own between consecutive samples).
    assert!(stats.silence_frames >= 24_000 && stats.silence_frames <= 26_000 + 2 * FRAME as u64);
    let tone_step = (0.3 * 32767.0 * 2.0 * std::f64::consts::PI * 440.0 / 48_000.0) as i32 + 2;
    assert!(max_step(&output, 2) <= tone_step, "step {} > {}", max_step(&output, 2), tone_step);
    assert_eq!(stats.state, RUNNING);
}

/// The capture stops with `left` frames in the queue after the last full pull
/// (what a pause leaves depends on its phase and the drift): the underrun
/// fades out over 3 ms even when less, or nothing, is left, so no step exceeds
/// the tone's own slope plus the fade's.
#[test]
fn screen_audio_pipe_fades_every_underrun_over_3_ms() {
    let tone_step = (0.3 * 32767.0 * 2.0 * std::f64::consts::PI * 440.0 / 48_000.0) as i32 + 2;
    let fade_step = (0.3 * 32767.0 * std::f64::consts::PI / 2.0 / (FADE as f64 + 1.0)) as i32 + 2;
    for left in [0usize, 1, 2, 20, 100, 143, 144, 300] {
        let mut h = pipe();
        let mut tone = Tone::new(48_000, 2, 440.0, 0.3);
        // Exactly the start threshold (target + one pull with look-ahead) plus
        // `left`; three pulls later `left` frames remain.
        let total = TARGET as usize + 483 + left;
        for packet in [FRAME, FRAME, FRAME, total - 3 * FRAME] {
            h.pin_mut().write(&tone.packet(packet));
        }
        let mut pulls = Vec::new();
        for _ in 0..6 {
            h.pin_mut().advance_us(10_000);
            pulls.push(h.pin_mut().pull(2));
        }
        let stats = h.pin_mut().stats();
        assert_eq!(stats.skipped_frames, 0, "left {left}");
        assert_eq!(stats.underruns, 1, "left {left}");
        let output: Vec<i16> = pulls.concat();
        let step = max_step(&output, 2);
        assert!(step <= tone_step + fade_step, "left {left}: step {step}");
        // The underrun pull still sounds well past what was left.
        let underrun = &pulls[3];
        let last = underrun.chunks(2).rposition(|f| f[0] != 0).unwrap_or(0);
        assert!(last + 16 >= left.max(FADE), "left {left}: audio ends at {last}");
        assert!(pulls[4].iter().all(|&s| s == 0));
    }
}

#[test]
fn screen_audio_pipe_raises_the_target_after_jitter_and_lowers_it_later() {
    let mut h = pipe();
    let mut tone = Tone::new(48_000, 2, 440.0, 0.3);
    let mut sim = Sim::new(480, 10_000.0);
    // The packets due between 2.00 and 2.04 s arrive 45 ms late (the capture
    // never paused 60 ms).
    let mut held = 0usize;
    sim.run(
        &mut h,
        &mut tone,
        2_045_000.0,
        |t| {
            if (2_000_000.0..2_040_000.0).contains(&t) {
                held += 1;
                false
            } else {
                true
            }
        },
        |_, _| {},
    );
    let late = tone.packet(held * 480);
    h.pin_mut().write(&late);
    sim.run(&mut h, &mut tone, 4_000_000.0, |_| true, |_, _| {});
    let stats = h.pin_mut().stats();
    assert_eq!(stats.underruns, 1);
    assert_eq!(stats.gaps, 0);
    assert_eq!(stats.target_frames, TARGET + 480);
    // After 60 s without another one it steps back down.
    sim.run(&mut h, &mut tone, 66_000_000.0, |_| true, |_, _| {});
    let stats = h.pin_mut().stats();
    assert_eq!(stats.underruns, 1);
    assert_eq!(stats.target_frames, TARGET);
}

#[test]
fn screen_audio_pipe_skips_a_backlog_when_it_starts() {
    let mut h = pipe();
    write_packets(&mut h, &Tone::new(48_000, 2, 440.0, 0.3).packet(24_000), 2);
    h.pin_mut().advance_us(10_000);
    h.pin_mut().pull(2);
    let stats = h.pin_mut().stats();
    // It keeps the target, one pull and one packet.
    assert_eq!(stats.skipped_frames, 24_000 - (TARGET as u64 + 483 + FRAME as u64));
    assert_eq!(stats.sent_frames, FRAME as u64);
}

#[test]
fn screen_audio_pipe_forgets_a_large_first_packet() {
    let mut h = pipe();
    let mut tone = Tone::new(48_000, 2, 440.0, 0.3);
    // A capture that starts with 50 ms of backlog in one packet, then 10 ms packets.
    h.pin_mut().write(&tone.packet(2400));
    let mut sim = Sim::new(480, 10_000.0);
    sim.run(&mut h, &mut tone, 10_000_000.0, |_| true, |_, _| {});
    let stats = h.pin_mut().stats();
    assert_eq!(stats.underruns, 0);
    assert!((stats.level_frames - TARGET).abs() < 200, "level {}", stats.level_frames);
}

#[test]
fn screen_audio_pipe_waits_for_a_large_packet_to_cover_the_target() {
    let mut h = pipe();
    let mut tone = Tone::new(48_000, 2, 440.0, 0.3);
    h.pin_mut().write(&tone.packet(4096));
    h.pin_mut().advance_us(10_000);
    assert!(h.pin_mut().pull(2).iter().all(|&s| s == 0));
    h.pin_mut().write(&tone.packet(4096));
    h.pin_mut().advance_us(10_000);
    assert!(h.pin_mut().pull(2).iter().any(|&s| s != 0));
    let stats = h.pin_mut().stats();
    assert_eq!(stats.skipped_frames, 0);
}

#[test]
fn screen_audio_pipe_trims_excess_click_free() {
    let mut h = pipe();
    let mut tone = Tone::new(48_000, 2, 440.0, 0.3);
    let mut sim = Sim::new(480, 10_000.0);
    sim.run(&mut h, &mut tone, 2_000_000.0, |_| true, |_, _| {});
    // 300 ms extra at once: the level jumps 300 ms above the target.
    let burst = tone.packet(14_400);
    h.pin_mut().write(&burst);
    let mut output = Vec::new();
    sim.run(&mut h, &mut tone, 4_000_000.0, |_| true, |_, out| output.extend_from_slice(out));
    let stats = h.pin_mut().stats();
    assert_eq!(stats.underruns, 0);
    assert!(
        stats.trimmed_frames >= 13_000 && stats.trimmed_frames <= 15_000,
        "trimmed {}",
        stats.trimmed_frames
    );
    assert!((stats.level_frames - TARGET).abs() < 700, "level {}", stats.level_frames);
    // The splice is aligned in phase: no jump beyond the tone's own slope.
    let tone_step = (0.3 * 32767.0 * 2.0 * std::f64::consts::PI * 440.0 / 48_000.0) as i32 + 40;
    assert!(max_step(&output, 2) <= tone_step, "step {} > {}", max_step(&output, 2), tone_step);
}

#[test]
fn screen_audio_pipe_counts_overflow_when_the_ring_is_full() {
    let mut h = pipe();
    let chunk = vec![0.1f32; 8192 * 2];
    for _ in 0..10 {
        h.pin_mut().write(&chunk);
    }
    let stats = h.pin_mut().stats();
    assert_eq!(stats.captured_frames, 81_920);
    assert_eq!(stats.overflow_frames, 81_920 - 65_536);
}

#[test]
fn screen_audio_pipe_clips_and_zeroes_non_finite_samples() {
    let mut h = pipe();
    let mut packet = Vec::new();
    for i in 0..4 * FRAME {
        let v = match i % 4 {
            0 => 1.5,
            1 => -2.0,
            2 => f32::NAN,
            _ => f32::INFINITY,
        };
        packet.push(v);
        packet.push(0.0);
    }
    write_packets(&mut h, &packet, 2);
    h.pin_mut().advance_us(10_000);
    let out = h.pin_mut().pull(2);
    let stats = h.pin_mut().stats();
    assert_eq!(stats.skipped_frames, 0);
    for (k, frame) in out.chunks(2).enumerate().skip(FADE) {
        let expected = match k % 4 {
            0 => 32767,
            1 => -32767,
            _ => 0,
        };
        assert_eq!(frame[0], expected, "frame {k}");
        assert_eq!(frame[1], 0);
    }
    // Every 1.5 and -2 after the fade-in, and those the fade left above 1.
    assert!(
        stats.clipped_samples >= 2 * (FRAME as u64 - FADE as u64) / 4,
        "clipped {}",
        stats.clipped_samples
    );
}

#[test]
fn screen_audio_pipe_counts_silence_discontinuities_and_write_gaps() {
    let mut h = pipe();
    h.pin_mut().write_silence(480);
    h.pin_mut().advance_us(70_000);
    h.pin_mut().write_silence(480);
    h.pin_mut().advance_us(5_000);
    h.pin_mut().discontinuity();
    h.pin_mut().write(&vec![0.0; 960]);
    let stats = h.pin_mut().stats();
    assert_eq!(stats.captured_frames, 1440);
    assert_eq!(stats.silent_capture_frames, 960);
    assert_eq!(stats.discontinuities, 1);
    assert_eq!(stats.write_max_gap_us, 70_000);
    // capture_stalls: one for the first configure (none here) plus the 70 ms wait.
    assert_eq!(stats.capture_stalls, 1);
    // Maxima reset on read.
    assert_eq!(h.pin_mut().stats().write_max_gap_us, 0);
}

#[test]
fn screen_audio_pipe_adapts_to_large_capture_packets() {
    let mut h = pipe();
    let mut tone = Tone::new(48_000, 2, 440.0, 0.3);
    // A PipeWire-style 4096-frame quantum (85 ms).
    let mut sim = Sim::new(4096, 4096.0 / 48_000.0 * 1e6);
    sim.run(&mut h, &mut tone, 20_000_000.0, |_| true, |_, _| {});
    let early = h.pin_mut().stats();
    assert!(early.underruns <= 2, "underruns {}", early.underruns);
    assert_eq!(early.gaps, 0);
    sim.run(&mut h, &mut tone, 40_000_000.0, |_| true, |_, _| {});
    let late = h.pin_mut().stats();
    assert_eq!(late.underruns, early.underruns, "underruns after settling");
    // The lowest level (just before a packet) holds the target.
    assert!((late.level_frames - late.target_frames).abs() < 600, "level {}", late.level_frames);
    assert_eq!(late.overflow_frames, 0);
}

// Capture with scripted backends (real time).

fn capture() -> UniquePtr<RootScreenAudioCaptureHarness> {
    new_root_screen_audio_capture_harness(2, 5)
}

fn system() -> RootScreenAudioTargetSpec {
    RootScreenAudioTargetSpec::default()
}

fn wait_for(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    done()
}

fn kinds(events: &[RootScreenAudioEventData]) -> Vec<u32> {
    events.iter().map(|e| e.kind).collect()
}

#[test]
fn screen_audio_capture_start_reports_and_feeds_the_source() {
    let h = capture();
    h.start(&RootScreenAudioTargetSpec { kind: 1, process_id: 1234, ..Default::default() }, 7);
    let events = h.wait_events(1, 2000);
    assert_eq!(kinds(&events), vec![STARTED]);
    assert_eq!(events[0].async_id, 7);
    assert_eq!(events[0].sample_rate, 48_000);
    assert_eq!(events[0].channels, 2);
    assert_eq!(events[0].description, "fake: all audio except this process");
    assert_eq!(h.last_target_kind(), 1);
    let loud_start = h.sink_loud_frames();
    assert!(wait_for(Duration::from_secs(3), || h.sink_loud_frames() > loud_start + 48_000));
    let stats = h.stats();
    assert_eq!(stats.state, RUNNING);
    assert_eq!(stats.capture_rate, 48_000);
    assert!(stats.sent_frames > 0);
    assert!(stats.pulls > 0);
    assert_eq!(stats.restarts, 0);
    assert_eq!(h.backend_starts(), 1);
    // The source keeps its 10 ms cadence.
    let frames = h.sink_frames();
    std::thread::sleep(Duration::from_millis(500));
    let delta = h.sink_frames() - frames;
    assert!((19_200..=28_800).contains(&delta), "{delta} frames in 0.5 s");
}

#[test]
fn screen_audio_capture_start_failure_does_not_restart() {
    let h = capture();
    h.script_start(2, 48_000, 2, 0);
    h.start(&system(), 3);
    let events = h.wait_events(1, 2000);
    assert_eq!(kinds(&events), vec![FAILED]);
    assert_eq!(events[0].async_id, 3);
    assert_eq!(events[0].error, 2);
    assert!(!events[0].retrying);
    std::thread::sleep(Duration::from_millis(100));
    assert!(h.wait_events(1, 50).is_empty());
    assert_eq!(h.backend_starts(), 1);
    let stats = h.stats();
    assert_eq!(stats.state, 4);
    assert_eq!(stats.last_error, 2);
    assert_eq!(stats.last_error_message, "fake: scripted failure");
    // The stream stays silent but keeps its cadence.
    assert_eq!(h.sink_loud_frames(), 0);
    assert!(wait_for(Duration::from_secs(1), || h.sink_frames() > 4800));
}

#[test]
fn screen_audio_capture_restarts_after_device_lost() {
    let h = capture();
    h.start(&system(), 1);
    assert_eq!(kinds(&h.wait_events(1, 2000)), vec![STARTED]);
    assert!(wait_for(Duration::from_secs(2), || h.stats().state == RUNNING));
    h.fail_running(4);
    let events = h.wait_events(2, 3000);
    assert_eq!(kinds(&events), vec![FAILED, STARTED]);
    assert_eq!(events[0].async_id, 0);
    assert_eq!(events[0].error, 4);
    assert!(events[0].retrying);
    assert_eq!(events[1].async_id, 0);
    assert_eq!(h.backend_starts(), 2);
    assert_eq!(h.backend_stops(), 1);
    assert!(h.last_target_restart());
    let loud = h.sink_loud_frames();
    assert!(wait_for(Duration::from_secs(2), || h.sink_loud_frames() > loud + 9600));
    let stats = h.stats();
    assert_eq!(stats.restarts, 1);
    assert_eq!(stats.last_error, 4);
    assert!(stats.gaps >= 1 || stats.underruns == 0);
}

#[test]
fn screen_audio_capture_gives_up_after_its_restart_budget() {
    let h = capture();
    h.start(&system(), 1);
    assert_eq!(kinds(&h.wait_events(1, 2000)), vec![STARTED]);
    for _ in 0..5 {
        h.script_start(3, 48_000, 2, 0);
    }
    h.fail_running(5);
    let events = h.wait_events(6, 5000);
    assert_eq!(kinds(&events), vec![FAILED; 6]);
    assert!(events[..5].iter().all(|e| e.retrying));
    assert!(!events[5].retrying);
    assert_eq!(events[0].error, 5);
    assert!(events[1..].iter().all(|e| e.error == 3));
    assert_eq!(h.backend_starts(), 6);
    assert_eq!(h.stats().state, 4);
}

/// A window share that resolved to one application: a restart that resolves to
/// all audio (the window is gone) ends the capture instead of widening it.
#[test]
fn screen_audio_capture_restart_never_widens_a_window_share() {
    let h = capture();
    h.script_start(0, 48_000, 2, 2);
    h.start(&RootScreenAudioTargetSpec { kind: 2, window_id: 42, ..Default::default() }, 1);
    let events = h.wait_events(1, 2000);
    assert_eq!(kinds(&events), vec![STARTED]);
    assert_eq!(events[0].scope, 2);
    assert!(!h.last_target_restart());
    assert!(wait_for(Duration::from_secs(2), || h.sink_loud_frames() > 4800));
    h.script_start(0, 48_000, 2, 0);
    h.fail_running(5);
    let events = h.wait_events(2, 3000);
    assert_eq!(kinds(&events), vec![FAILED, FAILED]);
    assert!(events[0].retrying);
    assert_eq!(events[0].error, 5);
    assert_eq!(events[1].async_id, 0);
    assert_eq!(events[1].error, 6);
    assert!(!events[1].retrying);
    assert!(events[1].message.contains("not widening"), "{}", events[1].message);
    assert_eq!(h.backend_starts(), 2);
    assert!(h.last_target_restart());
    assert_eq!(h.last_target_kind(), 2);
    std::thread::sleep(Duration::from_millis(200));
    assert!(h.wait_events(1, 50).is_empty());
    assert_eq!(h.backend_starts(), 2);
    let stats = h.stats();
    assert_eq!(stats.state, 4);
    assert_eq!(stats.scope, 2);
    assert_eq!(stats.last_error, 6);
    // Nothing of the widened capture reached the source.
    let loud = h.sink_loud_frames();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(h.sink_loud_frames(), loud);
}

#[test]
fn screen_audio_capture_restart_keeps_to_the_first_process() {
    let h = capture();
    h.script_start_for(1, 100);
    h.start(&RootScreenAudioTargetSpec { kind: 1, process_id: 100, ..Default::default() }, 1);
    assert_eq!(kinds(&h.wait_events(1, 2000)), vec![STARTED]);
    // The same process again: an ordinary restart.
    h.script_start_for(1, 100);
    h.fail_running(4);
    let events = h.wait_events(2, 3000);
    assert_eq!(kinds(&events), vec![FAILED, STARTED]);
    assert_eq!(events[1].scope, 1);
    // Another process (the window now belongs to someone else): gone.
    h.script_start_for(1, 200);
    h.fail_running(4);
    let events = h.wait_events(2, 3000);
    assert_eq!(kinds(&events), vec![FAILED, FAILED]);
    assert!(events[0].retrying);
    assert_eq!(events[1].error, 6);
    assert!(!events[1].retrying);
    assert_eq!(h.backend_starts(), 3);
    assert_eq!(h.stats().restarts, 1);
    // A new start from the host may resolve anything.
    h.script_start_for(1, 200);
    h.start(&RootScreenAudioTargetSpec { kind: 1, process_id: 200, ..Default::default() }, 2);
    let events = h.wait_events(1, 2000);
    assert_eq!(kinds(&events), vec![STARTED]);
    assert_eq!(events[0].async_id, 2);
    assert!(!h.last_target_restart());
}

/// Windows: a target that started Root captures nothing; the started event
/// says why, so the host can tell the user.
#[test]
fn screen_audio_capture_started_carries_the_limitation() {
    let h = capture();
    h.script_start_limited(1, 700, 1);
    h.start(&RootScreenAudioTargetSpec { kind: 1, process_id: 700, ..Default::default() }, 3);
    let events = h.wait_events(1, 2000);
    assert_eq!(kinds(&events), vec![STARTED]);
    assert_eq!(events[0].async_id, 3);
    assert_eq!(events[0].scope, 1);
    assert_eq!(events[0].limitation, 1);
    // An ordinary start has none.
    h.start(&system(), 4);
    let events = h.wait_events(1, 2000);
    assert_eq!(kinds(&events), vec![STARTED]);
    assert_eq!(events[0].limitation, 0);
}

#[test]
fn screen_audio_capture_restart_may_narrow_a_system_start() {
    let h = capture();
    h.start(&RootScreenAudioTargetSpec { kind: 2, window_id: 42, ..Default::default() }, 1);
    assert_eq!(kinds(&h.wait_events(1, 2000)), vec![STARTED]);
    h.script_start_for(1, 300);
    h.fail_running(4);
    let events = h.wait_events(2, 3000);
    assert_eq!(kinds(&events), vec![FAILED, STARTED]);
    assert_eq!(events[1].scope, 1);
}

#[test]
fn screen_audio_capture_target_gone_is_final() {
    let h = capture();
    h.start(&system(), 1);
    assert_eq!(kinds(&h.wait_events(1, 2000)), vec![STARTED]);
    h.fail_running(6);
    let events = h.wait_events(1, 2000);
    assert_eq!(kinds(&events), vec![FAILED]);
    assert!(!events[0].retrying);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(h.backend_starts(), 1);
    assert_eq!(h.backend_stops(), 1);
}

#[test]
fn screen_audio_capture_stop_reports_and_keeps_the_source_silent() {
    let h = capture();
    h.start(&system(), 1);
    assert_eq!(kinds(&h.wait_events(1, 2000)), vec![STARTED]);
    assert!(wait_for(Duration::from_secs(2), || h.sink_loud_frames() > 4800));
    h.stop(9);
    let events = h.wait_events(1, 2000);
    assert_eq!(kinds(&events), vec![STOPPED]);
    assert_eq!(events[0].async_id, 9);
    assert_eq!(h.backend_stops(), 1);
    assert_eq!(h.stats().state, 5);
    // What was queued drains, then silence at the same cadence.
    std::thread::sleep(Duration::from_millis(300));
    let loud = h.sink_loud_frames();
    let frames = h.sink_frames();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(h.sink_loud_frames(), loud);
    assert!(h.sink_frames() > frames + 9600);
    // A start after a stop works.
    h.start(&system(), 10);
    assert_eq!(kinds(&h.wait_events(1, 2000)), vec![STARTED]);
}

#[test]
fn screen_audio_capture_stop_reports_a_backend_that_could_not_stop() {
    let h = capture();
    h.start(&system(), 1);
    assert_eq!(kinds(&h.wait_events(1, 2000)), vec![STARTED]);
    h.script_stop(false, 0);
    h.stop(4);
    let events = h.wait_events(1, 2000);
    assert_eq!(kinds(&events), vec![STOPPED]);
    assert_eq!(events[0].async_id, 4);
    assert_eq!(events[0].error, 8);
    assert!(events[0].message.contains("did not stop"), "{}", events[0].message);
    let stats = h.stats();
    assert_eq!(stats.state, 5);
    assert_eq!(stats.last_error, 8);
    // A stop that worked reports no error.
    h.start(&system(), 5);
    assert_eq!(kinds(&h.wait_events(1, 2000)), vec![STARTED]);
    h.stop(6);
    let events = h.wait_events(1, 2000);
    assert_eq!(kinds(&events), vec![STOPPED]);
    assert_eq!(events[0].error, 0);
    assert!(events[0].message.is_empty());
}

#[test]
fn screen_audio_capture_shutdown_never_waits_for_the_backend() {
    let h = capture();
    h.start(&system(), 1);
    assert_eq!(kinds(&h.wait_events(1, 2000)), vec![STARTED]);
    h.script_stop(true, 800);
    let took_us = h.shutdown(false);
    assert!(took_us < 100_000, "shutdown took {took_us} us");
    // Detached at once: the source's own (empty) queue plays.
    let loud = h.sink_loud_frames();
    std::thread::sleep(Duration::from_millis(200));
    assert!(h.sink_loud_frames() <= loud + 960);
    assert!(h.source_accepts_capture());
    assert!(wait_for(Duration::from_secs(3), || h.backend_stops() == 1));
}

#[test]
fn screen_audio_capture_refuses_a_second_capture_and_host_frames() {
    let h = capture();
    assert!(!h.source_accepts_capture());
    assert_eq!(h.second_capture_error(), "the audio source already has a screen-audio capture");
    h.shutdown(true);
    assert!(h.source_accepts_capture());
}

#[test]
fn screen_audio_capture_rejects_sources_it_cannot_feed() {
    assert_eq!(
        capture_error_for_source(48_000, 2, 0),
        "the audio source has no queue (queue_size_ms 0)"
    );
    assert_eq!(
        capture_error_for_source(16_000, 1, 1000),
        "the audio source must be 48 kHz with 1 or 2 channels"
    );
    assert_eq!(
        capture_error_for_source(48_000, 4, 1000),
        "the audio source must be 48 kHz with 1 or 2 channels"
    );
    assert_eq!(capture_error_for_source(48_000, 1, 1000), "");
    assert_eq!(capture_error_for_source(48_000, 2, 200), "");
}

#[test]
fn screen_audio_capture_survives_a_backend_that_cannot_stop() {
    let h = capture();
    h.start(&system(), 1);
    assert_eq!(kinds(&h.wait_events(1, 2000)), vec![STARTED]);
    h.script_stop(false, 0);
    h.fail_running(4);
    let events = h.wait_events(2, 3000);
    assert_eq!(kinds(&events), vec![FAILED, STARTED]);
    let loud = h.sink_loud_frames();
    assert!(wait_for(Duration::from_secs(2), || h.sink_loud_frames() > loud + 9600));
    assert_eq!(h.backend_starts(), 2);
}

#[test]
fn screen_audio_capture_converts_a_44100_mono_backend() {
    let h = new_root_screen_audio_capture_harness(1, 5);
    h.script_start(0, 44_100, 1, 2);
    h.start(&RootScreenAudioTargetSpec { kind: 2, window_id: 42, ..Default::default() }, 5);
    let events = h.wait_events(1, 2000);
    assert_eq!(kinds(&events), vec![STARTED]);
    assert_eq!(events[0].sample_rate, 44_100);
    assert_eq!(events[0].channels, 1);
    assert_eq!(events[0].scope, 2);
    assert_eq!(h.last_target_kind(), 2);
    assert!(wait_for(Duration::from_secs(3), || h.sink_loud_frames() > 24_000));
    let stats = h.stats();
    assert_eq!(stats.capture_rate, 44_100);
    assert_eq!(stats.capture_channels, 1);
    assert_eq!(stats.scope, 2);
}

// The parent chain the Windows backend checks a target against (a window of
// File Explorer belongs to explorer.exe, which started Root).

fn ancestors(self_pid: u32, rows: &[(u32, u32, u64)]) -> Vec<u32> {
    let pids: Vec<u32> = rows.iter().map(|r| r.0).collect();
    let parents: Vec<u32> = rows.iter().map(|r| r.1).collect();
    let created: Vec<u64> = rows.iter().map(|r| r.2).collect();
    screen_audio_ancestors(self_pid, &pids, &parents, &created)
}

#[test]
fn screen_audio_ancestors_follow_the_parent_chain() {
    // winlogon 500 -> userinit 600 (exited) ... explorer 700 -> launcher 800 -> Root 900.
    let rows = [(500, 4, 10), (700, 600, 30), (800, 700, 40), (900, 800, 50), (950, 900, 60)];
    assert_eq!(ancestors(900, &rows), vec![800, 700]);
    // Root's own child is not an ancestor.
    assert_eq!(ancestors(950, &rows), vec![900, 800, 700]);
    // Unknown creation times do not break the chain.
    let unknown = [(700, 1, 0), (900, 700, 50)];
    assert_eq!(ancestors(900, &unknown), vec![700]);
    // A process that is not listed has no chain.
    assert!(ancestors(42, &rows).is_empty());
}

#[test]
fn screen_audio_ancestors_stop_at_a_reused_pid_and_a_cycle() {
    // Root's parent exited and its pid now belongs to a newer process.
    let reused = [(700, 1, 30), (800, 700, 90), (900, 800, 50)];
    assert!(ancestors(900, &reused).is_empty());
    let cycle = [(1, 2, 0), (2, 1, 0), (900, 1, 0)];
    assert_eq!(ancestors(900, &cycle), vec![1, 2]);
    let to_self = [(900, 900, 0)];
    assert!(ancestors(900, &to_self).is_empty());
}
