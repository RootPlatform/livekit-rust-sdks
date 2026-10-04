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

#ifndef WEBRTC_MF_REINIT_POLICY_H_
#define WEBRTC_MF_REINIT_POLICY_H_

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <optional>

namespace livekit_ffi {

// NVIDIA's H264 encoder MFT takes its VBV size from
// CODECAPI_AVEncCommonBufferSize once, when the media types are set, and
// silently ignores later updates (mean bitrate, max bitrate and
// MF_MT_AVG_BITRATE do follow). Its frames stay capped near that buffer however
// far the target rises, so the transform is rebuilt for the new target. Only
// upward, and rarely: a rebuild costs a keyframe.
class EncoderReinitPolicy {
 public:
  // SimulcastEncoderAdapter starts a layer the allocation cannot fit yet at
  // its 30 kbps minimum.
  static constexpr uint32_t kPlaceholderBps = 150'000;
  static constexpr uint32_t kRaiseFactor = 2;
  static constexpr int64_t kSustainMs = 1000;
  static constexpr int64_t kCooldownMs = 5000;
  static constexpr int64_t kWindowMs = 1000;
  static constexpr int kMinWindowFrames = 5;
  static constexpr uint64_t kFullBufferPercent = 50;
  // A buffer-sized average frame alone also describes an encoder that is meeting
  // a target of 2-3x the configured rate; starvation means it delivers well
  // below the target too.
  static constexpr uint64_t kStarvedPercent = 70;

  void OnConfigured(uint32_t configured_bps,
                    uint64_t buffer_bits,
                    int64_t now_ms) {
    configured_bps_ = configured_bps;
    buffer_bits_ = buffer_bits;
    configured_ms_ = now_ms;
    raised_since_ms_.reset();
    window_ = Window();
    last_ = Result();
  }

  void OnEncoded(size_t bytes, int64_t now_ms) {
    if (window_.start_ms < 0 || now_ms - window_.last_ms > 2 * kWindowMs) {
      window_ = Window();
      window_.start_ms = now_ms;
    }
    window_.last_ms = now_ms;
    window_.bits += static_cast<uint64_t>(bytes) * 8;
    window_.frames++;
    if (now_ms - window_.start_ms < kWindowMs) {
      return;
    }
    last_ = Result();
    if (window_.frames >= kMinWindowFrames) {
      last_.valid = true;
      last_.end_ms = now_ms;
      last_.average_frame_bits = window_.bits / window_.frames;
      last_.achieved_bps =
          window_.bits * 1000 / static_cast<uint64_t>(now_ms - window_.start_ms);
    }
    window_ = Window();
    window_.start_ms = now_ms;
    window_.last_ms = now_ms;
  }

  // Called for every frame about to be encoded.
  bool ShouldReinit(uint32_t target_bps, int64_t now_ms) {
    if (configured_bps_ == 0) {
      return false;
    }
    const uint64_t raised =
        static_cast<uint64_t>(configured_bps_) * kRaiseFactor;
    if (configured_bps_ < kPlaceholderBps) {
      return target_bps >= std::max<uint64_t>(raised, kPlaceholderBps);
    }
    if (target_bps < raised) {
      raised_since_ms_.reset();
      return false;
    }
    if (!raised_since_ms_) {
      raised_since_ms_ = now_ms;
    }
    if (now_ms - *raised_since_ms_ < kSustainMs ||
        now_ms - configured_ms_ < kCooldownMs) {
      return false;
    }
    return last_.valid && now_ms - last_.end_ms <= 2 * kWindowMs &&
           last_.average_frame_bits * 100 >= buffer_bits_ * kFullBufferPercent &&
           last_.achieved_bps * 100 <
               static_cast<uint64_t>(target_bps) * kStarvedPercent;
  }

  uint32_t configured_bps() const { return configured_bps_; }
  uint64_t last_average_frame_bits() const {
    return last_.valid ? last_.average_frame_bits : 0;
  }

 private:
  struct Window {
    int64_t start_ms = -1;
    int64_t last_ms = -1;
    uint64_t bits = 0;
    int frames = 0;
  };
  struct Result {
    bool valid = false;
    int64_t end_ms = 0;
    uint64_t average_frame_bits = 0;
    uint64_t achieved_bps = 0;
  };

  uint32_t configured_bps_ = 0;
  uint64_t buffer_bits_ = 0;
  int64_t configured_ms_ = 0;
  std::optional<int64_t> raised_since_ms_;
  Window window_;
  Result last_;
};

}  // namespace livekit_ffi

#endif  // WEBRTC_MF_REINIT_POLICY_H_
