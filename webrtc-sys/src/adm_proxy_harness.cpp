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
#include <set>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include "api/audio/audio_frame.h"
#include "api/audio/audio_processing.h"
#include "api/audio_codecs/builtin_audio_decoder_factory.h"
#include "api/audio_codecs/builtin_audio_encoder_factory.h"
#include "api/enable_media.h"
#include "api/environment/environment_factory.h"
#include "api/make_ref_counted.h"
#include "api/peer_connection_interface.h"
#include "api/video_codecs/video_decoder_factory.h"
#include "api/video_codecs/video_encoder_factory.h"
#include "livekit/adm_proxy.h"
#include "livekit/audio_device_controller.h"
#include "livekit/audio_thread.h"
#include "livekit/root_audio_mixer.h"
#include "livekit/root_voice_engine_hold.h"
#include "modules/audio_device/audio_device_buffer.h"
#include "modules/audio_device/include/audio_device_default.h"
#include "pc/connection_context.h"
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

constexpr uint32_t FourCc(const char (&code)[5]) {
  return (static_cast<uint32_t>(code[0]) << 24) |
         (static_cast<uint32_t>(code[1]) << 16) |
         (static_cast<uint32_t>(code[2]) << 8) | static_cast<uint32_t>(code[3]);
}

constexpr uint32_t kFakeSystemObject = 1;
constexpr uint32_t kFakeOutputDevice = 42;
constexpr AdmListenerAddress kFakeDevicesAddress{FourCc("dev#"), FourCc("glob"),
                                                 0};
constexpr AdmListenerAddress kFakeDefaultOutputAddress{FourCc("dOut"),
                                                       FourCc("glob"), 0};
constexpr AdmListenerAddress kFakeDefaultInputAddress{FourCc("dIn "),
                                                      FourCc("glob"), 0};
constexpr AdmListenerAddress kFakeFormatAddress{FourCc("sfmt"), FourCc("outp"),
                                                0};
static_assert(kFakeDevicesAddress.selector == kAdmSelectorDevices);
static_assert(kFakeDefaultOutputAddress.selector == kAdmSelectorDefaultOutput);
static_assert(kFakeDefaultInputAddress.selector == kAdmSelectorDefaultInput);
static_assert(kFakeFormatAddress.selector == kAdmSelectorStreamFormat);

using FakeListenerProc = int32_t (*)(uint32_t object,
                                     uint32_t count,
                                     const AdmListenerAddress* addresses,
                                     void* client);

int32_t CountingListener(uint32_t,
                         uint32_t,
                         const AdmListenerAddress*,
                         void* client) {
  static_cast<std::atomic<uint32_t>*>(client)->fetch_add(1);
  return 0;
}

// Reached through the router as AudioObjectAddPropertyListener is on macOS.
class FakeCoreAudio {
 public:
  int32_t AddListener(uint32_t object,
                      AdmListenerAddress address,
                      FakeListenerProc proc,
                      void* client) {
    const std::optional<int32_t> routed = AdmListenerRegistry::Get().Add(
        KeyOf(object, address, proc, client), &Invoke, [&](uint64_t id) {
          return Register({object, address, &Routed, ClientOf(id)});
        });
    return routed ? *routed : Register({object, address, proc, client});
  }

  int32_t RemoveListener(uint32_t object,
                         AdmListenerAddress address,
                         FakeListenerProc proc,
                         void* client) {
    const std::optional<int32_t> routed = AdmListenerRegistry::Get().Remove(
        KeyOf(object, address, proc, client), [&](uint64_t id) {
          return Unregister({object, address, &Routed, ClientOf(id)});
        });
    return routed ? *routed : Unregister({object, address, proc, client});
  }

  bool HasOriginal(uint32_t object,
                   AdmListenerAddress address,
                   FakeListenerProc proc,
                   void* client) const {
    std::lock_guard<std::mutex> lock(mutex_);
    return std::find(entries_.begin(), entries_.end(),
                     Entry{object, address, proc, client}) != entries_.end();
  }

  uint32_t Count() const {
    std::lock_guard<std::mutex> lock(mutex_);
    return static_cast<uint32_t>(entries_.size());
  }

  void Notify(uint32_t object, AdmListenerAddress address) {
    std::vector<Entry> matching;
    {
      std::lock_guard<std::mutex> lock(mutex_);
      for (const Entry& entry : entries_) {
        if (entry.object == object && entry.address == address) {
          matching.push_back(entry);
        }
      }
    }
    std::thread queue([&matching, object, address] {
      for (const Entry& entry : matching) {
        entry.proc(object, 1, &address, entry.client);
      }
    });
    queue.join();
  }

 private:
  struct Entry {
    uint32_t object = 0;
    AdmListenerAddress address;
    FakeListenerProc proc = nullptr;
    void* client = nullptr;

    bool operator==(const Entry& other) const = default;
  };

  static AdmListenerKey KeyOf(uint32_t object,
                              AdmListenerAddress address,
                              FakeListenerProc proc,
                              void* client) {
    AdmListenerKey key;
    key.object = object;
    key.address = address;
    key.proc = reinterpret_cast<const void*>(proc);
    key.client = client;
    return key;
  }

  static void* ClientOf(uint64_t id) {
    return reinterpret_cast<void*>(static_cast<uintptr_t>(id));
  }

  static void Invoke(const AdmListenerKey& key,
                     uint32_t object,
                     const std::vector<AdmListenerAddress>& addresses) {
    reinterpret_cast<FakeListenerProc>(const_cast<void*>(key.proc))(
        object, static_cast<uint32_t>(addresses.size()), addresses.data(),
        key.client);
  }

  static int32_t Routed(uint32_t object,
                        uint32_t count,
                        const AdmListenerAddress* addresses,
                        void* client) {
    AdmListenerRegistry::Get().Notify(
        reinterpret_cast<uintptr_t>(client), object,
        std::vector<AdmListenerAddress>(addresses, addresses + count));
    return 0;
  }

