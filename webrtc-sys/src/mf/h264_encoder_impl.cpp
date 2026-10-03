/*
 * Copyright 2025 LiveKit, Inc.
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

#include "h264_encoder_impl.h"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <deque>
#include <functional>
#include <limits>
#include <mutex>
#include <string>
#include <utility>

#include <common_video/h264/h264_common.h>
#include "api/array_view.h"
#include "api/task_queue/task_queue_base.h"
#include "common_video/libyuv/include/webrtc_libyuv.h"
#include "mf_common.h"
#include "modules/video_coding/include/video_codec_interface.h"
#include "modules/video_coding/include/video_error_codes.h"
#include "modules/video_coding/utility/simulcast_rate_allocator.h"
#include "rtc_base/checks.h"
#include "rtc_base/logging.h"
#include "system_wrappers/include/metrics.h"
#include "third_party/libyuv/include/libyuv/convert_from.h"

namespace webrtc {

using livekit_ffi::ComPtr;
using livekit_ffi::HResultToString;

namespace {

// Used by histograms. Values of entries should not be changed.
enum H264EncoderImplEvent {
  kH264EncoderEventInit = 0,
  kH264EncoderEventError = 1,
  kH264EncoderEventMax = 16,
};

// Async hardware MFTs post METransformNeedInput almost immediately, but the
// very first request after initialization can lag while the driver spins up.
constexpr int kNeedInputTimeoutMs = 500;
constexpr int kOutputWaitTimeoutMs = 500;
// How many frames may be in flight inside the MFT before Encode() blocks
// waiting for output. Low-latency mode keeps this near 1 in practice.
constexpr size_t kMaxPendingFrames = 4;
// Upper bound on pooled D3D11 input textures; the MFT normally holds at most
// kMaxPendingFrames of them, the rest is headroom for drivers that release
// input samples late.
constexpr size_t kMaxInputTextures = 16;

// Every runtime hardware failure asks the VideoEncoderSoftwareFallbackWrapper
// that SimulcastEncoderAdapter puts around this encoder to continue in
// software. ENCODER_FAILURE instead would make webrtc request a codec switch,
// which cannot recover when H.264 is the only negotiated codec.
constexpr int32_t kHardwareFailure = WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE;

// Test-only fault injection, read once per process:
//   LK_MF_FAULT_INIT=1           InitEncode fails.
//   LK_MF_FAULT_AFTER_FRAMES=N   ProcessInput reports DXGI_ERROR_DEVICE_REMOVED
//                                from the (N+1)th frame after InitEncode.
//   LK_MF_MAX_SESSIONS=N         InitEncode fails while N MF encoder sessions
//                                are open (simulates NVENC session limits).
struct FaultInjection {
  bool fail_init = false;
  std::optional<int64_t> fail_after_frames;
  std::optional<int64_t> max_sessions;
};

const FaultInjection& Faults() {
  static const FaultInjection faults = [] {
    FaultInjection f;
    f.fail_init = livekit_ffi::EnvFlagSet("LK_MF_FAULT_INIT");
    f.fail_after_frames = livekit_ffi::GetEnvInt("LK_MF_FAULT_AFTER_FRAMES");
    f.max_sessions = livekit_ffi::GetEnvInt("LK_MF_MAX_SESSIONS");
    if (f.fail_init || f.fail_after_frames || f.max_sessions) {
      RTC_LOG(LS_WARNING) << "MF encoder fault injection active: init="
                          << f.fail_init << " after_frames="
                          << f.fail_after_frames.value_or(-1)
                          << " max_sessions=" << f.max_sessions.value_or(-1);
    }
    return f;
  }();
  return faults;
}

std::atomic<int> g_open_sessions{0};

HRESULT SetCodecApiUInt32(ICodecAPI* api, const GUID& guid, UINT32 value) {
  VARIANT v = {};
  v.vt = VT_UI4;
  v.ulVal = value;
  return api->SetValue(&guid, &v);
}

HRESULT SetCodecApiBool(ICodecAPI* api, const GUID& guid, bool value) {
  VARIANT v = {};
  v.vt = VT_BOOL;
  v.boolVal = value ? VARIANT_TRUE : VARIANT_FALSE;
  return api->SetValue(&guid, &v);
}

UINT32 H264ProfileToMFProfile(H264Profile profile) {
  switch (profile) {
    // eAVEncH264VProfile_ConstrainedBase is not understood by every vendor
    // MFT; Base with webrtc's own constraints applied is the interoperable
    // choice (matches what other MF-based WebRTC stacks negotiate).
    case H264Profile::kProfileConstrainedBaseline:
    case H264Profile::kProfileBaseline:
      return eAVEncH264VProfile_Base;
    case H264Profile::kProfileMain:
      return eAVEncH264VProfile_Main;
    case H264Profile::kProfileConstrainedHigh:
    case H264Profile::kProfileHigh:
      return eAVEncH264VProfile_High;
    default:
      return eAVEncH264VProfile_Base;
  }
}

UINT32 H264LevelToMFLevel(H264Level level) {
  switch (level) {
    case H264Level::kLevel1_b:
      return eAVEncH264VLevel1_b;
    case H264Level::kLevel1:
      return eAVEncH264VLevel1;
    case H264Level::kLevel1_1:
      return eAVEncH264VLevel1_1;
    case H264Level::kLevel1_2:
      return eAVEncH264VLevel1_2;
    case H264Level::kLevel1_3:
      return eAVEncH264VLevel1_3;
    case H264Level::kLevel2:
      return eAVEncH264VLevel2;
    case H264Level::kLevel2_1:
      return eAVEncH264VLevel2_1;
    case H264Level::kLevel2_2:
      return eAVEncH264VLevel2_2;
    case H264Level::kLevel3:
      return eAVEncH264VLevel3;
    case H264Level::kLevel3_1:
      return eAVEncH264VLevel3_1;
    case H264Level::kLevel3_2:
      return eAVEncH264VLevel3_2;
    case H264Level::kLevel4:
      return eAVEncH264VLevel4;
    case H264Level::kLevel4_1:
      return eAVEncH264VLevel4_1;
    case H264Level::kLevel4_2:
      return eAVEncH264VLevel4_2;
    case H264Level::kLevel5:
      return eAVEncH264VLevel5;
    case H264Level::kLevel5_1:
      return eAVEncH264VLevel5_1;
    case H264Level::kLevel5_2:
      return eAVEncH264VLevel5_2;
  }
  return eAVEncH264VLevel4;
}

}  // namespace

// Receives an async MFT's events on a Media Foundation work queue thread via
// BeginGetEvent, so Encode() can block on a condition variable and wake as
// soon as the hardware signals, instead of polling with Sleep() (whose
// granularity is the system timer resolution, up to 15.6 ms).
class MFAsyncEventPump : public IMFAsyncCallback {
 public:
  enum class WaitResult { kEvent, kTimeout, kError };

  explicit MFAsyncEventPump(IMFMediaEventGenerator* generator)
      : generator_(generator) {}

  HRESULT Start() {
    std::lock_guard<std::mutex> lock(mutex_);
    return generator_->BeginGetEvent(this, nullptr);
  }

  // After Stop() returns the generator is never touched again; the pending
  // BeginGetEvent reference is dropped when the MFT shuts down.
  void Stop() {
    std::lock_guard<std::mutex> lock(mutex_);
    generator_ = nullptr;
    events_.clear();
    drain_queue_ = nullptr;
    drain_task_ = nullptr;
    cv_.notify_all();
  }

  void SetDrainTask(TaskQueueBase* queue,
                    scoped_refptr<PendingTaskSafetyFlag> safety,
                    std::function<void()> drain) {
    std::lock_guard<std::mutex> lock(mutex_);
    drain_queue_ = queue;
    drain_safety_ = std::move(safety);
    drain_task_ = std::move(drain);
    drain_posted_ = false;
    if (!events_.empty()) {
      PostDrainLocked();
    }
  }

  // Called by the drain task before it consumes events, so an event arriving
  // while it runs schedules another drain.
  void OnDrainStarted() {
    std::lock_guard<std::mutex> lock(mutex_);
    drain_posted_ = false;
  }

  WaitResult Wait(std::chrono::steady_clock::duration timeout,
                  MediaEventType* type,
                  HRESULT* status) {
    std::unique_lock<std::mutex> lock(mutex_);
    cv_.wait_for(lock, timeout,
                 [this] { return !events_.empty() || FAILED(error_); });
    if (!events_.empty()) {
      *type = events_.front();
      events_.pop_front();
      return WaitResult::kEvent;
    }
    if (FAILED(error_)) {
      *status = error_;
      return WaitResult::kError;
    }
    return WaitResult::kTimeout;
  }

  STDMETHODIMP QueryInterface(REFIID riid, void** ppv) override {
    if (!ppv) {
      return E_POINTER;
    }
    if (riid == __uuidof(IUnknown) || riid == __uuidof(IMFAsyncCallback)) {
      *ppv = static_cast<IMFAsyncCallback*>(this);
      AddRef();
      return S_OK;
    }
    *ppv = nullptr;
    return E_NOINTERFACE;
  }
  STDMETHODIMP_(ULONG) AddRef() override { return ++ref_count_; }
  STDMETHODIMP_(ULONG) Release() override {
    const ULONG count = --ref_count_;
    if (count == 0) {
      delete this;
    }
    return count;
  }

  STDMETHODIMP GetParameters(DWORD*, DWORD*) override { return E_NOTIMPL; }

  STDMETHODIMP Invoke(IMFAsyncResult* async_result) override {
    std::lock_guard<std::mutex> lock(mutex_);
    if (!generator_) {
      return S_OK;
    }
    ComPtr<IMFMediaEvent> event;
    HRESULT hr = generator_->EndGetEvent(async_result, &event);
    if (SUCCEEDED(hr)) {
      MediaEventType type = MEUnknown;
      event->GetType(&type);
      events_.push_back(type);
      hr = generator_->BeginGetEvent(this, nullptr);
    }
    if (FAILED(hr)) {
      error_ = hr;
    }
    cv_.notify_all();
    PostDrainLocked();
    return S_OK;
  }

 private:
  virtual ~MFAsyncEventPump() = default;

  void PostDrainLocked() {
    if (drain_queue_ && drain_task_ && !drain_posted_) {
      drain_posted_ = true;
      drain_queue_->PostTask(SafeTask(drain_safety_, drain_task_));
    }
  }

  std::atomic<ULONG> ref_count_{1};
  std::mutex mutex_;
  std::condition_variable cv_;
  IMFMediaEventGenerator* generator_;
  std::deque<MediaEventType> events_;
  HRESULT error_ = S_OK;
  TaskQueueBase* drain_queue_ = nullptr;
  scoped_refptr<PendingTaskSafetyFlag> drain_safety_;
  std::function<void()> drain_task_;
  bool drain_posted_ = false;
};

MFH264EncoderImpl::MFH264EncoderImpl(const Environment& env,
                                     const SdpVideoFormat& format)
    : env_(env), format_(format) {
  std::optional<H264ProfileLevelId> profile_level_id =
      ParseSdpForH264ProfileLevelId(format.parameters);
  if (profile_level_id.has_value()) {
    profile_ = profile_level_id->profile;
    level_ = profile_level_id->level;
  }
}

MFH264EncoderImpl::~MFH264EncoderImpl() {
  Release();
}

void MFH264EncoderImpl::ReportInit() {
  if (has_reported_init_)
    return;
  RTC_HISTOGRAM_ENUMERATION("WebRTC.Video.H264EncoderImpl.Event",
                            kH264EncoderEventInit, kH264EncoderEventMax);
  has_reported_init_ = true;
}

void MFH264EncoderImpl::ReportError() {
  if (has_reported_error_)
    return;
  RTC_HISTOGRAM_ENUMERATION("WebRTC.Video.H264EncoderImpl.Event",
                            kH264EncoderEventError, kH264EncoderEventMax);
  has_reported_error_ = true;
}

int32_t MFH264EncoderImpl::InitEncode(const VideoCodec* inst,
                                      const VideoEncoder::Settings& settings) {
  if (!inst || inst->codecType != kVideoCodecH264) {
    ReportError();
    return WEBRTC_VIDEO_CODEC_ERR_PARAMETER;
  }
  if (inst->maxFramerate == 0) {
    ReportError();
    return WEBRTC_VIDEO_CODEC_ERR_PARAMETER;
  }
  if (inst->width < 1 || inst->height < 1) {
    ReportError();
    return WEBRTC_VIDEO_CODEC_ERR_PARAMETER;
  }

  int32_t release_ret = Release();
  if (release_ret != WEBRTC_VIDEO_CODEC_OK) {
    ReportError();
    return release_ret;
  }

  codec_ = *inst;

  // Code expects simulcastStream resolutions to be correct, make sure they are
  // filled even when there are no simulcast layers.
  if (codec_.numberOfSimulcastStreams == 0) {
    codec_.simulcastStream[0].width = codec_.width;
    codec_.simulcastStream[0].height = codec_.height;
  }

  // Initialize encoded image. Default buffer size: size of unencoded data.
  const size_t new_capacity =
      CalcBufferSize(VideoType::kI420, codec_.width, codec_.height);
  encoded_image_.SetEncodedData(EncodedImageBuffer::Create(new_capacity));
  encoded_image_._encodedWidth = codec_.width;
  encoded_image_._encodedHeight = codec_.height;
  encoded_image_.set_size(0);

  configuration_.sending = false;
  configuration_.frame_dropping_on = codec_.GetFrameDropEnabled();
  configuration_.key_frame_interval = codec_.H264()->keyFrameInterval;
  configuration_.width = codec_.width;
  configuration_.height = codec_.height;
  configuration_.max_frame_rate = codec_.maxFramerate;
  configuration_.target_bps = codec_.startBitrate * 1000;
  configuration_.max_bps = codec_.maxBitrate * 1000;

  if (!livekit_ffi::EnsureComInitialized() || !livekit_ffi::EnsureMFStarted()) {
    ReportError();
    return WEBRTC_VIDEO_CODEC_ERROR;
  }

  if (Faults().fail_init) {
    RTC_LOG(LS_WARNING) << "LK_MF_FAULT_INIT: failing MF H264 InitEncode.";
    ReportError();
    return WEBRTC_VIDEO_CODEC_ERROR;
  }

  int32_t ret = CreateTransform();
  if (ret != WEBRTC_VIDEO_CODEC_OK) {
    ReportError();
    return ret;
  }

  RTC_LOG(LS_INFO) << "MediaFoundation H264 encoder initialized ("
                   << friendly_name_ << "): " << codec_.width << "x"
                   << codec_.height << " @ " << codec_.maxFramerate
                   << "fps, target_bps=" << configuration_.target_bps
                   << ", level=" << static_cast<int>(level_)
                   << ", input=" << (d3d_.manager ? "d3d11" : "system memory")
                   << ", async=" << is_async_;

  SimulcastRateAllocator init_allocator(env_, codec_);
  VideoBitrateAllocation allocation =
      init_allocator.Allocate(VideoBitrateAllocationParameters(
          DataRate::KilobitsPerSec(codec_.startBitrate), codec_.maxFramerate));
  SetRates(RateControlParameters(allocation, codec_.maxFramerate));
  initialized_ = true;
  ReportInit();
  return WEBRTC_VIDEO_CODEC_OK;
}

int32_t MFH264EncoderImpl::CreateTransform() {
  if (Faults().max_sessions &&
      g_open_sessions.load() >= *Faults().max_sessions) {
    RTC_LOG(LS_WARNING) << "LK_MF_MAX_SESSIONS: " << g_open_sessions.load()
                        << " MF encoder sessions open, refusing another.";
    return WEBRTC_VIDEO_CODEC_ERROR;
  }
  std::vector<ComPtr<IMFActivate>> activates =
      livekit_ffi::EnumHardwareH264Encoders();
  if (activates.empty()) {
    RTC_LOG(LS_ERROR) << "No hardware H264 encoder MFT found.";
    return WEBRTC_VIDEO_CODEC_ERROR;
  }

  for (const ComPtr<IMFActivate>& activate : activates) {
    HRESULT hr = ActivateTransform(activate.Get());
    if (SUCCEEDED(hr)) {
      if (ConfigureTransform() == WEBRTC_VIDEO_CODEC_OK) {
        session_open_ = true;
        RTC_LOG(LS_INFO) << "MF encoder sessions open: "
                         << ++g_open_sessions;
        return WEBRTC_VIDEO_CODEC_OK;
      }
      hr = E_FAIL;
    }
    RTC_LOG(LS_WARNING) << "H264 encoder MFT \"" << friendly_name_
                        << "\" unusable (" << HResultToString(hr)
                        << "); trying the next one.";
    ReleaseTransform();
  }

  RTC_LOG(LS_ERROR) << "No hardware H264 encoder MFT accepted the "
                    << configuration_.width << "x" << configuration_.height
                    << " configuration.";
  return WEBRTC_VIDEO_CODEC_ERROR;
}

HRESULT MFH264EncoderImpl::ActivateTransform(IMFActivate* activate) {
  activate_ = activate;
  friendly_name_ = livekit_ffi::GetFriendlyName(activate);

  HRESULT hr = activate_->ActivateObject(IID_PPV_ARGS(&transform_));
  if (FAILED(hr)) {
    RTC_LOG(LS_ERROR) << "Failed to activate H264 encoder MFT \""
                      << friendly_name_ << "\": " << HResultToString(hr);
    return hr;
  }

  // D3D11-aware vendor MFTs (NVIDIA, AMD) reject media types with
  // MF_E_UNSUPPORTED_D3D_TYPE until they have a device manager on their own
  // adapter, so this has to happen before any SetOutputType.
  hr = livekit_ffi::PrepareHardwareTransform(activate, transform_.Get(),
                                             &is_async_, &d3d_);
  if (FAILED(hr)) {
    return hr;
  }

  if (is_async_) {
    hr = transform_.As(&event_generator_);
    if (FAILED(hr)) {
      RTC_LOG(LS_ERROR) << "Async MFT without IMFMediaEventGenerator: "
                        << HResultToString(hr);
      return hr;
    }
    event_pump_.Attach(new MFAsyncEventPump(event_generator_.Get()));
    hr = event_pump_->Start();
    if (FAILED(hr)) {
      RTC_LOG(LS_ERROR) << "BeginGetEvent failed: " << HResultToString(hr);
      return hr;
    }
  }

  // ICodecAPI is how rate control is configured; refuse encoders without it
  // rather than running with driver-default VBR.
  hr = transform_.As(&codec_api_);
  if (FAILED(hr)) {
    RTC_LOG(LS_ERROR) << "H264 encoder MFT does not expose ICodecAPI: "
                      << HResultToString(hr);
    return hr;
  }

  return S_OK;
}

int32_t MFH264EncoderImpl::ApplyCodecApiSettings(bool log_failures) {
  // Rate control: CBR at the target bitrate, low latency, no B-frames (webrtc
  // cannot tolerate them), and an effectively infinite GOP since webrtc
  // requests IDR frames itself.
  HRESULT hr = SetCodecApiUInt32(codec_api_.Get(),
                                 CODECAPI_AVEncCommonRateControlMode,
                                 eAVEncCommonRateControlMode_CBR);
  if (FAILED(hr)) {
    if (log_failures) {
      RTC_LOG(LS_ERROR) << "Failed to set CBR rate control: "
                        << HResultToString(hr);
    }
    return WEBRTC_VIDEO_CODEC_ERROR;
  }
  hr = SetCodecApiUInt32(codec_api_.Get(), CODECAPI_AVEncCommonMeanBitRate,
                         configuration_.target_bps);
  if (FAILED(hr)) {
    if (log_failures) {
      RTC_LOG(LS_ERROR) << "Failed to set mean bitrate: "
                        << HResultToString(hr);
    }
    return WEBRTC_VIDEO_CODEC_ERROR;
  }
  hr = SetCodecApiUInt32(codec_api_.Get(),
                         CODECAPI_AVEncMPVDefaultBPictureCount, 0);
  if (FAILED(hr)) {
    // Baseline has no B-frames, and some MFTs (NVIDIA) reject the property
    // for that profile with E_INVALIDARG.
    if (H264ProfileToMFProfile(profile_) != eAVEncH264VProfile_Base) {
      if (log_failures) {
        RTC_LOG(LS_ERROR) << "Failed to disable B-frames: "
                          << HResultToString(hr);
      }
      return WEBRTC_VIDEO_CODEC_ERROR;
    }
    if (log_failures) {
      RTC_LOG(LS_INFO) << "Encoder MFT rejected B-frame count ("
                       << HResultToString(hr)
                       << "); Baseline profile has none anyway.";
    }
  }
  // Best effort from here on: support varies by vendor/driver.
  if (FAILED(SetCodecApiBool(codec_api_.Get(), CODECAPI_AVLowLatencyMode,
                             true)) &&
      log_failures) {
    RTC_LOG(LS_WARNING) << "Encoder MFT rejected AVLowLatencyMode.";
  }
  if (FAILED(SetCodecApiUInt32(codec_api_.Get(), CODECAPI_AVEncMPVGOPSize,
                               0x7FFFFFFF)) &&
      log_failures) {
    RTC_LOG(LS_WARNING) << "Encoder MFT rejected infinite GOP size.";
  }
  return WEBRTC_VIDEO_CODEC_OK;
}

int32_t MFH264EncoderImpl::ConfigureTransform() {
  DWORD input_ids[1] = {0};
  DWORD output_ids[1] = {0};
  HRESULT hr = transform_->GetStreamIDs(1, input_ids, 1, output_ids);
  if (hr == E_NOTIMPL) {
    input_ids[0] = 0;
    output_ids[0] = 0;
  } else if (FAILED(hr)) {
    RTC_LOG(LS_ERROR) << "GetStreamIDs failed: " << HResultToString(hr);
    return WEBRTC_VIDEO_CODEC_ERROR;
  }
  input_stream_id_ = input_ids[0];
  output_stream_id_ = output_ids[0];

  const UINT32 fps =
      std::max(1u, static_cast<UINT32>(configuration_.max_frame_rate + 0.5f));

  // Rate control, GOP and B-frame settings are latched when the media types
  // are set (Microsoft documents this for the encoder MFTs, and NVIDIA
  // ignores a later rate control mode / GOP size), so set them first.
  ApplyCodecApiSettings(/*log_failures=*/false);

  // Output type first: encoder MFTs require it before the input type.
  ComPtr<IMFMediaType> output_type;
  hr = MFCreateMediaType(&output_type);
  if (FAILED(hr)) {
    return WEBRTC_VIDEO_CODEC_ERROR;
  }
  output_type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
  output_type->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_H264);
  output_type->SetUINT32(MF_MT_AVG_BITRATE, configuration_.target_bps);
  MFSetAttributeSize(output_type.Get(), MF_MT_FRAME_SIZE, configuration_.width,
                     configuration_.height);
  MFSetAttributeRatio(output_type.Get(), MF_MT_FRAME_RATE, fps, 1);
  output_type->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive);
  output_type->SetUINT32(MF_MT_MPEG2_PROFILE, H264ProfileToMFProfile(profile_));
  output_type->SetUINT32(MF_MT_MPEG2_LEVEL, H264LevelToMFLevel(level_));

  hr = transform_->SetOutputType(output_stream_id_, output_type.Get(), 0);
  if (FAILED(hr)) {
    // Some drivers reject an explicit level; let the MFT derive it.
    output_type->DeleteItem(MF_MT_MPEG2_LEVEL);
    hr = transform_->SetOutputType(output_stream_id_, output_type.Get(), 0);
  }
  if (FAILED(hr)) {
    RTC_LOG(LS_ERROR) << "SetOutputType failed: " << HResultToString(hr);
    return WEBRTC_VIDEO_CODEC_ERROR;
  }

  // Input type: prefer the MFT's own NV12 type so vendor-specific attributes
  // (strides, apertures) are preserved.
  ComPtr<IMFMediaType> input_type;
  for (DWORD i = 0;; i++) {
    ComPtr<IMFMediaType> candidate;
    hr = transform_->GetInputAvailableType(input_stream_id_, i, &candidate);
    if (FAILED(hr)) {
      break;
    }
    GUID subtype = {};
    candidate->GetGUID(MF_MT_SUBTYPE, &subtype);
    if (subtype == MFVideoFormat_NV12) {
      input_type = candidate;
      break;
    }
  }
  if (!input_type) {
    hr = MFCreateMediaType(&input_type);
    if (FAILED(hr)) {
      return WEBRTC_VIDEO_CODEC_ERROR;
    }
    input_type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
    input_type->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_NV12);
  }
  MFSetAttributeSize(input_type.Get(), MF_MT_FRAME_SIZE, configuration_.width,
                     configuration_.height);
  MFSetAttributeRatio(input_type.Get(), MF_MT_FRAME_RATE, fps, 1);
  input_type->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive);

  hr = transform_->SetInputType(input_stream_id_, input_type.Get(), 0);
  if (FAILED(hr)) {
    RTC_LOG(LS_ERROR) << "SetInputType failed: " << HResultToString(hr);
    return WEBRTC_VIDEO_CODEC_ERROR;
  }

  // Applied again now that the types are set: some MFTs only accept these
  // afterwards, and this pass is the one whose failures count.
  if (ApplyCodecApiSettings(/*log_failures=*/true) != WEBRTC_VIDEO_CODEC_OK) {
    return WEBRTC_VIDEO_CODEC_ERROR;
  }

  active_bitrate_bps_ = configuration_.target_bps;
  dynamic_bitrate_supported_ = true;
  pending_bitrate_reinit_ = false;

  hr = transform_->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
  if (FAILED(hr)) {
    RTC_LOG(LS_ERROR) << "NOTIFY_BEGIN_STREAMING failed: "
                      << HResultToString(hr);
    return WEBRTC_VIDEO_CODEC_ERROR;
  }
  hr = transform_->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
  if (FAILED(hr)) {
    RTC_LOG(LS_ERROR) << "NOTIFY_START_OF_STREAM failed: "
                      << HResultToString(hr);
    return WEBRTC_VIDEO_CODEC_ERROR;
  }

  CacheSequenceHeader();
  return WEBRTC_VIDEO_CODEC_OK;
}

