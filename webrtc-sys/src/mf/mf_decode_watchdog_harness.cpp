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

// Scenarios for mf_decode_watchdog_tests.rs (the `harness` feature): the
// watchdog over fake decoders that decode, hang or stay silent, behind
// WebRTC's own software fallback wrapper, and over the real MF decoder.

#include <windows.h>

#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <memory>
#include <mutex>
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
#include <api/video_codecs/video_decoder.h>
#include <api/video_codecs/video_decoder_software_fallback_wrapper.h>
#include <api/video_codecs/video_encoder.h>
#include <modules/video_coding/codecs/h264/include/h264.h>
#include <modules/video_coding/include/video_error_codes.h>

#include "mf_decode_watchdog.h"
#include "mf_decoder_factory.h"

namespace {

using Clock = std::chrono::steady_clock;

int64_t MsSince(Clock::time_point start) {
  return std::chrono::duration_cast<std::chrono::milliseconds>(Clock::now() -
                                                               start)
      .count();
}

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
    case WEBRTC_VIDEO_CODEC_ERROR:
      return "ERROR";
    case WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE:
      return "FALLBACK_SOFTWARE";
    default:
      return std::to_string(code);
  }
}

webrtc::EncodedImage Image(bool key, uint32_t rtp_timestamp) {
  static const uint8_t kBytes[16] = {0, 0, 0, 1, 0x65, 1, 2, 3};
  webrtc::EncodedImage image;
  image.SetEncodedData(
      webrtc::EncodedImageBuffer::Create(kBytes, sizeof(kBytes)));
  image.SetFrameType(key ? webrtc::VideoFrameType::kVideoFrameKey
                         : webrtc::VideoFrameType::kVideoFrameDelta);
  image.SetRtpTimestamp(rtp_timestamp);
  return image;
}

webrtc::VideoDecoder::Settings H264Settings() {
  webrtc::VideoDecoder::Settings settings;
  settings.set_codec_type(webrtc::kVideoCodecH264);
  settings.set_max_render_resolution({640, 480});
  return settings;
}

class CountingCallback : public webrtc::DecodedImageCallback {
 public:
  int32_t Decoded(webrtc::VideoFrame& frame) override {
    frames_.fetch_add(1);
    width_.store(frame.width());
    height_.store(frame.height());
    return WEBRTC_VIDEO_CODEC_OK;
  }
  int frames() const { return frames_.load(); }
  int width() const { return width_.load(); }
  int height() const { return height_.load(); }

 private:
  std::atomic<int> frames_{0};
  std::atomic<int> width_{0};
  std::atomic<int> height_{0};
};

// What a FakeDecoder does, set by the test between calls. It outlives the
// decoder, which an abandoned watchdog thread destroys whenever its call
// returns.
struct FakeControl {
  enum class Mode { kDecode, kSilent, kBlock };
  std::atomic<Mode> mode{Mode::kDecode};
  std::atomic<bool> block_configure{false};
  std::atomic<int> decodes{0};
  std::atomic<bool> destroyed{false};

  std::mutex mutex;
  std::condition_variable cv;
  bool released = false;

  void Block() {
    std::unique_lock<std::mutex> lock(mutex);
    cv.wait(lock, [this] { return released; });
  }
  void Unblock() {
    {
      std::lock_guard<std::mutex> lock(mutex);
      released = true;
    }
    cv.notify_all();
  }
  bool WaitDestroyed(int timeout_ms) {
    const auto start = Clock::now();
    while (!destroyed.load()) {
      if (MsSince(start) > timeout_ms) {
        return false;
      }
      Sleep(5);
    }
    return true;
  }
};

class FakeDecoder : public webrtc::VideoDecoder {
 public:
  FakeDecoder(std::string name, std::shared_ptr<FakeControl> control)
      : name_(std::move(name)), control_(std::move(control)) {}
  ~FakeDecoder() override { control_->destroyed.store(true); }

  bool Configure(const Settings& settings) override {
    if (control_->block_configure.load()) {
      control_->Block();
    }
    return true;
  }

