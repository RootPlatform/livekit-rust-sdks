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

//! Types of the RootApp native audio engine (device requests, controls and
//! stats).

use libwebrtc::peer_connection_factory::native::{
    AudioDeviceOutcome, AudioDeviceRequestOutcome, NativeAudioControl, NativeAudioStats,
    NativeCaptureFormat, ADM_RESULT_NOT_ATTEMPTED,
};
pub use libwebrtc::peer_connection_factory::native::{AudioDeviceSelection, EffectPlay};

use super::{AudioError, AudioResult};

/// Slider volumes (input, master, tile) are linear 0..2.
pub const MAX_VOLUME: f32 = 2.0;
/// The app's voice gain only boosts, up to 18 dB.
pub const MAX_VOICE_GAIN_DB: f32 = 18.0;

/// A slider volume made safe for the audio thread: NaN is 0 (silent), the
/// rest is clamped to 0..[`MAX_VOLUME`].
pub fn sanitize_volume(volume: f32) -> f32 {
    if volume.is_nan() {
        0.0
    } else {
        volume.clamp(0.0, MAX_VOLUME)
    }
}

fn sanitize_unit(value: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

/// State of the capture gate's push-to-talk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushToTalk {
    /// No push-to-talk: voice activity decides (Phase 1: the gate is open).
    VoiceActivity,
    /// Key held: the gate is forced open.
    Held,
    /// Key up: nothing is sent.
    Released,
}

impl PushToTalk {
    /// The wire value (0, 1, 2). Unknown values close the gate.
    pub fn from_wire(value: u32) -> Self {
        match value {
            0 => Self::VoiceActivity,
            1 => Self::Held,
            _ => Self::Released,
        }
    }

    pub fn to_wire(self) -> u32 {
        match self {
            Self::VoiceActivity => 0,
            Self::Held => 1,
            Self::Released => 2,
        }
    }
}

/// Live control of the native audio engine. `None` leaves a setting as it is.
///
/// Take effect now: `mic_muted` and `push_to_talk` (the capture gate fades
/// out over 5 ms and zeroes every frame while muted or the key is up, and
/// fades in over 5 ms; both fields of one control change together),
/// `input_volume` (20 ms ramp), `master_volume` and `deafened` (remote audio
/// only; effects ignore both), and `echo_cancellation` /
/// `webrtc_noise_suppression` (the APM policy, applied at most once a second
/// because each toggle re-initializes the echo canceller or the noise
/// suppressor).
///
/// Stored for the DSP chain of a later build and not effective yet:
/// `denoise_power`, `auto_gain`, `gate_threshold`, `auto_sensitivity` and the
/// `gain_reset_db` command.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RootAudioControl {
    pub mic_muted: Option<bool>,
    pub push_to_talk: Option<PushToTalk>,
    /// Linear 0..2.
    pub input_volume: Option<f32>,
    /// The output volume slider / 100, linear 0..2; remote audio plays at
    /// VolumeCurve(tile x master).
    pub master_volume: Option<f32>,
    pub deafened: Option<bool>,
    pub echo_cancellation: Option<bool>,
    pub webrtc_noise_suppression: Option<bool>,
    /// 0..1.
    pub denoise_power: Option<f32>,
    pub auto_gain: Option<bool>,
    /// Linear RMS, 0..1.
    pub gate_threshold: Option<f32>,
    pub auto_sensitivity: Option<bool>,
    /// 0..[`MAX_VOICE_GAIN_DB`].
    pub gain_reset_db: Option<f32>,
}

impl RootAudioControl {
    /// Every value finite and in range: volumes clamp to 0..2 and NaN is 0,
    /// the denoise power and gate threshold clamp to 0..1 and NaN is 0, and a
    /// non-finite gain reset is dropped.
    pub fn sanitized(&self) -> Self {
        Self {
            input_volume: self.input_volume.map(sanitize_volume),
            master_volume: self.master_volume.map(sanitize_volume),
            denoise_power: self.denoise_power.map(sanitize_unit),
            gate_threshold: self.gate_threshold.map(sanitize_unit),
            gain_reset_db: self
                .gain_reset_db
                .filter(|db| db.is_finite())
                .map(|db| db.clamp(0.0, MAX_VOICE_GAIN_DB)),
            ..*self
        }
    }

