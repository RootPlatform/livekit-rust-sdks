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

//! Test hooks for the factory's playout mixer (`RootAudioMixer`), which is only
//! reachable through libwebrtc otherwise.

#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootMixTestSource {
        pub channels: usize,
        pub left: i16,
        pub right: i16,
        /// 0 normal, 1 muted, 2 error.
        pub info: u8,
        pub ssrc: u32,
        pub timestamp: u32,
        pub elapsed_time_ms: i64,
        pub ntp_time_ms: i64,
        pub preferred_rate: i32,
        /// Mixes again from inside its first pull, as a second render thread would.
        pub reenter: bool,
    }

    #[derive(Debug, Clone, Default)]
    pub struct RootMixTestResult {
        pub sample_rate: i32,
        pub samples_per_channel: usize,
        pub num_channels: usize,
        pub timestamp: u32,
        pub elapsed_time_ms: i64,
        pub ntp_time_ms: i64,
        pub muted: bool,
        pub data: Vec<i16>,
        pub sources: u32,
        pub ssrcs: Vec<u32>,
        pub mix_calls: u64,
        pub out_samples: u64,
        pub out_sumsq: f64,
        pub overlapping_mixes: u64,
        pub nested_mix_silent: bool,
    }

    #[derive(Debug, Clone, Default)]
    pub struct RootMixFrame {
        pub num_channels: usize,
        pub muted: bool,
        pub data: Vec<i16>,
    }

    /// One source's level as `RootAudioMixer::ReadSourceLevels` reports it.
    #[derive(Debug, Clone, Copy, Default, PartialEq)]
    pub struct RootMixLevel {
        pub ssrc: u32,
        pub source_id: u64,
        pub sumsq: f64,
        pub samples: u64,
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootMixStress {
        pub mix_calls: u64,
        pub overlapping_mixes: u64,
        /// Mixed frames that were not 10 ms of stereo at 48 kHz.
        pub bad_frames: u64,
        pub sources: u32,
    }

    unsafe extern "C++" {
        include!("livekit/root_audio_mixer.h");

        type RootMixerHarness;

        fn root_audio_soft_clip(sample: f32) -> f32;
        fn root_audio_volume_to_gain(linear_volume: f32) -> f32;

        /// Adds `sources`, removes the ones at the `remove` indices, then mixes
        /// once at `out_channels`.
        fn root_audio_mixer_run_for_test(
            sources: &[RootMixTestSource],
            remove: &[usize],
            out_channels: usize,
        ) -> RootMixTestResult;

        /// A mixer driven one Mix at a time (tests only; no device).
        fn new_root_mixer_harness() -> UniquePtr<RootMixerHarness>;
        fn add_source(self: &RootMixerHarness, spec: &RootMixTestSource) -> usize;
        fn remove_source(self: &RootMixerHarness, index: usize);
        fn set_source_value(self: &RootMixerHarness, index: usize, left: i16, right: i16);
        fn set_platform_active(self: &RootMixerHarness, active: bool);
        fn set_master_volume(self: &RootMixerHarness, volume: f32);
        fn set_deafened(self: &RootMixerHarness, deafened: bool);
        fn set_source_gain(self: &RootMixerHarness, ssrc: u32, tile: f32, paused: bool) -> bool;
        fn clear_source_gain(self: &RootMixerHarness, ssrc: u32);
        fn register_effect(self: &RootMixerHarness, id: u32, samples: &[i16]) -> bool;
        /// 0 queued, 1 unknown clip, 2 dropped (queue full), 3 idle (no platform audio).
        fn play_effect(self: &RootMixerHarness, id: u32, gain: f32) -> u8;
        fn stop_effects(self: &RootMixerHarness);
        fn reset_controls(self: &RootMixerHarness);
        fn mix(self: &RootMixerHarness, out_channels: usize) -> RootMixFrame;
        fn effects_played(self: &RootMixerHarness) -> u64;
        fn effects_dropped(self: &RootMixerHarness) -> u64;
        fn sources_added(self: &RootMixerHarness) -> u64;
        fn source_levels(self: &RootMixerHarness) -> Vec<RootMixLevel>;
        fn set_timeouts(
            self: &RootMixerHarness,
            unconfigured_hold_ms: i64,
            max_effect_wait_ms: i64,
        );

        /// Mixes `mixes` times while other threads change every control and
        /// add and remove sources.
        fn root_audio_mixer_stress_for_test(mixes: u32) -> RootMixStress;
    }
}

