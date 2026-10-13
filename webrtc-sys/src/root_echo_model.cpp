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

#include "livekit/root_echo_model.h"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdlib>
#include <utility>

#include "api/audio/builtin_audio_processing_builder.h"
#include "api/audio/echo_canceller3_config.h"
#include "api/environment/environment_factory.h"
#include "api/make_ref_counted.h"
#include "livekit/root_audio_processing.h"
#include "modules/audio_processing/aec3/block.h"

#if defined(WEBRTC_WIN)
#include <windows.h>

#include <cfgmgr32.h>
#include <mmdeviceapi.h>
#include <propidl.h>
#endif

#if defined(WEBRTC_MAC) && !defined(WEBRTC_IOS)
#include <CoreAudio/CoreAudio.h>
#include <CoreFoundation/CoreFoundation.h>
#endif

namespace livekit_ffi {
namespace {

constexpr uint32_t FourCc(const char (&s)[5]) {
  return (static_cast<uint32_t>(static_cast<unsigned char>(s[0])) << 24) |
         (static_cast<uint32_t>(static_cast<unsigned char>(s[1])) << 16) |
         (static_cast<uint32_t>(static_cast<unsigned char>(s[2])) << 8) |
         static_cast<uint32_t>(static_cast<unsigned char>(s[3]));
}

void RaiseTo(std::atomic<uint64_t>& maximum, uint64_t value) {
  uint64_t current = maximum.load(std::memory_order_relaxed);
  while (value > current &&
         !maximum.compare_exchange_weak(current, value,
                                        std::memory_order_relaxed)) {
  }
}

RootOutputDeviceKind KindFromMacTerminal(uint32_t terminal) {
  if (terminal == FourCc("spkr") || terminal == FourCc("lfes")) {
    return RootOutputDeviceKind::kSpeakers;
  }
  if (terminal == FourCc("line")) {
    return RootOutputDeviceKind::kLineOut;
  }
  if (terminal == FourCc("hdph")) {
    return RootOutputDeviceKind::kHeadphones;
  }
  if (terminal == FourCc("rspk")) {
    return RootOutputDeviceKind::kHandset;
  }
  if (terminal == FourCc("hdmi") || terminal == FourCc("dprt")) {
    return RootOutputDeviceKind::kDisplayAudio;
  }
  return RootOutputDeviceKind::kUnknown;
}

#if defined(WEBRTC_WIN)

std::wstring Wide(const std::string& utf8) {
  if (utf8.empty()) {
    return std::wstring();
  }
  const int n = MultiByteToWideChar(CP_UTF8, 0, utf8.data(),
                                    static_cast<int>(utf8.size()), nullptr, 0);
  std::wstring wide(static_cast<size_t>(std::max(n, 0)), L'\0');
  if (n > 0) {
    MultiByteToWideChar(CP_UTF8, 0, utf8.data(), static_cast<int>(utf8.size()),
                        wide.data(), n);
  }
  return wide;
}

std::string Utf8(const wchar_t* wide) {
  if (wide == nullptr || *wide == L'\0') {
    return std::string();
  }
  const int n =
      WideCharToMultiByte(CP_UTF8, 0, wide, -1, nullptr, 0, nullptr, nullptr);
  if (n <= 1) {
    return std::string();
  }
  std::string utf8(static_cast<size_t>(n), '\0');
  WideCharToMultiByte(CP_UTF8, 0, wide, -1, utf8.data(), n, nullptr, nullptr);
  utf8.resize(static_cast<size_t>(n - 1));
  return utf8;
}

// PKEY_AudioEndpoint_FormFactor.
const PROPERTYKEY kFormFactorKey = {
    {0x1da5d803,
     0xd492,
     0x4edd,
     {0x8c, 0x23, 0xe0, 0xc0, 0xff, 0xee, 0x7f, 0x0e}},
    0};

// The endpoint's devnode is SWD\MMDEVAPI\<endpoint id>; its ancestors name the
// bus the audio device sits on.
bool EndpointIsBluetooth(const std::wstring& endpoint_id) {
  std::wstring instance = L"SWD\\MMDEVAPI\\" + endpoint_id;
  DEVINST node = 0;
  if (CM_Locate_DevNodeW(&node, instance.data(), CM_LOCATE_DEVNODE_NORMAL) !=
      CR_SUCCESS) {
    return false;
  }
  for (int depth = 0; depth < 4; ++depth) {
    DEVINST parent = 0;
    if (CM_Get_Parent(&parent, node, 0) != CR_SUCCESS) {
      return false;
    }
    wchar_t id[MAX_DEVICE_ID_LEN] = {};
    if (CM_Get_Device_IDW(parent, id, MAX_DEVICE_ID_LEN, 0) != CR_SUCCESS) {
      return false;
    }
    if (RootIsBluetoothInstanceId(Utf8(id))) {
      return true;
    }
    node = parent;
  }
  return false;
}

RootOutputDevice ClassifyWindows(const RootPlayoutIdentity& identity) {
  RootOutputDevice result{identity.guid, RootOutputDeviceKind::kUnknown};
  IMMDeviceEnumerator* enumerator = nullptr;
  if (FAILED(CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr,
                              CLSCTX_INPROC_SERVER,
                              __uuidof(IMMDeviceEnumerator),
                              reinterpret_cast<void**>(&enumerator))) ||
      enumerator == nullptr) {
    return result;
  }
  IMMDevice* device = nullptr;
  HRESULT hr = E_FAIL;
  if (identity.system_default || identity.guid.empty()) {
    hr = enumerator->GetDefaultAudioEndpoint(eRender, eConsole, &device);
  } else {
    hr = enumerator->GetDevice(Wide(identity.guid).c_str(), &device);
  }
  if (SUCCEEDED(hr) && device != nullptr) {
    LPWSTR id = nullptr;
    std::wstring endpoint_id;
    if (SUCCEEDED(device->GetId(&id)) && id != nullptr) {
      endpoint_id = id;
      result.key = Utf8(id);
      CoTaskMemFree(id);
    }
    uint32_t form_factor = 10;
    IPropertyStore* store = nullptr;
    if (SUCCEEDED(device->OpenPropertyStore(STGM_READ, &store)) &&
        store != nullptr) {
      PROPVARIANT value;
      PropVariantInit(&value);
      if (SUCCEEDED(store->GetValue(kFormFactorKey, &value)) &&
          value.vt == VT_UI4) {
        form_factor = value.ulVal;
      }
      PropVariantClear(&value);
      store->Release();
    }
    result.kind = RootOutputKindFromWindows(
        form_factor, !endpoint_id.empty() && EndpointIsBluetooth(endpoint_id));
    device->Release();
  }
  enumerator->Release();
  return result;
}

#endif  // WEBRTC_WIN

#if defined(WEBRTC_MAC) && !defined(WEBRTC_IOS)

template <typename T>
std::optional<T> MacProperty(AudioObjectID object,
                             AudioObjectPropertySelector selector,
                             AudioObjectPropertyScope scope) {
  const AudioObjectPropertyAddress address = {selector, scope,
                                              kAudioObjectPropertyElementMain};
  if (!AudioObjectHasProperty(object, &address)) {
    return std::nullopt;
  }
  T value{};
  UInt32 size = static_cast<UInt32>(sizeof(T));
  if (AudioObjectGetPropertyData(object, &address, 0, nullptr, &size,
                                 &value) != noErr ||
      size != sizeof(T)) {
    return std::nullopt;
  }
  return value;
}

std::string MacDeviceUid(AudioObjectID device) {
  std::optional<CFStringRef> uid = MacProperty<CFStringRef>(
      device, kAudioDevicePropertyDeviceUID, kAudioObjectPropertyScopeGlobal);
  if (!uid.has_value() || *uid == nullptr) {
    return std::string();
  }
  char buffer[512] = {};
  const bool ok = CFStringGetCString(*uid, buffer, sizeof(buffer),
                                     kCFStringEncodingUTF8);
  CFRelease(*uid);
  return ok ? std::string(buffer) : std::string();
}

std::optional<uint32_t> MacTerminalType(AudioObjectID device) {
  const AudioObjectPropertyAddress address = {
      kAudioDevicePropertyStreams, kAudioObjectPropertyScopeOutput,
      kAudioObjectPropertyElementMain};
  UInt32 size = 0;
  if (!AudioObjectHasProperty(device, &address) ||
      AudioObjectGetPropertyDataSize(device, &address, 0, nullptr, &size) !=
          noErr ||
      size < sizeof(AudioStreamID)) {
    return std::nullopt;
  }
  std::vector<AudioStreamID> streams(size / sizeof(AudioStreamID));
  size = static_cast<UInt32>(streams.size() * sizeof(AudioStreamID));
  if (AudioObjectGetPropertyData(device, &address, 0, nullptr, &size,
                                 streams.data()) != noErr ||
      size < sizeof(AudioStreamID)) {
    return std::nullopt;
  }
  return MacProperty<UInt32>(streams[0], kAudioStreamPropertyTerminalType,
                             kAudioObjectPropertyScopeGlobal);
}

RootOutputDevice ClassifyMac(const RootPlayoutIdentity& identity) {
  RootOutputDevice result{identity.guid, RootOutputDeviceKind::kUnknown};
  AudioObjectID device = kAudioObjectUnknown;
  if (identity.system_default || identity.guid.empty() ||
      identity.guid == "default") {
    device = MacProperty<AudioObjectID>(kAudioObjectSystemObject,
                                        kAudioHardwarePropertyDefaultOutputDevice,
                                        kAudioObjectPropertyScopeGlobal)
                 .value_or(kAudioObjectUnknown);
  } else {
    char* end = nullptr;
    const unsigned long id = std::strtoul(identity.guid.c_str(), &end, 10);
    if (end != nullptr && *end == '\0') {
      device = static_cast<AudioObjectID>(id);
    }
  }
  if (device == kAudioObjectUnknown) {
    return result;
  }
  const std::string uid = MacDeviceUid(device);
  result.key = uid.empty() ? std::to_string(device) : uid;
  const uint32_t transport =
      MacProperty<UInt32>(device, kAudioDevicePropertyTransportType,
                          kAudioObjectPropertyScopeGlobal)
          .value_or(0);
  const std::optional<uint32_t> data_source = MacProperty<UInt32>(
      device, kAudioDevicePropertyDataSource, kAudioObjectPropertyScopeOutput);
  if (data_source.has_value()) {
    result.key += "/" + std::to_string(*data_source);
  }
  result.kind =
      RootOutputKindFromMac(transport, data_source, MacTerminalType(device));
  return result;
}

#endif  // WEBRTC_MAC && !WEBRTC_IOS

}  // namespace