    pub(crate) fn to_native(&self) -> NativeAudioControl {
        let c = self.sanitized();
        NativeAudioControl {
            has_mic_muted: c.mic_muted.is_some(),
            mic_muted: c.mic_muted.unwrap_or_default(),
            has_push_to_talk: c.push_to_talk.is_some(),
            push_to_talk: c.push_to_talk.map(PushToTalk::to_wire).unwrap_or_default(),
            has_input_volume: c.input_volume.is_some(),
            input_volume: c.input_volume.unwrap_or_default(),
            has_master_volume: c.master_volume.is_some(),
            master_volume: c.master_volume.unwrap_or_default(),
            has_deafened: c.deafened.is_some(),
            deafened: c.deafened.unwrap_or_default(),
            has_echo_cancellation: c.echo_cancellation.is_some(),
            echo_cancellation: c.echo_cancellation.unwrap_or_default(),
            has_noise_suppression: c.webrtc_noise_suppression.is_some(),
            noise_suppression: c.webrtc_noise_suppression.unwrap_or_default(),
            has_denoise_power: c.denoise_power.is_some(),
            denoise_power: c.denoise_power.unwrap_or_default(),
            has_auto_gain: c.auto_gain.is_some(),
            auto_gain: c.auto_gain.unwrap_or_default(),
            has_gate_threshold: c.gate_threshold.is_some(),
            gate_threshold: c.gate_threshold.unwrap_or_default(),
            has_auto_sensitivity: c.auto_sensitivity.is_some(),
            auto_sensitivity: c.auto_sensitivity.unwrap_or_default(),
            has_gain_reset_db: c.gain_reset_db.is_some(),
            gain_reset_db: c.gain_reset_db.unwrap_or_default(),
        }
    }
}

/// What the capture post-processor runs at: the APM's full-band rate and the
/// channel count of its buffer. `seq` moves whenever either changes (0 before
/// the first processed frame).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureFormat {
    pub seq: u64,
    pub sample_rate: u32,
    pub channels: u32,
}

impl From<NativeCaptureFormat> for CaptureFormat {
    fn from(value: NativeCaptureFormat) -> Self {
        Self { seq: value.seq, sample_rate: value.sample_rate, channels: value.channels }
    }
}

/// Result of a device request per direction; `None` for a direction the request
/// did not cover.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioDeviceResults {
    pub recording: Option<AudioResult<()>>,
    pub playout: Option<AudioResult<()>>,
}

impl AudioDeviceResults {
    pub(crate) fn from_outcome(
        outcome: Option<AudioDeviceRequestOutcome>,
        (recording, playout): (bool, bool),
    ) -> Self {
        let Some(outcome) = outcome else {
            let stopped = || Err(AudioError::OperationFailed("audio device thread stopped".into()));
            return Self { recording: recording.then(stopped), playout: playout.then(stopped) };
        };
        Self {
            recording: outcome.recording.map(device_result),
            playout: outcome.playout.map(device_result),
        }
    }
}

fn device_result(outcome: AudioDeviceOutcome) -> AudioResult<()> {
    let failed = |result: i32| result != 0 && result != ADM_RESULT_NOT_ATTEMPTED;
    if !outcome.found {
        return Err(AudioError::DeviceNotFound);
    }
    if outcome.attempted && failed(outcome.stop_result) {
        return Err(AudioError::OperationFailed(format!(
            "device stop failed ({}); retried natively",
            outcome.stop_result
        )));
    }
    if outcome.attempted && failed(outcome.init_result) {
        return Err(AudioError::OperationFailed(format!(
            "device init failed ({})",
            outcome.init_result
        )));
    }
    if outcome.attempted && failed(outcome.start_result) {
        return Err(AudioError::OperationFailed(format!(
            "device start failed ({})",
            outcome.start_result
        )));
    }
    Ok(())
}

