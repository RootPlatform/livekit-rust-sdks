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

#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    /// A policy script's outcome.
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
    #[derive(Debug, Clone, Copy, Default)]
    pub struct RootEchoWindowProbe {
        /// The inference that tripped it (-1: none).
        pub tripped_at: i64,
        pub p99_us: f32,
        pub failing_streak: u32,
    }

    /// The echo harness's APM, controller and capture state.
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
        pub format_seq: u64,
        pub erle_db: f64,
        pub pending_tasks: u32,
        /// Measured-echo evidence samples in the current window.
        pub evidence: u32,
    }

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
        /// One AEC3 block (64 samples of band 0); writes 65 residual
        /// fractions. True when it ran an inference. Never allocates.
        fn estimate_block(
            self: &mut RootEchoModelSession,
            render: &[f32],
            linear_out: &[f32],
            fraction: &mut [f32],
        ) -> bool;
    }

    unsafe extern "C++" {
        include!("livekit/root_echo_model.h");

        fn root_echo_model_supported() -> bool;
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
        fn open_dsp_chain(self: &RootEchoHarness);
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

#[cfg(not(any(target_os = "ios", target_os = "android")))]
mod imp {
    use rootapp_echo_model::{Estimator, Tuning};

    pub struct Session(Option<Estimator>);

    pub fn prepare() -> bool {
        rootapp_echo_model::prepare()
    }

    pub fn prepared() -> bool {
        rootapp_echo_model::prepared()
    }

    pub fn session() -> Session {
        Session(rootapp_echo_model::shared().map(|model| Estimator::new(model, Tuning::default())))
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

        pub fn estimate_block(
            &mut self,
            render: &[f32],
            linear_out: &[f32],
            fraction: &mut [f32],
        ) -> bool {
            match &mut self.0 {
                Some(estimator) => estimator.estimate_block(render, linear_out, fraction),
                None => {
                    fraction.iter_mut().for_each(|f| *f = 1.0);
                    false
                }
            }
        }
    }
}

#[cfg(any(target_os = "ios", target_os = "android"))]
mod imp {
    pub struct Session;

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

        pub fn estimate_block(
            &mut self,
            _render: &[f32],
            _linear_out: &[f32],
            fraction: &mut [f32],
        ) -> bool {
            fraction.iter_mut().for_each(|f| *f = 1.0);
            false
        }
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

    fn estimate_block(&mut self, render: &[f32], linear_out: &[f32], fraction: &mut [f32]) -> bool {
        self.0.estimate_block(render, linear_out, fraction)
    }
}
