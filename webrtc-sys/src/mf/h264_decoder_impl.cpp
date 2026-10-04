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

#include "h264_decoder_impl.h"

#include <codecapi.h>
#include <wmcodecdsp.h>

#include <algorithm>
#include <chrono>
#include <cstring>

#include <api/video/i420_buffer.h>
#include <api/video/video_codec_type.h>
#include <modules/video_coding/include/video_error_codes.h>
#include <third_party/libyuv/include/libyuv/convert.h>

#include "rtc_base/checks.h"
#include "rtc_base/logging.h"

namespace webrtc {

using livekit_ffi::ComPtr;
using livekit_ffi::HResultToString;

namespace {

// RTP video timestamps run at 90 kHz; MF sample times are 100 ns units. The
// timestamp is only used to carry the RTP value through the transform, so the
// conversion just needs to round-trip exactly for 32-bit inputs.
int64_t RtpToSampleTime(uint32_t rtp_timestamp) {
  return static_cast<int64_t>(rtp_timestamp) * 1000 / 9;
}

uint32_t SampleTimeToRtp(int64_t sample_time) {
  return static_cast<uint32_t>((sample_time * 9 + 500) / 1000);
}

// Lost or failed hardware: VideoDecoderSoftwareFallbackWrapper switches to
// the internal decoder on WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE. Anything else
// is a plain error, after which webrtc requests a keyframe.
int32_t DecodeFailure(HRESULT hr) {
  switch (hr) {
    case DXGI_ERROR_DEVICE_REMOVED:
    case DXGI_ERROR_DEVICE_RESET:
    case DXGI_ERROR_DEVICE_HUNG:
    case DXGI_ERROR_DRIVER_INTERNAL_ERROR:
    case MF_E_HW_MFT_FAILED_START_STREAMING:
    case E_OUTOFMEMORY:
      return WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE;
    default:
      return WEBRTC_VIDEO_CODEC_ERROR;
  }
}

// A hardware failure that is not in DecodeFailure's list still returns plain
// errors forever; after this many failed Decode() calls, or failed keyframes,
// without a decoded frame in between the decoder asks for the software one.
constexpr int kMaxConsecutiveDecodeErrors = 30;
constexpr int kMaxFailedKeyframes = 2;

// Test-only: once LK_MF_FAULT_DECODE_AFTER_FRAMES=N frames have been decoded,
// LK_MF_FAULT_DECODE_MODE picks the failure:
//   device-removed (default)  ProcessInput reports DXGI_ERROR_DEVICE_REMOVED.
//   unlisted                  ProcessInput reports E_FAIL.
//   renegotiate               ProcessOutput reports a stream change whose
//                             output renegotiation fails.
enum class DecodeFaultMode { kDeviceRemoved, kUnlisted, kRenegotiate };

struct DecodeFault {
  std::optional<int64_t> after_frames;
  DecodeFaultMode mode = DecodeFaultMode::kDeviceRemoved;
};

const DecodeFault& DecodeFaults() {
  static const DecodeFault fault = [] {
    DecodeFault f;
    f.after_frames = livekit_ffi::GetEnvInt("LK_MF_FAULT_DECODE_AFTER_FRAMES");
    std::optional<std::string> mode =
        livekit_ffi::GetEnvVar("LK_MF_FAULT_DECODE_MODE");
    if (mode && *mode == "unlisted") {
      f.mode = DecodeFaultMode::kUnlisted;
    } else if (mode && *mode == "renegotiate") {
      f.mode = DecodeFaultMode::kRenegotiate;
    }
    if (f.after_frames) {
      RTC_LOG(LS_WARNING) << "MF decoder fault injection active: mode "
                          << static_cast<int>(f.mode) << " after "
                          << *f.after_frames << " frames";
    }
    return f;
  }();
  return fault;
}

bool FaultReached(DecodeFaultMode mode, int64_t frames_decoded) {
  return DecodeFaults().after_frames && DecodeFaults().mode == mode &&
         frames_decoded >= *DecodeFaults().after_frames;
}

// What a configured decoder holds, released in this order on the deferred
// release thread: on unsubscribe WebRTC's worker thread waits for Release().
struct DecoderResources {
  ComPtr<IMFTransform> transform;
  ComPtr<ID3D11Texture2D> staging_texture;
  livekit_ffi::D3D11GpuFence readback_fence;
  livekit_ffi::SharedD3D11Device d3d;
  DWORD input_stream_id = 0;

