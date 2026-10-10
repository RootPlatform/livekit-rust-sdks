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

#include <array>
#include <atomic>
#include <cstring>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include "api/environment/environment_factory.h"
#include "api/make_ref_counted.h"
#include "livekit/adm_proxy.h"
#include "livekit/audio_device_controller.h"
#include "livekit/audio_thread.h"
#include "livekit/root_audio_mixer.h"
#include "modules/audio_device/include/audio_device_default.h"
#include "rtc_base/event.h"

namespace livekit_ffi {
namespace {

std::vector<std::string> ToStrings(rust::Slice<const rust::String> values) {
  std::vector<std::string> result;
  for (const rust::String& value : values) {
    result.emplace_back(value);
  }
  return result;
}

const char* OnOff(bool value) {
  return value ? "on" : "off";
}

void CopyDevice(const std::vector<std::string>& guids,
                uint16_t index,
                char name[webrtc::kAdmMaxDeviceNameSize],
                char guid[webrtc::kAdmMaxGuidSize]) {
  const std::string device_name = "Device " + guids[index];
  std::strncpy(name, device_name.c_str(), webrtc::kAdmMaxDeviceNameSize - 1);
  std::strncpy(guid, guids[index].c_str(), webrtc::kAdmMaxGuidSize - 1);
}

}  // namespace

// Records the state-changing calls it gets and behaves like a platform ADM:
// Start needs Init, Stop closes the device.
class FakePlatformAdm
    : public webrtc::webrtc_impl::AudioDeviceModuleDefault<
          webrtc::AudioDeviceModule> {
 public:
  std::vector<std::string> TakeCalls() {
    std::lock_guard<std::mutex> lock(mutex_);
    return std::exchange(calls_, {});
  }

  void SetDevices(std::vector<std::string> playout,
                  std::vector<std::string> recording) {
    std::lock_guard<std::mutex> lock(mutex_);
    playout_guids_ = std::move(playout);
    recording_guids_ = std::move(recording);
  }

  void FailNextStopPlayout(int32_t result) {
    next_stop_playout_result_.store(result);
  }
  void SetPlayoutDelay(uint16_t delay_ms) { playout_delay_ms_.store(delay_ms); }
  webrtc::AudioTransport* callback() const { return callback_.load(); }

  int32_t Init() override { return Record("Init"); }
  int32_t Terminate() override { return Record("Terminate"); }
  int32_t RegisterAudioCallback(webrtc::AudioTransport* transport) override {
    callback_.store(transport);
    return Record(transport ? "RegisterAudioCallback(set)"
                            : "RegisterAudioCallback(null)");
  }

  int16_t PlayoutDevices() override {
    std::lock_guard<std::mutex> lock(mutex_);
    return static_cast<int16_t>(playout_guids_.size());
  }
  int16_t RecordingDevices() override {
    std::lock_guard<std::mutex> lock(mutex_);
    return static_cast<int16_t>(recording_guids_.size());
  }
  int32_t PlayoutDeviceName(uint16_t index,
                            char name[webrtc::kAdmMaxDeviceNameSize],
                            char guid[webrtc::kAdmMaxGuidSize]) override {
    std::lock_guard<std::mutex> lock(mutex_);
    if (index >= playout_guids_.size()) {
      return -1;
    }
    CopyDevice(playout_guids_, index, name, guid);
    return 0;
  }
  int32_t RecordingDeviceName(uint16_t index,
                              char name[webrtc::kAdmMaxDeviceNameSize],
                              char guid[webrtc::kAdmMaxGuidSize]) override {
    std::lock_guard<std::mutex> lock(mutex_);
    if (index >= recording_guids_.size()) {
      return -1;
    }
    CopyDevice(recording_guids_, index, name, guid);
    return 0;
  }

  int32_t SetPlayoutDevice(uint16_t index) override {
    return Record("SetPlayoutDevice(" + std::to_string(index) + ")");
  }
  int32_t SetPlayoutDevice(WindowsDeviceType) override {
    return Record("SetPlayoutDevice(default)");
  }
  int32_t SetRecordingDevice(uint16_t index) override {
    return Record("SetRecordingDevice(" + std::to_string(index) + ")");
  }
  int32_t SetRecordingDevice(WindowsDeviceType) override {
    return Record("SetRecordingDevice(default)");
  }

  int32_t InitPlayout() override {
    playout_initialized_.store(true);
    return Record("InitPlayout");
  }
  bool PlayoutIsInitialized() const override {
    return playout_initialized_.load();
  }
  int32_t StartPlayout() override {
    Record("StartPlayout");
    if (!playout_initialized_.load()) {
      return -1;
    }
    playing_.store(true);
    return 0;
  }
  int32_t StopPlayout() override {
    Record("StopPlayout");
    const bool open = playout_initialized_.exchange(false);
    playing_.store(false);
    return open ? next_stop_playout_result_.exchange(0) : 0;
  }
  bool Playing() const override { return playing_.load(); }

  int32_t InitRecording() override {
    recording_initialized_.store(true);
    return Record("InitRecording");
  }
  bool RecordingIsInitialized() const override {
    return recording_initialized_.load();
  }
  int32_t StartRecording() override {
    Record("StartRecording");
    if (!recording_initialized_.load()) {
      return -1;
    }
    recording_.store(true);
    return 0;
  }
  int32_t StopRecording() override {
    Record("StopRecording");
    recording_initialized_.store(false);
    recording_.store(false);
    return 0;
  }
  bool Recording() const override { return recording_.load(); }

  int32_t PlayoutDelay(uint16_t* delay_ms) const override {
    *delay_ms = playout_delay_ms_.load();
    return 0;
  }

  int32_t SetMicrophoneMute(bool enable) override {
    return Record(std::string("SetMicrophoneMute(") + OnOff(enable) + ")");
  }
  int32_t EnableBuiltInAEC(bool enable) override {
    return Record(std::string("EnableBuiltInAEC(") + OnOff(enable) + ")");
  }
  int32_t EnableBuiltInAGC(bool enable) override {
    return Record(std::string("EnableBuiltInAGC(") + OnOff(enable) + ")");
  }
  int32_t EnableBuiltInNS(bool enable) override {
    return Record(std::string("EnableBuiltInNS(") + OnOff(enable) + ")");
  }
  bool BuiltInAECIsAvailable() const override { return true; }
  bool BuiltInAGCIsAvailable() const override { return true; }
  bool BuiltInNSIsAvailable() const override { return true; }

 private:
  int32_t Record(std::string call) {
    std::lock_guard<std::mutex> lock(mutex_);
    calls_.push_back(std::move(call));
    return 0;
  }

  std::mutex mutex_;
  std::vector<std::string> calls_;
  std::vector<std::string> playout_guids_;
  std::vector<std::string> recording_guids_;
  std::atomic<webrtc::AudioTransport*> callback_{nullptr};
  std::atomic<bool> playout_initialized_{false};
  std::atomic<bool> playing_{false};
  std::atomic<bool> recording_initialized_{false};
  std::atomic<bool> recording_{false};
  std::atomic<int32_t> next_stop_playout_result_{0};
  std::atomic<uint16_t> playout_delay_ms_{0};
};

// libwebrtc's side of the device callbacks. With a re-entry target it calls
// the device side again from inside its own callback, once.
class HarnessTransport : public webrtc::AudioTransport {
 public:
  void Reset(webrtc::AudioTransport* reenter) {
    entries_.store(0);
    reenter_.store(reenter);
  }
  uint32_t entries() const { return entries_.load(); }

