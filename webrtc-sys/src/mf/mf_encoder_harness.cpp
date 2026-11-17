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
#include <optional>
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

#include <d3d10_1.h>

#include <algorithm>
#include <array>
#include <atomic>
#include <cstdlib>

#include <api/video/encoded_image.h>
#include <api/video_codecs/video_decoder.h>
#include <modules/video_coding/codecs/h264/include/h264.h>
#include <third_party/libyuv/include/libyuv/convert.h>

#include <third_party/libyuv/include/libyuv/convert_from.h>

#include "d3d11_video_converter.h"
#include "h264_encoder_impl.h"
#include "livekit/d3d11_texture_buffer.h"
#include "mf_encoder_factory.h"

namespace {

using livekit_ffi::ComPtr;

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

webrtc::scoped_refptr<webrtc::I420Buffer> StripedFrame(int width,
                                                       int height,
                                                       int index) {
  webrtc::scoped_refptr<webrtc::I420Buffer> buffer =
      webrtc::I420Buffer::Create(width, height);
  webrtc::I420Buffer::SetBlack(buffer.get());
  std::memset(buffer->MutableDataY() + (index * 7 % height) * buffer->StrideY(),
              235, width);
  return buffer;
}

struct StallRun {
  int frames = 0;
  int dropped = 0;
  int accepted_after_stall = 0;
  int fallback_frame = -1;
  std::vector<std::string> errors;
  std::string name;
  bool skipped = false;
};

// Feeds frames at ~30 fps for `run_for` while the harness holds the MFT's
// events back for `stall_ms` from frame 30, as a game holding the GPU would.
StallRun RunWithStall(int stall_ms, std::chrono::milliseconds run_for) {
  StallRun run;
  constexpr int kWidth = 640;
  constexpr int kHeight = 360;
  constexpr int kStallAfter = 30;
  if (!webrtc::MFVideoEncoderFactory::IsSupported()) {
    run.skipped = true;
    return run;
  }
  const webrtc::Environment env = webrtc::CreateEnvironment();
  webrtc::MFVideoEncoderFactory factory;
  const std::vector<webrtc::SdpVideoFormat> formats =
      factory.GetSupportedFormats();
  std::unique_ptr<webrtc::VideoEncoder> encoder =
      formats.empty() ? nullptr : factory.Create(env, formats[0]);
  webrtc::VideoCodec codec = H264Codec(kWidth, kHeight);
  if (!encoder ||
      encoder->InitEncode(&codec, webrtc::VideoEncoder::Settings(
                                      webrtc::VideoEncoder::Capabilities(false),
                                      1, 1200)) != WEBRTC_VIDEO_CODEC_OK) {
    run.skipped = true;
    return run;
  }
  RefusingSink sink(0);
  encoder->RegisterEncodeCompleteCallback(&sink);
  webrtc::VideoBitrateAllocation allocation;
  allocation.SetBitrate(0, 0, 1000000);
  encoder->SetRates(
      webrtc::VideoEncoder::RateControlParameters(allocation, 30.0));

  livekit_ffi::SetMfEncoderStallForTest(kStallAfter, stall_ms);
  const auto start = std::chrono::steady_clock::now();
  std::optional<std::chrono::steady_clock::time_point> stall_over;
  int accepted_at_stall_end = 0;
  while (std::chrono::steady_clock::now() - start < run_for) {
    webrtc::VideoFrame frame =
        webrtc::VideoFrame::Builder()
            .set_video_frame_buffer(StripedFrame(kWidth, kHeight, run.frames))
            .set_timestamp_rtp(3000 * run.frames)
            .set_timestamp_us(33333LL * run.frames)
            .build();
    std::vector<webrtc::VideoFrameType> types = {
        run.frames == 0 ? webrtc::VideoFrameType::kVideoFrameKey
                        : webrtc::VideoFrameType::kVideoFrameDelta};
    const int32_t result = encoder->Encode(frame, &types);
    if (result == WEBRTC_VIDEO_CODEC_NO_OUTPUT) {
      run.dropped++;
    } else if (result == WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE) {
      run.fallback_frame = run.frames;
      break;
    } else if (result != WEBRTC_VIDEO_CODEC_OK) {
      run.errors.push_back("frame " + std::to_string(run.frames) + " " +
                           Code(result));
    }
    run.frames++;
    const auto now = std::chrono::steady_clock::now();
    if (!stall_over && run.frames > kStallAfter &&
        now - start > std::chrono::milliseconds(stall_ms) +
                          std::chrono::milliseconds(kStallAfter * 34)) {
      stall_over = now;
      accepted_at_stall_end = sink.accepted();
    }
    std::this_thread::sleep_for(std::chrono::milliseconds(33));
  }
  livekit_ffi::SetMfEncoderStallForTest(0, 0);
  run.accepted_after_stall =
      stall_over ? sink.accepted() - accepted_at_stall_end : 0;
  run.name = encoder->GetEncoderInfo().implementation_name;
  encoder->Release();
  return run;
}

std::string Joined(const std::vector<std::string>& errors) {
  std::string joined;
  for (const std::string& error : errors) {
    joined += error + " ";
  }
  return joined;
}

// A GPU busy for 1.5 s (a game's loading hitch): frames are dropped while the
// MFT makes no progress, the encoder stays on hardware, and encoding resumes.
std::string SlowHardware(Checks& c) {
  const StallRun run = RunWithStall(1500, std::chrono::milliseconds(4500));
  if (run.skipped) {
    return "skipped";
  }
  c.Expect(run.fallback_frame < 0,
           "fell back to software at frame " +
               std::to_string(run.fallback_frame));
  c.Expect(run.errors.empty(), "Encode returned " + Joined(run.errors));
  c.Expect(run.dropped > 0, "no frame was dropped during the stall");
  c.Expect(run.accepted_after_stall >= 20,
           "only " + std::to_string(run.accepted_after_stall) +
               " frames encoded after the stall");
  c.Expect(run.name.rfind("MediaFoundation H264 Encoder", 0) == 0,
           "name " + run.name);
  return run.name + ", " + std::to_string(run.frames) + " frames, " +
         std::to_string(run.dropped) + " dropped, " +
         std::to_string(run.accepted_after_stall) + " after the stall";
}

// An MFT that makes no progress for longer than the limit is a real failure:
// the encoder still asks for the software fallback.
std::string StuckHardware(Checks& c) {
  const StallRun run = RunWithStall(8000, std::chrono::milliseconds(8000));
  if (run.skipped) {
    return "skipped";
  }
  c.Expect(run.fallback_frame >= 0, "never fell back to software");
  c.Expect(run.errors.empty(), "Encode returned " + Joined(run.errors));
  return "fell back at frame " + std::to_string(run.fallback_frame) + " after " +
         std::to_string(run.dropped) + " dropped";
}

// Eight vertical bars, R,G,B,A in memory (R8G8B8A8, what the desktop's
// capture shader writes for the GPU path).
constexpr uint8_t kBars[8][3] = {{255, 255, 255}, {255, 255, 0}, {0, 255, 255},
                                 {0, 255, 0},     {255, 0, 255}, {255, 0, 0},
                                 {0, 0, 255},     {16, 16, 16}};

std::vector<uint8_t> BarPixels(int width, int height) {
  std::vector<uint8_t> pixels(static_cast<size_t>(width) * height * 4);
  for (int y = 0; y < height; y++) {
    for (int x = 0; x < width; x++) {
      const uint8_t* bar = kBars[x * 8 / width];
      uint8_t* p = &pixels[(static_cast<size_t>(y) * width + x) * 4];
      p[0] = bar[0];
      p[1] = bar[1];
      p[2] = bar[2];
      p[3] = 255;
    }
  }
  return pixels;
}

// Y, U, V of each bar as libyuv converts it (the memory path's reference).
std::vector<std::array<int, 3>> ExpectedBars() {
  std::vector<std::array<int, 3>> expected;
  for (const auto& bar : kBars) {
    uint8_t pixels[4 * 4 * 4];
    for (int i = 0; i < 16; i++) {
      pixels[i * 4] = bar[0];
      pixels[i * 4 + 1] = bar[1];
      pixels[i * 4 + 2] = bar[2];
      pixels[i * 4 + 3] = 255;
    }
    uint8_t y[16], u[4], v[4];
    libyuv::ABGRToI420(pixels, 16, y, 4, u, 2, v, 2, 4, 4);
    expected.push_back({y[0], u[0], v[0]});
  }
  return expected;
}

// Average Y, U, V over the middle half of each bar.
std::vector<std::array<int, 3>> MeasureBars(
    const webrtc::I420BufferInterface& frame) {
  std::vector<std::array<int, 3>> measured;
  const int width = frame.width();
  const int height = frame.height();
  for (int bar = 0; bar < 8; bar++) {
    const int x0 = bar * width / 8 + width / 32;
    const int x1 = (bar + 1) * width / 8 - width / 32;
    int64_t sum[3] = {0, 0, 0};
    int64_t count[3] = {0, 0, 0};
    for (int y = height / 4; y < height * 3 / 4; y++) {
      for (int x = x0; x < x1; x++) {
        sum[0] += frame.DataY()[y * frame.StrideY() + x];
        count[0]++;
        if ((x & 1) == 0 && (y & 1) == 0) {
          sum[1] += frame.DataU()[(y / 2) * frame.StrideU() + x / 2];
          sum[2] += frame.DataV()[(y / 2) * frame.StrideV() + x / 2];
          count[1]++;
          count[2]++;
        }
      }
    }
    measured.push_back({static_cast<int>(sum[0] / std::max<int64_t>(count[0], 1)),
                        static_cast<int>(sum[1] / std::max<int64_t>(count[1], 1)),
                        static_cast<int>(sum[2] / std::max<int64_t>(count[2], 1))});
  }
  return measured;
}

std::string CompareBars(Checks& c,
                        const std::string& what,
                        const std::vector<std::array<int, 3>>& measured,
                        int tolerance) {
  const std::vector<std::array<int, 3>> expected = ExpectedBars();
  int worst = 0;
  for (size_t bar = 0; bar < expected.size(); bar++) {
    for (int plane = 0; plane < 3; plane++) {
      const int error = std::abs(measured[bar][plane] - expected[bar][plane]);
      worst = std::max(worst, error);
      if (error > tolerance) {
        c.Expect(false, what + " bar " + std::to_string(bar) + " plane " +
                            std::to_string(plane) + ": " +
                            std::to_string(measured[bar][plane]) +
                            " expected " +
                            std::to_string(expected[bar][plane]));
      }
    }
  }
  return what + " worst error " + std::to_string(worst);
}

class CollectingSink : public webrtc::EncodedImageCallback {
 public:
  Result OnEncodedImage(const webrtc::EncodedImage& image,
                        const webrtc::CodecSpecificInfo* info) override {
    webrtc::EncodedImage copy = image;
    copy.SetEncodedData(
        webrtc::EncodedImageBuffer::Create(image.data(), image.size()));
    images.push_back(std::move(copy));
    return Result(Result::OK);
  }
  void OnFrameDropped(uint32_t, int, bool) override {}