  bool empty() const {
    return !transform && !staging_texture && !readback_fence && !d3d;
  }

  void Shutdown() {
    if (transform) {
      transform->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM,
                                input_stream_id);
      transform->ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
      transform->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
      transform->ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, 0);
    }
    staging_texture.Reset();
    transform.Reset();
    readback_fence.Reset();
    d3d.reset();
  }
};

}  // namespace

MFH264DecoderImpl::MFH264DecoderImpl() : buffer_pool_(false) {}

MFH264DecoderImpl::~MFH264DecoderImpl() {
  Release();
}

VideoDecoder::DecoderInfo MFH264DecoderImpl::GetDecoderInfo() const {
  // Without a usable DXVA path the MFT decodes on the CPU even when it was
  // handed a device manager, so report what the output buffers show.
  VideoDecoder::DecoderInfo info;
  if (!transform_ && !output_is_dxgi_) {
    // Not configured yet. VideoDecoderSoftwareFallbackWrapper caches its
    // "fallback from" name from this, so do not claim either mode.
    info.implementation_name = "MediaFoundation H264 Decoder";
    info.is_hardware_accelerated = true;
    return info;
  }
  const bool hardware = output_is_dxgi_.value_or(use_d3d_);
  info.implementation_name = hardware
                                 ? "MediaFoundation H264 Decoder (DXVA)"
                                 : "MediaFoundation H264 Decoder (software)";
  info.is_hardware_accelerated = hardware;
  return info;
}

HRESULT MFH264DecoderImpl::SetupD3D() {
  readback_fence_.Reset();
  HRESULT hr = livekit_ffi::AcquireD3D11Device(
      nullptr, livekit_ffi::D3D11DeviceUser::kDecoder, &d3d_);
  if (FAILED(hr)) {
    return hr;
  }
  hr = transform_->ProcessMessage(
      MFT_MESSAGE_SET_D3D_MANAGER,
      reinterpret_cast<ULONG_PTR>(d3d_->manager.Get()));
  if (FAILED(hr)) {
    return hr;
  }
  readback_fence_.Init(d3d_->device.Get(), d3d_->context.Get());
  return S_OK;
}

HRESULT MFH264DecoderImpl::MapStaging(D3D11_MAPPED_SUBRESOURCE* mapped) {
  const auto deadline =
      std::chrono::steady_clock::now() + std::chrono::milliseconds(200);
  if (const UINT64 value = readback_fence_.Signal()) {
    readback_fence_.WaitUntil(value, deadline);
  }
  d3d_->context->Flush();
  for (int attempt = 0;; attempt++) {
    HRESULT hr = d3d_->context->Map(staging_texture_.Get(), 0, D3D11_MAP_READ,
                                    D3D11_MAP_FLAG_DO_NOT_WAIT, mapped);
    if (hr != DXGI_ERROR_WAS_STILL_DRAWING) {
      return hr;
    }
    if (std::chrono::steady_clock::now() >= deadline) {
      return d3d_->context->Map(staging_texture_.Get(), 0, D3D11_MAP_READ, 0,
                                mapped);
    }
    if (attempt < 64) {
      YieldProcessor();
    } else if (attempt < 256) {
      SwitchToThread();
    } else {
      Sleep(1);
    }
  }
}

