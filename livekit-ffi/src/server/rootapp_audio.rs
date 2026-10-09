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

//! RootApp native audio requests (rootapp_audio.proto, oneof cases from 1000).
//!
//! Device requests are queued on the native device thread before the handler
//! returns and complete with a `RootAudioEvent`; nothing here blocks the caller.

use livekit::{AudioDeviceResults, AudioDeviceSelection, PlatformAudio, RootAudioStats};

use super::{platform_audio::FfiPlatformAudio, FfiServer};
use crate::{proto, FfiResult};

pub const CAP_SELECT_DEVICES: u64 = 1 << 0;
pub const CAP_PLAYOUT_HOLD: u64 = 1 << 1;
pub const CAP_STATS: u64 = 1 << 2;
pub const CAP_ADM_HARDENED: u64 = 1 << 3;
pub const CAP_MIXER: u64 = 1 << 4;
pub const CAP_TRACK_GAINS: u64 = 1 << 5;
pub const CAP_EFFECTS: u64 = 1 << 6;
pub const CAP_CONTROL: u64 = 1 << 7;
pub const CAP_DSP: u64 = 1 << 8;
pub const CAP_DENOISER: u64 = 1 << 9;
pub const CAP_MONITOR: u64 = 1 << 10;
pub const CAP_APM_DECORATOR: u64 = 1 << 11;

/// What `livekit_ffi_rootapp_audio_caps` reports. The ADM hardening is desktop
/// only, so mobile builds report nothing.
pub const ROOTAPP_AUDIO_CAPS: u64 = if cfg!(any(target_os = "ios", target_os = "android")) {
    0
} else {
    CAP_SELECT_DEVICES | CAP_PLAYOUT_HOLD | CAP_STATS | CAP_ADM_HARDENED | CAP_MIXER
};

const NOT_SUPPORTED: &str = "not supported by this build";

fn platform_audio(server: &'static FfiServer, handle: u64) -> FfiResult<PlatformAudio> {
    Ok(server.retrieve_handle::<FfiPlatformAudio>(handle)?.audio.clone())
}

fn device_selection(selection: proto::RootAudioDeviceSelection) -> AudioDeviceSelection {
    use proto::root_audio_device_selection::Device;
    match selection.device {
        Some(Device::Guid(guid)) => AudioDeviceSelection::Guid(guid),
        Some(Device::Name(name)) => AudioDeviceSelection::Name(name),
        Some(Device::SystemDefault(_)) | None => AudioDeviceSelection::SystemDefault,
    }
}

fn send_root_audio_event(
    server: &'static FfiServer,
    platform_audio_handle: u64,
    message: proto::root_audio_event::Message,
) {
    let event = proto::RootAudioEvent { platform_audio_handle, message: Some(message) };
    if let Err(e) = server.send_event(event.into()) {
        log::warn!("failed to send RootAudioEvent: {}", e);
    }
}

fn spawn_device_result(
    server: &'static FfiServer,
    platform_audio_handle: u64,
    async_id: u64,
    results: impl std::future::Future<Output = AudioDeviceResults> + Send + 'static,
) {
    let handle = server.async_runtime.spawn(async move {
        let results = results.await;
        let error = |result: Option<livekit::AudioResult<()>>| {
            result.and_then(|r| r.err()).map(|e| e.to_string())
        };
        send_root_audio_event(
            server,
            platform_audio_handle,
            proto::RootAudioDeviceResult {
                async_id,
                recording_error: error(results.recording),
                playout_error: error(results.playout),
            }
            .into(),
        );
    });
    server.watch_panic(handle);
}

pub fn on_select_devices(
    server: &'static FfiServer,
    req: proto::RootAudioSelectDevicesRequest,
) -> FfiResult<proto::RootAudioSelectDevicesResponse> {
    let audio = platform_audio(server, req.platform_audio_handle)?;
    let async_id = server.next_id();
    let results = audio
        .select_devices(req.recording.map(device_selection), req.playout.map(device_selection));
    spawn_device_result(server, req.platform_audio_handle, async_id, results);
    Ok(proto::RootAudioSelectDevicesResponse { async_id })
}

pub fn on_restart_device(
    server: &'static FfiServer,
    req: proto::RootAudioRestartDeviceRequest,
) -> FfiResult<proto::RootAudioRestartDeviceResponse> {
    let audio = platform_audio(server, req.platform_audio_handle)?;
    let async_id = server.next_id();
    let results = audio.restart_devices(req.recording, req.playout);
    spawn_device_result(server, req.platform_audio_handle, async_id, results);
    Ok(proto::RootAudioRestartDeviceResponse { async_id })
}