  std::vector<webrtc::EncodedImage> images;
};

class LastFrame : public webrtc::DecodedImageCallback {
 public:
  int32_t Decoded(webrtc::VideoFrame& frame) override {
    last = frame.video_frame_buffer()->ToI420();
    decoded++;
    return 0;
  }
  webrtc::scoped_refptr<webrtc::I420BufferInterface> last;
  int decoded = 0;
};

ComPtr<ID3D11Texture2D> BarTexture(ID3D11Device* device, int width, int height) {
  const std::vector<uint8_t> pixels = BarPixels(width, height);
  D3D11_TEXTURE2D_DESC desc = {};
  desc.Width = width;
  desc.Height = height;
  desc.MipLevels = 1;
  desc.ArraySize = 1;
  desc.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
  desc.SampleDesc.Count = 1;
  desc.Usage = D3D11_USAGE_DEFAULT;
  desc.BindFlags = D3D11_BIND_SHADER_RESOURCE;
  D3D11_SUBRESOURCE_DATA data = {pixels.data(), static_cast<UINT>(width * 4),
                                 0};
  ComPtr<ID3D11Texture2D> texture;
  if (FAILED(device->CreateTexture2D(&desc, &data, &texture))) {
    return nullptr;
  }
  return texture;
}

struct GpuRun {
  bool skipped = false;
  int64_t gpu_frames = 0;
  std::string gpu_note;
  int encoded = 0;
  int decoded = 0;
  std::vector<std::array<int, 3>> bars;
  std::vector<std::string> errors;
};

// Encodes `frames` copies of a 640x360 bar texture on `device` at
// encode_width x encode_height and decodes them in software.
GpuRun EncodeTexture(ID3D11Device* device,
                     int encode_width,
                     int encode_height,
                     int frames) {
  GpuRun run;
  constexpr int kWidth = 640;
  constexpr int kHeight = 360;
  const webrtc::Environment env = webrtc::CreateEnvironment();
  webrtc::MFVideoEncoderFactory factory;
  const std::vector<webrtc::SdpVideoFormat> formats =
      factory.GetSupportedFormats();
  std::unique_ptr<webrtc::VideoEncoder> encoder =
      formats.empty() ? nullptr : factory.Create(env, formats[0]);
  webrtc::VideoCodec codec = H264Codec(encode_width, encode_height);
  if (!encoder || !webrtc::H264Decoder::IsSupported() ||
      encoder->InitEncode(&codec, webrtc::VideoEncoder::Settings(
                                      webrtc::VideoEncoder::Capabilities(false),
                                      1, 1200)) != WEBRTC_VIDEO_CODEC_OK) {
    run.skipped = true;
    return run;
  }
  ComPtr<ID3D11Texture2D> texture = BarTexture(device, kWidth, kHeight);
  if (!texture) {
    run.errors.push_back("bar texture not created");
    return run;
  }
  CollectingSink sink;
  encoder->RegisterEncodeCompleteCallback(&sink);
  webrtc::VideoBitrateAllocation allocation;
  allocation.SetBitrate(0, 0, 1500000);
  encoder->SetRates(
      webrtc::VideoEncoder::RateControlParameters(allocation, 30.0));
  const int64_t gpu_before = livekit_ffi::MfEncoderGpuFramesForTest();
  for (int i = 0; i < frames; i++) {
    webrtc::scoped_refptr<livekit::D3D11TextureBuffer> buffer =
        livekit::D3D11TextureBuffer::CopyFrom(texture.Get(), kWidth, kHeight);
    if (!buffer) {
      run.errors.push_back("CopyFrom returned null at frame " +
                           std::to_string(i));
      break;
    }
    webrtc::VideoFrame frame = webrtc::VideoFrame::Builder()
                                   .set_video_frame_buffer(buffer)
                                   .set_timestamp_rtp(3000 * i)
                                   .set_timestamp_us(33333LL * i)
                                   .build();
    std::vector<webrtc::VideoFrameType> types = {
        i == 0 ? webrtc::VideoFrameType::kVideoFrameKey
               : webrtc::VideoFrameType::kVideoFrameDelta};
    const int32_t result = encoder->Encode(frame, &types);
    if (result != WEBRTC_VIDEO_CODEC_OK &&
        result != WEBRTC_VIDEO_CODEC_NO_OUTPUT) {
      run.errors.push_back("frame " + std::to_string(i) + " " + Code(result));
    }
    std::this_thread::sleep_for(std::chrono::milliseconds(20));
  }
  const auto deadline =
      std::chrono::steady_clock::now() + std::chrono::seconds(2);
  while (static_cast<int>(sink.images.size()) < frames - 4 &&
         std::chrono::steady_clock::now() < deadline) {
    std::this_thread::sleep_for(std::chrono::milliseconds(20));
  }
  encoder->Release();
  run.gpu_frames = livekit_ffi::MfEncoderGpuFramesForTest() - gpu_before;
  run.gpu_note = livekit_ffi::MfEncoderGpuNoteForTest();
  run.encoded = static_cast<int>(sink.images.size());

  std::unique_ptr<webrtc::H264Decoder> decoder = webrtc::H264Decoder::Create();
  webrtc::VideoDecoder::Settings settings;
  settings.set_codec_type(webrtc::kVideoCodecH264);
  settings.set_max_render_resolution({encode_width, encode_height});
  settings.set_number_of_cores(1);
  LastFrame last;
  if (!decoder || !decoder->Configure(settings)) {
    run.errors.push_back("software H.264 decoder unavailable");
    return run;
  }
  decoder->RegisterDecodeCompleteCallback(&last);
  for (const webrtc::EncodedImage& image : sink.images) {
    decoder->Decode(image, 0);
  }
  decoder->Release();
  run.decoded = last.decoded;
  if (last.last) {
    if (last.last->width() != encode_width ||
        last.last->height() != encode_height) {
      run.errors.push_back("decoded " + std::to_string(last.last->width()) +
                           "x" + std::to_string(last.last->height()));
    }
    run.bars = MeasureBars(*last.last);
  }
  return run;
}

livekit_ffi::SharedD3D11Device EncoderDevice() {
  std::vector<ComPtr<IMFActivate>> encoders =
      livekit_ffi::EnumHardwareH264Encoders();
  livekit_ffi::SharedD3D11Device device;
  if (encoders.empty() ||
      FAILED(livekit_ffi::AcquireD3D11DeviceForActivate(encoders[0].Get(),
                                                       &device))) {
    return nullptr;
  }
  return device;
}

std::string CheckGpuRun(Checks& c,
                        const std::string& what,
                        const GpuRun& run,
                        bool expect_gpu,
                        int frames,
                        int tolerance) {
  c.Expect(run.errors.empty(), what + ": " + Joined(run.errors));
  c.Expect(run.encoded >= frames - 4,
           what + ": " + std::to_string(run.encoded) + " of " +
               std::to_string(frames) + " frames encoded");
  c.Expect(run.decoded >= 1, what + ": nothing decoded");
  if (expect_gpu) {
    c.Expect(run.gpu_frames >= frames - 1,
             what + ": only " + std::to_string(run.gpu_frames) +
                 " frames took the GPU path (" + run.gpu_note + ")");
  } else {
    c.Expect(run.gpu_frames == 0, what + ": " + std::to_string(run.gpu_frames) +
                                      " frames took the GPU path");
  }
  std::string note = what + ": " + std::to_string(run.gpu_frames) + " GPU, " +
                     std::to_string(run.encoded) + " encoded, " +
                     std::to_string(run.decoded) + " decoded";
  if (!run.bars.empty()) {
    note += ", " + CompareBars(c, what, run.bars, tolerance);
  }
  return note;
}

// A texture on the encoder's device goes through the video processor: the
// frames decode to the same colours libyuv gives the memory path, at the
// encoder's size and at a scaled-down layer.
std::string GpuInput(Checks& c) {
  if (!webrtc::MFVideoEncoderFactory::IsSupported()) {
    return "skipped";
  }
  livekit_ffi::SharedD3D11Device device = EncoderDevice();
  if (!device) {
    return "skipped";
  }
  constexpr int kFrames = 30;
  const GpuRun full = EncodeTexture(device->device.Get(), 640, 360, kFrames);
  if (full.skipped) {
    return "skipped";
  }
  std::string note = CheckGpuRun(c, "640x360", full, true, kFrames, 6);
  const GpuRun half = EncodeTexture(device->device.Get(), 320, 180, kFrames);
  note += "; " + CheckGpuRun(c, "320x180 layer", half, true, kFrames, 8);
  return note;
}

// ToI420 reads the texture back exactly as libyuv converts the same pixels,
// for a cropped and scaled view too.
std::string GpuReadback(Checks& c) {
  livekit_ffi::SharedD3D11Device device = EncoderDevice();
  if (!device) {
    return "skipped";
  }
  ComPtr<ID3D11Texture2D> texture = BarTexture(device->device.Get(), 640, 360);
  webrtc::scoped_refptr<livekit::D3D11TextureBuffer> buffer =
      texture ? livekit::D3D11TextureBuffer::CopyFrom(texture.Get(), 640, 360)
              : nullptr;
  if (!buffer) {
    c.Expect(false, "CopyFrom returned null");
    return std::string();
  }
  c.Expect(livekit::D3D11TextureBuffer::FromNative(buffer.get()) == buffer.get(),
           "FromNative does not recognise the buffer");
  webrtc::scoped_refptr<webrtc::I420Buffer> plain =
      webrtc::I420Buffer::Create(640, 360);
  c.Expect(livekit::D3D11TextureBuffer::FromNative(plain.get()) == nullptr,
           "FromNative accepted an I420 buffer");
  webrtc::scoped_refptr<webrtc::I420BufferInterface> i420 = buffer->ToI420();
  if (!i420) {
    c.Expect(false, "ToI420 returned null");
    return std::string();
  }
  const std::vector<uint8_t> pixels = BarPixels(640, 360);
  webrtc::scoped_refptr<webrtc::I420Buffer> expected =
      webrtc::I420Buffer::Create(640, 360);
  libyuv::ABGRToI420(pixels.data(), 640 * 4, expected->MutableDataY(),
                     expected->StrideY(), expected->MutableDataU(),
                     expected->StrideU(), expected->MutableDataV(),
                     expected->StrideV(), 640, 360);
  bool same = true;
  for (int y = 0; y < 360 && same; y++) {
    same = std::memcmp(i420->DataY() + y * i420->StrideY(),
                       expected->DataY() + y * expected->StrideY(), 640) == 0;
  }
  c.Expect(same, "read-back luma differs from libyuv");
  webrtc::scoped_refptr<webrtc::VideoFrameBuffer> half =
      buffer->CropAndScale(0, 0, 640, 360, 320, 180);
  c.Expect(livekit::D3D11TextureBuffer::FromNative(half.get()) != nullptr,
           "CropAndScale left the GPU");
  webrtc::scoped_refptr<webrtc::I420BufferInterface> half_i420 =
      half->ToI420();
  c.Expect(half_i420 && half_i420->width() == 320 && half_i420->height() == 180,
           "scaled read-back has the wrong size");
  std::string note = "read-back matches libyuv";
  if (half_i420) {
    note += "; " + CompareBars(c, "scaled read-back", MeasureBars(*half_i420), 2);
  }
  return note;
}

std::string Hr(HRESULT hr) {
  char text[16];
  std::snprintf(text, sizeof(text), "0x%08lX", static_cast<unsigned long>(hr));
  return text;
}

// The video processor alone: RGBA bars into an NV12 render target at full and
// half size, read back and compared with libyuv.
std::string GpuConvert(Checks& c) {
  livekit_ffi::SharedD3D11Device device = EncoderDevice();
  if (!device) {
    return "skipped";
  }
  ComPtr<ID3D11Texture2D> bars = BarTexture(device->device.Get(), 640, 360);
  webrtc::scoped_refptr<livekit::D3D11TextureBuffer> buffer =
      bars ? livekit::D3D11TextureBuffer::CopyFrom(bars.Get(), 640, 360)
           : nullptr;
  if (!buffer) {
    c.Expect(false, "CopyFrom returned null");
    return std::string();
  }
  ComPtr<ID3D10Multithread> multithread;
  device->context.As(&multithread);
  std::string note;
  webrtc::D3D11VideoConverter converter;
  for (const auto [width, height] : {std::pair{640, 360}, std::pair{320, 180}}) {
    D3D11_TEXTURE2D_DESC desc = {};
    desc.Width = width;
    desc.Height = height;
    desc.MipLevels = 1;
    desc.ArraySize = 1;
    desc.Format = DXGI_FORMAT_NV12;
    desc.SampleDesc.Count = 1;
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
    ComPtr<ID3D11Texture2D> target;
    HRESULT hr = device->device->CreateTexture2D(&desc, nullptr, &target);
    if (FAILED(hr)) {
      c.Expect(false, "NV12 target " + Hr(hr));
      return note;
    }
    multithread->Enter();
    hr = converter.Convert(device->device.Get(), device->context.Get(),
                           buffer->texture(), buffer->source_rect(),
                           target.Get(), width, height);
    multithread->Leave();
    const std::string size = std::to_string(width) + "x" + std::to_string(height);
    if (FAILED(hr)) {
      c.Expect(false, size + " Convert " + Hr(hr));
      continue;
    }
    desc.Usage = D3D11_USAGE_STAGING;
    desc.BindFlags = 0;
    desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    ComPtr<ID3D11Texture2D> staging;
    device->device->CreateTexture2D(&desc, nullptr, &staging);
    multithread->Enter();
    device->context->CopyResource(staging.Get(), target.Get());
    D3D11_MAPPED_SUBRESOURCE mapped = {};
    hr = device->context->Map(staging.Get(), 0, D3D11_MAP_READ, 0, &mapped);
    multithread->Leave();
    if (FAILED(hr)) {
      c.Expect(false, size + " Map " + Hr(hr));
      continue;
    }
    webrtc::scoped_refptr<webrtc::I420Buffer> i420 =
        webrtc::I420Buffer::Create(width, height);
    const uint8_t* y = static_cast<const uint8_t*>(mapped.pData);
    libyuv::NV12ToI420(y, mapped.RowPitch, y + mapped.RowPitch * height,
                       mapped.RowPitch, i420->MutableDataY(), i420->StrideY(),
                       i420->MutableDataU(), i420->StrideU(),
                       i420->MutableDataV(), i420->StrideV(), width, height);
    multithread->Enter();
    device->context->Unmap(staging.Get(), 0);
    multithread->Leave();
    note += (note.empty() ? "" : "; ") +
            CompareBars(c, size + " NV12", MeasureBars(*i420), 3);
  }
  return note;
}

// A texture on another device than the encoder's is read back to memory and
// still encodes on the MFT.
std::string GpuOtherDevice(Checks& c) {
  if (!webrtc::MFVideoEncoderFactory::IsSupported() || !EncoderDevice()) {
    return "skipped";
  }
  ComPtr<ID3D11Device> device;
  ComPtr<ID3D11DeviceContext> context;
  if (FAILED(D3D11CreateDevice(nullptr, D3D_DRIVER_TYPE_HARDWARE, nullptr,
                               D3D11_CREATE_DEVICE_BGRA_SUPPORT, nullptr, 0,
                               D3D11_SDK_VERSION, &device, nullptr,
                               &context))) {
    return "skipped";
  }
  ComPtr<ID3D10Multithread> multithread;
  if (SUCCEEDED(context.As(&multithread))) {
    multithread->SetMultithreadProtected(TRUE);
  }
  constexpr int kFrames = 20;
  const GpuRun run = EncodeTexture(device.Get(), 640, 360, kFrames);
  if (run.skipped) {
    return "skipped";
  }
  return CheckGpuRun(c, "other device", run, false, kFrames, 6);
}

}  // namespace

