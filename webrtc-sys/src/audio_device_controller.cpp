/*
 * Copyright 2026 LiveKit, Inc.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

#include "livekit/audio_device_controller.h"

#include <algorithm>
#include <cmath>
#include <limits>
#include <memory>
#include <optional>
#include <string>
#include <utility>

#include "livekit/root_agc2.h"
#include "livekit/root_echo_model.h"

namespace livekit_ffi {
namespace {

AudioDeviceOutcome ToOutcome(const AdmDirectionOutcome& outcome) {
  AudioDeviceOutcome result{};
  result.requested = outcome.requested;
  result.found = outcome.found;
  result.attempted = outcome.attempted;
  result.stop_result = outcome.stop_result;
  result.init_result = outcome.init_result;
  result.start_result = outcome.start_result;
  result.init_detail = rust::String::lossy(outcome.init_detail);
  return result;
}

AdmRequestCallback ToAdmCallback(
    rust::Box<AudioDeviceRequestCallback> callback) {
  auto shared =
      std::make_shared<rust::Box<AudioDeviceRequestCallback>>(
          std::move(callback));
  return [shared](const AdmRequestOutcome& outcome) {
    (*shared)->on_complete(ToRequestOutcome(outcome));
  };
}

}  // namespace

AudioDeviceRequestOutcome ToRequestOutcome(const AdmRequestOutcome& outcome) {
  AudioDeviceRequestOutcome result{};
  result.recording = ToOutcome(outcome.recording);
  result.playout = ToOutcome(outcome.playout);
  return result;
}

AdmDeviceSelection ToAdmSelection(const AudioDeviceSelection& selection) {
  AdmDeviceSelection result;
  switch (selection.kind) {
    case AudioDeviceSelectionKind::Guid:
      result.kind = AdmSelectionKind::kGuid;
      break;
    case AudioDeviceSelectionKind::Name:
      result.kind = AdmSelectionKind::kName;
      break;
    case AudioDeviceSelectionKind::Index:
      result.kind = AdmSelectionKind::kIndex;
      break;
    case AudioDeviceSelectionKind::Uid:
      result.kind = AdmSelectionKind::kUid;
      break;
    default:
      result.kind = AdmSelectionKind::kDefault;
      break;
  }
  result.id = std::string(selection.id);
  result.index = selection.index;
  return result;
}

AudioDeviceController::AudioDeviceController(
    std::shared_ptr<RtcRuntime> rtc_runtime,
    webrtc::scoped_refptr<AdmProxy> adm_proxy)
    : rtc_runtime_(std::move(rtc_runtime)), adm_proxy_(std::move(adm_proxy)) {}

AudioDeviceController::AudioDeviceController(
    std::shared_ptr<RtcRuntime> rtc_runtime,
    webrtc::scoped_refptr<AdmProxy> adm_proxy,
    webrtc::scoped_refptr<RootAudioMixer> mixer,
    webrtc::scoped_refptr<RootApmDecorator> apm,
    std::shared_ptr<RootCaptureState> capture,
    std::shared_ptr<RootVoiceEngineHold> engine_hold)
    : rtc_runtime_(std::move(rtc_runtime)),
      adm_proxy_(std::move(adm_proxy)),
      mixer_(std::move(mixer)),
      apm_(std::move(apm)),
      capture_(std::move(capture)),
      engine_hold_(std::move(engine_hold)) {
  if (!capture_ || !capture_->denoiser_slot) {
    return;
  }
  std::shared_ptr<RootCaptureState> state = capture_;
  webrtc::scoped_refptr<RootApmDecorator> decorator = apm_;
  RootDenoiserControl::Hooks hooks;
  hooks.speaking = [state] { return state->AudiblySpeaking(); };
  hooks.wanted = [state] {
    return state->denoiser_wanted.load(std::memory_order_relaxed);
  };
  hooks.process_rate = [state] {
    return ReadCaptureFormat(*state).sample_rate;
  };
  hooks.processed_frames = [state] {
    return state->processed_frames.load(std::memory_order_relaxed);
  };
  hooks.stream_epoch = [state] {
    return state->stream_epoch.load(std::memory_order_relaxed);
  };
  hooks.fallback_decided = [state](bool on) {
    state->ns_fallback_decided.store(on, std::memory_order_relaxed);
  };
  hooks.set_fallback = [decorator](bool on) {
    if (decorator) {
      decorator->SetFallbackNoiseSuppression(on);
    }
  };
  denoiser_ = std::make_unique<RootDenoiserControl>(
      capture_->denoiser_slot, std::move(hooks), RootDenoiserControl::Timing());
}

int16_t AudioDeviceController::playout_devices() const {
  return adm_proxy_->PlayoutDevices();
}

int16_t AudioDeviceController::recording_devices() const {
  return adm_proxy_->RecordingDevices();
}

// Names may be cut inside a UTF-8 sequence, so they are passed on lossily.
rust::String AudioDeviceController::playout_device_name(uint16_t index) const {
  AdmNameBuffers buffers;
  adm_proxy_->PlayoutDeviceName(index, buffers.name, buffers.guid);
  return rust::String::lossy(buffers.Name());
}

rust::String AudioDeviceController::recording_device_name(uint16_t index) const {
  AdmNameBuffers buffers;
  adm_proxy_->RecordingDeviceName(index, buffers.name, buffers.guid);
  return rust::String::lossy(buffers.Name());
}

// Linux PulseAudio/ALSA does not populate device GUIDs in WebRTC. Synthesize an
// index-based identifier so callers can distinguish and select devices.
rust::String AudioDeviceController::playout_device_guid(uint16_t index) const {
  AdmNameBuffers buffers;
  adm_proxy_->PlayoutDeviceName(index, buffers.name, buffers.guid);
  std::string guid = buffers.Guid();
  if (guid.empty()) {
    return rust::String("index:" + std::to_string(index));
  }
  return rust::String::lossy(guid);
}

rust::String AudioDeviceController::recording_device_guid(uint16_t index) const {
  AdmNameBuffers buffers;
  adm_proxy_->RecordingDeviceName(index, buffers.name, buffers.guid);
  std::string guid = buffers.Guid();
  if (guid.empty()) {
    return rust::String("index:" + std::to_string(index));
  }
  return rust::String::lossy(guid);
}

#if LK_ADM_MOBILE

bool AudioDeviceController::set_playout_device(uint16_t index) const {
  return adm_proxy_->SetPlayoutDevice(index) == 0;
}

bool AudioDeviceController::set_recording_device(uint16_t index) const {
  return adm_proxy_->SetRecordingDevice(index) == 0;
}

bool AudioDeviceController::set_playout_device_by_guid(rust::String guid) const {
  int16_t count = adm_proxy_->PlayoutDevices();
  std::string target_guid = std::string(guid.c_str());

  if (target_guid.empty()) {
    return false;
  }

  // Handle synthetic index-based GUIDs (e.g. "index:1")
  if (target_guid.rfind("index:", 0) == 0) {
    try {
      size_t pos = 0;
      int idx = std::stoi(target_guid.substr(6), &pos);
      if (pos == target_guid.length() - 6 && idx >= 0 && idx < count) {
        return adm_proxy_->SetPlayoutDevice(static_cast<uint16_t>(idx)) == 0;
      }
    } catch (...) {}
    return false;
  }

  // Try to find a device matching the GUID (only match non-empty device GUIDs)
  for (int16_t i = 0; i < count; i++) {
    char name[webrtc::kAdmMaxDeviceNameSize] = {0};
    char device_guid[webrtc::kAdmMaxGuidSize] = {0};
    if (adm_proxy_->PlayoutDeviceName(i, name, device_guid) == 0) {
      if (device_guid[0] != '\0' && target_guid == std::string(device_guid)) {
        return adm_proxy_->SetPlayoutDevice(i) == 0;
      }
    }
  }

  // Fallback: match by device name if no GUID matched
  for (int16_t i = 0; i < count; i++) {
    char name[webrtc::kAdmMaxDeviceNameSize] = {0};
    char device_guid[webrtc::kAdmMaxGuidSize] = {0};
    if (adm_proxy_->PlayoutDeviceName(i, name, device_guid) == 0) {
      if (target_guid == std::string(name)) {
        return adm_proxy_->SetPlayoutDevice(i) == 0;
      }
    }
  }

#if defined(WEBRTC_IOS) || defined(WEBRTC_ANDROID)
  // On mobile platforms, device selection is a no-op (system handles routing),
  // so fall back to default device (index 0).
  if (count > 0) {
    return adm_proxy_->SetPlayoutDevice(0) == 0;
  }
#endif

  return false;
}

bool AudioDeviceController::set_recording_device_by_guid(rust::String guid) const {
  int16_t count = adm_proxy_->RecordingDevices();
  std::string target_guid = std::string(guid.c_str());

  if (target_guid.empty()) {
    return false;
  }

  // Handle synthetic index-based GUIDs (e.g. "index:1")
  if (target_guid.rfind("index:", 0) == 0) {
    try {
      size_t pos = 0;
      int idx = std::stoi(target_guid.substr(6), &pos);
      if (pos == target_guid.length() - 6 && idx >= 0 && idx < count) {
        return adm_proxy_->SetRecordingDevice(static_cast<uint16_t>(idx)) == 0;
      }
    } catch (...) {}
    return false;
  }

  // Try to find a device matching the GUID (only match non-empty device GUIDs)
  for (int16_t i = 0; i < count; i++) {
    char name[webrtc::kAdmMaxDeviceNameSize] = {0};
    char device_guid[webrtc::kAdmMaxGuidSize] = {0};
    if (adm_proxy_->RecordingDeviceName(i, name, device_guid) == 0) {
      if (device_guid[0] != '\0' && target_guid == std::string(device_guid)) {
        return adm_proxy_->SetRecordingDevice(i) == 0;
      }
    }
  }

  // Fallback: match by device name if no GUID matched
  for (int16_t i = 0; i < count; i++) {
    char name[webrtc::kAdmMaxDeviceNameSize] = {0};
    char device_guid[webrtc::kAdmMaxGuidSize] = {0};
    if (adm_proxy_->RecordingDeviceName(i, name, device_guid) == 0) {
      if (target_guid == std::string(name)) {
        return adm_proxy_->SetRecordingDevice(i) == 0;
      }
    }
  }

#if defined(WEBRTC_IOS) || defined(WEBRTC_ANDROID)
  // On mobile platforms, device selection is a no-op (system handles routing),
  // so fall back to default device (index 0).
  if (count > 0) {
    return adm_proxy_->SetRecordingDevice(0) == 0;
  }
#endif

  return false;
}

#else

// On desktop the selection is stored and applied before the next open; a
// running direction moves to the new device. A device that is not present is
// never replaced by another one.
bool AudioDeviceController::set_playout_device(uint16_t index) const {
  if (index >= adm_proxy_->PlayoutDevices()) {
    return false;
  }
  AdmDeviceSelection selection;
  selection.kind = AdmSelectionKind::kIndex;
  selection.index = index;
  adm_proxy_->SelectDevices(std::nullopt, selection, nullptr);
  return true;
}

bool AudioDeviceController::set_recording_device(uint16_t index) const {
  if (index >= adm_proxy_->RecordingDevices()) {
    return false;
  }
  AdmDeviceSelection selection;
  selection.kind = AdmSelectionKind::kIndex;
  selection.index = index;
  adm_proxy_->SelectDevices(selection, std::nullopt, nullptr);
  return true;
}

bool AudioDeviceController::set_playout_device_by_guid(rust::String guid) const {
  AdmDeviceSelection selection;
  selection.kind = AdmSelectionKind::kGuid;
  selection.id = std::string(guid);
  adm_proxy_->SelectDevices(std::nullopt, selection, nullptr);
  return true;
}

bool AudioDeviceController::set_recording_device_by_guid(rust::String guid) const {
  AdmDeviceSelection selection;
  selection.kind = AdmSelectionKind::kGuid;
  selection.id = std::string(guid);
  adm_proxy_->SelectDevices(selection, std::nullopt, nullptr);
  return true;
}

#endif

void AudioDeviceController::select_devices(
    const AudioDeviceSelection& recording,
    bool set_recording,
    const AudioDeviceSelection& playout,
    bool set_playout,
    rust::Box<AudioDeviceRequestCallback> callback) const {
  std::optional<AdmDeviceSelection> recording_selection;
  std::optional<AdmDeviceSelection> playout_selection;
  if (set_recording) {
    recording_selection = ToAdmSelection(recording);
  }
  if (set_playout) {
    playout_selection = ToAdmSelection(playout);
  }
  adm_proxy_->SelectDevices(std::move(recording_selection),
                            std::move(playout_selection),
                            ToAdmCallback(std::move(callback)));
}

void AudioDeviceController::restart_devices(
    bool recording,
    bool playout,
    rust::Box<AudioDeviceRequestCallback> callback) const {
  adm_proxy_->RestartDevices(recording, playout,
                             ToAdmCallback(std::move(callback)));
}

void AudioDeviceController::set_playout_hold(bool hold) const {
  adm_proxy_->SetPlayoutHold(hold);
}

RootAudioNativeStats AudioDeviceController::root_audio_stats() const {
  AdmNativeStats native = adm_proxy_->ReadNativeStats();
  RootAudioNativeStats stats{};
  stats.capture_callbacks = native.tap.capture_callbacks;
  stats.render_callbacks = native.tap.render_callbacks;
  stats.capture_max_gap_us = native.tap.capture_max_gap_us;
  stats.render_max_gap_us = native.tap.render_max_gap_us;
  stats.last_capture_age_us = native.tap.last_capture_age_us;
  stats.last_render_age_us = native.tap.last_render_age_us;
  stats.recording = native.recording;
  stats.playing = native.playing;
  stats.recording_init_result = native.recording_init_result;
  stats.recording_start_result = native.recording_start_result;
  stats.playout_init_result = native.playout_init_result;
  stats.playout_start_result = native.playout_start_result;
  stats.capture_rate = native.tap.capture_rate;
  stats.capture_channels = native.tap.capture_channels;
  stats.capture_raw_sumsq = native.tap.capture_raw_sumsq;
  stats.capture_raw_samples = native.tap.capture_raw_samples;
  stats.playout_delay_ms = native.tap.playout_delay_ms;
  stats.recording_delay_ms = native.tap.recording_delay_ms;
  stats.mix_calls = native.mix_calls;
  stats.mix_max_gap_us = native.mix_max_gap_us;
  stats.mix_sources = native.mix_sources;
  for (uint32_t ssrc : native.mix_ssrcs) {
    stats.mix_ssrcs.push_back(ssrc);
  }
  stats.mix_out_sumsq = native.mix_out_sumsq;
  stats.mix_out_samples = native.mix_out_samples;
  stats.effects_played = native.effects_played;
  stats.effects_dropped = native.effects_dropped;
  if (mixer_) {
    stats.mix_source_levels_available = true;
    for (const RootAudioMixerSourceLevel& level : mixer_->ReadSourceLevels()) {
      stats.mix_source_levels.push_back(RootMixSourceLevel{
          level.ssrc, level.source_id, level.sumsq, level.samples});
    }
  }
  stats.echo_cancellation_active = native.echo_cancellation;
  stats.webrtc_ns_active = native.noise_suppression;
  stats.audio_thread_lock_violations = native.audio_thread_lock_violations;
  stats.audio_thread_logs_dropped = native.audio_thread_logs_dropped;
  stats.overlapping_callbacks = native.overlapping_callbacks;
  stats.device_stop_failures = native.device_stop_failures;
  stats.device_formats_available = native.device_formats_available;
  stats.playout_device_rate = native.playout_device_rate;
  stats.playout_device_channels = native.playout_device_channels;
  stats.recording_device_rate = native.recording_device_rate;
  stats.recording_device_channels = native.recording_device_channels;
  stats.playout_format_changes = native.playout_format_changes;
  stats.playout_format_reopens = native.playout_format_reopens;
  stats.recording_format_changes = native.recording_format_changes;
  stats.device_changes_available = native.device_changes_available;
  stats.playout_device_uid = rust::String::lossy(native.playout_device_uid);
  stats.recording_device_uid = rust::String::lossy(native.recording_device_uid);
  stats.same_device = native.same_device;
  stats.device_list_notifications = native.device_list_notifications;
  stats.default_output_notifications = native.default_output_notifications;
  stats.default_input_notifications = native.default_input_notifications;
  stats.stream_format_notifications = native.stream_format_notifications;
  stats.other_device_notifications = native.other_notifications;
  auto reopens = [&native](AdmReopenReason reason) {
    return native.reopens[static_cast<size_t>(reason)];
  };
  stats.user_select_reopens = reopens(AdmReopenReason::kUserSelect);
  stats.default_change_reopens = reopens(AdmReopenReason::kDefaultChange);
  stats.device_lost_reopens = reopens(AdmReopenReason::kDeviceLost);
  stats.format_change_reopens = reopens(AdmReopenReason::kFormatChange);
  stats.render_stall_reopens = reopens(AdmReopenReason::kRenderStall);
  stats.coupled_reopens = native.coupled_reopens;
  stats.render_stall_recoveries = native.render_stall_recoveries;
  stats.playout_fallback_available = native.playout_fallback_available;
  stats.playout_fallback = native.playout_fallback;
  stats.playout_fallback_opens = native.playout_fallback_opens;
  stats.playout_init_detail = rust::String::lossy(native.playout_init_detail);

  const double nan = std::numeric_limits<double>::quiet_NaN();
  stats.erl_db = nan;
  stats.erle_db = nan;
  stats.aec_delay_ms = -1;
  stats.divergent_filter_fraction = nan;
  if (apm_) {
    const RootApmStats apm = apm_->ReadStats();
    stats.erl_db = apm.erl_db;
    stats.erle_db = apm.erle_db;
    stats.aec_delay_ms = apm.delay_ms;
    stats.divergent_filter_fraction = apm.divergent_filter_fraction;
    stats.echo_cancellation_active = apm.echo_cancellation_active;
    stats.webrtc_ns_active = apm.noise_suppression_active;
  }
  stats.gate_in_block_peak_db = std::numeric_limits<float>::quiet_NaN();
  stats.voice_gain_db = std::numeric_limits<float>::quiet_NaN();
  stats.denoiser_frames_until_first_output = -1;
  if (capture_) {
    const RootCaptureFormatSnapshot format = ReadCaptureFormat(*capture_);
    stats.process_rate = format.sample_rate;
    stats.process_channels = format.channels;
    capture_->ClearStaleSpeaking();
    const RootDspStatsSnapshot dsp = capture_->ReadDspStats(true);
    stats.dsp_available = kRootDspSupported;
    stats.processed_frames = dsp.processed_frames;
    stats.stage_samples = dsp.stage_samples;
    stats.apm_out_sumsq = dsp.apm_out_sumsq;
    stats.denoised_sumsq = dsp.denoised_sumsq;
    stats.gate_in_sumsq = dsp.gate_in_sumsq;
    stats.sent_sumsq = dsp.sent_sumsq;
    stats.gate_in_block_peak_db = dsp.gate_in_block_peak_db;
    stats.threshold = dsp.threshold;
    stats.envelope = dsp.envelope_max;
    stats.gate_speaking = dsp.gate_speaking;
    stats.speaking_seq = dsp.speaking_seq;
    stats.voice_gain_db = dsp.voice_gain_db;
    stats.voice_gain_settled = dsp.voice_gain_settled;
    stats.gate_open_frames = dsp.gate_open_frames;
    stats.speaking = dsp.speaking;
    stats.gate_input_above_frames = dsp.gate_input_above_frames;
    stats.startup_hold_frames = dsp.startup_hold_frames;
    stats.transmit_frames = dsp.transmit_frames;
    stats.unsupported_frames = dsp.unsupported_frames;
    stats.non_finite_samples = dsp.non_finite_samples;
    stats.dsp_resets = dsp.dsp_resets;
    stats.dsp_flushes = dsp.dsp_flushes;
    stats.gate_open_delay_us = dsp.gate_open_delay_us;
    stats.ns_gap_frames = dsp.ns_gap_frames;
    stats.ns_fallback_pending_frames = dsp.ns_fallback_pending_frames;
    stats.denoiser_latency_ms = dsp.aligner_latency_ms;
    stats.denoiser_buffered_samples = dsp.aligner_buffered_samples;
    stats.denoiser_underruns = dsp.aligner_underruns;
    stats.denoiser_padded_samples = dsp.aligner_padded_samples;
    stats.denoiser_trimmed_samples = dsp.aligner_trimmed_samples;
    stats.denoiser_drained_samples = dsp.aligner_drained_samples;
    stats.speech_available = kRootDspSupported;
    stats.speech_detection = dsp.speech_detection;
    stats.speech_probability = std::isnan(dsp.speech_probability_max)
                                   ? -1.0f
                                   : dsp.speech_probability_max;
    stats.speech_detector_running = dsp.speech_detector_running;
    stats.speech = dsp.speech;
    stats.speech_frames = dsp.speech_frames;
    stats.speech_closed_frames = dsp.speech_closed_frames;
    stats.speech_unavailable_frames = dsp.speech_unavailable_frames;
    stats.auto_sensitivity_minimum_db =
        root_auto_sensitivity_minimum_db(dsp.auto_sensitivity_minimum_db);
    const RootVoiceGainTuning voice_gain =
        root_voice_gain_tuning(dsp.voice_gain);
    stats.voice_gain_max_db = voice_gain.max_gain_db;
    stats.voice_gain_target_db = voice_gain.target_db;
    stats.voice_gain_rise_db_per_second = voice_gain.rise_db_per_second;
    stats.voice_gain_fall_db_per_second = voice_gain.fall_db_per_second;
    stats.voice_gain_level_time_constant_ms =
        voice_gain.level_time_constant_ms;
    stats.voice_gain_min_speech_probability =
        voice_gain.min_speech_probability;
    stats.gain_engine = dsp.gain_engine_applied;
    const RootAgc2Tuning agc2 = root_agc2_tuning(dsp.agc2);
    stats.agc2_headroom_db = agc2.headroom_db;
    stats.agc2_max_gain_db = agc2.max_gain_db;
    stats.agc2_max_gain_change_db_per_second =
        agc2.max_gain_change_db_per_second;
    stats.agc2_max_output_noise_dbfs = agc2.max_output_noise_dbfs;
    stats.agc2_builds = dsp.agc2_builds;
    stats.voice_gain_room_db = dsp.voice_gain_room_db;
    stats.voice_gain_speech_level_db = dsp.voice_gain_speech_level_db;
    stats.voice_gain_floor_db = dsp.voice_gain_floor_db;
    stats.voice_gain_limit = dsp.voice_gain_limit;
    stats.speech_denoiser_erased_frames = dsp.speech_denoiser_erased_frames;
    stats.speech_gate_cut_frames = dsp.speech_gate_cut_frames;
    stats.auto_sensitivity_floor_db = dsp.auto_sensitivity_floor_db;
    stats.open_threshold_db = dsp.open_threshold_db;
    stats.threshold_term = dsp.threshold_term;
    stats.speech_lookahead_ms = dsp.speech_lookahead_applied_ms;
  }
  if (denoiser_) {
    const RootDenoiserSlotStats slot = capture_->denoiser_slot->ReadStats(true);
    const RootDenoiserNotificationCounts notifications =
        root_denoiser_notification_counts();
    stats.denoiser_available = true;
    stats.denoiser_installed = slot.installed;
    stats.denoiser_frames = slot.frames;
    stats.denoiser_misses = slot.misses;
    stats.denoiser_empty_outputs = slot.empty_outputs;
    stats.denoiser_process_max_us = slot.process_max_us;
    stats.denoiser_slow_frames = slot.slow_frames;
    stats.denoiser_bypass_reason = static_cast<uint32_t>(slot.bypass);
    stats.denoiser_notifications = notifications.slow;
    stats.denoiser_other_notifications = notifications.other;
    stats.denoiser_rate = slot.rate;
    stats.denoiser_installs = slot.installs;
    stats.denoiser_bypassed_frames = slot.bypassed_frames;
    stats.denoiser_frames_until_first_output = slot.frames_until_first_output;
    for (uint64_t count : slot.histogram) {
      stats.denoiser_process_us_buckets.push_back(count);
    }
    stats.webrtc_ns_fallback = denoiser_->fallback();
  }
  if (apm_) {
    const RootApmPolicyStats policy = apm_->ReadPolicyStats();
    stats.policy_available = true;
    stats.apm_policy_applies = policy.applies;
    stats.apm_policy_deferred = policy.deferred;
    stats.apm_policy_forced = policy.forced;
    stats.apm_policy_pending = policy.pending;
  }
  if (RootEchoModelController* echo_model =
          apm_ ? apm_->echo_model() : nullptr) {
    const RootEchoModelStatsSnapshot echo = echo_model->ReadStats();
    stats.echo_model_available = true;
    stats.echo_model = static_cast<uint32_t>(echo.mode);
    stats.echo_model_state = static_cast<uint32_t>(echo.state);
    stats.echo_model_reason = static_cast<uint32_t>(echo.reason);
    stats.output_device_kind = static_cast<uint32_t>(echo.kind);
    stats.echo_model_swaps = echo.swaps;
    stats.echo_model_inferences = echo.inferences;
    stats.echo_model_inference_mean_us = echo.inference_mean_us;
    stats.echo_model_inference_max_us = echo.inference_max_us;
    stats.echo_model_inference_p99_us = echo.inference_p99_us;
    stats.echo_model_measured_echo = echo.measured_echo;
    stats.echo_model_kill_switch = echo.kill_switch;
  }
  if (capture_ && capture_->monitor) {
    const RootAudioMonitorStats monitor = capture_->monitor->Read();
    stats.monitor_available = true;
    stats.monitor_active = monitor.active;
    stats.monitor_frames = monitor.frames;
    stats.monitor_dropped_frames = monitor.dropped_frames;
  }
  return stats;
}

bool AudioDeviceController::set_root_audio_control(
    const RootAudioControlUpdate& control) const {
  auto unit = [](float value) {
    return std::isnan(value) ? 0.0f : std::min(1.0f, std::max(0.0f, value));
  };
  const bool capture_fields =
      control.has_mic_muted || control.has_push_to_talk ||
      control.has_input_volume || control.has_denoise_power ||
      control.has_auto_gain || control.has_gate_threshold ||
      control.has_auto_sensitivity || control.has_gain_reset_db ||
      control.has_denoiser_wanted || control.has_speech_detection ||
      control.has_auto_sensitivity_minimum_db ||
      control.has_voice_gain_max_db || control.has_voice_gain_target_db ||
      control.has_voice_gain_rise_db_per_second ||
      control.has_voice_gain_fall_db_per_second ||
      control.has_voice_gain_level_time_constant_ms ||
      control.has_voice_gain_min_speech_probability ||
      control.has_gain_engine || control.has_agc2_headroom_db ||
      control.has_agc2_max_gain_db ||
      control.has_agc2_max_gain_change_db_per_second ||
      control.has_agc2_max_output_noise_dbfs ||
      control.has_speech_lookahead_ms;
  const bool mixer_fields = control.has_master_volume || control.has_deafened;
  const bool apm_fields =
      control.has_echo_cancellation || control.has_noise_suppression;
  RootEchoModelController* echo_model = apm_ ? apm_->echo_model() : nullptr;
  if (control.has_echo_model && echo_model) {
    echo_model->SetMode(RootEchoModelModeFromWire(control.echo_model));
  }
  if (capture_) {
    if (control.has_mic_muted || control.has_push_to_talk) {
      capture_->SetGate(control.has_mic_muted
                            ? std::optional<bool>(control.mic_muted)
                            : std::nullopt,
                        control.has_push_to_talk
                            ? std::optional<uint32_t>(control.push_to_talk)
                            : std::nullopt);
    }
    if (control.has_input_volume) {
      capture_->input_volume.store(RootClampVolume(control.input_volume),
                                   std::memory_order_relaxed);
    }
    if (control.has_denoise_power) {
      capture_->denoise_power.store(unit(control.denoise_power),
                                    std::memory_order_relaxed);
    }
    if (control.has_auto_gain) {
      capture_->auto_gain.store(control.auto_gain, std::memory_order_relaxed);
    }
    if (control.has_gate_threshold) {
      capture_->gate_threshold.store(unit(control.gate_threshold),
                                     std::memory_order_relaxed);
    }
    if (control.has_auto_sensitivity) {
      capture_->auto_sensitivity.store(control.auto_sensitivity,
                                       std::memory_order_relaxed);
    }
    if (control.has_gain_reset_db && std::isfinite(control.gain_reset_db)) {
      const float db =
          std::min(kRootMaxGainResetDb, std::max(0.0f, control.gain_reset_db));
      if (capture_->agc2) {
        capture_->agc2->Reset(db);
      }
      capture_->gain_reset_db.store(db, std::memory_order_relaxed);
      capture_->gain_reset_seq.fetch_add(1, std::memory_order_release);
    }
    if (control.has_gain_engine) {
      capture_->gain_engine.store(
          control.gain_engine == static_cast<uint32_t>(RootGainEngine::kAgc2)
              ? control.gain_engine
              : static_cast<uint32_t>(RootGainEngine::kLegacy),
          std::memory_order_relaxed);
    }
    if (control.has_agc2_headroom_db) {
      capture_->agc2_headroom_db.store(control.agc2_headroom_db,
                                       std::memory_order_relaxed);
    }
    if (control.has_agc2_max_gain_db) {
      capture_->agc2_max_gain_db.store(control.agc2_max_gain_db,
                                       std::memory_order_relaxed);
    }
    if (control.has_agc2_max_gain_change_db_per_second) {
      capture_->agc2_max_gain_change_db_per_second.store(
          control.agc2_max_gain_change_db_per_second,
          std::memory_order_relaxed);
    }
    if (control.has_agc2_max_output_noise_dbfs) {
      capture_->agc2_max_output_noise_dbfs.store(
          control.agc2_max_output_noise_dbfs, std::memory_order_relaxed);
    }
    if (control.has_speech_detection) {
      const bool gate =
          control.speech_detection == 1 ||
          control.speech_detection ==
              static_cast<uint32_t>(RootSpeechDetection::kGate);
      const RootSpeechDetection mode =
          gate ? RootSpeechDetection::kGate : RootSpeechDetection::kOff;
      capture_->speech_detection.store(static_cast<uint32_t>(mode),
                                       std::memory_order_relaxed);
    }
    if (control.has_auto_sensitivity_minimum_db) {
      capture_->auto_sensitivity_minimum_db.store(
          control.auto_sensitivity_minimum_db, std::memory_order_relaxed);
    }
    if (control.has_speech_lookahead_ms) {
      capture_->speech_lookahead_ms.store(control.speech_lookahead_ms,
                                          std::memory_order_relaxed);
    }
    if (control.has_voice_gain_max_db) {
      capture_->voice_gain_max_db.store(
          control.voice_gain_max_db, std::memory_order_relaxed);
    }
    if (control.has_voice_gain_target_db) {
      capture_->voice_gain_target_db.store(
          control.voice_gain_target_db, std::memory_order_relaxed);
    }
    if (control.has_voice_gain_rise_db_per_second) {
      capture_->voice_gain_rise_db_per_second.store(
          control.voice_gain_rise_db_per_second, std::memory_order_relaxed);
    }
    if (control.has_voice_gain_fall_db_per_second) {
      capture_->voice_gain_fall_db_per_second.store(
          control.voice_gain_fall_db_per_second, std::memory_order_relaxed);
    }
    if (control.has_voice_gain_level_time_constant_ms) {
      capture_->voice_gain_level_time_constant_ms.store(
          control.voice_gain_level_time_constant_ms, std::memory_order_relaxed);
    }
    if (control.has_voice_gain_min_speech_probability) {
      capture_->voice_gain_min_speech_probability.store(
          control.voice_gain_min_speech_probability, std::memory_order_relaxed);
    }
    if (control.has_denoiser_wanted && control.denoiser_wanted) {
      capture_->denoiser_wanted.store(true, std::memory_order_relaxed);
      if (denoiser_) {
        denoiser_->Wake();
      }
    }
  }
  if (mixer_) {
    if (control.has_master_volume) {
      mixer_->SetMasterVolume(control.master_volume);
    }
    if (control.has_deafened) {
      mixer_->SetDeafened(control.deafened);
    }
  }
  if (apm_ && apm_fields) {
    apm_->SetPolicy(control.has_echo_cancellation
                        ? std::optional<bool>(control.echo_cancellation)
                        : std::nullopt,
                    control.has_noise_suppression
                        ? std::optional<bool>(control.noise_suppression)
                        : std::nullopt);
  }
  // A falling denoiser_wanted lands after the policy sent with it: the
  // effects thread may drop the fallback at once, and NS then has to be
  // what the host now asks for, not its previous value.
  if (capture_ && control.has_denoiser_wanted && !control.denoiser_wanted) {
    capture_->denoiser_wanted.store(false, std::memory_order_relaxed);
  }
  if (capture_ && capture_fields) {
    capture_->ServiceAgc2();
  }
  return (capture_ || !capture_fields) && (mixer_ || !mixer_fields) &&
         (apm_ || !apm_fields) && (echo_model || !control.has_echo_model);
}

bool AudioDeviceController::set_root_audio_source_gain(uint32_t ssrc,
                                                       float tile_volume,
                                                       bool paused) const {
  return mixer_ && mixer_->SetSourceGain(ssrc, tile_volume, paused);
}

void AudioDeviceController::clear_root_audio_source_gain(uint32_t ssrc) const {
  if (mixer_) {
    mixer_->ClearSourceGain(ssrc);
  }
}

bool AudioDeviceController::register_root_audio_effect(
    uint32_t id,
    rust::Slice<const int16_t> samples) const {
  return mixer_ &&
         mixer_->RegisterEffectClip(id, samples.data(), samples.size());
}

uint8_t AudioDeviceController::play_root_audio_effect(uint32_t id,
                                                      float gain) const {
  if (!mixer_) {
    return static_cast<uint8_t>(RootEffectPlay::kIdle);
  }
  return static_cast<uint8_t>(mixer_->PlayEffect(id, gain));
}

void AudioDeviceController::stop_root_audio_effects() const {
  if (mixer_) {
    mixer_->StopEffects();
  }
}

RootAudioCaptureFormat AudioDeviceController::root_audio_capture_format()
    const {
  RootAudioCaptureFormat format{};
  if (capture_) {
    const RootCaptureFormatSnapshot snapshot = ReadCaptureFormat(*capture_);
    format.seq = snapshot.seq;
    format.sample_rate = snapshot.sample_rate;
    format.channels = snapshot.channels;
  }
  return format;
}

uint64_t AudioDeviceController::root_audio_sources_added() const {
  return mixer_ ? mixer_->sources_added() : 0;
}

// The gate first, in one store, so closing it never passes through open.
void AudioDeviceController::reset_root_audio(bool close_gate) const {
  if (capture_) {
    capture_->ResetControls(close_gate);
    if (capture_->agc2) {
      capture_->agc2->ResetSession();
    }
    capture_->ServiceAgc2();
  }
  if (close_gate) {
    adm_proxy_->SetMonitor(false);
  }
  if (mixer_) {
    mixer_->ResetControls();
  }
  if (apm_) {
    apm_->ResetPolicy();
    if (RootEchoModelController* echo_model = apm_->echo_model()) {
      echo_model->ResetSession(close_gate ? RootEchoModelMode::kOff
                                          : RootEchoModelMode::kAuto);
    }
  }
  if (denoiser_ && close_gate) {
    denoiser_->Reset();
  }
}

uint8_t AudioDeviceController::set_root_audio_monitor(bool enabled) const {
  return static_cast<uint8_t>(adm_proxy_->SetMonitor(enabled));
}

bool AudioDeviceController::root_denoiser_available() const {
  return denoiser_ != nullptr;
}

bool AudioDeviceController::set_root_denoiser(uint64_t owner,
                                              uint64_t async_id,
                                              uint64_t pipeline,
                                              uint32_t sample_rate) const {
  if (!denoiser_) {
    return false;
  }
  denoiser_->Set(owner, async_id, pipeline, sample_rate);
  return true;
}

RootReturnPipelinesResult
AudioDeviceController::return_root_denoiser_pipelines() const {
  RootReturnPipelinesResult result{};
  if (!denoiser_) {
    return result;
  }
  const RootDenoiserReturnAll all = denoiser_->ReturnAll();
  for (uint64_t pipeline : all.pipelines) {
    result.pipelines.push_back(pipeline);
  }
  result.error = rust::String(all.error);
  return result;
}

rust::Vec<RootDenoiserCompletionItem>
AudioDeviceController::take_root_denoiser_completions() const {
  rust::Vec<RootDenoiserCompletionItem> result;
  if (!denoiser_) {
    return result;
  }
  for (const RootDenoiserCompletion& completion :
       denoiser_->TakeCompletions()) {
    RootDenoiserCompletionItem item{};
    item.owner = completion.owner;
    item.async_id = completion.async_id;
    item.error = rust::String(completion.error);
    item.pipeline = completion.pipeline;
    result.push_back(std::move(item));
  }
  return result;
}

rust::Vec<RootDenoiserReturnedItem>
AudioDeviceController::take_root_denoiser_returned() const {
  rust::Vec<RootDenoiserReturnedItem> result;
  if (!denoiser_) {
    return result;
  }
  for (const RootDenoiserReturned& returned : denoiser_->TakeReturned()) {
    result.push_back(
        RootDenoiserReturnedItem{returned.owner, returned.pipeline});
  }
  return result;
}

RootDenoiserStateInfo AudioDeviceController::root_denoiser_state() const {
  RootDenoiserStateInfo state{};
  if (!denoiser_) {
    return state;
  }
  const RootDenoiserSlot& slot = *capture_->denoiser_slot;
  state.installed = slot.installed();
  state.rate = slot.rate();
  state.bypass_reason = static_cast<uint32_t>(slot.bypass());
  state.webrtc_ns_fallback = denoiser_->fallback();
  return state;
}

RootSpeakingInfo AudioDeviceController::root_audio_speaking() const {
  RootSpeakingInfo info{};
  if (capture_) {
    // Polled every 10 ms while a session lives: frames that stopped without
    // a stop never leave the ring lit, and AGC2 follows the processor's rate.
    capture_->ClearStaleSpeaking();
    capture_->ServiceAgc2();
    const RootSpeakingSnapshot speaking = capture_->ReadSpeaking();
    info.speaking = speaking.speaking;
    info.seq = speaking.seq;
  }
  return info;
}

#if LK_ADM_MOBILE

bool AudioDeviceController::stop_recording() const {
  return adm_proxy_->StopRecording() == 0;
}

bool AudioDeviceController::start_recording() const {
  return adm_proxy_->StartRecording() == 0;
}

bool AudioDeviceController::stop_playout() const {
  return adm_proxy_->StopPlayout() == 0;
}

bool AudioDeviceController::start_playout() const {
  return adm_proxy_->StartPlayout() == 0;
}

#else

// The app's requests, kept apart from libwebrtc's own Start/Stop: an app stop
// never closes a device libwebrtc uses, and an app start never makes libwebrtc
// believe it started playout or recording itself.
bool AudioDeviceController::stop_recording() const {
  adm_proxy_->SetAppRecording(false);
  return true;
}

bool AudioDeviceController::start_recording() const {
  adm_proxy_->SetAppRecording(true);
  return true;
}

bool AudioDeviceController::stop_playout() const {
  adm_proxy_->SetAppPlayout(false);
  return true;
}

bool AudioDeviceController::start_playout() const {
  adm_proxy_->SetAppPlayout(true);
  return true;
}

#endif

bool AudioDeviceController::init_recording() const {
  return adm_proxy_->InitRecording() == 0;
}

bool AudioDeviceController::recording_is_initialized() const {
  return adm_proxy_->RecordingIsInitialized();
}

bool AudioDeviceController::init_playout() const {
  return adm_proxy_->InitPlayout() == 0;
}

bool AudioDeviceController::playout_is_initialized() const {
  return adm_proxy_->PlayoutIsInitialized();
}

bool AudioDeviceController::builtin_aec_is_available() const {
  return adm_proxy_->BuiltInAECIsAvailable();
}

bool AudioDeviceController::builtin_agc_is_available() const {
  return adm_proxy_->BuiltInAGCIsAvailable();
}

bool AudioDeviceController::builtin_ns_is_available() const {
  return adm_proxy_->BuiltInNSIsAvailable();
}

bool AudioDeviceController::enable_builtin_aec(bool enable) const {
  return adm_proxy_->EnableBuiltInAEC(enable) == 0;
}

bool AudioDeviceController::enable_builtin_agc(bool enable) const {
  return adm_proxy_->EnableBuiltInAGC(enable) == 0;
}

bool AudioDeviceController::enable_builtin_ns(bool enable) const {
  return adm_proxy_->EnableBuiltInNS(enable) == 0;
}

void AudioDeviceController::set_adm_recording_enabled(bool enabled) const {
  adm_proxy_->set_recording_enabled(enabled);
}

bool AudioDeviceController::adm_recording_enabled() const {
  return adm_proxy_->recording_enabled();
}

void AudioDeviceController::set_adm_playout_enabled(bool enabled) const {
  adm_proxy_->set_playout_enabled(enabled);
}

bool AudioDeviceController::adm_playout_enabled() const {
  return adm_proxy_->playout_enabled();
}

// The engine reference follows each platform reference, so libwebrtc's
// transport is registered (or about to be) whenever a device may open for the
// hold, the app or the monitor; until then the transport tap plays silence.
bool AudioDeviceController::acquire_platform_adm() const {
  if (!adm_proxy_->AcquirePlatformAdm()) {
    return false;
  }
  if (engine_hold_) {
    engine_hold_->Acquire();
  }
  return true;
}

void AudioDeviceController::release_platform_adm() const {
  if (adm_proxy_->ReleasePlatformAdm() && engine_hold_) {
    engine_hold_->Release();
  }
}

int AudioDeviceController::platform_adm_ref_count() const {
  return adm_proxy_->platform_adm_ref_count();
}

bool AudioDeviceController::is_platform_adm_active() const {
  return adm_proxy_->is_platform_adm_active();
}

#if defined(ROOTAPP_HARNESS)
int32_t resolve_audio_device_index_for_test(
    const AudioDeviceSelection& selection,
    rust::Slice<const rust::String> names,
    rust::Slice<const rust::String> guids) {
  const int count = static_cast<int>(std::min(names.size(), guids.size()));
  return ResolveAdmDeviceIndex(
      ToAdmSelection(selection), count,
      [&names, &guids](int index, std::string* name, std::string* guid) {
        *name = std::string(names[index]);
        *guid = std::string(guids[index]);
        return true;
      });
}
#endif

}  // namespace livekit_ffi
