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

//! The mic test monitor's ring and rate conversion (`RootAudioMonitor`),
//! driven as the capture post-processor and the render callback drive it.

use cxx::UniquePtr;

use crate::audio_device_controller::ffi::{
    new_root_monitor_harness, RootMonitorHarness, RootMonitorStats,
};

/// The fade after priming and before an underrun or a skip, at 48 kHz (2.5 ms).
const FADE: usize = 120;

/// A sine of `amplitude` at `frequency`, continuous across 10 ms frames.
struct Sine {
    rate: u32,
    frequency: f64,
    amplitude: f64,
    position: u64,
}

impl Sine {
    fn new(rate: u32, frequency: f64, amplitude: f64) -> Self {
        Self { rate, frequency, amplitude, position: 0 }
    }

    fn at(&self, seconds: f64) -> f64 {
        self.amplitude * (2.0 * std::f64::consts::PI * self.frequency * seconds).sin()
    }

    fn frame(&mut self) -> Vec<f32> {
        let frames = (self.rate / 100) as u64;
        let start = self.position;
        self.position += frames;
        (start..start + frames).map(|i| self.at(i as f64 / f64::from(self.rate)) as f32).collect()
    }
}

/// A small deterministic generator for the jitter scenarios.
struct Lcg(u64);

impl Lcg {
    fn chance(&mut self, percent: u64) -> bool {
        self.0 =
            self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % 100 < percent
    }
}

fn enabled() -> UniquePtr<RootMonitorHarness> {
    let h = new_root_monitor_harness();
    h.set_enabled(true);
    h
}

/// Frame `index` of a stream of distinct integer samples at 48 kHz.
fn numbered(index: usize) -> Vec<f32> {
    (0..480).map(|i| ((index * 480 + i) % 20_000) as f32 - 10_000.0).collect()
}

/// A 48 kHz frame of one value.
fn level(value: f32) -> Vec<f32> {
    vec![value; 480]
}

fn as_i16(frame: &[f32]) -> Vec<i16> {
    frame.iter().map(|&s| s as i16).collect()
}

fn silence(rate: u32, channels: usize) -> Vec<i16> {
    vec![0; (rate / 100) as usize * channels]
}

fn stats(h: &RootMonitorHarness) -> RootMonitorStats {
    h.stats()
}

fn mix48(h: &RootMonitorHarness) -> Vec<i16> {
    h.mix(48_000, 1, &silence(48_000, 1))
}

/// `played` is `expected` faded in over its first FADE samples.
fn assert_faded_in(played: &[i16], expected: &[i16]) {
    assert_eq!(played.len(), expected.len());
    for k in 0..FADE {
        let (p, e) = (i32::from(played[k]), i32::from(expected[k]));
        assert!(p.abs() <= e.abs() && p * e >= 0, "sample {k}: {p} is not {e} faded in");
    }
    assert!(i32::from(played[0]).abs() <= i32::from(expected[0]).abs() / 100 + 1, "starts near 0");
    assert_eq!(&played[FADE..], &expected[FADE..]);
}

#[test]
fn root_monitor_plays_nothing_while_disabled_or_priming() {
    let h = new_root_monitor_harness();
    h.push(48_000, &numbered(0));
    h.push(48_000, &numbered(1));
    let base: Vec<i16> = (0..480).map(|i| i as i16).collect();
    assert_eq!(h.mix(48_000, 1, &base), base, "disabled");
    assert_eq!(stats(&h), RootMonitorStats { active: false, frames: 0, dropped_frames: 0 });

    h.set_enabled(true);
    h.push(48_000, &numbered(0));
    h.push(48_000, &numbered(1));
    assert_eq!(h.mix(48_000, 1, &base), base, "two frames queued is still priming");
    h.push(48_000, &numbered(2));
    assert_faded_in(&mix48(&h), &as_i16(&numbered(0)));
    assert_eq!(stats(&h), RootMonitorStats { active: true, frames: 1, dropped_frames: 0 });
}