#[cfg(test)]
mod tests {
    use super::ffi::{
        root_audio_mixer_run_for_test, root_audio_soft_clip, RootMixTestResult, RootMixTestSource,
    };

    fn mono(value: i16, ssrc: u32) -> RootMixTestSource {
        RootMixTestSource {
            channels: 1,
            left: value,
            ssrc,
            elapsed_time_ms: -1,
            ntp_time_ms: -1,
            preferred_rate: 48000,
            ..Default::default()
        }
    }

    fn stereo(left: i16, right: i16, ssrc: u32) -> RootMixTestSource {
        RootMixTestSource { channels: 2, right, ..mono(left, ssrc) }
    }

    fn run(sources: &[RootMixTestSource], remove: &[usize], channels: usize) -> RootMixTestResult {
        root_audio_mixer_run_for_test(sources, remove, channels)
    }

    fn assert_frame(result: &RootMixTestResult, channels: usize, expected: &[i16]) {
        assert_eq!(result.sample_rate, 48000);
        assert_eq!(result.samples_per_channel, 480);
        assert_eq!(result.num_channels, channels);
        assert_eq!(result.data.len(), 480 * channels);
        for (i, frame) in result.data.chunks(channels).enumerate() {
            assert_eq!(frame, expected, "frame {i}");
        }
    }

    #[test]
    fn root_audio_soft_clip_is_linear_up_to_the_threshold() {
        for x in [0.0f32, 0.1, -0.25, 0.5, 0.8, -0.8] {
            assert_eq!(root_audio_soft_clip(x), x);
        }
    }

    #[test]
    fn root_audio_soft_clip_bends_into_the_headroom() {
        let mut previous = 0.8f32;
        for x in [0.81f32, 0.9, 1.0, 1.5, 3.0, 100.0] {
            let y = root_audio_soft_clip(x);
            assert!(y <= 1.0, "clip({x}) = {y}");
            if x <= 1.5 {
                assert!(y > previous, "clip({x}) = {y}");
            } else {
                assert!(y >= previous, "clip({x}) = {y}");
            }
            assert_eq!(root_audio_soft_clip(-x), -y);
            previous = y;
        }
        let expected = 0.8 + 0.2 * ((1.0f32 - 0.8) / 0.2).tanh();
        assert!((root_audio_soft_clip(1.0) - expected).abs() < 1e-6);
    }

    #[test]
    fn root_audio_mixer_passes_a_single_source_through() {
        let result = run(&[mono(1000, 7)], &[], 1);
        assert_frame(&result, 1, &[1000]);
        assert!(!result.muted);
        assert_eq!(result.sources, 1);
        assert_eq!(result.ssrcs, vec![7]);
        assert_eq!(result.mix_calls, 1);
        assert_eq!(result.out_samples, 480);
        assert!((result.out_sumsq - 480.0 * 1000.0 * 1000.0).abs() < 1.0);
    }

    #[test]
    fn root_audio_mixer_maps_channels() {
        assert_frame(&run(&[mono(1000, 1)], &[], 2), 2, &[1000, 1000]);
        assert_frame(&run(&[stereo(1000, 3000, 1)], &[], 2), 2, &[1000, 3000]);
        assert_frame(&run(&[stereo(1000, 3000, 1)], &[], 1), 1, &[2000]);
        assert_frame(&run(&[stereo(1000, -3000, 1), mono(500, 2)], &[], 2), 2, &[1500, -2500]);
    }

    #[test]
    fn root_audio_mixer_sums_sources_at_unity_gain() {
        let result = run(&[mono(1000, 1), mono(2000, 2), mono(-500, 3)], &[], 1);
        assert_frame(&result, 1, &[2500]);
        assert_eq!(result.sources, 3);
    }