int32_t MFH264EncoderImpl::ReinitTransform() {
  RTC_LOG(LS_INFO) << "Reinitializing MF H264 encoder (bitrate "
                   << active_bitrate_bps_ << " -> "
                   << configuration_.target_bps << " bps).";
  ReleaseTransform();
  pending_frames_.clear();
  frame_count_ = 0;
  if (CreateTransform() != WEBRTC_VIDEO_CODEC_OK) {
    RTC_LOG(LS_ERROR) << "MF H264 encoder re-init failed; falling back to "
                         "software.";
    return kHardwareFailure;
  }
  configuration_.key_frame_request = true;
  return WEBRTC_VIDEO_CODEC_OK;
}

HRESULT MFH264EncoderImpl::NegotiateOutputType() {
  for (DWORD i = 0;; i++) {
    ComPtr<IMFMediaType> candidate;
    HRESULT hr =
        transform_->GetOutputAvailableType(output_stream_id_, i, &candidate);
    if (FAILED(hr)) {
      return hr;
    }
    GUID subtype = {};
    candidate->GetGUID(MF_MT_SUBTYPE, &subtype);
    if (subtype != MFVideoFormat_H264) {
      continue;
    }
    hr = transform_->SetOutputType(output_stream_id_, candidate.Get(), 0);
    if (SUCCEEDED(hr)) {
      sequence_header_.clear();
      CacheSequenceHeader();
    }
    return hr;
  }
}