  int32_t Register(const Entry& entry) {
    std::lock_guard<std::mutex> lock(mutex_);
    if (std::find(entries_.begin(), entries_.end(), entry) == entries_.end()) {
      entries_.push_back(entry);
    }
    return 0;
  }

  int32_t Unregister(const Entry& entry) {
    std::lock_guard<std::mutex> lock(mutex_);
    const auto found = std::find(entries_.begin(), entries_.end(), entry);
    if (found == entries_.end()) {
      return -1;
    }
    entries_.erase(found);
    return 0;
  }

  mutable std::mutex mutex_;
  std::vector<Entry> entries_;
};

// AudioDeviceMac's device and IOProc bookkeeping (webrtc-sdk m150 89d790b,
// audio_device_mac.cc) over a fake HAL: devices by AudioObjectID, a default
// per direction, and IOProcs that run while started on a device that is
// alive. Waits for an IOProc that never runs are counted, not slept.
class FakeMacAdm {
 public:
  struct Io {
    bool render = false;
    bool capture = false;
  };

  struct State {
    uint32_t running = 0;
    uint32_t running_input = 0;
    uint32_t running_shared = 0;
    bool two_devices = true;
    uint32_t stop_timeouts = 0;
    uint32_t bad_ioproc_calls = 0;
    bool play_init = false;
    bool playing = false;
    bool rec_init = false;
    bool recording = false;
  };

  void SetDevices(std::vector<std::string> playout,
                  std::vector<std::string> recording) {
    std::lock_guard<std::mutex> lock(mutex_);
    playout_guids_ = std::move(playout);
    recording_guids_ = std::move(recording);
  }

  void SetDefault(bool playout, std::string id) {
    std::lock_guard<std::mutex> lock(mutex_);
    (playout ? default_output_ : default_input_) = std::move(id);
  }

  void SetAlive(const std::string& id, bool alive) {
    std::lock_guard<std::mutex> lock(mutex_);
    if (alive) {
      dead_.erase(id);
    } else {
      dead_.insert(id);
    }
  }

  // The default output becomes `id` as the next InitPlayout resolves it.
  void SetDefaultOutputAtNextInitPlayout(std::string id) {
    std::lock_guard<std::mutex> lock(mutex_);
    default_output_at_init_ = std::move(id);
  }

  std::optional<std::string> DeviceFor(bool playout, const std::string& guid) {
    std::lock_guard<std::mutex> lock(mutex_);
    const std::string id =
        guid.empty() ? (playout ? default_output_ : default_input_) : guid;
    if (!AliveLocked(id)) {
      return std::nullopt;
    }
    return id;
  }

  bool Alive(const std::string& id) {
    std::lock_guard<std::mutex> lock(mutex_);
    return AliveLocked(id);
  }

  int32_t SetPlayoutDevice(uint16_t index) {
    std::lock_guard<std::mutex> lock(mutex_);
    if (play_init_ || index >= playout_guids_.size()) {
      return -1;
    }
    output_index_ = index;
    return 0;
  }

  int32_t SetRecordingDevice(uint16_t index) {
    std::lock_guard<std::mutex> lock(mutex_);
    if (rec_init_ || index >= recording_guids_.size()) {
      return -1;
    }
    input_index_ = index;
    return 0;
  }

  int32_t InitSpeaker() {
    std::lock_guard<std::mutex> lock(mutex_);
    return InitSpeakerLocked() ? 0 : -1;
  }

  // The model fails an Init whose device does not resolve; the ADM only
  // warns and fails a little later, reading the stale device's format.
  int32_t InitPlayout() {
    std::lock_guard<std::mutex> lock(mutex_);
    if (playing_) {
      return -1;
    }
    if (play_init_) {
      return 0;
    }
    if (default_output_at_init_) {
      default_output_ = *default_output_at_init_;
      default_output_at_init_.reset();
    }
    if (!InitSpeakerLocked()) {
      return -1;
    }
    if (!mic_mixer_ && !recording_) {
      const std::string input = ResolveLocked(true, input_index_);
      if (!input.empty()) {
        input_id_ = input;
        two_devices_ = input_id_ != output_id_;
      }
    }
    if (two_devices_ || !rec_init_) {
      device_ioproc_ = CreateLocked(output_id_, false);
    }
    play_init_ = true;
    render_alive_ = true;
    return 0;
  }

  int32_t StartPlayout() {
    std::lock_guard<std::mutex> lock(mutex_);
    if (!play_init_) {
      return -1;
    }
    if (playing_) {
      return 0;
    }
    if ((two_devices_ || !recording_) &&
        !StartLocked(output_id_, device_ioproc_)) {
      return -1;
    }
    playing_ = true;
    return 0;
  }

  int32_t StopPlayout() {
    std::lock_guard<std::mutex> lock(mutex_);
    if (!play_init_) {
      return 0;
    }
    if (playing_ && render_alive_) {
      playing_ = false;
      if (!AnyRunningLocked(false)) {
        ++stop_timeouts_;
        StopAndDestroyLocked(output_id_, device_ioproc_);
      } else if (two_devices_ || !recording_) {
        StopAndDestroyLocked(output_id_, device_ioproc_);
      }
    } else if (two_devices_ || !rec_init_) {
      DestroyLocked(output_id_, device_ioproc_);
    }
    render_alive_ = false;
    play_init_ = false;
    playing_ = false;
    return 0;
  }

  int32_t InitRecording() {
    std::lock_guard<std::mutex> lock(mutex_);
    if (recording_) {
      return -1;
    }
    if (rec_init_) {
      return 0;
    }
    if (!InitMicrophoneLocked()) {
      return -1;
    }
    if (!speaker_mixer_ && !playing_) {
      const std::string output = ResolveLocked(false, output_index_);
      if (!output.empty()) {
        output_id_ = output;
        two_devices_ = input_id_ != output_id_;
      }
    }
    if (two_devices_) {
      in_device_ioproc_ = CreateLocked(input_id_, true);
    } else if (!play_init_) {
      device_ioproc_ = CreateLocked(input_id_, false);
    }
    rec_init_ = true;
    capture_alive_ = true;
    return 0;
  }

