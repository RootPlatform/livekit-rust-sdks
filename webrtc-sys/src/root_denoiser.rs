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

//! The Effects SDK denoiser slot of the capture processor (`RootDenoiserSlot`),
//! its control side (the `rootapp-effects` thread and the WebRTC NS fallback)
//! and the native notification handler the app installs on its pipelines.

#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootDenoiserNotificationCounts {
        /// slowProcessing (code 1).
        pub slow: u64,
        pub other: u64,
    }

    /// A fake Effects SDK pipeline (tests): output = round(scale x input)
    /// delayed by delay_samples; call `burst_call` pops two frames, calls
    /// [empty_from, empty_from + empty_count) return nothing (calls count from
    /// 1 after creation or a flush).
    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootFakeFxSpec {
        pub scale: f32,
        pub delay_samples: u32,
        pub burst_call: u32,
        pub empty_from: u32,
        pub empty_count: u32,
        pub sleep_us: u32,
    }

    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootFakeFxCounts {
        pub process_calls: u64,
        pub flush_calls: u64,
        pub power_sets: u64,
        pub last_power: f32,
        pub calls_after_poison: u64,
        pub releases: u64,
    }

    #[derive(Debug, Clone, Default)]
    pub struct RootDenoiseTestFrame {
        /// 0 not in use, 1 missed, 2 ran.
        pub kind: u8,
        pub samples: usize,
        pub restart: bool,
        pub out: Vec<f32>,
    }

    #[derive(Debug, Clone, Default)]
    pub struct RootDenoiserSlotInfo {
        pub installed: bool,
        pub rate: u32,
        /// 0 none, 1 rate mismatch, 2 output stalled, 3 slow.
        pub bypass: u32,
        pub generation: u64,
        pub frames: u64,
        pub misses: u64,
        pub empty_outputs: u64,
        pub slow_frames: u64,
        pub bypassed_frames: u64,
        pub installs: u64,
        pub frames_until_first_output: i32,
        pub process_max_us: u64,
        pub histogram: Vec<u64>,
    }

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct RootDenoiserTestCompletion {
        pub owner: u64,
        pub async_id: u64,
        /// Empty on success.
        pub error: String,
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct RootDenoiserTestReturned {
        pub owner: u64,
        pub pipeline: u64,
    }

    #[derive(Debug, Clone, Default)]
    pub struct RootDenoiserTestReturnAll {
        pub pipelines: Vec<u64>,
        pub error: String,
    }

    unsafe extern "C++" {
        include!("livekit/root_denoiser.h");

        type RootDenoiserHarness;

        fn root_denoiser_notification_counts() -> RootDenoiserNotificationCounts;
        /// The handler's address, 0 where the denoiser is not built.
        fn root_denoiser_notification_handler() -> usize;
        fn root_denoiser_supported() -> bool;
        fn root_denoiser_notify_for_test(codes: &[i32], include_null: bool);

        /// A slot, fake pipelines and the control side over hooks the test
        /// sets (tests only; no device).
        fn new_root_denoiser_harness(
            install_wait_ms: i64,
            fallback_grace_ms: i64,
        ) -> UniquePtr<RootDenoiserHarness>;
        fn add_fake(self: &RootDenoiserHarness, spec: &RootFakeFxSpec) -> usize;
        fn fake_pipeline(self: &RootDenoiserHarness, index: usize) -> u64;
        fn fake_counts(self: &RootDenoiserHarness, index: usize) -> RootFakeFxCounts;
        fn poison_fake(self: &RootDenoiserHarness, index: usize);
        fn set_fake_sleep_us(self: &RootDenoiserHarness, index: usize, sleep_us: u32);
        fn set_fake_empty(self: &RootDenoiserHarness, index: usize, empty: bool);
        fn install_now(self: &RootDenoiserHarness, index: usize, rate: u32) -> bool;
        fn remove_now(self: &RootDenoiserHarness) -> u64;
        /// The replaced pipeline, or u64::MAX when the audio thread held on.
        fn swap(self: &RootDenoiserHarness, pipeline: u64, rate: u32) -> u64;
        fn hold_slot(self: &RootDenoiserHarness, hold: bool) -> bool;
        fn process(
            self: &RootDenoiserHarness,
            rate: u32,
            input: &[f32],
            flush: bool,
            power: f32,
            out_capacity: usize,
        ) -> RootDenoiseTestFrame;
        fn latch_bypass(self: &RootDenoiserHarness, reason: u32);
        fn slot_info(self: &RootDenoiserHarness, take_max: bool) -> RootDenoiserSlotInfo;
        fn start_audio(self: &RootDenoiserHarness, rate: u32);
        fn stop_audio(self: &RootDenoiserHarness) -> u64;
        fn set_speaking(self: &RootDenoiserHarness, speaking: bool);
        fn set_wanted(self: &RootDenoiserHarness, wanted: bool);
        fn set_process_rate(self: &RootDenoiserHarness, rate: u32);
        fn request(
            self: &RootDenoiserHarness,
            owner: u64,
            async_id: u64,
            pipeline: u64,
            rate: u32,
        );
        fn wake(self: &RootDenoiserHarness);
        fn return_all(self: &RootDenoiserHarness) -> RootDenoiserTestReturnAll;
        fn reset(self: &RootDenoiserHarness);
        fn take_completions(self: &RootDenoiserHarness) -> Vec<RootDenoiserTestCompletion>;
        fn take_returned(self: &RootDenoiserHarness) -> Vec<RootDenoiserTestReturned>;
        fn take_fallback_changes(self: &RootDenoiserHarness) -> Vec<u8>;
        fn fallback(self: &RootDenoiserHarness) -> bool;
        fn pending(self: &RootDenoiserHarness) -> usize;
        /// The fallback state machine alone: -1 no change, 0 off, 1 on.
        fn fallback_update(
            self: &RootDenoiserHarness,
            now_ms: i64,
            wanted: bool,
            installed: bool,
            bypass: u32,
            pipeline_rate: u32,
            process_rate: u32,
        ) -> i8;
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::ffi::*;

    const NOT_IN_USE: u8 = 0;
    const MISSED: u8 = 1;
    const RAN: u8 = 2;
    const RATE_MISMATCH: u32 = 1;
    const OUTPUT_STALLED: u32 = 2;
    const SLOW: u32 = 3;

    fn harness() -> cxx::UniquePtr<RootDenoiserHarness> {
        new_root_denoiser_harness(3000, 1000)
    }

    fn fake(scale: f32) -> RootFakeFxSpec {
        RootFakeFxSpec { scale, ..Default::default() }
    }

    fn frame(rate: u32, value: f32) -> Vec<f32> {
        vec![value; (rate / 100) as usize]
    }

    fn run(h: &RootDenoiserHarness, rate: u32, value: f32) -> RootDenoiseTestFrame {
        h.process(rate, &frame(rate, value), false, 0.5, 8 * (rate / 100) as usize)
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn completions(h: &RootDenoiserHarness, count: usize) -> Vec<RootDenoiserTestCompletion> {
        let mut all = Vec::new();
        wait_until("completions", || {
            all.extend(h.take_completions());
            all.len() >= count
        });
        all
    }

    #[test]
    fn root_denoiser_slot_passes_frames_dry_until_a_pipeline_is_installed() {
        let h = harness();
        assert_eq!(run(&h, 48_000, 1000.0).kind, NOT_IN_USE);
        let a = h.add_fake(&fake(0.5));
        assert!(h.install_now(a, 48_000));

        let first = run(&h, 48_000, 1000.0);
        assert_eq!((first.kind, first.samples, first.restart), (RAN, 480, true));
        assert!(first.out.iter().all(|s| *s == 500.0));
        let second = run(&h, 48_000, 1000.0);
        assert_eq!((second.kind, second.restart), (RAN, false));

        let info = h.slot_info(false);
        assert!(info.installed);
        assert_eq!((info.rate, info.bypass, info.frames, info.installs), (48_000, 0, 2, 1));
        assert_eq!(info.histogram.iter().sum::<u64>(), 2);
        assert_eq!(info.frames_until_first_output, 0);

        assert_eq!(h.remove_now(), h.fake_pipeline(a));
        assert_eq!(run(&h, 48_000, 1000.0).kind, NOT_IN_USE);
        assert_eq!(h.fake_counts(a).process_calls, 2);
        assert_eq!(h.fake_counts(a).releases, 0, "native never releases a pipeline");
    }

    #[test]
    fn root_denoiser_flush_runs_once_before_the_next_process() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        h.install_now(a, 16_000);
        let f = frame(16_000, 10.0);
        assert!(h.process(16_000, &f, true, 0.5, 1280).restart);
        assert_eq!(h.fake_counts(a).flush_calls, 0, "a new pipeline needs no flush");

        assert!(!h.process(16_000, &f, false, 0.5, 1280).restart);
        let flushed = h.process(16_000, &f, true, 0.5, 1280);
        assert!(flushed.restart);
        assert_eq!(h.fake_counts(a).flush_calls, 1);
        assert!(!h.process(16_000, &f, false, 0.5, 1280).restart);

        h.hold_slot(true);
        assert_eq!(h.process(16_000, &f, true, 0.5, 1280).kind, MISSED);
        h.hold_slot(false);
        let after_miss = h.process(16_000, &f, false, 0.5, 1280);
        assert!(after_miss.restart, "a flush asked on a missed frame is kept");
        assert_eq!(h.fake_counts(a).flush_calls, 2);
        assert_eq!(h.slot_info(false).misses, 1);
    }

    #[test]
    fn root_denoiser_power_is_applied_once_per_change_and_per_pipeline() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        h.install_now(a, 48_000);
        let f = frame(48_000, 1.0);
        h.process(48_000, &f, false, 0.5, 3840);
        h.process(48_000, &f, false, 0.5, 3840);
        assert_eq!((h.fake_counts(a).power_sets, h.fake_counts(a).last_power), (1, 0.5));
        h.process(48_000, &f, false, 0.75, 3840);
        h.process(48_000, &f, false, f32::NAN, 3840);
        h.process(48_000, &f, false, 7.0, 3840);
        let counts = h.fake_counts(a);
        assert_eq!((counts.power_sets, counts.last_power), (4, 1.0), "NaN is 0, clamped to 1");

        let b = h.add_fake(&fake(1.0));
        h.install_now(b, 48_000);
        h.process(48_000, &f, false, 1.0, 3840);
        assert_eq!(h.fake_counts(b).power_sets, 1, "a new pipeline gets the power");
    }

    #[test]
    fn root_denoiser_rate_mismatch_bypasses_until_a_matching_pipeline() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        h.install_now(a, 16_000);
        assert_eq!(run(&h, 48_000, 1.0).kind, NOT_IN_USE);
        assert_eq!(h.slot_info(false).bypass, RATE_MISMATCH);
        assert_eq!(run(&h, 16_000, 1.0).kind, NOT_IN_USE, "the bypass stays");
        assert_eq!(h.slot_info(false).bypassed_frames, 2);
        assert_eq!(h.fake_counts(a).process_calls, 0);

        let b = h.add_fake(&fake(1.0));
        h.install_now(b, 48_000);
        assert_eq!(h.slot_info(false).bypass, 0);
        assert_eq!(run(&h, 48_000, 1.0).kind, RAN);

        let wrong_length = h.process(48_000, &[1.0; 479], false, 0.5, 3840);
        assert_eq!(wrong_length.kind, NOT_IN_USE);
        assert_eq!(h.slot_info(false).bypass, RATE_MISMATCH);

        let c = h.add_fake(&fake(1.0));
        h.install_now(c, 48_000);
        assert_eq!(h.process(48_000, &frame(48_000, 1.0), false, 0.5, 3839).kind, NOT_IN_USE);
        assert_eq!(h.slot_info(false).bypass, RATE_MISMATCH, "output capacity under 8 frames");
    }

    #[test]
    fn root_denoiser_slow_frames_latch_a_bypass() {
        let h = harness();
        let a = h.add_fake(&RootFakeFxSpec { sleep_us: 6_000, ..fake(1.0) });
        h.install_now(a, 48_000);
        for call in 0..3 {
            assert_eq!(run(&h, 48_000, 1.0).kind, RAN);
            let info = h.slot_info(true);
            if call < 2 && info.process_max_us <= 20_000 {
                assert_eq!(info.bypass, 0, "two slow calls are not enough");
            }
        }
        let info = h.slot_info(false);
        assert_eq!((info.slow_frames, info.bypass), (3, SLOW));
        assert_eq!(run(&h, 48_000, 1.0).kind, NOT_IN_USE);
        assert_eq!(info.histogram[13..].iter().sum::<u64>(), 3, "{:?}", info.histogram);

        let b = h.add_fake(&RootFakeFxSpec { sleep_us: 25_000, ..fake(1.0) });
        h.install_now(b, 48_000);
        assert_eq!(run(&h, 48_000, 1.0).kind, RAN);
        assert_eq!(h.slot_info(false).bypass, SLOW, "one call over 20 ms");
        assert!(h.slot_info(true).process_max_us >= 20_000);
        assert_eq!(h.slot_info(true).process_max_us, 0, "the maximum resets on read");
    }

    #[test]
    fn root_denoiser_latched_stall_bypasses_only_the_pipeline_that_ran() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        h.install_now(a, 48_000);
        run(&h, 48_000, 1.0);
        h.latch_bypass(OUTPUT_STALLED);
        assert_eq!(h.slot_info(false).bypass, OUTPUT_STALLED);
        assert_eq!(run(&h, 48_000, 1.0).kind, NOT_IN_USE);

        let b = h.add_fake(&fake(1.0));
        h.install_now(b, 48_000);
        h.latch_bypass(OUTPUT_STALLED);
        assert_eq!(h.slot_info(false).bypass, 0, "b never ran");
        assert_eq!(run(&h, 48_000, 1.0).kind, RAN);

        h.hold_slot(true);
        h.latch_bypass(SLOW);
        assert_eq!(h.slot_info(false).bypass, 0, "kept for the next frame");
        h.hold_slot(false);
        assert_eq!(run(&h, 48_000, 1.0).kind, NOT_IN_USE);
        assert_eq!(h.slot_info(false).bypass, SLOW);
    }

    #[test]
    fn root_denoiser_counts_empty_outputs_and_calls_before_the_first_output() {
        let h = harness();
        let a = h.add_fake(&RootFakeFxSpec { empty_from: 1, empty_count: 3, ..fake(1.0) });
        h.install_now(a, 48_000);
        for _ in 0..3 {
            let f = run(&h, 48_000, 1.0);
            assert_eq!((f.kind, f.samples), (RAN, 0));
            assert_eq!(h.slot_info(false).frames_until_first_output, -1);
        }
        assert_eq!(run(&h, 48_000, 1.0).samples, 480);
        let info = h.slot_info(false);
        assert_eq!((info.frames_until_first_output, info.empty_outputs), (3, 3));

        let b = h.add_fake(&RootFakeFxSpec { delay_samples: 480, burst_call: 2, ..fake(1.0) });
        h.install_now(b, 48_000);
        assert_eq!(h.slot_info(false).frames_until_first_output, -1, "reset per install");
        assert_eq!(run(&h, 48_000, 1.0).samples, 480);
        assert_eq!(run(&h, 48_000, 1.0).samples, 960, "the burst call");
        h.remove_now();
        assert_eq!(h.slot_info(false).frames_until_first_output, -1);
    }

    #[test]
    fn root_denoiser_install_and_remove_while_the_audio_thread_runs() {
        let h = harness();
        h.start_audio(48_000);
        let mut fakes = Vec::new();
        let mut installed: Option<usize> = None;
        for i in 0..400 {
            let next = if i % 7 == 6 { None } else { Some(h.add_fake(&fake(0.5))) };
            fakes.extend(next);
            let previous = h.swap(next.map_or(0, |n| h.fake_pipeline(n)), 48_000);
            assert_ne!(previous, u64::MAX, "the audio thread let go within 200 ms");
            assert_eq!(previous, installed.map_or(0, |p| h.fake_pipeline(p)));
            if let Some(p) = installed {
                h.poison_fake(p);
            }
            installed = next;
            std::thread::sleep(Duration::from_micros(200));
        }
        let frames = h.stop_audio();
        assert!(frames > 0);
        let ran: u64 = fakes.iter().map(|f| h.fake_counts(*f).process_calls).sum();
        assert!(ran > 0, "the audio thread used the pipelines");
        let after_return: u64 = fakes.iter().map(|f| h.fake_counts(*f).calls_after_poison).sum();
        assert_eq!(after_return, 0, "a returned pipeline was called again");
    }

    #[test]
    fn root_denoiser_notification_handler_counts_through_its_vtable() {
        assert_eq!(root_denoiser_supported(), cfg!(target_pointer_width = "64"));
        if root_denoiser_supported() {
            assert_ne!(root_denoiser_notification_handler(), 0);
        }
        let before = root_denoiser_notification_counts();
        root_denoiser_notify_for_test(&[1, 1, 2, 7], true);
        let after = root_denoiser_notification_counts();
        assert_eq!(after.slow - before.slow, 2);
        assert_eq!(after.other - before.other, 2, "a null notification is ignored");
    }

    #[test]
    fn root_denoiser_fallback_turns_on_at_once_for_a_dead_denoiser() {
        let h = harness();
        assert_eq!(h.fallback_update(0, true, true, 0, 48_000, 48_000), -1);
        assert_eq!(h.fallback_update(10, true, true, OUTPUT_STALLED, 48_000, 48_000), 1);
        assert_eq!(h.fallback_update(20, true, false, 0, 0, 48_000), -1, "removed: stays on");
        assert_eq!(h.fallback_update(30, true, true, 0, 48_000, 48_000), 0, "a new one runs");
        assert_eq!(h.fallback_update(40, true, true, SLOW, 48_000, 48_000), 1);
        assert_eq!(h.fallback_update(50, false, true, SLOW, 48_000, 48_000), 0, "not wanted");
    }

    #[test]
    fn root_denoiser_fallback_waits_a_grace_period_for_a_missing_denoiser() {
        let h = harness();
        assert_eq!(h.fallback_update(0, true, false, 0, 0, 48_000), -1);
        assert_eq!(h.fallback_update(999, true, false, 0, 0, 48_000), -1);
        assert_eq!(h.fallback_update(1000, true, false, 0, 0, 48_000), 1);
        assert_eq!(h.fallback_update(1010, true, true, 0, 48_000, 48_000), 0);

        assert_eq!(h.fallback_update(2000, true, true, RATE_MISMATCH, 48_000, 16_000), -1);
        assert_eq!(h.fallback_update(2999, true, true, RATE_MISMATCH, 48_000, 16_000), -1);
        assert_eq!(h.fallback_update(3000, true, true, RATE_MISMATCH, 48_000, 16_000), 1);
        assert_eq!(h.fallback_update(3010, true, true, 0, 16_000, 16_000), 0);

        assert_eq!(h.fallback_update(4000, true, true, 0, 48_000, 16_000), -1);
        assert_eq!(h.fallback_update(4500, true, true, 0, 48_000, 48_000), -1, "active again");
        assert_eq!(h.fallback_update(5400, true, true, 0, 48_000, 16_000), -1);
        assert_eq!(
            h.fallback_update(6399, true, true, 0, 48_000, 16_000),
            -1,
            "the grace starts over"
        );
        assert_eq!(h.fallback_update(6400, true, true, 0, 48_000, 16_000), 1);

        assert_eq!(h.fallback_update(7000, false, false, 0, 0, 0), 0);
        assert_eq!(h.fallback_update(9000, false, false, 0, 0, 0), -1, "not wanted: never");
        assert_eq!(
            h.fallback_update(9000, true, true, 0, 48_000, 0),
            -1,
            "no frame yet: the rate is unknown"
        );
    }

    #[test]
    fn root_denoiser_install_waits_for_a_pause_in_speech() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        h.set_speaking(true);
        h.request(1, 10, h.fake_pipeline(a), 48_000);
        std::thread::sleep(Duration::from_millis(150));
        assert!(h.take_completions().is_empty());
        assert_eq!(h.pending(), 1);
        assert!(!h.slot_info(false).installed);
        h.set_speaking(false);
        let done = completions(&h, 1);
        assert_eq!(
            done,
            vec![RootDenoiserTestCompletion { owner: 1, async_id: 10, error: String::new() }]
        );
        assert!(h.slot_info(false).installed);
        assert_eq!(h.pending(), 0);
    }

    #[test]
    fn root_denoiser_install_goes_ahead_at_the_limit_while_speaking() {
        let h = new_root_denoiser_harness(250, 1000);
        let a = h.add_fake(&fake(1.0));
        h.set_speaking(true);
        let started = Instant::now();
        h.request(1, 10, h.fake_pipeline(a), 48_000);
        let done = completions(&h, 1);
        assert!(started.elapsed() >= Duration::from_millis(240));
        assert!(done[0].error.is_empty());
        assert!(h.slot_info(false).installed);
    }

    #[test]
    fn root_denoiser_replace_and_remove_return_the_previous_pipeline_to_its_owner() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        let b = h.add_fake(&fake(1.0));
        h.request(1, 10, h.fake_pipeline(a), 48_000);
        completions(&h, 1);
        h.request(2, 11, h.fake_pipeline(b), 48_000);
        completions(&h, 1);
        assert_eq!(
            h.take_returned(),
            vec![RootDenoiserTestReturned { owner: 1, pipeline: h.fake_pipeline(a) }]
        );

        h.set_speaking(true);
        h.request(3, 12, 0, 0);
        let done = completions(&h, 1);
        assert_eq!(done[0].async_id, 12);
        assert!(done[0].error.is_empty(), "a removal does not wait for a pause");
        assert_eq!(
            h.take_returned(),
            vec![RootDenoiserTestReturned { owner: 2, pipeline: h.fake_pipeline(b) }]
        );
        assert!(!h.slot_info(false).installed);
    }

    #[test]
    fn root_denoiser_a_newer_request_supersedes_a_waiting_install() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        let b = h.add_fake(&fake(1.0));
        h.set_speaking(true);
        h.request(1, 10, h.fake_pipeline(a), 48_000);
        std::thread::sleep(Duration::from_millis(30));
        h.request(1, 11, h.fake_pipeline(b), 48_000);
        let superseded = completions(&h, 1);
        assert_eq!(superseded[0].async_id, 10);
        assert!(superseded[0].error.contains("superseded"));
        h.set_speaking(false);
        let done = completions(&h, 1);
        assert_eq!((done[0].async_id, done[0].error.as_str()), (11, ""));
        assert!(h.take_returned().is_empty(), "a superseded pipeline is never held");
        assert_eq!(h.remove_now(), h.fake_pipeline(b));
    }

    #[test]
    fn root_denoiser_install_fails_when_the_audio_thread_holds_on() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        assert!(h.hold_slot(true));
        h.request(1, 10, h.fake_pipeline(a), 48_000);
        let done = completions(&h, 1);
        assert!(done[0].error.contains("did not let go"));
        h.hold_slot(false);
        assert!(!h.slot_info(false).installed);
        assert!(h.take_returned().is_empty());
    }

    #[test]
    fn root_denoiser_return_all_takes_installed_waiting_and_undelivered_pipelines() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        let b = h.add_fake(&fake(1.0));
        let c = h.add_fake(&fake(1.0));
        h.request(1, 10, h.fake_pipeline(a), 48_000);
        completions(&h, 1);
        h.request(1, 11, h.fake_pipeline(b), 48_000);
        completions(&h, 1);
        h.set_speaking(true);
        h.request(1, 12, h.fake_pipeline(c), 48_000);
        wait_until("c waiting", || h.pending() == 1);

        let all = h.return_all();
        assert!(all.error.is_empty());
        let mut pipelines = all.pipelines.clone();
        pipelines.sort_unstable();
        let mut expected = vec![h.fake_pipeline(a), h.fake_pipeline(b), h.fake_pipeline(c)];
        expected.sort_unstable();
        assert_eq!(pipelines, expected);
        let done = completions(&h, 1);
        assert_eq!(done[0].async_id, 12);
        assert!(done[0].error.contains("returned before install"));
        assert!(!h.slot_info(false).installed);
        assert!(h.take_returned().is_empty());
        assert_eq!(h.pending(), 0);

        h.set_speaking(false);
        h.request(1, 13, h.fake_pipeline(a), 48_000);
        assert!(completions(&h, 1)[0].error.is_empty(), "later requests work");
    }

    #[test]
    fn root_denoiser_return_all_reports_an_installed_pipeline_it_could_not_remove() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        h.install_now(a, 48_000);
        assert!(h.hold_slot(true));
        let all = h.return_all();
        h.hold_slot(false);
        assert!(all.error.contains("stays installed"));
        assert!(all.pipelines.is_empty());
        assert!(h.slot_info(false).installed);
    }

    #[test]
    fn root_denoiser_reset_returns_everything_and_clears_the_fallback() {
        let h = new_root_denoiser_harness(3000, 50);
        let a = h.add_fake(&fake(1.0));
        let b = h.add_fake(&fake(1.0));
        h.request(1, 10, h.fake_pipeline(a), 48_000);
        completions(&h, 1);
        h.set_speaking(true);
        h.request(2, 11, h.fake_pipeline(b), 48_000);
        wait_until("b waiting", || h.pending() == 1);
        h.set_wanted(true);
        h.set_process_rate(16_000);
        h.wake();
        wait_until("fallback", || h.fallback());

        h.set_wanted(false);
        h.reset();
        assert!(!h.fallback());
        let mut returned = h.take_returned();
        returned.sort_by_key(|r| r.owner);
        assert_eq!(
            returned,
            vec![
                RootDenoiserTestReturned { owner: 1, pipeline: h.fake_pipeline(a) },
                RootDenoiserTestReturned { owner: 2, pipeline: h.fake_pipeline(b) },
            ]
        );
        let done = completions(&h, 1);
        assert_eq!(done[0].async_id, 11);
        assert!(!done[0].error.is_empty());
        assert!(!h.slot_info(false).installed);
        assert_eq!(h.take_fallback_changes(), vec![1, 0]);
    }

    #[test]
    fn root_denoiser_effects_thread_drives_the_ns_fallback() {
        let h = new_root_denoiser_harness(3000, 100);
        h.set_process_rate(48_000);
        h.set_wanted(true);
        let started = Instant::now();
        h.wake();
        wait_until("fallback after the grace", || h.fallback());
        assert!(started.elapsed() >= Duration::from_millis(95));
        assert_eq!(h.take_fallback_changes(), vec![1]);

        let a = h.add_fake(&fake(1.0));
        h.request(1, 10, h.fake_pipeline(a), 48_000);
        completions(&h, 1);
        wait_until("the denoiser runs", || !h.fallback());
        assert_eq!(h.take_fallback_changes(), vec![0]);

        run(&h, 48_000, 1.0);
        let latched = Instant::now();
        h.latch_bypass(OUTPUT_STALLED);
        wait_until("fallback for the stall", || h.fallback());
        assert!(latched.elapsed() < Duration::from_millis(500));

        h.set_wanted(false);
        wait_until("fallback off", || !h.fallback());
        assert_eq!(h.take_fallback_changes(), vec![1, 0]);
    }
}