pub fn on_set_playout_hold(
    server: &'static FfiServer,
    req: proto::RootAudioSetPlayoutHoldRequest,
) -> FfiResult<proto::RootAudioSetPlayoutHoldResponse> {
    platform_audio(server, req.platform_audio_handle)?.set_playout_hold(req.hold);
    Ok(proto::RootAudioSetPlayoutHoldResponse { error: None })
}

pub fn on_stats(
    server: &'static FfiServer,
    req: proto::RootAudioStatsRequest,
) -> FfiResult<proto::RootAudioStatsResponse> {
    let stats = platform_audio(server, req.platform_audio_handle)?.rootapp_audio_stats();
    Ok(proto::RootAudioStatsResponse { stats: Some(stats.into()), error: None })
}

pub fn on_set_control(
    _server: &'static FfiServer,
    _req: proto::RootAudioSetControlRequest,
) -> FfiResult<proto::RootAudioSetControlResponse> {
    Ok(proto::RootAudioSetControlResponse { error: Some(NOT_SUPPORTED.into()) })
}

pub fn on_set_track_gains(
    _server: &'static FfiServer,
    _req: proto::RootAudioSetTrackGainsRequest,
) -> FfiResult<proto::RootAudioSetTrackGainsResponse> {
    Ok(proto::RootAudioSetTrackGainsResponse { error: Some(NOT_SUPPORTED.into()) })
}

pub fn on_register_effect(
    _server: &'static FfiServer,
    _req: proto::RootAudioRegisterEffectRequest,
) -> FfiResult<proto::RootAudioRegisterEffectResponse> {
    Ok(proto::RootAudioRegisterEffectResponse { error: Some(NOT_SUPPORTED.into()) })
}

pub fn on_play_effect(
    _server: &'static FfiServer,
    _req: proto::RootAudioPlayEffectRequest,
) -> FfiResult<proto::RootAudioPlayEffectResponse> {
    Ok(proto::RootAudioPlayEffectResponse { error: Some(NOT_SUPPORTED.into()) })
}

pub fn on_stop_effects(
    _server: &'static FfiServer,
    _req: proto::RootAudioStopEffectsRequest,
) -> FfiResult<proto::RootAudioStopEffectsResponse> {
    Ok(proto::RootAudioStopEffectsResponse { error: Some(NOT_SUPPORTED.into()) })
}

pub fn on_set_denoiser(
    server: &'static FfiServer,
    req: proto::RootAudioSetDenoiserRequest,
) -> FfiResult<proto::RootAudioSetDenoiserResponse> {
    let async_id = server.next_id();
    let platform_audio_handle = req.platform_audio_handle;
    server.async_runtime.spawn(async move {
        send_root_audio_event(
            server,
            platform_audio_handle,
            proto::RootAudioDenoiserResult { async_id, error: Some(NOT_SUPPORTED.into()) }.into(),
        );
    });
    Ok(proto::RootAudioSetDenoiserResponse { async_id })
}

pub fn on_return_pipelines(
    _server: &'static FfiServer,
    _req: proto::RootAudioReturnPipelinesRequest,
) -> FfiResult<proto::RootAudioReturnPipelinesResponse> {
    Ok(proto::RootAudioReturnPipelinesResponse {
        pipelines: Vec::new(),
        error: Some(NOT_SUPPORTED.into()),
    })
}

pub fn on_set_monitor(
    _server: &'static FfiServer,
    _req: proto::RootAudioSetMonitorRequest,
) -> FfiResult<proto::RootAudioSetMonitorResponse> {
    Ok(proto::RootAudioSetMonitorResponse { error: Some(NOT_SUPPORTED.into()) })
}