/// Counters of the native audio engine, field for field the FFI `RootAudioStats`.
///
/// Counters are cumulative since the engine started (take deltas); levels are sums
/// of squares in int16 units with their sample counts; durations are microseconds;
/// `*_max_gap_us` is the maximum since the previous read. Last Init/Start results
/// are ADM return codes: 0 success, 1 not attempted yet, -10000 selected device
/// not present, other negative values ADM errors. Fields of stages this build does
/// not have stay 0, `false`, or NaN for dB values and echo statistics:
/// `denoised_sumsq`, `gate_in_sumsq` and the gate, speaking, voice gain and
/// denoiser fields arrive with the DSP chain. `echo_cancellation_active` and
/// `webrtc_ns_active` are what the APM has applied; the echo statistics are the
/// APM's (NaN while it has none, and `aec_delay_ms` -1).
#[derive(Debug, Clone, PartialEq)]
pub struct RootAudioStats {
    // Device IO (transport tap on the platform ADM).
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

    // Mixer.
    pub mix_calls: u64,
    pub mix_max_gap_us: u64,
    pub mix_sources: u32,
    pub mix_ssrcs: Vec<u32>,
    pub mix_out_sumsq: f64,
    pub mix_out_samples: u64,
    pub effects_played: u64,
    pub effects_dropped: u64,

    // Capture processing.
    pub processed_frames: u64,
    pub process_rate: u32,
    pub process_channels: u32,
    pub apm_out_sumsq: f64,
    pub denoised_sumsq: f64,
    pub gate_in_sumsq: f64,
    pub sent_sumsq: f64,
    pub stage_samples: u64,
    pub gate_in_block_peak_db: f32,
    pub threshold: f32,
    pub envelope: f32,
    pub gate_speaking: bool,
    pub speaking_seq: u64,
    pub voice_gain_db: f32,
    pub voice_gain_settled: bool,
    pub gate_open_frames: u64,

    // Denoiser.
    pub denoiser_installed: bool,
    pub denoiser_frames: u64,
    pub denoiser_misses: u64,
    pub denoiser_empty_outputs: u64,
    pub denoiser_process_max_us: u64,
    pub denoiser_slow_frames: u64,
    pub denoiser_bypass_reason: u32,
    pub denoiser_notifications: u64,

    // Echo canceller.
    pub erl_db: f64,
    pub erle_db: f64,
    pub aec_delay_ms: i32,
    pub divergent_filter_fraction: f64,
    pub echo_cancellation_active: bool,
    pub webrtc_ns_active: bool,

    // Real-time rule checks. Locks taken and libwebrtc log lines raised on device
    // threads; device callbacks and mixes that found another one in flight (a
    // device thread the ADM failed to stop) and were answered with silence;
    // devices that failed to stop.
    pub audio_thread_lock_violations: u64,
    pub audio_thread_logs_dropped: u64,
    pub overlapping_callbacks: u64,
    pub device_stop_failures: u64,
}

impl Default for RootAudioStats {
    fn default() -> Self {
        Self {
            capture_callbacks: 0,
            render_callbacks: 0,
            capture_max_gap_us: 0,
            render_max_gap_us: 0,
            last_capture_age_us: 0,
            last_render_age_us: 0,
            recording: false,
            playing: false,
            recording_init_result: ADM_RESULT_NOT_ATTEMPTED,
            recording_start_result: ADM_RESULT_NOT_ATTEMPTED,
            playout_init_result: ADM_RESULT_NOT_ATTEMPTED,
            playout_start_result: ADM_RESULT_NOT_ATTEMPTED,
            capture_rate: 0,
            capture_channels: 0,
            capture_raw_sumsq: 0.0,
            capture_raw_samples: 0,
            playout_delay_ms: 0,
            recording_delay_ms: 0,
            mix_calls: 0,
            mix_max_gap_us: 0,
            mix_sources: 0,
            mix_ssrcs: Vec::new(),
            mix_out_sumsq: 0.0,
            mix_out_samples: 0,
            effects_played: 0,
            effects_dropped: 0,
            processed_frames: 0,
            process_rate: 0,
            process_channels: 0,
            apm_out_sumsq: 0.0,
            denoised_sumsq: 0.0,
            gate_in_sumsq: 0.0,
            sent_sumsq: 0.0,
            stage_samples: 0,
            gate_in_block_peak_db: f32::NAN,
            threshold: 0.0,
            envelope: 0.0,
            gate_speaking: false,
            speaking_seq: 0,
            voice_gain_db: f32::NAN,
            voice_gain_settled: false,
            gate_open_frames: 0,
            denoiser_installed: false,
            denoiser_frames: 0,
            denoiser_misses: 0,
            denoiser_empty_outputs: 0,
            denoiser_process_max_us: 0,
            denoiser_slow_frames: 0,
            denoiser_bypass_reason: 0,
            denoiser_notifications: 0,
            erl_db: f64::NAN,
            erle_db: f64::NAN,
            aec_delay_ms: -1,
            divergent_filter_fraction: f64::NAN,
            echo_cancellation_active: false,
            webrtc_ns_active: false,
            audio_thread_lock_violations: 0,
            audio_thread_logs_dropped: 0,
            overlapping_callbacks: 0,
            device_stop_failures: 0,
        }
    }
}