#[test]
fn root_monitor_passes_48_khz_through_to_every_channel_on_top_of_the_mix() {
    let h = enabled();
    h.push(48_000, &numbered(0));
    h.push(48_000, &numbered(1));
    for index in 2..52 {
        h.push(48_000, &numbered(index));
        let base: Vec<i16> = (0..960).map(|i| (i % 7) as i16 - 3).collect();
        let played = h.mix(48_000, 2, &base);
        let expected = as_i16(&numbered(index - 2));
        let from = if index == 2 { FADE } else { 0 };
        for (i, pair) in played.chunks(2).enumerate().skip(from) {
            assert_eq!(i32::from(pair[0]), i32::from(expected[i]) + i32::from(base[2 * i]));
            assert_eq!(i32::from(pair[1]), i32::from(expected[i]) + i32::from(base[2 * i + 1]));
        }
    }
    assert_eq!(stats(&h).frames, 50);
    assert_eq!(stats(&h).dropped_frames, 0);
}

/// Plays one second of a 1 kHz sine at `capture` through the monitor into
/// `render` frames, one capture frame per render frame, and checks every
/// sample after the fade-in against the sine at the render rate. The first
/// render output is the first captured sample.
fn converts(capture: u32, render: u32) {
    let h = enabled();
    let mut sine = Sine::new(capture, 1000.0, 8000.0);
    let reference = Sine::new(render, 1000.0, 8000.0);
    h.push(capture as i32, &sine.frame());
    h.push(capture as i32, &sine.frame());
    let mut output = Vec::new();
    for _ in 0..100 {
        h.push(capture as i32, &sine.frame());
        output.extend(h.mix(render, 1, &silence(render, 1)));
    }
    assert_eq!(stats(&h).dropped_frames, 0, "{capture} -> {render}");
    assert_eq!(stats(&h).frames, 100, "{capture} -> {render}");
    let mut worst = 0.0f64;
    for (j, &sample) in output.iter().enumerate().skip((render / 400) as usize + 4) {
        let expected = reference.at(j as f64 / f64::from(render));
        worst = worst.max((f64::from(sample) - expected).abs());
    }
    // Catmull-Rom on a 1 kHz sine: well under 1 % of the amplitude.
    assert!(worst < 80.0, "{capture} -> {render}: worst error {worst}");
}

#[test]
fn root_monitor_converts_16_khz_capture_to_48_khz_playout() {
    converts(16_000, 48_000);
}

#[test]
fn root_monitor_converts_48_khz_capture_to_44_1_khz_playout() {
    converts(48_000, 44_100);
}

#[test]
fn root_monitor_converts_32_khz_capture_to_48_khz_and_48_to_16_khz() {
    converts(32_000, 48_000);
    converts(48_000, 16_000);
}

/// The processor's rate moves (the device changed format): playback goes on
/// without a gap, a few samples late at most, at the new rate.
#[test]
fn root_monitor_follows_a_capture_rate_change_mid_stream() {
    let h = enabled();
    let mut before = Sine::new(48_000, 1000.0, 8000.0);
    // Continues where 50 frames at 48 kHz end (0.5 s), 500 frames per period.
    let mut after = Sine::new(16_000, 1000.0, 8000.0);
    after.position = 8_000;
    let reference = Sine::new(48_000, 1000.0, 8000.0);
    h.push(48_000, &before.frame());
    h.push(48_000, &before.frame());
    let mut output = Vec::new();
    for period in 0..100 {
        if period < 48 {
            h.push(48_000, &before.frame());
        } else {
            h.push(16_000, &after.frame());
        }
        output.extend(mix48(&h));
    }
    assert_eq!(stats(&h), RootMonitorStats { active: true, frames: 100, dropped_frames: 0 });

    let error = |range: std::ops::Range<usize>, lag: usize| {
        range
            .map(|j| (f64::from(output[j]) - reference.at((j - lag) as f64 / 48_000.0)).abs())
            .fold(0.0, f64::max)
    };
    assert!(error(FADE + 4..23_990, 0) < 80.0, "48 kHz before the change");
    let switch = 24_000;
    let (lag, worst) = (0..=8)
        .map(|lag| (lag, error(switch + 64..output.len(), lag)))
        .fold((0, f64::MAX), |best, next| if next.1 < best.1 { next } else { best });
    assert!(lag <= 6 && worst < 80.0, "16 kHz after the change: lag {lag}, worst error {worst}");
    assert!(
        output[switch - 16..switch + 64].iter().all(|&s| s.unsigned_abs() <= 9_000),
        "no burst at the change"
    );
}