// For loopback tests in other crates: a 640x360-style bar texture (R8G8B8A8,
// see kBars) on the hardware encoder's shared device, with one reference for
// the caller (lk_mf_test_release_texture). Null without a hardware encoder.
// The texture comes with a host reference on its device, as the desktop's
// capture holds one, so codecs opened meanwhile get that same device.
extern "C" void* lk_mf_test_create_bar_texture(int32_t width, int32_t height) {
  if (!webrtc::MFVideoEncoderFactory::IsSupported()) {
    return nullptr;
  }
  livekit_ffi::SharedD3D11Device device = EncoderDevice();
  if (!device) {
    return nullptr;
  }
  ComPtr<IDXGIDevice> dxgi;
  ComPtr<IDXGIAdapter> adapter;
  DXGI_ADAPTER_DESC desc = {};
  ID3D11Device* host = nullptr;
  if (FAILED(device->device.As(&dxgi)) || FAILED(dxgi->GetAdapter(&adapter)) ||
      FAILED(adapter->GetDesc(&desc)) ||
      FAILED(livekit_ffi::AcquireSharedD3D11DeviceForHost(&desc.AdapterLuid,
                                                          &host))) {
    return nullptr;
  }
  host->Release();
  return BarTexture(device->device.Get(), width, height).Detach();
}