bool MFH264DecoderImpl::Configure(const Settings& settings) {
  if (settings.codec_type() != kVideoCodecH264) {
    RTC_LOG(LS_ERROR)
        << "initialization failed on codectype is not kVideoCodecH264";
    return false;
  }
  if (!settings.max_render_resolution().Valid()) {
    RTC_LOG(LS_ERROR)
        << "initialization failed on codec_settings width < 0 or height < 0";
    return false;
  }

  settings_ = settings;
  // Kept across Release() so the fallback wrapper's "fallback from" name
  // still says what the hardware decoder was doing.
  output_is_dxgi_.reset();

  if (!livekit_ffi::EnsureComInitialized() || !livekit_ffi::EnsureMFStarted()) {
    return false;
  }

  HRESULT hr =
      CoCreateInstance(CLSID_CMSH264DecoderMFT, nullptr, CLSCTX_INPROC_SERVER,
                       IID_PPV_ARGS(&transform_));
  if (FAILED(hr)) {
    RTC_LOG(LS_ERROR) << "Failed to create inbox H264 decoder MFT: "
                      << HResultToString(hr);
    return false;
  }

  DWORD input_ids[1] = {0};
  DWORD output_ids[1] = {0};
  hr = transform_->GetStreamIDs(1, input_ids, 1, output_ids);
  if (hr == E_NOTIMPL) {
    input_ids[0] = 0;
    output_ids[0] = 0;
  } else if (FAILED(hr)) {
    return false;
  }
  input_stream_id_ = input_ids[0];
  output_stream_id_ = output_ids[0];

  // Without the D3D manager the inbox MFT decodes in software; that still
  // works, so treat D3D failure (headless boxes, RDP sessions, broken
  // drivers) as a downgrade rather than an error.
  hr = SetupD3D();
  use_d3d_ = SUCCEEDED(hr);
  if (!use_d3d_) {
    RTC_LOG(LS_WARNING)
        << "D3D11 unavailable for H264 decode, falling back to software: "
        << HResultToString(hr);
    readback_fence_.Reset();
    d3d_.reset();
  }

  // Cap internal reordering/buffering so frames come out ~1-in/1-out.
  ComPtr<IMFAttributes> attributes;
  if (SUCCEEDED(transform_->GetAttributes(&attributes)) && attributes) {
    attributes->SetUINT32(MF_LOW_LATENCY, TRUE);
  }
  ComPtr<ICodecAPI> codec_api;
  if (SUCCEEDED(transform_.As(&codec_api))) {
    VARIANT v = {};
    v.vt = VT_BOOL;
    v.boolVal = VARIANT_TRUE;
    codec_api->SetValue(&CODECAPI_AVLowLatencyMode, &v);
  }

  ComPtr<IMFMediaType> input_type;
  hr = MFCreateMediaType(&input_type);
  if (FAILED(hr)) {
    return false;
  }
  input_type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
  input_type->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_H264);
  const RenderResolution& resolution = settings.max_render_resolution();
  MFSetAttributeSize(input_type.Get(), MF_MT_FRAME_SIZE,
                     static_cast<UINT32>(resolution.Width()),
                     static_cast<UINT32>(resolution.Height()));
  hr = transform_->SetInputType(input_stream_id_, input_type.Get(), 0);
  if (FAILED(hr)) {
    RTC_LOG(LS_ERROR) << "Decoder SetInputType failed: "
                      << HResultToString(hr);
    return false;
  }

  hr = NegotiateOutputType();
  if (FAILED(hr)) {
    RTC_LOG(LS_ERROR) << "Decoder output negotiation failed: "
                      << HResultToString(hr);
    return false;
  }

  transform_->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
  transform_->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);

  RTC_LOG(LS_INFO) << "MediaFoundation H264 decoder initialized ("
                   << (use_d3d_ ? "DXVA hardware" : "software") << ").";
  return true;
}

HRESULT MFH264DecoderImpl::NegotiateOutputType() {
  HRESULT hr = E_FAIL;
  for (DWORD i = 0;; i++) {
    ComPtr<IMFMediaType> candidate;
    hr = transform_->GetOutputAvailableType(output_stream_id_, i, &candidate);
    if (FAILED(hr)) {
      return hr;
    }
    GUID subtype = {};
    candidate->GetGUID(MF_MT_SUBTYPE, &subtype);
    if (subtype != MFVideoFormat_NV12) {
      continue;
    }
    hr = transform_->SetOutputType(output_stream_id_, candidate.Get(), 0);
    if (FAILED(hr)) {
      return hr;
    }
    break;
  }

  ComPtr<IMFMediaType> current;
  hr = transform_->GetOutputCurrentType(output_stream_id_, &current);
  if (FAILED(hr)) {
    return hr;
  }
  hr = MFGetAttributeSize(current.Get(), MF_MT_FRAME_SIZE, &coded_width_,
                          &coded_height_);
  if (FAILED(hr)) {
    return hr;
  }

  // 1080p decodes as 1920x1088 coded size; the display aperture carries the
  // real dimensions.
  has_aperture_ =
      SUCCEEDED(current->GetBlob(MF_MT_MINIMUM_DISPLAY_APERTURE,
                                 reinterpret_cast<UINT8*>(&display_aperture_),
                                 sizeof(display_aperture_), nullptr));

  UINT32 stride = 0;
  if (SUCCEEDED(current->GetUINT32(MF_MT_DEFAULT_STRIDE, &stride)) &&
      static_cast<INT32>(stride) > 0) {
    default_stride_ = stride;
  } else {
    default_stride_ = coded_width_;
  }

  // Coded size may have changed; the staging texture is recreated lazily.
  staging_texture_.Reset();
  return S_OK;
}

