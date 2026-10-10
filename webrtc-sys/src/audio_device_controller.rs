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

use std::sync::Mutex;

pub use cxx::SharedPtr;

use crate::impl_thread_safety;

/// `stop_result` / `init_result` / `start_result` before an attempt.
pub const ADM_RESULT_NOT_ATTEMPTED: i32 = 1;
/// `init_result` when the selected device is not present (no ADM returns it).
pub const ADM_RESULT_DEVICE_NOT_FOUND: i32 = -10000;

#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[repr(u8)]
    pub enum AudioDeviceSelectionKind {
        Default = 0,
        Guid = 1,
        Name = 2,
        Index = 3,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct AudioDeviceSelection {
        pub kind: AudioDeviceSelectionKind,
        pub id: String,
        pub index: u16,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AudioDeviceOutcome {
        pub requested: bool,
        pub found: bool,
        pub attempted: bool,
        pub stop_result: i32,
        pub init_result: i32,
        pub start_result: i32,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AudioDeviceRequestOutcome {
        pub recording: AudioDeviceOutcome,
        pub playout: AudioDeviceOutcome,
    }

    #[derive(Debug, Clone, Default)]
    pub struct RootAudioNativeStats {
        pub capture_callbacks: u64,
        pub render_callbacks: u64,
        pub capture_max_gap_us: u64,
        pub render_max_gap_us: u64,
        pub last_capture_age_us: u64,
        pub last_render_age_us: u64,
        pub recording: bool,
        pub playing: bool,
        pub recording_init_result: i32,
        pub recording_start_result: i32,
        pub playout_init_result: i32,
        pub playout_start_result: i32,
        pub capture_rate: u32,
        pub capture_channels: u32,
        pub capture_raw_sumsq: f64,
        pub capture_raw_samples: u64,
        pub playout_delay_ms: u32,
        pub recording_delay_ms: u32,
        pub mix_calls: u64,
        pub mix_max_gap_us: u64,
        pub mix_sources: u32,
        pub mix_ssrcs: Vec<u32>,
        pub mix_out_sumsq: f64,
        pub mix_out_samples: u64,
        pub effects_played: u64,
        pub effects_dropped: u64,
        pub processed_frames: u64,
        pub process_rate: u32,
        pub process_channels: u32,
        pub apm_out_sumsq: f64,
        pub sent_sumsq: f64,
        pub stage_samples: u64,
        pub erl_db: f64,
        pub erle_db: f64,
        pub aec_delay_ms: i32,
        pub divergent_filter_fraction: f64,
        pub echo_cancellation_active: bool,
        pub webrtc_ns_active: bool,
        pub audio_thread_lock_violations: u64,
        pub audio_thread_logs_dropped: u64,
        pub overlapping_callbacks: u64,
        pub device_stop_failures: u64,

        /// The DSP chain, the denoiser slot and the APM decorator exist; the
        /// fields of the ones that do not stay at their defaults.
        pub dsp_available: bool,
        pub denoiser_available: bool,
        pub policy_available: bool,
        pub denoised_sumsq: f64,
        pub gate_in_sumsq: f64,
        pub gate_in_block_peak_db: f32,
        pub threshold: f32,
        pub envelope: f32,
        pub gate_speaking: bool,
        pub speaking_seq: u64,
        pub voice_gain_db: f32,
        pub voice_gain_settled: bool,
        pub gate_open_frames: u64,
        pub speaking: bool,
        pub gate_input_above_frames: u64,
        pub startup_hold_frames: u64,
        pub transmit_frames: u64,
        pub unsupported_frames: u64,
        pub non_finite_samples: u64,
        pub dsp_resets: u64,
        pub dsp_flushes: u64,
        pub gate_open_delay_us: u64,
        pub ns_gap_frames: u64,
        pub ns_fallback_pending_frames: u64,
        pub denoiser_installed: bool,
        pub denoiser_frames: u64,
        pub denoiser_misses: u64,
        pub denoiser_empty_outputs: u64,
        pub denoiser_process_max_us: u64,
        pub denoiser_slow_frames: u64,
        pub denoiser_bypass_reason: u32,
        pub denoiser_notifications: u64,
        pub denoiser_rate: u32,
        pub denoiser_installs: u64,
        pub denoiser_bypassed_frames: u64,
        pub denoiser_process_us_buckets: Vec<u64>,
        pub denoiser_other_notifications: u64,
        pub denoiser_latency_ms: f32,
        pub denoiser_buffered_samples: u32,
        pub denoiser_underruns: u64,
        pub denoiser_padded_samples: u64,
        pub denoiser_trimmed_samples: u64,
        pub denoiser_drained_samples: u64,
        pub denoiser_frames_until_first_output: i32,
        pub webrtc_ns_fallback: bool,
        pub apm_policy_applies: u64,
        pub apm_policy_deferred: u64,
        pub apm_policy_forced: u64,
        pub apm_policy_pending: bool,
    }

    /// One RootAudioControl; `has_*` marks the fields to change.
    #[derive(Debug, Clone, Copy, Default, PartialEq)]
    pub struct RootAudioControlUpdate {
        pub has_mic_muted: bool,
        pub mic_muted: bool,
        pub has_push_to_talk: bool,
        pub push_to_talk: u32,
        pub has_input_volume: bool,
        pub input_volume: f32,
        pub has_master_volume: bool,
        pub master_volume: f32,
        pub has_deafened: bool,
        pub deafened: bool,
        pub has_echo_cancellation: bool,
        pub echo_cancellation: bool,
        pub has_noise_suppression: bool,
        pub noise_suppression: bool,
        pub has_denoise_power: bool,
        pub denoise_power: f32,
        pub has_auto_gain: bool,
        pub auto_gain: bool,
        pub has_gate_threshold: bool,
        pub gate_threshold: f32,
        pub has_auto_sensitivity: bool,
        pub auto_sensitivity: bool,
        pub has_gain_reset_db: bool,
        pub gain_reset_db: f32,
        pub has_denoiser_wanted: bool,
        pub denoiser_wanted: bool,
    }

    /// The stored controls (tests).
    #[derive(Debug, Clone, Copy, Default, PartialEq)]
    pub struct RootControlState {
        pub mic_muted: bool,
        pub push_to_talk: u32,
        pub input_volume: f32,
        pub denoise_power: f32,
        pub auto_gain: bool,
        pub gate_threshold: f32,
        pub auto_sensitivity: bool,
        pub gain_reset_db: f32,
        pub gain_reset_seq: u64,
        pub master_volume: f32,
        pub deafened: bool,
        pub denoiser_wanted: bool,
        pub stream_epoch: u64,
        pub dsp_reset_seq: u64,
    }

    /// What the capture post-processor runs at; `seq` moves on every change.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct RootAudioCaptureFormat {
        pub seq: u64,
        pub sample_rate: u32,
        pub channels: u32,
    }

    /// A SetDenoiser request finished (`error` empty on success). With an
    /// error, `pipeline` is the request's pipeline when native let go of it
    /// and lists it nowhere else (superseded, busy); else 0.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct RootDenoiserCompletionItem {
        pub owner: u64,
        pub async_id: u64,
        pub error: String,
        pub pipeline: u64,
    }

    /// A pipeline native no longer holds, for the owner that set it.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct RootDenoiserReturnedItem {
        pub owner: u64,
        pub pipeline: u64,
    }

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct RootReturnPipelinesResult {
        pub pipelines: Vec<u64>,
        /// Empty unless the installed pipeline could not be removed.
        pub error: String,
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct RootDenoiserStateInfo {
        pub installed: bool,
        pub rate: u32,
        pub bypass_reason: u32,
        pub webrtc_ns_fallback: bool,
    }

    /// The published speaking flag and its transition count.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct RootSpeakingInfo {
        pub speaking: bool,
        pub seq: u64,
    }

    /// What a fake Effects SDK pipeline was asked (tests).
    #[derive(Debug, Clone, Copy, Default, PartialEq)]
    pub struct RootVoiceFakeCounts {
        pub process_calls: u64,
        pub flush_calls: u64,
        pub power_sets: u64,
        pub last_power: f32,
    }

    unsafe extern "C++" {
        include!("livekit/audio_device_controller.h");
        include!("livekit/peer_connection_factory.h");

        type AudioDeviceController;
        type AdmProxyHarness;
        type RootVoiceHarness;
        type PeerConnectionFactory = crate::peer_connection_factory::ffi::PeerConnectionFactory;

        fn audio_device(self: &PeerConnectionFactory) -> SharedPtr<AudioDeviceController>;

        fn playout_devices(self: &AudioDeviceController) -> i16;
        fn recording_devices(self: &AudioDeviceController) -> i16;
        fn playout_device_name(self: &AudioDeviceController, index: u16) -> String;
        fn recording_device_name(self: &AudioDeviceController, index: u16) -> String;
        fn playout_device_guid(self: &AudioDeviceController, index: u16) -> String;
        fn recording_device_guid(self: &AudioDeviceController, index: u16) -> String;

        fn set_playout_device(self: &AudioDeviceController, index: u16) -> bool;
        fn set_recording_device(self: &AudioDeviceController, index: u16) -> bool;
        fn set_playout_device_by_guid(self: &AudioDeviceController, guid: String) -> bool;
        fn set_recording_device_by_guid(self: &AudioDeviceController, guid: String) -> bool;

        fn select_devices(
            self: &AudioDeviceController,
            recording: &AudioDeviceSelection,
            set_recording: bool,
            playout: &AudioDeviceSelection,
            set_playout: bool,
            callback: Box<AudioDeviceRequestCallback>,
        );
        fn restart_devices(
            self: &AudioDeviceController,
            recording: bool,
            playout: bool,
            callback: Box<AudioDeviceRequestCallback>,
        );
        fn set_playout_hold(self: &AudioDeviceController, hold: bool);
        fn root_audio_stats(self: &AudioDeviceController) -> RootAudioNativeStats;

        /// False when a set field has nothing to act on in this build; the
        /// other fields are applied.
        fn set_root_audio_control(
            self: &AudioDeviceController,
            control: &RootAudioControlUpdate,
        ) -> bool;
        fn set_root_audio_source_gain(
            self: &AudioDeviceController,
            ssrc: u32,
            tile_volume: f32,
            paused: bool,
        ) -> bool;
        fn clear_root_audio_source_gain(self: &AudioDeviceController, ssrc: u32);
        fn register_root_audio_effect(
            self: &AudioDeviceController,
            id: u32,
            samples: &[i16],
        ) -> bool;
        /// 0 queued, 1 unknown clip, 2 dropped (queue full), 3 idle (no platform audio).
        fn play_root_audio_effect(self: &AudioDeviceController, id: u32, gain: f32) -> u8;
        fn stop_root_audio_effects(self: &AudioDeviceController);
        fn root_audio_capture_format(self: &AudioDeviceController) -> RootAudioCaptureFormat;
        fn root_audio_sources_added(self: &AudioDeviceController) -> u64;
        fn reset_root_audio(self: &AudioDeviceController, close_gate: bool);

        /// The Effects SDK slot exists (desktop, 64-bit).
        fn root_denoiser_available(self: &AudioDeviceController) -> bool;
        /// Queues an install (`pipeline` non-zero) or removal for `owner`;
        /// false (nothing queued) without a slot.
        fn set_root_denoiser(
            self: &AudioDeviceController,
            owner: u64,
            async_id: u64,
            pipeline: u64,
            sample_rate: u32,
        ) -> bool;
        /// Synchronous: everything native holds, waiting or not yet taken back.
        fn return_root_denoiser_pipelines(
            self: &AudioDeviceController,
        ) -> RootReturnPipelinesResult;
        fn take_root_denoiser_completions(
            self: &AudioDeviceController,
        ) -> Vec<RootDenoiserCompletionItem>;
        fn take_root_denoiser_returned(
            self: &AudioDeviceController,
        ) -> Vec<RootDenoiserReturnedItem>;
        fn root_denoiser_state(self: &AudioDeviceController) -> RootDenoiserStateInfo;
        fn root_audio_speaking(self: &AudioDeviceController) -> RootSpeakingInfo;

        fn stop_recording(self: &AudioDeviceController) -> bool;
        fn init_recording(self: &AudioDeviceController) -> bool;
        fn start_recording(self: &AudioDeviceController) -> bool;
        fn recording_is_initialized(self: &AudioDeviceController) -> bool;

        fn stop_playout(self: &AudioDeviceController) -> bool;
        fn init_playout(self: &AudioDeviceController) -> bool;
        fn start_playout(self: &AudioDeviceController) -> bool;
        fn playout_is_initialized(self: &AudioDeviceController) -> bool;

        fn builtin_aec_is_available(self: &AudioDeviceController) -> bool;
        fn builtin_agc_is_available(self: &AudioDeviceController) -> bool;
        fn builtin_ns_is_available(self: &AudioDeviceController) -> bool;
        fn enable_builtin_aec(self: &AudioDeviceController, enable: bool) -> bool;
        fn enable_builtin_agc(self: &AudioDeviceController, enable: bool) -> bool;
        fn enable_builtin_ns(self: &AudioDeviceController, enable: bool) -> bool;

        fn set_adm_recording_enabled(self: &AudioDeviceController, enabled: bool);
        fn adm_recording_enabled(self: &AudioDeviceController) -> bool;

        fn set_adm_playout_enabled(self: &AudioDeviceController, enabled: bool);
        fn adm_playout_enabled(self: &AudioDeviceController) -> bool;

        fn acquire_platform_adm(self: &AudioDeviceController) -> bool;
        fn release_platform_adm(self: &AudioDeviceController);
        fn platform_adm_ref_count(self: &AudioDeviceController) -> i32;
        fn is_platform_adm_active(self: &AudioDeviceController) -> bool;

        fn resolve_audio_device_index_for_test(
            selection: &AudioDeviceSelection,
            names: &[String],
            guids: &[String],
        ) -> i32;

        // AdmProxy over a fake platform ADM (tests only; opens no device).
        fn new_adm_proxy_harness(
            playout_guids: &[String],
            recording_guids: &[String],
        ) -> UniquePtr<AdmProxyHarness>;
        /// With an APM decorator over a fake APM behind the controller.
        fn new_adm_proxy_harness_with_apm(
            playout_guids: &[String],
            recording_guids: &[String],
        ) -> UniquePtr<AdmProxyHarness>;
        fn take_calls(self: &AdmProxyHarness) -> Vec<String>;
        fn set_devices(
            self: &AdmProxyHarness,
            playout_guids: &[String],
            recording_guids: &[String],
        );
        fn fail_next_stop_playout(self: &AdmProxyHarness, result: i32);
        fn fail_next_stop_recording(self: &AdmProxyHarness, result: i32);
        fn set_device_playout_delay(self: &AdmProxyHarness, delay_ms: u16);
        fn acquire(self: &AdmProxyHarness);
        fn release(self: &AdmProxyHarness);
        fn enable(self: &AdmProxyHarness, recording: bool, playout: bool);
        fn hold(self: &AdmProxyHarness, hold: bool);
        fn held(self: &AdmProxyHarness) -> bool;
        fn app_playout(self: &AdmProxyHarness, start: bool);
        fn app_recording(self: &AdmProxyHarness, start: bool);
        fn engine_start_playout(self: &AdmProxyHarness);
        fn engine_stop_playout(self: &AdmProxyHarness);
        fn engine_start_recording(self: &AdmProxyHarness);
        fn engine_stop_recording(self: &AdmProxyHarness);
        fn engine_init_playout(self: &AdmProxyHarness) -> i32;
        fn engine_init_recording(self: &AdmProxyHarness) -> i32;
        fn register_transport(self: &AdmProxyHarness);
        fn terminate(self: &AdmProxyHarness);
        fn select(
            self: &AdmProxyHarness,
            recording: &AudioDeviceSelection,
            set_recording: bool,
            playout: &AudioDeviceSelection,
            set_playout: bool,
        ) -> AudioDeviceRequestOutcome;
        fn restart(
            self: &AdmProxyHarness,
            recording: bool,
            playout: bool,
        ) -> AudioDeviceRequestOutcome;
        fn flush(self: &AdmProxyHarness);
        fn playing(self: &AdmProxyHarness) -> bool;
        fn recording(self: &AdmProxyHarness) -> bool;
        fn playout_initialized(self: &AdmProxyHarness) -> bool;
        fn recording_initialized(self: &AdmProxyHarness) -> bool;
        fn playout_delay(self: &AdmProxyHarness) -> u16;
        fn has_device_stats(self: &AdmProxyHarness) -> bool;
        fn stop_on_mute(self: &AdmProxyHarness) -> bool;
        fn builtin_effects_available(self: &AdmProxyHarness) -> bool;
        fn set_microphone_mute(self: &AdmProxyHarness, mute: bool) -> i32;
        fn enable_builtin_effect(self: &AdmProxyHarness, effect: u8, enable: bool) -> i32;
        fn platform_playing(self: &AdmProxyHarness) -> bool;
        fn platform_recording(self: &AdmProxyHarness) -> bool;
        fn synthetic_playing(self: &AdmProxyHarness) -> bool;
        fn device_callback(self: &AdmProxyHarness, capture: bool, reenter: bool) -> u32;
        fn lock_on_audio_thread(self: &AdmProxyHarness) -> u64;
        fn stats(self: &AdmProxyHarness) -> RootAudioNativeStats;
        fn mixer_platform_active(self: &AdmProxyHarness) -> bool;
        fn apply_control(self: &AdmProxyHarness, control: &RootAudioControlUpdate) -> bool;
        fn reset_root_audio(self: &AdmProxyHarness, close_gate: bool);
        fn control_state(self: &AdmProxyHarness) -> RootControlState;
        /// The controller behind the harness (its capture state has a slot).
        fn controller(self: &AdmProxyHarness) -> &AudioDeviceController;
        /// A fake Effects SDK pipeline owned by the harness (scale 1, no delay).
        fn add_fake_pipeline(self: &AdmProxyHarness) -> u64;
        /// One 10 ms frame at `rate` through the slot, as the capture
        /// processor would run it; returns the slot's result kind.
        fn run_denoiser(self: &AdmProxyHarness, rate: u32) -> u8;
        fn latch_denoiser_bypass(self: &AdmProxyHarness, reason: u32);
        /// Stores the speaking flag as the capture processor publishes it on
        /// a frame now.
        fn set_speaking(self: &AdmProxyHarness, speaking: bool);

        /// The voice path end to end over one capture state, wired as the
        /// factory does: AdmProxy over a fake platform ADM, the builtin APM
        /// with the capture processor behind the decorator (policy on a worker
        /// thread), the denoiser slot and the controller (tests only; no
        /// device). A session has started; recording has not.
        fn new_root_voice_harness() -> UniquePtr<RootVoiceHarness>;
        fn controller(self: &RootVoiceHarness) -> &AudioDeviceController;
        /// The app starts or stops recording (a start bumps the stream epoch).
        fn recording(self: &RootVoiceHarness, start: bool);
        fn stream_epoch(self: &RootVoiceHarness) -> u64;
        /// One 10 ms frame of interleaved channels from the device, through
        /// AdmProxy and the APM on a device thread; returns what is sent.
        fn capture(self: &RootVoiceHarness, rate: i32, channels: usize, input: &[i16]) -> Vec<i16>;
        /// A fake Effects SDK pipeline (output round(scale x input) delayed
        /// by delay_samples) owned by the harness.
        fn add_fake_pipeline(self: &RootVoiceHarness, scale: f32, delay_samples: u32) -> u64;
        /// Every output of that pipeline is empty from now on (or again not).
        fn set_fake_empty(self: &RootVoiceHarness, pipeline: u64, empty: bool);
        fn fake_counts(self: &RootVoiceHarness, pipeline: u64) -> RootVoiceFakeCounts;
        /// Holds the worker thread (policy applications wait) until released.
        fn hold_worker(self: &RootVoiceHarness, hold: bool);
        /// What the decorator applied, as the capture processor reads it.
        fn webrtc_ns_applied(self: &RootVoiceHarness) -> bool;
    }

    extern "Rust" {
        type AudioDeviceRequestCallback;

        fn on_complete(self: &AudioDeviceRequestCallback, outcome: AudioDeviceRequestOutcome);
    }
}