  int32_t Decode(const webrtc::EncodedImage& image,
                 int64_t render_time_ms) override {
    control_->decodes.fetch_add(1);
    const FakeControl::Mode mode = control_->mode.load();
    if (mode == FakeControl::Mode::kBlock) {
      control_->Block();
    }
    if (mode == FakeControl::Mode::kSilent) {
      return WEBRTC_VIDEO_CODEC_OK;
    }
    if (callback_) {
      webrtc::VideoFrame frame =
          webrtc::VideoFrame::Builder()
              .set_video_frame_buffer(webrtc::I420Buffer::Create(16, 16))
              .set_timestamp_rtp(image.RtpTimestamp())
              .build();
      callback_->Decoded(frame, std::nullopt, std::nullopt);
    }
    return WEBRTC_VIDEO_CODEC_OK;
  }

  int32_t RegisterDecodeCompleteCallback(
      webrtc::DecodedImageCallback* callback) override {
    callback_ = callback;
    return WEBRTC_VIDEO_CODEC_OK;
  }

  int32_t Release() override { return WEBRTC_VIDEO_CODEC_OK; }

  DecoderInfo GetDecoderInfo() const override {
    DecoderInfo info;
    info.implementation_name = name_;
    info.is_hardware_accelerated = true;
    return info;
  }

 private:
  const std::string name_;
  const std::shared_ptr<FakeControl> control_;
  webrtc::DecodedImageCallback* callback_ = nullptr;
};

webrtc::MFDecodeWatchdogLimits FastLimits() {
  webrtc::MFDecodeWatchdogLimits limits;
  limits.configure_timeout_ms = 200;
  limits.call_timeout_ms = 100;
  limits.silent_frames = 5;
  limits.silent_ms = 60;
  return limits;
}

void PassesThrough(Checks& c) {
  auto control = std::make_shared<FakeControl>();
  webrtc::MFDecodeHealth health;
  CountingCallback callback;
  webrtc::MFDecodeWatchdog watchdog(
      std::make_unique<FakeDecoder>("Fake HW", control), FastLimits(),
      &health);
  c.Expect(watchdog.GetDecoderInfo().implementation_name == "Fake HW",
           "info before Configure");
  c.Expect(watchdog.Configure(H264Settings()), "Configure");
  watchdog.RegisterDecodeCompleteCallback(&callback);
  for (int i = 0; i < 30; i++) {
    const int32_t result = watchdog.Decode(Image(i == 0, 3000 * i), 0);
    c.Expect(result == WEBRTC_VIDEO_CODEC_OK,
             "frame " + std::to_string(i) + " returned " + Code(result));
  }
  c.Expect(callback.frames() == 30,
           "frames delivered " + std::to_string(callback.frames()));
  c.Expect(watchdog.Release() == WEBRTC_VIDEO_CODEC_OK, "Release");
  c.Expect(health.hangs() == 0 && health.silences() == 0, "health clean");
}

void HangOnDelta(Checks& c) {
  auto control = std::make_shared<FakeControl>();
  webrtc::MFDecodeHealth health;
  CountingCallback callback;
  int hang_reports = 0;
  {
    webrtc::MFDecodeWatchdog watchdog(
        std::make_unique<FakeDecoder>("Fake HW", control), FastLimits(),
        &health, [&hang_reports] { hang_reports++; });
    watchdog.Configure(H264Settings());
    watchdog.RegisterDecodeCompleteCallback(&callback);
    c.Expect(watchdog.Decode(Image(true, 0), 0) == WEBRTC_VIDEO_CODEC_OK,
             "keyframe");
    c.Expect(watchdog.Decode(Image(false, 3000), 0) == WEBRTC_VIDEO_CODEC_OK,
             "delta");

    control->mode.store(FakeControl::Mode::kBlock);
    const auto start = Clock::now();
    const int32_t hung = watchdog.Decode(Image(false, 6000), 0);
    const int64_t waited = MsSince(start);
    c.Expect(hung == WEBRTC_VIDEO_CODEC_ERROR,
             "hung delta returned " + Code(hung) + ", want ERROR");
    c.Expect(waited >= 90 && waited < 2000,
             "hung delta took " + std::to_string(waited) + " ms");
    c.Expect(health.hangs() == 1 && health.decode_disabled(),
             "hang recorded");
    c.Expect(hang_reports == 1, "on_hang ran " + std::to_string(hang_reports));

    const int decodes = control->decodes.load();
    const int32_t next = watchdog.Decode(Image(true, 9000), 0);
    c.Expect(next == WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE,
             "next keyframe returned " + Code(next));
    c.Expect(control->decodes.load() == decodes,
             "abandoned decoder got another frame");
    const auto release_start = Clock::now();
    c.Expect(watchdog.Release() == WEBRTC_VIDEO_CODEC_OK, "Release");
    c.Expect(MsSince(release_start) < 50, "Release waited");
    c.Expect(!watchdog.Configure(H264Settings()),
             "Configure after a hang succeeded");
  }
  c.Expect(!control->destroyed.load(),
           "decoder destroyed while its call was running");
  control->Unblock();
  c.Expect(control->WaitDestroyed(2000),
           "abandoned decoder not destroyed after its call returned");
  c.Expect(callback.frames() == 2,
           "late output reached WebRTC: " + std::to_string(callback.frames()));
}

