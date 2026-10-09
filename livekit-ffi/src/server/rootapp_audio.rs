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
//! Controls, track gains and effects store atomics or take short locks the
//! audio threads never take.
//!
//! Track gains are keyed by the remote track's FFI handle and applied to the
//! playout mixer by SSRC: the receiver's signaled SSRC (the SFU signals one per
//! subscribed track), or the track's inbound-rtp SSRC for an unsignaled stream.
//! The SSRC is resolved off the caller's thread when a gain arrives for a track
//! whose SSRC is not known yet, and again every second, so a renegotiated SSRC
//! moves the gain along; gains of dropped track handles are cleared.

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use livekit::{
    prelude::*, sanitize_volume, webrtc::stats::RtcStats, AudioDeviceResults,
    AudioDeviceSelection, EffectPlay, PlatformAudio, PushToTalk, RootAudioControl,
    RootAudioStats,
};
use parking_lot::Mutex;
use tokio::time::MissedTickBehavior;

use super::{
    platform_audio::{platform_audio_from_handle, FfiPlatformAudio},
    room::FfiTrack,
    FfiServer,
};
use crate::{proto, FfiError, FfiHandleId, FfiResult};

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
    CAP_SELECT_DEVICES
        | CAP_PLAYOUT_HOLD
        | CAP_STATS
        | CAP_ADM_HARDENED
        | CAP_MIXER
        | CAP_TRACK_GAINS
        | CAP_EFFECTS
        | CAP_CONTROL
        | CAP_APM_DECORATOR
};

const NOT_SUPPORTED: &str = "not supported by this build";

/// How often the session task reads the native format atomics.
const EVENT_POLL: Duration = Duration::from_millis(10);
/// Every this many polls the track SSRCs are resolved again (1 s).
const RESOLVE_EVERY_POLLS: u32 = 100;
/// A track whose SSRC came from its stats is re-read only every this many passes.
const STATS_RESOLVE_EVERY_PASSES: u32 = 5;
/// The longest effect clip RegisterEffect accepts (60 s at 48 kHz).
const MAX_EFFECT_SAMPLES: u32 = 48_000 * 60;

struct TrackGain {
    track: RemoteAudioTrack,
    tile: f32,
    paused: bool,
    ssrc: Option<u32>,
    from_stats: bool,
}

#[derive(Default)]
struct SessionState {
    closed: bool,
    gains: HashMap<FfiHandleId, TrackGain>,
    resolve_passes: u32,
}

/// The FFI side of one `PlatformAudio` handle: the track gains it set and the
/// task that turns native state into `RootAudioEvent`s.
pub struct RootAudioSession {
    handle: FfiHandleId,
    audio: PlatformAudio,
    state: Mutex<SessionState>,
    resolving: AtomicBool,
}

impl RootAudioSession {
    /// Creates the session and starts its event task, which ends when the
    /// handle is dropped.
    pub fn start(
        server: &'static FfiServer,
        handle: FfiHandleId,
        audio: PlatformAudio,
    ) -> Arc<Self> {
        let session = Arc::new(Self {
            handle,
            audio,
            state: Mutex::new(SessionState::default()),
            resolving: AtomicBool::new(false),
        });
        let dropped = server.watch_handle_dropped(handle);
        let task = server.async_runtime.spawn(session.clone().run(server, dropped));
        server.watch_panic(task);
        session
    }

    /// The handle was dropped: controls go back to their defaults so the next
    /// PlatformAudio starts clean, and nothing this session resolves later is
    /// applied.
    pub fn end(&self) {
        let mut state = self.state.lock();
        if state.closed {
            return;
        }
        state.closed = true;
        state.gains.clear();
        self.audio.reset_rootapp_controls();
    }

