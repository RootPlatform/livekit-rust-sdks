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

#pragma once

#include <memory>

#include "api/scoped_refptr.h"
#include "livekit/adm_proxy.h"
#include "rust/cxx.h"

namespace livekit_ffi {
class AudioDeviceController;
class AdmProxyHarness;
struct AudioDeviceSelection;
struct AudioDeviceRequestOutcome;
struct RootAudioNativeStats;
}  // namespace livekit_ffi
#include "webrtc-sys/src/audio_device_controller.rs.h"

namespace livekit_ffi {

AdmDeviceSelection ToAdmSelection(const AudioDeviceSelection& selection);
AudioDeviceRequestOutcome ToRequestOutcome(const AdmRequestOutcome& outcome);

class AudioDeviceController {
 public:
  explicit AudioDeviceController(webrtc::scoped_refptr<AdmProxy> adm_proxy);

  // Device enumeration
  int16_t playout_devices() const;
  int16_t recording_devices() const;
  rust::String playout_device_name(uint16_t index) const;
  rust::String recording_device_name(uint16_t index) const;
  rust::String playout_device_guid(uint16_t index) const;
  rust::String recording_device_guid(uint16_t index) const;

  // Device selection
  bool set_playout_device(uint16_t index) const;
  bool set_recording_device(uint16_t index) const;
  bool set_playout_device_by_guid(rust::String guid) const;
  bool set_recording_device_by_guid(rust::String guid) const;

  // Selection and restart applied on the device thread; `callback` runs there
  // once done.
  void select_devices(const AudioDeviceSelection& recording,
                      bool set_recording,
                      const AudioDeviceSelection& playout,
                      bool set_playout,
                      rust::Box<AudioDeviceRequestCallback> callback) const;
  void restart_devices(bool recording,
                       bool playout,
                       rust::Box<AudioDeviceRequestCallback> callback) const;
  void set_playout_hold(bool hold) const;
  RootAudioNativeStats root_audio_stats() const;

  // Recording control. On desktop start/stop are the app's own request, apart
  // from libwebrtc's; init does nothing (devices open on start).
  bool stop_recording() const;
  bool init_recording() const;
  bool start_recording() const;
  bool recording_is_initialized() const;

  // Playout control, as recording.
  bool stop_playout() const;
  bool init_playout() const;
  bool start_playout() const;
  bool playout_is_initialized() const;

  // Built-in audio processing
  bool builtin_aec_is_available() const;
  bool builtin_agc_is_available() const;
  bool builtin_ns_is_available() const;
  bool enable_builtin_aec(bool enable) const;
  bool enable_builtin_agc(bool enable) const;
  bool enable_builtin_ns(bool enable) const;

  // ADM recording control
  void set_adm_recording_enabled(bool enabled) const;
  bool adm_recording_enabled() const;

  // ADM playout control
  void set_adm_playout_enabled(bool enabled) const;
  bool adm_playout_enabled() const;

  // Platform ADM lifecycle management
  bool acquire_platform_adm() const;
  void release_platform_adm() const;
  int platform_adm_ref_count() const;
  bool is_platform_adm_active() const;

 private:
  webrtc::scoped_refptr<AdmProxy> adm_proxy_;
};

int32_t resolve_audio_device_index_for_test(
    const AudioDeviceSelection& selection,
    rust::Slice<const rust::String> names,
    rust::Slice<const rust::String> guids);

class FakePlatformAdm;
class HarnessTransport;

// Drives an AdmProxy whose platform ADM is a fake that records the calls it
// gets, so the proxy's state machine is tested without any audio device.
// "engine_*" calls are what libwebrtc calls; app_* go through the controller.
class AdmProxyHarness {
 public:
  AdmProxyHarness(rust::Slice<const rust::String> playout_guids,
                  rust::Slice<const rust::String> recording_guids);
  ~AdmProxyHarness();

  // Calls the fake got since the last take, e.g. "StartPlayout".
  rust::Vec<rust::String> take_calls() const;
  void set_devices(rust::Slice<const rust::String> playout_guids,
                   rust::Slice<const rust::String> recording_guids) const;
  // The next StopPlayout fails with `result` and leaves the device closed, as
  // the Windows ADM does when it gives up joining its render thread.
  void fail_next_stop_playout(int32_t result) const;
  void set_device_playout_delay(uint16_t delay_ms) const;

  void acquire() const;
  void release() const;
  void enable(bool recording, bool playout) const;
  void hold(bool hold) const;
  bool held() const;
  void app_playout(bool start) const;
  void app_recording(bool start) const;
  void engine_start_playout() const;
  void engine_stop_playout() const;
  void engine_start_recording() const;
  void engine_stop_recording() const;
  int32_t engine_init_playout() const;
  int32_t engine_init_recording() const;
  void register_transport() const;
  void terminate() const;
  AudioDeviceRequestOutcome select(const AudioDeviceSelection& recording,
                                   bool set_recording,
                                   const AudioDeviceSelection& playout,
                                   bool set_playout) const;
  AudioDeviceRequestOutcome restart(bool recording, bool playout) const;
  void flush() const;

  // libwebrtc's view of the proxy.
  bool playing() const;
  bool recording() const;
  bool playout_initialized() const;
  bool recording_initialized() const;
  uint16_t playout_delay() const;
  bool has_device_stats() const;
  bool stop_on_mute() const;
  bool builtin_effects_available() const;
  int32_t set_microphone_mute(bool mute) const;
  // 0 AEC, 1 AGC, 2 NS.
  int32_t enable_builtin_effect(uint8_t effect, bool enable) const;

  // The devices behind it.
  bool platform_playing() const;
  bool platform_recording() const;
  bool synthetic_playing() const;

  // Runs one device callback on a thread of its own, as the ADM would; with
  // `reenter`, libwebrtc's side starts a second one before the first returns.
  // Returns how often libwebrtc's transport was entered.
  uint32_t device_callback(bool capture, bool reenter) const;
  // Takes an AdmProxy lock on a thread marked as an audio thread and returns
  // how many violations that added.
  uint64_t lock_on_audio_thread() const;

  RootAudioNativeStats stats() const;

 private:
  webrtc::scoped_refptr<FakePlatformAdm> fake_;
  webrtc::scoped_refptr<AdmProxy> proxy_;
  std::unique_ptr<AudioDeviceController> controller_;
  std::unique_ptr<HarnessTransport> transport_;
};

std::unique_ptr<AdmProxyHarness> new_adm_proxy_harness(
    rust::Slice<const rust::String> playout_guids,
    rust::Slice<const rust::String> recording_guids);

}  // namespace livekit_ffi
