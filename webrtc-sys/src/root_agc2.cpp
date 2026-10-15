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

#include "livekit/root_agc2.h"

#include <algorithm>
#include <cmath>
#include <utility>

#include "api/audio/audio_processing.h"
#include "api/audio/builtin_audio_processing_builder.h"

namespace livekit_ffi {
namespace {

bool SameTuning(const RootAgc2Tuning& a, const RootAgc2Tuning& b) {
  return a.headroom_db == b.headroom_db && a.max_gain_db == b.max_gain_db &&
         a.max_gain_change_db_per_second == b.max_gain_change_db_per_second &&
         a.max_output_noise_dbfs == b.max_output_noise_dbfs;
}

}  // namespace

struct RootAgc2::Instance {
  webrtc::scoped_refptr<webrtc::AudioProcessing> apm;
  int rate = 0;
  size_t frames = 0;
  RootAgc2Tuning tuning{};
  float gain_db = 0.0f;
  std::array<float, kMaxFrames> in{};
  std::array<float, kMaxFrames> out{};
};

RootAgc2::RootAgc2(webrtc::Environment env) : env_(std::move(env)) {}

RootAgc2::~RootAgc2() {
  delete active_.exchange(nullptr);
  delete pending_.exchange(nullptr);
  delete retired_.exchange(nullptr);
}

std::unique_ptr<RootAgc2::Instance> RootAgc2::Build(
    int rate,
    const RootAgc2Tuning& tuning,
    float initial_gain_db) const {
  const size_t frames = static_cast<size_t>(std::max(rate, 0) / 100);
  if (rate < 8000 || rate > 48000 || frames == 0 || frames > kMaxFrames) {
    return nullptr;
  }
  webrtc::AudioProcessing::Config config;
  config.high_pass_filter.enabled = false;
  config.echo_canceller.enabled = false;
  config.noise_suppression.enabled = false;
  config.gain_controller1.enabled = false;
  auto& agc2 = config.gain_controller2;
  agc2.enabled = true;
  agc2.input_volume_controller.enabled = false;
  agc2.fixed_digital.gain_db = 0.0f;
  agc2.adaptive_digital.enabled = true;
  agc2.adaptive_digital.headroom_db = tuning.headroom_db;
  agc2.adaptive_digital.max_gain_db = tuning.max_gain_db;
  agc2.adaptive_digital.initial_gain_db = initial_gain_db;
  agc2.adaptive_digital.max_gain_change_db_per_second =
      tuning.max_gain_change_db_per_second;
  agc2.adaptive_digital.max_output_noise_level_dbfs =
      tuning.max_output_noise_dbfs;
  auto instance = std::make_unique<Instance>();
  instance->apm = webrtc::BuiltinAudioProcessingBuilder(config).Build(env_);
  if (!instance->apm) {
    return nullptr;
  }
  const webrtc::StreamConfig stream(rate, 1);
  if (instance->apm->Initialize(
          webrtc::ProcessingConfig{{stream, stream, stream, stream}}) !=
      webrtc::AudioProcessing::kNoError) {
    return nullptr;
  }
  const float* src = instance->in.data();
  float* dst = instance->out.data();
  if (instance->apm->ProcessStream(&src, stream, stream, &dst) !=
      webrtc::AudioProcessing::kNoError) {
    return nullptr;
  }
  instance->rate = rate;
  instance->frames = frames;
  instance->tuning = tuning;
  instance->gain_db = initial_gain_db;
  return instance;
}

void RootAgc2::CollectLocked() {
  delete retired_.exchange(nullptr, std::memory_order_acq_rel);
}

void RootAgc2::Reset(float gain_db) {
  webrtc::MutexLock lock(&mutex_);
  const float db = std::isfinite(gain_db) ? std::max(0.0f, gain_db) : 0.0f;
  reset_gain_db_ = db;
  last_reset_db_ = db;
}

void RootAgc2::ResetSession() {
  webrtc::MutexLock lock(&mutex_);
  reset_gain_db_ = 0.0f;
  last_reset_db_ = 0.0f;
  applied_gain_db_.store(std::numeric_limits<float>::quiet_NaN(),
                         std::memory_order_relaxed);
}

void RootAgc2::Service(bool wanted,
                       int rate,
                       const RootAgc2Tuning& requested) {
  webrtc::MutexLock lock(&mutex_);
  CollectLocked();
  if (!wanted) {
    wanted_ = false;
    return;
  }
  if (!wanted_ && !reset_gain_db_.has_value()) {
    reset_gain_db_ = last_reset_db_;
  }
  wanted_ = true;
  const RootAgc2Tuning tuning = root_agc2_tuning(requested);
  const int target =
      rate > 0 ? rate : (built_rate_ > 0 ? built_rate_ : kDefaultRate);
  if (built_ && built_rate_ == target && SameTuning(built_tuning_, tuning) &&
      !reset_gain_db_.has_value()) {
    return;
  }
  float initial = last_reset_db_;
  if (reset_gain_db_.has_value()) {
    initial = *reset_gain_db_;
  } else {
    const float applied = applied_gain_db_.load(std::memory_order_relaxed);
    if (std::isfinite(applied)) {
      initial = applied;
    }
  }
  initial = std::clamp(initial, 0.0f, tuning.max_gain_db);
  std::unique_ptr<Instance> next = Build(target, tuning, initial);
  if (!next) {
    ++build_failures_;
    return;
  }
  built_ = true;
  built_rate_ = target;
  built_tuning_ = tuning;
  reset_gain_db_.reset();
  ++builds_;
  delete pending_.exchange(next.release(), std::memory_order_acq_rel);
}

RootAgc2::Stats RootAgc2::ReadStats() const {
  webrtc::MutexLock lock(&mutex_);
  Stats stats;
  stats.builds = builds_;
  stats.build_failures = build_failures_;
  stats.rate = built_ ? built_rate_ : 0;
  stats.tuning = built_ ? built_tuning_ : RootAgc2TuningUnset();
  return stats;
}

RootAgc2::Frame RootAgc2::Process(float* samples, size_t frames, int rate) {
  Frame frame;
  if (pending_.load(std::memory_order_acquire) != nullptr &&
      retired_.load(std::memory_order_acquire) == nullptr) {
    if (Instance* next =
            pending_.exchange(nullptr, std::memory_order_acq_rel)) {
      if (Instance* replaced =
              active_.exchange(next, std::memory_order_acq_rel)) {
        retired_.store(replaced, std::memory_order_release);
      }
    }
  }
  Instance* agc2 = active_.load(std::memory_order_relaxed);
  if (agc2 == nullptr || agc2->rate != rate || agc2->frames != frames ||
      samples == nullptr) {
    return frame;
  }
  constexpr float kToUnit = 1.0f / 32768.0f;
  double in = 0.0;
  for (size_t i = 0; i < frames; ++i) {
    in += static_cast<double>(samples[i]) * samples[i];
    agc2->in[i] = samples[i] * kToUnit;
  }
  const webrtc::StreamConfig stream(rate, 1);
  const float* src = agc2->in.data();
  float* dst = agc2->out.data();
  if (agc2->apm->ProcessStream(&src, stream, stream, &dst) !=
      webrtc::AudioProcessing::kNoError) {
    return frame;
  }
  double out = 0.0;
  for (size_t i = 0; i < frames; ++i) {
    samples[i] = agc2->out[i] * 32768.0f;
    out += static_cast<double>(samples[i]) * samples[i];
  }
  if (in > kSilenceMeanSquare * static_cast<double>(frames) && out > 0.0) {
    agc2->gain_db = static_cast<float>(10.0 * std::log10(out / in));
  }
  applied_gain_db_.store(agc2->gain_db, std::memory_order_relaxed);
  frame.ran = true;
  frame.gain_db = agc2->gain_db;
  frame.tuning = agc2->tuning;
  return frame;
}

}  // namespace livekit_ffi
