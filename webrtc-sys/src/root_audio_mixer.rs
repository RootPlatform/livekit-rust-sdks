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
    }

    unsafe extern "C++" {
        include!("livekit/root_audio_mixer.h");

        fn root_audio_soft_clip(sample: f32) -> f32;

        /// Adds `sources`, removes the ones at the `remove` indices, then mixes
        /// once at `out_channels`.
        fn root_audio_mixer_run_for_test(
            sources: &[RootMixTestSource],
            remove: &[usize],
            out_channels: usize,
        ) -> RootMixTestResult;
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
}
