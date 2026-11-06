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

// Scenarios for mf_encoder_tests.rs (the `harness` feature), over the real MF
// H.264 encoder on this machine's GPU.

#include <chrono>
#include <cstdio>
#include <cstring>
#include <memory>
#include <sstream>
#include <string>
#include <thread>
#include <vector>

#include <api/environment/environment.h>
#include <api/environment/environment_factory.h>
#include <api/video/encoded_image.h>
#include <api/video/i420_buffer.h>
#include <api/video/video_bitrate_allocation.h>
#include <api/video/video_frame.h>
#include <api/video_codecs/sdp_video_format.h>
#include <api/video_codecs/video_codec.h>
#include <api/video_codecs/video_encoder.h>
#include <modules/video_coding/include/video_error_codes.h>

#include "mf_encoder_factory.h"

namespace {

class Checks {
 public:
  void Expect(bool ok, const std::string& what) {
    if (!ok) {
      failures_ << what << "; ";
      failed_ = true;
    }
  }
  bool failed() const { return failed_; }
  std::string text() const { return failures_.str(); }

 private:
  std::ostringstream failures_;
  bool failed_ = false;
};

std::string Code(int32_t code) {
  switch (code) {
    case WEBRTC_VIDEO_CODEC_OK:
      return "OK";
    case WEBRTC_VIDEO_CODEC_NO_OUTPUT:
      return "NO_OUTPUT";
    case WEBRTC_VIDEO_CODEC_ERROR:
      return "ERROR";
    case WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE:
      return "FALLBACK_SOFTWARE";
    default:
      return std::to_string(code);
  }
}

// Refuses the first `refuse` images the way RtpVideoSender refuses a frame for
// a simulcast layer that stopped sending, then accepts the rest.
class RefusingSink : public webrtc::EncodedImageCallback {
 public:
  explicit RefusingSink(int refuse) : refuse_(refuse) {}

  Result OnEncodedImage(const webrtc::EncodedImage& image,
                        const webrtc::CodecSpecificInfo* info) override {
    if (refused_ < refuse_) {
      refused_++;
      return Result(Result::ERROR_SEND_FAILED);
    }
    accepted_++;
    return Result(Result::OK);
  }
  void OnFrameDropped(uint32_t rtp_timestamp,
                      int spatial_id,
                      bool is_end_of_temporal_unit) override {}

  int refused() const { return refused_; }
  int accepted() const { return accepted_; }

