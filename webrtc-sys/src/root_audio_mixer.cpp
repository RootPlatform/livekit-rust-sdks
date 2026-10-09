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

#include "livekit/root_audio_mixer.h"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <memory>
#include <thread>
#include <utility>

#include "api/make_ref_counted.h"
#include "rtc_base/logging.h"

namespace livekit_ffi {
namespace {

constexpr float kSoftClipThreshold = 0.8f;
constexpr float kSoftClipHeadroom = 1.0f - kSoftClipThreshold;
constexpr float kS16Scale = 32768.0f;

int64_t NowNs() {
  return std::chrono::duration_cast<std::chrono::nanoseconds>(
             std::chrono::steady_clock::now().time_since_epoch())
      .count();
}

int16_t ToS16(float value) {
  value = std::min(32767.0f, std::max(-32768.0f, value));
  return static_cast<int16_t>(std::lround(value));
}

void UpdateMax(std::atomic<uint64_t>& max, uint64_t value) {
  uint64_t current = max.load(std::memory_order_relaxed);
  while (value > current &&
         !max.compare_exchange_weak(current, value, std::memory_order_relaxed)) {
  }
}

}  // namespace

float RootSoftClip(float sample) {
  const float magnitude = std::fabs(sample);
  if (magnitude <= kSoftClipThreshold) {
    return sample;
  }
  const float clipped =
      kSoftClipThreshold +
      kSoftClipHeadroom *
          std::tanh((magnitude - kSoftClipThreshold) / kSoftClipHeadroom);
  return sample >= 0.0f ? clipped : -clipped;
}

RootAudioMixer::RootAudioMixer() = default;

RootAudioMixer::~RootAudioMixer() {
  for (auto& slot : slots_) {
    delete slot.exchange(nullptr);
  }
}

bool RootAudioMixer::AddSource(Source* source) {
  const int ssrc = source->Ssrc();
  auto entry = std::make_unique<Entry>(source, ssrc);
  webrtc::MutexLock lock(&control_mutex_);
  for (size_t i = 0; i < kMaxSources; ++i) {
    if (slots_[i].load(std::memory_order_relaxed) != nullptr) {
      continue;
    }
    slot_ssrcs_[i].store(static_cast<uint32_t>(ssrc), std::memory_order_relaxed);
    slots_[i].store(entry.release(), std::memory_order_seq_cst);
    if (i + 1 > slot_limit_.load(std::memory_order_relaxed)) {
      slot_limit_.store(i + 1, std::memory_order_release);
    }
    const uint32_t count =
        source_count_.fetch_add(1, std::memory_order_relaxed) + 1;
    RTC_LOG(LS_INFO) << "RootAudioMixer: added source ssrc="
                     << static_cast<uint32_t>(ssrc) << " (" << count
                     << " sources)";
    return true;
  }
  RTC_LOG(LS_ERROR) << "RootAudioMixer: no free slot for source ssrc="
                    << static_cast<uint32_t>(ssrc);
  return false;
}

void RootAudioMixer::RemoveSource(Source* source) {
  Entry* removed = nullptr;
  {
    webrtc::MutexLock lock(&control_mutex_);
    const size_t limit = slot_limit_.load(std::memory_order_relaxed);
    for (size_t i = 0; i < limit; ++i) {
      Entry* entry = slots_[i].load(std::memory_order_relaxed);
      if (entry != nullptr && entry->source == source) {
        slots_[i].store(nullptr, std::memory_order_seq_cst);
        source_count_.fetch_sub(1, std::memory_order_relaxed);
        removed = entry;
        break;
      }
    }
  }
  if (removed == nullptr) {
    return;
  }
  WaitForMixToRelease();
  delete removed;
}

// Mix makes the epoch odd before it loads any slot and even after its last use
// of an entry. A Mix that began before the slot was cleared shows up as an odd
// epoch here; one that begins later cannot see the entry.
void RootAudioMixer::WaitForMixToRelease() {
  const uint64_t epoch = mix_epoch_.load(std::memory_order_seq_cst);
  if ((epoch & 1) == 0) {
    return;
  }
  while (mix_epoch_.load(std::memory_order_acquire) == epoch) {
    std::this_thread::yield();
  }
}

void RootAudioMixer::ResetGapClock() {
  gap_reset_.store(true, std::memory_order_release);
}

void RootAudioMixer::Accumulate(const webrtc::AudioFrame& frame,
                                float gain,
                                size_t mix_channels) {
  const int16_t* data = frame.data();
  const size_t source_channels = frame.num_channels();
  if (mix_channels == 1) {
    if (source_channels == 1) {
      for (size_t k = 0; k < kSamplesPerChannel; ++k) {
        accumulator_[k] += gain * data[k];
      }
    } else {
      const float half = 0.5f * gain;
      for (size_t k = 0; k < kSamplesPerChannel; ++k) {
        accumulator_[k] += half * (static_cast<float>(data[2 * k]) +
                                   static_cast<float>(data[2 * k + 1]));
      }
    }
    return;
  }
  if (source_channels == 1) {
    for (size_t k = 0; k < kSamplesPerChannel; ++k) {
      const float value = gain * data[k];
      accumulator_[2 * k] += value;
      accumulator_[2 * k + 1] += value;
    }
  } else {
    for (size_t k = 0; k < 2 * kSamplesPerChannel; ++k) {
      accumulator_[k] += gain * data[k];
    }
  }
}

void RootAudioMixer::Mix(size_t number_of_channels,
                         webrtc::AudioFrame* audio_frame_for_mixing) {
  const int64_t now = NowNs();
  if (gap_reset_.exchange(false, std::memory_order_acquire)) {
    last_mix_ns_ = 0;
  }
  if (last_mix_ns_ != 0 && now > last_mix_ns_) {
    UpdateMax(max_gap_ns_, static_cast<uint64_t>(now - last_mix_ns_));
  }
  last_mix_ns_ = now;

  const size_t out_channels =
      std::clamp<size_t>(number_of_channels, 1, kMaxOutputChannels);
  const size_t mix_channels = out_channels == 1 ? 1 : 2;
  std::fill(accumulator_.begin(),
            accumulator_.begin() + mix_channels * kSamplesPerChannel, 0.0f);

  webrtc::AudioFrame* out = audio_frame_for_mixing;
  bool mixed_any = false;
  uint32_t timestamp = 0;
  int64_t elapsed_time_ms = -1;
  int64_t ntp_time_ms = -1;

  mix_epoch_.fetch_add(1, std::memory_order_seq_cst);
  const size_t limit = slot_limit_.load(std::memory_order_acquire);
  for (size_t i = 0; i < limit; ++i) {
    Entry* entry = slots_[i].load(std::memory_order_seq_cst);
    if (entry == nullptr) {
      continue;
    }
    const auto info =
        entry->source->GetAudioFrameWithInfo(kSampleRateHz, &entry->frame);
    if (info != Source::AudioFrameInfo::kNormal) {
      continue;
    }
    webrtc::AudioFrame& frame = entry->frame;
    if (frame.sample_rate_hz() != kSampleRateHz ||
        frame.samples_per_channel() != kSamplesPerChannel ||
        frame.num_channels() < 1 || frame.num_channels() > 2) {
      continue;
    }
    Accumulate(frame, entry->gain, mix_channels);
    if (!mixed_any) {
      timestamp = frame.timestamp_;
      elapsed_time_ms = frame.elapsed_time_ms_;
      ntp_time_ms = frame.ntp_time_ms_;
      // A swap, so no reference count drops to zero (and frees) here; the
      // source overwrites its frame on the next pull anyway.
      std::swap(out->packet_infos_, frame.packet_infos_);
      mixed_any = true;
    } else {
      timestamp = std::min(timestamp, frame.timestamp_);
      ntp_time_ms = std::min(ntp_time_ms, frame.ntp_time_ms_);
      elapsed_time_ms = std::max(elapsed_time_ms, frame.elapsed_time_ms_);
    }
  }
  mix_epoch_.fetch_add(1, std::memory_order_release);

  out->UpdateFrame(timestamp, nullptr, kSamplesPerChannel, kSampleRateHz,
                   webrtc::AudioFrame::kUndefined,
                   webrtc::AudioFrame::kVadUnknown, out_channels);
  out->elapsed_time_ms_ = elapsed_time_ms;
  out->ntp_time_ms_ = ntp_time_ms;

  const uint64_t samples = kSamplesPerChannel * out_channels;
  double sumsq = 0;
  if (mixed_any) {
    int16_t* dst = out->mutable_data();
    const float gain = master_gain_ / kS16Scale;
    for (size_t k = 0; k < kSamplesPerChannel; ++k) {
      for (size_t c = 0; c < out_channels; ++c) {
        int16_t value = 0;
        if (c < mix_channels) {
          value = ToS16(
              RootSoftClip(accumulator_[k * mix_channels + c] * gain) *
              kS16Scale);
        }
        dst[k * out_channels + c] = value;
        sumsq += static_cast<double>(value) * value;
      }
    }
  }

  out_sumsq_.store(out_sumsq_.load(std::memory_order_relaxed) + sumsq,
                   std::memory_order_relaxed);
  out_samples_.fetch_add(samples, std::memory_order_relaxed);
  mix_calls_.fetch_add(1, std::memory_order_relaxed);
}

RootAudioMixerStats RootAudioMixer::ReadStats() {
  RootAudioMixerStats stats;
  stats.mix_calls = mix_calls_.load(std::memory_order_relaxed);
  stats.max_gap_us = max_gap_ns_.exchange(0, std::memory_order_relaxed) / 1000;
  stats.out_sumsq = out_sumsq_.load(std::memory_order_relaxed);
  stats.out_samples = out_samples_.load(std::memory_order_relaxed);
  const size_t limit = slot_limit_.load(std::memory_order_acquire);
  for (size_t i = 0; i < limit; ++i) {
    if (slots_[i].load(std::memory_order_acquire) != nullptr) {
      stats.ssrcs.push_back(slot_ssrcs_[i].load(std::memory_order_relaxed));
    }
  }
  stats.sources = static_cast<uint32_t>(stats.ssrcs.size());
  return stats;
}

float root_audio_soft_clip(float sample) {
  return RootSoftClip(sample);
}

namespace {

class TestSource : public webrtc::AudioMixer::Source {
 public:
  explicit TestSource(const RootMixTestSource& spec) : spec_(spec) {}

