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

#include "livekit/root_audio_monitor.h"

#include <algorithm>
#include <cmath>

namespace livekit_ffi {
namespace {

// As libwebrtc's FloatS16ToS16, so equal rates play exactly what the APM
// would put out.
int16_t ToS16(float value) {
  value = std::min(value, 32767.0f);
  value = std::max(value, -32768.0f);
  return static_cast<int16_t>(value + std::copysign(0.5f, value));
}

int16_t Saturate(int32_t value) {
  return static_cast<int16_t>(std::clamp<int32_t>(value, -32768, 32767));
}

float CatmullRom(const std::array<float, 4>& w, float t) {
  return w[1] +
         0.5f * t *
             (w[2] - w[0] +
              t * (2.0f * w[0] - 5.0f * w[1] + 4.0f * w[2] - w[3] +
                   t * (3.0f * (w[1] - w[2]) + w[3] - w[0])));
}

}  // namespace

void RootAudioMonitor::SetEnabled(bool enabled) {
  if (enabled) {
    epoch_start_.store(written_.load(std::memory_order_acquire),
                       std::memory_order_relaxed);
    epoch_.fetch_add(1, std::memory_order_release);
  }
  enabled_.store(enabled, std::memory_order_release);
}

bool RootAudioMonitor::enabled() const {
  return enabled_.load(std::memory_order_acquire);
}

void RootAudioMonitor::Push(const float* samples, size_t frames, int rate) {
  if (!enabled_.load(std::memory_order_acquire)) {
    return;
  }
  if (samples == nullptr || frames == 0 || frames > kMaxFrame || rate < 8000 ||
      rate > kMaxRate) {
    return;
  }
  const uint64_t written = written_.load(std::memory_order_relaxed);
  const uint64_t read = read_.load(std::memory_order_acquire);
  if (written - read >= kMaxQueuedFrames) {
    if (playing_.load(std::memory_order_relaxed)) {
      dropped_.fetch_add(1, std::memory_order_relaxed);
    }
    return;
  }
  Slot& slot = slots_[written % kSlots];
  for (size_t i = 0; i < frames; ++i) {
    const float value = samples[i];
    slot.samples[i] = std::isfinite(value) ? value : 0.0f;
  }
  slot.frames = static_cast<uint32_t>(frames);
  slot.rate = static_cast<uint32_t>(rate);
  written_.store(written + 1, std::memory_order_release);
}

bool RootAudioMonitor::NextSample(float* sample) {
  const uint64_t read = read_.load(std::memory_order_relaxed);
  if (read == written_.load(std::memory_order_acquire)) {
    return false;
  }
  const Slot& slot = slots_[read % kSlots];
  *sample = slot.samples[read_offset_];
  source_rate_ = slot.rate;
  if (++read_offset_ >= slot.frames) {
    read_offset_ = 0;
    read_.store(read + 1, std::memory_order_release);
  }
  return true;
}

bool RootAudioMonitor::FillWindow() {
  float first = 0.0f;
  float second = 0.0f;
  float third = 0.0f;
  if (!NextSample(&first) || !NextSample(&second) || !NextSample(&third)) {
    return false;
  }
  window_ = {first, first, second, third};
  phase_ = 0;
  window_ready_ = true;
  return true;
}

void RootAudioMonitor::Unprime() {
  primed_ = false;
  window_ready_ = false;
  playing_.store(false, std::memory_order_relaxed);
}

void RootAudioMonitor::MixInto(int16_t* audio,
                               size_t samples_per_channel,
                               size_t channels,
                               uint32_t sample_rate) {
  if (!enabled_.load(std::memory_order_acquire)) {
    if (primed_) {
      Unprime();
    }
    return;
  }
  if (audio == nullptr || samples_per_channel == 0 || channels == 0 ||
      sample_rate == 0) {
    return;
  }
  const uint64_t epoch = epoch_.load(std::memory_order_acquire);
  if (epoch != seen_epoch_) {
    seen_epoch_ = epoch;
    Unprime();
    const uint64_t start = epoch_start_.load(std::memory_order_relaxed);
    if (start > read_.load(std::memory_order_relaxed)) {
      read_offset_ = 0;
      read_.store(start, std::memory_order_release);
    }
  }
  if (!primed_) {
    const uint64_t written = written_.load(std::memory_order_acquire);
    const uint64_t read = read_.load(std::memory_order_relaxed);
    if (written - read < kPrimeFrames) {
      return;
    }
    if (written - read > kPrimeFrames) {
      read_offset_ = 0;
      read_.store(written - kPrimeFrames, std::memory_order_release);
    }
    primed_ = true;
    playing_.store(true, std::memory_order_relaxed);
  }
  if (render_rate_ != sample_rate) {
    render_rate_ = sample_rate;
    phase_ = 0;
  }
  if (!window_ready_ && !FillWindow()) {
    dropped_.fetch_add(1, std::memory_order_relaxed);
    Unprime();
    return;
  }

  const float inverse_rate = 1.0f / static_cast<float>(render_rate_);
  for (size_t i = 0; i < samples_per_channel; ++i) {
    while (phase_ >= render_rate_) {
      float next = 0.0f;
      if (!NextSample(&next)) {
        dropped_.fetch_add(1, std::memory_order_relaxed);
        Unprime();
        return;
      }
      window_ = {window_[1], window_[2], window_[3], next};
      phase_ -= render_rate_;
    }
    const int16_t value =
        ToS16(CatmullRom(window_, static_cast<float>(phase_) * inverse_rate));
    int16_t* frame = audio + i * channels;
    for (size_t c = 0; c < channels; ++c) {
      frame[c] = Saturate(static_cast<int32_t>(frame[c]) + value);
    }
    phase_ += source_rate_;
  }
  frames_.fetch_add(1, std::memory_order_relaxed);
}

RootAudioMonitorStats RootAudioMonitor::Read() const {
  RootAudioMonitorStats stats;
  stats.active = enabled_.load(std::memory_order_acquire);
  stats.frames = frames_.load(std::memory_order_relaxed);
  stats.dropped_frames = dropped_.load(std::memory_order_relaxed);
  return stats;
}

}  // namespace livekit_ffi
