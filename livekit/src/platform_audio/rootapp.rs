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

//! Types of the RootApp native audio engine (device requests and stats).

pub use libwebrtc::peer_connection_factory::native::AudioDeviceSelection;
use libwebrtc::peer_connection_factory::native::{
    AudioDeviceOutcome, AudioDeviceRequestOutcome, NativeAudioStats, ADM_RESULT_NOT_ATTEMPTED,
};

use super::{AudioError, AudioResult};

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
/// are ADM return codes: 0 success, 1 not attempted yet, -2 selected device not
/// present, other negative values ADM errors. Fields of stages this build does not
/// have stay 0, `false`, or NaN for dB values and echo statistics.
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
            aec_delay_ms: 0,
            divergent_filter_fraction: f64::NAN,
            echo_cancellation_active: false,
            webrtc_ns_active: false,
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
            ..Self::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use libwebrtc::peer_connection_factory::native::ADM_RESULT_DEVICE_NOT_FOUND;

    use super::*;

    fn outcome(found: bool, attempted: bool, init: i32, start: i32) -> AudioDeviceOutcome {
        AudioDeviceOutcome { found, attempted, init_result: init, start_result: start }
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
    fn root_audio_stats_fill_phase_0_fields_and_leave_later_stages_unset() {
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
        assert_eq!(stats.processed_frames, 0);
        assert!(!stats.denoiser_installed);
        assert!(stats.erle_db.is_nan() && stats.voice_gain_db.is_nan());
    }
}
