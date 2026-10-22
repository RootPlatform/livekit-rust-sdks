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

#ifndef WEBRTC_MF_DECODE_WATCHDOG_H_
#define WEBRTC_MF_DECODE_WATCHDOG_H_

#include <atomic>
#include <chrono>
#include <cstdint>
#include <functional>
#include <memory>

#include <api/video_codecs/video_decoder.h>

namespace webrtc {

struct MFDecodeWatchdogLimits {
  // A Configure() still running after this long is abandoned. It may create
  // the adapter's D3D11 device.
  int configure_timeout_ms = 5000;
  // A Decode() or Release() still running after this long is abandoned.
  int call_timeout_ms = 1500;
  // A decoder that returned WEBRTC_VIDEO_CODEC_OK from this many Decode()
  // calls in a row, over at least this long, without one decoded frame is
  // given up on for its stream.
  int silent_frames = 20;
  int silent_ms = 1500;
};

// What the watchdogs have seen in this process. A hang turns MediaFoundation
// decoding off until the process exits: the hung call stays stuck in the
// driver, and on the machine of ROOT-DESKTOP-3QM a fresh process (so a fresh
// device) hung again within 17 s.
class MFDecodeHealth {
 public:
  bool decode_disabled() const { return hangs() > 0; }
  uint32_t hangs() const { return hangs_.load(std::memory_order_relaxed); }
  uint32_t silences() const {
    return silences_.load(std::memory_order_relaxed);
  }
  void RecordHang() { hangs_.fetch_add(1, std::memory_order_relaxed); }
  void RecordSilence() { silences_.fetch_add(1, std::memory_order_relaxed); }

 private:
  std::atomic<uint32_t> hangs_{0};
  std::atomic<uint32_t> silences_{0};
};

MFDecodeHealth& MFDecodeProcessHealth();

// Runs a hardware decoder on a thread of its own, so that a call stuck in the
// driver cannot hold WebRTC's decode thread. Every call waits for the thread
// with a time limit. When a call misses it, the call is left running on the
// thread, the decoder never reaches WebRTC's callback again, and decoding
// goes to the software decoder (VideoDecoderSoftwareFallbackWrapper):
//
// - A stuck Decode() stops WebRTC's receive stream without a trace: its
//   decode queue waits for the call, so no frame is decoded or dropped and the
//   no-decodable-frame timer that sends PLIs never runs again. Every stream
//   on the shared device stops with it.
// - A decoder that takes frames and returns OK but never outputs one leaves
//   WebRTC waiting just the same, without a keyframe request.
//
// After giving up on a keyframe the watchdog returns
// WEBRTC_VIDEO_CODEC_FALLBACK_SOFTWARE, so the software decoder decodes that
// keyframe. After giving up on a delta frame it returns
// WEBRTC_VIDEO_CODEC_ERROR, which makes WebRTC ask the sender for a keyframe
// and hand over only keyframes, and the fallback follows on the next call.
class MFDecodeWatchdog : public VideoDecoder {
 public:
  MFDecodeWatchdog(std::unique_ptr<VideoDecoder> decoder,
                   MFDecodeWatchdogLimits limits = {},
                   MFDecodeHealth* health = &MFDecodeProcessHealth(),
                   std::function<void()> on_hang = nullptr);
  MFDecodeWatchdog(const MFDecodeWatchdog&) = delete;
  MFDecodeWatchdog& operator=(const MFDecodeWatchdog&) = delete;
  ~MFDecodeWatchdog() override;

  bool Configure(const Settings& settings) override;
  int32_t Decode(const EncodedImage& input_image,
                 int64_t render_time_ms) override;
  int32_t RegisterDecodeCompleteCallback(
      DecodedImageCallback* callback) override;
  int32_t Release() override;
  DecoderInfo GetDecoderInfo() const override;

 private:
  class Worker;

  void Abandon(const char* call, int timeout_ms);
  int32_t GiveUp(const EncodedImage& input_image);
  void ResetSilence();

  const MFDecodeWatchdogLimits limits_;
  MFDecodeHealth* const health_;
  const std::function<void()> on_hang_;
  // Null once a call was abandoned.
  std::shared_ptr<Worker> worker_;
  DecoderInfo info_;
  bool gave_up_ = false;
  int64_t outputs_seen_ = 0;
  int silent_frames_ = 0;
  std::chrono::steady_clock::time_point silent_since_;
};

}  // namespace webrtc

#endif  // WEBRTC_MF_DECODE_WATCHDOG_H_
