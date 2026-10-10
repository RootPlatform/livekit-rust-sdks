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
#include <vector>

#include "api/scoped_refptr.h"
#include "livekit/adm_proxy.h"
#include "livekit/root_audio_mixer.h"
#include "livekit/root_audio_processing.h"
#include "livekit/root_denoiser.h"
#include "rust/cxx.h"

namespace webrtc {
class Event;
class Thread;
}  // namespace webrtc

namespace livekit_ffi {
class AudioDeviceController;
class AdmProxyHarness;
class RootVoiceHarness;
struct RootVoiceFakeCounts;
struct AudioDeviceSelection;
struct AudioDeviceRequestOutcome;
struct RootAudioNativeStats;
struct RootAudioControlUpdate;
struct RootAudioCaptureFormat;
struct RootControlState;
struct RootDenoiserCompletionItem;
struct RootDenoiserReturnedItem;
struct RootReturnPipelinesResult;
struct RootDenoiserStateInfo;
struct RootSpeakingInfo;
}  // namespace livekit_ffi
#include "webrtc-sys/src/audio_device_controller.rs.h"

namespace livekit_ffi {

AdmDeviceSelection ToAdmSelection(const AudioDeviceSelection& selection);
AudioDeviceRequestOutcome ToRequestOutcome(const AdmRequestOutcome& outcome);

class AudioDeviceController {
 public:
  explicit AudioDeviceController(webrtc::scoped_refptr<AdmProxy> adm_proxy);
  // The native audio engine's pieces; any of them may be null (mobile, tests).
  AudioDeviceController(webrtc::scoped_refptr<AdmProxy> adm_proxy,
                        webrtc::scoped_refptr<RootAudioMixer> mixer,
                        webrtc::scoped_refptr<RootApmDecorator> apm,
                        std::shared_ptr<RootCaptureState> capture);

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

  // Live control of the native audio engine; values arrive sanitised and are
  // clamped again here. Never blocks on an audio thread or the APM. False when
  // a set field has nothing to act on in this build (no capture processor,
  // mixer or APM decorator); the other fields are applied.
  bool set_root_audio_control(const RootAudioControlUpdate& control) const;
  bool set_root_audio_source_gain(uint32_t ssrc,
                                  float tile_volume,
                                  bool paused) const;
  void clear_root_audio_source_gain(uint32_t ssrc) const;
  bool register_root_audio_effect(uint32_t id,
                                  rust::Slice<const int16_t> samples) const;
  // 0 queued, 1 unknown clip, 2 dropped, 3 idle.
  uint8_t play_root_audio_effect(uint32_t id, float gain) const;
  void stop_root_audio_effects() const;
  RootAudioCaptureFormat root_audio_capture_format() const;
  // Moves whenever the playout mixer gets a new source.
  uint64_t root_audio_sources_added() const;
  // Controls back to their defaults (unity volumes, no gains or effects,
  // AEC/NS following the device track), with the capture gate open or closed
  // (muted, push-to-talk key up). Closing it (the last session ended) also
  // hands back every denoiser pipeline and ends the NS fallback.
  void reset_root_audio(bool close_gate) const;

  // The Effects SDK hand-off (RootDenoiserControl); every call is a no-op
  // without a slot.
  bool root_denoiser_available() const;
  bool set_root_denoiser(uint64_t owner,
                         uint64_t async_id,
                         uint64_t pipeline,
                         uint32_t sample_rate) const;
  RootReturnPipelinesResult return_root_denoiser_pipelines() const;
  rust::Vec<RootDenoiserCompletionItem> take_root_denoiser_completions() const;
  rust::Vec<RootDenoiserReturnedItem> take_root_denoiser_returned() const;
  RootDenoiserStateInfo root_denoiser_state() const;
  RootSpeakingInfo root_audio_speaking() const;

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
  webrtc::scoped_refptr<RootAudioMixer> mixer_;
  webrtc::scoped_refptr<RootApmDecorator> apm_;
  std::shared_ptr<RootCaptureState> capture_;
  // With the capture state's slot: the rootapp-effects thread.
  std::unique_ptr<RootDenoiserControl> denoiser_;
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
  // With `with_apm`, the controller also gets an APM decorator over a fake APM
  // (NewRootApmOverFakeForTest).
  AdmProxyHarness(rust::Slice<const rust::String> playout_guids,
                  rust::Slice<const rust::String> recording_guids,
                  bool with_apm);
  ~AdmProxyHarness();