impl From<RootAudioStats> for proto::RootAudioStats {
    fn from(stats: RootAudioStats) -> Self {
        Self {
            capture_callbacks: stats.capture_callbacks,
            render_callbacks: stats.render_callbacks,
            capture_max_gap_us: stats.capture_max_gap_us,
            render_max_gap_us: stats.render_max_gap_us,
            last_capture_age_us: stats.last_capture_age_us,
            last_render_age_us: stats.last_render_age_us,
            recording: stats.recording,
            playing: stats.playing,
            recording_init_result: stats.recording_init_result,
            recording_start_result: stats.recording_start_result,
            playout_init_result: stats.playout_init_result,
            playout_start_result: stats.playout_start_result,
            capture_rate: stats.capture_rate,
            capture_channels: stats.capture_channels,
            capture_raw_sumsq: stats.capture_raw_sumsq,
            capture_raw_samples: stats.capture_raw_samples,
            playout_delay_ms: stats.playout_delay_ms,
            recording_delay_ms: stats.recording_delay_ms,
            mix_calls: stats.mix_calls,
            mix_max_gap_us: stats.mix_max_gap_us,
            mix_sources: stats.mix_sources,
            mix_ssrcs: stats.mix_ssrcs,
            mix_out_sumsq: stats.mix_out_sumsq,
            mix_out_samples: stats.mix_out_samples,
            effects_played: stats.effects_played,
            effects_dropped: stats.effects_dropped,
            processed_frames: stats.processed_frames,
            process_rate: stats.process_rate,
            process_channels: stats.process_channels,
            apm_out_sumsq: stats.apm_out_sumsq,
            denoised_sumsq: stats.denoised_sumsq,
            gate_in_sumsq: stats.gate_in_sumsq,
            sent_sumsq: stats.sent_sumsq,
            stage_samples: stats.stage_samples,
            gate_in_block_peak_db: stats.gate_in_block_peak_db,
            threshold: stats.threshold,
            envelope: stats.envelope,
            gate_speaking: stats.gate_speaking,
            speaking_seq: stats.speaking_seq,
            voice_gain_db: stats.voice_gain_db,
            voice_gain_settled: stats.voice_gain_settled,
            gate_open_frames: stats.gate_open_frames,
            denoiser_installed: stats.denoiser_installed,
            denoiser_frames: stats.denoiser_frames,
            denoiser_misses: stats.denoiser_misses,
            denoiser_empty_outputs: stats.denoiser_empty_outputs,
            denoiser_process_max_us: stats.denoiser_process_max_us,
            denoiser_slow_frames: stats.denoiser_slow_frames,
            denoiser_bypass_reason: stats.denoiser_bypass_reason,
            denoiser_notifications: stats.denoiser_notifications,
            erl_db: stats.erl_db,
            erle_db: stats.erle_db,
            aec_delay_ms: stats.aec_delay_ms,
            divergent_filter_fraction: stats.divergent_filter_fraction,
            echo_cancellation_active: stats.echo_cancellation_active,
            webrtc_ns_active: stats.webrtc_ns_active,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rootapp_audio_caps_cover_phase_0() {
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        assert_eq!(ROOTAPP_AUDIO_CAPS, 0b11111);
        assert_eq!(crate::cabi::livekit_ffi_rootapp_audio_caps(), ROOTAPP_AUDIO_CAPS);
        assert_eq!(ROOTAPP_AUDIO_CAPS & (CAP_TRACK_GAINS | CAP_EFFECTS | CAP_CONTROL), 0);
        assert_eq!(ROOTAPP_AUDIO_CAPS & (CAP_DSP | CAP_DENOISER | CAP_MONITOR), 0);
        assert_eq!(ROOTAPP_AUDIO_CAPS & CAP_APM_DECORATOR, 0);
    }

    #[test]
    fn rootapp_audio_device_selection_maps_every_case() {
        use proto::root_audio_device_selection::Device;
        let selection = |device| proto::RootAudioDeviceSelection { device };
        assert_eq!(
            device_selection(selection(Some(Device::SystemDefault(true)))),
            AudioDeviceSelection::SystemDefault
        );
        assert_eq!(device_selection(selection(None)), AudioDeviceSelection::SystemDefault);
        assert_eq!(
            device_selection(selection(Some(Device::Guid("{0.0.1}".into())))),
            AudioDeviceSelection::Guid("{0.0.1}".into())
        );
        assert_eq!(
            device_selection(selection(Some(Device::Name("Headset".into())))),
            AudioDeviceSelection::Name("Headset".into())
        );
    }

    #[test]
    fn rootapp_audio_stats_convert_field_for_field() {
        let stats = RootAudioStats {
            capture_callbacks: 3,
            render_callbacks: 4,
            mix_ssrcs: vec![9],
            playout_start_result: -1,
            ..Default::default()
        };
        let proto: proto::RootAudioStats = stats.into();
        assert_eq!(proto.capture_callbacks, 3);
        assert_eq!(proto.render_callbacks, 4);
        assert_eq!(proto.mix_ssrcs, vec![9]);
        assert_eq!(proto.playout_start_result, -1);
        assert!(proto.erle_db.is_nan());
    }
}