extern "C" void lk_mf_test_release_texture(void* texture) {
  if (texture) {
    ComPtr<ID3D11Device> device;
    static_cast<ID3D11Texture2D*>(texture)->GetDevice(&device);
    livekit_ffi::ReleaseSharedD3D11DeviceForHost(device.Get());
    static_cast<ID3D11Texture2D*>(texture)->Release();
  }
}

namespace {

// GpuHog.hlsl: `Iterations` rounds of sin/cos per thread into a structured
// buffer, compiled with fxc /T cs_5_0 /O3.
constexpr uint8_t kGpuHogShader[] = {
    0x44, 0x58, 0x42, 0x43, 0xA2, 0xF8, 0x0C, 0x4C, 0xAE, 0x92, 0xF1, 0x9D, 0x3D, 0xE2, 0xB4, 0x20,
    0x07, 0x19, 0x27, 0x92, 0x01, 0x00, 0x00, 0x00, 0xD8, 0x01, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00,
    0x2C, 0x00, 0x00, 0x00, 0x3C, 0x00, 0x00, 0x00, 0x4C, 0x00, 0x00, 0x00, 0x49, 0x53, 0x47, 0x4E,
    0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x4F, 0x53, 0x47, 0x4E,
    0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, 0x53, 0x48, 0x45, 0x58,
    0x84, 0x01, 0x00, 0x00, 0x50, 0x00, 0x05, 0x00, 0x61, 0x00, 0x00, 0x00, 0x6A, 0x08, 0x00, 0x01,
    0x59, 0x00, 0x00, 0x04, 0x46, 0x8E, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x9E, 0x00, 0x00, 0x04, 0x00, 0xE0, 0x11, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00,
    0x5F, 0x00, 0x00, 0x02, 0x12, 0x00, 0x02, 0x00, 0x68, 0x00, 0x00, 0x02, 0x02, 0x00, 0x00, 0x00,
    0x9B, 0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x56, 0x00, 0x00, 0x04, 0x12, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0A, 0x00, 0x02, 0x00,
    0x38, 0x00, 0x00, 0x07, 0x12, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0A, 0x00, 0x10, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x01, 0x40, 0x00, 0x00, 0x6F, 0x12, 0x83, 0x3A, 0x36, 0x00, 0x00, 0x05,
    0x22, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0A, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x36, 0x00, 0x00, 0x05, 0x42, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x40, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x30, 0x00, 0x00, 0x01, 0x50, 0x00, 0x00, 0x08, 0x82, 0x00, 0x10, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x2A, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0A, 0x80, 0x20, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x04, 0x03, 0x3A, 0x00, 0x10, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x4D, 0x00, 0x00, 0x06, 0x82, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0xD0, 0x00, 0x00, 0x1A, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x38, 0x00, 0x00, 0x07,
    0x12, 0x00, 0x10, 0x00, 0x01, 0x00, 0x00, 0x00, 0x1A, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3F, 0x4D, 0x00, 0x00, 0x06, 0x00, 0xD0, 0x00, 0x00,
    0x12, 0x00, 0x10, 0x00, 0x01, 0x00, 0x00, 0x00, 0x0A, 0x00, 0x10, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x32, 0x00, 0x00, 0x09, 0x22, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3A, 0x00, 0x10, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x01, 0x40, 0x00, 0x00, 0x47, 0x03, 0x80, 0x3F, 0x0A, 0x00, 0x10, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x1E, 0x00, 0x00, 0x07, 0x42, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x2A, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x40, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x16, 0x00, 0x00, 0x01, 0xA8, 0x00, 0x00, 0x08, 0x12, 0xE0, 0x11, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x0A, 0x00, 0x02, 0x00, 0x01, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1A, 0x00, 0x10, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x3E, 0x00, 0x00, 0x01,
};

// A game stand-in: its own device on the encoder's adapter, dispatching
// heavy compute back to back with at most two frames queued.
struct GpuLoad {
  std::thread thread;
  std::atomic<bool> stop{false};
  std::atomic<int64_t> frames{0};
  std::chrono::steady_clock::time_point started;
};

void RunGpuLoad(GpuLoad* load, ComPtr<ID3D11Device> device, uint32_t iterations) {
  ComPtr<ID3D11DeviceContext> context;
  device->GetImmediateContext(&context);
  ComPtr<ID3D11ComputeShader> shader;
  if (FAILED(device->CreateComputeShader(kGpuHogShader, sizeof(kGpuHogShader),
                                         nullptr, &shader))) {
    return;
  }
  constexpr UINT kThreads = 1 << 20;
  D3D11_BUFFER_DESC desc = {};
  desc.ByteWidth = kThreads * sizeof(float);
  desc.BindFlags = D3D11_BIND_UNORDERED_ACCESS;
  desc.MiscFlags = D3D11_RESOURCE_MISC_BUFFER_STRUCTURED;
  desc.StructureByteStride = sizeof(float);
  ComPtr<ID3D11Buffer> buffer;
  ComPtr<ID3D11UnorderedAccessView> view;
  device->CreateBuffer(&desc, nullptr, &buffer);
  device->CreateUnorderedAccessView(buffer.Get(), nullptr, &view);
  const uint32_t constants[4] = {iterations, 0, 0, 0};
  D3D11_BUFFER_DESC cdesc = {};
  cdesc.ByteWidth = sizeof(constants);
  cdesc.BindFlags = D3D11_BIND_CONSTANT_BUFFER;
  D3D11_SUBRESOURCE_DATA cdata = {constants, 0, 0};
  ComPtr<ID3D11Buffer> cbuffer;
  device->CreateBuffer(&cdesc, &cdata, &cbuffer);
  D3D11_QUERY_DESC qdesc = {D3D11_QUERY_EVENT, 0};
  ComPtr<ID3D11Query> queries[2];
  device->CreateQuery(&qdesc, &queries[0]);
  device->CreateQuery(&qdesc, &queries[1]);
  context->CSSetShader(shader.Get(), nullptr, 0);
  context->CSSetConstantBuffers(0, 1, cbuffer.GetAddressOf());
  context->CSSetUnorderedAccessViews(0, 1, view.GetAddressOf(), nullptr);
  for (int64_t frame = 0; !load->stop.load(); frame++) {
    ID3D11Query* query = queries[frame & 1].Get();
    if (frame >= 2) {
      while (context->GetData(query, nullptr, 0, 0) == S_FALSE &&
             !load->stop.load()) {
        std::this_thread::sleep_for(std::chrono::microseconds(200));
      }
    }
    context->Dispatch(kThreads / 256, 1, 1);
    context->End(query);
    context->Flush();
    load->frames++;
  }
  context->ClearState();
  context->Flush();
}

}  // namespace