void MFH264EncoderImpl::CacheSequenceHeader() {
  ComPtr<IMFMediaType> current;
  if (FAILED(transform_->GetOutputCurrentType(output_stream_id_, &current))) {
    return;
  }
  UINT8* blob = nullptr;
  UINT32 blob_size = 0;
  if (SUCCEEDED(current->GetAllocatedBlob(MF_MT_MPEG_SEQUENCE_HEADER, &blob,
                                          &blob_size)) &&
      blob_size > 0) {
    sequence_header_.assign(blob, blob + blob_size);
  }
  if (blob) {
    CoTaskMemFree(blob);
  }
}

int32_t MFH264EncoderImpl::RegisterEncodeCompleteCallback(
    EncodedImageCallback* callback) {
  encoded_image_callback_ = callback;
  return WEBRTC_VIDEO_CODEC_OK;
}

int32_t MFH264EncoderImpl::Release() {
  initialized_ = false;
  ReleaseTransform();
  pending_frames_.clear();
  frame_count_ = 0;
  frames_submitted_ = 0;
  return WEBRTC_VIDEO_CODEC_OK;
}

void MFH264EncoderImpl::ReleaseTransform() {
  if (session_open_) {
    session_open_ = false;
    --g_open_sessions;
  }
  if (drain_safety_) {
    drain_safety_->SetNotAlive();
    drain_safety_ = nullptr;
  }
  async_failed_ = false;
  if (event_pump_) {
    event_pump_->Stop();
    event_pump_.Reset();
  }
  if (transform_) {
    transform_->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM,
                               input_stream_id_);
    transform_->ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
    transform_->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
  }
  event_generator_.Reset();
  codec_api_.Reset();
  transform_.Reset();
  if (activate_) {
    activate_->ShutdownObject();
    activate_.Reset();
  }
  input_textures_.clear();
  staging_texture_.Reset();
  d3d_ = livekit_ffi::D3D11DeviceBundle();
  need_input_credits_ = 0;
  sequence_header_.clear();
  is_async_ = false;
  requested_keyframes_ = 0;
}