    #[test]
    fn root_audio_mixer_soft_clips_the_sum() {
        let result = run(&[mono(20000, 1), mono(20000, 2)], &[], 1);
        let expected = (root_audio_soft_clip(40000.0 / 32768.0) * 32768.0).round() as i16;
        assert!(expected > 26214 && expected < 32767, "{expected}");
        assert_frame(&result, 1, &[expected]);

        let result = run(&[mono(-32768, 1), mono(-32768, 2)], &[], 1);
        assert!(result.data.iter().all(|&s| s >= -32768 && s < -26214));
    }

    #[test]
    fn root_audio_mixer_skips_muted_and_failed_sources() {
        let muted = RootMixTestSource { info: 1, ..mono(9000, 2) };
        let failed = RootMixTestSource { info: 2, ..mono(9000, 3) };
        let result = run(&[mono(1000, 1), muted, failed], &[], 1);
        assert_frame(&result, 1, &[1000]);
        assert_eq!(result.sources, 3);
    }

    #[test]
    fn root_audio_mixer_is_silent_without_audio() {
        let result = run(&[], &[], 2);
        assert_frame(&result, 2, &[0, 0]);
        assert!(result.muted);
        assert_eq!(result.elapsed_time_ms, -1);
        assert_eq!(result.out_samples, 960);

        let muted = RootMixTestSource { info: 1, ..mono(9000, 2) };
        assert!(run(&[muted], &[], 1).muted);
    }

    #[test]
    fn root_audio_mixer_drops_removed_sources() {
        let result = run(&[mono(1000, 1), mono(2000, 2), mono(4000, 3)], &[1], 1);
        assert_frame(&result, 1, &[5000]);
        assert_eq!(result.sources, 2);
        assert_eq!(result.ssrcs, vec![1, 3]);
    }

    #[test]
    fn root_audio_mixer_with_every_source_removed_is_silent() {
        let result = run(&[mono(1000, 1), mono(2000, 2)], &[0, 1], 1);
        assert_frame(&result, 1, &[0]);
        assert!(result.muted);
        assert_eq!(result.sources, 0);
    }

    #[test]
    fn root_audio_mixer_asks_every_source_for_48_khz() {
        let low = RootMixTestSource { preferred_rate: 16000, ..mono(1000, 1) };
        let result = run(&[low], &[], 1);
        assert_frame(&result, 1, &[1000]);
    }

    #[test]
    fn root_audio_mixer_carries_the_timing_of_the_sources() {
        let a = RootMixTestSource {
            timestamp: 960,
            elapsed_time_ms: 40,
            ntp_time_ms: 5000,
            ..mono(1, 1)
        };
        let b = RootMixTestSource {
            timestamp: 480,
            elapsed_time_ms: 60,
            ntp_time_ms: 4000,
            ..mono(1, 2)
        };
        let result = run(&[a, b], &[], 1);
        assert_eq!(result.timestamp, 480);
        assert_eq!(result.elapsed_time_ms, 60);
        assert_eq!(result.ntp_time_ms, 4000);
    }

    #[test]
    fn root_audio_mixer_answers_an_overlapping_mix_with_silence() {
        let reentering = RootMixTestSource { reenter: true, ..mono(1000, 1) };
        let result = run(&[reentering, mono(2000, 2)], &[], 1);
        assert!(result.nested_mix_silent);
        assert_eq!(result.overlapping_mixes, 1);
        assert_eq!(result.mix_calls, 1);
        assert_frame(&result, 1, &[3000]);
        assert_eq!(run(&[mono(1000, 1)], &[], 1).overlapping_mixes, 0);
    }
}

/// The app's mixer semantics (CallAudioMixerTests, VolumeCurveTests) on the
/// native mixer while platform audio is in use.
#[cfg(test)]
mod gain_tests {
    use cxx::UniquePtr;

    use super::ffi::{
        new_root_mixer_harness, root_audio_mixer_stress_for_test, root_audio_soft_clip,
        root_audio_volume_to_gain, RootMixFrame, RootMixTestSource, RootMixerHarness,
    };

