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

#include "mf_encoder_factory.h"

#include <memory>
#include <optional>
#include <string>

#include "api/video_codecs/h264_profile_level_id.h"
#include "h264_encoder_impl.h"
#include "mf_common.h"
#include "modules/video_coding/codecs/h264/include/h264.h"
#include "rtc_base/logging.h"

namespace webrtc {

using livekit_ffi::ComPtr;
using livekit_ffi::HResultToString;

namespace {

struct ProbeResult {
  bool supported = false;
  H264Level level = H264Level::kLevel3_1;
};

// Returns the highest H264 level (of the ones we care about) the MFT accepts,
// by offering output media types with MFT_SET_TYPE_TEST_ONLY at a resolution
// and frame rate that level actually requires. Empty when the MFT rejects
// everything, including the 720p30 / level 3.1 floor.
std::optional<H264Level> ProbeMaxLevel(IMFTransform* transform) {
  DWORD input_ids[1] = {0};
  DWORD output_ids[1] = {0};
  if (transform->GetStreamIDs(1, input_ids, 1, output_ids) == E_NOTIMPL) {
    output_ids[0] = 0;
  }

  struct Candidate {
    H264Level level;
    UINT32 mf_level;
    UINT32 width;
    UINT32 height;
    UINT32 fps;
  };
  const Candidate candidates[] = {
      // 2160p30 and 1440p60.
      {H264Level::kLevel5_1, eAVEncH264VLevel5_1, 3840, 2160, 30},
      {H264Level::kLevel5, eAVEncH264VLevel5, 2560, 1440, 30},
      // 1080p60.
      {H264Level::kLevel4_2, eAVEncH264VLevel4_2, 1920, 1080, 60},
      {H264Level::kLevel4_1, eAVEncH264VLevel4_1, 1920, 1080, 30},
      {H264Level::kLevel4, eAVEncH264VLevel4, 1920, 1080, 30},
      {H264Level::kLevel3_1, eAVEncH264VLevel3_1, 1280, 720, 30},
  };

  for (const Candidate& candidate : candidates) {
    ComPtr<IMFMediaType> type;
    if (FAILED(MFCreateMediaType(&type))) {
      break;
    }
    type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video);
    type->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_H264);
    type->SetUINT32(MF_MT_AVG_BITRATE, 5'000'000);
    MFSetAttributeSize(type.Get(), MF_MT_FRAME_SIZE, candidate.width,
                       candidate.height);
    MFSetAttributeRatio(type.Get(), MF_MT_FRAME_RATE, candidate.fps, 1);
    type->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive);
    type->SetUINT32(MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base);
    type->SetUINT32(MF_MT_MPEG2_LEVEL, candidate.mf_level);

    HRESULT hr = transform->SetOutputType(output_ids[0], type.Get(),
                                          MFT_SET_TYPE_TEST_ONLY);
    if (SUCCEEDED(hr)) {
      return candidate.level;
    }
  }
  return std::nullopt;
}

// Walks the hardware encoder MFTs in the same order MFH264EncoderImpl does
// and reports the level of the first one that activates and accepts an
// output type once prepared the same way (async unlock, D3D11 device manager
// on its own adapter). A registration alone is not enough — old or broken
// drivers register MFTs that fail to activate.
ProbeResult ProbeHardwareEncoders() {
  ProbeResult result;
  if (!livekit_ffi::EnsureComInitialized() || !livekit_ffi::EnsureMFStarted()) {
    return result;
  }

  for (const ComPtr<IMFActivate>& activate :
       livekit_ffi::EnumHardwareH264Encoders()) {
    const std::string name = livekit_ffi::GetFriendlyName(activate.Get());
    ComPtr<IMFTransform> transform;
    HRESULT hr = activate->ActivateObject(IID_PPV_ARGS(&transform));
    if (FAILED(hr)) {
      RTC_LOG(LS_WARNING) << "Hardware H264 encoder MFT \"" << name
                          << "\" failed to activate: " << HResultToString(hr);
      activate->ShutdownObject();
      continue;
    }

    std::optional<H264Level> level;
    {
      bool is_async = false;
      livekit_ffi::D3D11DeviceBundle d3d;
      hr = livekit_ffi::PrepareHardwareTransform(activate.Get(),
                                                 transform.Get(), &is_async,
                                                 &d3d);
      if (SUCCEEDED(hr)) {
        level = ProbeMaxLevel(transform.Get());
      }
      RTC_LOG(LS_INFO) << "Probed hardware H264 encoder MFT \"" << name
                       << "\": async=" << is_async
                       << " d3d11=" << (d3d.manager != nullptr) << " hr="
                       << HResultToString(hr) << " max_level="
                       << (level ? std::to_string(static_cast<int>(*level))
                                 : std::string("none"));
      transform.Reset();
    }
    activate->ShutdownObject();

    if (level) {
      result.supported = true;
      result.level = *level;
      return result;
    }
  }
  return result;
}

const ProbeResult& CachedProbe() {
  // Enumeration + activation can take tens of milliseconds; the answer cannot
  // change within the process lifetime, so probe once.
  static const ProbeResult probe = ProbeHardwareEncoders();
  return probe;
}

}  // namespace

MFVideoEncoderFactory::MFVideoEncoderFactory() {
  const ProbeResult& probe = CachedProbe();
  if (!probe.supported) {
    return;
  }

  // Unlike the NVENC factory's hardcoded 42e01f (level 3.1, a 720p30 cap),
  // advertise the level the hardware actually accepts so 1080p60 and above
  // can be negotiated. Baseline-family only for now, matching the software
  // H264 default; High is a possible later addition.
  supported_formats_.push_back(CreateH264Format(
      H264Profile::kProfileConstrainedBaseline, probe.level, "1"));
}

MFVideoEncoderFactory::~MFVideoEncoderFactory() {}

bool MFVideoEncoderFactory::IsSupported() {
  static const bool supported = [] {
    if (livekit_ffi::EnvFlagSet("LK_DISABLE_MF_ENCODE")) {
      RTC_LOG(LS_WARNING)
          << "LK_DISABLE_MF_ENCODE is set; MediaFoundation encoding disabled.";
      return false;
    }
    if (!CachedProbe().supported) {
      RTC_LOG(LS_WARNING)
          << "No usable hardware H264 encoder MFT; MF encoding disabled.";
      return false;
    }
    RTC_LOG(LS_INFO) << "MediaFoundation hardware H264 encoder is available.";
    return true;
  }();
  return supported;
}

std::unique_ptr<VideoEncoder> MFVideoEncoderFactory::Create(
    const Environment& env,
    const SdpVideoFormat& format) {
  for (const auto& supported_format : supported_formats_) {
    if (format.IsSameCodec(supported_format)) {
      if (format.name == "H264") {
        RTC_LOG(LS_INFO) << "Using MediaFoundation HW encoder for H264";
        return std::make_unique<MFH264EncoderImpl>(env, format);
      }
    }
  }
  return nullptr;
}

std::vector<SdpVideoFormat> MFVideoEncoderFactory::GetSupportedFormats()
    const {
  return supported_formats_;
}

std::vector<SdpVideoFormat> MFVideoEncoderFactory::GetImplementations()
    const {
  return supported_formats_;
}

}  // namespace webrtc
