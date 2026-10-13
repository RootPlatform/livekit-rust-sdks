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

//! Native screen-share audio (`root_screen_audio.h`): an OS backend captures
//! what the machine plays and the core paces it at 10 ms into a queued
//! AudioSource.

use crate::impl_thread_safety;

#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    /// RootScreenAudioTarget: kind 0 everything but this process tree, 1 one
    /// process tree, 2 one window's application.
    #[derive(Debug, Clone, Default)]
    pub struct RootScreenAudioTargetSpec {
        pub kind: i32,
        pub process_id: u32,
        pub window_id: u64,
        pub window_title: String,
    }

    /// kind: 1 started, 2 failed, 3 stopped. A stopped event with an error:
    /// the OS capture could not be stopped (message says so).
    #[derive(Debug, Clone, Default)]
    pub struct RootScreenAudioEventData {
        pub kind: u32,
        pub async_id: u64,
        pub scope: i32,
        pub description: String,
        pub sample_rate: u32,
        pub channels: u32,
        pub error: i32,
        pub message: String,
        pub retrying: bool,
    }

    #[derive(Debug, Clone, Default)]
    pub struct RootScreenAudioStatsData {
        pub state: i32,
        pub scope: i32,
        pub capture_rate: u32,
        pub capture_channels: u32,
        pub captured_frames: u64,
        pub silent_capture_frames: u64,
        pub discontinuities: u64,
        pub overflow_frames: u64,
        pub write_max_gap_us: u64,
        pub capture_stalls: u64,
        pub pulls: u64,
        pub pull_max_gap_us: u64,
        pub sent_frames: u64,
        pub silence_frames: u64,
        pub underruns: u64,
        pub gaps: u64,
        pub trimmed_frames: u64,
        pub skipped_frames: u64,
        pub clipped_samples: u64,
        pub sent_sumsq: u64,
        pub sent_samples: u64,
        pub level_frames: i32,
        pub target_frames: i32,
        pub drift_ppm: i32,
        pub restarts: u32,
        pub last_error: i32,
        pub last_error_message: String,
    }

    extern "C++" {
        include!("livekit/audio_track.h");

        type AudioTrackSource = crate::audio_track::ffi::AudioTrackSource;
    }

    unsafe extern "C++" {
        include!("livekit/root_screen_audio.h");

        type RootScreenAudioCaptureHandle;

        /// A backend for this OS is compiled in (the OS itself is checked at
        /// start).
        fn root_screen_audio_backend_built() -> bool;

        /// Attaches a capture to a queued 48 kHz mono or stereo source; null
        /// with `error` set when the source cannot take one.
        fn new_root_screen_audio_capture(
            source: SharedPtr<AudioTrackSource>,
            error: &mut String,
        ) -> UniquePtr<RootScreenAudioCaptureHandle>;

        fn start(
            self: &RootScreenAudioCaptureHandle,
            target: &RootScreenAudioTargetSpec,
            async_id: u64,
        );
        fn stop(self: &RootScreenAudioCaptureHandle, async_id: u64);
        fn stats(self: &RootScreenAudioCaptureHandle) -> RootScreenAudioStatsData;
        fn take_events(self: &RootScreenAudioCaptureHandle) -> Vec<RootScreenAudioEventData>;

        /// Testing: captures made afterwards use a fake backend (a 48 kHz
        /// stereo 440 Hz tone in 10 ms packets from its own thread).
        fn root_screen_audio_use_fake_backend(enabled: bool);

        type RootScreenAudioPipeHarness;

        fn new_root_screen_audio_pipe_harness() -> UniquePtr<RootScreenAudioPipeHarness>;
        fn configure(
            self: Pin<&mut RootScreenAudioPipeHarness>,
            sample_rate: i32,
            channels: i32,
        ) -> bool;
        fn advance_us(self: Pin<&mut RootScreenAudioPipeHarness>, us: i64);
        fn now_us(self: &RootScreenAudioPipeHarness) -> i64;
        fn write(self: Pin<&mut RootScreenAudioPipeHarness>, interleaved: &[f32]);
        fn write_silence(self: Pin<&mut RootScreenAudioPipeHarness>, frames: usize);
        fn discontinuity(self: Pin<&mut RootScreenAudioPipeHarness>);
        fn pull(self: Pin<&mut RootScreenAudioPipeHarness>, channels: i32) -> Vec<i16>;
        fn stats(self: Pin<&mut RootScreenAudioPipeHarness>) -> RootScreenAudioStatsData;
        fn ratio(self: &RootScreenAudioPipeHarness) -> f64;

        type RootScreenAudioCaptureHarness;

        /// A real queued source (48 kHz, `channels`) with a recording sink and a
        /// capture whose backends are scripted fakes; restart delays in ms.
        fn new_root_screen_audio_capture_harness(
            channels: i32,
            restart_delay_ms: i32,
        ) -> UniquePtr<RootScreenAudioCaptureHarness>;
        /// The next backend start returns this (error 0: success with this
        /// format and scope); unscripted starts succeed at 48 kHz stereo.
        fn script_start(
            self: &RootScreenAudioCaptureHarness,
            error: i32,
            sample_rate: i32,
            channels: i32,
            scope: i32,
        );
        /// The next backend start succeeds at 48 kHz stereo and resolves this
        /// scope and process.
        fn script_start_for(self: &RootScreenAudioCaptureHarness, scope: i32, process_id: u32);
        /// The next backend stop returns `ok` after `delay_ms`.
        fn script_stop(self: &RootScreenAudioCaptureHarness, ok: bool, delay_ms: i32);
        /// The running backend reports `error` from its audio thread.
        fn fail_running(self: &RootScreenAudioCaptureHarness, error: i32);
        /// The running backend stops (true) or resumes writing.
        fn pause_running(self: &RootScreenAudioCaptureHarness, paused: bool);
        fn start(
            self: &RootScreenAudioCaptureHarness,
            target: &RootScreenAudioTargetSpec,
            async_id: u64,
        );
        fn stop(self: &RootScreenAudioCaptureHarness, async_id: u64);
        /// Waits until `count` events are queued (or the timeout passed) and
        /// takes them all.
        fn wait_events(
            self: &RootScreenAudioCaptureHarness,
            count: usize,
            timeout_ms: i32,
        ) -> Vec<RootScreenAudioEventData>;
        fn stats(self: &RootScreenAudioCaptureHarness) -> RootScreenAudioStatsData;
        fn backend_starts(self: &RootScreenAudioCaptureHarness) -> u32;
        fn backend_stops(self: &RootScreenAudioCaptureHarness) -> u32;
        fn last_target_kind(self: &RootScreenAudioCaptureHarness) -> i32;
        /// Whether the last backend start was a restart.
        fn last_target_restart(self: &RootScreenAudioCaptureHarness) -> bool;
        /// Frames the source's sinks received, and how many of them were not
        /// silent.
        fn sink_frames(self: &RootScreenAudioCaptureHarness) -> u64;
        fn sink_loud_frames(self: &RootScreenAudioCaptureHarness) -> u64;
        /// Shuts the capture down; returns how long that call took in µs.
        fn shutdown(self: &RootScreenAudioCaptureHarness, wait: bool) -> i64;
        /// Whether the source accepts a host capture_frame now.
        fn source_accepts_capture(self: &RootScreenAudioCaptureHarness) -> bool;
        /// Tries to attach a second capture to the source; the error text.
        fn second_capture_error(self: &RootScreenAudioCaptureHarness) -> String;
        /// The capture error for a source made with these parameters.
        fn capture_error_for_source(sample_rate: i32, channels: i32, queue_size_ms: i32) -> String;
        /// ScreenAudioAncestors over (pid, parent, created) rows.
        fn screen_audio_ancestors(
            self_pid: u32,
            pids: &[u32],
            parents: &[u32],
            created: &[u64],
        ) -> Vec<u32>;
    }
}