impl_thread_safety!(ffi::AudioDeviceController, Send + Sync);

/// Completion of a device request, called once on the native device thread.
pub struct AudioDeviceRequestCallback {
    on_complete: Mutex<Option<Box<dyn FnOnce(ffi::AudioDeviceRequestOutcome) + Send>>>,
}

impl AudioDeviceRequestCallback {
    pub fn new(on_complete: impl FnOnce(ffi::AudioDeviceRequestOutcome) + Send + 'static) -> Self {
        Self { on_complete: Mutex::new(Some(Box::new(on_complete))) }
    }

    fn on_complete(&self, outcome: ffi::AudioDeviceRequestOutcome) {
        let callback = self.on_complete.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(callback) = callback {
            callback(outcome);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ffi::{
        resolve_audio_device_index_for_test, AudioDeviceSelection, AudioDeviceSelectionKind,
    };
    use crate::peer_connection_factory::ffi::create_peer_connection_factory;
    use std::sync::Mutex;

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    const NOT_FOUND: i32 = -2;
    const DEFAULT: i32 = -1;

    fn resolve(kind: AudioDeviceSelectionKind, id: &str, index: u16) -> i32 {
        let names: Vec<String> =
            ["Speakers", "Headset", "Headset"].iter().map(|s| s.to_string()).collect();
        let guids: Vec<String> = ["{a}", "{b}", "{c}"].iter().map(|s| s.to_string()).collect();
        resolve_audio_device_index_for_test(
            &AudioDeviceSelection { kind, id: id.to_string(), index },
            &names,
            &guids,
        )
    }

    #[test]
    fn device_selection_default_resolves_to_the_os_default() {
        assert_eq!(resolve(AudioDeviceSelectionKind::Default, "", 0), DEFAULT);
        assert_eq!(resolve(AudioDeviceSelectionKind::Default, "{b}", 2), DEFAULT);
    }

    #[test]
    fn device_selection_by_guid_finds_the_current_index() {
        assert_eq!(resolve(AudioDeviceSelectionKind::Guid, "{a}", 0), 0);
        assert_eq!(resolve(AudioDeviceSelectionKind::Guid, "{c}", 0), 2);
    }

    #[test]
    fn device_selection_by_name_takes_the_first_match() {
        assert_eq!(resolve(AudioDeviceSelectionKind::Name, "Headset", 0), 1);
        assert_eq!(resolve(AudioDeviceSelectionKind::Name, "Speakers", 0), 0);
    }

    #[test]
    fn device_selection_missing_device_is_not_found_never_index_zero() {
        assert_eq!(resolve(AudioDeviceSelectionKind::Guid, "{gone}", 0), NOT_FOUND);
        assert_eq!(resolve(AudioDeviceSelectionKind::Name, "USB Mic", 0), NOT_FOUND);
        assert_eq!(resolve(AudioDeviceSelectionKind::Guid, "", 0), NOT_FOUND);
        assert_eq!(resolve(AudioDeviceSelectionKind::Name, "", 0), NOT_FOUND);
        assert_eq!(resolve(AudioDeviceSelectionKind::Guid, "Headset", 0), NOT_FOUND);
    }

    #[test]
    fn device_selection_by_index_checks_the_range() {
        assert_eq!(resolve(AudioDeviceSelectionKind::Index, "", 1), 1);
        assert_eq!(resolve(AudioDeviceSelectionKind::Index, "", 3), NOT_FOUND);
    }

    #[test]
    fn device_selection_with_no_devices_is_not_found() {
        let none: Vec<String> = Vec::new();
        let selection = AudioDeviceSelection {
            kind: AudioDeviceSelectionKind::Guid,
            id: "{a}".to_string(),
            index: 0,
        };
        assert_eq!(resolve_audio_device_index_for_test(&selection, &none, &none), NOT_FOUND);
    }

    #[test]
    fn test_audio_device_controller_bridge() {
        if std::env::var_os("LK_HW_AUDIO_TESTS").is_none() {
            eprintln!("skipped: creates the platform ADM; set LK_HW_AUDIO_TESTS=1 to run");
            return;
        }
        let _guard = TEST_MUTEX.lock().expect("test mutex poisoned");
        let factory = create_peer_connection_factory();
        let audio = factory.audio_device();

        let recording_count = audio.recording_devices();
        let playout_count = audio.playout_devices();
        assert!(recording_count >= 0);
        assert!(playout_count >= 0);

        if recording_count > 0 {
            let _ = audio.recording_device_name(0);
            let guid = audio.recording_device_guid(0);
            let _ = audio.set_recording_device_by_guid(guid);
        }

        if playout_count > 0 {
            let _ = audio.playout_device_name(0);
            let guid = audio.playout_device_guid(0);
            let _ = audio.set_playout_device_by_guid(guid);
        }

        let initial_recording = audio.adm_recording_enabled();
        audio.set_adm_recording_enabled(!initial_recording);
        assert_eq!(audio.adm_recording_enabled(), !initial_recording);
        audio.set_adm_recording_enabled(initial_recording);
        assert_eq!(audio.adm_recording_enabled(), initial_recording);

        let initial_playout = audio.adm_playout_enabled();
        audio.set_adm_playout_enabled(!initial_playout);
        assert_eq!(audio.adm_playout_enabled(), !initial_playout);
        audio.set_adm_playout_enabled(initial_playout);
        assert_eq!(audio.adm_playout_enabled(), initial_playout);

        assert!(audio.platform_adm_ref_count() >= 0);
        let _ = audio.is_platform_adm_active();
    }
}
