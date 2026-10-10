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

fn enabled() -> UniquePtr<RootMonitorHarness> {
    let h = new_root_monitor_harness();
    h.set_enabled(true);
    h
}

/// Frame `index` of a stream of distinct integer samples at 48 kHz.
fn numbered(index: usize) -> Vec<f32> {
    (0..480).map(|i| ((index * 480 + i) % 20_000) as f32 - 10_000.0).collect()
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
    assert_eq!(h.mix(48_000, 1, &base), base, "one frame queued is still priming");
    h.push(48_000, &numbered(1));
    let played = h.mix(48_000, 1, &silence(48_000, 1));
    assert_eq!(played, as_i16(&numbered(0)));
    assert_eq!(stats(&h), RootMonitorStats { active: true, frames: 1, dropped_frames: 0 });
}

#[test]
fn root_monitor_passes_48_khz_through_to_every_channel_on_top_of_the_mix() {
    let h = enabled();
    h.push(48_000, &numbered(0));
    for index in 1..50 {
        h.push(48_000, &numbered(index));
        let base: Vec<i16> = (0..960).map(|i| (i % 7) as i16 - 3).collect();
        let played = h.mix(48_000, 2, &base);
        let expected = as_i16(&numbered(index - 1));
        for (i, pair) in played.chunks(2).enumerate() {
            assert_eq!(i32::from(pair[0]), i32::from(expected[i]) + i32::from(base[2 * i]));
            assert_eq!(i32::from(pair[1]), i32::from(expected[i]) + i32::from(base[2 * i + 1]));
        }
    }
    assert_eq!(stats(&h).frames, 49);
    assert_eq!(stats(&h).dropped_frames, 0);
}

/// Plays one second of a 1 kHz sine at `capture` through the monitor into
/// `render` frames, one capture frame per render frame, and checks every
/// sample against the sine at the render rate. The first render output is
/// the first captured sample.
fn converts(capture: u32, render: u32) {
    let h = enabled();
    let mut sine = Sine::new(capture, 1000.0, 8000.0);
    let reference = Sine::new(render, 1000.0, 8000.0);
    h.push(capture as i32, &sine.frame());
    let mut output = Vec::new();
    for _ in 0..100 {
        h.push(capture as i32, &sine.frame());
        output.extend(h.mix(render, 1, &silence(render, 1)));
    }
    assert_eq!(stats(&h).dropped_frames, 0, "{capture} -> {render}");
    assert_eq!(stats(&h).frames, 100, "{capture} -> {render}");
    let mut worst = 0.0f64;
    for (j, &sample) in output.iter().enumerate().skip(4) {
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

#[test]
fn root_monitor_counts_an_underrun_once_and_primes_again() {
    let h = enabled();
    h.push(48_000, &numbered(0));
    h.push(48_000, &numbered(1));
    assert_eq!(h.mix(48_000, 1, &silence(48_000, 1)), as_i16(&numbered(0)));
    let partial = h.mix(48_000, 1, &silence(48_000, 1));
    assert_eq!(&partial[..478], &as_i16(&numbered(1))[..478], "plays what it has");
    assert_eq!(stats(&h).dropped_frames, 1);
    assert_eq!(h.mix(48_000, 1, &silence(48_000, 1)), silence(48_000, 1));
    assert_eq!(stats(&h).dropped_frames, 1, "priming again is no drop");

    h.push(48_000, &numbered(2));
    assert_eq!(h.mix(48_000, 1, &silence(48_000, 1)), silence(48_000, 1));
    h.push(48_000, &numbered(3));
    assert_eq!(h.mix(48_000, 1, &silence(48_000, 1)), as_i16(&numbered(2)));
    assert_eq!(stats(&h), RootMonitorStats { active: true, frames: 2, dropped_frames: 1 });
}

#[test]
fn root_monitor_drops_capture_frames_beyond_40_ms_while_playing() {
    let h = enabled();
    h.push(48_000, &numbered(0));
    h.push(48_000, &numbered(1));
    h.mix(48_000, 1, &silence(48_000, 1));
    for index in 2..7 {
        h.push(48_000, &numbered(index));
    }
    // Frame 1 is being read; 2, 3 and 4 make four queued, 5 and 6 are dropped.
    assert_eq!(stats(&h).dropped_frames, 2);
    for index in 1..5 {
        let played = h.mix(48_000, 1, &silence(48_000, 1));
        assert_eq!(&played[..478], &as_i16(&numbered(index))[..478], "frame {index}");
    }
}

#[test]
fn root_monitor_starts_playing_at_20_ms_however_much_waited() {
    let h = enabled();
    for index in 0..6 {
        h.push(48_000, &numbered(index));
    }
    assert_eq!(stats(&h).dropped_frames, 0, "nothing played yet, so nothing was lost");
    // Frames 0-3 were kept and 4 and 5 found the ring full; playback starts
    // with the newest two of those.
    assert_eq!(h.mix(48_000, 1, &silence(48_000, 1)), as_i16(&numbered(2)));
}

#[test]
fn root_monitor_enabling_again_starts_the_stream_over() {
    let h = enabled();
    h.push(48_000, &numbered(0));
    h.push(48_000, &numbered(1));
    h.mix(48_000, 1, &silence(48_000, 1));
    h.push(48_000, &numbered(2));

    h.set_enabled(false);
    assert_eq!(h.mix(48_000, 1, &silence(48_000, 1)), silence(48_000, 1));
    h.push(48_000, &numbered(3));
    assert!(!stats(&h).active);

    h.set_enabled(true);
    h.push(48_000, &numbered(10));
    assert_eq!(h.mix(48_000, 1, &silence(48_000, 1)), silence(48_000, 1), "old frames dropped");
    h.push(48_000, &numbered(11));
    assert_eq!(h.mix(48_000, 1, &silence(48_000, 1)), as_i16(&numbered(10)));
}

#[test]
fn root_monitor_saturates_and_plays_non_finite_samples_as_silence() {
    let h = enabled();
    let mut loud = vec![30_000.0f32; 480];
    loud[0] = f32::NAN;
    loud[1] = f32::INFINITY;
    loud[2] = -40_000.0;
    h.push(48_000, &loud);
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
    assert_eq!(h.mix(48_000, 1, &silence(48_000, 1)), silence(48_000, 1));
    assert_eq!(stats(&h), RootMonitorStats { active: true, frames: 0, dropped_frames: 0 });
}