HRESULT MFH264EncoderImpl::CreateInputSample(const I420BufferInterface& buffer,
                                             int64_t sample_time_100ns,
                                             int64_t duration_100ns,
                                             IMFSample** sample_out) {
  const int width = buffer.width();
  const int height = buffer.height();

  ComPtr<IMFMediaBuffer> media_buffer;
  HRESULT hr = MFCreate2DMediaBuffer(width, height, MFVideoFormat_NV12.Data1,
                                     FALSE, &media_buffer);
  if (FAILED(hr)) {
    return hr;
  }

  ComPtr<IMF2DBuffer> buffer_2d;
  hr = media_buffer.As(&buffer_2d);
  if (FAILED(hr)) {
    return hr;
  }

  BYTE* scanline0 = nullptr;
  LONG pitch = 0;
  hr = buffer_2d->Lock2D(&scanline0, &pitch);
  if (FAILED(hr)) {
    return hr;
  }
  // NV12 is never bottom-up: Y plane at scanline0, interleaved UV plane
  // directly after it.
  uint8_t* dst_y = scanline0;
  uint8_t* dst_uv = scanline0 + static_cast<size_t>(pitch) * height;
  int ret = libyuv::I420ToNV12(buffer.DataY(), buffer.StrideY(), buffer.DataU(),
                               buffer.StrideU(), buffer.DataV(),
                               buffer.StrideV(), dst_y, pitch, dst_uv, pitch,
                               width, height);
  buffer_2d->Unlock2D();
  if (ret != 0) {
    return E_FAIL;
  }

  DWORD contiguous_length = 0;
  if (SUCCEEDED(buffer_2d->GetContiguousLength(&contiguous_length))) {
    media_buffer->SetCurrentLength(contiguous_length);
  }

  ComPtr<IMFSample> sample;
  hr = MFCreateSample(&sample);
  if (FAILED(hr)) {
    return hr;
  }
  sample->AddBuffer(media_buffer.Get());
  sample->SetSampleTime(sample_time_100ns);
  sample->SetSampleDuration(duration_100ns);

  *sample_out = sample.Detach();
  return S_OK;
}