    async fn run(
        self: Arc<Self>,
        server: &'static FfiServer,
        mut dropped: tokio::sync::oneshot::Receiver<()>,
    ) {
        let mut interval = tokio::time::interval(EVENT_POLL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut last_format_seq = 0u64;
        let mut polls = 0u32;
        loop {
            tokio::select! {
                _ = &mut dropped => break,
                _ = interval.tick() => {}
            }
            let format = self.audio.capture_format();
            if format.seq != last_format_seq {
                last_format_seq = format.seq;
                send_root_audio_event(
                    server,
                    self.handle,
                    proto::RootAudioCaptureFormat {
                        sample_rate: format.sample_rate,
                        channels: format.channels,
                    }
                    .into(),
                );
            }
            polls = polls.wrapping_add(1);
            if polls % RESOLVE_EVERY_POLLS == 0 {
                self.clone().spawn_resolve_pass(server);
            }
        }
    }

    fn spawn_resolve_pass(self: Arc<Self>, server: &'static FfiServer) {
        if self.resolving.swap(true, Ordering::AcqRel) {
            return;
        }
        let task = server.async_runtime.spawn(async move {
            self.resolve_pass(server).await;
            self.resolving.store(false, Ordering::Release);
        });
        server.watch_panic(task);
    }

    /// Drops the gains of track handles that are gone and resolves the others'
    /// SSRCs again.
    async fn resolve_pass(&self, server: &'static FfiServer) {
        let (handles, pass) = {
            let mut state = self.state.lock();
            state.resolve_passes = state.resolve_passes.wrapping_add(1);
            let handles: Vec<(FfiHandleId, bool, bool)> = state
                .gains
                .iter()
                .map(|(handle, gain)| (*handle, gain.ssrc.is_some(), gain.from_stats))
                .collect();
            (handles, state.resolve_passes)
        };
        let mut live = Vec::new();
        for (handle, known, from_stats) in handles {
            if server.retrieve_handle::<FfiTrack>(handle).is_err() {
                self.forget(handle);
            } else if !known || !from_stats || pass % STATS_RESOLVE_EVERY_PASSES == 0 {
                live.push(handle);
            }
        }
        self.resolve(live).await;
    }

    async fn resolve(&self, handles: Vec<FfiHandleId>) {
        for handle in handles {
            let track = match self.state.lock().gains.get(&handle) {
                Some(gain) => gain.track.clone(),
                None => continue,
            };
            if let Some(resolved) = resolve_ssrc(track).await {
                self.set_ssrc(handle, resolved);
            }
        }
    }

    fn spawn_resolve(self: &Arc<Self>, server: &'static FfiServer, handles: Vec<FfiHandleId>) {
        if handles.is_empty() {
            return;
        }
        let session = self.clone();
        let task = server.async_runtime.spawn(async move { session.resolve(handles).await });
        server.watch_panic(task);
    }

    fn set_ssrc(&self, handle: FfiHandleId, (ssrc, from_stats): (u32, bool)) {
        let mut state = self.state.lock();
        if state.closed {
            return;
        }
        let Some(gain) = state.gains.get_mut(&handle) else {
            return;
        };
        gain.from_stats = from_stats;
        let previous = gain.ssrc.replace(ssrc);
        let (tile, paused) = (gain.tile, gain.paused);
        if previous == Some(ssrc) {
            return;
        }
        if let Some(previous) = previous {
            self.clear_if_unused(&state, previous);
        }
        self.apply(ssrc, tile, paused);
    }

    fn forget(&self, handle: FfiHandleId) {
        let mut state = self.state.lock();
        if let Some(ssrc) = state.gains.remove(&handle).and_then(|gain| gain.ssrc) {
            self.clear_if_unused(&state, ssrc);
        }
    }

    fn clear_if_unused(&self, state: &SessionState, ssrc: u32) {
        if !state.closed && !state.gains.values().any(|gain| gain.ssrc == Some(ssrc)) {
            self.audio.clear_source_gain(ssrc);
        }
    }

    fn apply(&self, ssrc: u32, tile: f32, paused: bool) {
        if !self.audio.set_source_gain(ssrc, tile, paused) {
            log::warn!("RootAudio: the mixer's gain table is full; ssrc {ssrc} plays at tile 1");
        }
    }

    /// Stores the gains and applies the ones whose SSRC is known; returns the
    /// handles still to resolve, or Err when the session has ended.
    fn set_gains(
        &self,
        gains: Vec<(FfiHandleId, RemoteAudioTrack, f32, Option<bool>)>,
    ) -> Result<Vec<FfiHandleId>, ()> {
        let mut state = self.state.lock();
        if state.closed {
            return Err(());
        }
        let mut unresolved = Vec::new();
        for (handle, track, tile, paused) in gains {
            let gain = state.gains.entry(handle).or_insert_with(|| TrackGain {
                track,
                tile: 1.0,
                paused: false,
                ssrc: None,
                from_stats: false,
            });
            gain.tile = tile;
            if let Some(paused) = paused {
                gain.paused = paused;
            }
            match gain.ssrc {
                Some(ssrc) => {
                    let (tile, paused) = (gain.tile, gain.paused);
                    self.apply(ssrc, tile, paused);
                }
                None => unresolved.push(handle),
            }
        }
        Ok(unresolved)
    }
}

/// The SSRC of the stream `track` plays, and whether it came from the stats:
/// the receiver's signaled SSRC first (a hop to libwebrtc's worker thread, off
/// the async runtime), else the track's inbound-rtp SSRC.
async fn resolve_ssrc(track: RemoteAudioTrack) -> Option<(u32, bool)> {
    let signaled = {
        let track = track.clone();
        tokio::task::spawn_blocking(move || track.signaled_ssrc()).await.ok().flatten()
    };
    if let Some(ssrc) = signaled {
        return Some((ssrc, false));
    }
    let stats = track.get_stats().await.ok()?;
    stats
        .iter()
        .find_map(|stats| match stats {
            RtcStats::InboundRtp(inbound) if inbound.stream.ssrc != 0 => Some(inbound.stream.ssrc),
            _ => None,
        })
        .map(|ssrc| (ssrc, true))
}

fn session_from_handle(
    server: &'static FfiServer,
    handle: FfiHandleId,
) -> FfiResult<Arc<RootAudioSession>> {
    Ok(server.retrieve_handle::<FfiPlatformAudio>(handle)?.rootapp.clone())
}

fn remote_audio_track(server: &'static FfiServer, handle: FfiHandleId) -> Option<RemoteAudioTrack> {
    match &server.retrieve_handle::<FfiTrack>(handle).ok()?.track {
        Track::RemoteAudio(track) => Some(track.clone()),
        _ => None,
    }
}

/// The proto's control, with only the fields that are set.
pub fn root_audio_control(control: &proto::RootAudioControl) -> RootAudioControl {
    RootAudioControl {
        mic_muted: control.mic_muted,
        push_to_talk: control.push_to_talk.map(PushToTalk::from_wire),
        input_volume: control.input_volume,
        master_volume: control.master_volume,
        deafened: control.deafened,
        echo_cancellation: control.echo_cancellation,
        webrtc_noise_suppression: control.webrtc_noise_suppression,
        denoise_power: control.denoise_power,
        auto_gain: control.auto_gain,
        gate_threshold: control.gate_threshold,
        auto_sensitivity: control.auto_sensitivity,
        gain_reset_db: control.gain_reset_db,
    }
    .sanitized()
}

/// An effect clip from the host's memory: `sample_count` int16 samples at
/// `data_ptr`, which must stay valid for the duration of the request.
fn effect_samples(req: &proto::RootAudioRegisterEffectRequest) -> Result<&[i16], String> {
    if req.data_ptr == 0 || req.sample_count == 0 {
        return Err("effect clip is empty".into());
    }
    if req.sample_count > MAX_EFFECT_SAMPLES {
        return Err(format!("effect clip of {} samples is longer than 60 s", req.sample_count));
    }
    if req.data_ptr % std::mem::align_of::<i16>() as u64 != 0 {
        return Err("effect clip pointer is not aligned for int16".into());
    }
    // SAFETY: the host keeps `sample_count` int16 samples at `data_ptr` alive and
    // unchanged until the request returns (checked non-null, aligned, bounded).
    Ok(unsafe {
        std::slice::from_raw_parts(req.data_ptr as usize as *const i16, req.sample_count as usize)
    })
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
    let audio = platform_audio_from_handle(server, req.platform_audio_handle)?;
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
    let audio = platform_audio_from_handle(server, req.platform_audio_handle)?;
    let async_id = server.next_id();
    let results = audio.restart_devices(req.recording, req.playout);
    spawn_device_result(server, req.platform_audio_handle, async_id, results);
    Ok(proto::RootAudioRestartDeviceResponse { async_id })
}

pub fn on_set_playout_hold(
    server: &'static FfiServer,
    req: proto::RootAudioSetPlayoutHoldRequest,
) -> FfiResult<proto::RootAudioSetPlayoutHoldResponse> {
    platform_audio_from_handle(server, req.platform_audio_handle)?.set_playout_hold(req.hold);
    Ok(proto::RootAudioSetPlayoutHoldResponse { error: None })
}

pub fn on_stats(
    server: &'static FfiServer,
    req: proto::RootAudioStatsRequest,
) -> FfiResult<proto::RootAudioStatsResponse> {
    let stats =
        platform_audio_from_handle(server, req.platform_audio_handle)?.rootapp_audio_stats();
    Ok(proto::RootAudioStatsResponse { stats: Some(stats.into()), error: None })
}

/// Every field is sanitised (finite, clamped; an unknown push-to-talk value
/// closes the gate). mic_muted, push_to_talk, input_volume, master_volume and
/// deafened take effect at once; echo_cancellation and
/// webrtc_noise_suppression within a second (rate-limited APM policy).
/// denoise_power, auto_gain, gate_threshold, auto_sensitivity and
/// gain_reset_db are stored for the DSP chain and have no effect yet.
pub fn on_set_control(
    server: &'static FfiServer,
    req: proto::RootAudioSetControlRequest,
) -> FfiResult<proto::RootAudioSetControlResponse> {
    let audio = platform_audio_from_handle(server, req.platform_audio_handle)?;
    audio.set_rootapp_control(&root_audio_control(&req.control));
    Ok(proto::RootAudioSetControlResponse { error: None })
}

/// Tile volumes (slider / 100, 0..2, NaN is 0) per remote track handle; an
/// absent `paused` keeps the track's pause. Unknown handles are reported in
/// the error, the other gains are applied.
pub fn on_set_track_gains(
    server: &'static FfiServer,
    req: proto::RootAudioSetTrackGainsRequest,
) -> FfiResult<proto::RootAudioSetTrackGainsResponse> {
    let session = session_from_handle(server, req.platform_audio_handle)?;
    let mut unknown = Vec::new();
    let mut gains = Vec::with_capacity(req.gains.len());
    for gain in &req.gains {
        match remote_audio_track(server, gain.track_handle) {
            Some(track) => {
                gains.push((gain.track_handle, track, sanitize_volume(gain.gain), gain.paused))
            }
            None => unknown.push(gain.track_handle),
        }
    }
    let unresolved = session
        .set_gains(gains)
        .map_err(|_| FfiError::InvalidRequest("platform audio handle was dropped".into()))?;
    session.spawn_resolve(server, unresolved);
    let error = (!unknown.is_empty()).then(|| {
        format!("not remote audio track handles: {unknown:?}; the other gains are applied")
    });
    Ok(proto::RootAudioSetTrackGainsResponse { error })
}

/// Copies the clip before returning.
pub fn on_register_effect(
    server: &'static FfiServer,
    req: proto::RootAudioRegisterEffectRequest,
) -> FfiResult<proto::RootAudioRegisterEffectResponse> {
    let audio = platform_audio_from_handle(server, req.platform_audio_handle)?;
    let error = match effect_samples(&req) {
        Ok(samples) => audio.register_effect(req.effect_id, samples).err().map(|e| e.to_string()),
        Err(e) => Some(e),
    };
    Ok(proto::RootAudioRegisterEffectResponse { error })
}

/// A full queue is not an error (the effect is counted in effects_dropped); an
/// unregistered id is.
pub fn on_play_effect(
    server: &'static FfiServer,
    req: proto::RootAudioPlayEffectRequest,
) -> FfiResult<proto::RootAudioPlayEffectResponse> {
    let audio = platform_audio_from_handle(server, req.platform_audio_handle)?;
    let error = match audio.play_effect(req.effect_id, req.gain) {
        EffectPlay::UnknownClip => Some(format!("effect {} is not registered", req.effect_id)),
        EffectPlay::Queued | EffectPlay::Dropped | EffectPlay::Idle => None,
    };
    Ok(proto::RootAudioPlayEffectResponse { error })
}

pub fn on_stop_effects(
    server: &'static FfiServer,
    req: proto::RootAudioStopEffectsRequest,
) -> FfiResult<proto::RootAudioStopEffectsResponse> {
    platform_audio_from_handle(server, req.platform_audio_handle)?.stop_effects();
    Ok(proto::RootAudioStopEffectsResponse { error: None })
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
            audio_thread_lock_violations: Some(stats.audio_thread_lock_violations),
            audio_thread_logs_dropped: Some(stats.audio_thread_logs_dropped),
            overlapping_callbacks: Some(stats.overlapping_callbacks),
            device_stop_failures: Some(stats.device_stop_failures),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rootapp_audio_caps_cover_phase_1() {
        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        {
            assert_eq!(ROOTAPP_AUDIO_CAPS, 0b1000_1111_1111);
            assert_eq!(
                ROOTAPP_AUDIO_CAPS & (CAP_TRACK_GAINS | CAP_EFFECTS | CAP_CONTROL),
                CAP_TRACK_GAINS | CAP_EFFECTS | CAP_CONTROL
            );
            assert_eq!(ROOTAPP_AUDIO_CAPS & CAP_APM_DECORATOR, CAP_APM_DECORATOR);
        }
        assert_eq!(crate::cabi::livekit_ffi_rootapp_audio_caps(), ROOTAPP_AUDIO_CAPS);
        assert_eq!(ROOTAPP_AUDIO_CAPS & (CAP_DSP | CAP_DENOISER | CAP_MONITOR), 0);
    }

    #[test]
    fn rootapp_audio_control_keeps_unset_fields_unset() {
        let control = root_audio_control(&proto::RootAudioControl {
            mic_muted: Some(true),
            ..Default::default()
        });
        assert_eq!(control, RootAudioControl { mic_muted: Some(true), ..Default::default() });
    }

    #[test]
    fn rootapp_audio_control_sanitises_every_field() {
        let control = root_audio_control(&proto::RootAudioControl {
            mic_muted: Some(false),
            push_to_talk: Some(9),
            input_volume: Some(f32::NAN),
            master_volume: Some(f32::INFINITY),
            deafened: Some(true),
            echo_cancellation: Some(false),
            webrtc_noise_suppression: Some(true),
            denoise_power: Some(f32::NEG_INFINITY),
            auto_gain: Some(false),
            gate_threshold: Some(3.0),
            auto_sensitivity: Some(true),
            gain_reset_db: Some(f32::NAN),
        });
        assert_eq!(control.push_to_talk, Some(PushToTalk::Released));
        assert_eq!(control.input_volume, Some(0.0));
        assert_eq!(control.master_volume, Some(2.0));
        assert_eq!(control.denoise_power, Some(0.0));
        assert_eq!(control.gate_threshold, Some(1.0));
        assert_eq!(control.gain_reset_db, None);
        assert_eq!(
            (control.mic_muted, control.deafened, control.echo_cancellation),
            (Some(false), Some(true), Some(false))
        );
        assert_eq!(
            (control.webrtc_noise_suppression, control.auto_gain, control.auto_sensitivity),
            (Some(true), Some(false), Some(true))
        );

        let push_to_talk = |value| {
            root_audio_control(&proto::RootAudioControl {
                push_to_talk: Some(value),
                ..Default::default()
            })
            .push_to_talk
        };
        assert_eq!(push_to_talk(0), Some(PushToTalk::VoiceActivity));
        assert_eq!(push_to_talk(1), Some(PushToTalk::Held));
        assert_eq!(push_to_talk(2), Some(PushToTalk::Released));
    }

    fn effect_request(data_ptr: u64, sample_count: u32) -> proto::RootAudioRegisterEffectRequest {
        proto::RootAudioRegisterEffectRequest {
            platform_audio_handle: 1,
            effect_id: 7,
            data_ptr,
            sample_count,
        }
    }

    #[test]
    fn rootapp_audio_effect_clips_are_checked_before_they_are_read() {
        let clip: Vec<i16> = (0..480).map(|i| i as i16).collect();
        let ptr = clip.as_ptr() as usize as u64;
        let request = effect_request(ptr, 480);
        assert_eq!(effect_samples(&request).unwrap(), clip.as_slice());

        assert!(effect_samples(&effect_request(0, 480)).is_err());
        assert!(effect_samples(&effect_request(ptr, 0)).is_err());
        assert!(effect_samples(&effect_request(ptr, MAX_EFFECT_SAMPLES + 1)).is_err());
        assert!(effect_samples(&effect_request(ptr + 1, 4)).is_err());
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
            audio_thread_lock_violations: 1,
            overlapping_callbacks: 2,
            ..Default::default()
        };
        let proto: proto::RootAudioStats = stats.into();
        assert_eq!(proto.capture_callbacks, 3);
        assert_eq!(proto.render_callbacks, 4);
        assert_eq!(proto.mix_ssrcs, vec![9]);
        assert_eq!(proto.playout_start_result, -1);
        assert!(proto.erle_db.is_nan());
        assert_eq!(proto.audio_thread_lock_violations, Some(1));
        assert_eq!(proto.audio_thread_logs_dropped, Some(0));
        assert_eq!(proto.overlapping_callbacks, Some(2));
        assert_eq!(proto.device_stop_failures, Some(0));
    }
}