impl RootAudioStats {
    pub(crate) fn from_native(native: NativeAudioStats) -> Self {
        Self {
            capture_callbacks: native.capture_callbacks,
            render_callbacks: native.render_callbacks,
            capture_max_gap_us: native.capture_max_gap_us,
            render_max_gap_us: native.render_max_gap_us,
            last_capture_age_us: native.last_capture_age_us,
            last_render_age_us: native.last_render_age_us,
            recording: native.recording,
            playing: native.playing,
            recording_init_result: native.recording_init_result,
            recording_start_result: native.recording_start_result,
            playout_init_result: native.playout_init_result,
            playout_start_result: native.playout_start_result,
            capture_rate: native.capture_rate,
            capture_channels: native.capture_channels,
            capture_raw_sumsq: native.capture_raw_sumsq,
            capture_raw_samples: native.capture_raw_samples,
            playout_delay_ms: native.playout_delay_ms,
            recording_delay_ms: native.recording_delay_ms,
            mix_calls: native.mix_calls,
            mix_max_gap_us: native.mix_max_gap_us,
            mix_sources: native.mix_sources,
            mix_ssrcs: native.mix_ssrcs,
            mix_out_sumsq: native.mix_out_sumsq,
            mix_out_samples: native.mix_out_samples,
            effects_played: native.effects_played,
            effects_dropped: native.effects_dropped,
            processed_frames: native.processed_frames,
            process_rate: native.process_rate,
            process_channels: native.process_channels,
            apm_out_sumsq: native.apm_out_sumsq,
            sent_sumsq: native.sent_sumsq,
            stage_samples: native.stage_samples,
            erl_db: native.erl_db,
            erle_db: native.erle_db,
            aec_delay_ms: native.aec_delay_ms,
            divergent_filter_fraction: native.divergent_filter_fraction,
            echo_cancellation_active: native.echo_cancellation_active,
            webrtc_ns_active: native.webrtc_ns_active,
            audio_thread_lock_violations: native.audio_thread_lock_violations,
            audio_thread_logs_dropped: native.audio_thread_logs_dropped,
            overlapping_callbacks: native.overlapping_callbacks,
            device_stop_failures: native.device_stop_failures,
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use libwebrtc::peer_connection_factory::native::ADM_RESULT_DEVICE_NOT_FOUND;

    use super::*;

    fn outcome(found: bool, attempted: bool, init: i32, start: i32) -> AudioDeviceOutcome {
        AudioDeviceOutcome {
            found,
            attempted,
            stop_result: ADM_RESULT_NOT_ATTEMPTED,
            init_result: init,
            start_result: start,
        }
    }

    #[test]
    fn device_results_map_each_direction() {
        let results = AudioDeviceResults::from_outcome(
            Some(AudioDeviceRequestOutcome {
                recording: Some(outcome(true, true, 0, 0)),
                playout: Some(outcome(true, false, 1, 1)),
            }),
            (true, true),
        );
        assert_eq!(results.recording, Some(Ok(())));
        assert_eq!(results.playout, Some(Ok(())));
    }

    #[test]
    fn device_results_report_a_missing_device() {
        let results = AudioDeviceResults::from_outcome(
            Some(AudioDeviceRequestOutcome {
                recording: Some(outcome(false, true, ADM_RESULT_DEVICE_NOT_FOUND, 1)),
                playout: Some(outcome(false, false, 1, 1)),
            }),
            (true, true),
        );
        assert_eq!(results.recording, Some(Err(AudioError::DeviceNotFound)));
        assert_eq!(results.playout, Some(Err(AudioError::DeviceNotFound)));
    }

    #[test]
    fn device_results_report_init_and_start_failures() {
        let results = AudioDeviceResults::from_outcome(
            Some(AudioDeviceRequestOutcome {
                recording: Some(outcome(true, true, -1, 1)),
                playout: Some(outcome(true, true, 0, -1)),
            }),
            (true, true),
        );
        assert!(matches!(
            results.recording,
            Some(Err(AudioError::OperationFailed(ref m))) if m.contains("init")
        ));
        assert!(matches!(
            results.playout,
            Some(Err(AudioError::OperationFailed(ref m))) if m.contains("start")
        ));
    }

    #[test]
    fn device_results_report_a_device_that_failed_to_stop() {
        let stuck = AudioDeviceOutcome { stop_result: -1, ..outcome(true, true, 1, 1) };
        let stopped = AudioDeviceOutcome { stop_result: 0, ..outcome(true, true, 0, 0) };
        let results = AudioDeviceResults::from_outcome(
            Some(AudioDeviceRequestOutcome { recording: Some(stopped), playout: Some(stuck) }),
            (true, true),
        );
        assert_eq!(results.recording, Some(Ok(())));
        assert!(matches!(
            results.playout,
            Some(Err(AudioError::OperationFailed(ref m))) if m.contains("stop")
        ));
    }

    #[test]
    fn device_results_leave_unrequested_directions_empty() {
        let results = AudioDeviceResults::from_outcome(
            Some(AudioDeviceRequestOutcome {
                recording: None,
                playout: Some(outcome(true, true, 0, 0)),
            }),
            (false, true),
        );
        assert_eq!(results.recording, None);
        assert_eq!(results.playout, Some(Ok(())));
    }

    #[test]
    fn device_results_fail_requested_directions_when_the_device_thread_is_gone() {
        let results = AudioDeviceResults::from_outcome(None, (true, false));
        assert!(matches!(results.recording, Some(Err(AudioError::OperationFailed(_)))));
        assert_eq!(results.playout, None);
    }

    #[test]
    fn root_audio_stats_fill_phase_1_fields_and_leave_later_stages_unset() {
        let native = NativeAudioStats {
            capture_callbacks: 10,
            render_callbacks: 20,
            capture_max_gap_us: 11_000,
            recording: true,
            playing: true,
            recording_init_result: 0,
            playout_start_result: -1,
            capture_rate: 48_000,
            capture_channels: 2,
            mix_calls: 30,
            mix_sources: 2,
            mix_ssrcs: vec![1, 2],
            effects_played: 7,
            effects_dropped: 8,
            processed_frames: 9,
            process_rate: 16_000,
            process_channels: 1,
            apm_out_sumsq: 1.5,
            sent_sumsq: 0.5,
            stage_samples: 160,
            erl_db: 12.0,
            erle_db: f64::NAN,
            aec_delay_ms: 40,
            divergent_filter_fraction: 0.25,
            echo_cancellation_active: true,
            audio_thread_lock_violations: 3,
            audio_thread_logs_dropped: 4,
            overlapping_callbacks: 5,
            device_stop_failures: 6,
            ..Default::default()
        };
        let stats = RootAudioStats::from_native(native);
        assert_eq!(stats.capture_callbacks, 10);
        assert_eq!(stats.render_callbacks, 20);
        assert_eq!(stats.capture_max_gap_us, 11_000);
        assert!(stats.recording && stats.playing);
        assert_eq!(stats.recording_init_result, 0);
        assert_eq!(stats.playout_start_result, -1);
        assert_eq!(stats.capture_rate, 48_000);
        assert_eq!(stats.mix_ssrcs, vec![1, 2]);
        assert!(stats.echo_cancellation_active && !stats.webrtc_ns_active);
        assert_eq!(
            (
                stats.audio_thread_lock_violations,
                stats.audio_thread_logs_dropped,
                stats.overlapping_callbacks,
                stats.device_stop_failures
            ),
            (3, 4, 5, 6)
        );
        assert_eq!((stats.effects_played, stats.effects_dropped), (7, 8));
        assert_eq!(
            (stats.processed_frames, stats.process_rate, stats.process_channels),
            (9, 16_000, 1)
        );
        assert_eq!((stats.apm_out_sumsq, stats.sent_sumsq, stats.stage_samples), (1.5, 0.5, 160));
        assert_eq!(
            (stats.erl_db, stats.aec_delay_ms, stats.divergent_filter_fraction),
            (12.0, 40, 0.25)
        );
        assert!(stats.erle_db.is_nan());
        assert_eq!((stats.denoised_sumsq, stats.gate_in_sumsq), (0.0, 0.0));
        assert!(!stats.denoiser_installed && !stats.gate_speaking);
        assert!(stats.voice_gain_db.is_nan());
        assert_eq!(RootAudioStats::default().aec_delay_ms, -1);
    }

    #[test]
    fn root_audio_control_sanitizes_every_value() {
        let control = RootAudioControl {
            input_volume: Some(f32::NAN),
            master_volume: Some(f32::INFINITY),
            denoise_power: Some(-0.5),
            gate_threshold: Some(f32::NAN),
            gain_reset_db: Some(f32::NEG_INFINITY),
            ..Default::default()
        }
        .sanitized();
        assert_eq!(control.input_volume, Some(0.0));
        assert_eq!(control.master_volume, Some(2.0));
        assert_eq!(control.denoise_power, Some(0.0));
        assert_eq!(control.gate_threshold, Some(0.0));
        assert_eq!(control.gain_reset_db, None);

        let control = RootAudioControl {
            input_volume: Some(1.25),
            master_volume: Some(-3.0),
            denoise_power: Some(7.0),
            gate_threshold: Some(0.05),
            gain_reset_db: Some(40.0),
            ..Default::default()
        }
        .sanitized();
        assert_eq!(control.input_volume, Some(1.25));
        assert_eq!(control.master_volume, Some(0.0));
        assert_eq!(control.denoise_power, Some(1.0));
        assert_eq!(control.gate_threshold, Some(0.05));
        assert_eq!(control.gain_reset_db, Some(MAX_VOICE_GAIN_DB));
    }

    #[test]
    fn root_audio_control_marks_only_the_fields_it_sets() {
        let native = RootAudioControl {
            mic_muted: Some(true),
            push_to_talk: Some(PushToTalk::Held),
            master_volume: Some(f32::NAN),
            webrtc_noise_suppression: Some(false),
            ..Default::default()
        }
        .to_native();
        assert!(native.has_mic_muted && native.mic_muted);
        assert!(native.has_push_to_talk && native.push_to_talk == 1);
        assert!(native.has_master_volume && native.master_volume == 0.0);
        assert!(native.has_noise_suppression && !native.noise_suppression);
        assert!(!native.has_input_volume && !native.has_deafened && !native.has_echo_cancellation);
        assert!(!native.has_denoise_power && !native.has_auto_gain && !native.has_gate_threshold);
        assert!(!native.has_auto_sensitivity && !native.has_gain_reset_db);
        assert_eq!(RootAudioControl::default().to_native(), Default::default());
    }

    #[test]
    fn push_to_talk_closes_the_gate_on_unknown_values() {
        assert_eq!(PushToTalk::from_wire(0), PushToTalk::VoiceActivity);
        assert_eq!(PushToTalk::from_wire(1), PushToTalk::Held);
        assert_eq!(PushToTalk::from_wire(2), PushToTalk::Released);
        assert_eq!(PushToTalk::from_wire(7), PushToTalk::Released);
        for state in [PushToTalk::VoiceActivity, PushToTalk::Held, PushToTalk::Released] {
            assert_eq!(PushToTalk::from_wire(state.to_wire()), state);
        }
    }

    #[test]
    fn sanitize_volume_matches_the_apps_clamp() {
        assert_eq!(sanitize_volume(f32::NAN), 0.0);
        assert_eq!(sanitize_volume(7.0), 2.0);
        assert_eq!(sanitize_volume(-1.0), 0.0);
        assert_eq!(sanitize_volume(0.5), 0.5);
    }
}