void HangOnKeyframe(Checks& c) {
  auto control = std::make_shared<FakeControl>();
  webrtc::MFDecodeHealth health;
  {
    webrtc::MFDecodeWatchdog watchdog(
        std::make_unique<FakeDecoder>("Fake HW", control), FastLimits(),
        &health);
    watchdog.Configure(H264Settings());
    control->mode.store(FakeControl::Mode::kBlock);
    const int32_t hung = watchdog.Decode(Image(true, 0), 0);
    c.Expect(hung == WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE,
             "hung keyframe returned " + Code(hung));
  }
  control->Unblock();
  c.Expect(control->WaitDestroyed(2000), "abandoned decoder not destroyed");
}

void Silent(Checks& c) {
  auto control = std::make_shared<FakeControl>();
  webrtc::MFDecodeHealth health;
  CountingCallback callback;
  webrtc::MFDecodeWatchdog watchdog(
      std::make_unique<FakeDecoder>("Fake HW", control), FastLimits(),
      &health);
  watchdog.Configure(H264Settings());
  watchdog.RegisterDecodeCompleteCallback(&callback);
  c.Expect(watchdog.Decode(Image(true, 0), 0) == WEBRTC_VIDEO_CODEC_OK,
           "keyframe");

  control->mode.store(FakeControl::Mode::kSilent);
  const auto start = Clock::now();
  int calls = 0;
  int32_t result = WEBRTC_VIDEO_CODEC_OK;
  while (result == WEBRTC_VIDEO_CODEC_OK && calls < 200) {
    result = watchdog.Decode(Image(false, 3000 * (calls + 1)), 0);
    calls++;
    Sleep(10);
  }
  const int64_t silent_for = MsSince(start);
  c.Expect(result == WEBRTC_VIDEO_CODEC_ERROR,
           "silent delta returned " + Code(result));
  c.Expect(calls >= 5, "gave up after " + std::to_string(calls) + " calls");
  c.Expect(silent_for >= 60,
           "gave up after " + std::to_string(silent_for) + " ms");
  c.Expect(health.silences() == 1 && !health.decode_disabled(),
           "silence recorded without turning MF decoding off");
  c.Expect(watchdog.Decode(Image(true, 900000), 0) ==
               WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE,
           "next frame not handed to software");

  watchdog.Release();
  control->mode.store(FakeControl::Mode::kDecode);
  c.Expect(watchdog.Configure(H264Settings()),
           "Configure after a silence failed");
  c.Expect(watchdog.Decode(Image(true, 903000), 0) == WEBRTC_VIDEO_CODEC_OK,
           "decoding after reconfigure");
  c.Expect(callback.frames() == 2,
           "frames delivered " + std::to_string(callback.frames()));
}

void ConfigureHang(Checks& c) {
  auto control = std::make_shared<FakeControl>();
  control->block_configure.store(true);
  webrtc::MFDecodeHealth health;
  {
    webrtc::MFDecodeWatchdog watchdog(
        std::make_unique<FakeDecoder>("Fake HW", control), FastLimits(),
        &health);
    const auto start = Clock::now();
    c.Expect(!watchdog.Configure(H264Settings()),
             "hung Configure succeeded");
    c.Expect(MsSince(start) < 2000, "hung Configure waited too long");
    c.Expect(health.decode_disabled(), "hang not recorded");
    c.Expect(watchdog.Decode(Image(true, 0), 0) ==
                 WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE,
             "Decode after a hung Configure");
  }
  control->Unblock();
  c.Expect(control->WaitDestroyed(2000), "abandoned decoder not destroyed");
}