  AudioFrameInfo GetAudioFrameWithInfo(int sample_rate_hz,
                                       webrtc::AudioFrame* frame) override {
    const size_t channels = spec_.channels;
    const size_t samples = static_cast<size_t>(sample_rate_hz / 100);
    frame->UpdateFrame(spec_.timestamp, nullptr, samples, sample_rate_hz,
                       webrtc::AudioFrame::kNormalSpeech,
                       webrtc::AudioFrame::kVadUnknown, channels);
    int16_t* data = frame->mutable_data();
    for (size_t k = 0; k < samples; ++k) {
      data[k * channels] = spec_.left;
      if (channels > 1) {
        data[k * channels + 1] = spec_.right;
      }
    }
    frame->elapsed_time_ms_ = spec_.elapsed_time_ms;
    frame->ntp_time_ms_ = spec_.ntp_time_ms;
    switch (spec_.info) {
      case 1:
        return AudioFrameInfo::kMuted;
      case 2:
        return AudioFrameInfo::kError;
      default:
        return AudioFrameInfo::kNormal;
    }
  }

  int Ssrc() const override { return static_cast<int>(spec_.ssrc); }
  int PreferredSampleRate() const override { return spec_.preferred_rate; }

 private:
  const RootMixTestSource spec_;
};

}  // namespace

RootMixTestResult root_audio_mixer_run_for_test(
    rust::Slice<const RootMixTestSource> sources,
    rust::Slice<const size_t> remove,
    size_t out_channels) {
  auto mixer = webrtc::make_ref_counted<RootAudioMixer>();
  std::vector<std::unique_ptr<TestSource>> owned;
  for (const auto& spec : sources) {
    owned.push_back(std::make_unique<TestSource>(spec));
    mixer->AddSource(owned.back().get());
  }
  for (size_t index : remove) {
    if (index < owned.size()) {
      mixer->RemoveSource(owned[index].get());
    }
  }

  webrtc::AudioFrame frame;
  mixer->Mix(out_channels, &frame);
  RootAudioMixerStats stats = mixer->ReadStats();

  RootMixTestResult result{};
  result.sample_rate = frame.sample_rate_hz();
  result.samples_per_channel = frame.samples_per_channel();
  result.num_channels = frame.num_channels();
  result.timestamp = frame.timestamp_;
  result.elapsed_time_ms = frame.elapsed_time_ms_;
  result.ntp_time_ms = frame.ntp_time_ms_;
  result.muted = frame.muted();
  const int16_t* data = frame.data();
  for (size_t i = 0; i < frame.samples_per_channel() * frame.num_channels();
       ++i) {
    result.data.push_back(data[i]);
  }
  result.sources = stats.sources;
  for (uint32_t ssrc : stats.ssrcs) {
    result.ssrcs.push_back(ssrc);
  }
  result.mix_calls = stats.mix_calls;
  result.out_samples = stats.out_samples;
  result.out_sumsq = stats.out_sumsq;

  for (auto& source : owned) {
    mixer->RemoveSource(source.get());
  }
  return result;
}

}  // namespace livekit_ffi