bool root_echo_model_supported() {
  return kRootEchoModelSupported;
}

RootEchoModelMode RootEchoModelModeFromWire(uint32_t value) {
  switch (value) {
    case 1:
      return RootEchoModelMode::kOn;
    case 2:
      return RootEchoModelMode::kAuto;
    default:
      return RootEchoModelMode::kOff;
  }
}

bool RootOutputKindIsLoudspeaker(RootOutputDeviceKind kind) {
  return kind == RootOutputDeviceKind::kSpeakers ||
         kind == RootOutputDeviceKind::kLineOut ||
         kind == RootOutputDeviceKind::kDisplayAudio;
}

RootOutputDeviceKind RootOutputKindFromWindows(uint32_t form_factor,
                                               bool bluetooth) {
  if (bluetooth) {
    return RootOutputDeviceKind::kBluetooth;
  }
  switch (form_factor) {
    case 1:
      return RootOutputDeviceKind::kSpeakers;
    case 2:
      return RootOutputDeviceKind::kLineOut;
    case 3:
      return RootOutputDeviceKind::kHeadphones;
    case 5:
      return RootOutputDeviceKind::kHeadset;
    case 6:
      return RootOutputDeviceKind::kHandset;
    case 9:
      return RootOutputDeviceKind::kDisplayAudio;
    default:
      return RootOutputDeviceKind::kUnknown;
  }
}

bool RootIsBluetoothInstanceId(std::string_view instance_id) {
  if (instance_id.size() < 3) {
    return false;
  }
  auto upper = [](char c) {
    return c >= 'a' && c <= 'z' ? static_cast<char>(c - 'a' + 'A') : c;
  };
  return upper(instance_id[0]) == 'B' && upper(instance_id[1]) == 'T' &&
         upper(instance_id[2]) == 'H';
}