// The sequence WebRTC sees: VideoDecoderSoftwareFallbackWrapper over the
// watchdog, as livekit_ffi::VideoDecoderFactory builds it.
void BehindFallbackWrapper(Checks& c) {
  auto hw = std::make_shared<FakeControl>();
  auto sw = std::make_shared<FakeControl>();
  webrtc::MFDecodeHealth health;
  CountingCallback callback;
  const webrtc::Environment env = webrtc::CreateEnvironment();
  {
    std::unique_ptr<webrtc::VideoDecoder> decoder =
        webrtc::CreateVideoDecoderSoftwareFallbackWrapper(
            env, std::make_unique<FakeDecoder>("Fake SW", sw),
            std::make_unique<webrtc::MFDecodeWatchdog>(
                std::make_unique<FakeDecoder>("Fake HW", hw), FastLimits(),
                &health));
    decoder->RegisterDecodeCompleteCallback(&callback);
    c.Expect(decoder->Configure(H264Settings()), "Configure");
    c.Expect(decoder->Decode(Image(true, 0), 0) == WEBRTC_VIDEO_CODEC_OK,
             "keyframe on hardware");

    hw->mode.store(FakeControl::Mode::kBlock);
    const int32_t hung = decoder->Decode(Image(false, 3000), 0);
    c.Expect(hung == WEBRTC_VIDEO_CODEC_ERROR,
             "hung delta returned " + Code(hung) +
                 ", want ERROR so that WebRTC asks for a keyframe");
    c.Expect(sw->decodes.load() == 0, "software decoder used too early");

    const int32_t key = decoder->Decode(Image(true, 6000), 0);
    c.Expect(key == WEBRTC_VIDEO_CODEC_OK,
             "keyframe after the hang returned " + Code(key));
    c.Expect(sw->decodes.load() == 1,
             "software decoder decodes " + std::to_string(sw->decodes.load()));
    const std::string name = decoder->GetDecoderInfo().implementation_name;
    c.Expect(name == "Fake SW (fallback from: Fake HW)", "name " + name);
    c.Expect(decoder->Decode(Image(false, 9000), 0) == WEBRTC_VIDEO_CODEC_OK,
             "delta on software");
  }
  hw->Unblock();
  c.Expect(hw->WaitDestroyed(2000), "abandoned decoder not destroyed");
  c.Expect(callback.frames() == 3,
           "frames delivered " + std::to_string(callback.frames()));
}

class EncodedSink : public webrtc::EncodedImageCallback {
 public:
  Result OnEncodedImage(const webrtc::EncodedImage& image,
                        const webrtc::CodecSpecificInfo* info) override {
    webrtc::EncodedImage copy = image;
    copy.SetEncodedData(
        webrtc::EncodedImageBuffer::Create(image.data(), image.size()));
    images.push_back(std::move(copy));
    return Result(Result::OK);
  }
  void OnFrameDropped(uint32_t rtp_timestamp,
                      int spatial_id,
                      bool is_end_of_temporal_unit) override {}
  std::vector<webrtc::EncodedImage> images;
};

