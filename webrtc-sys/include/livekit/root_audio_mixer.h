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
#include <vector>

#include "api/audio/audio_frame.h"
#include "api/audio/audio_mixer.h"
#include "rtc_base/synchronization/mutex.h"
#include "rust/cxx.h"

namespace livekit_ffi {
struct RootMixTestSource;
struct RootMixTestResult;
}  // namespace livekit_ffi
#include "webrtc-sys/src/root_audio_mixer.rs.h"

namespace livekit_ffi {

// Port of the app's SoftClipper: linear up to 0.8 of full scale, then a tanh
// knee into the remaining headroom. Works on samples normalized to [-1, 1].
float RootSoftClip(float sample);

struct RootAudioMixerStats {
  uint64_t mix_calls = 0;
  uint64_t max_gap_us = 0;
  uint32_t sources = 0;
  std::vector<uint32_t> ssrcs;
  double out_sumsq = 0;
  uint64_t out_samples = 0;
  uint64_t overlapping_mixes = 0;
};

// The factory's playout mixer: asks every source for 48 kHz, mixes in float with
// no limiter, soft clips and writes int16 at the requested channel count. Its
// output is what the ADM plays and what the echo canceller gets as reference.
//
// AddSource/RemoveSource run on the worker while Mix runs on the device's render
// thread. Mix takes no lock: sources live in a fixed slot array, and
// RemoveSource only returns once no Mix that may still hold the source runs.
// One Mix runs at a time; a second one that starts meanwhile (a render thread
// the ADM failed to stop) returns silence without touching the sources.
class RootAudioMixer : public webrtc::AudioMixer {
 public:
  static constexpr int kSampleRateHz = 48000;
  static constexpr size_t kSamplesPerChannel = kSampleRateHz / 100;
  static constexpr size_t kMaxSources = 256;
  static constexpr size_t kMaxOutputChannels = 8;

  RootAudioMixer();
  ~RootAudioMixer() override;

  bool AddSource(Source* source) override;
  void RemoveSource(Source* source) override;
  void Mix(size_t number_of_channels,
           webrtc::AudioFrame* audio_frame_for_mixing) override;

  // Called before a device starts pulling, so the idle time before it does not
  // count as a gap between Mix calls.
  void ResetGapClock();

  // Atomics only. Max gap is reset on read.
  RootAudioMixerStats ReadStats();

 private:
  // Per-source state for the mix; `gain` is where per-source volume (keyed by
  // the source's SSRC) plugs in.
  struct Entry {
    Entry(Source* source, int ssrc) : source(source), ssrc(ssrc) {}
    Source* const source;
    const int ssrc;
    float gain = 1.0f;
    webrtc::AudioFrame frame;
  };

  void Accumulate(const webrtc::AudioFrame& frame,
                  float gain,
                  size_t mix_channels);
  void WaitForMixToRelease();

  webrtc::Mutex control_mutex_;
  std::array<std::atomic<Entry*>, kMaxSources> slots_{};
  std::array<std::atomic<uint32_t>, kMaxSources> slot_ssrcs_{};
  std::atomic<size_t> slot_limit_{0};
  std::atomic<uint64_t> mix_epoch_{0};
  std::atomic<uint32_t> source_count_{0};
  std::atomic<bool> in_mix_{false};

  std::array<float, kSamplesPerChannel * 2> accumulator_{};
  int64_t last_mix_ns_ = 0;
  float master_gain_ = 1.0f;

  std::atomic<bool> gap_reset_{false};
  std::atomic<uint64_t> mix_calls_{0};
  std::atomic<uint64_t> max_gap_ns_{0};
  std::atomic<double> out_sumsq_{0.0};
  std::atomic<uint64_t> out_samples_{0};
  std::atomic<uint64_t> overlapping_mixes_{0};
};

static_assert(std::atomic<double>::is_always_lock_free);

float root_audio_soft_clip(float sample);

RootMixTestResult root_audio_mixer_run_for_test(
    rust::Slice<const RootMixTestSource> sources,
    rust::Slice<const size_t> remove,
    size_t out_channels);

}  // namespace livekit_ffi