RootOutputDeviceKind RootOutputKindFromMac(uint32_t transport,
                                           std::optional<uint32_t> data_source,
                                           std::optional<uint32_t> terminal) {
  if (transport == FourCc("blue") || transport == FourCc("blea")) {
    return RootOutputDeviceKind::kBluetooth;
  }
  if (transport == FourCc("hdmi") || transport == FourCc("dprt")) {
    return RootOutputDeviceKind::kDisplayAudio;
  }
  if (transport == 0 || transport == FourCc("airp") ||
      transport == FourCc("virt") || transport == FourCc("grup") ||
      transport == FourCc("fgrp") || transport == FourCc("ccwd") ||
      transport == FourCc("ccwl")) {
    return RootOutputDeviceKind::kUnknown;
  }
  if (transport == FourCc("bltn") && data_source.has_value()) {
    if (*data_source == FourCc("ispk") || *data_source == FourCc("espk")) {
      return RootOutputDeviceKind::kSpeakers;
    }
    if (*data_source == FourCc("hdpn")) {
      return RootOutputDeviceKind::kHeadphones;
    }
    if (*data_source == FourCc("line")) {
      return RootOutputDeviceKind::kLineOut;
    }
  }
  return terminal.has_value() ? KindFromMacTerminal(*terminal)
                              : RootOutputDeviceKind::kUnknown;
}

RootOutputDevice ClassifyOutputDevice(const RootPlayoutIdentity& identity) {
#if defined(WEBRTC_WIN)
  return ClassifyWindows(identity);
#elif defined(WEBRTC_MAC) && !defined(WEBRTC_IOS)
  return ClassifyMac(identity);
#else
  RootOutputDevice result;
  if (!identity.name.empty()) {
    result.key = identity.system_default ? "default:" + identity.name
                                         : identity.name;
  }
  result.kind = RootOutputDeviceKind::kUnknown;
  return result;
#endif
}

// =============================================================================
// RootInferenceWindow
// =============================================================================

const std::array<uint32_t, RootInferenceWindow::kBuckets - 1>
    RootInferenceWindow::kBucketBounds = {
        50,   75,   100,  125,  150,  175,   200,   250,   300,   350,  400,
        500,  600,  700,  800,  900,  1000,  1250,  1500,  1750,  2000, 2500,
        3000, 4000, 5000, 7500, 10000, 15000, 20000, 30000, 50000};

void RootInferenceWindow::StartOver() {
  count_ = 0;
  slow_ = 0;
  failing_streak_ = 0;
  max_us_ = 0;
  histogram_.fill(0);
}

void RootInferenceWindow::Add(uint64_t us, RootEchoModelCounters& counters) {
  constexpr auto kRelaxed = std::memory_order_relaxed;
  const uint64_t session = counters.session.load(kRelaxed);
  if (session != session_) {
    session_ = session;
    StartOver();
  }
  counters.inference_us_total.fetch_add(us, kRelaxed);
  counters.inferences.fetch_add(1, kRelaxed);
  RaiseTo(counters.inference_max_us, us);
  if (warmup_ > 0) {
    --warmup_;
    return;
  }
  size_t bucket = 0;
  while (bucket < kBucketBounds.size() && us > kBucketBounds[bucket]) {
    ++bucket;
  }
  ++histogram_[bucket];
  max_us_ = std::max(max_us_, us);
  if (us > kSlowUs) {
    ++slow_;
  }
  if (++count_ < kWindow) {
    return;
  }
  constexpr uint32_t kRank = kWindow - kAllowedSlow;
  uint32_t seen = 0;
  float p99 = static_cast<float>(max_us_);
  for (size_t b = 0; b < histogram_.size(); ++b) {
    seen += histogram_[b];
    if (seen >= kRank) {
      p99 = b < kBucketBounds.size()
                ? static_cast<float>(std::min<uint64_t>(kBucketBounds[b], max_us_))
                : static_cast<float>(max_us_);
      break;
    }
  }
  last_p99_us_ = p99;
  counters.inference_p99_us.store(p99, kRelaxed);
  failing_streak_ = slow_ > kAllowedSlow ? failing_streak_ + 1 : 0;
  if (failing_streak_ >= kFailingWindows) {
    counters.kill_switch.store(true, kRelaxed);
  }
  count_ = 0;
  slow_ = 0;
  max_us_ = 0;
  histogram_.fill(0);
}

// =============================================================================
// RootResidualEchoEstimator
// =============================================================================

RootResidualEchoEstimator::RootResidualEchoEstimator(
    rust::Box<RootEchoModelSession> session,
    std::shared_ptr<RootEchoModelCounters> counters)
    : session_(std::move(session)),
      counters_(counters ? std::move(counters)
                         : std::make_shared<RootEchoModelCounters>()) {}

RootResidualEchoEstimator::~RootResidualEchoEstimator() = default;

void RootResidualEchoEstimator::Estimate(
    const webrtc::Block& render,
    std::span<const std::array<float, 64>> /* y */,
    std::span<const std::array<float, 64>> e,
    std::span<const std::array<float, 65>> /* S2 */,
    std::span<const std::array<float, 65>> /* Y2 */,
    std::span<const std::array<float, 65>> E2,
    bool /* dominant_nearend */,
    std::span<std::array<float, 65>> R2,
    std::span<std::array<float, 65>> R2_unbounded) {
  std::array<float, 64> reference{};
  const int render_channels = render.NumChannels();
  if (render.NumBands() > 0 && render_channels > 0) {
    for (int channel = 0; channel < render_channels; ++channel) {
      const auto view = render.View(0, channel);
      for (size_t i = 0; i < reference.size(); ++i) {
        reference[i] += view[i];
      }
    }
    const float scale = 1.0f / static_cast<float>(render_channels);
    for (float& v : reference) {
      v *= scale;
    }
  }
  std::array<float, 64> linear{};
  if (!e.empty()) {
    for (const auto& channel : e) {
      for (size_t i = 0; i < linear.size(); ++i) {
        linear[i] += channel[i];
      }
    }
    const float scale = 1.0f / static_cast<float>(e.size());
    for (float& v : linear) {
      v *= scale;
    }
  }
  std::array<float, 65> fraction{};
  const auto start = std::chrono::steady_clock::now();
  const bool inferred = session_->estimate_block(
      rust::Slice<const float>(reference.data(), reference.size()),
      rust::Slice<const float>(linear.data(), linear.size()),
      rust::Slice<float>(fraction.data(), fraction.size()));
  if (inferred) {
    const auto elapsed = std::chrono::steady_clock::now() - start;
    const int64_t us =
        std::chrono::duration_cast<std::chrono::microseconds>(elapsed).count();
    window_.Add(static_cast<uint64_t>(std::max<int64_t>(us, 0)), *counters_);
  }
  const size_t channels = std::min({E2.size(), R2.size(), R2_unbounded.size()});
  for (size_t channel = 0; channel < channels; ++channel) {
    for (size_t k = 0; k < fraction.size(); ++k) {
      R2[channel][k] = E2[channel][k] * fraction[k];
    }
    R2_unbounded[channel] = R2[channel];
  }
}