HRESULT MFH264EncoderImpl::AcquireInputTexture(ID3D11Texture2D** texture_out) {
  for (const ComPtr<ID3D11Texture2D>& texture : input_textures_) {
    // Only the pool's reference left means the MFT has released the sample
    // and DXGI buffer that wrapped this texture.
    texture->AddRef();
    if (texture->Release() == 1) {
      *texture_out = texture.Get();
      (*texture_out)->AddRef();
      return S_OK;
    }
  }
  if (input_textures_.size() >= kMaxInputTextures) {
    RTC_LOG(LS_ERROR) << "All " << input_textures_.size()
                      << " encoder input textures are still owned by the MFT.";
    return MF_E_SAMPLEALLOCATOR_EMPTY;
  }

  D3D11_TEXTURE2D_DESC desc = {};
  desc.Width = static_cast<UINT>(configuration_.width);
  desc.Height = static_cast<UINT>(configuration_.height);
  desc.MipLevels = 1;
  desc.ArraySize = 1;
  desc.Format = DXGI_FORMAT_NV12;
  desc.SampleDesc.Count = 1;
  desc.Usage = D3D11_USAGE_DEFAULT;
  ComPtr<ID3D11Texture2D> texture;
  HRESULT hr = E_FAIL;
  for (UINT bind_flags :
       {static_cast<UINT>(D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE),
        static_cast<UINT>(D3D11_BIND_SHADER_RESOURCE), 0u}) {
    desc.BindFlags = bind_flags;
    hr = d3d_.device->CreateTexture2D(&desc, nullptr, &texture);
    if (SUCCEEDED(hr)) {
      break;
    }
  }
  if (FAILED(hr)) {
    RTC_LOG(LS_ERROR) << "Failed to create NV12 encoder input texture: "
                      << HResultToString(hr);
    return hr;
  }
  input_textures_.push_back(texture);
  RTC_LOG(LS_INFO) << "Allocated D3D11 encoder input texture "
                   << input_textures_.size() << " (" << desc.Width << "x"
                   << desc.Height << ", bind flags " << desc.BindFlags << ")";
  *texture_out = texture.Detach();
  return S_OK;
}

HRESULT MFH264EncoderImpl::CreateD3DInputSample(
    const I420BufferInterface& buffer,
    int64_t sample_time_100ns,
    int64_t duration_100ns,
    IMFSample** sample_out) {
  const int width = buffer.width();
  const int height = buffer.height();
  if (width != configuration_.width || height != configuration_.height) {
    RTC_LOG(LS_ERROR) << "Frame size " << width << "x" << height
                      << " does not match encoder configuration "
                      << configuration_.width << "x" << configuration_.height;
    return E_INVALIDARG;
  }

  if (!staging_texture_) {
    D3D11_TEXTURE2D_DESC desc = {};
    desc.Width = static_cast<UINT>(width);
    desc.Height = static_cast<UINT>(height);
    desc.MipLevels = 1;
    desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_NV12;
    desc.SampleDesc.Count = 1;
    desc.Usage = D3D11_USAGE_STAGING;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_WRITE;
    HRESULT hr = d3d_.device->CreateTexture2D(&desc, nullptr, &staging_texture_);
    if (FAILED(hr)) {
      RTC_LOG(LS_ERROR) << "Failed to create NV12 staging texture: "
                        << HResultToString(hr);
      return hr;
    }
  }

  D3D11_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = d3d_.context->Map(staging_texture_.Get(), 0, D3D11_MAP_WRITE, 0,
                                 &mapped);
  if (FAILED(hr)) {
    return hr;
  }
  // Mapped NV12: Y rows at RowPitch, then the interleaved UV plane starting
  // Height rows below with the same pitch.
  uint8_t* dst_y = static_cast<uint8_t*>(mapped.pData);
  uint8_t* dst_uv = dst_y + static_cast<size_t>(mapped.RowPitch) * height;
  const int pitch = static_cast<int>(mapped.RowPitch);
  int ret = libyuv::I420ToNV12(buffer.DataY(), buffer.StrideY(), buffer.DataU(),
                               buffer.StrideU(), buffer.DataV(),
                               buffer.StrideV(), dst_y, pitch, dst_uv, pitch,
                               width, height);
  d3d_.context->Unmap(staging_texture_.Get(), 0);
  if (ret != 0) {
    return E_FAIL;
  }

  ComPtr<ID3D11Texture2D> texture;
  hr = AcquireInputTexture(&texture);
  if (FAILED(hr)) {
    return hr;
  }
  d3d_.context->CopyResource(texture.Get(), staging_texture_.Get());

  ComPtr<IMFMediaBuffer> media_buffer;
  hr = MFCreateDXGISurfaceBuffer(__uuidof(ID3D11Texture2D), texture.Get(), 0,
                                 FALSE, &media_buffer);
  if (FAILED(hr)) {
    return hr;
  }
  DWORD length = 0;
  ComPtr<IMF2DBuffer> buffer_2d;
  if (SUCCEEDED(media_buffer.As(&buffer_2d)) &&
      SUCCEEDED(buffer_2d->GetContiguousLength(&length))) {
    media_buffer->SetCurrentLength(length);
  } else if (SUCCEEDED(media_buffer->GetMaxLength(&length))) {
    media_buffer->SetCurrentLength(length);
  }

  ComPtr<IMFSample> sample;
  hr = MFCreateSample(&sample);
  if (FAILED(hr)) {
    return hr;
  }
  sample->AddBuffer(media_buffer.Get());
  sample->SetSampleTime(sample_time_100ns);
  sample->SetSampleDuration(duration_100ns);

  *sample_out = sample.Detach();
  return S_OK;
}

