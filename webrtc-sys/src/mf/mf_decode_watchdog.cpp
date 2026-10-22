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

#include "mf_decode_watchdog.h"

#include <windows.h>

#include <condition_variable>
#include <mutex>
#include <thread>
#include <utility>

#include <modules/video_coding/include/video_error_codes.h>

#include "rtc_base/logging.h"

namespace webrtc {

MFDecodeHealth& MFDecodeProcessHealth() {
  static MFDecodeHealth* health = new MFDecodeHealth();
  return *health;
}

// The decoder and the thread that makes every call into it. The thread holds
// a reference until it exits, so an abandoned call can still return into
// valid memory; the decoder is destroyed on the thread after its last call.
class MFDecodeWatchdog::Worker {
 public:
  explicit Worker(std::unique_ptr<VideoDecoder> decoder)
      : decoder_(std::move(decoder)),
        proxy_(this),
        done_(CreateEventW(nullptr, FALSE, FALSE, nullptr)) {
    decoder_->RegisterDecodeCompleteCallback(&proxy_);
  }

  ~Worker() {
    if (done_) {
      CloseHandle(done_);
    }
  }

  static void Start(const std::shared_ptr<Worker>& worker) {
    std::thread([worker] { worker->Loop(); }).detach();
  }

  // Runs `call` on the thread; false when it has not returned after
  // `timeout_ms`. Waits that a sleeping machine spends asleep do not count.
  bool Run(std::function<void(VideoDecoder&)> call, int timeout_ms) {
    if (!done_) {
      return false;
    }
    {
      std::lock_guard<std::mutex> lock(mutex_);
      job_ = std::move(call);
    }
    wake_.notify_one();
    return WaitForSingleObject(done_, static_cast<DWORD>(timeout_ms)) ==
           WAIT_OBJECT_0;
  }

  // Only between calls, while the thread is idle.
  const VideoDecoder& decoder() const { return *decoder_; }

  void SetCallback(DecodedImageCallback* callback) {
    std::lock_guard<std::mutex> lock(delivery_mutex_);
    callback_ = callback;
  }

  int64_t outputs() const { return outputs_.load(std::memory_order_relaxed); }

  // Nothing the decoder outputs from now on reaches WebRTC, and the thread
  // exits once the call it is in, if any, returns.
  void Detach() {
    SetCallback(nullptr);
    {
      std::lock_guard<std::mutex> lock(mutex_);
      quit_ = true;
    }
    wake_.notify_one();
  }

 private:
  class Proxy : public DecodedImageCallback {
   public:
    explicit Proxy(Worker* worker) : worker_(worker) {}

    int32_t Decoded(VideoFrame& frame) override {
      int32_t result = WEBRTC_VIDEO_CODEC_OK;
      worker_->Deliver(
          [&](DecodedImageCallback& callback) { result = callback.Decoded(frame); });
      return result;
    }

    int32_t Decoded(VideoFrame& frame, int64_t decode_time_ms) override {
      int32_t result = WEBRTC_VIDEO_CODEC_OK;
      worker_->Deliver([&](DecodedImageCallback& callback) {
        result = callback.Decoded(frame, decode_time_ms);
      });
      return result;
    }

    void Decoded(VideoFrame& frame,
                 std::optional<int32_t> decode_time_ms,
                 std::optional<uint8_t> qp) override {
      worker_->Deliver([&](DecodedImageCallback& callback) {
        callback.Decoded(frame, decode_time_ms, qp);
      });
    }

   private:
    Worker* const worker_;
  };

  template <typename F>
  void Deliver(F&& deliver) {
    std::lock_guard<std::mutex> lock(delivery_mutex_);
    outputs_.fetch_add(1, std::memory_order_relaxed);
    if (callback_) {
      deliver(*callback_);
    }
  }

  void Loop() {
    for (;;) {
      std::function<void(VideoDecoder&)> job;
      {
        std::unique_lock<std::mutex> lock(mutex_);
        wake_.wait(lock, [this] { return quit_ || job_; });
        if (!job_) {
          break;
        }
        job = std::move(job_);
        job_ = nullptr;
      }
      job(*decoder_);
      SetEvent(done_);
    }
    decoder_.reset();
  }

  std::unique_ptr<VideoDecoder> decoder_;
  Proxy proxy_;
  const HANDLE done_;

  std::mutex mutex_;
  std::condition_variable wake_;
  std::function<void(VideoDecoder&)> job_;
  bool quit_ = false;