webrtc::EchoCanceller3Config::Suppressor RootResidualEchoEstimator::Tuned(
    const webrtc::EchoCanceller3Config::Suppressor& config) {
  using Suppressor = webrtc::EchoCanceller3Config::Suppressor;
  Suppressor tuned = config;
  const Suppressor::MaskingThresholds masks(0.0f, 1.0f, 0.3f);
  const Suppressor::Tuning tuning(masks, masks, 100.0f, 0.0f);
  tuned.nearend_average_blocks = 1;
  tuned.normal_tuning = tuning;
  tuned.nearend_tuning = tuning;
  tuned.dominant_nearend_detection.enr_threshold = 0.5f;
  tuned.dominant_nearend_detection.trigger_threshold = 2;
  tuned.high_frequency_suppression.limiting_gain_band = 24;
  tuned.high_frequency_suppression.bands_in_limiting_gain = 3;
  return tuned;
}

webrtc::EchoCanceller3Config::Suppressor RootResidualEchoEstimator::AdjustConfig(
    const webrtc::EchoCanceller3Config::Suppressor& config) const {
  return Tuned(config);
}

void RootResidualEchoEstimator::Reset() {
  session_->reset();
}

std::unique_ptr<webrtc::NeuralResidualEchoEstimator> NewRootResidualEchoEstimator(
    std::shared_ptr<RootEchoModelCounters> counters) {
  if (!kRootEchoModelSupported || !root_echo_model_prepared()) {
    return nullptr;
  }
  rust::Box<RootEchoModelSession> session = new_root_echo_model_session();
  if (!session->valid()) {
    return nullptr;
  }
  return std::make_unique<RootResidualEchoEstimator>(std::move(session),
                                                     std::move(counters));
}

// =============================================================================
// RootEchoModelPolicy
// =============================================================================

void RootEchoModelPolicy::SetDevice(RootOutputDevice device) {
  if (device.key == device_.key && device.kind == device_.kind) {
    return;
  }
  device_ = std::move(device);
  RestartMeasurement();
}

void RootEchoModelPolicy::ResetSession() {
  mode_ = RootEchoModelMode::kOff;
  kill_switch_ = false;
  unavailable_ = false;
  latched_.clear();
  RestartMeasurement();
}

void RootEchoModelPolicy::ForgetDevice() {
  SetDevice(RootOutputDevice{});
}

void RootEchoModelPolicy::RestartMeasurement() {
  window_.fill(false);
  next_ = 0;
  counted_ = 0;
  last_frames_.reset();
}

bool RootEchoModelPolicy::measured_echo() const {
  return std::find(latched_.begin(), latched_.end(), device_.key) !=
         latched_.end();
}

size_t RootEchoModelPolicy::evidence() const {
  return static_cast<size_t>(std::count(window_.begin(), window_.end(), true));
}

bool RootEchoModelPolicy::Measuring() const {
  return mode_ == RootEchoModelMode::kAuto && !kill_switch_ && !unavailable_ &&
         !device_.key.empty() && !measured_echo() &&
         !RootOutputKindIsLoudspeaker(device_.kind);
}

bool RootEchoModelPolicy::AddErleSample(double erle_db,
                                        uint64_t processed_frames) {
  if (!Measuring()) {
    return false;
  }
  if (!last_frames_.has_value() ||
      processed_frames < *last_frames_ + kMinFramesPerSample) {
    if (!last_frames_.has_value() || processed_frames < *last_frames_) {
      last_frames_ = processed_frames;
    }
    return false;
  }
  last_frames_ = processed_frames;
  window_[next_] = std::isfinite(erle_db) && erle_db >= kEvidenceErleDb;
  next_ = (next_ + 1) % window_.size();
  counted_ = std::min(counted_ + 1, window_.size());
  if (evidence() < kEvidenceSamples) {
    return false;
  }
  if (latched_.size() >= kMaxLatchedDevices) {
    latched_.erase(latched_.begin());
  }
  latched_.push_back(device_.key);
  RestartMeasurement();
  return true;
}

RootEchoModelPolicy::Decision RootEchoModelPolicy::Decide() const {
  if (mode_ == RootEchoModelMode::kOff) {
    return {false, RootEchoModelReason::kModeOff};
  }
  if (kill_switch_) {
    return {false, RootEchoModelReason::kKillSwitch};
  }
  if (unavailable_) {
    return {false, RootEchoModelReason::kUnavailable};
  }
  if (mode_ == RootEchoModelMode::kOn) {
    return {true, RootEchoModelReason::kModeOn};
  }
  if (measured_echo()) {
    return {true, RootEchoModelReason::kMeasuredEcho};
  }
  return {RootOutputKindIsLoudspeaker(device_.kind),
          RootEchoModelReason::kDeviceKind};
}

// =============================================================================
// RootEchoModelController
// =============================================================================

RootEchoModelController::RootEchoModelController(
    RootApmDecorator* decorator,
    Scheduler scheduler,
    Clock clock,
    std::shared_ptr<RootCaptureState> capture_state,
    Prepare prepare)
    : decorator_(decorator),
      scheduler_(std::move(scheduler)),
      clock_(std::move(clock)),
      capture_(std::move(capture_state)),
      prepare_(std::move(prepare)) {}

RootEchoModelController::~RootEchoModelController() {
  if (prepare_thread_.joinable()) {
    prepare_thread_.join();
  }
}

void RootEchoModelController::Schedule(int64_t delay_ms) {
  const uint64_t generation = ++generation_;
  scheduler_(delay_ms,
             [decorator = webrtc::scoped_refptr<RootApmDecorator>(decorator_),
              generation]() mutable {
               if (RootEchoModelController* controller =
                       decorator->echo_model()) {
                 controller->RunGeneration(generation);
               }
             });
}