    const BLOCK: usize = 480;
    const QUEUED: u8 = 0;
    const UNKNOWN: u8 = 1;
    const DROPPED: u8 = 2;
    const IDLE: u8 = 3;

    fn s16(value: f32) -> i16 {
        (value * 32768.0).round() as i16
    }

    fn level(sample: i16) -> f32 {
        sample as f32 / 32768.0
    }

    fn mono(value: f32, ssrc: u32) -> RootMixTestSource {
        RootMixTestSource {
            channels: 1,
            left: s16(value),
            ssrc,
            elapsed_time_ms: -1,
            ntp_time_ms: -1,
            preferred_rate: 48000,
            ..Default::default()
        }
    }

    fn active() -> UniquePtr<RootMixerHarness> {
        let h = new_root_mixer_harness();
        h.set_platform_active(true);
        h
    }

    /// Past any 20 ms ramp.
    fn settle(h: &RootMixerHarness, channels: usize) -> RootMixFrame {
        h.mix(channels);
        h.mix(channels);
        h.mix(channels)
    }

    fn last(frame: &RootMixFrame) -> f32 {
        level(*frame.data.last().unwrap())
    }

    fn clip(id: u32, h: &RootMixerHarness, value: f32, samples: usize) {
        assert!(h.register_effect(id, &vec![s16(value); samples]));
    }

    #[test]
    fn root_audio_volume_curve_matches_the_app() {
        for v in [0.0f32, 0.5, 1.0, 1.5, 2.0] {
            let expected = if v <= 0.0 { 0.0 } else { (v as f64).powf(2.5) as f32 };
            assert!((root_audio_volume_to_gain(v) - expected).abs() < 1e-4, "{v}");
        }
        assert!((root_audio_volume_to_gain(0.5) - 0.1768).abs() < 1e-3);
        assert_eq!(root_audio_volume_to_gain(1.0), 1.0);
        assert!((root_audio_volume_to_gain(2.0) - 5.657).abs() < 1e-2);
        assert_eq!(root_audio_volume_to_gain(4.0), 10.0);
        assert_eq!(root_audio_volume_to_gain(-1.0), 0.0);
        assert_eq!(root_audio_volume_to_gain(f32::NAN), 0.0);
    }

    #[test]
    fn root_audio_soft_clip_matches_the_app_knee() {
        let expected = 0.8 + 0.2 * ((1.5f32 - 0.8) / 0.2).tanh();
        assert!((root_audio_soft_clip(1.5) - expected).abs() < 1e-5);
        assert!((root_audio_soft_clip(-1.5) + expected).abs() < 1e-5);
        let mut previous = root_audio_soft_clip(-5.0);
        let mut x = -5.0f32;
        while x <= 5.0 {
            let y = root_audio_soft_clip(x);
            assert!(y >= previous - 1e-6 && (-1.0..=1.0).contains(&y), "clip({x}) = {y}");
            previous = y;
            x += 0.01;
        }
    }

    #[test]
    fn root_audio_mixer_plays_an_unknown_source_at_unity() {
        let h = active();
        h.add_source(&mono(0.5, 7));
        let frame = h.mix(1);
        assert_eq!(frame.data, vec![s16(0.5); BLOCK]);
    }

    #[test]
    fn root_audio_mixer_tile_volume_follows_the_curve() {
        let h = active();
        h.add_source(&mono(0.5, 7));
        assert!(h.set_source_gain(7, 0.5, false));
        let expected = 0.5 * root_audio_volume_to_gain(0.5);
        assert!((last(&settle(&h, 1)) - expected).abs() < 1e-4);
    }

    #[test]
    fn root_audio_mixer_master_volume_multiplies_before_the_curve() {
        let h = active();
        h.add_source(&mono(0.4, 7));
        h.set_source_gain(7, 0.5, false);
        h.set_master_volume(2.0);
        assert_eq!(settle(&h, 1).data, vec![s16(0.4); BLOCK]);
    }