#[test]
fn root_monitor_counts_an_underrun_once_fades_out_and_primes_again_with_more() {
    let h = enabled();
    for index in 0..3 {
        h.push(48_000, &numbered(index));
    }
    assert_faded_in(&mix48(&h), &as_i16(&numbered(0)));
    assert_eq!(mix48(&h), as_i16(&numbered(1)));
    // Frame 2 runs out two samples early (the interpolation reads ahead).
    let partial = mix48(&h);
    let expected = as_i16(&numbered(2));
    assert_eq!(&partial[..478 - FADE], &expected[..478 - FADE], "plays what it has");
    for k in 478 - FADE..478 {
        let (p, e) = (i32::from(partial[k]), i32::from(expected[k]));
        assert!(p.abs() <= e.abs() && p * e >= 0, "sample {k}: fades out");
    }
    assert!(i32::from(partial[477]).abs() <= i32::from(expected[477]).abs() / 100 + 1);
    assert!(partial[478..].iter().all(|&s| s == 0));
    assert_eq!(stats(&h).dropped_frames, 1);
    assert_eq!(mix48(&h), silence(48_000, 1));
    assert_eq!(stats(&h).dropped_frames, 1, "priming again is no drop");

    // One more frame before it plays again.
    for index in 3..6 {
        h.push(48_000, &numbered(index));
    }
    assert_eq!(mix48(&h), silence(48_000, 1), "three frames no longer prime it");
    h.push(48_000, &numbered(6));
    assert_faded_in(&mix48(&h), &as_i16(&numbered(3)));
    assert_eq!(stats(&h), RootMonitorStats { active: true, frames: 3, dropped_frames: 1 });
}

#[test]
fn root_monitor_starts_playing_at_30_ms_however_much_waited() {
    let h = enabled();
    for index in 0..6 {
        h.push(48_000, &numbered(index));
    }
    assert_faded_in(&mix48(&h), &as_i16(&numbered(3)));

    let h = enabled();
    for index in 0..20 {
        h.push(48_000, &numbered(index));
    }
    // The ring kept frames 0-15; nothing played yet, so nothing was lost.
    assert_eq!(stats(&h).dropped_frames, 0);
    assert_faded_in(&mix48(&h), &as_i16(&numbered(13)));
}

#[test]
fn root_monitor_enabling_again_starts_the_stream_over() {
    let h = enabled();
    for index in 0..3 {
        h.push(48_000, &numbered(index));
    }
    mix48(&h);
    h.push(48_000, &numbered(3));

    h.set_enabled(false);
    assert_eq!(mix48(&h), silence(48_000, 1));
    h.push(48_000, &numbered(4));
    assert!(!stats(&h).active);

    h.set_enabled(true);
    h.push(48_000, &numbered(10));
    h.push(48_000, &numbered(11));
    assert_eq!(mix48(&h), silence(48_000, 1), "old frames dropped");
    h.push(48_000, &numbered(12));
    assert_faded_in(&mix48(&h), &as_i16(&numbered(10)));
}

/// AdmProxy resyncs around a device stop or start: frames captured while
/// playout restarted are stale and never played.
#[test]
fn root_monitor_resync_skips_what_queued_before_it() {
    let h = enabled();
    h.push(48_000, &level(1.0));
    h.push(48_000, &level(2.0));
    for index in 3..10 {
        h.push(48_000, &level(index as f32));
        let played = mix48(&h);
        assert_eq!(played[FADE..], vec![index as i16 - 2; 480 - FADE][..]);
    }
    for _ in 0..5 {
        h.push(48_000, &level(500.0));
    }
    h.resync();
    h.push(48_000, &level(1000.0));
    assert_eq!(mix48(&h), silence(48_000, 1));
    h.push(48_000, &level(1000.0));
    assert_eq!(mix48(&h), silence(48_000, 1));
    h.push(48_000, &level(1000.0));
    let played = mix48(&h);
    assert!(played.iter().all(|&s| s > 0 && s <= 1000) && played[479] == 1000);
    assert_eq!(stats(&h).dropped_frames, 0);
}