void RootEchoModelController::RunGeneration(uint64_t generation) {
  webrtc::MutexLock lock(&mutex_);
  if (generation != generation_) {
    return;
  }
  RunLocked(clock_());
}

void RootEchoModelController::Run() {
  webrtc::MutexLock lock(&mutex_);
  RunLocked(clock_());
}

void RootEchoModelController::SetMode(RootEchoModelMode mode) {
  webrtc::MutexLock lock(&mutex_);
  policy_.SetMode(mode);
  mode_.store(static_cast<uint32_t>(mode), std::memory_order_relaxed);
  Schedule(0);
}

void RootEchoModelController::OnPlayoutOpened(
    const RootPlayoutIdentity& identity) {
  OnOutputDevice(ClassifyOutputDevice(identity));
}

void RootEchoModelController::OnOutputDevice(RootOutputDevice device) {
  webrtc::MutexLock lock(&mutex_);
  playout_open_ = true;
  kind_.store(static_cast<uint32_t>(device.kind), std::memory_order_relaxed);
  policy_.SetDevice(std::move(device));
  Schedule(0);
}

void RootEchoModelController::OnPlayoutClosed() {
  webrtc::MutexLock lock(&mutex_);
  playout_open_ = false;
}

void RootEchoModelController::ResetSession() {
  webrtc::MutexLock lock(&mutex_);
  policy_.ResetSession();
  if (!playout_open_) {
    policy_.ForgetDevice();
    kind_.store(static_cast<uint32_t>(RootOutputDeviceKind::kNone),
                std::memory_order_relaxed);
  }
  counters_->kill_switch.store(false, std::memory_order_relaxed);
  counters_->inference_p99_us.store(std::numeric_limits<float>::quiet_NaN(),
                                    std::memory_order_relaxed);
  counters_->session.fetch_add(1, std::memory_order_relaxed);
  mode_.store(static_cast<uint32_t>(RootEchoModelMode::kOff),
              std::memory_order_relaxed);
  change_since_ms_.reset();
  Schedule(0);
}

void RootEchoModelController::StartPrepareLocked() {
  if (prepare_started_) {
    return;
  }
  prepare_started_ = true;
  // The runs poll every kSwapPollMs while the model is wanted and not
  // running, so they see the result without this thread scheduling one.
  prepare_thread_ = std::thread([this] {
    const bool ok = prepare_ ? prepare_() : false;
    (ok ? prepared_ : prepare_failed_).store(true, std::memory_order_release);
  });
}

void RootEchoModelController::RunLocked(int64_t now_ms) {
  if (counters_->kill_switch.load(std::memory_order_relaxed)) {
    policy_.SetKillSwitch();
  }
  if (prepare_failed_.load(std::memory_order_acquire)) {
    policy_.SetUnavailable(true);
  }
  bool running = decorator_->running_model();
  const uint64_t swaps = decorator_->swaps();
  if (swaps != seen_swaps_) {
    seen_swaps_ = swaps;
    policy_.RestartMeasurement();
  }
  if (policy_.Measuring() && !running &&
      now_ms - last_sample_ms_ >= RootEchoModelPolicy::kSampleIntervalMs) {
    last_sample_ms_ = now_ms;
    const RootApmStats stats = decorator_->ReadStats();
    const uint64_t frames =
        capture_ ? capture_->processed_frames.load(std::memory_order_relaxed)
                 : 0;
    policy_.AddErleSample(stats.erle_db, frames);
  }
  const RootEchoModelPolicy::Decision decision = DecideLocked();
  if (decision.on) {
    StartPrepareLocked();
  }
  const bool prepared =
      prepared_.load(std::memory_order_acquire) || root_echo_model_prepared();
  std::optional<bool> staged = decorator_->staged_variant();
  if (staged.has_value() && *staged != decision.on) {
    decorator_->CancelStaged();
    staged.reset();
  }
  bool collected = decorator_->CollectRetired();
  running = decorator_->running_model();
  if (decision.on == running) {
    change_since_ms_.reset();
  } else if (!staged.has_value() && collected) {
    if (!change_since_ms_.has_value()) {
      change_since_ms_ = now_ms;
    }
    const bool pause = !capture_ || !capture_->AudiblySpeaking() ||
                       now_ms >= *change_since_ms_ + kSpeechPauseMaxMs;
    if (pause && (!decision.on || prepared)) {
      if (decorator_->StageVariant(decision.on)) {
        staged = decision.on;
      } else if (decision.on) {
        policy_.SetUnavailable(true);
      }
    }
  }
  if (staged.has_value() && decorator_->SwitchIfIdle()) {
    staged.reset();
    collected = decorator_->CollectRetired();
  }
  const bool now_running = decorator_->running_model();
  const RootEchoModelPolicy::Decision settled = DecideLocked();
  target_on_.store(settled.on, std::memory_order_relaxed);
  reason_.store(static_cast<uint32_t>(settled.reason), std::memory_order_relaxed);
  measured_.store(policy_.measured_echo(), std::memory_order_relaxed);
  evidence_.store(static_cast<uint32_t>(policy_.evidence()),
                  std::memory_order_relaxed);
  const bool switching = staged.has_value() || !collected ||
                         settled.on != now_running;
  if (switching) {
    Schedule(kSwapPollMs);
  } else if (policy_.mode() != RootEchoModelMode::kOff || now_running) {
    Schedule(kPollMs);
  }
}

RootEchoModelPolicy::Decision RootEchoModelController::DecideLocked() const {
  RootEchoModelPolicy::Decision decision = policy_.Decide();
  if (decision.on && !decorator_->echo_cancellation_active()) {
    decision = {false, RootEchoModelReason::kUnavailable};
  }
  return decision;
}

