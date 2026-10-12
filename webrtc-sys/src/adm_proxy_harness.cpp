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

#include <algorithm>
#include <array>
#include <atomic>
#include <chrono>
#include <cstring>
#include <map>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include "api/audio/audio_frame.h"
#include "api/environment/environment_factory.h"
#include "api/make_ref_counted.h"
#include "livekit/adm_proxy.h"
#include "livekit/audio_device_controller.h"
#include "livekit/audio_thread.h"
#include "livekit/root_audio_mixer.h"
#include "modules/audio_device/audio_device_buffer.h"
#include "modules/audio_device/include/audio_device_default.h"
#include "rtc_base/event.h"
#include "rtc_base/thread.h"

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

// Plays a constant level.
class LevelTransport : public webrtc::AudioTransport {
 public:
  explicit LevelTransport(int16_t level) : level_(level) {}

  using webrtc::AudioTransport::RecordedDataIsAvailable;
  int32_t RecordedDataIsAvailable(const void*,
                                  size_t,
                                  size_t,
                                  size_t,
                                  uint32_t,
                                  uint32_t,
                                  int32_t,
                                  uint32_t,
                                  bool,
                                  uint32_t&) override {
    return 0;
  }

  int32_t NeedMorePlayData(size_t samples_per_channel,
                           size_t,
                           size_t channels,
                           uint32_t,
                           void* audio_samples,
                           size_t& samples_out,
                           int64_t* elapsed_time_ms,
                           int64_t* ntp_time_ms) override {
    samples_out = samples_per_channel * channels;
    std::fill_n(static_cast<int16_t*>(audio_samples), samples_out, level_);
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
  const int16_t level_;
};

}  // namespace