int32_t MFH264EncoderImpl::PumpEvents(int timeout_ms,
                                      bool until_need_input,
                                      size_t until_pending_at_most,
                                      bool fail_on_timeout) {
  const auto deadline =
      std::chrono::steady_clock::now() + std::chrono::milliseconds(timeout_ms);
  for (;;) {
    const bool goals_met = (!until_need_input || need_input_credits_ > 0) &&
                           pending_frames_.size() <= until_pending_at_most;

    MediaEventType type = MEUnknown;
    HRESULT status = S_OK;
    const auto wait = goals_met
                          ? std::chrono::steady_clock::duration::zero()
                          : deadline - std::chrono::steady_clock::now();
    const MFAsyncEventPump::WaitResult result =
        event_pump_->Wait(wait, &type, &status);
    if (result == MFAsyncEventPump::WaitResult::kTimeout) {
      if (goals_met || !fail_on_timeout) {
        return WEBRTC_VIDEO_CODEC_OK;
      }
      RTC_LOG(LS_ERROR) << "Timed out waiting for encoder MFT ("
                        << (until_need_input ? "input slot" : "output") << ", "
                        << pending_frames_.size() << " frames in flight).";
      ReportError();
      return kHardwareFailure;
    }
    if (result == MFAsyncEventPump::WaitResult::kError) {
      RTC_LOG(LS_ERROR) << "Encoder MFT event queue failed: "
                        << HResultToString(status);
      ReportError();
      return kHardwareFailure;
    }

    if (type == METransformNeedInput) {
      need_input_credits_++;
    } else if (type == METransformHaveOutput) {
      int32_t ret = CollectOneOutput();
      if (ret != WEBRTC_VIDEO_CODEC_OK) {
        return ret;
      }
    }
    // Other events (drain complete, markers) need no handling here.
  }
}

int32_t MFH264EncoderImpl::Encode(
    const VideoFrame& input_frame,
    const std::vector<VideoFrameType>* frame_types) {
  if (!initialized_) {
    ReportError();
    return WEBRTC_VIDEO_CODEC_UNINITIALIZED;
  }
  if (!transform_ || async_failed_) {
    ReportError();
    return kHardwareFailure;
  }
  if (!encoded_image_callback_) {
    RTC_LOG(LS_WARNING)
        << "InitEncode() has been called, but a callback function "
           "has not been set with RegisterEncodeCompleteCallback()";
    ReportError();
    return WEBRTC_VIDEO_CODEC_UNINITIALIZED;
  }

  if (pending_bitrate_reinit_) {
    int32_t ret = ReinitTransform();
    if (ret != WEBRTC_VIDEO_CODEC_OK) {
      ReportError();
      return ret;
    }
  }

  webrtc::scoped_refptr<I420BufferInterface> frame_buffer =
      input_frame.video_frame_buffer()->ToI420();
  if (!frame_buffer) {
    RTC_LOG(LS_ERROR) << "Failed to convert "
                      << VideoFrameBufferTypeToString(
                             input_frame.video_frame_buffer()->type())
                      << " image to I420. Can't encode frame.";
    return kHardwareFailure;
  }
  RTC_CHECK(frame_buffer->type() == VideoFrameBuffer::Type::kI420);

  bool is_keyframe_needed = false;
  if (configuration_.key_frame_request && configuration_.sending) {
    is_keyframe_needed = true;
  }

  bool send_key_frame =
      is_keyframe_needed ||
      (frame_types && (*frame_types)[0] == VideoFrameType::kVideoFrameKey);
  if (send_key_frame) {
    is_keyframe_needed = true;
    configuration_.key_frame_request = false;
  }

  RTC_DCHECK_EQ(configuration_.width, frame_buffer->width());
  RTC_DCHECK_EQ(configuration_.height, frame_buffer->height());

  if (!configuration_.sending) {
    return WEBRTC_VIDEO_CODEC_NO_OUTPUT;
  }

  if (frame_types != nullptr) {
    // Skip frame?
    if ((*frame_types)[0] == VideoFrameType::kEmptyFrame) {
      return WEBRTC_VIDEO_CODEC_NO_OUTPUT;
    }
  }

  if (is_async_) {
    if (!drain_safety_) {
      EnableOutputDrainTask();
    }
    int32_t ret = PumpEvents(kNeedInputTimeoutMs, /*until_need_input=*/true,
                             /*until_pending_at_most=*/SIZE_MAX);
    if (ret != WEBRTC_VIDEO_CODEC_OK) {
      return ret;
    }
  }

  if (is_keyframe_needed) {
    HRESULT hr = SetCodecApiUInt32(codec_api_.Get(),
                                   CODECAPI_AVEncVideoForceKeyFrame, 1);
    if (FAILED(hr)) {
      RTC_LOG(LS_WARNING) << "ForceKeyFrame rejected: " << HResultToString(hr);
    }
    requested_keyframes_++;
  }

  const int64_t fps =
      std::max<int64_t>(1, static_cast<int64_t>(configuration_.max_frame_rate));
  const int64_t duration_100ns = 10'000'000 / fps;
  const int64_t sample_time_100ns = frame_count_ * duration_100ns;
  frame_count_++;

  ComPtr<IMFSample> sample;
  HRESULT hr = d3d_.manager
                   ? CreateD3DInputSample(*frame_buffer, sample_time_100ns,
                                          duration_100ns, &sample)
                   : CreateInputSample(*frame_buffer, sample_time_100ns,
                                       duration_100ns, &sample);
  if (FAILED(hr)) {
    RTC_LOG(LS_ERROR) << "Failed to create input sample: "
                      << HResultToString(hr);
    ReportError();
    return kHardwareFailure;
  }

  PendingFrameInfo info;
  info.sample_time_100ns = sample_time_100ns;
  info.rtp_timestamp = input_frame.rtp_timestamp();
  info.ntp_time_ms = input_frame.ntp_time_ms();
  info.render_time_ms = input_frame.render_time_ms();
  info.rotation = input_frame.rotation();
  info.color_space = input_frame.color_space();
  pending_frames_.push_back(info);

  if (Faults().fail_after_frames &&
      frames_submitted_ >= *Faults().fail_after_frames) {
    RTC_LOG(LS_WARNING) << "LK_MF_FAULT_AFTER_FRAMES: simulating device "
                           "removal after "
                        << frames_submitted_ << " frames.";
    hr = DXGI_ERROR_DEVICE_REMOVED;
  } else {
    hr = transform_->ProcessInput(input_stream_id_, sample.Get(), 0);
  }
  if (hr == MF_E_NOTACCEPTING && !is_async_) {
    int32_t ret = CollectOutputsSync();
    if (ret != WEBRTC_VIDEO_CODEC_OK) {
      return ret;
    }
    hr = transform_->ProcessInput(input_stream_id_, sample.Get(), 0);
  }
  if (FAILED(hr)) {
    pending_frames_.pop_back();
    RTC_LOG(LS_ERROR) << "ProcessInput failed: " << HResultToString(hr);
    ReportError();
    return kHardwareFailure;
  }
  frames_submitted_++;

  if (is_async_) {
    need_input_credits_--;
    // Without an encoder task queue to post drains to, give the hardware a
    // short window to return this frame so latency is the encode time rather
    // than a whole frame interval.
    if (!drain_safety_) {
      const int soft_wait_ms = std::max(
          2, static_cast<int>(500 / std::max(1.0f, configuration_.max_frame_rate)));
      int32_t ret = PumpEvents(soft_wait_ms, /*until_need_input=*/false,
                               /*until_pending_at_most=*/0,
                               /*fail_on_timeout=*/false);
      if (ret != WEBRTC_VIDEO_CODEC_OK) {
        return ret;
      }
    }
    // Block only when too many frames are in flight so encoder latency
    // stays bounded.
    return PumpEvents(kOutputWaitTimeoutMs, /*until_need_input=*/false,
                      /*until_pending_at_most=*/kMaxPendingFrames);
  }
  return CollectOutputsSync();
}

