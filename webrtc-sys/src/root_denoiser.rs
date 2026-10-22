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
    /// 1 after creation or a flush); process, flush and the power setter each
    /// busy-wait sleep_us.
    #[cfg(feature = "harness")]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootFakeFxSpec {
        pub scale: f32,
        pub delay_samples: u32,
        pub burst_call: u32,
        pub empty_from: u32,
        pub empty_count: u32,
        pub sleep_us: u32,
    }

    #[cfg(feature = "harness")]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootFakeFxCounts {
        pub process_calls: u64,
        pub flush_calls: u64,
        pub power_sets: u64,
        pub last_power: f32,
        pub calls_after_poison: u64,
        pub releases: u64,
    }

    #[cfg(feature = "harness")]
    #[derive(Debug, Clone, Default)]
    pub struct RootDenoiseTestFrame {
        /// 0 not in use, 1 missed, 2 ran.
        pub kind: u8,
        pub samples: usize,
        pub restart: bool,
        pub out: Vec<f32>,
    }

    #[cfg(feature = "harness")]
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

    #[cfg(feature = "harness")]
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct RootDenoiserTestCompletion {
        pub owner: u64,
        pub async_id: u64,
        /// Empty on success.
        pub error: String,
        /// With an error: the pipeline native let go of and lists nowhere
        /// else (superseded, busy); 0 otherwise.
        pub pipeline: u64,
    }

    #[cfg(feature = "harness")]
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct RootDenoiserTestReturned {
        pub owner: u64,
        pub pipeline: u64,
    }

    #[cfg(feature = "harness")]
    #[derive(Debug, Clone, Default)]
    pub struct RootDenoiserTestReturnAll {
        pub pipelines: Vec<u64>,
        pub error: String,
    }

    unsafe extern "C++" {
        include!("livekit/root_denoiser.h");

        fn root_denoiser_notification_counts() -> RootDenoiserNotificationCounts;
        /// The handler's address, 0 where the denoiser is not built.
        fn root_denoiser_notification_handler() -> usize;
        fn root_denoiser_supported() -> bool;
    }

    #[cfg(feature = "harness")]
    unsafe extern "C++" {
        include!("livekit/root_denoiser.h");

        type RootDenoiserHarness;

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
        /// A thread running frames back to back, latching an output stall
        /// after every `latch_every`-th one (0 never).
        fn start_audio(self: &RootDenoiserHarness, rate: u32, latch_every: u32);
        fn stop_audio(self: &RootDenoiserHarness) -> u64;
        fn set_speaking(self: &RootDenoiserHarness, speaking: bool);
        fn set_wanted(self: &RootDenoiserHarness, wanted: bool);
        fn set_process_rate(self: &RootDenoiserHarness, rate: u32);
        /// Whether the control side sees frames being processed (from the
        /// start it does).
        fn set_frames_flowing(self: &RootDenoiserHarness, flowing: bool);
        fn bump_stream_epoch(self: &RootDenoiserHarness);
        fn request(self: &RootDenoiserHarness, owner: u64, async_id: u64, pipeline: u64, rate: u32);
        fn wake(self: &RootDenoiserHarness);
        fn return_all(self: &RootDenoiserHarness) -> RootDenoiserTestReturnAll;
        fn reset(self: &RootDenoiserHarness);
        fn take_completions(self: &RootDenoiserHarness) -> Vec<RootDenoiserTestCompletion>;
        fn take_returned(self: &RootDenoiserHarness) -> Vec<RootDenoiserTestReturned>;
        fn take_fallback_changes(self: &RootDenoiserHarness) -> Vec<u8>;
        fn fallback(self: &RootDenoiserHarness) -> bool;
        /// What the control last decided through the fallback_decided hook.
        fn fallback_decided(self: &RootDenoiserHarness) -> bool;
        /// While set, set_fallback waits (an APM re-initialisation that hangs).
        fn block_fallback(self: &RootDenoiserHarness, block: bool);
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
            processed_frames: u64,
            stream_epoch: u64,
            arriving: bool,
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

    fn ok(owner: u64, async_id: u64) -> RootDenoiserTestCompletion {
        RootDenoiserTestCompletion { owner, async_id, error: String::new(), pipeline: 0 }
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
    fn root_denoiser_rate_mismatch_bypasses_until_the_rate_or_the_pipeline_matches() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        h.install_now(a, 16_000);
        assert_eq!(run(&h, 16_000, 1.0).kind, RAN);
        assert_eq!(run(&h, 48_000, 1.0).kind, NOT_IN_USE);
        assert_eq!(h.slot_info(false).bypass, RATE_MISMATCH);
        assert_eq!(run(&h, 48_000, 1.0).kind, NOT_IN_USE, "the bypass stays while it differs");
        assert_eq!(h.slot_info(false).bypassed_frames, 2);
        assert_eq!(h.fake_counts(a).process_calls, 1);

        let back = run(&h, 16_000, 1.0);
        assert_eq!((back.kind, back.restart), (RAN, true), "the rate is back: it starts over");
        assert_eq!(h.slot_info(false).bypass, 0);
        assert_eq!(h.fake_counts(a).flush_calls, 1, "flushed before it runs again");
        assert!(!run(&h, 16_000, 1.0).restart);

        h.latch_bypass(SLOW);
        assert_eq!(run(&h, 16_000, 1.0).kind, NOT_IN_USE, "only a rate mismatch clears itself");

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
        // The fake spins for 12 ms, longer than the 10 ms frame; a plain call
        // preempted past 20 ms latches on its own, so that run is tried again.
        let mut attempts = 0;
        let h = loop {
            attempts += 1;
            let h = harness();
            let a = h.add_fake(&RootFakeFxSpec { sleep_us: 12_000, ..fake(1.0) });
            h.install_now(a, 48_000);
            let mut preempted = false;
            for call in 0..4 {
                assert_eq!(run(&h, 48_000, 1.0).kind, RAN);
                let info = h.slot_info(true);
                preempted = call > 0 && info.process_max_us > 20_000;
                if preempted {
                    break;
                }
                if call < 3 {
                    assert_eq!(info.bypass, 0, "a warm-up and two slow calls are not enough");
                }
            }
            if !preempted {
                break h;
            }
            assert!(attempts < 5, "every run had a call preempted past 20 ms");
        };
        let info = h.slot_info(false);
        assert_eq!((info.slow_frames, info.bypass), (4, SLOW), "every slow call is counted");
        assert_eq!(run(&h, 48_000, 1.0).kind, NOT_IN_USE);
        assert_eq!(info.histogram[14..].iter().sum::<u64>(), 4, "{:?}", info.histogram);

        let b = h.add_fake(&RootFakeFxSpec { sleep_us: 25_000, ..fake(1.0) });
        h.install_now(b, 48_000);
        assert_eq!(run(&h, 48_000, 1.0).kind, RAN);
        let warm_up = h.slot_info(true);
        assert_eq!(warm_up.bypass, 0, "the first call after an install is a warm-up");
        assert!(warm_up.process_max_us >= 25_000, "still measured: {}", warm_up.process_max_us);
        assert_eq!(
            (warm_up.slow_frames, warm_up.histogram[15]),
            (5, 2),
            "both warm-ups (power and process: two spins each) over 20 ms"
        );
        assert_eq!(run(&h, 48_000, 1.0).kind, RAN);
        assert_eq!(h.slot_info(false).bypass, SLOW, "one later call over 20 ms");
        assert!(h.slot_info(true).process_max_us >= 20_000);
        assert_eq!(h.slot_info(true).process_max_us, 0, "the maximum resets on read");
    }

    /// A call over 5 ms that fits in the 10 ms frame is counted slow and keeps
    /// up: any number of them never latch the bypass (0.9.153: 11 of the 14
    /// callers bypassed at 5 ms had no call over 9.6 ms, and 9 ended the call
    /// with the Effects SDK off).
    #[test]
    fn root_denoiser_slow_calls_within_the_frame_never_latch() {
        // The fake spins for 7 ms; a run with a call preempted past the frame
        // is tried again.
        for attempt in 1.. {
            let h = harness();
            let a = h.add_fake(&RootFakeFxSpec { sleep_us: 7_000, ..fake(1.0) });
            h.install_now(a, 48_000);
            // The warm-up sets the power too: two spins, over the frame.
            assert_eq!(run(&h, 48_000, 1.0).kind, RAN);
            h.slot_info(true);
            let mut over_frame = false;
            for _ in 0..10 {
                assert_eq!(run(&h, 48_000, 1.0).kind, RAN);
                over_frame |= h.slot_info(true).process_max_us > 10_000;
            }
            if over_frame {
                assert!(attempt < 5, "every run had a call preempted past 10 ms");
                continue;
            }
            let info = h.slot_info(false);
            assert_eq!((info.slow_frames, info.bypass), (11, 0), "counted slow, not bypassed");
            assert_eq!(info.histogram[13], 10, "{:?}", info.histogram);
            assert_eq!(run(&h, 48_000, 1.0).kind, RAN);
            break;
        }
    }

    /// Stream restarts, device flaps and power changes in a minute never
    /// bypass a denoiser that keeps up with plain frames.
    #[test]
    fn root_denoiser_warm_ups_flushes_and_power_changes_never_count_toward_the_slow_latch() {
        // A plain call preempted past 20 ms latches on its own, so that run is
        // tried again.
        let plain = |h: &RootDenoiserHarness| {
            assert_eq!(h.process(48_000, &frame(48_000, 1.0), false, 0.8, 3840).kind, RAN);
            h.slot_info(true).process_max_us <= 20_000
        };
        for attempt in 1.. {
            let h = harness();
            let a = h.add_fake(&fake(1.0));
            h.install_now(a, 48_000);
            run(&h, 48_000, 1.0);
            h.set_fake_sleep_us(a, 25_000);
            for _ in 0..3 {
                let flushed = h.process(48_000, &frame(48_000, 1.0), true, 0.5, 3840);
                assert!(flushed.restart);
            }
            let info = h.slot_info(true);
            assert_eq!((info.bypass, info.slow_frames), (0, 3), "three slow flushes: counted only");
            assert!(info.process_max_us >= 50_000, "flush and process: {}", info.process_max_us);

            h.set_fake_sleep_us(a, 12_000);
            for power in [0.6, 0.7, 0.8] {
                assert_eq!(h.process(48_000, &frame(48_000, 1.0), false, power, 3840).kind, RAN);
            }
            let info = h.slot_info(true);
            assert_eq!((info.bypass, info.slow_frames), (0, 6), "power changes neither");
            if !(plain(&h) && plain(&h)) {
                assert!(attempt < 5, "every run had a plain call preempted past 20 ms");
                continue;
            }
            assert_eq!(h.slot_info(false).bypass, 0, "two plain slow calls");
            plain(&h);
            assert_eq!(h.slot_info(false).bypass, SLOW, "the third plain one latches");
            break;
        }
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
    fn root_denoiser_times_flush_and_power_with_process() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        h.install_now(a, 48_000);
        run(&h, 48_000, 1.0);
        run(&h, 48_000, 1.0);
        h.slot_info(true);
        h.set_fake_sleep_us(a, 2_000);
        let flushed = h.process(48_000, &frame(48_000, 1.0), true, 0.25, 3840);
        assert!(flushed.restart);
        let counts = h.fake_counts(a);
        assert_eq!((counts.flush_calls, counts.power_sets), (1, 2));
        let info = h.slot_info(true);
        assert!(info.process_max_us >= 6_000, "flush, power and process: {}", info.process_max_us);
        assert!(info.slow_frames >= 1, "together over 5 ms");
        assert_eq!(info.histogram.iter().sum::<u64>(), 3, "one bucket per frame");
    }

    #[test]
    fn root_denoiser_install_and_remove_while_the_audio_thread_runs() {
        let h = harness();
        h.start_audio(48_000, 0);
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

    /// The fallback state machine with frames flowing (one a millisecond) on
    /// the first stream.
    fn update(
        h: &RootDenoiserHarness,
        now_ms: i64,
        wanted: bool,
        installed: bool,
        bypass: u32,
        pipeline_rate: u32,
        process_rate: u32,
    ) -> i8 {
        let frames = now_ms as u64 + 1;
        h.fallback_update(
            now_ms,
            wanted,
            installed,
            bypass,
            pipeline_rate,
            process_rate,
            frames,
            0,
            false,
        )
    }

    #[test]
    fn root_denoiser_fallback_turns_on_at_once_for_a_dead_denoiser() {
        let h = harness();
        assert_eq!(update(&h, 0, true, true, 0, 48_000, 48_000), -1);
        assert_eq!(update(&h, 10, true, true, OUTPUT_STALLED, 48_000, 48_000), 1);
        assert_eq!(update(&h, 20, true, false, 0, 0, 48_000), -1, "removed: stays on");
        assert_eq!(update(&h, 30, true, true, 0, 48_000, 48_000), 0, "a new one runs");
        assert_eq!(update(&h, 40, true, true, SLOW, 48_000, 48_000), 1);
        assert_eq!(update(&h, 50, false, true, SLOW, 48_000, 48_000), 0, "not wanted");
        assert_eq!(h.fallback_update(60, true, true, SLOW, 48_000, 48_000, 50, 0, false), 1);
        assert_eq!(
            h.fallback_update(5000, true, false, 0, 0, 48_000, 50, 0, false),
            -1,
            "on stays on while no frames flow"
        );
    }

    #[test]
    fn root_denoiser_fallback_waits_a_grace_period_for_a_missing_denoiser() {
        let h = harness();
        assert_eq!(update(&h, 0, true, false, 0, 0, 48_000), -1);
        assert_eq!(update(&h, 999, true, false, 0, 0, 48_000), -1);
        assert_eq!(update(&h, 1000, true, false, 0, 0, 48_000), 1);
        assert_eq!(update(&h, 1010, true, true, 0, 48_000, 48_000), 0);

        assert_eq!(update(&h, 2000, true, true, RATE_MISMATCH, 48_000, 16_000), -1);
        assert_eq!(update(&h, 2999, true, true, RATE_MISMATCH, 48_000, 16_000), -1);
        assert_eq!(update(&h, 3000, true, true, RATE_MISMATCH, 48_000, 16_000), 1);
        assert_eq!(update(&h, 3010, true, true, 0, 16_000, 16_000), 0);

        assert_eq!(update(&h, 4000, true, true, 0, 48_000, 16_000), -1);
        assert_eq!(update(&h, 4500, true, true, 0, 48_000, 48_000), -1, "active again");
        assert_eq!(update(&h, 5400, true, true, 0, 48_000, 16_000), -1);
        assert_eq!(update(&h, 6399, true, true, 0, 48_000, 16_000), -1, "the grace starts over");
        assert_eq!(update(&h, 6400, true, true, 0, 48_000, 16_000), 1);

        assert_eq!(update(&h, 7000, false, false, 0, 0, 0), 0);
        assert_eq!(update(&h, 9000, false, false, 0, 0, 0), -1, "not wanted: never");
        assert_eq!(
            update(&h, 9000, true, true, 0, 48_000, 0),
            -1,
            "no frame yet: the rate is unknown"
        );
    }

    #[test]
    fn root_denoiser_fallback_grace_counts_only_while_frames_flow() {
        let h = harness();
        assert_eq!(
            h.fallback_update(0, true, false, 0, 0, 0, 0, 0, false),
            -1,
            "wanted at the first control send"
        );
        assert_eq!(
            h.fallback_update(5000, true, false, 0, 0, 0, 0, 0, false),
            -1,
            "no frames, no grace"
        );
        assert_eq!(
            h.fallback_update(6000, true, false, 0, 0, 48_000, 1, 1, false),
            -1,
            "recording"
        );
        assert_eq!(h.fallback_update(6500, true, false, 0, 0, 48_000, 50, 1, false), -1);
        assert_eq!(h.fallback_update(6999, true, false, 0, 0, 48_000, 99, 1, false), -1);
        assert_eq!(
            h.fallback_update(7000, true, false, 0, 0, 48_000, 100, 1, false),
            1,
            "1 s of frames"
        );
    }

    #[test]
    fn root_denoiser_fallback_grace_starts_over_when_frames_stop_or_a_stream_starts() {
        let h = harness();
        assert_eq!(h.fallback_update(0, true, false, 0, 0, 48_000, 1, 0, false), -1);
        assert_eq!(h.fallback_update(600, true, false, 0, 0, 48_000, 60, 0, false), -1);
        assert_eq!(
            h.fallback_update(800, true, false, 0, 0, 48_000, 60, 0, false),
            -1,
            "200 ms, no frame"
        );
        assert_eq!(
            h.fallback_update(900, true, false, 0, 0, 48_000, 61, 0, false),
            -1,
            "frames again"
        );
        assert_eq!(h.fallback_update(1899, true, false, 0, 0, 48_000, 160, 0, false), -1);
        assert_eq!(h.fallback_update(1900, true, false, 0, 0, 48_000, 161, 0, false), 1);

        let h = harness();
        assert_eq!(h.fallback_update(0, true, false, 0, 0, 48_000, 1, 0, false), -1);
        assert_eq!(h.fallback_update(900, true, false, 0, 0, 48_000, 90, 0, false), -1);
        assert_eq!(
            h.fallback_update(910, true, false, 0, 0, 48_000, 91, 1, false),
            -1,
            "a new stream"
        );
        assert_eq!(h.fallback_update(1909, true, false, 0, 0, 48_000, 190, 1, false), -1);
        assert_eq!(h.fallback_update(1910, true, false, 0, 0, 48_000, 191, 1, false), 1);
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
        assert_eq!(done, vec![ok(1, 10)]);
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
        assert_eq!(superseded[0].pipeline, h.fake_pipeline(a), "let go of, for its owner");
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
        assert_eq!(done[0].pipeline, h.fake_pipeline(a));
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
        assert_eq!(done[0].pipeline, 0, "it is in the response");
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
        run(&h, 48_000, 1.0);
        h.set_speaking(true);
        h.request(2, 11, h.fake_pipeline(b), 48_000);
        wait_until("b waiting", || h.pending() == 1);
        h.set_wanted(true);
        h.set_process_rate(48_000);
        h.latch_bypass(OUTPUT_STALLED);
        h.wake();
        wait_until("fallback", || h.fallback());
        assert_eq!(fallback_changes(&h, 1), vec![1], "a dead denoiser: b on its way or not");

        h.set_wanted(false);
        h.reset();
        assert!(!h.fallback() && !h.fallback_decided());
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
        assert_eq!(done[0].pipeline, 0, "it is on the returned list");
        assert!(!h.slot_info(false).installed);
        assert_eq!(h.take_fallback_changes(), vec![0], "applied before reset returns");
    }

    #[test]
    fn root_denoiser_handing_over_the_installed_pipeline_again_changes_nothing() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        let pa = h.fake_pipeline(a);
        h.request(1, 10, pa, 48_000);
        completions(&h, 1);
        run(&h, 48_000, 1.0);
        h.set_speaking(true);
        h.request(2, 11, pa, 48_000);
        assert_eq!(completions(&h, 1), vec![ok(2, 11)], "no wait and no error");
        assert!(h.take_returned().is_empty(), "never given back while installed");
        let info = h.slot_info(false);
        assert_eq!((info.installed, info.installs), (true, 1), "nothing swapped");
        assert!(!run(&h, 48_000, 1.0).restart);

        h.request(2, 12, 0, 0);
        completions(&h, 1);
        assert_eq!(
            h.take_returned(),
            vec![RootDenoiserTestReturned { owner: 2, pipeline: pa }],
            "once, to the newest owner"
        );
    }

    #[test]
    fn root_denoiser_a_pipeline_queued_twice_is_never_given_back_while_held() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        let b = h.add_fake(&fake(1.0));
        let (pa, pb) = (h.fake_pipeline(a), h.fake_pipeline(b));
        h.request(1, 10, pa, 48_000);
        completions(&h, 1);
        h.set_speaking(true);
        h.request(1, 11, pb, 48_000);
        wait_until("b waiting", || h.pending() == 1);
        h.request(1, 12, pa, 48_000);
        let done = completions(&h, 2);
        assert_eq!((done[0].async_id, done[0].pipeline), (11, pb));
        assert!(done[0].error.contains("superseded"));
        assert_eq!(done[1], ok(1, 12), "a, still installed, at once");
        assert!(h.take_returned().is_empty());
        assert_eq!(h.slot_info(false).installs, 1);

        let c = h.add_fake(&fake(1.0));
        let pc = h.fake_pipeline(c);
        h.request(1, 13, pc, 48_000);
        wait_until("c waiting", || h.pending() == 1);
        h.request(2, 14, pc, 48_000);
        assert_eq!(completions(&h, 1), vec![ok(1, 13)], "superseded by itself: still held");
        h.set_speaking(false);
        assert_eq!(completions(&h, 1), vec![ok(2, 14)]);
        assert_eq!(
            h.take_returned(),
            vec![RootDenoiserTestReturned { owner: 1, pipeline: pa }],
            "c replaced a"
        );
    }

    #[test]
    fn root_denoiser_a_replaced_pipeline_queued_again_is_not_given_back() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        let b = h.add_fake(&fake(1.0));
        let (pa, pb) = (h.fake_pipeline(a), h.fake_pipeline(b));
        h.request(1, 10, pa, 48_000);
        completions(&h, 1);
        assert!(h.hold_slot(true));
        h.request(1, 11, pb, 48_000);
        wait_until("b being installed", || h.pending() == 0);
        h.request(1, 12, pa, 48_000);
        assert_eq!(h.pending(), 1);
        h.hold_slot(false);
        let done = completions(&h, 2);
        assert_eq!(done, vec![ok(1, 11), ok(1, 12)]);
        assert_eq!(
            h.take_returned(),
            vec![RootDenoiserTestReturned { owner: 1, pipeline: pb }],
            "a was queued again when b replaced it"
        );
        assert_eq!(h.slot_info(false).installs, 3);
    }

    #[test]
    fn root_denoiser_a_returned_pipeline_handed_over_before_it_was_taken_is_held_again() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        let b = h.add_fake(&fake(1.0));
        let (pa, pb) = (h.fake_pipeline(a), h.fake_pipeline(b));
        h.request(1, 10, pa, 48_000);
        completions(&h, 1);
        h.request(1, 11, pb, 48_000);
        completions(&h, 1);
        h.request(1, 12, pa, 48_000);
        completions(&h, 1);
        assert_eq!(h.take_returned(), vec![RootDenoiserTestReturned { owner: 1, pipeline: pb }]);
    }

    #[test]
    fn root_denoiser_return_all_never_lists_a_pipeline_still_being_installed() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        let pa = h.fake_pipeline(a);
        assert!(h.hold_slot(true));
        h.request(1, 10, pa, 48_000);
        wait_until("a being installed", || h.pending() == 0);
        h.request(1, 11, pa, 48_000);
        let all = h.return_all();
        assert!(all.error.contains("stays installed"), "{}", all.error);
        assert!(!all.pipelines.contains(&pa), "{:?}", all.pipelines);
        let mut done = completions(&h, 2);
        h.hold_slot(false);
        done.sort_by_key(|c| c.async_id);
        assert!(done[0].error.contains("did not let go"));
        assert_eq!(done[0].pipeline, pa, "released once, by the request that failed");
        assert_eq!(done[1], ok(1, 11), "held while drained");
    }

    #[test]
    fn root_denoiser_return_all_shares_one_budget_with_a_change_in_progress() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        let b = h.add_fake(&fake(1.0));
        h.request(1, 10, h.fake_pipeline(a), 48_000);
        completions(&h, 1);
        assert!(h.hold_slot(true));
        h.request(1, 11, h.fake_pipeline(b), 48_000);
        wait_until("b being installed", || h.pending() == 0);
        let started = Instant::now();
        let all = h.return_all();
        let took = started.elapsed();
        assert!(took < Duration::from_millis(300), "{took:?}");
        assert!(all.error.contains("stays installed"));
        assert!(all.pipelines.is_empty());
        let done = completions(&h, 1);
        h.hold_slot(false);
        assert_eq!((done[0].async_id, done[0].pipeline), (11, h.fake_pipeline(b)));
        assert!(h.slot_info(false).installed, "a stays");
    }

    #[test]
    fn root_denoiser_effects_thread_waits_for_frames_before_the_grace() {
        let h = new_root_denoiser_harness(3000, 100);
        h.set_frames_flowing(false);
        h.set_process_rate(48_000);
        h.set_wanted(true);
        h.wake();
        std::thread::sleep(Duration::from_millis(300));
        assert!(!h.fallback(), "no frames yet: no fallback");
        let started = Instant::now();
        h.set_frames_flowing(true);
        wait_until("fallback after the grace", || h.fallback());
        assert!(started.elapsed() >= Duration::from_millis(95));

        h.set_wanted(false);
        wait_until("fallback off", || !h.fallback());
        h.set_wanted(true);
        h.wake();
        std::thread::sleep(Duration::from_millis(50));
        h.bump_stream_epoch();
        let restarted = Instant::now();
        wait_until("fallback after a new stream's grace", || h.fallback());
        assert!(restarted.elapsed() >= Duration::from_millis(90));
    }

    #[test]
    fn root_denoiser_stall_latches_racing_swaps_only_bypass_a_pipeline_that_ran() {
        let h = harness();
        h.start_audio(48_000, 3);
        let mut fakes = Vec::new();
        let mut installed: Option<usize> = None;
        for _ in 0..300 {
            let next = h.add_fake(&fake(0.5));
            fakes.push(next);
            let previous = h.swap(h.fake_pipeline(next), 48_000);
            assert_ne!(previous, u64::MAX, "the audio thread let go within 200 ms");
            assert_eq!(previous, installed.map_or(0, |p| h.fake_pipeline(p)));
            if let Some(p) = installed {
                h.poison_fake(p);
            }
            installed = Some(next);
            std::thread::sleep(Duration::from_micros(200));
        }
        h.stop_audio();
        let after_return: u64 = fakes.iter().map(|f| h.fake_counts(*f).calls_after_poison).sum();
        assert_eq!(after_return, 0, "a returned pipeline was called again");
        let info = h.slot_info(false);
        assert!(info.bypassed_frames > 0, "the latches landed");
        if h.fake_counts(installed.unwrap()).process_calls == 0 {
            assert_eq!(info.bypass, 0, "a pipeline that never ran is never bypassed");
        }
        let fresh = h.add_fake(&fake(0.5));
        h.swap(h.fake_pipeline(fresh), 48_000);
        h.latch_bypass(OUTPUT_STALLED);
        assert_eq!(h.slot_info(false).bypass, 0, "the report was for the replaced pipeline");
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
        assert_eq!(fallback_changes(&h, 1), vec![1]);
        assert!(h.fallback_decided());

        let a = h.add_fake(&fake(1.0));
        h.request(1, 10, h.fake_pipeline(a), 48_000);
        completions(&h, 1);
        wait_until("the denoiser runs", || !h.fallback());
        assert_eq!(fallback_changes(&h, 1), vec![0]);

        run(&h, 48_000, 1.0);
        let latched = Instant::now();
        h.latch_bypass(OUTPUT_STALLED);
        wait_until("fallback for the stall", || h.fallback());
        assert!(latched.elapsed() < Duration::from_millis(500));

        h.set_wanted(false);
        wait_until("fallback off", || !h.fallback());
        assert_eq!(fallback_changes(&h, 2), vec![1, 0]);
    }

    /// The fallback changes set_fallback was handed, once `count` arrived.
    fn fallback_changes(h: &RootDenoiserHarness, count: usize) -> Vec<u8> {
        let mut all = Vec::new();
        wait_until("fallback changes", || {
            all.extend(h.take_fallback_changes());
            all.len() >= count
        });
        all
    }

    #[test]
    fn root_denoiser_fallback_grace_waits_for_an_install_on_its_way() {
        let h = harness();
        let arriving = |now_ms: i64, arriving: bool| {
            h.fallback_update(now_ms, true, false, 0, 0, 48_000, now_ms as u64 + 1, 0, arriving)
        };
        assert_eq!(arriving(0, true), -1);
        assert_eq!(arriving(2999, true), -1, "no grace while the install waits");
        assert_eq!(arriving(3000, false), -1, "it starts once nothing is on its way");
        assert_eq!(arriving(3999, false), -1);
        assert_eq!(arriving(4000, false), 1);
        assert_eq!(arriving(4010, true), -1, "on stays on until a denoiser runs");

        let h = harness();
        let dead = |now_ms: i64, bypass: u32| {
            h.fallback_update(
                now_ms,
                true,
                true,
                bypass,
                48_000,
                48_000,
                now_ms as u64 + 1,
                0,
                true,
            )
        };
        assert_eq!(dead(0, 0), -1);
        assert_eq!(dead(10, OUTPUT_STALLED), 1, "a dead denoiser never waits");
        assert_eq!(dead(20, 0), 0);
        assert_eq!(dead(30, SLOW), 1);
        let h = harness();
        assert_eq!(
            h.fallback_update(0, true, true, RATE_MISMATCH, 16_000, 48_000, 1, 0, true),
            -1,
            "a pipeline at the new rate on its way"
        );
        assert_eq!(
            h.fallback_update(1500, true, true, RATE_MISMATCH, 16_000, 48_000, 150, 0, true),
            -1
        );
    }

    /// A pipeline handed over while the user talks waits for a pause, up to
    /// the install wait; the missing-denoiser grace does not run out meanwhile,
    /// so WebRTC NS is not turned on just before the pipeline lands.
    #[test]
    fn root_denoiser_effects_thread_holds_the_grace_while_an_install_waits_for_a_pause() {
        let h = new_root_denoiser_harness(600, 100);
        h.set_process_rate(48_000);
        h.set_wanted(true);
        h.set_speaking(true);
        let a = h.add_fake(&fake(1.0));
        h.request(1, 10, h.fake_pipeline(a), 48_000);
        std::thread::sleep(Duration::from_millis(300));
        assert!(!h.fallback(), "three graces long, the install still waiting");
        assert_eq!(h.pending(), 1);
        h.set_speaking(false);
        assert_eq!(completions(&h, 1), vec![ok(1, 10)]);
        std::thread::sleep(Duration::from_millis(150));
        assert!(!h.fallback());
        assert!(h.take_fallback_changes().is_empty(), "WebRTC NS never toggled");

        let b = h.add_fake(&fake(1.0));
        h.set_speaking(true);
        assert!(h.hold_slot(true));
        h.request(1, 11, h.fake_pipeline(b), 16_000);
        h.set_process_rate(16_000);
        let started = Instant::now();
        let failed = completions(&h, 1);
        assert!(failed[0].error.contains("did not let go"), "{:?}", failed[0]);
        assert!(started.elapsed() >= Duration::from_millis(550), "it waited the install wait");
        h.hold_slot(false);
        wait_until("fallback once nothing is on its way", || h.fallback());
        assert_eq!(fallback_changes(&h, 1), vec![1]);
    }

    #[test]
    fn root_denoiser_a_rate_the_chain_never_runs_fails_unless_native_holds_the_pipeline() {
        let h = harness();
        let a = h.add_fake(&fake(1.0));
        let b = h.add_fake(&fake(1.0));
        let (pa, pb) = (h.fake_pipeline(a), h.fake_pipeline(b));
        h.request(1, 10, pa, 7_999);
        let failed = completions(&h, 1);
        assert!(failed[0].error.contains("outside 8000-48000 Hz"), "{:?}", failed[0]);
        assert_eq!(failed[0].pipeline, pa, "never held: the app releases it");
        assert_eq!(h.pending(), 0);

        h.request(1, 11, pa, 48_000);
        completions(&h, 1);
        h.request(2, 12, pa, 96_000);
        assert_eq!(completions(&h, 1), vec![ok(2, 12)], "installed: nothing changes");
        assert!(h.slot_info(false).installed && h.slot_info(false).rate == 48_000);

        h.request(1, 13, pb, 48_000);
        completions(&h, 1);
        h.request(1, 14, pa, 0);
        assert_eq!(completions(&h, 1), vec![ok(1, 14)], "returned, not taken back yet");
        assert_eq!(
            h.take_returned(),
            vec![RootDenoiserTestReturned { owner: 1, pipeline: pa }],
            "it still comes back"
        );
    }

    /// set_fallback can wait on an APM re-initialisation (and a capture frame
    /// with the SDK in it); requests and the FFI's takes never wait for it.
    #[test]
    fn root_denoiser_applying_the_fallback_blocks_no_request() {
        struct Unblock<'a>(&'a RootDenoiserHarness);
        impl Drop for Unblock<'_> {
            fn drop(&mut self) {
                self.0.block_fallback(false);
            }
        }
        let h = new_root_denoiser_harness(3000, 50);
        let _unblock = Unblock(&h);
        h.set_process_rate(48_000);
        h.block_fallback(true);
        h.set_wanted(true);
        h.wake();
        wait_until("decided", || h.fallback() && h.fallback_decided());
        std::thread::sleep(Duration::from_millis(20));
        let started = Instant::now();
        let a = h.add_fake(&fake(1.0));
        h.request(1, 10, h.fake_pipeline(a), 48_000);
        assert!(h.take_completions().is_empty());
        let _ = h.pending();
        assert!(started.elapsed() < Duration::from_millis(50), "{:?}", started.elapsed());
        assert!(h.take_fallback_changes().is_empty(), "still being applied");
        h.block_fallback(false);
        completions(&h, 1);
        wait_until("the denoiser runs", || !h.fallback());
        assert_eq!(fallback_changes(&h, 2), vec![1, 0], "applied in the order decided");
    }
}