  using webrtc::AudioTransport::RecordedDataIsAvailable;
  int32_t RecordedDataIsAvailable(const void* audio_samples,
                                  size_t samples_per_channel,
                                  size_t bytes_per_frame,
                                  size_t channels,
                                  uint32_t sample_rate,
                                  uint32_t total_delay_ms,
                                  int32_t clock_drift,
                                  uint32_t current_mic_level,
                                  bool key_pressed,
                                  uint32_t& new_mic_level) override {
    entries_.fetch_add(1);
    if (webrtc::AudioTransport* device = reenter_.exchange(nullptr)) {
      device->RecordedDataIsAvailable(
          audio_samples, samples_per_channel, bytes_per_frame, channels,
          sample_rate, total_delay_ms, clock_drift, current_mic_level,
          key_pressed, new_mic_level);
    }
    return 0;
  }

  int32_t NeedMorePlayData(size_t samples_per_channel,
                           size_t bytes_per_frame,
                           size_t channels,
                           uint32_t sample_rate,
                           void* audio_samples,
                           size_t& samples_out,
                           int64_t* elapsed_time_ms,
                           int64_t* ntp_time_ms) override {
    entries_.fetch_add(1);
    if (webrtc::AudioTransport* device = reenter_.exchange(nullptr)) {
      std::array<int16_t, 960> nested;
      nested.fill(1);
      size_t nested_out = 0;
      int64_t nested_elapsed = 0;
      int64_t nested_ntp = 0;
      device->NeedMorePlayData(samples_per_channel, bytes_per_frame, channels,
                               sample_rate, nested.data(), nested_out,
                               &nested_elapsed, &nested_ntp);
    }
    std::memset(audio_samples, 0, samples_per_channel * bytes_per_frame);
    samples_out = samples_per_channel * channels;
    *elapsed_time_ms = -1;
    *ntp_time_ms = -1;
    return 0;
  }