// Starts the game stand-in on the encoder's adapter: `iterations` rounds per
// thread per dispatch sets how long each GPU "frame" takes. Null without a
// hardware encoder.
extern "C" void* lk_mf_test_gpu_load_start(uint32_t iterations) {
  livekit_ffi::SharedD3D11Device shared = EncoderDevice();
  if (!shared) {
    return nullptr;
  }
  ComPtr<IDXGIDevice> dxgi;
  ComPtr<IDXGIAdapter> adapter;
  if (FAILED(shared->device.As(&dxgi)) || FAILED(dxgi->GetAdapter(&adapter))) {
    return nullptr;
  }
  ComPtr<ID3D11Device> device;
  if (FAILED(D3D11CreateDevice(adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr,
                               0, nullptr, 0, D3D11_SDK_VERSION, &device,
                               nullptr, nullptr))) {
    return nullptr;
  }
  auto* load = new GpuLoad();
  load->started = std::chrono::steady_clock::now();
  load->thread = std::thread(RunGpuLoad, load, device, iterations);
  return load;
}

// Stops it; returns the GPU frames per second it ran at.
extern "C" double lk_mf_test_gpu_load_stop(void* handle) {
  auto* load = static_cast<GpuLoad*>(handle);
  if (!load) {
    return 0;
  }
  load->stop = true;
  load->thread.join();
  const double seconds = std::chrono::duration<double>(
                             std::chrono::steady_clock::now() - load->started)
                             .count();
  const double fps = seconds > 0 ? load->frames.load() / seconds : 0;
  delete load;
  return fps;
}