    #[test]
    fn root_audio_mixer_soft_clips_a_boosted_volume_instead_of_hard_clipping() {
        let h = active();
        h.add_source(&mono(0.5, 7));
        h.set_source_gain(7, 2.0, false);
        let value = last(&settle(&h, 1));
        assert!(value < 1.0 && value > 0.8, "{value}");
    }

    #[test]
    fn root_audio_mixer_volume_touches_only_that_source() {
        let h = active();
        h.add_source(&mono(0.3, 1));
        h.add_source(&mono(0.2, 2));
        h.set_source_gain(2, 1.0, false);
        h.set_source_gain(1, 0.0, false);
        assert_eq!(settle(&h, 1).data, vec![s16(0.2); BLOCK]);
    }

    #[test]
    fn root_audio_mixer_deafen_silences_remote_audio_but_not_effects() {
        let h = active();
        h.add_source(&mono(0.5, 7));
        h.set_deafened(true);
        clip(1, &h, 0.999, BLOCK * 10);
        assert_eq!(h.play_effect(1, 0.2), QUEUED);
        assert!((last(&settle(&h, 1)) - 0.2).abs() < 1e-3);

        h.stop_effects();
        h.set_deafened(false);
        assert!((last(&settle(&h, 1)) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn root_audio_mixer_effects_ignore_master_volume() {
        let h = active();
        h.set_master_volume(0.0);
        clip(1, &h, 0.5, BLOCK);
        assert_eq!(h.play_effect(1, 0.2), QUEUED);
        let frame = h.mix(1);
        assert!(!frame.muted);
        assert!(frame.data.iter().all(|&v| (level(v) - 0.1).abs() < 1e-4));
        assert!(h.mix(1).data.iter().all(|&v| v == 0));
        assert_eq!(h.effects_played(), 1);
    }

    #[test]
    fn root_audio_mixer_stop_effects_cuts_playing_and_queued_effects() {
        let h = active();
        clip(1, &h, 0.999, BLOCK * 10);
        h.play_effect(1, 0.2);
        h.mix(1);
        h.play_effect(1, 0.2);
        h.stop_effects();
        assert!(h.mix(1).data.iter().all(|&v| v == 0));

        clip(2, &h, 0.999, BLOCK);
        h.play_effect(2, 0.2);
        assert!(h.mix(1).data.iter().all(|&v| (level(v) - 0.2).abs() < 1e-3));
    }

    #[test]
    fn root_audio_mixer_paused_source_is_silent() {
        let h = active();
        h.add_source(&mono(0.5, 9));
        h.set_source_gain(9, 1.0, true);
        assert!(settle(&h, 1).data.iter().all(|&v| v == 0));
        h.set_source_gain(9, 1.0, false);
        assert_eq!(settle(&h, 1).data, vec![s16(0.5); BLOCK]);
    }

    #[test]
    fn root_audio_mixer_sum_is_soft_clipped() {
        let h = active();
        h.add_source(&mono(0.7, 1));
        h.add_source(&mono(0.7, 2));
        let sum = 2.0 * level(s16(0.7));
        let expected = (root_audio_soft_clip(sum) * 32768.0).round() as i16;
        assert_eq!(h.mix(1).data, vec![expected; BLOCK]);
    }

    #[test]
    fn root_audio_stereo_mixer_plays_effects_on_both_channels() {
        let h = active();
        clip(1, &h, 0.5, BLOCK);
        h.play_effect(1, 0.2);
        let frame = h.mix(2);
        assert_eq!(frame.num_channels, 2);
        assert_eq!(frame.data.len(), BLOCK * 2);
        assert!(frame.data.iter().all(|&v| (level(v) - 0.1).abs() < 1e-4));
        assert!(h.mix(2).data.iter().all(|&v| v == 0));
    }

    #[test]
    fn root_audio_mixer_ramps_a_gain_change_over_20_ms() {
        let h = active();
        h.add_source(&mono(0.5, 7));
        h.mix(1);
        h.set_source_gain(7, 0.0, false);
        let first = h.mix(1);
        let second = h.mix(1);
        let ramp: Vec<i16> = first.data.iter().chain(second.data.iter()).copied().collect();
        assert!(ramp.windows(2).all(|w| w[1] <= w[0]));
        assert!((level(first.data[BLOCK - 1]) - 0.25).abs() < 1e-3);
        assert_eq!(second.data[BLOCK - 1], 0);
        assert!(h.mix(1).data.iter().all(|&v| v == 0));
    }

    #[test]
    fn root_audio_mixer_master_and_deafen_ramp_too() {
        let h = active();
        h.add_source(&mono(0.5, 7));
        h.mix(1);
        h.set_deafened(true);
        let frame = h.mix(1);
        assert!(frame.data[0] > 0 && frame.data[BLOCK - 1] < frame.data[0]);
        assert!(settle(&h, 1).data.iter().all(|&v| v == 0));
    }

    #[test]
    fn root_audio_mixer_applies_a_gain_set_before_its_source_appears() {
        let h = active();
        assert!(h.set_source_gain(42, 0.5, false));
        h.add_source(&mono(0.5, 42));
        let expected = 0.5 * root_audio_volume_to_gain(0.5);
        assert!((level(h.mix(1).data[0]) - expected).abs() < 1e-4);
    }

    #[test]
    fn root_audio_mixer_cleared_gain_keeps_a_playing_stream_and_is_forgotten() {
        let h = active();
        h.set_timeouts(0, 1000);
        let source = h.add_source(&mono(0.5, 7));
        h.set_source_gain(7, 0.0, true);
        settle(&h, 1);
        h.clear_source_gain(7);
        assert!(settle(&h, 1).data.iter().all(|&v| v == 0), "no return to full volume");

        h.remove_source(source);
        h.add_source(&mono(0.5, 7));
        assert_eq!(settle(&h, 1).data, vec![s16(0.5); BLOCK]);
    }

    #[test]
    fn root_audio_mixer_holds_a_new_source_until_its_gain_arrives() {
        let h = active();
        h.set_timeouts(300, 1000);
        h.add_source(&mono(0.25, 1));
        assert_eq!(h.mix(1).data, vec![s16(0.25); BLOCK], "no gains in use yet");

        assert!(h.set_source_gain(1, 1.0, false));
        h.add_source(&mono(0.5, 2));
        assert_eq!(settle(&h, 1).data, vec![s16(0.25); BLOCK], "held");
        h.set_source_gain(2, 0.5, false);
        let expected = 0.25 + 0.5 * root_audio_volume_to_gain(0.5);
        assert!((last(&settle(&h, 1)) - expected).abs() < 1e-3);

        h.add_source(&mono(0.125, 3));
        assert!((last(&settle(&h, 1)) - expected).abs() < 1e-3, "held");
        std::thread::sleep(std::time::Duration::from_millis(350));
        assert!((last(&settle(&h, 1)) - (expected + 0.125)).abs() < 1e-3, "no gain came");
    }

    #[test]
    fn root_audio_mixer_holds_nothing_after_a_reset() {
        let h = active();
        h.set_source_gain(1, 1.0, false);
        h.reset_controls();
        h.add_source(&mono(0.5, 2));
        assert_eq!(h.mix(1).data, vec![s16(0.5); BLOCK]);
    }

    #[test]
    fn root_audio_mixer_counts_added_sources() {
        let h = active();
        let first = h.add_source(&mono(0.1, 1));
        h.add_source(&mono(0.1, 2));
        h.remove_source(first);
        assert_eq!(h.sources_added(), 2);
        h.add_source(&mono(0.1, 1));
        assert_eq!(h.sources_added(), 3);
    }

    #[test]
    fn root_audio_mixer_without_platform_audio_is_unity_and_the_bus_is_idle() {
        let h = new_root_mixer_harness();
        h.add_source(&mono(0.5, 7));
        h.set_source_gain(7, 0.0, true);
        h.set_deafened(true);
        h.set_master_volume(0.0);
        clip(1, &h, 0.5, BLOCK);
        assert_eq!(h.play_effect(1, 0.2), IDLE);
        assert_eq!(h.mix(1).data, vec![s16(0.5); BLOCK]);
        assert_eq!((h.effects_played(), h.effects_dropped()), (0, 1));

        h.set_platform_active(true);
        assert!(settle(&h, 1).data.iter().all(|&v| v == 0));
    }

    #[test]
    fn root_audio_mixer_effects_queued_before_a_release_never_play() {
        let h = active();
        clip(1, &h, 0.5, BLOCK * 4);
        h.play_effect(1, 0.2);
        h.mix(1);
        h.set_platform_active(false);
        assert!(h.mix(1).muted);
        h.set_platform_active(true);
        assert!(h.mix(1).muted);
    }

    #[test]
    fn root_audio_mixer_effects_queued_before_a_release_never_play_without_a_mix_between() {
        let h = active();
        clip(1, &h, 0.5, BLOCK * 4);
        assert_eq!(h.play_effect(1, 0.2), QUEUED);
        h.set_platform_active(false);
        h.set_platform_active(true);
        assert!(h.mix(1).muted);
        assert_eq!(h.play_effect(1, 0.2), QUEUED);
        assert!(!h.mix(1).muted);
    }

    #[test]
    fn root_audio_mixer_drops_effects_that_waited_too_long() {
        let h = active();
        h.set_timeouts(500, 50);
        clip(1, &h, 0.5, BLOCK);
        h.play_effect(1, 0.2);
        h.play_effect(1, 0.2);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(h.mix(1).muted, "a stalled device does not fire them all at once");
        assert_eq!(h.effects_dropped(), 2);
        h.play_effect(1, 0.2);
        assert!(!h.mix(1).muted);
    }

    #[test]
    fn root_audio_mixer_effect_ring_wraps() {
        let h = active();
        clip(1, &h, 0.5, BLOCK);
        for round in 0..80 {
            assert_eq!(h.play_effect(1, 0.2), QUEUED, "round {round}");
            let frame = h.mix(1);
            assert!(frame.data.iter().all(|&v| (level(v) - 0.1).abs() < 1e-4), "round {round}");
        }
        assert_eq!((h.effects_played(), h.effects_dropped()), (80, 0));
    }

    #[test]
    fn root_audio_mixer_re_registering_a_playing_effect_keeps_the_voice() {
        let h = active();
        clip(1, &h, 0.5, BLOCK * 3);
        h.play_effect(1, 0.2);
        assert!((last(&h.mix(1)) - 0.1).abs() < 1e-4);
        clip(1, &h, 0.25, BLOCK * 3);
        h.play_effect(1, 0.2);
        assert!((last(&h.mix(1)) - 0.15).abs() < 1e-4);
        assert!((last(&h.mix(1)) - 0.15).abs() < 1e-4);
        assert!((last(&h.mix(1)) - 0.05).abs() < 1e-4, "the old clip ended");
    }

    #[test]
    fn root_audio_mixer_drops_voices_beyond_the_limit() {
        let h = active();
        clip(1, &h, 0.001, BLOCK * 10);
        for _ in 0..32 {
            assert_eq!(h.play_effect(1, 1.0), QUEUED);
        }
        h.mix(1);
        assert_eq!(h.effects_dropped(), 0);
        assert_eq!(h.play_effect(1, 1.0), QUEUED);
        h.mix(1);
        assert_eq!(h.effects_dropped(), 1);
        let expected = 32.0 * level(s16(0.001));
        assert!((last(&h.mix(1)) - expected).abs() < 1e-3);
    }

    #[test]
    fn root_audio_mixer_counts_each_source_before_its_gain() {
        let h = active();
        h.add_source(&mono(0.5, 1));
        h.add_source(&RootMixTestSource { channels: 2, right: s16(-0.25), ..mono(0.25, 2) });
        h.set_source_gain(1, 2.0, false);
        h.set_source_gain(2, 0.0, false);
        h.set_deafened(true);
        settle(&h, 2);

        let levels = h.source_levels();
        assert_eq!(levels.len(), 2);
        let one = levels.iter().find(|l| l.ssrc == 1).unwrap();
        let two = levels.iter().find(|l| l.ssrc == 2).unwrap();
        let s1 = s16(0.5) as f64;
        let s2 = s16(0.25) as f64;
        assert_eq!((one.samples, two.samples), (3 * 480, 3 * 960));
        assert!((one.sumsq - 3.0 * 480.0 * s1 * s1).abs() < 1.0, "{}", one.sumsq);
        assert!((two.sumsq - 3.0 * 960.0 * s2 * s2).abs() < 1.0, "{}", two.sumsq);
        assert_ne!(one.source_id, two.source_id);
    }

    #[test]
    fn root_audio_mixer_counts_a_muted_source_as_silence_and_a_failed_one_not_at_all() {
        let h = active();
        h.add_source(&RootMixTestSource { info: 1, ..mono(0.5, 1) });
        h.add_source(&RootMixTestSource { info: 2, ..mono(0.5, 2) });
        h.mix(1);
        h.mix(1);

        let levels = h.source_levels();
        let muted = levels.iter().find(|l| l.ssrc == 1).unwrap();
        let failed = levels.iter().find(|l| l.ssrc == 2).unwrap();
        assert_eq!((muted.samples, muted.sumsq), (960, 0.0));
        assert_eq!((failed.samples, failed.sumsq), (0, 0.0));
    }

    #[test]
    fn root_audio_mixer_source_added_again_starts_a_new_count() {
        let h = active();
        let first = h.add_source(&mono(0.5, 7));
        h.mix(1);
        let before = h.source_levels()[0];
        assert_eq!(before.samples, 480);

        h.remove_source(first);
        assert!(h.source_levels().is_empty());
        h.add_source(&mono(0.5, 7));
        let after = h.source_levels()[0];
        assert_eq!((after.ssrc, after.samples, after.sumsq), (7, 0, 0.0));
        assert!(after.source_id > before.source_id);
        h.mix(1);
        assert_eq!(h.source_levels()[0].samples, 480);
    }

    #[test]
    fn root_audio_mixer_survives_concurrent_control_and_sources() {
        let stress = root_audio_mixer_stress_for_test(3000);
        assert_eq!(stress.bad_frames, 0);
        assert_eq!(stress.mix_calls, 3000);
        assert_eq!(stress.overlapping_mixes, 0);
        assert_eq!(stress.sources, 4);
    }

    #[test]
    fn root_audio_mixer_drops_effects_beyond_the_queue() {
        let h = active();
        clip(1, &h, 0.01, BLOCK);
        for _ in 0..32 {
            assert_eq!(h.play_effect(1, 0.2), QUEUED);
        }
        assert_eq!(h.play_effect(1, 0.2), DROPPED);
        assert_eq!(h.play_effect(99, 0.2), UNKNOWN);
        assert_eq!((h.effects_played(), h.effects_dropped()), (32, 1));
        assert!(!h.mix(1).muted);
        assert_eq!(h.play_effect(1, 0.2), QUEUED);
    }

    #[test]
    fn root_audio_mixer_refuses_empty_effect_clips() {
        let h = active();
        assert!(!h.register_effect(1, &[]));
        assert!(h.register_effect(1, &[1, 2, 3]));
        assert!(h.register_effect(1, &[1, 2, 3]));
    }

    #[test]
    fn root_audio_mixer_reset_restores_the_defaults() {
        let h = active();
        h.add_source(&mono(0.5, 7));
        h.set_source_gain(7, 0.2, true);
        h.set_master_volume(0.1);
        h.set_deafened(true);
        clip(1, &h, 0.25, BLOCK * 20);
        h.play_effect(1, 1.0);
        settle(&h, 1);
        h.reset_controls();
        assert_eq!(settle(&h, 1).data, vec![s16(0.5); BLOCK]);
    }

    #[test]
    fn root_audio_mixer_a_non_finite_volume_is_silent() {
        let h = active();
        h.add_source(&mono(0.5, 7));
        h.set_source_gain(7, f32::NAN, false);
        assert!(settle(&h, 1).data.iter().all(|&v| v == 0));
        h.set_source_gain(7, f32::INFINITY, false);
        let value = last(&settle(&h, 1));
        assert!(value > 0.8 && value < 1.0, "{value}");
    }
}
