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

#pragma once

#include <array>
#include <atomic>
#include <cstddef>
#include <cstdint>

namespace livekit_ffi {

struct RootAudioMonitorStats {
  bool active = false;
  uint64_t frames = 0;
  uint64_t dropped_frames = 0;
};

// The mic test: the capture post-processor's output (what a call would send)
// played on the playout device. The transport tap mixes it in after
// libwebrtc's render path has handed its mix to the APM, so the echo
// canceller's reference never contains it.
//
// One producer (the capture post-processor: 10 ms of channel 0 at its rate)
// and one consumer (the tap's render callback) share a ring of preallocated
// 10 ms frames. Playback starts once a prime target of frames is queued
// (older ones are skipped) and starts over the same way after an underrun.
// The consumer owns the target: kPrimeFrames, one more per underrun (up to
// kMaxPrimeFrames, for devices that run their callbacks in bursts) and one
// less after kDecayCallbacks without one. When more than kTrimSlack frames
// above the target stayed queued for kTrimWindow callbacks, the consumer
// skips the excess (clock drift, a render stall). The consumer converts the
// capture rate to the render rate (Catmull-Rom interpolation on an integer
// phase, so equal rates pass every sample through unchanged) and fades in
// after priming and out before an underrun or a skip.
//
// Real-time code on both sides: atomics only, no locks, allocation or logging.
class RootAudioMonitor {
 public:
  static constexpr int kMaxRate = 48000;
  static constexpr size_t kMaxFrame = kMaxRate / 100;
  // Render callbacks up to 384 kHz.
  static constexpr size_t kMaxRenderFrames = 3840;
  static constexpr uint64_t kSlots = 16;
  static constexpr uint64_t kPrimeFrames = 3;
  static constexpr uint64_t kMaxPrimeFrames = 8;
  static constexpr uint64_t kTrimSlack = 2;
  static constexpr uint32_t kTrimWindow = 100;
  static constexpr uint32_t kDecayCallbacks = 1000;
  // A capture frame that finds the ring full is counted as dropped only when
  // the render side ran within the last kAliveFrames capture frames; after
  // more than kStallFrames capture frames without a render callback the
  // consumer drops what queued (it is late) and primes again.
  static constexpr uint64_t kAliveFrames = 3;
  static constexpr uint64_t kStallFrames = 6;
  // Fade length: render rate / kFadeDivisor samples (2.5 ms).
  static constexpr uint32_t kFadeDivisor = 400;

  RootAudioMonitor() = default;
  RootAudioMonitor(const RootAudioMonitor&) = delete;
  RootAudioMonitor& operator=(const RootAudioMonitor&) = delete;

  // Control side. Enabling starts the stream over: what was queued before is
  // dropped and playback primes again.
  void SetEnabled(bool enabled);
  bool enabled() const;
  // Control side, around a device stop or start: frames queued so far are
  // stale; playback primes again from the frames that follow.
  void Resync();

  // Capture thread: one frame of `frames` FloatS16 samples (rate / 100) at
  // `rate` (8-48 kHz). Non-finite samples are stored as 0.
  void Push(const float* samples, size_t frames, int rate);

  // Render thread: adds the monitor to `audio`, interleaved int16 with
  // `channels` channels, every channel the same, saturating.
  void MixInto(int16_t* audio,
               size_t samples_per_channel,
               size_t channels,
               uint32_t sample_rate);

  // Any thread.
  RootAudioMonitorStats Read() const;

 private:
  struct Slot {
    std::array<float, kMaxFrame> samples{};
    uint32_t frames = 0;
    uint32_t rate = 0;
  };

  // Consumer side.
  bool NextSample(float* sample);
  bool FillWindow();
  void Unprime();
  void ResetTrim();
  // Renders up to `count` samples into render_; returns how many it could.
  size_t Render(size_t count);

  std::array<Slot, kSlots> slots_{};
  std::atomic<bool> enabled_{false};
  std::atomic<uint64_t> epoch_{0};
  // Frames written when the newest epoch began; the consumer skips them.
  std::atomic<uint64_t> epoch_start_{0};
  // Frames written (producer) and frames fully read (consumer).
  std::atomic<uint64_t> written_{0};
  std::atomic<uint64_t> read_{0};
  // Capture frames offered (producer), and that count as the consumer last
  // saw it: how long ago the render side ran.
  std::atomic<uint64_t> pushes_{0};
  std::atomic<uint64_t> consumer_mark_{0};
  std::atomic<uint64_t> frames_{0};
  std::atomic<uint64_t> dropped_{0};

  // Consumer only.
  uint64_t seen_epoch_ = 0;
  uint64_t target_ = kPrimeFrames;
  uint32_t calls_since_underrun_ = 0;
  uint64_t trim_min_ = 0;
  uint32_t trim_calls_ = 0;
  bool primed_ = false;
  bool window_ready_ = false;
  uint32_t fade_in_done_ = 0;
  uint32_t read_offset_ = 0;
  uint32_t source_rate_ = 0;
  uint32_t render_rate_ = 0;
  uint32_t fade_length_ = 1;
  // Source position past window_[1], in 1 / render_rate_ units.
  uint32_t phase_ = 0;
  std::array<float, 4> window_{};
  std::array<float, kMaxRenderFrames> render_{};
};

static_assert(std::atomic<uint64_t>::is_always_lock_free);
static_assert(std::atomic<bool>::is_always_lock_free);

}  // namespace livekit_ffi