impl_thread_safety!(ffi::RootScreenAudioCaptureHandle, Send + Sync);

/// A native screen-audio capture attached to one queued source. Dropping it
/// detaches the capture at once and stops the OS capture on the capture's own
/// thread (never blocks).
pub struct ScreenAudioCapture(cxx::UniquePtr<ffi::RootScreenAudioCaptureHandle>);

impl ScreenAudioCapture {
    /// Attaches a capture to a queued 48 kHz mono or stereo source; the error
    /// says why the source cannot take one.
    pub fn new(
        source: cxx::SharedPtr<crate::audio_track::ffi::AudioTrackSource>,
    ) -> Result<Self, String> {
        let mut error = String::new();
        let capture = ffi::new_root_screen_audio_capture(source, &mut error);
        if capture.is_null() {
            Err(error)
        } else {
            Ok(Self(capture))
        }
    }

    pub fn start(&self, target: &ffi::RootScreenAudioTargetSpec, async_id: u64) {
        self.0.start(target, async_id);
    }

    pub fn stop(&self, async_id: u64) {
        self.0.stop(async_id);
    }

    pub fn stats(&self) -> ffi::RootScreenAudioStatsData {
        self.0.stats()
    }

    pub fn take_events(&self) -> Vec<ffi::RootScreenAudioEventData> {
        self.0.take_events()
    }
}