int32_t MFH264DecoderImpl::RegisterDecodeCompleteCallback(
    DecodedImageCallback* callback) {
  this->decoded_complete_callback_ = callback;
  return WEBRTC_VIDEO_CODEC_OK;
}

int32_t MFH264DecoderImpl::Release() {
  auto resources = std::make_shared<DecoderResources>();
  resources->transform = std::move(transform_);
  resources->staging_texture = std::move(staging_texture_);
  resources->readback_fence = std::move(readback_fence_);
  resources->d3d = std::move(d3d_);
  resources->input_stream_id = input_stream_id_;
  if (!resources->empty()) {
    livekit_ffi::MFDeferredReleases().Post(
        [resources] { resources->Shutdown(); });
  }
  use_d3d_ = false;
  frames_decoded_ = 0;
  consecutive_errors_ = 0;
  failed_keyframes_ = 0;
  buffer_pool_.Release();
  return WEBRTC_VIDEO_CODEC_OK;
}

int32_t MFH264DecoderImpl::Decode(const EncodedImage& input_image,
                                  bool missing_frames,
                                  int64_t render_time_ms) {
  const int64_t decoded_before = frames_decoded_;
  const int32_t ret = DecodeFrame(input_image);
  if (frames_decoded_ != decoded_before) {
    consecutive_errors_ = 0;
    failed_keyframes_ = 0;
  }
  if (ret != WEBRTC_VIDEO_CODEC_ERROR) {
    return ret;
  }
  consecutive_errors_++;
  if (input_image._frameType == VideoFrameType::kVideoFrameKey) {
    failed_keyframes_++;
  }
  if (consecutive_errors_ >= kMaxConsecutiveDecodeErrors ||
      failed_keyframes_ >= kMaxFailedKeyframes) {
    RTC_LOG(LS_ERROR) << "MF H264 decoder failed " << consecutive_errors_
                      << " frames (" << failed_keyframes_
                      << " keyframes) without output; falling back to "
                         "software.";
    return WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE;
  }
  return ret;
}

