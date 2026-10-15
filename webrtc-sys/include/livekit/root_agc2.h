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
#include <limits>
#include <memory>
#include <optional>

#include "api/environment/environment.h"
#include "api/scoped_refptr.h"
#include "livekit/root_audio_processing.h"
#include "rtc_base/synchronization/mutex.h"

namespace webrtc {
class AudioProcessing;
}  // namespace webrtc

namespace livekit_ffi {

// libwebrtc's AGC2 (GainController2, adaptive digital only, no input volume
// controller) as the platform engine's voice gain: a second AudioProcessing
// with nothing else enabled, run by the capture processor on the aligned
// voice after the Effects SDK. It lives in the capture state, so the APM
// variants the echo model switches between share it with the voice chain.
//
// The control side builds an instance for one rate (Initialize and a frame of
// silence, so nothing the APM sets up on its first frames happens on the
// audio thread) and stages it; the audio thread takes a staged instance at
// its next frame and hands the one it replaced back, which the control side
// frees. A gain reset (initial_gain_db), other settings or another rate is a
// new instance. A frame at a rate no instance is built for yet passes
// untouched (Process returns ran = false).
class RootAgc2 {
 public:
  // An input under -70 dBFS keeps the last gain measured.
  static constexpr double kSilenceMeanSquare = 32768.0 * 32768.0 * 1e-7;
  static constexpr size_t kMaxFrames = 480;
  static constexpr int kDefaultRate = 48000;

  struct Frame {
    bool ran = false;
    // 10·log10(Σout²/Σin²) of the frame, held while the input is under
    // -70 dBFS (the instance's initial gain before any).
    float gain_db = 0.0f;
    RootAgc2Tuning tuning{};
  };

  struct Stats {
    uint64_t builds = 0;
    uint64_t build_failures = 0;
    // The newest instance: its rate (0 before any) and settings.
    int rate = 0;
    RootAgc2Tuning tuning{};
  };

  explicit RootAgc2(webrtc::Environment env);
  ~RootAgc2();
  RootAgc2(const RootAgc2&) = delete;
  RootAgc2& operator=(const RootAgc2&) = delete;

  // Control side. A gain reset: the next instance starts from gain_db
  // (clamped to its maximum). A session start: from 0 dB, as the chain.
  void Reset(float gain_db);
  void ResetSession();
  // Builds the instance the audio thread should run next when `wanted` and
  // the newest one is for another rate or other settings, or a reset waits
  // (or AGC2 was not wanted before: it starts over from the last reset); a
  // rate change starts from the gain applied last. rate 0 (no frame yet)
  // keeps the newest instance's rate, else 48 kHz. Frees what the audio
  // thread let go of.
  void Service(bool wanted, int rate, const RootAgc2Tuning& requested);
  Stats ReadStats() const;

  // Audio thread: `samples` is channel 0 in FloatS16, `frames` = rate / 100.
  Frame Process(float* samples, size_t frames, int rate);

 private:
  struct Instance;

  std::unique_ptr<Instance> Build(int rate,
                                  const RootAgc2Tuning& tuning,
                                  float initial_gain_db) const;
  void CollectLocked();

  const webrtc::Environment env_;
  // In use (the audio thread swaps it), staged (the control side), and the
  // one a swap replaced (the audio thread leaves it, the control side frees
  // it; nothing is staged-in while one waits).
  std::atomic<Instance*> active_{nullptr};
  std::atomic<Instance*> pending_{nullptr};
  std::atomic<Instance*> retired_{nullptr};
  std::atomic<float> applied_gain_db_{std::numeric_limits<float>::quiet_NaN()};

  mutable webrtc::Mutex mutex_;
  bool built_ = false;
  int built_rate_ = 0;
  RootAgc2Tuning built_tuning_{};
  bool wanted_ = false;
  std::optional<float> reset_gain_db_;
  float last_reset_db_ = 0.0f;
  uint64_t builds_ = 0;
  uint64_t build_failures_ = 0;
};

}  // namespace livekit_ffi