RootEchoModelStatsSnapshot RootEchoModelController::ReadStats() {
  constexpr auto kRelaxed = std::memory_order_relaxed;
  RootEchoModelStatsSnapshot stats;
  stats.mode = static_cast<RootEchoModelMode>(mode_.load(kRelaxed));
  const bool running = decorator_->running_model();
  const bool target = target_on_.load(kRelaxed);
  if (running) {
    stats.state = target ? RootEchoModelState::kOn : RootEchoModelState::kStopping;
  } else {
    stats.state = target ? RootEchoModelState::kStarting : RootEchoModelState::kOff;
  }
  stats.reason = static_cast<RootEchoModelReason>(reason_.load(kRelaxed));
  stats.kind = static_cast<RootOutputDeviceKind>(kind_.load(kRelaxed));
  stats.swaps = decorator_->swaps();
  stats.measured_echo = measured_.load(kRelaxed);
  stats.kill_switch = counters_->kill_switch.load(kRelaxed);
  stats.inference_p99_us = counters_->inference_p99_us.load(kRelaxed);
  stats.inference_max_us = counters_->inference_max_us.exchange(0, kRelaxed);
  webrtc::MutexLock lock(&read_mutex_);
  const uint64_t inferences = counters_->inferences.load(kRelaxed);
  const uint64_t total = counters_->inference_us_total.load(kRelaxed);
  stats.inferences = inferences;
  if (inferences > read_inferences_) {
    stats.inference_mean_us =
        static_cast<float>(static_cast<double>(total - read_us_total_) /
                           static_cast<double>(inferences - read_inferences_));
  }
  read_inferences_ = inferences;
  read_us_total_ = total;
  return stats;
}

// =============================================================================
// Test hooks
// =============================================================================

uint32_t root_output_kind_from_windows(uint32_t form_factor, bool bluetooth) {
  return static_cast<uint32_t>(RootOutputKindFromWindows(form_factor, bluetooth));
}

bool root_is_bluetooth_instance_id(rust::Str instance_id) {
  return RootIsBluetoothInstanceId(
      std::string_view(instance_id.data(), instance_id.size()));
}

uint32_t root_output_kind_from_mac(uint32_t transport,
                                   bool has_data_source,
                                   uint32_t data_source,
                                   bool has_terminal,
                                   uint32_t terminal) {
  return static_cast<uint32_t>(RootOutputKindFromMac(
      transport,
      has_data_source ? std::optional<uint32_t>(data_source) : std::nullopt,
      has_terminal ? std::optional<uint32_t>(terminal) : std::nullopt));
}

RootEchoPolicyProbe root_echo_policy_probe(uint32_t mode,
                                           uint32_t kind,
                                           rust::Slice<const double> erle_db,
                                           uint64_t frames_per_sample) {
  RootEchoModelPolicy policy;
  policy.SetMode(RootEchoModelModeFromWire(mode));
  policy.SetDevice({"device", static_cast<RootOutputDeviceKind>(kind)});
  RootEchoPolicyProbe probe{};
  probe.latched_at = -1;
  uint64_t frames = 0;
  for (size_t i = 0; i < erle_db.size(); ++i) {
    frames += frames_per_sample;
    if (policy.AddErleSample(erle_db[i], frames) && probe.latched_at < 0) {
      probe.latched_at = static_cast<int64_t>(i);
    }
  }
  const RootEchoModelPolicy::Decision decision = policy.Decide();
  probe.on = decision.on;
  probe.reason = static_cast<uint32_t>(decision.reason);
  probe.measured_echo = policy.measured_echo();
  probe.evidence = static_cast<uint32_t>(policy.evidence());
  probe.counted = static_cast<uint32_t>(policy.counted());
  return probe;
}

RootEchoWindowProbe root_echo_window_probe(rust::Slice<const uint64_t> us,
                                           int64_t reset_at) {
  RootEchoModelCounters counters;
  RootInferenceWindow window;
  RootEchoWindowProbe probe{};
  probe.tripped_at = -1;
  for (size_t i = 0; i < us.size(); ++i) {
    if (static_cast<int64_t>(i) == reset_at) {
      counters.kill_switch.store(false);
      counters.session.fetch_add(1);
    }
    window.Add(us[i], counters);
    if (probe.tripped_at < 0 && counters.kill_switch.load()) {
      probe.tripped_at = static_cast<int64_t>(i);
    }
  }
  probe.p99_us = counters.inference_p99_us.load();
  probe.failing_streak = window.failing_streak();
  return probe;
}

RootEchoHarness::RootEchoHarness(bool today)
    : now_ms_(std::make_shared<int64_t>(1'000'000)),
      tasks_(std::make_shared<std::vector<Task>>()),
      capture_(std::make_shared<RootCaptureState>()) {
  const webrtc::Environment env = webrtc::CreateEnvironment();
  auto now = now_ms_;
  auto tasks = tasks_;
  RootApmDecorator::Scheduler scheduler =
      [now, tasks](int64_t delay_ms, absl::AnyInvocable<void() &&> task) {
        tasks->push_back(Task{*now + delay_ms, std::move(task)});
      };
  RootApmDecorator::Clock clock = [now] { return *now; };
  if (today) {
    webrtc::AudioProcessing::Config config;
    config.high_pass_filter.enabled = true;
    webrtc::BuiltinAudioProcessingBuilder builder(config);
    builder.SetCapturePostProcessing(
        std::make_unique<RootCaptureProcessor>(capture_));
    decorator_ = webrtc::make_ref_counted<RootApmDecorator>(
        builder.Build(env), std::move(scheduler), std::move(clock), capture_);
  } else {
    auto processor = std::make_shared<RootCaptureProcessor>(capture_);
    decorator_ = webrtc::make_ref_counted<RootApmDecorator>(
        BuildRootApmVariant(env, processor, nullptr), std::move(scheduler),
        std::move(clock), capture_, NewRootVariantBuilder(env, processor));
  }
  // As the voice engine does for a device track with echo cancellation.
  webrtc::AudioProcessing::Config config = decorator_->GetConfig();
  config.echo_canceller.enabled = true;
  decorator_->ApplyConfig(config);
}

RootEchoHarness::~RootEchoHarness() {
  tasks_->clear();
  decorator_ = nullptr;
}

void RootEchoHarness::RunDue() const {
  for (int round = 0; round < 8; ++round) {
    std::vector<absl::AnyInvocable<void() &&>> due;
    for (auto it = tasks_->begin(); it != tasks_->end();) {
      if (it->due_ms <= *now_ms_) {
        due.push_back(std::move(it->task));
        it = tasks_->erase(it);
      } else {
        ++it;
      }
    }
    if (due.empty()) {
      return;
    }
    for (auto& task : due) {
      std::move(task)();
    }
  }
}

