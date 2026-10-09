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

//! Test hooks for the factory's APM: the capture post-processor
//! (`RootCaptureProcessor`) and the decorator (`RootApmDecorator`), which are
//! otherwise only reachable through libwebrtc.

#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    #[derive(Debug, Clone, Default)]
    pub struct RootProcessorFrame {
        pub samples: Vec<f32>,
        /// Every other channel equals channel 0.
        pub copies_match: bool,
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootProcessorStats {
        pub format_seq: u64,
        pub process_rate: u32,
        pub process_channels: u32,
        pub processed_frames: u64,
        pub apm_out_sumsq: f64,
        pub sent_sumsq: f64,
        pub stage_samples: u64,
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootDecoratorProbe {
        /// Inner ApplyConfig calls the decorator made.
        pub inner_applies: u64,
        /// ApplyConfig calls the fake APM got.
        pub fake_applies: u64,
        pub inner_aec: bool,
        pub inner_ns: bool,
        pub inner_agc1: bool,
        pub inner_agc2: bool,
        pub inner_hpf: bool,
        pub fake_mute_calls: u64,
        pub fake_output_used_settings: u64,
        pub output_will_be_muted: bool,
        pub swallowed: u64,
        pub aec_active: bool,
        pub ns_active: bool,
        /// GetConfig serves what the inner APM has.
        pub served_matches_inner: bool,
    }

    unsafe extern "C++" {
        include!("livekit/root_audio_processing.h");

        type RootProcessorHarness;
        type RootApmHarness;

        fn new_root_processor_harness() -> UniquePtr<RootProcessorHarness>;
        fn initialize(self: &RootProcessorHarness, rate: i32, channels: i32);
        fn set_mic_muted(self: &RootProcessorHarness, muted: bool);
        fn set_push_to_talk(self: &RootProcessorHarness, state: u32);
        fn set_input_volume(self: &RootProcessorHarness, volume: f32);
        fn reset_controls(self: &RootProcessorHarness);
        fn process(
            self: &RootProcessorHarness,
            rate: i32,
            channels: usize,
            input: &[f32],
        ) -> RootProcessorFrame;
        fn stats(self: &RootProcessorHarness) -> RootProcessorStats;

        /// The decorator over a recording fake APM, or over the real builtin APM
        /// with the capture processor.
        fn new_root_apm_harness(real_apm: bool) -> UniquePtr<RootApmHarness>;
        fn engine_apply(
            self: &RootApmHarness,
            echo_cancellation: bool,
            noise_suppression: bool,
            auto_gain_control: bool,
            gain_controller2: bool,
            high_pass_filter: bool,
        );
        fn set_output_will_be_muted(self: &RootApmHarness, muted: bool);
        fn post_capture_output_used(self: &RootApmHarness, used: bool) -> bool;
        /// -1 leaves that part of the policy unchanged, 0 off, 1 on.
        fn set_policy(self: &RootApmHarness, echo_cancellation: i8, noise_suppression: i8);
        fn reset_policy(self: &RootApmHarness);
        fn advance_clock(self: &RootApmHarness, ms: i64);
        fn run_due_tasks(self: &RootApmHarness) -> u32;
        fn pending_tasks(self: &RootApmHarness) -> u32;
        fn probe(self: &RootApmHarness) -> RootDecoratorProbe;
        fn process_frame(self: &RootApmHarness, rate: i32, channels: usize, amplitude: i16)
            -> i16;
        fn set_mic_muted(self: &RootApmHarness, muted: bool);
        fn processed_frames(self: &RootApmHarness) -> u64;
        fn processor_rate(self: &RootApmHarness) -> u32;
        fn processor_channels(self: &RootApmHarness) -> u32;
    }
}

#[cfg(test)]
mod processor_tests {
    use super::ffi::{new_root_processor_harness, RootProcessorHarness};

    const VOICE_ACTIVITY: u32 = 0;
    const HELD: u32 = 1;
    const RELEASED: u32 = 2;

    fn frames(rate: i32) -> usize {
        (rate / 100) as usize
    }

    fn constant(rate: i32, value: f32) -> Vec<f32> {
        vec![value; frames(rate)]
    }

    fn run(h: &RootProcessorHarness, rate: i32, channels: usize, value: f32) -> Vec<f32> {
        let frame = h.process(rate, channels, &constant(rate, value));
        assert!(frame.copies_match);
        assert_eq!(frame.samples.len(), frames(rate));
        frame.samples
    }

    fn started(rate: i32, channels: i32) -> cxx::UniquePtr<RootProcessorHarness> {
        let h = new_root_processor_harness();
        h.initialize(rate, channels);
        h
    }

    #[test]
    fn root_capture_processor_passes_audio_through_by_default() {
        let h = started(48000, 1);
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![1000.0; 480]);
    }

    #[test]
    fn root_capture_processor_mute_zeroes_the_frame() {
        let h = started(48000, 1);
        h.set_mic_muted(true);
        assert!(run(&h, 48000, 1, 1000.0).iter().all(|&v| v == 0.0));
        h.set_mic_muted(false);
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![1000.0; 480]);
    }

    #[test]
    fn root_capture_processor_push_to_talk_gates_on_the_key() {
        let h = started(48000, 1);
        h.set_push_to_talk(RELEASED);
        assert!(run(&h, 48000, 1, 1000.0).iter().all(|&v| v == 0.0));
        h.set_push_to_talk(HELD);
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![1000.0; 480]);
        h.set_push_to_talk(VOICE_ACTIVITY);
        assert_eq!(run(&h, 48000, 1, 1000.0), vec![1000.0; 480]);
        h.set_push_to_talk(HELD);
        h.set_mic_muted(true);
        assert!(run(&h, 48000, 1, 1000.0).iter().all(|&v| v == 0.0));
    }

    #[test]
    fn root_capture_processor_ramps_the_input_volume_over_20_ms() {
        let h = started(48000, 1);
        run(&h, 48000, 1, 1.0);
        h.set_input_volume(0.0);
        let first = run(&h, 48000, 1, 1.0);
        let second = run(&h, 48000, 1, 1.0);
        let ramp: Vec<f32> = first.iter().chain(second.iter()).copied().collect();
        assert!(ramp.windows(2).all(|w| w[1] <= w[0]));
        assert!((first[479] - 0.5).abs() < 1e-3);
        assert_eq!(second[479], 0.0);
        assert!(run(&h, 48000, 1, 1.0).iter().all(|&v| v == 0.0));

        h.set_input_volume(2.0);
        run(&h, 48000, 1, 1.0);
        run(&h, 48000, 1, 1.0);
        assert_eq!(run(&h, 48000, 1, 1.0), vec![2.0; 480]);
    }

    #[test]
    fn root_capture_processor_ramp_is_20_ms_at_any_rate() {
        for rate in [8000, 16000, 32000, 44100] {
            let h = started(rate, 1);
            run(&h, rate, 1, 1.0);
            h.set_input_volume(0.0);
            let n = frames(rate);
            let first = run(&h, rate, 1, 1.0);
            assert!((first[n - 1] - 0.5).abs() < 1e-2, "{rate}: {}", first[n - 1]);
            assert_eq!(run(&h, rate, 1, 1.0)[n - 1], 0.0, "{rate}");
        }
    }

    #[test]
    fn root_capture_processor_clamps_and_sanitises_the_input_volume() {
        let h = started(16000, 1);
        h.set_input_volume(f32::NAN);
        run(&h, 16000, 1, 1.0);
        run(&h, 16000, 1, 1.0);
        assert!(run(&h, 16000, 1, 1.0).iter().all(|&v| v == 0.0));
        h.set_input_volume(9.0);
        run(&h, 16000, 1, 1.0);
        run(&h, 16000, 1, 1.0);
        assert_eq!(run(&h, 16000, 1, 1.0), vec![2.0; 160]);
    }

    #[test]
    fn root_capture_processor_handles_odd_rates_and_stereo_buffers() {
        for (rate, channels) in [(16000, 1), (8000, 1), (44100, 1), (48000, 2), (16000, 2)] {
            let h = started(rate, 1);
            let input: Vec<f32> = (0..frames(rate)).map(|i| i as f32).collect();
            let frame = h.process(rate, channels, &input);
            assert!(frame.copies_match, "{rate}/{channels}");
            assert_eq!(frame.samples, input, "{rate}/{channels}");
            let stats = h.stats();
            assert_eq!(stats.process_rate, rate as u32);
            assert_eq!(stats.process_channels, channels as u32);
            assert_eq!(stats.stage_samples, frames(rate) as u64);
        }
    }

    #[test]
    fn root_capture_processor_mutes_every_channel() {
        let h = started(48000, 1);
        h.set_mic_muted(true);
        let frame = h.process(48000, 2, &constant(48000, 500.0));
        assert!(frame.copies_match);
        assert!(frame.samples.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn root_capture_processor_counts_frames_and_levels() {
        let h = started(48000, 1);
        run(&h, 48000, 1, 100.0);
        h.set_mic_muted(true);
        run(&h, 48000, 1, 100.0);
        let stats = h.stats();
        assert_eq!(stats.processed_frames, 2);
        assert_eq!(stats.stage_samples, 960);
        assert_eq!(stats.apm_out_sumsq, 960.0 * 100.0 * 100.0);
        assert_eq!(stats.sent_sumsq, 480.0 * 100.0 * 100.0);
    }

    #[test]
    fn root_capture_processor_bumps_the_format_sequence_only_on_a_change() {
        let h = started(48000, 1);
        assert_eq!(h.stats().format_seq, 0);
        run(&h, 48000, 1, 1.0);
        assert_eq!(h.stats().format_seq, 1);
        h.initialize(48000, 1);
        run(&h, 48000, 1, 1.0);
        assert_eq!(h.stats().format_seq, 1);
        run(&h, 48000, 2, 1.0);
        assert_eq!(h.stats().format_seq, 2);
        h.initialize(16000, 1);
        run(&h, 16000, 2, 1.0);
        let stats = h.stats();
        assert_eq!((stats.format_seq, stats.process_rate, stats.process_channels), (3, 16000, 2));
    }

    #[test]
    fn root_capture_processor_takes_the_rate_from_the_buffer_when_initialize_disagrees() {
        let h = started(48000, 1);
        run(&h, 16000, 1, 1.0);
        assert_eq!(h.stats().process_rate, 16000);
    }

    #[test]
    fn root_capture_processor_reset_opens_the_gate() {
        let h = started(48000, 1);
        h.set_mic_muted(true);
        h.set_push_to_talk(RELEASED);
        h.set_input_volume(0.5);
        h.reset_controls();
        run(&h, 48000, 1, 1.0);
        run(&h, 48000, 1, 1.0);
        assert_eq!(run(&h, 48000, 1, 1.0), vec![1.0; 480]);
    }
}

#[cfg(test)]
mod decorator_tests {
    use super::ffi::{new_root_apm_harness, RootApmHarness};

    const KEEP: i8 = -1;

    fn fake() -> cxx::UniquePtr<RootApmHarness> {
        new_root_apm_harness(false)
    }

    #[test]
    fn root_apm_decorator_never_enables_agc_and_keeps_the_high_pass_filter() {
        let h = fake();
        for (aec, ns, agc1, agc2, hpf) in [
            (true, true, true, true, false),
            (true, false, true, false, false),
            (false, true, false, true, true),
        ] {
            h.engine_apply(aec, ns, agc1, agc2, hpf);
            let probe = h.probe();
            assert!(!probe.inner_agc1 && !probe.inner_agc2, "{probe:?}");
            assert!(probe.inner_hpf);
            assert_eq!((probe.inner_aec, probe.inner_ns), (aec, ns));
            assert_eq!((probe.aec_active, probe.ns_active), (aec, ns));
            assert!(probe.served_matches_inner);
        }
    }

    #[test]
    fn root_apm_decorator_applies_only_a_changed_config() {
        let h = fake();
        h.engine_apply(true, true, true, false, true);
        let applies = h.probe().fake_applies;
        h.engine_apply(true, true, true, false, true);
        h.engine_apply(true, true, false, false, false);
        assert_eq!(h.probe().fake_applies, applies);
        h.engine_apply(false, true, false, false, true);
        assert_eq!(h.probe().fake_applies, applies + 1);
        assert_eq!(h.probe().inner_applies, h.probe().fake_applies);
    }

    #[test]
    fn root_apm_decorator_swallows_mute_so_capture_output_stays_used() {
        let h = fake();
        h.set_output_will_be_muted(true);
        assert!(h.post_capture_output_used(false));
        h.set_output_will_be_muted(false);
        let probe = h.probe();
        assert_eq!((probe.fake_mute_calls, probe.fake_output_used_settings), (0, 0));
        assert_eq!(probe.swallowed, 4);
        assert!(!probe.output_will_be_muted);
    }

    #[test]
    fn root_apm_decorator_policy_overrides_the_engine() {
        let h = fake();
        h.engine_apply(true, true, false, false, true);
        h.set_policy(0, KEEP);
        assert_eq!(h.run_due_tasks(), 1);
        let probe = h.probe();
        assert!(!probe.inner_aec && probe.inner_ns);

        h.engine_apply(true, false, false, false, true);
        let probe = h.probe();
        assert!(!probe.inner_aec && !probe.inner_ns);
        assert!(!probe.aec_active);

        h.advance_clock(1000);
        h.reset_policy();
        assert_eq!(h.run_due_tasks(), 1);
        h.engine_apply(true, false, false, false, true);
        assert!(h.probe().inner_aec);
    }

    #[test]
    fn root_apm_decorator_applies_policy_at_most_once_a_second_and_coalesces() {
        let h = fake();
        h.engine_apply(true, true, false, false, true);
        h.set_policy(0, KEEP);
        assert_eq!(h.run_due_tasks(), 1);
        let applies = h.probe().fake_applies;

        h.set_policy(1, KEEP);
        h.set_policy(0, KEEP);
        h.set_policy(1, 0);
        assert_eq!(h.pending_tasks(), 1);
        assert_eq!(h.run_due_tasks(), 0);
        h.advance_clock(999);
        assert_eq!(h.run_due_tasks(), 0);
        assert!(!h.probe().inner_aec);

        h.advance_clock(1);
        assert_eq!(h.run_due_tasks(), 1);
        let probe = h.probe();
        assert!(probe.inner_aec && !probe.inner_ns);
        assert_eq!(probe.fake_applies, applies + 1);
    }

    #[test]
    fn root_apm_decorator_holds_a_pending_policy_back_from_engine_applies() {
        let h = fake();
        h.engine_apply(true, true, false, false, true);
        h.set_policy(0, KEEP);
        h.run_due_tasks();
        h.set_policy(1, KEEP);
        h.engine_apply(false, true, false, false, true);
        assert!(!h.probe().inner_aec);
        h.advance_clock(1000);
        h.run_due_tasks();
        assert!(h.probe().inner_aec);
    }

    #[test]
    fn root_apm_decorator_policy_matching_the_current_one_schedules_nothing() {
        let h = fake();
        h.set_policy(KEEP, KEEP);
        assert_eq!(h.pending_tasks(), 0);
        h.reset_policy();
        assert_eq!(h.pending_tasks(), 0);
    }

    #[test]
    fn root_apm_decorator_runs_the_capture_processor_every_frame_on_a_real_apm() {
        let h = new_root_apm_harness(true);
        h.engine_apply(true, false, true, true, true);
        h.set_output_will_be_muted(true);
        h.post_capture_output_used(false);
        for _ in 0..20 {
            h.process_frame(48000, 1, 8000);
        }
        assert_eq!(h.processed_frames(), 20);
        let probe = h.probe();
        assert!(probe.inner_aec && !probe.inner_agc1 && !probe.inner_agc2 && probe.inner_hpf);
        assert!(!probe.output_will_be_muted);
        assert!(h.process_frame(48000, 1, 8000) > 1000);

        h.set_mic_muted(true);
        assert_eq!(h.process_frame(48000, 1, 8000), 0);
        assert_eq!(h.processed_frames(), 22);
    }

    #[test]
    fn root_apm_decorator_real_apm_feeds_the_processor_at_the_stream_rate() {
        // (stream rate, channels, rate the processor sees): the APM never
        // processes below 16 kHz, and a 48 kHz stream reaches the post-processor
        // through its full-band buffer with every output channel.
        for (rate, channels, processed_rate) in
            [(16000, 1, 16000), (8000, 1, 16000), (48000, 2, 48000), (32000, 1, 32000)]
        {
            let h = new_root_apm_harness(true);
            h.engine_apply(true, false, false, false, true);
            for _ in 0..3 {
                h.process_frame(rate, channels, 8000);
            }
            assert_eq!(h.processed_frames(), 3, "{rate}/{channels}");
            assert_eq!(h.processor_rate(), processed_rate, "{rate}/{channels}");
            assert!(h.processor_channels() >= 1, "{rate}/{channels}");
            h.set_mic_muted(true);
            // At 8 kHz the APM processes at 16 kHz, and its resampler's delay line
            // still holds the last unmuted samples for the first muted frame.
            h.process_frame(rate, channels, 8000);
            assert_eq!(h.process_frame(rate, channels, 8000), 0, "{rate}/{channels}");
        }
    }
}