// Records the state-changing calls it gets and behaves like a platform ADM:
// Start needs Init, Stop closes the device. Its playout runs through
// libwebrtc's own AudioDeviceBuffer, as AudioDeviceModuleImpl's does.
class FakePlatformAdm
    : public webrtc::webrtc_impl::AudioDeviceModuleDefault<
          webrtc::AudioDeviceModule> {
 public:
  std::vector<std::string> TakeCalls() {
    std::lock_guard<std::mutex> lock(mutex_);
    return std::exchange(calls_, {});
  }

  std::vector<std::string> TakeChannelCalls() {
    std::lock_guard<std::mutex> lock(mutex_);
    return std::exchange(channel_calls_, {});
  }

  void SetDevices(std::vector<std::string> playout,
                  std::vector<std::string> recording) {
    std::lock_guard<std::mutex> lock(mutex_);
    playout_guids_ = std::move(playout);
    recording_guids_ = std::move(recording);
  }

  void FailNextStopRecording(int32_t result) {
    next_stop_recording_result_.store(result);
  }
  void FailNextStopPlayout(int32_t result) {
    next_stop_playout_result_.store(result);
  }
  void SetPlayoutDelay(uint16_t delay_ms) { playout_delay_ms_.store(delay_ms); }
  webrtc::AudioTransport* callback() const { return callback_.load(); }

  // The next StartPlayout blocks (as the PulseAudio ADM waits for its stream)
  // until released, at most 10 s.
  void BlockNextStartPlayout() {
    start_entered_.Reset();
    start_release_.Reset();
    block_start_.store(true);
  }
  bool WaitStartPlayoutBlocked() {
    return start_entered_.Wait(webrtc::TimeDelta::Seconds(5));
  }
  void ReleaseStartPlayout() { start_release_.Set(); }

  // One 10 ms render callback as the Windows ADM's render thread makes it
  // (audio_device_core_win.cc): RequestPlayoutData, then GetPlayoutData into
  // the device's buffer. Returns what RequestPlayoutData returned (the frames
  // the transport produced, 0 without a transport), -1 when not playing.
  int32_t DeviceRender(std::vector<int16_t>* samples) {
    samples->clear();
    if (!playing_.load()) {
      return -1;
    }
    const int32_t frames = buffer_.RequestPlayoutData(kDeviceFrames);
    std::vector<int16_t> device(webrtc::kMaxBufferSizeBytes / sizeof(int16_t));
    buffer_.GetPlayoutData(device.data());
    device.resize(kDeviceFrames * buffer_.PlayoutChannels());
    *samples = std::move(device);
    return frames;
  }

  // Leaves `level` in the buffer's stereo playout samples, as an earlier pull
  // would. It stands in for the uninitialized heap block the buffer allocates
  // on its first pull and plays as it is when no transport is registered.
  void StaleBuffer(int16_t level) {
    LevelTransport stale(level);
    buffer_.SetPlayoutSampleRate(48000);
    buffer_.SetPlayoutChannels(2);
    buffer_.RegisterAudioCallback(&stale);
    buffer_.RequestPlayoutData(kDeviceFrames);
    buffer_.RegisterAudioCallback(callback_.load());
  }

  int32_t Init() override { return Record("Init"); }
  int32_t Terminate() override { return Record("Terminate"); }
  int32_t RegisterAudioCallback(webrtc::AudioTransport* transport) override {
    callback_.store(transport);
    buffer_.RegisterAudioCallback(transport);
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
    buffer_.SetPlayoutSampleRate(48000);
    buffer_.SetPlayoutChannels(stereo_playout_.load() ? 2 : 1);
    RecordChannel(stereo_playout_.load() ? "InitPlayout stereo"
                                         : "InitPlayout mono",
                  0);
    return Record("InitPlayout");
  }

  // As AudioDeviceModuleImpl: no channel change while playout is initialized.
  // Mono until set, as the PulseAudio ADM starts.
  int32_t SetStereoPlayout(bool enable) override {
    const std::string call = std::string("SetStereoPlayout(") + OnOff(enable) +
                             ")";
    if (playout_initialized_.load()) {
      return RecordChannel(call + " refused", -1);
    }
    stereo_playout_.store(enable);
    return RecordChannel(call, 0);
  }
  int32_t StereoPlayoutIsAvailable(bool* available) const override {
    *available = true;
    return 0;
  }
  int32_t StereoPlayout(bool* enabled) const override {
    *enabled = stereo_playout_.load();
    return 0;
  }
  // As the Windows ADM: no speaker or microphone (re)initialization while
  // that direction runs.
  int32_t InitSpeaker() override {
    return RecordChannel("InitSpeaker", playing_.load() ? -1 : 0);
  }
  int32_t InitMicrophone() override {
    return RecordChannel("InitMicrophone", recording_.load() ? -1 : 0);
  }
  int32_t SpeakerVolumeIsAvailable(bool* available) override {
    *available = true;
    return 0;
  }
  int32_t SetSpeakerVolume(uint32_t volume) override {
    return RecordChannel("SetSpeakerVolume(" + std::to_string(volume) + ")",
                         0);
  }
  int32_t SpeakerMuteIsAvailable(bool* available) override {
    *available = true;
    return 0;
  }
  int32_t SetSpeakerMute(bool enable) override {
    return RecordChannel(std::string("SetSpeakerMute(") + OnOff(enable) + ")",
                         0);
  }
  bool PlayoutIsInitialized() const override {
    return playout_initialized_.load();
  }
  int32_t StartPlayout() override {
    Record("StartPlayout");
    if (block_start_.exchange(false)) {
      start_entered_.Set();
      start_release_.Wait(webrtc::TimeDelta::Seconds(10));
    }
    if (!playout_initialized_.load()) {
      return -1;
    }
    buffer_.StartPlayout();
    playing_.store(true);
    return 0;
  }
  int32_t StopPlayout() override {
    Record("StopPlayout");
    const bool open = playout_initialized_.exchange(false);
    playing_.store(false);
    buffer_.StopPlayout();
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
    const bool open = recording_initialized_.exchange(false);
    recording_.store(false);
    return open ? next_stop_recording_result_.exchange(0) : 0;
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

  int32_t RecordChannel(std::string call, int32_t result) {
    std::lock_guard<std::mutex> lock(mutex_);
    channel_calls_.push_back(std::move(call));
    return result;
  }

  static constexpr size_t kDeviceFrames = 480;

  webrtc::AudioDeviceBuffer buffer_{webrtc::CreateEnvironment(), true};
  std::mutex mutex_;
  std::vector<std::string> calls_;
  std::vector<std::string> channel_calls_;
  std::atomic<bool> stereo_playout_{false};
  std::vector<std::string> playout_guids_;
  std::vector<std::string> recording_guids_;
  std::atomic<webrtc::AudioTransport*> callback_{nullptr};
  std::atomic<bool> playout_initialized_{false};
  std::atomic<bool> playing_{false};
  std::atomic<bool> recording_initialized_{false};
  std::atomic<bool> recording_{false};
  std::atomic<int32_t> next_stop_playout_result_{0};
  std::atomic<int32_t> next_stop_recording_result_{0};
  std::atomic<uint16_t> playout_delay_ms_{0};
  std::atomic<bool> block_start_{false};
  webrtc::Event start_entered_;
  webrtc::Event start_release_;
};

// libwebrtc's side of the device callbacks. With a re-entry target it calls
// the device side again from inside its own callback, once. It plays silence,
// or the mixer as AudioTransportImpl does (the device runs at the mixer's
// 48 kHz, so there is nothing to resample).
class HarnessTransport : public webrtc::AudioTransport {
 public:
  void Reset(webrtc::AudioTransport* reenter) {
    entries_.store(0);
    reenter_.store(reenter);
  }
  uint32_t entries() const { return entries_.load(); }
  void PlayMixer(RootAudioMixer* mixer) { mixer_.store(mixer); }

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
    RootAudioMixer* mixer = mixer_.load();
    if (mixer != nullptr) {
      mixer->Mix(channels, &mixed_);
    }
    if (mixer != nullptr &&
        mixed_.samples_per_channel() == samples_per_channel &&
        mixed_.num_channels() == channels) {
      std::memcpy(audio_samples, mixed_.data(),
                  samples_per_channel * bytes_per_frame);
    } else {
      std::memset(audio_samples, 0, samples_per_channel * bytes_per_frame);
    }
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
  std::atomic<RootAudioMixer*> mixer_{nullptr};
  webrtc::AudioFrame mixed_;
};

AdmProxyHarness::AdmProxyHarness(
    rust::Slice<const rust::String> playout_guids,
    rust::Slice<const rust::String> recording_guids,
    bool with_apm,
    bool with_platform)
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
  capture_->monitor = std::make_shared<RootAudioMonitor>();
  proxy_ = webrtc::make_ref_counted<AdmProxy>(
      webrtc::CreateEnvironment(), nullptr, mixer_,
      with_platform ? webrtc::scoped_refptr<webrtc::AudioDeviceModule>(fake_)
                    : nullptr);
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

rust::Vec<rust::String> AdmProxyHarness::take_channel_calls() const {
  rust::Vec<rust::String> calls;
  for (const std::string& call : fake_->TakeChannelCalls()) {
    calls.push_back(rust::String(call));
  }
  return calls;
}

void AdmProxyHarness::set_devices(
    rust::Slice<const rust::String> playout_guids,
    rust::Slice<const rust::String> recording_guids) const {
  fake_->SetDevices(ToStrings(playout_guids), ToStrings(recording_guids));
}

void AdmProxyHarness::set_uid_map(rust::Slice<const rust::String> uids,
                                  rust::Slice<const rust::String> guids) const {
  auto map = std::make_shared<std::map<std::string, std::string>>();
  const std::vector<std::string> keys = ToStrings(uids);
  const std::vector<std::string> values = ToStrings(guids);
  for (size_t i = 0; i < std::min(keys.size(), values.size()); ++i) {
    (*map)[keys[i]] = values[i];
  }
  proxy_->SetUidTranslatorForTest(
      [map](const std::string& uid) -> std::optional<std::string> {
        const auto found = map->find(uid);
        if (found == map->end()) {
          return std::nullopt;
        }
        return found->second;
      });
}

void AdmProxyHarness::set_enumeration_lock(bool serialize,
                                           int32_t wait_ms) const {
  proxy_->SetEnumerationLockForTest(serialize, wait_ms);
}

void AdmProxyHarness::block_next_start_playout() const {
  fake_->BlockNextStartPlayout();
}

bool AdmProxyHarness::start_playout_blocked() const {
  return fake_->WaitStartPlayoutBlocked();
}

void AdmProxyHarness::release_start_playout(uint32_t after_ms) const {
  std::thread([fake = fake_, after_ms] {
    std::this_thread::sleep_for(std::chrono::milliseconds(after_ms));
    fake->ReleaseStartPlayout();
  }).detach();
}

void AdmProxyHarness::fail_next_stop_recording(int32_t result) const {
  fake_->FailNextStopRecording(result);
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

uint8_t AdmProxyHarness::monitor(bool enabled) const {
  return controller_->set_root_audio_monitor(enabled);
}

void AdmProxyHarness::monitor_push(int32_t rate,
                                   rust::Slice<const float> samples) const {
  capture_->monitor->Push(samples.data(), samples.size(), rate);
}

rust::Vec<int16_t> AdmProxyHarness::render_frame() const {
  rust::Vec<int16_t> result;
  webrtc::AudioTransport* device = fake_->callback();
  if (device == nullptr) {
    return result;
  }
  std::array<int16_t, 960> samples{};
  std::thread device_thread([device, &samples] {
    size_t samples_out = 0;
    int64_t elapsed_time_ms = 0;
    int64_t ntp_time_ms = 0;
    device->NeedMorePlayData(480, 4, 2, 48000, samples.data(), samples_out,
                             &elapsed_time_ms, &ntp_time_ms);
  });
  device_thread.join();
  for (int16_t sample : samples) {
    result.push_back(sample);
  }
  return result;
}

void AdmProxyHarness::stale_device_buffer(int16_t level) const {
  fake_->StaleBuffer(level);
}

AdmDeviceRender AdmProxyHarness::device_render() const {
  AdmDeviceRender result{};
  std::vector<int16_t> samples;
  std::thread device_thread([this, &samples, &result] {
    result.frames = fake_->DeviceRender(&samples);
  });
  device_thread.join();
  for (int16_t sample : samples) {
    result.samples.push_back(sample);
  }
  return result;
}

void AdmProxyHarness::engine_plays_mixer() const {
  transport_->PlayMixer(mixer_.get());
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

rust::Vec<int32_t> AdmProxyHarness::engine_adm_init() const {
  rust::Vec<int32_t> results;
  results.push_back(proxy_->InitSpeaker());
  bool available = false;
  results.push_back(proxy_->StereoPlayoutIsAvailable(&available));
  results.push_back(proxy_->SetStereoPlayout(available));
  results.push_back(proxy_->InitMicrophone());
  return results;
}

bool AdmProxyHarness::engine_stereo_playout() const {
  bool enabled = false;
  proxy_->StereoPlayout(&enabled);
  return enabled;
}

int32_t AdmProxyHarness::engine_set_stereo_playout(bool enable) const {
  return proxy_->SetStereoPlayout(enable);
}

int32_t AdmProxyHarness::engine_set_speaker_volume(uint32_t volume) const {
  return proxy_->SetSpeakerVolume(volume);
}

int32_t AdmProxyHarness::engine_set_speaker_mute(bool mute) const {
  return proxy_->SetSpeakerMute(mute);
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
  capture_->last_frame_us.store(
      std::chrono::duration_cast<std::chrono::microseconds>(
          std::chrono::steady_clock::now().time_since_epoch())
          .count());
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

std::unique_ptr<AdmProxyHarness> new_adm_proxy_harness_without_platform_adm() {
  return std::make_unique<AdmProxyHarness>(
      rust::Slice<const rust::String>(), rust::Slice<const rust::String>(),
      false, false);
}

// The voice engine's capture path in miniature (AudioTransportImpl): each
// recorded frame through the APM as int16 in its own format.
class HarnessApmTransport : public webrtc::AudioTransport {
 public:
  explicit HarnessApmTransport(
      webrtc::scoped_refptr<webrtc::AudioProcessing> apm)
      : apm_(std::move(apm)) {}

  std::vector<int16_t> TakeOutput() {
    std::lock_guard<std::mutex> lock(mutex_);
    return std::exchange(output_, {});
  }

  using webrtc::AudioTransport::RecordedDataIsAvailable;
  int32_t RecordedDataIsAvailable(const void* audio_samples,
                                  size_t samples_per_channel,
                                  size_t /* bytes_per_frame */,
                                  size_t channels,
                                  uint32_t sample_rate,
                                  uint32_t total_delay_ms,
                                  int32_t /* clock_drift */,
                                  uint32_t current_mic_level,
                                  bool key_pressed,
                                  uint32_t& new_mic_level) override {
    std::vector<int16_t> output(samples_per_channel * channels);
    const webrtc::StreamConfig config(static_cast<int>(sample_rate), channels);
    apm_->set_stream_delay_ms(static_cast<int>(total_delay_ms));
    apm_->set_stream_key_pressed(key_pressed);
    apm_->ProcessStream(static_cast<const int16_t*>(audio_samples), config,
                        config, output.data());
    new_mic_level = current_mic_level;
    std::lock_guard<std::mutex> lock(mutex_);
    output_ = std::move(output);
    return 0;
  }

  void SetRemoteLevel(int16_t level) { remote_level_.store(level); }

  std::vector<int16_t> ReverseInput() {
    std::lock_guard<std::mutex> lock(mutex_);
    return reverse_input_;
  }

  // As AudioTransportImpl: the mix goes to the APM as its reverse stream, then
  // to the device.
  int32_t NeedMorePlayData(size_t samples_per_channel,
                           size_t bytes_per_frame,
                           size_t channels,
                           uint32_t sample_rate,
                           void* audio_samples,
                           size_t& samples_out,
                           int64_t* elapsed_time_ms,
                           int64_t* ntp_time_ms) override {
    std::vector<int16_t> mix(samples_per_channel * channels,
                             remote_level_.load());
    std::vector<int16_t> processed(mix.size());
    const webrtc::StreamConfig config(static_cast<int>(sample_rate), channels);
    apm_->ProcessReverseStream(mix.data(), config, config, processed.data());
    std::memcpy(audio_samples, mix.data(),
                std::min(mix.size() * sizeof(int16_t),
                         samples_per_channel * bytes_per_frame));
    samples_out = samples_per_channel * channels;
    *elapsed_time_ms = -1;
    *ntp_time_ms = -1;
    std::lock_guard<std::mutex> lock(mutex_);
    reverse_input_ = std::move(mix);
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
  const webrtc::scoped_refptr<webrtc::AudioProcessing> apm_;
  std::atomic<int16_t> remote_level_{0};
  std::mutex mutex_;
  std::vector<int16_t> output_;
  std::vector<int16_t> reverse_input_;
};

RootVoiceHarness::RootVoiceHarness()
    : worker_(webrtc::Thread::Create()),
      fake_(webrtc::make_ref_counted<FakePlatformAdm>()),
      mixer_(webrtc::make_ref_counted<RootAudioMixer>()),
      capture_(std::make_shared<RootCaptureState>()),
      worker_hold_(std::make_shared<webrtc::Event>()) {
  worker_->SetName("root-voice-worker", nullptr);
  worker_->Start();
  fake_->SetDevices({"{spk}"}, {"{mic}"});
  if constexpr (kRootDenoiserSupported) {
    capture_->denoiser_slot = std::make_shared<RootDenoiserSlot>();
  }
  capture_->monitor = std::make_shared<RootAudioMonitor>();
  apm_ = CreateRootAudioProcessing(webrtc::CreateEnvironment(), capture_,
                                   worker_.get());
  transport_ = std::make_unique<HarnessApmTransport>(apm_);
  proxy_ = webrtc::make_ref_counted<AdmProxy>(webrtc::CreateEnvironment(),
                                              nullptr, mixer_, fake_);
  proxy_->SetCaptureState(capture_);
  controller_ = std::make_unique<AudioDeviceController>(proxy_, mixer_, apm_,
                                                        capture_);
  proxy_->AcquirePlatformAdm();
  proxy_->set_recording_enabled(true);
  proxy_->set_playout_enabled(true);
  proxy_->RegisterAudioCallback(transport_.get());
  // A session starts, as the FFI's first handle does.
  controller_->reset_root_audio(false);
  proxy_->FlushDeviceTasksForTest();
}

RootVoiceHarness::~RootVoiceHarness() {
  worker_hold_->Set();
  controller_ = nullptr;
  proxy_->RegisterAudioCallback(nullptr);
  proxy_ = nullptr;
  worker_->Stop();
  transport_ = nullptr;
  apm_ = nullptr;
  fakes_.clear();
}

const AudioDeviceController& RootVoiceHarness::controller() const {
  return *controller_;
}

void RootVoiceHarness::recording(bool start) const {
  if (start) {
    controller_->start_recording();
  } else {
    controller_->stop_recording();
  }
  proxy_->FlushDeviceTasksForTest();
}

void RootVoiceHarness::engine_recording(bool start) const {
  if (start) {
    proxy_->StartRecording();
  } else {
    proxy_->StopRecording();
  }
  proxy_->FlushDeviceTasksForTest();
}

uint8_t RootVoiceHarness::monitor(bool enabled) const {
  const uint8_t result = controller_->set_root_audio_monitor(enabled);
  proxy_->FlushDeviceTasksForTest();
  return result;
}

bool RootVoiceHarness::platform_playing() const {
  return fake_->Playing();
}

bool RootVoiceHarness::platform_recording() const {
  return fake_->Recording();
}

uint64_t RootVoiceHarness::stream_epoch() const {
  return capture_->stream_epoch.load();
}

void RootVoiceHarness::set_remote_level(int16_t level) const {
  transport_->SetRemoteLevel(level);
}

rust::Vec<int16_t> RootVoiceHarness::render(int32_t rate,
                                            size_t channels) const {
  rust::Vec<int16_t> result;
  webrtc::AudioTransport* device = fake_->callback();
  if (device == nullptr || rate < 100 || channels == 0) {
    return result;
  }
  const size_t frames = static_cast<size_t>(rate / 100);
  std::vector<int16_t> samples(frames * channels, 0);
  std::thread device_thread([&] {
    size_t samples_out = 0;
    int64_t elapsed_time_ms = 0;
    int64_t ntp_time_ms = 0;
    device->NeedMorePlayData(frames, 2 * channels, channels,
                             static_cast<uint32_t>(rate), samples.data(),
                             samples_out, &elapsed_time_ms, &ntp_time_ms);
  });
  device_thread.join();
  for (int16_t sample : samples) {
    result.push_back(sample);
  }
  return result;
}

rust::Vec<int16_t> RootVoiceHarness::reverse_input() const {
  rust::Vec<int16_t> result;
  for (int16_t sample : transport_->ReverseInput()) {
    result.push_back(sample);
  }
  return result;
}

rust::Vec<int16_t> RootVoiceHarness::capture(
    int32_t rate,
    size_t channels,
    rust::Slice<const int16_t> input) const {
  rust::Vec<int16_t> result;
  webrtc::AudioTransport* device = fake_->callback();
  if (device == nullptr || rate < 100 || channels == 0) {
    return result;
  }
  const size_t frames = static_cast<size_t>(rate / 100);
  std::vector<int16_t> samples(frames * channels, 0);
  std::copy_n(input.data(), std::min(input.size(), samples.size()),
              samples.begin());
  std::thread device_thread([&] {
    uint32_t new_mic_level = 0;
    device->RecordedDataIsAvailable(samples.data(), frames, 2 * channels,
                                    channels, static_cast<uint32_t>(rate), 0,
                                    0, 0, false, new_mic_level);
  });
  device_thread.join();
  for (int16_t sample : transport_->TakeOutput()) {
    result.push_back(sample);
  }
  return result;
}

RootFakeFxPipeline* RootVoiceHarness::Fake(uint64_t pipeline) const {
  for (const std::unique_ptr<RootFakeFxPipeline>& fake : fakes_) {
    if (static_cast<uint64_t>(reinterpret_cast<uintptr_t>(fake->pipeline())) ==
        pipeline) {
      return fake.get();
    }
  }
  return nullptr;
}

uint64_t RootVoiceHarness::add_fake_pipeline(float scale,
                                             uint32_t delay_samples) const {
  RootFakeFxConfig config;
  config.scale = scale;
  config.delay_samples = delay_samples;
  fakes_.push_back(std::make_unique<RootFakeFxPipeline>(config));
  return static_cast<uint64_t>(
      reinterpret_cast<uintptr_t>(fakes_.back()->pipeline()));
}

void RootVoiceHarness::set_fake_empty(uint64_t pipeline, bool empty) const {
  if (RootFakeFxPipeline* fake = Fake(pipeline)) {
    fake->set_empty(empty);
  }
}

RootVoiceFakeCounts RootVoiceHarness::fake_counts(uint64_t pipeline) const {
  RootVoiceFakeCounts counts{};
  if (const RootFakeFxPipeline* fake = Fake(pipeline)) {
    counts.process_calls = fake->process_calls();
    counts.flush_calls = fake->flush_calls();
    counts.power_sets = fake->power_sets();
    counts.last_power = fake->last_power();
  }
  return counts;
}

void RootVoiceHarness::hold_worker(bool hold) const {
  if (!hold) {
    worker_hold_->Set();
    return;
  }
  worker_hold_->Reset();
  auto entered = std::make_shared<webrtc::Event>();
  worker_->PostTask([entered, release = worker_hold_] {
    entered->Set();
    release->Wait(webrtc::Event::kForever);
  });
  entered->Wait(webrtc::Event::kForever);
}

bool RootVoiceHarness::webrtc_ns_applied() const {
  return capture_->webrtc_ns_applied.load();
}

std::unique_ptr<RootVoiceHarness> new_root_voice_harness() {
  return std::make_unique<RootVoiceHarness>();
}

RootMonitorHarness::RootMonitorHarness()
    : monitor_(std::make_unique<RootAudioMonitor>()) {}

RootMonitorHarness::~RootMonitorHarness() = default;

void RootMonitorHarness::set_enabled(bool enabled) const {
  monitor_->SetEnabled(enabled);
}

void RootMonitorHarness::resync() const {
  monitor_->Resync();
}

void RootMonitorHarness::push(int32_t rate,
                              rust::Slice<const float> samples) const {
  monitor_->Push(samples.data(), samples.size(), rate);
}

rust::Vec<int16_t> RootMonitorHarness::mix(
    uint32_t rate,
    size_t channels,
    rust::Slice<const int16_t> base) const {
  const size_t frames = rate / 100;
  std::vector<int16_t> samples(frames * channels, 0);
  std::copy_n(base.data(), std::min(base.size(), samples.size()),
              samples.begin());
  monitor_->MixInto(samples.data(), frames, channels, rate);
  rust::Vec<int16_t> result;
  for (int16_t sample : samples) {
    result.push_back(sample);
  }
  return result;
}

RootMonitorStats RootMonitorHarness::stats() const {
  const RootAudioMonitorStats stats = monitor_->Read();
  return RootMonitorStats{stats.active, stats.frames, stats.dropped_frames};
}

std::unique_ptr<RootMonitorHarness> new_root_monitor_harness() {
  return std::make_unique<RootMonitorHarness>();
}

bool adm_name_matches_for_test(rust::Slice<const uint8_t> device,
                               rust::Str wanted) {
  AdmDeviceSelection selection;
  selection.kind = AdmSelectionKind::kName;
  selection.id = std::string(wanted);
  return ResolveAdmDeviceIndex(
             selection, 1,
             [&device](int, std::string* name, std::string* guid) {
               *name = AdmDeviceString(
                   reinterpret_cast<const char*>(device.data()),
                   device.size());
               guid->clear();
               return true;
             }) == 0;
}

}  // namespace livekit_ffi
