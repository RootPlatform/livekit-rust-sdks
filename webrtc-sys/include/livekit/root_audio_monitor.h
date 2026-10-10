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
// 10 ms frames. Playback starts with kPrimeFrames queued (older ones are
// skipped) and starts over the same way after an underrun; a frame that finds
// kMaxQueuedFrames queued while playing is dropped. The consumer converts the
// capture rate to the render rate (Catmull-Rom interpolation on an integer
// phase, so equal rates pass every sample through unchanged).
//
// Real-time code on both sides: atomics only, no locks, allocation or logging.
class RootAudioMonitor {
 public:
  static constexpr int kMaxRate = 48000;
  static constexpr size_t kMaxFrame = kMaxRate / 100;
  static constexpr uint64_t kSlots = 8;
  static constexpr uint64_t kPrimeFrames = 2;
  static constexpr uint64_t kMaxQueuedFrames = 4;

  RootAudioMonitor() = default;
  RootAudioMonitor(const RootAudioMonitor&) = delete;
  RootAudioMonitor& operator=(const RootAudioMonitor&) = delete;

  // Control side. Enabling starts the stream over: what was queued before is
  // dropped and playback primes again.
  void SetEnabled(bool enabled);
  bool enabled() const;

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

  std::array<Slot, kSlots> slots_{};
  std::atomic<bool> enabled_{false};
  std::atomic<uint64_t> epoch_{0};
  // Frames written when the newest epoch began; the consumer skips them.
  std::atomic<uint64_t> epoch_start_{0};
  // Frames written (producer) and frames fully read (consumer).
  std::atomic<uint64_t> written_{0};
  std::atomic<uint64_t> read_{0};
  std::atomic<bool> playing_{false};
  std::atomic<uint64_t> frames_{0};
  std::atomic<uint64_t> dropped_{0};

  // Consumer only.
  uint64_t seen_epoch_ = 0;
  bool primed_ = false;
  bool window_ready_ = false;
  uint32_t read_offset_ = 0;
  uint32_t source_rate_ = 0;
  uint32_t render_rate_ = 0;
  // Source position past window_[1], in 1 / render_rate_ units.
  uint32_t phase_ = 0;
  std::array<float, 4> window_{};
};

}  // namespace livekit_ffi