int32_t MFH264DecoderImpl::DecodeFrame(const EncodedImage& input_image) {
  if (!transform_) {
    RTC_LOG(LS_ERROR) << "decode failed: decoder not configured";
    return WEBRTC_VIDEO_CODEC_UNINITIALIZED;
  }
  if (decoded_complete_callback_ == nullptr) {
    RTC_LOG(LS_ERROR) << "decode failed on not set decoded_complete_callback";
    return WEBRTC_VIDEO_CODEC_UNINITIALIZED;
  }
  if (!input_image.data() || !input_image.size()) {
    RTC_LOG(LS_ERROR) << "decode failed on input image is null";
    return WEBRTC_VIDEO_CODEC_ERR_PARAMETER;
  }

  h264_bitstream_parser_.ParseBitstream(input_image);
  std::optional<int> qp = h264_bitstream_parser_.GetLastSliceQp();

  // Pass on color space from the input frame if explicitly specified.
  if (input_image.ColorSpace()) {
    input_color_space_ = *input_image.ColorSpace();
  }

  ComPtr<IMFMediaBuffer> buffer;
  HRESULT hr = MFCreateMemoryBuffer(
      static_cast<DWORD>(input_image.size()), &buffer);
  if (FAILED(hr)) {
    return WEBRTC_VIDEO_CODEC_ERROR;
  }
  BYTE* data = nullptr;
  hr = buffer->Lock(&data, nullptr, nullptr);
  if (FAILED(hr)) {
    return WEBRTC_VIDEO_CODEC_ERROR;
  }
  std::memcpy(data, input_image.data(), input_image.size());
  buffer->Unlock();
  buffer->SetCurrentLength(static_cast<DWORD>(input_image.size()));

  ComPtr<IMFSample> sample;
  hr = MFCreateSample(&sample);
  if (FAILED(hr)) {
    return WEBRTC_VIDEO_CODEC_ERROR;
  }
  sample->AddBuffer(buffer.Get());
  sample->SetSampleTime(RtpToSampleTime(input_image.RtpTimestamp()));

  for (int attempt = 0; attempt < 2; attempt++) {
    const bool unlisted =
        FaultReached(DecodeFaultMode::kUnlisted, frames_decoded_);
    if (unlisted ||
        FaultReached(DecodeFaultMode::kDeviceRemoved, frames_decoded_)) {
      hr = unlisted ? E_FAIL : DXGI_ERROR_DEVICE_REMOVED;
      RTC_LOG(LS_WARNING) << "LK_MF_FAULT_DECODE_AFTER_FRAMES: simulating "
                          << HResultToString(hr) << " after "
                          << frames_decoded_ << " frames.";
      break;
    }
    hr = transform_->ProcessInput(input_stream_id_, sample.Get(), 0);
    if (hr != MF_E_NOTACCEPTING) {
      break;
    }
    // The transform is full; empty it and try once more.
    int32_t ret = DrainOutputs(qp);
    if (ret != WEBRTC_VIDEO_CODEC_OK) {
      return ret;
    }
  }
  if (FAILED(hr)) {
    RTC_LOG(LS_ERROR) << "Decoder ProcessInput failed: "
                      << HResultToString(hr);
    return DecodeFailure(hr);
  }

  return DrainOutputs(qp);
}

int32_t MFH264DecoderImpl::DrainOutputs(std::optional<int> qp) {
  for (;;) {
    MFT_OUTPUT_STREAM_INFO stream_info = {};
    HRESULT hr =
        transform_->GetOutputStreamInfo(output_stream_id_, &stream_info);
    if (FAILED(hr)) {
      RTC_LOG(LS_ERROR) << "Decoder GetOutputStreamInfo failed: "
                        << HResultToString(hr);
      return WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE;
    }
    const bool transform_allocates =
        stream_info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES |
                               MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES);

    ComPtr<IMFSample> allocated;
    if (!transform_allocates) {
      hr = MFCreateSample(&allocated);
      if (FAILED(hr)) {
        return WEBRTC_VIDEO_CODEC_ERROR;
      }
      ComPtr<IMFMediaBuffer> out_buffer;
      const DWORD size =
          stream_info.cbSize
              ? stream_info.cbSize
              : coded_width_ * coded_height_ * 3 / 2;
      hr = MFCreateAlignedMemoryBuffer(
          size, stream_info.cbAlignment > 1 ? stream_info.cbAlignment - 1 : 0,
          &out_buffer);
      if (FAILED(hr)) {
        return WEBRTC_VIDEO_CODEC_ERROR;
      }
      allocated->AddBuffer(out_buffer.Get());
    }

    MFT_OUTPUT_DATA_BUFFER output = {};
    output.dwStreamID = output_stream_id_;
    output.pSample = transform_allocates ? nullptr : allocated.Get();
    DWORD status = 0;
    const bool renegotiate_fault =
        FaultReached(DecodeFaultMode::kRenegotiate, frames_decoded_);
    if (renegotiate_fault) {
      hr = MF_E_TRANSFORM_STREAM_CHANGE;
    } else {
      hr = transform_->ProcessOutput(0, 1, &output, &status);
      if (output.pEvents) {
        output.pEvents->Release();
      }
    }

    if (hr == MF_E_TRANSFORM_NEED_MORE_INPUT) {
      return WEBRTC_VIDEO_CODEC_OK;
    }
    if (hr == MF_E_TRANSFORM_STREAM_CHANGE) {
      HRESULT nhr =
          renegotiate_fault ? MF_E_INVALIDMEDIATYPE : NegotiateOutputType();
      if (FAILED(nhr)) {
        RTC_LOG(LS_ERROR) << "Decoder output renegotiation failed: "
                          << HResultToString(nhr);
        return WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE;
      }
      continue;
    }
    if (FAILED(hr)) {
      RTC_LOG(LS_ERROR) << "Decoder ProcessOutput failed: "
                        << HResultToString(hr);
      return DecodeFailure(hr);
    }

    ComPtr<IMFSample> sample;
    if (transform_allocates) {
      sample.Attach(output.pSample);
    } else {
      sample = allocated;
    }
    if (!sample) {
      continue;
    }
    int32_t ret = DeliverSample(sample.Get(), qp);
    if (ret != WEBRTC_VIDEO_CODEC_OK) {
      return ret;
    }
  }
}

