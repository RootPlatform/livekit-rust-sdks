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
  stats.echo_cancellation_active = native.echo_cancellation;
  stats.webrtc_ns_active = native.noise_suppression;
  stats.audio_thread_lock_violations = native.audio_thread_lock_violations;
  stats.audio_thread_logs_dropped = native.audio_thread_logs_dropped;
  stats.overlapping_callbacks = native.overlapping_callbacks;
  stats.device_stop_failures = native.device_stop_failures;
  return stats;
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