  std::mutex delivery_mutex_;
  DecodedImageCallback* callback_ = nullptr;
  std::atomic<int64_t> outputs_{0};
};

MFDecodeWatchdog::MFDecodeWatchdog(std::unique_ptr<VideoDecoder> decoder,
                                   MFDecodeWatchdogLimits limits,
                                   MFDecodeHealth* health,
                                   std::function<void()> on_hang)
    : limits_(limits),
      health_(health),
      on_hang_(std::move(on_hang)),
      info_(decoder->GetDecoderInfo()) {
  worker_ = std::make_shared<Worker>(std::move(decoder));
  Worker::Start(worker_);
}

MFDecodeWatchdog::~MFDecodeWatchdog() {
  if (worker_) {
    worker_->Detach();
  }
}

bool MFDecodeWatchdog::Configure(const Settings& settings) {
  if (health_->decode_disabled()) {
    RTC_LOG(LS_WARNING) << "MF H264 decoder watchdog: MediaFoundation decoding "
                           "is off after a hang earlier in this process; "
                           "decoding in software.";
    return false;
  }
  if (!worker_) {
    return false;
  }
  gave_up_ = false;
  ResetSilence();
  auto configured = std::make_shared<bool>(false);
  if (!worker_->Run(
          [configured, settings](VideoDecoder& decoder) {
            *configured = decoder.Configure(settings);
          },
          limits_.configure_timeout_ms)) {
    Abandon("Configure", limits_.configure_timeout_ms);
    return false;
  }
  info_ = worker_->decoder().GetDecoderInfo();
  return *configured;
}

int32_t MFDecodeWatchdog::Decode(const EncodedImage& input_image,
                                 int64_t render_time_ms) {
  if (gave_up_ || !worker_) {
    return WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE;
  }
  // The copy shares the refcounted bitstream, which an abandoned call may
  // read after WebRTC has dropped the frame.
  auto result = std::make_shared<int32_t>(WEBRTC_VIDEO_CODEC_ERROR);
  if (!worker_->Run(
          [result, image = input_image, render_time_ms](VideoDecoder& decoder) {
            *result = decoder.Decode(image, render_time_ms);
          },
          limits_.call_timeout_ms)) {
    Abandon("Decode", limits_.call_timeout_ms);
    return GiveUp(input_image);
  }
  info_ = worker_->decoder().GetDecoderInfo();

  const int64_t outputs = worker_->outputs();
  if (outputs != outputs_seen_) {
    outputs_seen_ = outputs;
    ResetSilence();
    return *result;
  }
  // Failed frames are the decoder's to handle: WebRTC asks for a keyframe
  // after each, and the decoder falls back on its own after too many.
  if (*result != WEBRTC_VIDEO_CODEC_OK) {
    return *result;
  }
  const auto now = std::chrono::steady_clock::now();
  if (silent_frames_++ == 0) {
    silent_since_ = now;
  }
  const auto silent_for =
      std::chrono::duration_cast<std::chrono::milliseconds>(now -
                                                            silent_since_);
  if (silent_frames_ < limits_.silent_frames ||
      silent_for.count() < limits_.silent_ms) {
    return *result;
  }
  RTC_LOG(LS_ERROR) << "MF H264 decoder watchdog: " << silent_frames_
                    << " frames in " << silent_for.count()
                    << " ms without a decoded frame; decoding this stream in "
                       "software.";
  health_->RecordSilence();
  if (!worker_->Run([](VideoDecoder& decoder) { decoder.Release(); },
                    limits_.call_timeout_ms)) {
    Abandon("Release", limits_.call_timeout_ms);
  }
  return GiveUp(input_image);
}

int32_t MFDecodeWatchdog::RegisterDecodeCompleteCallback(
    DecodedImageCallback* callback) {
  if (worker_) {
    worker_->SetCallback(callback);
  }
  return WEBRTC_VIDEO_CODEC_OK;
}

int32_t MFDecodeWatchdog::Release() {
  ResetSilence();
  if (!worker_) {
    return WEBRTC_VIDEO_CODEC_OK;
  }
  auto released = std::make_shared<int32_t>(WEBRTC_VIDEO_CODEC_OK);
  if (!worker_->Run(
          [released](VideoDecoder& decoder) {
            *released = decoder.Release();
          },
          limits_.call_timeout_ms)) {
    Abandon("Release", limits_.call_timeout_ms);
    return WEBRTC_VIDEO_CODEC_OK;
  }
  return *released;
}

VideoDecoder::DecoderInfo MFDecodeWatchdog::GetDecoderInfo() const {
  return info_;
}

void MFDecodeWatchdog::Abandon(const char* call, int timeout_ms) {
  RTC_LOG(LS_ERROR) << "MF H264 decoder watchdog: " << call
                    << "() has not returned after " << timeout_ms
                    << " ms; leaving it on its thread and decoding H264 in "
                       "software for the rest of this process.";
  worker_->Detach();
  worker_.reset();
  health_->RecordHang();
  if (on_hang_) {
    on_hang_();
  }
}

int32_t MFDecodeWatchdog::GiveUp(const EncodedImage& input_image) {
  gave_up_ = true;
  return input_image.IsKey() ? WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE
                             : WEBRTC_VIDEO_CODEC_ERROR;
}

void MFDecodeWatchdog::ResetSilence() {
  silent_frames_ = 0;
}

}  // namespace webrtc

// For webrtc-sys's mf_device.rs, which livekit-ffi exports as
// livekit_ffi_mf_decode_watchdog_counts.
extern "C" void lk_mf_decode_watchdog_counts(uint32_t* hangs,
                                             uint32_t* silences) {
  const webrtc::MFDecodeHealth& health = webrtc::MFDecodeProcessHealth();
  if (hangs) {
    *hangs = health.hangs();
  }
  if (silences) {
    *silences = health.silences();
  }
}