int32_t MFH264DecoderImpl::DeliverSample(IMFSample* sample,
                                         std::optional<int> qp) {
  int64_t sample_time = 0;
  sample->GetSampleTime(&sample_time);
  const uint32_t rtp_timestamp = SampleTimeToRtp(sample_time);

  ComPtr<IMFMediaBuffer> buffer;
  HRESULT hr = sample->GetBufferByIndex(0, &buffer);
  if (FAILED(hr)) {
    return WEBRTC_VIDEO_CODEC_ERROR;
  }

  // Crop offsets within the coded frame (1088-line 1080p etc.).
  const UINT32 crop_x = has_aperture_ ? display_aperture_.OffsetX.value : 0;
  const UINT32 crop_y = has_aperture_ ? display_aperture_.OffsetY.value : 0;

  ComPtr<IMFDXGIBuffer> dxgi_buffer;
  const bool is_dxgi = use_d3d_ && SUCCEEDED(buffer.As(&dxgi_buffer));
  if (output_is_dxgi_ != is_dxgi) {
    RTC_LOG(LS_INFO) << "MF H264 decoder output is "
                     << (is_dxgi ? "DXVA (D3D11 surfaces)"
                                 : "system memory (software decode)");
    output_is_dxgi_ = is_dxgi;
  }
  frames_decoded_++;
  if (is_dxgi) {
    ComPtr<ID3D11Texture2D> texture;
    hr = dxgi_buffer->GetResource(IID_PPV_ARGS(&texture));
    if (FAILED(hr)) {
      return DecodeFailure(hr);
    }
    UINT subresource = 0;
    dxgi_buffer->GetSubresourceIndex(&subresource);

    D3D11_TEXTURE2D_DESC desc = {};
    texture->GetDesc(&desc);

    if (staging_texture_) {
      D3D11_TEXTURE2D_DESC staging_desc = {};
      staging_texture_->GetDesc(&staging_desc);
      if (staging_desc.Width != desc.Width ||
          staging_desc.Height != desc.Height) {
        staging_texture_.Reset();
      }
    }
    if (!staging_texture_) {
      D3D11_TEXTURE2D_DESC staging_desc = {};
      staging_desc.Width = desc.Width;
      staging_desc.Height = desc.Height;
      staging_desc.MipLevels = 1;
      staging_desc.ArraySize = 1;
      staging_desc.Format = DXGI_FORMAT_NV12;
      staging_desc.SampleDesc.Count = 1;
      staging_desc.Usage = D3D11_USAGE_STAGING;
      staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
      hr = d3d_->device->CreateTexture2D(&staging_desc, nullptr,
                                         &staging_texture_);
      if (FAILED(hr)) {
        RTC_LOG(LS_ERROR) << "Failed to create staging texture: "
                          << HResultToString(hr);
        return WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE;
      }
    }

    // GPU -> CPU readback: the known copy-back cost of this design.
    d3d_->context->CopySubresourceRegion(staging_texture_.Get(), 0, 0, 0, 0,
                                         texture.Get(), subresource, nullptr);
    D3D11_MAPPED_SUBRESOURCE mapped = {};
    hr = MapStaging(&mapped);
    if (FAILED(hr)) {
      RTC_LOG(LS_ERROR) << "Decoder staging Map failed: "
                        << HResultToString(hr);
      return WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE;
    }
    const uint8_t* base = static_cast<const uint8_t*>(mapped.pData);
    const uint8_t* data_y = base + crop_y * mapped.RowPitch + crop_x;
    const uint8_t* data_uv = base +
                             static_cast<size_t>(mapped.RowPitch) *
                                 desc.Height +
                             (crop_y / 2) * mapped.RowPitch + crop_x;
    int32_t ret = DeliverNV12(data_y, mapped.RowPitch, data_uv,
                              mapped.RowPitch, rtp_timestamp, qp);
    d3d_->context->Unmap(staging_texture_.Get(), 0);
    return ret;
  }

  // Software path: system-memory NV12.
  ComPtr<IMF2DBuffer> buffer_2d;
  if (SUCCEEDED(buffer.As(&buffer_2d))) {
    BYTE* scanline0 = nullptr;
    LONG pitch = 0;
    hr = buffer_2d->Lock2D(&scanline0, &pitch);
    if (FAILED(hr) || pitch <= 0) {
      if (SUCCEEDED(hr)) {
        buffer_2d->Unlock2D();
      }
      return WEBRTC_VIDEO_CODEC_ERROR;
    }
    const uint8_t* data_y = scanline0 + crop_y * pitch + crop_x;
    const uint8_t* data_uv = scanline0 +
                             static_cast<size_t>(pitch) * coded_height_ +
                             (crop_y / 2) * pitch + crop_x;
    int32_t ret =
        DeliverNV12(data_y, pitch, data_uv, pitch, rtp_timestamp, qp);
    buffer_2d->Unlock2D();
    return ret;
  }

  ComPtr<IMFMediaBuffer> contiguous;
  hr = sample->ConvertToContiguousBuffer(&contiguous);
  if (FAILED(hr)) {
    return WEBRTC_VIDEO_CODEC_ERROR;
  }
  BYTE* data = nullptr;
  DWORD length = 0;
  hr = contiguous->Lock(&data, nullptr, &length);
  if (FAILED(hr)) {
    return WEBRTC_VIDEO_CODEC_ERROR;
  }
  const UINT32 stride = default_stride_;
  const uint8_t* data_y = data + crop_y * stride + crop_x;
  const uint8_t* data_uv = data + static_cast<size_t>(stride) * coded_height_ +
                           (crop_y / 2) * stride + crop_x;
  int32_t ret = DeliverNV12(data_y, stride, data_uv, stride, rtp_timestamp, qp);
  contiguous->Unlock();
  return ret;
}