 private:
  const int refuse_;
  int refused_ = 0;
  int accepted_ = 0;
};

webrtc::VideoCodec H264Codec(int width, int height) {
  webrtc::VideoCodec codec;
  codec.codecType = webrtc::kVideoCodecH264;
  codec.width = width;
  codec.height = height;
  codec.maxFramerate = 30;
  codec.startBitrate = 1000;
  codec.minBitrate = 100;
  codec.maxBitrate = 2000;
  codec.qpMax = 51;
  codec.numberOfSimulcastStreams = 1;
  codec.simulcastStream[0].width = width;
  codec.simulcastStream[0].height = height;
  codec.simulcastStream[0].maxFramerate = 30;
  codec.simulcastStream[0].numberOfTemporalLayers = 1;
  codec.simulcastStream[0].maxBitrate = 2000;
  codec.simulcastStream[0].targetBitrate = 1000;
  codec.simulcastStream[0].minBitrate = 100;
  codec.simulcastStream[0].qpMax = 51;
  codec.simulcastStream[0].active = true;
  codec.SetFrameDropEnabled(false);
  return codec;
}

// A frame the encoder delivered but the sink refused is not an encode error:
// Encode() keeps returning OK (or NO_OUTPUT while a session opens), and the
// encoder keeps delivering once the sink accepts again. Without a task queue
// the encoder collects output inside Encode(), so a refused frame can be the
// one Encode() just submitted, the case where SimulcastEncoderAdapter would
// pop a timestamp its callback already consumed.
std::string RefusedSend(Checks& c) {
  if (!webrtc::MFVideoEncoderFactory::IsSupported()) {
    return "skipped";
  }
  constexpr int kWidth = 640;
  constexpr int kHeight = 360;
  constexpr int kRefused = 10;
  constexpr int kAccepted = 20;
  constexpr int kMaxFrames = 400;
  constexpr auto kDeadline = std::chrono::seconds(5);

  const webrtc::Environment env = webrtc::CreateEnvironment();
  webrtc::MFVideoEncoderFactory factory;
  const std::vector<webrtc::SdpVideoFormat> formats =
      factory.GetSupportedFormats();
  if (formats.empty()) {
    return "skipped";
  }
  std::unique_ptr<webrtc::VideoEncoder> encoder =
      factory.Create(env, formats[0]);
  if (!encoder) {
    c.Expect(false, "factory created no encoder");
    return std::string();
  }
  webrtc::VideoCodec codec = H264Codec(kWidth, kHeight);
  const int32_t init = encoder->InitEncode(
      &codec, webrtc::VideoEncoder::Settings(
                  webrtc::VideoEncoder::Capabilities(false), 1, 1200));
  if (init != WEBRTC_VIDEO_CODEC_OK) {
    return "skipped";
  }
  RefusingSink sink(kRefused);
  encoder->RegisterEncodeCompleteCallback(&sink);
  webrtc::VideoBitrateAllocation allocation;
  allocation.SetBitrate(0, 0, 1000000);
  encoder->SetRates(
      webrtc::VideoEncoder::RateControlParameters(allocation, 30.0));

  std::vector<std::string> errors;
  const auto start = std::chrono::steady_clock::now();
  int frames = 0;
  while (frames < kMaxFrames && sink.accepted() < kAccepted &&
         std::chrono::steady_clock::now() - start < kDeadline) {
    webrtc::scoped_refptr<webrtc::I420Buffer> buffer =
        webrtc::I420Buffer::Create(kWidth, kHeight);
    webrtc::I420Buffer::SetBlack(buffer.get());
    std::memset(
        buffer->MutableDataY() + (frames * 7 % kHeight) * buffer->StrideY(),
        235, kWidth);
    webrtc::VideoFrame frame = webrtc::VideoFrame::Builder()
                                   .set_video_frame_buffer(buffer)
                                   .set_timestamp_rtp(3000 * frames)
                                   .set_timestamp_us(33333LL * frames)
                                   .build();
    std::vector<webrtc::VideoFrameType> types = {
        frames == 0 ? webrtc::VideoFrameType::kVideoFrameKey
                    : webrtc::VideoFrameType::kVideoFrameDelta};
    const int32_t result = encoder->Encode(frame, &types);
    if (result != WEBRTC_VIDEO_CODEC_OK &&
        result != WEBRTC_VIDEO_CODEC_NO_OUTPUT) {
      errors.push_back("frame " + std::to_string(frames) + " " + Code(result));
    }
    frames++;
    std::this_thread::sleep_for(std::chrono::milliseconds(10));
  }
  const std::string name = encoder->GetEncoderInfo().implementation_name;
  encoder->Release();

  std::string joined;
  for (const std::string& error : errors) {
    joined += error + " ";
  }
  c.Expect(errors.empty(), "Encode returned " + joined);
  c.Expect(sink.refused() == kRefused,
           "refused " + std::to_string(sink.refused()) + " of " +
               std::to_string(kRefused));
  c.Expect(sink.accepted() >= kAccepted,
           "accepted " + std::to_string(sink.accepted()) + " after " +
               std::to_string(frames) + " frames");
  c.Expect(name.rfind("MediaFoundation H264 Encoder", 0) == 0, "name " + name);
  return name + ", " + std::to_string(frames) + " frames";
}

}  // namespace

// 0 passed, 1 failed, 2 skipped; `message` gets the failures, or a note.
extern "C" int32_t lk_mf_encoder_test(const char* scenario,
                                      char* message,
                                      size_t message_len) {
  Checks checks;
  std::string note;
  const std::string name = scenario ? scenario : "";
  if (name == "refused_send") {
    note = RefusedSend(checks);
  } else {
    checks.Expect(false, "unknown scenario " + name);
  }
  const std::string text = checks.failed() ? checks.text() : note;
  if (message && message_len > 0) {
    std::snprintf(message, message_len, "%s", text.c_str());
  }
  if (checks.failed()) {
    return 1;
  }
  return note == "skipped" ? 2 : 0;
}
