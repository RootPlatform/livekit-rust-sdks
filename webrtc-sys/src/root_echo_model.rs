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

//! The echo model (`rootapp-echo-model`) for `RootResidualEchoEstimator`,
//! and test hooks for the native decisions. Mobile builds carry no model: the
//! session is never valid there.

#[allow(clippy::too_many_arguments)]
#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    /// A policy script's outcome.
    #[cfg(feature = "harness")]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootEchoPolicyProbe {
        pub on: bool,
        pub reason: u32,
        /// The ERLE sample that latched the device (-1: none).
        pub latched_at: i64,
        pub measured_echo: bool,
        pub evidence: u32,
        pub counted: u32,
    }

    /// A kill-switch script's outcome.
    #[cfg(feature = "harness")]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootEchoWindowProbe {
        /// The inference that tripped it (-1: none).
        pub tripped_at: i64,
        pub p99_us: f32,
        pub failing_streak: u32,
    }

    /// The echo harness's APM, controller and capture state.
    #[cfg(feature = "harness")]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootEchoHarnessProbe {
        pub running_model: bool,
        /// -1 nothing staged, 0 the variant without the model, 1 with it.
        pub staged: i32,
        pub swaps: u64,
        pub has_controller: bool,
        pub mode: u32,
        pub state: u32,
        pub reason: u32,
        pub kind: u32,
        pub inferences: u64,
        pub inference_mean_us: f32,
        pub inference_max_us: u64,
        pub inference_p99_us: f32,
        pub measured_echo: bool,
        pub kill_switch: bool,
        pub processed_frames: u64,
        pub dsp_resets: u64,
        pub dsp_flushes: u64,
        /// The voice gain on the last frame, the engine that applied it (0
        /// the chain's, 1 AGC2) and the AGC2 instances built.
        pub voice_gain_db: f32,
        pub gain_engine: u32,
        pub agc2_builds: u64,
        pub format_seq: u64,
        pub erle_db: f64,
        pub pending_tasks: u32,
        /// Measured-echo evidence samples in the current window.
        pub evidence: u32,
        /// The estimator's far-end view at the last capture frame: active,
        /// the hold, the onset guard and the delay D they follow.
        pub far_end_active: bool,
        pub far_end_hold_ms: i64,
        pub onset_guard_blocks: u32,
        pub far_end_delay_ms: i32,
    }

    #[cfg(feature = "harness")]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootEchoStressProbe {
        pub swaps: u64,
        pub render_frames: u64,
        pub capture_frames: u64,
        /// Statistics reads by the third thread.
        pub reads: u64,
        pub failed_stages: u32,
        pub collected: bool,
    }

    /// `grace_replay`: a reader that read the epoch before switch 1 and
    /// counted itself after it.
    #[cfg(feature = "harness")]
    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootEchoGraceProbe {
        pub first_switched: bool,
        /// Switch 1's APM was released (nobody was counted yet).
        pub first_released: bool,
        /// The reader holds the APM switch 1 made active...
        pub reader_holds_second: bool,
        /// ...counted in the new epoch's parity, not the one it first read.
        pub reader_parity_moved: bool,
        pub second_switched: bool,
        /// Switch 2's collection waits for the reader and keeps its APM.
        pub second_held: bool,
        pub held_usable: bool,
        pub released_after_leave: bool,
    }

    extern "Rust" {
        type RootEchoModelSession;

        /// Decodes the embedded model once per process. Blocking: never on an
        /// audio thread.
        fn root_echo_model_prepare() -> bool;
        fn root_echo_model_prepared() -> bool;
        /// A stream of the prepared model with the default tuning (invalid
        /// before the model is prepared, and on mobile).
        fn new_root_echo_model_session() -> Box<RootEchoModelSession>;
        fn valid(self: &RootEchoModelSession) -> bool;
        fn reset(self: &mut RootEchoModelSession);
        /// Today's enr_suppress per state, from the config AdjustConfig gets.
        fn set_today_masks(
            self: &mut RootEchoModelSession,
            normal_lf: f32,
            normal_hf: f32,
            nearend_lf: f32,
            nearend_hf: f32,
            last_lf_band: usize,
            first_hf_band: usize,
        );
        /// One AEC3 block (estimator-spec.md): render, capture and linear
        /// output channel 0 (64 samples of band 0), and S2, Y2, E2 and R2 of
        /// every capture channel (65 bins each). True when it ran an
        /// inference. Never allocates.
        fn estimate(
            self: &mut RootEchoModelSession,
            render: &[f32],
            y: &[f32],
            e: &[f32],
            s2: &[f32],
            y2: &[f32],
            e2: &[f32],
            dominant_nearend: bool,
            far_end_active: bool,
            onset_guard_blocks: u32,
            r2: &mut [f32],
        ) -> bool;
    }

    unsafe extern "C++" {
        include!("livekit/root_echo_model.h");

        fn root_echo_model_supported() -> bool;
    }

    #[cfg(feature = "harness")]
    unsafe extern "C++" {
        include!("livekit/root_echo_model.h");

        fn root_output_kind_from_windows(form_factor: u32, bluetooth: bool) -> u32;
        fn root_is_bluetooth_instance_id(instance_id: &str) -> bool;
        fn root_output_kind_from_mac(
            transport: u32,
            has_data_source: bool,
            data_source: u32,
            has_terminal: bool,
            terminal: u32,
        ) -> u32;
        fn root_echo_policy_probe(
            mode: u32,
            kind: u32,
            erle_db: &[f64],
            frames_per_sample: u64,
        ) -> RootEchoPolicyProbe;
        fn root_echo_window_probe(us: &[u64], reset_at: i64) -> RootEchoWindowProbe;
        fn root_echo_adjust_config_is_googles_tuning() -> bool;

        type RootEchoHarness;

        fn new_root_echo_harness(today: bool) -> UniquePtr<RootEchoHarness>;
        fn process(
            self: &RootEchoHarness,
            rate: i32,
            render_channels: u32,
            render: &[f32],
            capture: &[f32],
        ) -> Vec<f32>;
        fn process_int16(
            self: &RootEchoHarness,
            rate: i32,
            render_channels: u32,
            render: &[f32],
            capture: &[f32],
        ) -> Vec<f32>;
        fn initialize(self: &RootEchoHarness, rate: i32, render_channels: u32);
        fn set_stream_delay(self: &RootEchoHarness, delay_ms: i32);
        fn open_dsp_chain(self: &RootEchoHarness);
        /// AGC2 as the voice gain from a gain reset, built as the controller does.
        fn use_agc2(self: &RootEchoHarness, gain_reset_db: f32);
        fn prepare_model(self: &RootEchoHarness);
        fn set_mode(self: &RootEchoHarness, mode: u32);
        fn set_device(self: &RootEchoHarness, kind: u32, key: &str);
        fn close_playout(self: &RootEchoHarness);
        fn set_echo_cancellation(self: &RootEchoHarness, on: bool);
        fn reset_session(self: &RootEchoHarness);
        fn trip_kill_switch(self: &RootEchoHarness);
        fn set_speaking(self: &RootEchoHarness, speaking: bool);
        fn advance(self: &RootEchoHarness, ms: i64);
        fn probe(self: &RootEchoHarness) -> RootEchoHarnessProbe;
        fn stress(self: &RootEchoHarness, switches: u32) -> RootEchoStressProbe;
        fn grace_replay(self: &RootEchoHarness) -> RootEchoGraceProbe;
    }
}

