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

#pragma once

#include <cstdint>
#include <memory>
#include <optional>
#include <vector>

#include "api/task_queue/task_queue_base.h"
#include "api/video_codecs/video_encoder.h"
#include "api/video_codecs/video_encoder_factory.h"

namespace livekit_ffi {
enum class VideoEncoderBackend : std::int32_t;

struct VideoEncoderBackendFactory {
  VideoEncoderBackend backend;
  std::unique_ptr<webrtc::VideoEncoderFactory> factory;
};

// A sender's first encoder is created from the format libwebrtc picked from
// GetImplementations() (the first, i.e. hardware, implementation) before the
// sender's encoder selector can request its backend, so a Software sender
// used to open and InitEncode a hardware session and then switch. The
// selector learns of that encoder (OnCurrentEncoder) on the encoder task
// queue before SimulcastEncoderAdapter creates the per-layer encoders on the
// same queue, so it records its backend here and creates on that queue use
// it in place of the format's tag.
void SetEncoderBackendForTaskQueue(webrtc::TaskQueueBase* queue,
                                   const void* owner,
                                   VideoEncoderBackend backend);
void ClearEncoderBackendForTaskQueue(webrtc::TaskQueueBase* queue,
                                     const void* owner);

class VideoEncoderFactory : public webrtc::VideoEncoderFactory {
  // Hands SimulcastEncoderAdapter a software encoder for each hardware layer;
  // SEA wraps the pair in VideoEncoderSoftwareFallbackWrapper, so a layer
  // whose hardware encoder fails InitEncode or returns
  // WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE keeps encoding in software.
  class SoftwareFallbackFactory : public webrtc::VideoEncoderFactory {
   public:
    std::vector<webrtc::SdpVideoFormat> GetSupportedFormats() const override;

    std::unique_ptr<webrtc::VideoEncoder> Create(
        const webrtc::Environment& env, const webrtc::SdpVideoFormat& format) override;
  };

  class InternalFactory : public webrtc::VideoEncoderFactory {
   public:
    InternalFactory();

    std::vector<webrtc::SdpVideoFormat> GetSupportedFormats() const override;

    std::vector<webrtc::SdpVideoFormat> GetImplementations() const override;

    CodecSupport QueryCodecSupport(
        const webrtc::SdpVideoFormat& format,
        std::optional<std::string> scalability_mode) const override;

    std::unique_ptr<webrtc::VideoEncoder> Create(
        const webrtc::Environment& env, const webrtc::SdpVideoFormat& format) override;

   private:
    std::vector<VideoEncoderBackendFactory> factories_;
  };

 public:
  VideoEncoderFactory();

  std::vector<webrtc::SdpVideoFormat> GetSupportedFormats() const override;

  std::vector<webrtc::SdpVideoFormat> GetImplementations() const override;

  CodecSupport QueryCodecSupport(
      const webrtc::SdpVideoFormat& format,
      std::optional<std::string> scalability_mode) const override;

  std::unique_ptr<webrtc::VideoEncoder> Create(
      const webrtc::Environment& env, const webrtc::SdpVideoFormat& format) override;

 private:
  std::unique_ptr<InternalFactory> internal_factory_;
  std::unique_ptr<SoftwareFallbackFactory> software_fallback_factory_;
};
}  // namespace livekit_ffi