int32_t MFH264DecoderImpl::DeliverNV12(const uint8_t* data_y,
                                       int stride_y,
                                       const uint8_t* data_uv,
                                       int stride_uv,
                                       uint32_t rtp_timestamp,
                                       std::optional<int> qp) {
  const int width = has_aperture_ ? display_aperture_.Area.cx : coded_width_;
  const int height = has_aperture_ ? display_aperture_.Area.cy : coded_height_;

  webrtc::scoped_refptr<webrtc::I420Buffer> i420_buffer =
      buffer_pool_.CreateI420Buffer(width, height);
  if (!i420_buffer) {
    return WEBRTC_VIDEO_CODEC_ERROR;
  }

  int result = libyuv::NV12ToI420(
      data_y, stride_y, data_uv, stride_uv, i420_buffer->MutableDataY(),
      i420_buffer->StrideY(), i420_buffer->MutableDataU(),
      i420_buffer->StrideU(), i420_buffer->MutableDataV(),
      i420_buffer->StrideV(), width, height);
  if (result) {
    RTC_LOG(LS_INFO) << "libyuv::NV12ToI420 failed. error:" << result;
  }

  VideoFrame decoded_frame = VideoFrame::Builder()
                                 .set_video_frame_buffer(i420_buffer)
                                 .set_timestamp_rtp(rtp_timestamp)
                                 .set_color_space(input_color_space_)
                                 .build();

  std::optional<int32_t> decode_time;
  decoded_complete_callback_->Decoded(decoded_frame, decode_time, qp);
  return WEBRTC_VIDEO_CODEC_OK;
}

}  // namespace webrtc