  int32_t StartRecording() {
    std::lock_guard<std::mutex> lock(mutex_);
    if (!rec_init_) {
      return -1;
    }
    if (recording_) {
      return 0;
    }
    if (two_devices_) {
      if (!StartLocked(input_id_, in_device_ioproc_)) {
        return -1;
      }
    } else if (!playing_ && !StartLocked(input_id_, device_ioproc_)) {
      return -1;
    }
    recording_ = true;
    return 0;
  }

  int32_t StopRecording() {
    std::lock_guard<std::mutex> lock(mutex_);
    if (!rec_init_) {
      return 0;
    }
    if (two_devices_ && capture_alive_) {
      if (recording_) {
        recording_ = false;
        if (!AnyRunningLocked(true)) {
          ++stop_timeouts_;
        }
        StopAndDestroyLocked(input_id_, in_device_ioproc_);
      } else {
        DestroyLocked(input_id_, in_device_ioproc_);
      }
    } else if (recording_ && capture_alive_) {
      recording_ = false;
      if (!AnyRunningLocked(false)) {
        ++stop_timeouts_;
        StopAndDestroyLocked(output_id_, device_ioproc_);
      } else if (two_devices_ || !playing_) {
        StopAndDestroyLocked(output_id_, device_ioproc_);
      }
    } else if (!playing_ && !play_init_) {
      DestroyLocked(output_id_, device_ioproc_);
    }
    capture_alive_ = false;
    rec_init_ = false;
    recording_ = false;
    return 0;
  }

  // One IO cycle: implDeviceIOProc renders while playout plays and captures
  // while _twoDevices is false; implInDeviceIOProc captures.
  Io Cycle() {
    std::lock_guard<std::mutex> lock(mutex_);
    Io io;
    const bool shared = AnyRunningLocked(false);
    io.render = playing_ && shared;
    io.capture =
        recording_ && (AnyRunningLocked(true) || (!two_devices_ && shared));
    return io;
  }

  State Read() {
    std::lock_guard<std::mutex> lock(mutex_);
    State state;
    for (const auto& entry : ioprocs_) {
      if (entry.second.running) {
        ++state.running;
        ++(entry.second.input_only ? state.running_input
                                   : state.running_shared);
      }
    }
    state.two_devices = two_devices_;
    state.stop_timeouts = stop_timeouts_;
    state.bad_ioproc_calls = bad_ioproc_calls_;
    state.play_init = play_init_;
    state.playing = playing_;
    state.rec_init = rec_init_;
    state.recording = recording_;
    return state;
  }

 private:
  struct IoProc {
    std::string device;
    bool input_only = false;
    bool running = false;
  };

  bool AliveLocked(const std::string& id) const {
    if (id.empty() || dead_.count(id) != 0) {
      return false;
    }
    auto listed = [&id](const std::vector<std::string>& guids) {
      return std::find(guids.begin() + std::min<size_t>(1, guids.size()),
                       guids.end(), id) != guids.end();
    };
    return listed(playout_guids_) || listed(recording_guids_);
  }

  // InitDevice: index 0 is the system default, the others the listed ids.
  std::string ResolveLocked(bool input, uint16_t index) const {
    const std::vector<std::string>& guids =
        input ? recording_guids_ : playout_guids_;
    if (index >= guids.size()) {
      return {};
    }
    const std::string id =
        index == 0 ? (input ? default_input_ : default_output_) : guids[index];
    return AliveLocked(id) ? id : std::string();
  }

  bool InitSpeakerLocked() {
    if (playing_) {
      return false;
    }
    const std::string id = ResolveLocked(false, output_index_);
    if (id.empty()) {
      return false;
    }
    output_id_ = id;
    two_devices_ = input_id_ != output_id_;
    speaker_mixer_ = true;
    return true;
  }

  bool InitMicrophoneLocked() {
    if (recording_) {
      return false;
    }
    const std::string id = ResolveLocked(true, input_index_);
    if (id.empty()) {
      return false;
    }
    input_id_ = id;
    two_devices_ = input_id_ != output_id_;
    mic_mixer_ = true;
    return true;
  }

  bool AnyRunningLocked(bool input_only) const {
    for (const auto& entry : ioprocs_) {
      const IoProc& ioproc = entry.second;
      if (ioproc.running && ioproc.input_only == input_only &&
          dead_.count(ioproc.device) == 0) {
        return true;
      }
    }
    return false;
  }

  int CreateLocked(const std::string& device, bool input_only) {
    const int id = next_ioproc_++;
    ioprocs_[id] = IoProc{device, input_only, false};
    return id;
  }

  bool StartLocked(const std::string& device, int id) {
    const auto found = ioprocs_.find(id);
    if (found == ioprocs_.end() || found->second.device != device ||
        !AliveLocked(device)) {
      ++bad_ioproc_calls_;
      return false;
    }
    found->second.running = true;
    return true;
  }

  void StopAndDestroyLocked(const std::string& device, int id) {
    const auto found = ioprocs_.find(id);
    if (found == ioprocs_.end() || found->second.device != device) {
      bad_ioproc_calls_ += 2;
      return;
    }
    ioprocs_.erase(found);
  }

  void DestroyLocked(const std::string& device, int id) {
    const auto found = ioprocs_.find(id);
    if (found == ioprocs_.end() || found->second.device != device) {
      ++bad_ioproc_calls_;
      return;
    }
    ioprocs_.erase(found);
  }