void MFH264EncoderImpl::EnableOutputDrainTask() {
  TaskQueueBase* queue = TaskQueueBase::Current();
  if (!queue || !event_pump_) {
    return;
  }
  drain_safety_ = PendingTaskSafetyFlag::Create();
  event_pump_->SetDrainTask(queue, drain_safety_,
                            [this] { DrainAsyncOutput(); });
}

void MFH264EncoderImpl::DrainAsyncOutput() {
  if (!transform_ || !event_pump_ || async_failed_) {
    return;
  }
  event_pump_->OnDrainStarted();
  if (!encoded_image_callback_) {
    return;
  }
  int32_t ret = PumpEvents(0, /*until_need_input=*/false,
                           /*until_pending_at_most=*/SIZE_MAX,
                           /*fail_on_timeout=*/false);
  if (ret == kHardwareFailure) {
    async_failed_ = true;
  }
}

int32_t MFH264EncoderImpl::CollectOutputsSync() {
  for (;;) {
    int32_t ret = CollectOneOutput();
    if (ret == WEBRTC_VIDEO_CODEC_NO_OUTPUT) {
      return WEBRTC_VIDEO_CODEC_OK;
    }
    if (ret != WEBRTC_VIDEO_CODEC_OK) {
      return ret;
    }
  }
}

int32_t MFH264EncoderImpl::CollectOneOutput() {
  MFT_OUTPUT_STREAM_INFO stream_info = {};
  HRESULT hr = transform_->GetOutputStreamInfo(output_stream_id_, &stream_info);
  if (FAILED(hr)) {
    RTC_LOG(LS_ERROR) << "GetOutputStreamInfo failed: " << HResultToString(hr);
    ReportError();
    return kHardwareFailure;
  }
  const bool transform_allocates =
      stream_info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES |
                             MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES);

  ComPtr<IMFSample> allocated;
  if (!transform_allocates) {
    hr = MFCreateSample(&allocated);
    if (FAILED(hr)) {
      return kHardwareFailure;
    }
    ComPtr<IMFMediaBuffer> out_buffer;
    const DWORD size = stream_info.cbSize
                           ? stream_info.cbSize
                           : static_cast<DWORD>(configuration_.width) *
                                 configuration_.height * 3 / 2;
    hr = MFCreateAlignedMemoryBuffer(
        size, stream_info.cbAlignment > 1 ? stream_info.cbAlignment - 1 : 0,
        &out_buffer);
    if (FAILED(hr)) {
      return kHardwareFailure;
    }
    allocated->AddBuffer(out_buffer.Get());
  }

  for (int attempt = 0; attempt < 2; attempt++) {
    MFT_OUTPUT_DATA_BUFFER output = {};
    output.dwStreamID = output_stream_id_;
    output.pSample = transform_allocates ? nullptr : allocated.Get();
    DWORD status = 0;
    hr = transform_->ProcessOutput(0, 1, &output, &status);
    if (output.pEvents) {
      output.pEvents->Release();
    }

    if (hr == MF_E_TRANSFORM_NEED_MORE_INPUT) {
      return WEBRTC_VIDEO_CODEC_NO_OUTPUT;
    }
    if (hr == MF_E_TRANSFORM_STREAM_CHANGE) {
      HRESULT nhr = NegotiateOutputType();
      if (FAILED(nhr)) {
        RTC_LOG(LS_ERROR) << "Output renegotiation failed: "
                          << HResultToString(nhr);
        ReportError();
        return kHardwareFailure;
      }
      // Async MFTs re-signal METransformHaveOutput after a stream change; an
      // immediate ProcessOutput retry returns E_UNEXPECTED (seen on Intel
      // QSV). Only sync MFTs retry inline.
      if (is_async_) {
        return WEBRTC_VIDEO_CODEC_NO_OUTPUT;
      }
      continue;
    }
    if (FAILED(hr)) {
      RTC_LOG(LS_ERROR) << "ProcessOutput failed: " << HResultToString(hr);
      ReportError();
      return kHardwareFailure;
    }

    ComPtr<IMFSample> sample;
    if (transform_allocates) {
      sample.Attach(output.pSample);
    } else {
      sample = allocated;
    }
    if (!sample) {
      return WEBRTC_VIDEO_CODEC_NO_OUTPUT;
    }

    int64_t sample_time = 0;
    sample->GetSampleTime(&sample_time);

    ComPtr<IMFMediaBuffer> contiguous;
    hr = sample->ConvertToContiguousBuffer(&contiguous);
    if (FAILED(hr)) {
      return kHardwareFailure;
    }
    BYTE* data = nullptr;
    DWORD max_length = 0;
    DWORD current_length = 0;
    hr = contiguous->Lock(&data, &max_length, &current_length);
    if (FAILED(hr)) {
      return kHardwareFailure;
    }
    packet_.assign(data, data + current_length);
    contiguous->Unlock();

    pending_output_info_ = TakePendingInfo(sample_time);
    return ProcessEncodedFrame(packet_);
  }

  RTC_LOG(LS_ERROR) << "Encoder output stream change did not settle.";
  ReportError();
  return kHardwareFailure;
}