  void PullRenderData(int,
                      int,
                      size_t,
                      size_t,
                      void*,
                      int64_t*,
                      int64_t*) override {}

 private:
  std::atomic<uint32_t> entries_{0};
  std::atomic<webrtc::AudioTransport*> reenter_{nullptr};
};

AdmProxyHarness::AdmProxyHarness(
    rust::Slice<const rust::String> playout_guids,
    rust::Slice<const rust::String> recording_guids,
    bool with_apm)
    : fake_(webrtc::make_ref_counted<FakePlatformAdm>()),
      mixer_(webrtc::make_ref_counted<RootAudioMixer>()),
      capture_(std::make_shared<RootCaptureState>()),
      fakes_(std::make_unique<
             std::vector<std::unique_ptr<RootFakeFxPipeline>>>()),
      transport_(std::make_unique<HarnessTransport>()) {
  fake_->SetDevices(ToStrings(playout_guids), ToStrings(recording_guids));
  if (with_apm) {
    apm_ = NewRootApmOverFakeForTest();
  }
  capture_->denoiser_slot = std::make_shared<RootDenoiserSlot>();
  proxy_ = webrtc::make_ref_counted<AdmProxy>(webrtc::CreateEnvironment(),
                                              nullptr, mixer_, fake_);
  proxy_->SetCaptureState(capture_);
  controller_ = std::make_unique<AudioDeviceController>(proxy_, mixer_, apm_,
                                                        capture_);
}

AdmProxyHarness::~AdmProxyHarness() {
  controller_ = nullptr;
  proxy_ = nullptr;
  fakes_ = nullptr;
}

rust::Vec<rust::String> AdmProxyHarness::take_calls() const {
  rust::Vec<rust::String> calls;
  for (const std::string& call : fake_->TakeCalls()) {
    calls.push_back(rust::String(call));
  }
  return calls;
}

void AdmProxyHarness::set_devices(
    rust::Slice<const rust::String> playout_guids,
    rust::Slice<const rust::String> recording_guids) const {
  fake_->SetDevices(ToStrings(playout_guids), ToStrings(recording_guids));
}

void AdmProxyHarness::fail_next_stop_playout(int32_t result) const {
  fake_->FailNextStopPlayout(result);
}

void AdmProxyHarness::set_device_playout_delay(uint16_t delay_ms) const {
  fake_->SetPlayoutDelay(delay_ms);
}

void AdmProxyHarness::acquire() const {
  proxy_->AcquirePlatformAdm();
}

void AdmProxyHarness::release() const {
  proxy_->ReleasePlatformAdm();
}

void AdmProxyHarness::enable(bool recording, bool playout) const {
  proxy_->set_recording_enabled(recording);
  proxy_->set_playout_enabled(playout);
}

void AdmProxyHarness::hold(bool hold) const {
  controller_->set_playout_hold(hold);
}

bool AdmProxyHarness::held() const {
  return proxy_->playout_hold();
}

void AdmProxyHarness::app_playout(bool start) const {
  if (start) {
    controller_->start_playout();
  } else {
    controller_->stop_playout();
  }
}

void AdmProxyHarness::app_recording(bool start) const {
  if (start) {
    controller_->start_recording();
  } else {
    controller_->stop_recording();
  }
}

void AdmProxyHarness::engine_start_playout() const {
  proxy_->StartPlayout();
}

void AdmProxyHarness::engine_stop_playout() const {
  proxy_->StopPlayout();
}

void AdmProxyHarness::engine_start_recording() const {
  proxy_->StartRecording();
}

void AdmProxyHarness::engine_stop_recording() const {
  proxy_->StopRecording();
}

int32_t AdmProxyHarness::engine_init_playout() const {
  return proxy_->InitPlayout();
}

int32_t AdmProxyHarness::engine_init_recording() const {
  return proxy_->InitRecording();
}

void AdmProxyHarness::register_transport() const {
  proxy_->RegisterAudioCallback(transport_.get());
}

void AdmProxyHarness::terminate() const {
  proxy_->Terminate();
}

namespace {

// Shared with the callback, which may outlive a waiter that gave up.
class OutcomeWaiter {
 public:
  AdmRequestCallback callback() const {
    return [state = state_](const AdmRequestOutcome& outcome) {
      state->outcome = outcome;
      state->done.Set();
    };
  }