/// The render side stopped while capture went on: what queued meanwhile is
/// late, and what found the ring full is no loss of the render side's.
#[test]
fn root_monitor_a_render_stall_drops_the_late_frames_uncounted() {
    let h = enabled();
    h.push(48_000, &level(1.0));
    h.push(48_000, &level(2.0));
    for index in 3..10 {
        h.push(48_000, &level(index as f32));
        mix48(&h);
    }
    for _ in 0..30 {
        h.push(48_000, &level(500.0));
    }
    assert_eq!(stats(&h).dropped_frames, 0, "the render side was not running");
    assert_eq!(mix48(&h), silence(48_000, 1), "the late frames are dropped");
    for _ in 0..3 {
        h.push(48_000, &level(1000.0));
    }
    let played = mix48(&h);
    assert!(played.iter().all(|&s| s > 0 && s <= 1000) && played[479] == 1000);
    assert_eq!(stats(&h).dropped_frames, 0);
}

#[test]
fn root_monitor_counts_frames_lost_while_the_render_side_runs() {
    let h = enabled();
    for index in 0..20 {
        h.push(48_000, &numbered(index));
    }
    assert_eq!(stats(&h).dropped_frames, 0);
    // A render side that runs but cannot play (a callback larger than the
    // monitor renders) leaves the ring full: every frame now is lost.
    for index in 20..25 {
        assert_eq!(h.mix(400_000, 1, &[]), silence(400_000, 1));
        h.push(48_000, &numbered(index));
    }
    assert_eq!(stats(&h).dropped_frames, 5);
}

/// Latency that piled up (the render side late by 50 ms, capture slightly
/// faster) is skipped once it stayed above the target for a second.
#[test]
fn root_monitor_trims_latency_that_stays_above_the_target() {
    let h = enabled();
    h.push(48_000, &level(0.0));
    h.push(48_000, &level(1.0));
    let mut newest: i32 = 1;
    let delay = |h: &RootMonitorHarness, newest: i32| newest - i32::from(mix48(h)[300]);
    for _ in 0..50 {
        newest += 1;
        h.push(48_000, &level(newest as f32));
        assert_eq!(delay(&h, newest), 2);
    }
    for _ in 0..5 {
        newest += 1;
        h.push(48_000, &level(newest as f32));
    }
    newest += 1;
    h.push(48_000, &level(newest as f32));
    assert_eq!(delay(&h, newest), 7, "50 ms late now");
    let mut callbacks = 0;
    while {
        newest += 1;
        h.push(48_000, &level(newest as f32));
        delay(&h, newest) != 2
    } {
        callbacks += 1;
        assert!(callbacks < 250, "still late after {callbacks} callbacks");
    }
    assert!(callbacks >= 98, "trimmed after {callbacks} callbacks, not at once");
    assert_eq!(stats(&h).dropped_frames, 5, "the skipped frames");
    for _ in 0..300 {
        newest += 1;
        h.push(48_000, &level(newest as f32));
        assert_eq!(delay(&h, newest), 2);
    }
    assert_eq!(stats(&h).dropped_frames, 5);
}

/// Drives `periods` 10 ms periods of constant capture through the monitor;
/// `capture_first(period)` says whether the period's capture frame comes
/// before its render callback. Asserts that after the first second nothing
/// is dropped and every render callback plays the capture in full.
fn steady_after_one_second(
    name: &str,
    capture: u32,
    render: u32,
    periods: usize,
    mut capture_first: impl FnMut(usize) -> bool,
) {
    let h = enabled();
    let frame = vec![1000.0f32; (capture / 100) as usize];
    let full = vec![1000i16; (render / 100) as usize];
    let mut dropped_at_1s = 0;
    for period in 0..periods {
        if period == 100 {
            dropped_at_1s = stats(&h).dropped_frames;
        }
        let first = capture_first(period);
        if first {
            h.push(capture as i32, &frame);
        }
        let played = h.mix(render, 1, &silence(render, 1));
        if period >= 100 {
            assert_eq!(played, full, "{name}: period {period} did not play in full");
        }
        if !first {
            h.push(capture as i32, &frame);
        }
    }
    let dropped = stats(&h).dropped_frames;
    assert_eq!(dropped, dropped_at_1s, "{name}: dropped after the first second");
    assert!(dropped_at_1s <= 1, "{name}: {dropped_at_1s} dropped in the first second");
}