// The real MF decoder through the factory, so through the watchdog's thread.
// "skipped" without a DXVA-capable device or the software H264 encoder.
std::string RealDecoder(Checks& c) {
  if (!webrtc::MFVideoDecoderFactory::IsSupported() ||
      !webrtc::H264Encoder::IsSupported()) {
    return "skipped";
  }
  constexpr int kWidth = 320;
  constexpr int kHeight = 240;
  constexpr int kFrames = 30;
  const webrtc::Environment env = webrtc::CreateEnvironment();

  std::unique_ptr<webrtc::VideoEncoder> encoder =
      webrtc::CreateH264Encoder(env);
  webrtc::VideoCodec codec;
  codec.codecType = webrtc::kVideoCodecH264;
  codec.width = kWidth;
  codec.height = kHeight;
  codec.maxFramerate = 30;
  codec.startBitrate = 500;
  codec.minBitrate = 50;
  codec.maxBitrate = 1000;
  codec.qpMax = 51;
  codec.numberOfSimulcastStreams = 1;
  codec.simulcastStream[0].width = kWidth;
  codec.simulcastStream[0].height = kHeight;
  codec.simulcastStream[0].maxFramerate = 30;
  codec.simulcastStream[0].numberOfTemporalLayers = 1;
  codec.simulcastStream[0].maxBitrate = 1000;
  codec.simulcastStream[0].targetBitrate = 500;
  codec.simulcastStream[0].minBitrate = 50;
  codec.simulcastStream[0].qpMax = 51;
  codec.simulcastStream[0].active = true;
  codec.SetFrameDropEnabled(false);
  EncodedSink sink;
  const int32_t init = encoder->InitEncode(
      &codec, webrtc::VideoEncoder::Settings(
                  webrtc::VideoEncoder::Capabilities(false), 1, 1200));
  if (init != WEBRTC_VIDEO_CODEC_OK) {
    c.Expect(false, "InitEncode returned " + Code(init));
    return std::string();
  }
  encoder->RegisterEncodeCompleteCallback(&sink);
  webrtc::VideoBitrateAllocation allocation;
  allocation.SetBitrate(0, 0, 500000);
  encoder->SetRates(
      webrtc::VideoEncoder::RateControlParameters(allocation, 30.0));
  for (int i = 0; i < kFrames; i++) {
    webrtc::scoped_refptr<webrtc::I420Buffer> buffer =
        webrtc::I420Buffer::Create(kWidth, kHeight);
    webrtc::I420Buffer::SetBlack(buffer.get());
    std::memset(buffer->MutableDataY() + (i * 7 % kHeight) * buffer->StrideY(),
                235, kWidth);
    webrtc::VideoFrame frame = webrtc::VideoFrame::Builder()
                                   .set_video_frame_buffer(buffer)
                                   .set_timestamp_rtp(3000 * i)
                                   .set_timestamp_us(33333 * i)
                                   .build();
    std::vector<webrtc::VideoFrameType> types = {
        i == 0 ? webrtc::VideoFrameType::kVideoFrameKey
               : webrtc::VideoFrameType::kVideoFrameDelta};
    encoder->Encode(frame, &types);
  }
  encoder->Release();
  c.Expect(sink.images.size() == kFrames,
           "encoded " + std::to_string(sink.images.size()) + " frames");
  if (sink.images.empty()) {
    return std::string();
  }

  webrtc::MFVideoDecoderFactory factory;
  std::unique_ptr<webrtc::VideoDecoder> decoder = factory.Create(
      env, webrtc::CreateH264Format(webrtc::H264Profile::kProfileConstrainedBaseline,
                                    webrtc::H264Level::kLevel3_1, "1"));
  if (!decoder) {
    c.Expect(false, "factory created no decoder");
    return std::string();
  }
  CountingCallback callback;
  decoder->RegisterDecodeCompleteCallback(&callback);
  c.Expect(decoder->Configure(H264Settings()), "Configure");
  for (size_t i = 0; i < sink.images.size(); i++) {
    const int32_t result = decoder->Decode(sink.images[i], 0);
    c.Expect(result == WEBRTC_VIDEO_CODEC_OK,
             "frame " + std::to_string(i) + " returned " + Code(result));
  }
  c.Expect(callback.frames() >= kFrames - 2,
           "decoded " + std::to_string(callback.frames()) + " of " +
               std::to_string(kFrames));
  c.Expect(callback.width() == kWidth && callback.height() == kHeight,
           "decoded size " + std::to_string(callback.width()) + "x" +
               std::to_string(callback.height()));
  const std::string name = decoder->GetDecoderInfo().implementation_name;
  c.Expect(name.rfind("MediaFoundation H264 Decoder", 0) == 0, "name " + name);
  c.Expect(decoder->Release() == WEBRTC_VIDEO_CODEC_OK, "Release");
  c.Expect(!webrtc::MFDecodeProcessHealth().decode_disabled(),
           "the real decoder hung");
  return name;
}

}  // namespace

// 0 passed, 1 failed, 2 skipped; `message` gets the failures, or a note.
extern "C" int32_t lk_mf_decode_watchdog_test(const char* scenario,
                                              char* message,
                                              size_t message_len) {
  Checks checks;
  std::string note;
  const std::string name = scenario ? scenario : "";
  if (name == "passes_through") {
    PassesThrough(checks);
  } else if (name == "hang_on_delta") {
    HangOnDelta(checks);
  } else if (name == "hang_on_keyframe") {
    HangOnKeyframe(checks);
  } else if (name == "silent") {
    Silent(checks);
  } else if (name == "configure_hang") {
    ConfigureHang(checks);
  } else if (name == "behind_fallback_wrapper") {
    BehindFallbackWrapper(checks);
  } else if (name == "real_decoder") {
    note = RealDecoder(checks);
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