MFH264EncoderImpl::PendingFrameInfo MFH264EncoderImpl::TakePendingInfo(
    int64_t sample_time_100ns) {
  // With B-frames disabled output order matches input order; the sample time
  // check only guards against a driver dropping frames internally.
  while (!pending_frames_.empty() &&
         pending_frames_.front().sample_time_100ns < sample_time_100ns) {
    RTC_LOG(LS_WARNING) << "Encoder MFT dropped frame with sample time "
                        << pending_frames_.front().sample_time_100ns;
    pending_frames_.pop_front();
  }
  PendingFrameInfo info;
  if (!pending_frames_.empty()) {
    info = pending_frames_.front();
    pending_frames_.pop_front();
  } else {
    RTC_LOG(LS_WARNING) << "Encoded output without pending frame metadata.";
    info.sample_time_100ns = sample_time_100ns;
  }
  return info;
}

int32_t MFH264EncoderImpl::ProcessEncodedFrame(std::vector<uint8_t>& packet) {
  const PendingFrameInfo& info = pending_output_info_;

  bool is_idr = false;
  bool has_sps = false;
  std::vector<H264::NaluIndex> nalu_indices =
      H264::FindNaluIndices(MakeArrayView(packet.data(), packet.size()));
  for (const H264::NaluIndex& index : nalu_indices) {
    const H264::NaluType nalu_type =
        H264::ParseNaluType(packet[index.payload_start_offset]);
    if (nalu_type == H264::kIdr) {
      is_idr = true;
    } else if (nalu_type == H264::kSps) {
      has_sps = true;
    }
  }

  if (is_idr) {
    if (requested_keyframes_ > 0) {
      requested_keyframes_--;
    } else {
      RTC_LOG(LS_VERBOSE) << "Encoder MFT produced an unrequested IDR ("
                       << packet.size() << " bytes, bitrate "
                       << active_bitrate_bps_ << ").";
    }
  }

  // Some vendor MFTs do not repeat SPS/PPS on every IDR; the RTP packetizer
  // needs them inline, so prepend the cached sequence header.
  if (is_idr && !has_sps) {
    if (sequence_header_.empty()) {
      CacheSequenceHeader();
    }
    if (!sequence_header_.empty()) {
      packet.insert(packet.begin(), sequence_header_.begin(),
                    sequence_header_.end());
    } else {
      RTC_LOG(LS_WARNING)
          << "IDR frame without SPS/PPS and no cached sequence header.";
    }
  }

  encoded_image_._encodedWidth = configuration_.width;
  encoded_image_._encodedHeight = configuration_.height;
  encoded_image_.SetRtpTimestamp(info.rtp_timestamp);
  encoded_image_.SetSimulcastIndex(0);
  encoded_image_.ntp_time_ms_ = info.ntp_time_ms;
  encoded_image_.capture_time_ms_ = info.render_time_ms;
  encoded_image_.rotation_ = info.rotation;
  encoded_image_.content_type_ = VideoContentType::UNSPECIFIED;
  encoded_image_.timing_.flags = VideoSendTiming::kInvalid;
  encoded_image_._frameType = is_idr ? VideoFrameType::kVideoFrameKey
                                     : VideoFrameType::kVideoFrameDelta;
  encoded_image_.SetColorSpace(info.color_space);

  encoded_image_.SetEncodedData(
      EncodedImageBuffer::Create(packet.data(), packet.size()));
  encoded_image_.set_size(packet.size());

  h264_bitstream_parser_.ParseBitstream(encoded_image_);
  encoded_image_.qp_ = h264_bitstream_parser_.GetLastSliceQp().value_or(-1);

  CodecSpecificInfo codec_info;
  codec_info.codecType = kVideoCodecH264;
  codec_info.codecSpecific.H264.packetization_mode =
      H264PacketizationMode::NonInterleaved;

  const auto result =
      encoded_image_callback_->OnEncodedImage(encoded_image_, &codec_info);
  if (result.error != EncodedImageCallback::Result::OK) {
    RTC_LOG(LS_ERROR) << "OnEncodedImage failed " << result.error;
    return WEBRTC_VIDEO_CODEC_ERROR;
  }
  return WEBRTC_VIDEO_CODEC_OK;
}

VideoEncoder::EncoderInfo MFH264EncoderImpl::GetEncoderInfo() const {
  EncoderInfo info;
  info.supports_native_handle = false;
  info.implementation_name = friendly_name_.empty()
                                 ? "MediaFoundation H264 Encoder"
                                 : "MediaFoundation H264 Encoder (" +
                                       friendly_name_ + ")";
  info.scaling_settings = VideoEncoder::ScalingSettings::kOff;
  info.is_hardware_accelerated = true;
  info.supports_simulcast = false;
  // NV12 surfaces (and most hardware encoders) need even dimensions.
  info.requested_resolution_alignment = 2;
  info.preferred_pixel_formats = {VideoFrameBuffer::Type::kI420};
  return info;
}

void MFH264EncoderImpl::ApplyBitrate(uint32_t bitrate_bps) {
  if (bitrate_bps == active_bitrate_bps_) {
    return;
  }
  if (dynamic_bitrate_supported_) {
    HRESULT hr = SetCodecApiUInt32(
        codec_api_.Get(), CODECAPI_AVEncCommonMeanBitRate, bitrate_bps);
    if (SUCCEEDED(hr)) {
      active_bitrate_bps_ = bitrate_bps;
      return;
    }
    dynamic_bitrate_supported_ = false;
    RTC_LOG(LS_WARNING)
        << "Encoder MFT rejected runtime bitrate update ("
        << HResultToString(hr)
        << "); falling back to re-init with hysteresis.";
  }
  // Dynamic update unsupported: re-initialize, but only when the target has
  // moved enough to matter — a full re-init costs a keyframe.
  const uint32_t reference = std::max(active_bitrate_bps_, 1u);
  const uint32_t delta = bitrate_bps > active_bitrate_bps_
                             ? bitrate_bps - active_bitrate_bps_
                             : active_bitrate_bps_ - bitrate_bps;
  if (delta * 5 > reference) {  // > 20% change
    pending_bitrate_reinit_ = true;
  }
}

void MFH264EncoderImpl::SetRates(const RateControlParameters& parameters) {
  if (!transform_) {
    RTC_LOG(LS_WARNING) << "SetRates() while uninitialized.";
    return;
  }

  if (parameters.framerate_fps < 1.0) {
    RTC_LOG(LS_WARNING) << "Invalid frame rate: " << parameters.framerate_fps;
    return;
  }

  if (parameters.bitrate.get_sum_bps() == 0) {
    configuration_.SetStreamState(false);
    return;
  }

  codec_.maxFramerate = static_cast<uint32_t>(parameters.framerate_fps);
  codec_.maxBitrate = parameters.bitrate.GetSpatialLayerSum(0);

  configuration_.target_bps = parameters.bitrate.GetSpatialLayerSum(0);
  configuration_.max_frame_rate = parameters.framerate_fps;

  if (configuration_.target_bps) {
    ApplyBitrate(configuration_.target_bps);
    configuration_.SetStreamState(true);
  } else {
    configuration_.SetStreamState(false);
  }
}

void MFH264EncoderImpl::LayerConfig::SetStreamState(bool send_stream) {
  if (send_stream && !sending) {
    // Need a key frame if we have not sent this stream before.
    key_frame_request = true;
  }
  sending = send_stream;
}

}  // namespace webrtc