#[test]
fn root_monitor_plays_through_callback_order_jitter() {
    let mut lcg = Lcg(1234);
    steady_after_one_second("order random each period", 48_000, 48_000, 1000, |_| lcg.chance(50));
    let mut lcg = Lcg(99);
    steady_after_one_second("render first 5 %", 48_000, 48_000, 1000, |_| !lcg.chance(5));
    steady_after_one_second("render always first", 48_000, 48_000, 1000, |_| false);
    let mut lcg = Lcg(7);
    steady_after_one_second("48 -> 44.1 kHz, random", 48_000, 44_100, 1000, |_| lcg.chance(50));
    let mut lcg = Lcg(8);
    steady_after_one_second("16 -> 48 kHz, random", 16_000, 48_000, 1000, |_| lcg.chance(50));
    let mut lcg = Lcg(9);
    steady_after_one_second("48 -> 16 kHz, random", 48_000, 16_000, 1000, |_| lcg.chance(50));
}

/// Capture and render each run in bursts of `burst` 10 ms callbacks every
/// burst x 10 ms (a PipeWire quantum of 1024 frames is about two), the
/// render bursts `offset` periods after the capture bursts.
fn bursts(burst: usize, offset: usize) {
    let name = format!("bursts of {burst}, render offset {offset}");
    let h = enabled();
    let frame = level(1000.0);
    let full = vec![1000i16; 480];
    let mut dropped_at_1s = 0;
    for tick in 0..1000 {
        if tick == 100 {
            dropped_at_1s = stats(&h).dropped_frames;
        }
        if tick % burst == 0 {
            for _ in 0..burst {
                h.push(48_000, &frame);
            }
        }
        if (tick + burst - offset) % burst == 0 {
            for _ in 0..burst {
                let played = mix48(&h);
                if tick >= 100 {
                    assert_eq!(played, full, "{name}: tick {tick} did not play in full");
                }
            }
        }
    }
    assert_eq!(stats(&h).dropped_frames, dropped_at_1s, "{name}: dropped after the first second");
    assert!(dropped_at_1s <= 3, "{name}: {dropped_at_1s} dropped in the first second");
}

#[test]
fn root_monitor_plays_through_bursts_of_two_three_and_four_callbacks() {
    for burst in 2..=4 {
        for offset in 0..burst {
            bursts(burst, offset);
        }
    }
}

#[test]
fn root_monitor_saturates_and_plays_non_finite_samples_as_silence() {
    let h = enabled();
    let loud = vec![30_000.0f32; 480];
    let mut special = loud.clone();
    special[0] = f32::NAN;
    special[1] = f32::INFINITY;
    special[2] = -40_000.0;
    h.push(48_000, &loud);
    h.push(48_000, &special);
    h.push(48_000, &loud);
    h.mix(48_000, 1, &vec![10_000i16; 480]);
    h.push(48_000, &loud);
    let played = h.mix(48_000, 1, &vec![10_000i16; 480]);
    assert_eq!(&played[..3], &[10_000, 10_000, -22_768]);
    assert!(played[3..].iter().all(|&s| s == 32_767));
}

#[test]
fn root_monitor_ignores_frames_it_cannot_hold() {
    let h = enabled();
    h.push(48_000, &vec![1000.0; 481]);
    h.push(96_000, &vec![1000.0; 960]);
    h.push(4_000, &vec![1000.0; 40]);
    h.push(48_000, &[]);
    assert_eq!(mix48(&h), silence(48_000, 1));
    assert_eq!(stats(&h), RootMonitorStats { active: true, frames: 0, dropped_frames: 0 });
}
