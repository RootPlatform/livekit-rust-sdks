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
        /// macOS: a CoreAudio device UID, translated to the device's current
        /// AudioObjectID at every open.
        Uid = 4,
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

    /// One render callback of the harness's fake device (tests only).
    #[derive(Debug, Clone, Default)]
    pub struct AdmDeviceRender {
        /// AudioDeviceBuffer::RequestPlayoutData's result: the frames
        /// libwebrtc's side produced, 0 when it had no transport; -1 when the
        /// device does not play.
        pub frames: i32,
        /// What the device plays, interleaved.
        pub samples: Vec<i16>,
    }

    /// One playout mixer source (a remote audio stream): its audio as it
    /// reached the mixer, before its gain (a muted frame counts as silence),
    /// cumulative since it was added; `source_id` tells a stream added again
    /// under the same SSRC from the earlier one.
    #[derive(Debug, Clone, Copy, Default, PartialEq)]
    pub struct RootMixSourceLevel {
        pub ssrc: u32,
        pub source_id: u64,
        pub sumsq: f64,
        pub samples: u64,
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
        /// The mixer counts per-source levels (the playout mixer exists).
        pub mix_source_levels_available: bool,
        pub mix_source_levels: Vec<RootMixSourceLevel>,
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

        /// Device formats are read (macOS). Each open direction's device
        /// format when the ADM opened it (0 while closed), the format changes
        /// seen under an open direction, and playout reopened for one.
        pub device_formats_available: bool,
        pub playout_device_rate: u32,
        pub playout_device_channels: u32,
        pub recording_device_rate: u32,
        pub recording_device_channels: u32,
        pub playout_format_changes: u64,
        pub playout_format_reopens: u64,
        pub recording_format_changes: u64,

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
        /// The auto sensitivity minimum the chain applies (dBFS before the
        /// voice gain).
        pub auto_sensitivity_minimum_db: f32,
        /// The voice gain's tunables the chain applies (the stored ones after
        /// clamping, the defaults when unset).
        pub voice_gain_max_db: f32,
        pub voice_gain_target_db: f32,
        pub voice_gain_rise_db_per_second: f32,
        pub voice_gain_fall_db_per_second: f32,
        pub voice_gain_level_time_constant_ms: f32,
        pub voice_gain_min_speech_probability: f32,
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

        /// The mic test monitor exists (the capture processor does).
        pub monitor_available: bool,
        pub monitor_active: bool,
        pub monitor_frames: u64,
        pub monitor_dropped_frames: u64,

        /// The speech detector exists (with the DSP chain). The setting (0 off,
        /// 1 ring, 2 gate), the highest probability since the previous read
        /// (-1: none), whether the speech gate judged the last frame and found
        /// it speech, and frames with confirmed speech, frames it held the
        /// noise gate shut or forced it closed, and frames with detection on
        /// but no probability.
        pub speech_available: bool,
        pub speech_detection: u32,
        pub speech_probability: f32,
        pub speech_detector_running: bool,
        pub speech: bool,
        pub speech_frames: u64,
        pub speech_closed_frames: u64,
        pub speech_unavailable_frames: u64,

        /// The echo model exists (the factory's APM can switch to it). Its
        /// mode, state, reason and the playout device's kind (proto wire
        /// values), APM switches, inferences, the inference mean and maximum
        /// since the previous read (NaN and 0 without one), the last window's
        /// p99 (NaN before one), and the measured-echo latch and kill switch.
        pub echo_model_available: bool,
        pub echo_model: u32,
        pub echo_model_state: u32,
        pub echo_model_reason: u32,
        pub output_device_kind: u32,
        pub echo_model_swaps: u64,
        pub echo_model_inferences: u64,
        pub echo_model_inference_mean_us: f32,
        pub echo_model_inference_max_us: u64,
        pub echo_model_inference_p99_us: f32,
        pub echo_model_measured_echo: bool,
        pub echo_model_kill_switch: bool,
    }

    /// A RootAudioMonitor's counters (tests).
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct RootMonitorStats {
        pub active: bool,
        pub frames: u64,
        pub dropped_frames: u64,
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
        /// 0 off, 1 ring, 2 gate; other values are off.
        pub has_speech_detection: bool,
        pub speech_detection: u32,
        /// 0 off, 1 on, 2 auto; other values are off.
        pub has_echo_model: bool,
        pub echo_model: u32,
        /// dBFS before the voice gain; the chain clamps it to -80..-30 and
        /// takes a non-finite value as its default (-54).
        pub has_auto_sensitivity_minimum_db: bool,
        pub auto_sensitivity_minimum_db: f32,
        /// The voice gain's tunables (rootapp_voice_dsp::VoiceGainTuning); the
        /// chain clamps each and takes a non-finite value as its default.
        pub has_voice_gain_max_db: bool,
        pub voice_gain_max_db: f32,
        pub has_voice_gain_target_db: bool,
        pub voice_gain_target_db: f32,
        pub has_voice_gain_rise_db_per_second: bool,
        pub voice_gain_rise_db_per_second: f32,
        pub has_voice_gain_fall_db_per_second: bool,
        pub voice_gain_fall_db_per_second: f32,
        pub has_voice_gain_level_time_constant_ms: bool,
        pub voice_gain_level_time_constant_ms: f32,
        pub has_voice_gain_min_speech_probability: bool,
        pub voice_gain_min_speech_probability: f32,
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
        pub speech_detection: u32,
        /// As stored (NaN: none, the chain's default).
        pub auto_sensitivity_minimum_db: f32,
        /// As stored (NaN: none, the chain's defaults).
        pub voice_gain_max_db: f32,
        pub voice_gain_target_db: f32,
        pub voice_gain_rise_db_per_second: f32,
        pub voice_gain_fall_db_per_second: f32,
        pub voice_gain_level_time_constant_ms: f32,
        pub voice_gain_min_speech_probability: f32,
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
        type RootEngineHarness;
        type RootMonitorHarness;
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
        /// The mic test monitor: 0 applied, 1 not supported by this build, 2
        /// refused while a call sends from the platform microphone, 3 no
        /// platform audio reference, 4 no platform device module.
        fn set_root_audio_monitor(self: &AudioDeviceController, enabled: bool) -> u8;

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
        /// A selection by `wanted` against one device the ADM names with
        /// these raw bytes.
        fn adm_name_matches_for_test(device: &[u8], wanted: &str) -> bool;

        /// A RootAudioMonitor driven as its capture and render threads would.
        fn new_root_monitor_harness() -> UniquePtr<RootMonitorHarness>;
        fn set_enabled(self: &RootMonitorHarness, enabled: bool);
        /// What AdmProxy does around a device stop or start.
        fn resync(self: &RootMonitorHarness);
        /// One capture frame of FloatS16 samples at `rate`.
        fn push(self: &RootMonitorHarness, rate: i32, samples: &[f32]);
        /// One render callback of rate / 100 frames over `base`; returns what
        /// the device plays.
        fn mix(self: &RootMonitorHarness, rate: u32, channels: usize, base: &[i16]) -> Vec<i16>;
        fn stats(self: &RootMonitorHarness) -> RootMonitorStats;

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
        /// With no platform ADM (one that failed to initialize).
        fn new_adm_proxy_harness_without_platform_adm() -> UniquePtr<AdmProxyHarness>;
        fn take_calls(self: &AdmProxyHarness) -> Vec<String>;
        fn take_playout_opened(self: &AdmProxyHarness) -> Vec<String>;
        /// The fake's channel, speaker and microphone calls and the channels of
        /// every InitPlayout ("InitPlayout stereo"), kept apart from take_calls.
        fn take_channel_calls(self: &AdmProxyHarness) -> Vec<String>;
        fn set_devices(
            self: &AdmProxyHarness,
            playout_guids: &[String],
            recording_guids: &[String],
        );
        /// UID translation as CoreAudio would answer it now: `uids[i]` is
        /// the device with GUID `guids[i]`; any other UID is no device.
        fn set_uid_map(self: &AdmProxyHarness, uids: &[String], guids: &[String]);
        /// Enumeration serialized with device operations (as on Linux),
        /// waiting at most `wait_ms` for one in progress; or never waiting.
        fn set_enumeration_lock(self: &AdmProxyHarness, serialize: bool, wait_ms: i32);
        /// A device's format as CoreAudio would report it now (`guid`
        /// empty: the OS default). The first call gives the proxy a format
        /// reader that only `poll_device_formats` runs; a device without a
        /// format cannot be read.
        fn set_device_format(
            self: &AdmProxyHarness,
            playout: bool,
            guid: &str,
            rate: u32,
            channels: u32,
        );
        fn poll_device_formats(self: &AdmProxyHarness);
        /// The next StartPlayout blocks the device thread (holding the
        /// device mutex) until released.
        fn block_next_start_playout(self: &AdmProxyHarness);
        /// Whether the device thread got there within 5 s.
        fn start_playout_blocked(self: &AdmProxyHarness) -> bool;
        /// Releases it `after_ms` from now, from another thread.
        fn release_start_playout(self: &AdmProxyHarness, after_ms: u32);
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
        /// The controller's set_root_audio_monitor.
        fn monitor(self: &AdmProxyHarness, enabled: bool) -> u8;
        /// One frame into the monitor, as the capture post-processor gives it.
        fn monitor_push(self: &AdmProxyHarness, rate: i32, samples: &[f32]);
        /// One 10 ms 48 kHz stereo render callback from a device thread
        /// through the transport tap (libwebrtc's side plays silence);
        /// returns what the device plays.
        fn render_frame(self: &AdmProxyHarness) -> Vec<i16>;
        /// The fake's AudioDeviceBuffer (libwebrtc's own) holds `level` in its
        /// playout samples, standing in for the uninitialized heap block it
        /// allocates on its first pull.
        fn stale_device_buffer(self: &AdmProxyHarness, level: i16);
        /// One 10 ms render callback from a device thread as the Windows ADM's
        /// render thread makes it: AudioDeviceBuffer::RequestPlayoutData, then
        /// GetPlayoutData into the device.
        fn device_render(self: &AdmProxyHarness) -> AdmDeviceRender;
        /// libwebrtc's transport plays the mixer from now on, as
        /// AudioTransportImpl does.
        fn engine_plays_mixer(self: &AdmProxyHarness);
        fn engine_start_playout(self: &AdmProxyHarness);
        fn engine_stop_playout(self: &AdmProxyHarness);
        fn engine_start_recording(self: &AdmProxyHarness);
        fn engine_stop_recording(self: &AdmProxyHarness);
        fn engine_init_playout(self: &AdmProxyHarness) -> i32;
        fn engine_init_recording(self: &AdmProxyHarness) -> i32;
        /// libwebrtc's adm_helpers::Init on the proxy: the results of
        /// InitSpeaker, StereoPlayoutIsAvailable, SetStereoPlayout and
        /// InitMicrophone.
        fn engine_adm_init(self: &AdmProxyHarness) -> Vec<i32>;
        fn engine_stereo_playout(self: &AdmProxyHarness) -> bool;
        fn engine_set_stereo_playout(self: &AdmProxyHarness, enable: bool) -> i32;
        fn engine_set_speaker_volume(self: &AdmProxyHarness, volume: u32) -> i32;
        fn engine_set_speaker_mute(self: &AdmProxyHarness, mute: bool) -> i32;
        fn register_transport(self: &AdmProxyHarness);
        /// WebRtcVoiceEngine::Terminate on the proxy (m150: the last
        /// PeerConnection went): StopPlayout, StopRecording,
        /// RegisterAudioCallback(nullptr), Terminate.
        fn engine_terminate(self: &AdmProxyHarness);
        /// What the proxy's destructor does to the devices.
        fn shutdown(self: &AdmProxyHarness);
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
        /// libwebrtc starts or stops recording for a send stream.
        fn engine_recording(self: &RootVoiceHarness, start: bool);
        /// The controller's set_root_audio_monitor, applied on the device
        /// thread before it returns.
        fn monitor(self: &RootVoiceHarness, enabled: bool) -> u8;
        fn platform_playing(self: &RootVoiceHarness) -> bool;
        fn platform_recording(self: &RootVoiceHarness) -> bool;
        fn stream_epoch(self: &RootVoiceHarness) -> u64;
        /// One 10 ms frame of interleaved channels from the device, through
        /// AdmProxy and the APM on a device thread; returns what is sent.
        fn capture(self: &RootVoiceHarness, rate: i32, channels: usize, input: &[i16]) -> Vec<i16>;
        /// libwebrtc's render side plays `level` everywhere and hands that
        /// frame to the APM as its reverse stream (the AEC reference).
        fn set_remote_level(self: &RootVoiceHarness, level: i16);
        /// One 10 ms render callback from the device thread; returns what the
        /// device plays.
        fn render(self: &RootVoiceHarness, rate: i32, channels: usize) -> Vec<i16>;
        /// The last frame the APM got as its reverse stream.
        fn reverse_input(self: &RootVoiceHarness) -> Vec<i16>;
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

        /// libwebrtc's own voice engine (WebRtcVoiceEngine in a
        /// ConnectionContext) over AdmProxy and a fake platform ADM, wired as
        /// the factory does, with the voice engine hold. PeerConnections are
        /// stood in for by their references to the context (tests only; no
        /// device, no PeerConnection).
        fn new_root_engine_harness() -> UniquePtr<RootEngineHarness>;
        fn controller(self: &RootEngineHarness) -> &AudioDeviceController;
        /// As a PlatformAudio handle's creation: a platform reference, both
        /// directions enabled, a session start.
        fn acquire(self: &RootEngineHarness) -> bool;
        /// As its drop: the session's reset with the gate closed, then the
        /// reference.
        fn release(self: &RootEngineHarness);
        /// A PeerConnection takes its reference to the ConnectionContext
        /// (m150 initializes the voice engine with the first one).
        fn open_peer_connection(self: &RootEngineHarness);
        /// One is destroyed (the last one terminates the voice engine).
        fn close_peer_connection(self: &RootEngineHarness);
        /// AudioState starts or stops a call's receive streams' playout.
        fn engine_playout(self: &RootEngineHarness, start: bool);
        /// AudioState starts or stops a call's send streams' recording.
        fn engine_recording(self: &RootEngineHarness, start: bool);
        fn hold(self: &RootEngineHarness, hold: bool);
        fn monitor(self: &RootEngineHarness, enabled: bool) -> u8;
        /// Returns once the worker and device threads ran what was posted.
        fn flush(self: &RootEngineHarness);
        /// One 10 ms frame of interleaved channels from the device's capture
        /// thread.
        fn capture(self: &RootEngineHarness, rate: i32, channels: usize, input: &[i16]);
        /// One 10 ms render callback from the device's render thread; returns
        /// what the device plays.
        fn render(self: &RootEngineHarness, rate: i32, channels: usize) -> Vec<i16>;
        fn platform_playing(self: &RootEngineHarness) -> bool;
        fn platform_recording(self: &RootEngineHarness) -> bool;
        fn synthetic_playing(self: &RootEngineHarness) -> bool;
        fn take_calls(self: &RootEngineHarness) -> Vec<String>;
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
        adm_name_matches_for_test, resolve_audio_device_index_for_test, AudioDeviceSelection,
        AudioDeviceSelectionKind,
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
        assert_eq!(
            resolve(AudioDeviceSelectionKind::Uid, "{a}", 0),
            NOT_FOUND,
            "a UID matches only once translated to a GUID"
        );
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

    fn resolve_in(names: &[&str], guids: &[&str], kind: AudioDeviceSelectionKind, id: &str) -> i32 {
        let names: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        let guids: Vec<String> = guids.iter().map(|s| s.to_string()).collect();
        resolve_audio_device_index_for_test(
            &AudioDeviceSelection { kind, id: id.to_string(), index: 0 },
            &names,
            &guids,
        )
    }

    /// The macOS ADM's list: the system default first (GUID "default"), then
    /// every device of the scope under its AudioObjectID, the default again
    /// among them.
    #[test]
    fn device_selection_macos_audio_object_ids_match_exactly() {
        const GUID: AudioDeviceSelectionKind = AudioDeviceSelectionKind::Guid;
        let system = AudioDeviceSelectionKind::Default;
        let names = ["default (MacBook Pro Speakers)", "MacBook Pro Speakers", "AirPods Pro"];
        let guids = ["default", "73", "91"];
        assert_eq!(resolve_in(&names, &guids, GUID, "91"), 2);
        assert_eq!(resolve_in(&names, &guids, GUID, "73"), 1, "the default itself, not index 0");
        assert_eq!(resolve_in(&names, &guids, GUID, "default"), 0, "the system default");
        assert_eq!(resolve_in(&names, &guids, system, ""), DEFAULT, "opened as index 0");
        assert_eq!(resolve_in(&names, &guids, GUID, "7"), NOT_FOUND, "no prefix match");
        assert_eq!(resolve_in(&names, &guids, GUID, " 91"), NOT_FOUND);

        // Unplugged and plugged in again: CoreAudio gave the AirPods a new id.
        let replugged = ["default", "73", "95"];
        assert_eq!(resolve_in(&names, &replugged, GUID, "91"), NOT_FOUND, "never another device");
        assert_eq!(resolve_in(&names, &replugged, GUID, "95"), 2);
    }

    /// The PulseAudio ADM's list: "default: <description>" first, then every
    /// sink or source (monitors left out) by description; GUIDs are empty.
    #[test]
    fn device_selection_linux_names_match_pulse_descriptions() {
        const NAME: AudioDeviceSelectionKind = AudioDeviceSelectionKind::Name;
        const GUID: AudioDeviceSelectionKind = AudioDeviceSelectionKind::Guid;
        let names = [
            "default: Built-in Audio Analog Stereo",
            "Built-in Audio Analog Stereo",
            "USB Headset Analog Stereo",
            "USB Headset Analog Stereo",
        ];
        let guids = ["", "", "", ""];
        assert_eq!(resolve_in(&names, &guids, NAME, "Built-in Audio Analog Stereo"), 1);
        assert_eq!(
            resolve_in(&names, &guids, NAME, "USB Headset Analog Stereo"),
            2,
            "of two devices with one description, the first"
        );
        assert_eq!(resolve_in(&names, &guids, NAME, "Built-in Audio"), NOT_FOUND);
        assert_eq!(resolve_in(&names, &guids, NAME, "built-in audio analog stereo"), NOT_FOUND);
        assert_eq!(
            resolve_in(&names, &guids, GUID, ""),
            NOT_FOUND,
            "an empty GUID selects nothing"
        );
        assert_eq!(
            resolve_in(&names, &guids, NAME, "default: Built-in Audio Analog Stereo"),
            0,
            "the default entry"
        );
    }

    /// The ADMs keep 127 bytes of a name (the macOS one up to 128, unterminated).
    #[test]
    fn device_selection_long_names_match_the_part_the_adm_kept() {
        let long = "Family 17h (Models 10h-1fh) HD Audio Controller Analog Stereo + ".repeat(3);
        assert!(long.len() > 127);
        let kept = &long.as_bytes()[..127];
        assert!(adm_name_matches_for_test(kept, &long));
        assert!(adm_name_matches_for_test(kept, &long[..127]));
        assert!(!adm_name_matches_for_test(kept, &long[..126]));
        assert!(!adm_name_matches_for_test(kept, &format!("x{long}")));
        assert!(adm_name_matches_for_test(&long.as_bytes()[..128], &long), "unterminated");

        // Cut inside a two-byte character: the full name and the name as read
        // through the FFI (the cut character replaced) both match.
        let accented = "é".repeat(70);
        let cut = &accented.as_bytes()[..127];
        let lossy = format!("{}\u{FFFD}", "é".repeat(63));
        assert!(adm_name_matches_for_test(cut, &accented));
        assert!(adm_name_matches_for_test(cut, &lossy));
        assert!(!adm_name_matches_for_test(cut, &format!("{}x", "é".repeat(62))));

        assert!(adm_name_matches_for_test(b"USB Mic", "USB Mic"));
        assert!(!adm_name_matches_for_test(b"USB Mic", "USB Mic 2"));
        assert!(!adm_name_matches_for_test(b"USB Mic 2", "USB Mic"));
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