rust::Vec<float> RootEchoHarness::process(
    int32_t rate,
    uint32_t render_channels,
    rust::Slice<const float> render,
    rust::Slice<const float> capture) const {
  // The float interface takes -1..1.
  constexpr float kScale = 1.0f / 32768.0f;
  const size_t frames = static_cast<size_t>(rate / 100);
  const size_t channels = std::max<uint32_t>(render_channels, 1);
  std::vector<float> render_data(frames * channels, 0.0f);
  for (size_t i = 0; i < std::min(render.size(), render_data.size()); ++i) {
    render_data[i] = render[i] * kScale;
  }
  std::vector<float*> render_ptrs(channels);
  for (size_t c = 0; c < channels; ++c) {
    render_ptrs[c] = render_data.data() + c * frames;
  }
  const webrtc::StreamConfig render_config(rate, channels);
  decorator_->ProcessReverseStream(render_ptrs.data(), render_config,
                                   render_config, render_ptrs.data());
  rust::Vec<float> out;
  if (capture.empty()) {
    return out;
  }
  std::vector<float> capture_data(frames, 0.0f);
  for (size_t i = 0; i < std::min(capture.size(), frames); ++i) {
    capture_data[i] = capture[i] * kScale;
  }
  float* capture_ptr = capture_data.data();
  const webrtc::StreamConfig capture_config(rate, 1);
  decorator_->set_stream_delay_ms(0);
  decorator_->ProcessStream(&capture_ptr, capture_config, capture_config,
                            &capture_ptr);
  for (float v : capture_data) {
    out.push_back(v * 32768.0f);
  }
  return out;
}

rust::Vec<float> RootEchoHarness::process_int16(
    int32_t rate,
    uint32_t render_channels,
    rust::Slice<const float> render,
    rust::Slice<const float> capture) const {
  auto to_int16 = [](float v) {
    return static_cast<int16_t>(std::clamp(std::lround(v), -32768L, 32767L));
  };
  const size_t frames = static_cast<size_t>(rate / 100);
  const size_t channels = std::max<uint32_t>(render_channels, 1);
  std::vector<int16_t> render_data(frames * channels, 0);
  for (size_t c = 0; c < channels; ++c) {
    for (size_t i = 0; i < frames && c * frames + i < render.size(); ++i) {
      render_data[i * channels + c] = to_int16(render[c * frames + i]);
    }
  }
  const webrtc::StreamConfig render_config(rate, channels);
  decorator_->ProcessReverseStream(render_data.data(), render_config,
                                   render_config, render_data.data());
  rust::Vec<float> out;
  if (capture.empty()) {
    return out;
  }
  std::vector<int16_t> capture_data(frames, 0);
  for (size_t i = 0; i < std::min(capture.size(), frames); ++i) {
    capture_data[i] = to_int16(capture[i]);
  }
  const webrtc::StreamConfig capture_config(rate, 1);
  decorator_->set_stream_delay_ms(0);
  decorator_->ProcessStream(capture_data.data(), capture_config,
                            capture_config, capture_data.data());
  for (int16_t v : capture_data) {
    out.push_back(static_cast<float>(v));
  }
  return out;
}

void RootEchoHarness::initialize(int32_t rate, uint32_t render_channels) const {
  const webrtc::StreamConfig capture(rate, 1);
  const webrtc::StreamConfig render(rate,
                                    std::max<uint32_t>(render_channels, 1));
  decorator_->Initialize(
      webrtc::ProcessingConfig{{capture, capture, render, render}});
}

void RootEchoHarness::open_dsp_chain() const {
  capture_->auto_gain.store(false);
  capture_->auto_sensitivity.store(false);
  capture_->gate_threshold.store(0.0f);
}

void RootEchoHarness::prepare_model() const {
  root_echo_model_prepare();
}

void RootEchoHarness::set_mode(uint32_t mode) const {
  if (RootEchoModelController* echo = decorator_->echo_model()) {
    echo->SetMode(RootEchoModelModeFromWire(mode));
  }
}

void RootEchoHarness::set_device(uint32_t kind, rust::Str key) const {
  if (RootEchoModelController* echo = decorator_->echo_model()) {
    echo->OnOutputDevice({std::string(key.data(), key.size()),
                          static_cast<RootOutputDeviceKind>(kind)});
  }
}

void RootEchoHarness::close_playout() const {
  if (RootEchoModelController* echo = decorator_->echo_model()) {
    echo->OnPlayoutClosed();
  }
}

void RootEchoHarness::set_echo_cancellation(bool on) const {
  decorator_->SetPolicy(on, std::nullopt);
}

void RootEchoHarness::reset_session() const {
  if (RootEchoModelController* echo = decorator_->echo_model()) {
    echo->ResetSession();
  }
}

void RootEchoHarness::trip_kill_switch() const {
  if (RootEchoModelController* echo = decorator_->echo_model()) {
    echo->counters()->kill_switch.store(true);
  }
}

void RootEchoHarness::set_speaking(bool speaking) const {
  const uint64_t transitions = capture_->ReadSpeaking().seq + 1;
  capture_->last_frame_us.store(
      std::chrono::duration_cast<std::chrono::microseconds>(
          std::chrono::steady_clock::now().time_since_epoch())
          .count());
  capture_->speaking_word.store((transitions << 1) | (speaking ? 1u : 0u));
}

void RootEchoHarness::advance(int64_t ms) const {
  for (int64_t step = 0; step < ms; step += 10) {
    *now_ms_ += std::min<int64_t>(10, ms - step);
    RunDue();
  }
  if (ms == 0) {
    RunDue();
  }
}

RootEchoHarnessProbe RootEchoHarness::probe() const {
  RootEchoHarnessProbe probe{};
  probe.running_model = decorator_->running_model();
  const std::optional<bool> staged = decorator_->staged_variant();
  probe.staged = staged.has_value() ? (*staged ? 1 : 0) : -1;
  probe.swaps = decorator_->swaps();
  if (RootEchoModelController* echo = decorator_->echo_model()) {
    const RootEchoModelStatsSnapshot stats = echo->ReadStats();
    probe.has_controller = true;
    probe.mode = static_cast<uint32_t>(stats.mode);
    probe.state = static_cast<uint32_t>(stats.state);
    probe.reason = static_cast<uint32_t>(stats.reason);
    probe.kind = static_cast<uint32_t>(stats.kind);
    probe.inferences = stats.inferences;
    probe.inference_mean_us = stats.inference_mean_us;
    probe.inference_max_us = stats.inference_max_us;
    probe.inference_p99_us = stats.inference_p99_us;
    probe.measured_echo = stats.measured_echo;
    probe.kill_switch = stats.kill_switch;
    probe.evidence = echo->evidence();
  }
  const RootDspStatsSnapshot dsp = capture_->ReadDspStats(false);
  probe.processed_frames = dsp.processed_frames;
  probe.dsp_resets = dsp.dsp_resets;
  probe.dsp_flushes = dsp.dsp_flushes;
  probe.format_seq = capture_->format_seq.load();
  probe.erle_db = decorator_->ReadStats().erle_db;
  probe.pending_tasks = static_cast<uint32_t>(tasks_->size());
  return probe;
}