/// What the C++ estimator passes for one block.
struct Block<'a> {
    render: &'a [f32],
    y: &'a [f32],
    e: &'a [f32],
    s2: &'a [f32],
    y2: &'a [f32],
    e2: &'a [f32],
    dominant_nearend: bool,
    far_end_active: bool,
    onset_guard_blocks: u32,
}

#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod imp {
    use rootapp_echo_model::{Estimator, Hook, TodayMasks};

    pub struct Session(Option<Estimator>);

    pub fn prepare() -> bool {
        rootapp_echo_model::prepare()
    }

    pub fn prepared() -> bool {
        rootapp_echo_model::prepared()
    }

    pub fn session() -> Session {
        Session(rootapp_echo_model::shared().map(Estimator::new))
    }

    impl Session {
        pub fn valid(&self) -> bool {
            self.0.is_some()
        }

        pub fn reset(&mut self) {
            if let Some(estimator) = &mut self.0 {
                estimator.reset();
            }
        }

        pub fn set_today_masks(&mut self, masks: TodayMasks) {
            if let Some(estimator) = &mut self.0 {
                estimator.set_today_masks(&masks);
            }
        }

        pub fn estimate(&mut self, b: &super::Block, r2: &mut [f32]) -> bool {
            let Some(estimator) = &mut self.0 else {
                super::copy_floor(b.s2, r2);
                return false;
            };
            let hook = Hook {
                render: b.render,
                y: b.y,
                e: b.e,
                s2: b.s2,
                y2: b.y2,
                e2: b.e2,
                dominant_nearend: b.dominant_nearend,
                far_end_active: b.far_end_active,
                onset_guard_blocks: b.onset_guard_blocks,
            };
            estimator.estimate(&hook, r2)
        }
    }

    pub fn masks(
        normal_lf: f32,
        normal_hf: f32,
        nearend_lf: f32,
        nearend_hf: f32,
        last_lf_band: usize,
        first_hf_band: usize,
    ) -> TodayMasks {
        TodayMasks { normal_lf, normal_hf, nearend_lf, nearend_hf, last_lf_band, first_hf_band }
    }
}