  std::mutex mutex_;
  std::vector<std::string> playout_guids_;
  std::vector<std::string> recording_guids_;
  std::string default_output_;
  std::string default_input_;
  std::optional<std::string> default_output_at_init_;
  std::set<std::string> dead_;
  std::map<int, IoProc> ioprocs_;
  int next_ioproc_ = 1;
  // _deviceIOProcID and _inDeviceIOProcID: whatever was created last, also
  // once destroyed.
  int device_ioproc_ = 0;
  int in_device_ioproc_ = 0;
  // _inputDeviceID, _outputDeviceID and the indices SetRecordingDevice and
  // SetPlayoutDevice stored.
  std::string input_id_;
  std::string output_id_;
  uint16_t input_index_ = 0;
  uint16_t output_index_ = 0;
  bool two_devices_ = true;
  // The mixer manager's speaker and microphone stay open once opened.
  bool speaker_mixer_ = false;
  bool mic_mixer_ = false;
  bool play_init_ = false;
  bool playing_ = false;
  bool rec_init_ = false;
  bool recording_ = false;
  bool render_alive_ = false;
  bool capture_alive_ = false;
  uint32_t stop_timeouts_ = 0;
  uint32_t bad_ioproc_calls_ = 0;
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
    mac_.SetDevices(playout, recording);
    std::lock_guard<std::mutex> lock(mutex_);
    playout_guids_ = std::move(playout);
    recording_guids_ = std::move(recording);
  }

  // Device calls follow AudioDeviceMac's rules from now on (FakeMacAdm).
  void SetMacRules(bool mac) { mac_rules_.store(mac); }
  FakeMacAdm& mac() { return mac_; }

  // One IO cycle under AudioDeviceMac's rules, on the calling thread: the
  // capture and render callbacks its IOProcs would make.
  FakeMacAdm::Io MacCycle() {
    const FakeMacAdm::Io io = mac_.Cycle();
    if (io.capture) {
      if (webrtc::AudioTransport* transport = callback_.load()) {
        std::array<int16_t, kDeviceFrames * 2> samples{};
        uint32_t new_mic_level = 0;
        transport->RecordedDataIsAvailable(samples.data(), kDeviceFrames, 4, 2,
                                           48000, 10, 0, 0, false,
                                           new_mic_level);
      }
    }
    if (io.render) {
      std::vector<int16_t> device(webrtc::kMaxBufferSizeBytes /
                                  sizeof(int16_t));
      buffer_.RequestPlayoutData(kDeviceFrames);
      buffer_.GetPlayoutData(device.data());
    }
    return io;
  }

  FakeCoreAudio& core_audio() { return core_audio_; }
  uint32_t ListenerCalls() const { return listener_calls_.load(); }
  std::vector<std::thread::id> ListenerThreads() {
    std::lock_guard<std::mutex> lock(mutex_);
    return listener_threads_;
  }
  uint32_t OverlappingCalls() const { return overlapping_calls_.load(); }

  static int32_t Listener(uint32_t object,
                          uint32_t count,
                          const AdmListenerAddress* addresses,
                          void* client) {
    auto* adm = static_cast<FakePlatformAdm*>(client);
    for (uint32_t i = 0; i < count; ++i) {
      adm->OnListener(addresses[i]);
    }
    return 0;
  }

  void FailNextStopRecording(int32_t result) {
    next_stop_recording_result_.store(result);
  }
  void FailNextStopPlayout(int32_t result) {
    next_stop_playout_result_.store(result);
  }
  // As the macOS ADM: the start spawns its worker thread, then the device
  // fails to start, so the worker stays until a Stop joins it. A Start while
  // one stays records "StartPlayout over a worker" (libwebrtc's move
  // assignment joins it there and aborts when that join fails).
  void FailNextStartPlayout(int32_t result) {
    next_start_playout_result_.store(result);
  }
  void FailNextStartRecording(int32_t result) {
    next_start_recording_result_.store(result);
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

  // As AudioDeviceMac::Init: the device list and both defaults.
  int32_t Init() override {
    const DeviceCall call(this);
    for (const AdmListenerAddress& address :
         {kFakeDevicesAddress, kFakeDefaultOutputAddress,
          kFakeDefaultInputAddress}) {
      core_audio_.AddListener(kFakeSystemObject, address, &Listener, this);
    }
    return Record("Init");
  }
  int32_t Terminate() override {
    const DeviceCall call(this);
    for (const AdmListenerAddress& address :
         {kFakeDevicesAddress, kFakeDefaultOutputAddress,
          kFakeDefaultInputAddress}) {
      core_audio_.RemoveListener(kFakeSystemObject, address, &Listener, this);
    }
    return Record("Terminate");
  }
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
    const DeviceCall call(this);
    Record("SetPlayoutDevice(" + std::to_string(index) + ")");
    return mac_rules_.load() ? mac_.SetPlayoutDevice(index) : 0;
  }
  int32_t SetPlayoutDevice(WindowsDeviceType) override {
    const DeviceCall call(this);
    Record("SetPlayoutDevice(default)");
    return mac_rules_.load() ? mac_.SetPlayoutDevice(0) : 0;
  }
  int32_t SetRecordingDevice(uint16_t index) override {
    const DeviceCall call(this);
    Record("SetRecordingDevice(" + std::to_string(index) + ")");
    return mac_rules_.load() ? mac_.SetRecordingDevice(index) : 0;
  }
  int32_t SetRecordingDevice(WindowsDeviceType) override {
    const DeviceCall call(this);
    Record("SetRecordingDevice(default)");
    return mac_rules_.load() ? mac_.SetRecordingDevice(0) : 0;
  }

  int32_t InitPlayout() override {
    const DeviceCall call(this);
    if (mac_rules_.load()) {
      Record("InitPlayout");
      const bool was_initialized = playout_initialized_.load();
      const int32_t result = mac_.InitPlayout();
      SyncMac();
      if (!was_initialized && playout_initialized_.load()) {
        core_audio_.AddListener(kFakeOutputDevice, kFakeFormatAddress,
                                &Listener, this);
      }
      buffer_.SetPlayoutSampleRate(48000);
      buffer_.SetPlayoutChannels(stereo_playout_.load() ? 2 : 1);
      return result;
    }
    if (!playout_initialized_.exchange(true)) {
      core_audio_.AddListener(kFakeOutputDevice, kFakeFormatAddress, &Listener,
                              this);
    }
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
    if (mac_rules_.load()) {
      const DeviceCall call(this);
      Record("InitSpeaker");
      return mac_.InitSpeaker();
    }
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
    const DeviceCall call(this);
    if (mac_rules_.load()) {
      Record("StartPlayout");
      const int32_t result = mac_.StartPlayout();
      SyncMac();
      return result;
    }
    Record(playout_worker_left_.exchange(false) ? "StartPlayout over a worker"
                                                : "StartPlayout");
    if (block_start_.exchange(false)) {
      start_entered_.Set();
      start_release_.Wait(webrtc::TimeDelta::Seconds(10));
    }
    if (!playout_initialized_.load()) {
      return -1;
    }
    if (const int32_t failed = next_start_playout_result_.exchange(0)) {
      playout_worker_left_.store(true);
      return failed;
    }
    buffer_.StartPlayout();
    playing_.store(true);
    return 0;
  }
  int32_t StopPlayout() override {
    const DeviceCall call(this);
    Record("StopPlayout");
    if (mac_rules_.load()) {
      const bool was_initialized = playout_initialized_.load();
      const int32_t result = mac_.StopPlayout();
      SyncMac();
      if (was_initialized && !playout_initialized_.load()) {
        core_audio_.RemoveListener(kFakeOutputDevice, kFakeFormatAddress,
                                   &Listener, this);
      }
      return result;
    }
    const bool open = playout_initialized_.exchange(false);
    if (open) {
      core_audio_.RemoveListener(kFakeOutputDevice, kFakeFormatAddress,
                                 &Listener, this);
    }
    playout_worker_left_.store(false);
    playing_.store(false);
    buffer_.StopPlayout();
    return open ? next_stop_playout_result_.exchange(0) : 0;
  }
  bool Playing() const override { return playing_.load(); }

  int32_t InitRecording() override {
    const DeviceCall call(this);
    if (mac_rules_.load()) {
      Record("InitRecording");
      const int32_t result = mac_.InitRecording();
      SyncMac();
      return result;
    }
    recording_initialized_.store(true);
    return Record("InitRecording");
  }
  bool RecordingIsInitialized() const override {
    return recording_initialized_.load();
  }
  int32_t StartRecording() override {
    const DeviceCall call(this);
    if (mac_rules_.load()) {
      Record("StartRecording");
      const int32_t result = mac_.StartRecording();
      SyncMac();
      return result;
    }
    Record(recording_worker_left_.exchange(false) ? "StartRecording over a worker"
                                                  : "StartRecording");
    if (!recording_initialized_.load()) {
      return -1;
    }
    if (const int32_t failed = next_start_recording_result_.exchange(0)) {
      recording_worker_left_.store(true);
      return failed;
    }
    recording_.store(true);
    return 0;
  }
  int32_t StopRecording() override {
    const DeviceCall call(this);
    Record("StopRecording");
    if (mac_rules_.load()) {
      const int32_t result = mac_.StopRecording();
      SyncMac();
      return result;
    }
    const bool open = recording_initialized_.exchange(false);
    recording_worker_left_.store(false);
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
  class DeviceCall {
   public:
    explicit DeviceCall(FakePlatformAdm* adm) : adm_(adm) {
      if (adm_->in_device_call_.fetch_add(1) > 0) {
        adm_->overlapping_calls_.fetch_add(1);
      }
    }
    ~DeviceCall() { adm_->in_device_call_.fetch_sub(1); }

   private:
    FakePlatformAdm* const adm_;
  };

  // AudioDeviceMac's objectListenerProc. Its device-list and default-device
  // handlers restarted devices themselves; the proxy keeps those
  // notifications, so only their arrival is recorded.
  void OnListener(const AdmListenerAddress& address) {
    listener_calls_.fetch_add(1);
    {
      std::lock_guard<std::mutex> lock(mutex_);
      listener_threads_.push_back(std::this_thread::get_id());
    }
    if (address == kFakeFormatAddress) {
      Record("listener: format");
    } else if (address == kFakeDefaultOutputAddress) {
      Record("listener: default output");
    } else if (address == kFakeDefaultInputAddress) {
      Record("listener: default input");
    } else {
      Record("listener: devices");
    }
  }

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

  void SyncMac() {
    const FakeMacAdm::State state = mac_.Read();
    playout_initialized_.store(state.play_init);
    recording_initialized_.store(state.rec_init);
    recording_.store(state.recording);
    if (playing_.exchange(state.playing) != state.playing) {
      if (state.playing) {
        buffer_.StartPlayout();
      } else {
        buffer_.StopPlayout();
      }
    }
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
  std::atomic<int32_t> next_start_playout_result_{0};
  std::atomic<int32_t> next_start_recording_result_{0};
  std::atomic<bool> playout_worker_left_{false};
  std::atomic<bool> recording_worker_left_{false};
  std::atomic<uint16_t> playout_delay_ms_{0};
  std::atomic<bool> block_start_{false};
  webrtc::Event start_entered_;
  webrtc::Event start_release_;
  FakeCoreAudio core_audio_;
  std::atomic<uint32_t> listener_calls_{0};
  std::vector<std::thread::id> listener_threads_;
  std::atomic<uint32_t> in_device_call_{0};
  std::atomic<uint32_t> overlapping_calls_{0};
  std::atomic<bool> mac_rules_{false};
  FakeMacAdm mac_;
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
  proxy_->SetDeviceFormatReaderForTest(nullptr, false);
  proxy_->SetDeviceProbeForTest(AdmDeviceProbe{});
  proxy_->SetRenderStallGuardForTest(false, false, 0, 0);
  proxy_->SetCaptureState(capture_);
  proxy_->SetPlayoutOpenedSink(
      [log = opened_](const AdmPlayoutOpened& opened) {
        webrtc::MutexLock lock(&log->mutex);
        log->entries.push_back(!opened.open ? std::string("closed")
                               : opened.system_default
                                   ? "default|" + opened.name
                                   : opened.guid + "|" + opened.name);
      });
  controller_ = std::make_unique<AudioDeviceController>(nullptr, proxy_, mixer_,
                                                        apm_, capture_);
}

rust::Vec<rust::String> AdmProxyHarness::take_playout_opened() const {
  rust::Vec<rust::String> entries;
  webrtc::MutexLock lock(&opened_->mutex);
  for (const std::string& entry : opened_->entries) {
    entries.push_back(rust::String(entry));
  }
  opened_->entries.clear();
  return entries;
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

void AdmProxyHarness::set_device_format(bool playout,
                                        rust::Str guid,
                                        uint32_t rate,
                                        uint32_t channels) const {
  const std::string device =
      guid.empty() ? std::string("default") : std::string(guid);
  bool install = false;
  {
    webrtc::MutexLock lock(&formats_->mutex);
    AdmDeviceFormat format;
    format.device = device;
    format.sample_rate = rate;
    format.channels = channels;
    format.bytes_per_frame = channels * 4;
    formats_->formats[{playout, device}] = format;
    install = !std::exchange(formats_->installed, true);
  }
  if (install) {
    proxy_->SetDeviceFormatReaderForTest(
        [formats = formats_](bool playout, const std::string& guid)
            -> std::optional<AdmDeviceFormat> {
          webrtc::MutexLock lock(&formats->mutex);
          const auto found = formats->formats.find(
              {playout, guid.empty() ? std::string("default") : guid});
          if (found == formats->formats.end()) {
            return std::nullopt;
          }
          return found->second;
        },
        false);
  }
}

void AdmProxyHarness::poll_device_formats() const {
  proxy_->PollDeviceFormatsForTest();
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

void AdmProxyHarness::fail_next_start_playout(int32_t result) const {
  fake_->FailNextStartPlayout(result);
}

void AdmProxyHarness::fail_next_start_recording(int32_t result) const {
  fake_->FailNextStartRecording(result);
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

void AdmProxyHarness::engine_terminate() const {
  proxy_->StopPlayout();
  proxy_->StopRecording();
  proxy_->RegisterAudioCallback(nullptr);
  proxy_->Terminate();
}

void AdmProxyHarness::shutdown() const {
  proxy_->ShutdownForTest();
}

void AdmProxyHarness::notify_devices_changed() const {
  fake_->core_audio().Notify(kFakeSystemObject, kFakeDevicesAddress);
}

void AdmProxyHarness::notify_format_changed() const {
  fake_->core_audio().Notify(kFakeOutputDevice, kFakeFormatAddress);
}

void AdmProxyHarness::notify_default_output_changed() const {
  fake_->core_audio().Notify(kFakeSystemObject, kFakeDefaultOutputAddress);
}

void AdmProxyHarness::notify_default_input_changed() const {
  fake_->core_audio().Notify(kFakeSystemObject, kFakeDefaultInputAddress);
}

void AdmProxyHarness::mac_rules(bool probe) const {
  fake_->SetMacRules(true);
  if (!probe) {
    return;
  }
  AdmDeviceProbe device_probe;
  device_probe.identify = [fake = fake_](bool playout, const std::string& guid)
      -> std::optional<AdmDeviceIdentity> {
    const std::optional<std::string> id = fake->mac().DeviceFor(playout, guid);
    if (!id) {
      return std::nullopt;
    }
    return AdmDeviceIdentity{*id, "UID-" + *id};
  };
  device_probe.alive = [fake = fake_](const std::string& id) {
    return fake->mac().Alive(id);
  };
  proxy_->SetDeviceProbeForTest(std::move(device_probe));
}

void AdmProxyHarness::set_render_stall_guard(uint32_t stall_ms,
                                             uint32_t min_interval_ms) const {
  proxy_->SetRenderStallGuardForTest(true, false, static_cast<int>(stall_ms),
                                     static_cast<int>(min_interval_ms));
}

void AdmProxyHarness::poll_render_stall() const {
  proxy_->PollRenderStallForTest();
}

void AdmProxyHarness::set_default_device(bool playout, rust::Str id) const {
  fake_->mac().SetDefault(playout, std::string(id));
}

void AdmProxyHarness::set_device_alive(rust::Str id, bool alive) const {
  fake_->mac().SetAlive(std::string(id), alive);
}

void AdmProxyHarness::set_default_output_at_next_playout_open(
    rust::Str id) const {
  fake_->mac().SetDefaultOutputAtNextInitPlayout(std::string(id));
}

AdmMacIo AdmProxyHarness::mac_io(uint32_t cycles) const {
  AdmMacIo result{};
  std::thread device_thread([this, cycles, &result] {
    for (uint32_t i = 0; i < cycles; ++i) {
      const FakeMacAdm::Io io = fake_->MacCycle();
      result.render_callbacks += io.render ? 1 : 0;
      result.capture_callbacks += io.capture ? 1 : 0;
    }
  });
  device_thread.join();
  return result;
}

AdmMacState AdmProxyHarness::mac_state() const {
  const FakeMacAdm::State state = fake_->mac().Read();
  AdmMacState result{};
  result.running_ioprocs = state.running;
  result.running_input_ioprocs = state.running_input;
  result.running_shared_ioprocs = state.running_shared;
  result.two_devices = state.two_devices;
  result.stop_timeouts = state.stop_timeouts;
  result.bad_ioproc_calls = state.bad_ioproc_calls;
  return result;
}

uint32_t AdmProxyHarness::listener_calls() const {
  return fake_->ListenerCalls();
}

bool AdmProxyHarness::listeners_ran_on_device_thread() const {
  std::thread::id device;
  proxy_->RunOnDeviceThreadForTest(
      [&device] { device = std::this_thread::get_id(); });
  const std::vector<std::thread::id> threads = fake_->ListenerThreads();
  return !threads.empty() &&
         std::all_of(threads.begin(), threads.end(),
                     [&device](std::thread::id id) { return id == device; });
}

uint32_t AdmProxyHarness::overlapping_device_calls() const {
  return fake_->OverlappingCalls();
}

uint32_t AdmProxyHarness::core_audio_listeners() const {
  return fake_->core_audio().Count();
}

uint64_t AdmProxyHarness::routed_listeners() const {
  return proxy_->RoutedListenersForTest();
}

uint64_t AdmProxyHarness::delivered_listener_notifications() const {
  return proxy_->DeliveredListenerNotificationsForTest();
}

bool AdmProxyHarness::add_listener_from_test_thread(bool adm_listener) const {
  if (adm_listener) {
    fake_->core_audio().AddListener(kFakeOutputDevice, kFakeFormatAddress,
                                    &FakePlatformAdm::Listener, fake_.get());
    return !fake_->core_audio().HasOriginal(
        kFakeOutputDevice, kFakeFormatAddress, &FakePlatformAdm::Listener,
        fake_.get());
  }
  fake_->core_audio().AddListener(kFakeSystemObject, kFakeDevicesAddress,
                                  &CountingListener, foreign_calls_.get());
  return !fake_->core_audio().HasOriginal(
      kFakeSystemObject, kFakeDevicesAddress, &CountingListener,
      foreign_calls_.get());
}

void AdmProxyHarness::remove_listener_from_test_thread(
    bool adm_listener) const {
  if (adm_listener) {
    fake_->core_audio().RemoveListener(kFakeOutputDevice, kFakeFormatAddress,
                                       &FakePlatformAdm::Listener, fake_.get());
    return;
  }
  fake_->core_audio().RemoveListener(kFakeSystemObject, kFakeDevicesAddress,
                                     &CountingListener, foreign_calls_.get());
}

uint32_t AdmProxyHarness::foreign_listener_calls() const {
  return foreign_calls_->load();
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
  state.speech_detection = capture_->speech_detection.load();
  state.auto_sensitivity_minimum_db =
      capture_->auto_sensitivity_minimum_db.load();
  state.voice_gain_max_db = capture_->voice_gain_max_db.load();
  state.voice_gain_target_db = capture_->voice_gain_target_db.load();
  state.voice_gain_rise_db_per_second =
      capture_->voice_gain_rise_db_per_second.load();
  state.voice_gain_fall_db_per_second =
      capture_->voice_gain_fall_db_per_second.load();
  state.voice_gain_level_time_constant_ms =
      capture_->voice_gain_level_time_constant_ms.load();
  state.voice_gain_min_speech_probability =
      capture_->voice_gain_min_speech_probability.load();
  state.gain_engine = capture_->gain_engine.load();
  state.agc2_headroom_db = capture_->agc2_headroom_db.load();
  state.agc2_max_gain_db = capture_->agc2_max_gain_db.load();
  state.agc2_max_gain_change_db_per_second =
      capture_->agc2_max_gain_change_db_per_second.load();
  state.agc2_max_output_noise_dbfs =
      capture_->agc2_max_output_noise_dbfs.load();
  state.speech_lookahead_ms = capture_->speech_lookahead_ms.load();
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
  proxy_->SetDeviceFormatReaderForTest(nullptr, false);
  proxy_->SetDeviceProbeForTest(AdmDeviceProbe{});
  proxy_->SetRenderStallGuardForTest(false, false, 0, 0);
  proxy_->SetCaptureState(capture_);
  controller_ = std::make_unique<AudioDeviceController>(nullptr, proxy_, mixer_,
                                                        apm_, capture_);
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
    counts.silent_calls = fake->silent_calls();
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

namespace {

class NoVideoEncoderFactory : public webrtc::VideoEncoderFactory {
 public:
  std::vector<webrtc::SdpVideoFormat> GetSupportedFormats() const override {
    return {};
  }
  std::unique_ptr<webrtc::VideoEncoder> Create(
      const webrtc::Environment&,
      const webrtc::SdpVideoFormat&) override {
    return nullptr;
  }
};

class NoVideoDecoderFactory : public webrtc::VideoDecoderFactory {
 public:
  std::vector<webrtc::SdpVideoFormat> GetSupportedFormats() const override {
    return {};
  }
  std::unique_ptr<webrtc::VideoDecoder> Create(
      const webrtc::Environment&,
      const webrtc::SdpVideoFormat&) override {
    return nullptr;
  }
};

}  // namespace

struct RootEngineHarness::Parts {
  std::unique_ptr<webrtc::Thread> network;
  std::unique_ptr<webrtc::Thread> worker;
  std::unique_ptr<webrtc::Thread> signaling;
  webrtc::scoped_refptr<FakePlatformAdm> fake;
  webrtc::scoped_refptr<RootAudioMixer> mixer;
  std::shared_ptr<RootCaptureState> capture;
  webrtc::scoped_refptr<RootApmDecorator> apm;
  webrtc::scoped_refptr<AdmProxy> proxy;
  webrtc::scoped_refptr<webrtc::ConnectionContext> context;
  std::shared_ptr<RootVoiceEngineHold> hold;
  std::unique_ptr<AudioDeviceController> controller;
  // Worker thread only.
  std::vector<std::unique_ptr<webrtc::ConnectionContext::MediaEngineReference>>
      peer_connections;
};

// Wired as PeerConnectionFactory wires it, with fakes for the platform ADM
// and the video codecs.
RootEngineHarness::RootEngineHarness() : parts_(std::make_unique<Parts>()) {
  Parts& p = *parts_;
  p.network = webrtc::Thread::CreateWithSocketServer();
  p.network->SetName("engine-network", nullptr);
  p.network->Start();
  p.worker = webrtc::Thread::Create();
  p.worker->SetName("engine-worker", nullptr);
  p.worker->Start();
  p.signaling = webrtc::Thread::Create();
  p.signaling->SetName("engine-signaling", nullptr);
  p.signaling->Start();

  const webrtc::Environment env = webrtc::CreateEnvironment();
  p.fake = webrtc::make_ref_counted<FakePlatformAdm>();
  p.fake->SetDevices({"{spk}"}, {"{mic}"});
  p.mixer = webrtc::make_ref_counted<RootAudioMixer>();
  p.capture = std::make_shared<RootCaptureState>();
  if constexpr (kRootDenoiserSupported) {
    p.capture->denoiser_slot = std::make_shared<RootDenoiserSlot>();
  }
  p.capture->monitor = std::make_shared<RootAudioMonitor>();
  p.apm = CreateRootAudioProcessing(env, p.capture, p.worker.get());
  p.proxy = p.worker->BlockingCall([&] {
    return webrtc::make_ref_counted<AdmProxy>(env, p.worker.get(), p.mixer,
                                              p.fake);
  });
  p.proxy->SetDeviceFormatReaderForTest(nullptr, false);
  p.proxy->SetDeviceProbeForTest(AdmDeviceProbe{});
  p.proxy->SetRenderStallGuardForTest(false, false, 0, 0);
  p.proxy->SetCaptureState(p.capture);

  webrtc::PeerConnectionFactoryDependencies dependencies;
  dependencies.network_thread = p.network.get();
  dependencies.worker_thread = p.worker.get();
  dependencies.signaling_thread = p.signaling.get();
  dependencies.socket_factory = p.network->socketserver();
  dependencies.adm = p.proxy;
  dependencies.audio_mixer = p.mixer;
  dependencies.audio_encoder_factory =
      webrtc::CreateBuiltinAudioEncoderFactory();
  dependencies.audio_decoder_factory =
      webrtc::CreateBuiltinAudioDecoderFactory();
  dependencies.video_encoder_factory =
      std::make_unique<NoVideoEncoderFactory>();
  dependencies.video_decoder_factory =
      std::make_unique<NoVideoDecoderFactory>();
  dependencies.audio_processing_builder = webrtc::CustomAudioProcessing(p.apm);
  webrtc::EnableMedia(dependencies);
  p.context = p.signaling->BlockingCall([&] {
    return webrtc::ConnectionContext::Create(env, &dependencies);
  });
  p.hold = std::make_shared<RootVoiceEngineHold>(p.worker.get(), p.context);
  p.controller = std::make_unique<AudioDeviceController>(
      nullptr, p.proxy, p.mixer, p.apm, p.capture, p.hold);
}

RootEngineHarness::~RootEngineHarness() {
  Parts& p = *parts_;
  p.controller = nullptr;
  p.worker->BlockingCall([&p] { p.peer_connections.clear(); });
  p.hold->Shutdown();
  p.hold = nullptr;
  p.signaling->BlockingCall([&p] { p.context = nullptr; });
  // The media engine is destroyed on the worker after the context.
  p.worker->BlockingCall([] {});
  p.proxy = nullptr;
  p.signaling->Stop();
  p.worker->Stop();
  p.network->Stop();
  p.apm = nullptr;
}

const AudioDeviceController& RootEngineHarness::controller() const {
  return *parts_->controller;
}

bool RootEngineHarness::acquire() const {
  Parts& p = *parts_;
  if (!p.controller->acquire_platform_adm()) {
    return false;
  }
  p.controller->set_adm_recording_enabled(true);
  p.controller->set_adm_playout_enabled(true);
  p.controller->reset_root_audio(false);
  flush();
  return true;
}

void RootEngineHarness::release() const {
  parts_->controller->reset_root_audio(true);
  parts_->controller->release_platform_adm();
  flush();
}

void RootEngineHarness::open_peer_connection() const {
  Parts& p = *parts_;
  p.worker->BlockingCall([&p] {
    p.peer_connections.push_back(
        std::make_unique<webrtc::ConnectionContext::MediaEngineReference>(
            p.context));
  });
  flush();
}

void RootEngineHarness::close_peer_connection() const {
  Parts& p = *parts_;
  p.worker->BlockingCall([&p] {
    if (!p.peer_connections.empty()) {
      p.peer_connections.pop_back();
    }
  });
  flush();
}

void RootEngineHarness::engine_playout(bool start) const {
  if (start) {
    parts_->proxy->StartPlayout();
  } else {
    parts_->proxy->StopPlayout();
  }
  flush();
}

void RootEngineHarness::engine_recording(bool start) const {
  if (start) {
    parts_->proxy->StartRecording();
  } else {
    parts_->proxy->StopRecording();
  }
  flush();
}

void RootEngineHarness::hold(bool hold) const {
  parts_->controller->set_playout_hold(hold);
  flush();
}

uint8_t RootEngineHarness::monitor(bool enabled) const {
  const uint8_t result = parts_->controller->set_root_audio_monitor(enabled);
  flush();
  return result;
}

void RootEngineHarness::flush() const {
  parts_->worker->BlockingCall([] {});
  parts_->proxy->FlushDeviceTasksForTest();
}

void RootEngineHarness::capture(int32_t rate,
                                size_t channels,
                                rust::Slice<const int16_t> input) const {
  webrtc::AudioTransport* device = parts_->fake->callback();
  if (device == nullptr || rate < 100 || channels == 0) {
    return;
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
}

rust::Vec<int16_t> RootEngineHarness::render(int32_t rate,
                                             size_t channels) const {
  rust::Vec<int16_t> result;
  webrtc::AudioTransport* device = parts_->fake->callback();
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

bool RootEngineHarness::platform_playing() const {
  return parts_->fake->Playing();
}

bool RootEngineHarness::platform_recording() const {
  return parts_->fake->Recording();
}

bool RootEngineHarness::synthetic_playing() const {
  return parts_->proxy->SyntheticPlayingForTest();
}

rust::Vec<rust::String> RootEngineHarness::take_calls() const {
  rust::Vec<rust::String> calls;
  for (const std::string& call : parts_->fake->TakeCalls()) {
    calls.push_back(rust::String(call));
  }
  return calls;
}

std::unique_ptr<RootEngineHarness> new_root_engine_harness() {
  return std::make_unique<RootEngineHarness>();
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