  // Calls the fake got since the last take, e.g. "StartPlayout".
  rust::Vec<rust::String> take_calls() const;
  void set_devices(rust::Slice<const rust::String> playout_guids,
                   rust::Slice<const rust::String> recording_guids) const;
  // The next StopPlayout fails with `result` and leaves the device closed, as
  // the Windows ADM does when it gives up joining its render thread.
  void fail_next_stop_playout(int32_t result) const;
  // The same for StopRecording.
  void fail_next_stop_recording(int32_t result) const;
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

  // The engine controls behind the controller.
  bool mixer_platform_active() const;
  bool apply_control(const RootAudioControlUpdate& control) const;
  void reset_root_audio(bool close_gate) const;
  RootControlState control_state() const;
  const AudioDeviceController& controller() const;
  uint64_t add_fake_pipeline() const;
  uint8_t run_denoiser(uint32_t rate) const;
  void latch_denoiser_bypass(uint32_t reason) const;
  // Stores the speaking flag as the capture processor publishes it.
  void set_speaking(bool speaking) const;

 private:
  webrtc::scoped_refptr<FakePlatformAdm> fake_;
  webrtc::scoped_refptr<RootAudioMixer> mixer_;
  std::shared_ptr<RootCaptureState> capture_;
  webrtc::scoped_refptr<RootApmDecorator> apm_;
  webrtc::scoped_refptr<AdmProxy> proxy_;
  std::unique_ptr<std::vector<std::unique_ptr<RootFakeFxPipeline>>> fakes_;
  std::unique_ptr<AudioDeviceController> controller_;
  std::unique_ptr<HarnessTransport> transport_;
};

std::unique_ptr<AdmProxyHarness> new_adm_proxy_harness(
    rust::Slice<const rust::String> playout_guids,
    rust::Slice<const rust::String> recording_guids);
std::unique_ptr<AdmProxyHarness> new_adm_proxy_harness_with_apm(
    rust::Slice<const rust::String> playout_guids,
    rust::Slice<const rust::String> recording_guids);

class HarnessApmTransport;

// The native voice path end to end over one RootCaptureState, wired as the
// factory wires it: AdmProxy over a fake platform ADM, libwebrtc's builtin APM
// with RootCaptureProcessor behind RootApmDecorator (policy changes on a
// worker thread), the denoiser slot with fake Effects SDK pipelines, and the
// controller with its rootapp-effects thread. A recorded frame goes device ->
// AdmProxy -> the APM on a device thread, as the voice engine runs it. Tests
// only; no audio device is opened.
class RootVoiceHarness {
 public:
  RootVoiceHarness();
  ~RootVoiceHarness();

  const AudioDeviceController& controller() const;
  // The app starts or stops recording (a start bumps the stream epoch).
  void recording(bool start) const;
  uint64_t stream_epoch() const;
  // One 10 ms frame of interleaved int16 channels from the device; returns
  // what the APM sends.
  rust::Vec<int16_t> capture(int32_t rate,
                             size_t channels,
                             rust::Slice<const int16_t> input) const;
  // A fake Effects SDK pipeline owned by the harness: output round(scale x
  // input) delayed by delay_samples.
  uint64_t add_fake_pipeline(float scale, uint32_t delay_samples) const;
  // Every output of that pipeline is empty from now on (or again not).
  void set_fake_empty(uint64_t pipeline, bool empty) const;
  RootVoiceFakeCounts fake_counts(uint64_t pipeline) const;
  // Holds the worker thread, so policy applications wait, until released.
  void hold_worker(bool hold) const;
  // What the decorator applied, as the capture processor reads it.
  bool webrtc_ns_applied() const;

 private:
  RootFakeFxPipeline* Fake(uint64_t pipeline) const;

  std::unique_ptr<webrtc::Thread> worker_;
  webrtc::scoped_refptr<FakePlatformAdm> fake_;
  webrtc::scoped_refptr<RootAudioMixer> mixer_;
  std::shared_ptr<RootCaptureState> capture_;
  webrtc::scoped_refptr<RootApmDecorator> apm_;
  std::unique_ptr<HarnessApmTransport> transport_;
  webrtc::scoped_refptr<AdmProxy> proxy_;
  std::unique_ptr<AudioDeviceController> controller_;
  mutable std::vector<std::unique_ptr<RootFakeFxPipeline>> fakes_;
  std::shared_ptr<webrtc::Event> worker_hold_;
};

std::unique_ptr<RootVoiceHarness> new_root_voice_harness();

}  // namespace livekit_ffi
