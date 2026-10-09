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
    default:
      result.kind = AdmSelectionKind::kDefault;
      break;
  }
  result.id = std::string(selection.id);
  result.index = selection.index;
  return result;
}

AudioDeviceController::AudioDeviceController(
    webrtc::scoped_refptr<AdmProxy> adm_proxy)
    : adm_proxy_(std::move(adm_proxy)) {}

AudioDeviceController::AudioDeviceController(
    webrtc::scoped_refptr<AdmProxy> adm_proxy,
    webrtc::scoped_refptr<RootAudioMixer> mixer,
    webrtc::scoped_refptr<RootApmDecorator> apm,
    std::shared_ptr<RootCaptureState> capture)
    : adm_proxy_(std::move(adm_proxy)),
      mixer_(std::move(mixer)),
      apm_(std::move(apm)),
      capture_(std::move(capture)) {}

int16_t AudioDeviceController::playout_devices() const {
  return adm_proxy_->PlayoutDevices();
}

int16_t AudioDeviceController::recording_devices() const {
  return adm_proxy_->RecordingDevices();
}

rust::String AudioDeviceController::playout_device_name(uint16_t index) const {
  char name[webrtc::kAdmMaxDeviceNameSize] = {0};
  char guid[webrtc::kAdmMaxGuidSize] = {0};
  adm_proxy_->PlayoutDeviceName(index, name, guid);
  return rust::String(name);
}

rust::String AudioDeviceController::recording_device_name(uint16_t index) const {
  char name[webrtc::kAdmMaxDeviceNameSize] = {0};
  char guid[webrtc::kAdmMaxGuidSize] = {0};
  adm_proxy_->RecordingDeviceName(index, name, guid);
  return rust::String(name);
}

rust::String AudioDeviceController::playout_device_guid(uint16_t index) const {
  char name[webrtc::kAdmMaxDeviceNameSize] = {0};
  char guid[webrtc::kAdmMaxGuidSize] = {0};
  adm_proxy_->PlayoutDeviceName(index, name, guid);
  return rust::String(guid);
}

rust::String AudioDeviceController::recording_device_guid(uint16_t index) const {
  char name[webrtc::kAdmMaxDeviceNameSize] = {0};
  char guid[webrtc::kAdmMaxGuidSize] = {0};
  adm_proxy_->RecordingDeviceName(index, name, guid);
  return rust::String(guid);
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

  // Try to find a device matching the GUID
  for (int16_t i = 0; i < count; i++) {
    char name[webrtc::kAdmMaxDeviceNameSize] = {0};
    char device_guid[webrtc::kAdmMaxGuidSize] = {0};
    if (adm_proxy_->PlayoutDeviceName(i, name, device_guid) == 0) {
      if (std::string(guid.c_str()) == std::string(device_guid)) {
        return adm_proxy_->SetPlayoutDevice(i) == 0;
      }
    }
  }

  // No match found - fall back to default device (index 0).
  // This handles mobile platforms (iOS/Android) where:
  // - GUIDs may be empty or not meaningful
  // - Device selection is a no-op (system handles routing)
  if (count > 0) {
    return adm_proxy_->SetPlayoutDevice(0) == 0;
  }
  return false;
}

bool AudioDeviceController::set_recording_device_by_guid(rust::String guid) const {
  int16_t count = adm_proxy_->RecordingDevices();

  // Try to find a device matching the GUID
  for (int16_t i = 0; i < count; i++) {
    char name[webrtc::kAdmMaxDeviceNameSize] = {0};
    char device_guid[webrtc::kAdmMaxGuidSize] = {0};
    if (adm_proxy_->RecordingDeviceName(i, name, device_guid) == 0) {
      if (std::string(guid.c_str()) == std::string(device_guid)) {
        return adm_proxy_->SetRecordingDevice(i) == 0;
      }
    }
  }

  // No match found - fall back to default device (index 0).
  // This handles mobile platforms (iOS/Android) where:
  // - GUIDs may be empty or not meaningful
  // - Device selection is a no-op (system handles routing)
  if (count > 0) {
    return adm_proxy_->SetRecordingDevice(0) == 0;
  }
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
  stats.echo_cancellation_active = native.echo_cancellation;
  stats.webrtc_ns_active = native.noise_suppression;
  stats.audio_thread_lock_violations = native.audio_thread_lock_violations;
  stats.audio_thread_logs_dropped = native.audio_thread_logs_dropped;
  stats.overlapping_callbacks = native.overlapping_callbacks;
  stats.device_stop_failures = native.device_stop_failures;

  const double nan = std::numeric_limits<double>::quiet_NaN();
  stats.erl_db = nan;
  stats.erle_db = nan;
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
  if (capture_) {
    const RootCaptureFormatSnapshot format = ReadCaptureFormat(*capture_);
    stats.process_rate = format.sample_rate;
    stats.process_channels = format.channels;
    stats.processed_frames =
        capture_->processed_frames.load(std::memory_order_relaxed);
    stats.apm_out_sumsq = capture_->apm_out_sumsq.load(std::memory_order_relaxed);
    stats.sent_sumsq = capture_->sent_sumsq.load(std::memory_order_relaxed);
    stats.stage_samples =
        capture_->stage_samples.load(std::memory_order_relaxed);
  }
  return stats;
}

void AudioDeviceController::set_root_audio_control(
    const RootAudioControlUpdate& control) const {
  auto unit = [](float value) {
    return std::isnan(value) ? 0.0f : std::min(1.0f, std::max(0.0f, value));
  };
  if (capture_) {
    if (control.has_mic_muted) {
      capture_->mic_muted.store(control.mic_muted, std::memory_order_relaxed);
    }
    if (control.has_push_to_talk) {
      capture_->push_to_talk.store(
          std::min<uint32_t>(control.push_to_talk,
                             static_cast<uint32_t>(RootPushToTalk::kReleased)),
          std::memory_order_relaxed);
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
      capture_->gain_reset_db.store(
          std::min(kRootMaxVoiceGainDb, std::max(0.0f, control.gain_reset_db)),
          std::memory_order_relaxed);
      capture_->gain_reset_seq.fetch_add(1, std::memory_order_release);
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
  if (apm_ && (control.has_echo_cancellation || control.has_noise_suppression)) {
    apm_->SetPolicy(control.has_echo_cancellation
                        ? std::optional<bool>(control.echo_cancellation)
                        : std::nullopt,
                    control.has_noise_suppression
                        ? std::optional<bool>(control.noise_suppression)
                        : std::nullopt);
  }
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

void AudioDeviceController::reset_root_audio() const {
  if (capture_) {
    capture_->ResetControls();
  }
  if (mixer_) {
    mixer_->ResetControls();
  }
  if (apm_) {
    apm_->ResetPolicy();
  }
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

bool AudioDeviceController::acquire_platform_adm() const {
  return adm_proxy_->AcquirePlatformAdm();
}

void AudioDeviceController::release_platform_adm() const {
  adm_proxy_->ReleasePlatformAdm();
}

int AudioDeviceController::platform_adm_ref_count() const {
  return adm_proxy_->platform_adm_ref_count();
}

bool AudioDeviceController::is_platform_adm_active() const {
  return adm_proxy_->is_platform_adm_active();
}

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

}  // namespace livekit_ffi