// Frames every MF encoder in the process converted on the GPU.
extern "C" int64_t lk_mf_test_gpu_frames_total() {
  return livekit_ffi::MfEncoderGpuFramesTotalForTest();
}

// MfEncoderFrameTypesForTest into `text`.
extern "C" void lk_mf_test_frame_types(char* text, size_t length) {
  if (text && length > 0) {
    std::snprintf(text, length, "%s",
                  livekit_ffi::MfEncoderFrameTypesForTest().c_str());
  }
}

// 0 passed, 1 failed, 2 skipped; `message` gets the failures, or a note.
extern "C" int32_t lk_mf_encoder_test(const char* scenario,
                                      char* message,
                                      size_t message_len) {
  Checks checks;
  std::string note;
  const std::string name = scenario ? scenario : "";
  if (name == "refused_send") {
    note = RefusedSend(checks);
  } else if (name == "slow_hardware") {
    note = SlowHardware(checks);
  } else if (name == "stuck_hardware") {
    note = StuckHardware(checks);
  } else if (name == "gpu_input") {
    note = GpuInput(checks);
  } else if (name == "gpu_convert") {
    note = GpuConvert(checks);
  } else if (name == "gpu_readback") {
    note = GpuReadback(checks);
  } else if (name == "gpu_other_device") {
    note = GpuOtherDevice(checks);
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