RootEchoStressProbe RootEchoHarness::stress(uint32_t switches) const {
  constexpr int kRate = 48000;
  constexpr size_t kFrames = kRate / 100;
  std::atomic<bool> stop{false};
  std::atomic<uint64_t> render_frames{0};
  std::atomic<uint64_t> capture_frames{0};
  webrtc::scoped_refptr<RootApmDecorator> decorator = decorator_;
  std::thread render([&] {
    std::vector<float> left(kFrames), right(kFrames);
    float* channels[2] = {left.data(), right.data()};
    const webrtc::StreamConfig config(kRate, 2);
    uint64_t n = 0;
    while (!stop.load()) {
      for (size_t i = 0; i < kFrames; ++i) {
        left[i] = right[i] = 0.1f * std::sin(0.05f * static_cast<float>(n * kFrames + i));
      }
      decorator->ProcessReverseStream(channels, config, config, channels);
      render_frames.fetch_add(1);
      ++n;
    }
  });
  std::thread capture([&] {
    std::vector<float> mic(kFrames);
    float* channel = mic.data();
    const webrtc::StreamConfig config(kRate, 1);
    uint64_t n = 0;
    while (!stop.load()) {
      for (size_t i = 0; i < kFrames; ++i) {
        mic[i] = 0.03f * std::sin(0.05f * static_cast<float>(n * kFrames + i) + 1.0f);
      }
      decorator->set_stream_delay_ms(10);
      decorator->ProcessStream(&channel, config, config, &channel);
      capture_frames.fetch_add(1);
      ++n;
    }
  });
  std::atomic<uint64_t> reads{0};
  std::thread reader([&] {
    while (!stop.load()) {
      decorator->ReadStats();
      if (decorator->proc_sample_rate_hz() < 0) {
        break;
      }
      reads.fetch_add(1);
    }
  });
  RootEchoStressProbe probe{};
  const uint64_t swaps_before = decorator->swaps();
  for (uint32_t s = 0; s < switches; ++s) {
    const bool model = !decorator->running_model();
    while (!decorator->CollectRetired()) {
      std::this_thread::yield();
    }
    if (!decorator->StageVariant(model)) {
      probe.failed_stages += 1;
      continue;
    }
    const auto deadline =
        std::chrono::steady_clock::now() + std::chrono::seconds(5);
    while (decorator->staged_variant().has_value() &&
           std::chrono::steady_clock::now() < deadline) {
      std::this_thread::yield();
    }
  }
  while (!decorator->CollectRetired()) {
    std::this_thread::yield();
  }
  stop.store(true);
  render.join();
  capture.join();
  reader.join();
  probe.swaps = decorator->swaps() - swaps_before;
  probe.render_frames = render_frames.load();
  probe.capture_frames = capture_frames.load();
  probe.reads = reads.load();
  probe.collected = decorator->CollectRetired();
  return probe;
}

RootEchoGraceProbe RootEchoHarness::grace_replay() const {
  RootApmDecorator& d = *decorator_;
  auto live = [&d](webrtc::AudioProcessing* apm) {
    webrtc::MutexLock lock(&d.mutex_);
    return std::any_of(
        d.live_.begin(), d.live_.end(),
        [apm](const auto& variant) { return variant->apm.get() == apm; });
  };
  auto swap = [&d] { return d.StageVariant(false) && d.SwitchIfIdle(); };
  RootEchoGraceProbe probe{};
  webrtc::AudioProcessing* first = d.active_for_test();
  const uint32_t epoch = d.GraceEpoch();
  probe.first_switched = swap();
  webrtc::AudioProcessing* second = d.active_for_test();
  probe.first_released = d.CollectRetired() && !live(first);
  const RootApmDecorator::Entered entered = d.GraceEnter(epoch);
  probe.reader_holds_second = entered.apm == second;
  probe.reader_parity_moved = entered.parity != (epoch & 1u);
  probe.second_switched = swap();
  probe.second_held = !d.CollectRetired() && live(second);
  probe.held_usable = entered.apm->num_output_channels() > 0;
  d.GraceLeave(entered.parity);
  probe.released_after_leave = d.CollectRetired() && !live(second);
  return probe;
}

std::unique_ptr<RootEchoHarness> new_root_echo_harness(bool today) {
  return std::make_unique<RootEchoHarness>(today);
}

bool root_echo_adjust_config_is_googles_tuning() {
  const webrtc::EchoCanceller3Config config;
  const auto tuned = RootResidualEchoEstimator::Tuned(config.suppressor);
  auto masks_are = [](const auto& m, float t, float s, float e) {
    return m.enr_transparent == t && m.enr_suppress == s &&
           m.emr_transparent == e;
  };
  return masks_are(tuned.normal_tuning.mask_lf, 0.0f, 1.0f, 0.3f) &&
         masks_are(tuned.normal_tuning.mask_hf, 0.0f, 1.0f, 0.3f) &&
         masks_are(tuned.nearend_tuning.mask_lf, 0.0f, 1.0f, 0.3f) &&
         masks_are(tuned.nearend_tuning.mask_hf, 0.0f, 1.0f, 0.3f) &&
         tuned.normal_tuning.max_inc_factor == 100.0f &&
         tuned.normal_tuning.max_dec_factor_lf == 0.0f &&
         tuned.nearend_average_blocks == 1 &&
         tuned.dominant_nearend_detection.enr_threshold == 0.5f &&
         tuned.dominant_nearend_detection.trigger_threshold == 2 &&
         tuned.high_frequency_suppression.limiting_gain_band == 24 &&
         tuned.high_frequency_suppression.bands_in_limiting_gain == 3 &&
         config.suppressor.normal_tuning.mask_lf.enr_transparent == 0.3f;
}

}  // namespace livekit_ffi