  // An all-false outcome (nothing requested) when the request never completed.
  AudioDeviceRequestOutcome Wait() const {
    if (!state_->done.Wait(webrtc::TimeDelta::Seconds(10))) {
      return AudioDeviceRequestOutcome{};
    }
    return ToRequestOutcome(state_->outcome);
  }

 private:
  struct State {
    webrtc::Event done;
    AdmRequestOutcome outcome;
  };
  std::shared_ptr<State> state_ = std::make_shared<State>();
};

}  // namespace

AudioDeviceRequestOutcome AdmProxyHarness::select(
    const AudioDeviceSelection& recording,
    bool set_recording,
    const AudioDeviceSelection& playout,
    bool set_playout) const {
  OutcomeWaiter waiter;
  std::optional<AdmDeviceSelection> recording_selection;
  std::optional<AdmDeviceSelection> playout_selection;
  if (set_recording) {
    recording_selection = ToAdmSelection(recording);
  }
  if (set_playout) {
    playout_selection = ToAdmSelection(playout);
  }
  proxy_->SelectDevices(std::move(recording_selection),
                        std::move(playout_selection), waiter.callback());
  return waiter.Wait();
}

AudioDeviceRequestOutcome AdmProxyHarness::restart(bool recording,
                                                   bool playout) const {
  OutcomeWaiter waiter;
  proxy_->RestartDevices(recording, playout, waiter.callback());
  return waiter.Wait();
}

void AdmProxyHarness::flush() const {
  proxy_->FlushDeviceTasksForTest();
}

bool AdmProxyHarness::playing() const {
  return proxy_->Playing();
}

bool AdmProxyHarness::recording() const {
  return proxy_->Recording();
}

bool AdmProxyHarness::playout_initialized() const {
  return proxy_->PlayoutIsInitialized();
}

bool AdmProxyHarness::recording_initialized() const {
  return proxy_->RecordingIsInitialized();
}

uint16_t AdmProxyHarness::playout_delay() const {
  uint16_t delay_ms = 0;
  proxy_->PlayoutDelay(&delay_ms);
  return delay_ms;
}

bool AdmProxyHarness::has_device_stats() const {
  return proxy_->GetStats().has_value();
}

bool AdmProxyHarness::stop_on_mute() const {
  return proxy_->IsStopOnMuteModeEnabled();
}

bool AdmProxyHarness::builtin_effects_available() const {
  return proxy_->BuiltInAECIsAvailable() || proxy_->BuiltInAGCIsAvailable() ||
         proxy_->BuiltInNSIsAvailable();
}

int32_t AdmProxyHarness::set_microphone_mute(bool mute) const {
  return proxy_->SetMicrophoneMute(mute);
}

int32_t AdmProxyHarness::enable_builtin_effect(uint8_t effect,
                                               bool enable) const {
  switch (effect) {
    case 0:
      return proxy_->EnableBuiltInAEC(enable);
    case 1:
      return proxy_->EnableBuiltInAGC(enable);
    default:
      return proxy_->EnableBuiltInNS(enable);
  }
}

bool AdmProxyHarness::platform_playing() const {
  return fake_->Playing();
}

bool AdmProxyHarness::platform_recording() const {
  return fake_->Recording();
}

bool AdmProxyHarness::synthetic_playing() const {
  return proxy_->SyntheticPlayingForTest();
}

uint32_t AdmProxyHarness::device_callback(bool capture, bool reenter) const {
  webrtc::AudioTransport* device = fake_->callback();
  if (device == nullptr) {
    return 0;
  }
  transport_->Reset(reenter ? device : nullptr);
  std::thread device_thread([device, capture] {
    std::array<int16_t, 960> samples{};
    if (capture) {
      uint32_t new_mic_level = 0;
      device->RecordedDataIsAvailable(samples.data(), 480, 4, 2, 48000, 10, 0,
                                      0, false, new_mic_level);
    } else {
      size_t samples_out = 0;
      int64_t elapsed_time_ms = 0;
      int64_t ntp_time_ms = 0;
      device->NeedMorePlayData(480, 4, 2, 48000, samples.data(), samples_out,
                               &elapsed_time_ms, &ntp_time_ms);
    }
  });
  device_thread.join();
  return transport_->entries();
}

uint64_t AdmProxyHarness::lock_on_audio_thread() const {
  const uint64_t before = AudioThreadLockViolations();
  std::thread audio_thread([this] {
    MarkAudioThread();
    proxy_->playout_hold();
  });
  audio_thread.join();
  return AudioThreadLockViolations() - before;
}

RootAudioNativeStats AdmProxyHarness::stats() const {
  return controller_->root_audio_stats();
}

bool AdmProxyHarness::mixer_platform_active() const {
  return mixer_->platform_active();
}

bool AdmProxyHarness::apply_control(
    const RootAudioControlUpdate& control) const {
  return controller_->set_root_audio_control(control);
}

void AdmProxyHarness::reset_root_audio(bool close_gate) const {
  controller_->reset_root_audio(close_gate);
}

RootControlState AdmProxyHarness::control_state() const {
  RootControlState state{};
  state.mic_muted = capture_->mic_muted();
  state.push_to_talk = capture_->push_to_talk();
  state.input_volume = capture_->input_volume.load();
  state.denoise_power = capture_->denoise_power.load();
  state.auto_gain = capture_->auto_gain.load();
  state.gate_threshold = capture_->gate_threshold.load();
  state.auto_sensitivity = capture_->auto_sensitivity.load();
  state.gain_reset_db = capture_->gain_reset_db.load();
  state.gain_reset_seq = capture_->gain_reset_seq.load();
  state.master_volume = mixer_->master_volume();
  state.deafened = mixer_->deafened();
  state.denoiser_wanted = capture_->denoiser_wanted.load();
  state.stream_epoch = capture_->stream_epoch.load();
  state.dsp_reset_seq = capture_->dsp_reset_seq.load();
  return state;
}

const AudioDeviceController& AdmProxyHarness::controller() const {
  return *controller_;
}

uint64_t AdmProxyHarness::add_fake_pipeline() const {
  fakes_->push_back(std::make_unique<RootFakeFxPipeline>(RootFakeFxConfig()));
  return static_cast<uint64_t>(
      reinterpret_cast<uintptr_t>(fakes_->back()->pipeline()));
}

uint8_t AdmProxyHarness::run_denoiser(uint32_t rate) const {
  const size_t frames = rate / 100;
  std::vector<float> in(frames, 100.0f);
  std::vector<float> out(RootDenoiserSlot::kOutputFrames * frames + 1);
  const RootDenoiseResult result = capture_->denoiser_slot->ProcessAudio(
      in.data(), frames, static_cast<int>(rate), false, 0.75f, out.data(),
      RootDenoiserSlot::kOutputFrames * frames);
  return static_cast<uint8_t>(result.kind);
}

void AdmProxyHarness::latch_denoiser_bypass(uint32_t reason) const {
  capture_->denoiser_slot->LatchBypass(static_cast<RootDenoiserBypass>(reason));
}

void AdmProxyHarness::set_speaking(bool speaking) const {
  const uint64_t transitions = capture_->ReadSpeaking().seq + 1;
  capture_->speaking_word.store((transitions << 1) | (speaking ? 1u : 0u),
                                std::memory_order_release);
}

std::unique_ptr<AdmProxyHarness> new_adm_proxy_harness(
    rust::Slice<const rust::String> playout_guids,
    rust::Slice<const rust::String> recording_guids) {
  return std::make_unique<AdmProxyHarness>(playout_guids, recording_guids,
                                           false);
}

std::unique_ptr<AdmProxyHarness> new_adm_proxy_harness_with_apm(
    rust::Slice<const rust::String> playout_guids,
    rust::Slice<const rust::String> recording_guids) {
  return std::make_unique<AdmProxyHarness>(playout_guids, recording_guids,
                                           true);
}

}  // namespace livekit_ffi