#[cfg(any(target_os = "ios", target_os = "android"))]
mod imp {
    pub struct Session;

    pub struct TodayMasks;

    pub fn prepare() -> bool {
        false
    }

    pub fn prepared() -> bool {
        false
    }

    pub fn session() -> Session {
        Session
    }

    impl Session {
        pub fn valid(&self) -> bool {
            false
        }

        pub fn reset(&mut self) {}

        pub fn set_today_masks(&mut self, _masks: TodayMasks) {}

        pub fn estimate(&mut self, b: &super::Block, r2: &mut [f32]) -> bool {
            super::copy_floor(b.s2, r2);
            false
        }
    }

    pub fn masks(_: f32, _: f32, _: f32, _: f32, _: usize, _: usize) -> TodayMasks {
        TodayMasks
    }
}

/// Without a model: AEC3's own linear echo estimate.
fn copy_floor(s2: &[f32], r2: &mut [f32]) {
    for (r, s) in r2.iter_mut().zip(s2) {
        *r = *s;
    }
}

pub struct RootEchoModelSession(imp::Session);

fn root_echo_model_prepare() -> bool {
    imp::prepare()
}

fn root_echo_model_prepared() -> bool {
    imp::prepared()
}

fn new_root_echo_model_session() -> Box<RootEchoModelSession> {
    Box::new(RootEchoModelSession(imp::session()))
}

impl RootEchoModelSession {
    fn valid(&self) -> bool {
        self.0.valid()
    }

    fn reset(&mut self) {
        self.0.reset()
    }

    fn set_today_masks(
        &mut self,
        normal_lf: f32,
        normal_hf: f32,
        nearend_lf: f32,
        nearend_hf: f32,
        last_lf_band: usize,
        first_hf_band: usize,
    ) {
        self.0.set_today_masks(imp::masks(
            normal_lf,
            normal_hf,
            nearend_lf,
            nearend_hf,
            last_lf_band,
            first_hf_band,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn estimate(
        &mut self,
        render: &[f32],
        y: &[f32],
        e: &[f32],
        s2: &[f32],
        y2: &[f32],
        e2: &[f32],
        dominant_nearend: bool,
        far_end_active: bool,
        onset_guard_blocks: u32,
        r2: &mut [f32],
    ) -> bool {
        let block = Block {
            render,
            y,
            e,
            s2,
            y2,
            e2,
            dominant_nearend,
            far_end_active,
            onset_guard_blocks,
        };
        self.0.estimate(&block, r2)
    }
}
