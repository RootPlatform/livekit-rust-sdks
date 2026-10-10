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
constexpr float kInvS16Scale = 1.0f / kS16Scale;
constexpr float kVolumeExponent = 2.5f;

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

// One source's contribution in int16 units, soft clipped on its own as the
// app's mixer does with every input.
float Contribution(float gain, float sample) {
  return RootSoftClip(gain * sample * kInvS16Scale) * kS16Scale;
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

float RootClampVolume(float volume) {
  if (std::isnan(volume)) {
    return 0.0f;
  }
  return std::min(kRootMaxVolume, std::max(0.0f, volume));
}

float RootVolumeToGain(float linear_volume) {
  if (!(linear_volume > 0.0f)) {
    return 0.0f;
  }
  return std::min(std::pow(linear_volume, kVolumeExponent), kRootMaxGain);
}

RootAudioMixer::RootAudioMixer() = default;

RootAudioMixer::~RootAudioMixer() {
  for (auto& slot : slots_) {
    delete slot.exchange(nullptr);
  }
}

bool RootAudioMixer::AddSource(Source* source) {
  const uint32_t ssrc = static_cast<uint32_t>(source->Ssrc());
  auto entry = std::make_unique<Entry>(source, ssrc, NowNs());
  webrtc::MutexLock lock(&control_mutex_);
  const auto setting = gain_settings_.find(ssrc);
  if (setting != gain_settings_.end()) {
    entry->tile.store(setting->second.tile, std::memory_order_relaxed);
    entry->paused.store(setting->second.paused, std::memory_order_relaxed);
    entry->configured.store(true, std::memory_order_relaxed);
  }
  for (size_t i = 0; i < kMaxSources; ++i) {
    if (slots_[i].load(std::memory_order_relaxed) != nullptr) {
      continue;
    }
    slot_ssrcs_[i].store(ssrc, std::memory_order_relaxed);
    slot_sumsq_[i].store(0.0, std::memory_order_relaxed);
    slot_samples_[i].store(0, std::memory_order_relaxed);
    slot_ids_[i].store(sources_added_.load(std::memory_order_relaxed) + 1,
                       std::memory_order_release);
    slots_[i].store(entry.release(), std::memory_order_seq_cst);
    if (i + 1 > slot_limit_.load(std::memory_order_relaxed)) {
      slot_limit_.store(i + 1, std::memory_order_release);
    }
    sources_added_.fetch_add(1, std::memory_order_release);
    const uint32_t count =
        source_count_.fetch_add(1, std::memory_order_relaxed) + 1;
    RTC_LOG(LS_INFO) << "RootAudioMixer: added source ssrc=" << ssrc << " ("
                     << count << " sources)";
    return true;
  }
  RTC_LOG(LS_ERROR) << "RootAudioMixer: no free slot for source ssrc=" << ssrc;
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

// Under effects_mutex_, so a PlayEffect either sees the deactivation or queues
// before it and is cut by the generation bump.
void RootAudioMixer::SetPlatformActive(bool active) {
  std::lock_guard<std::mutex> lock(effects_mutex_);
  if (!active && platform_active_.load(std::memory_order_relaxed)) {
    effect_generation_.fetch_add(1, std::memory_order_release);
  }
  platform_active_.store(active, std::memory_order_release);
}

bool RootAudioMixer::platform_active() const {
  return platform_active_.load(std::memory_order_acquire);
}

void RootAudioMixer::SetMasterVolume(float volume) {
  master_volume_.store(RootClampVolume(volume), std::memory_order_relaxed);
}

void RootAudioMixer::SetDeafened(bool deafened) {
  deafened_.store(deafened, std::memory_order_relaxed);
}

float RootAudioMixer::master_volume() const {
  return master_volume_.load(std::memory_order_relaxed);
}

bool RootAudioMixer::deafened() const {
  return deafened_.load(std::memory_order_relaxed);
}

bool RootAudioMixer::SetSourceGain(uint32_t ssrc,
                                   float tile_volume,
                                   bool paused) {
  const float tile = RootClampVolume(tile_volume);
  webrtc::MutexLock lock(&control_mutex_);
  auto setting = gain_settings_.find(ssrc);
  if (setting == gain_settings_.end()) {
    if (gain_settings_.size() >= kMaxGainSettings) {
      return false;
    }
    setting = gain_settings_.emplace(ssrc, GainSetting{}).first;
  }
  setting->second.tile = tile;
  setting->second.paused = paused;
  gains_used_.store(true, std::memory_order_relaxed);
  const size_t limit = slot_limit_.load(std::memory_order_relaxed);
  for (size_t i = 0; i < limit; ++i) {
    Entry* entry = slots_[i].load(std::memory_order_relaxed);
    if (entry != nullptr && entry->ssrc == ssrc) {
      entry->tile.store(tile, std::memory_order_relaxed);
      entry->paused.store(paused, std::memory_order_relaxed);
      entry->configured.store(true, std::memory_order_release);
    }
  }
  return true;
}

// The live entry keeps its gain: a dropped track's stream that still plays (a
// room being left) must not come back at full volume.
void RootAudioMixer::ClearSourceGain(uint32_t ssrc) {
  webrtc::MutexLock lock(&control_mutex_);
  gain_settings_.erase(ssrc);
}

uint64_t RootAudioMixer::sources_added() const {
  return sources_added_.load(std::memory_order_acquire);
}

void RootAudioMixer::SetTimeoutsForTest(int64_t unconfigured_hold_ms,
                                        int64_t max_effect_wait_ms) {
  unconfigured_hold_ns_.store(unconfigured_hold_ms * 1000000,
                              std::memory_order_relaxed);
  max_effect_wait_ns_.store(max_effect_wait_ms * 1000000,
                            std::memory_order_relaxed);
}

bool RootAudioMixer::RegisterEffectClip(uint32_t id,
                                        const int16_t* samples,
                                        size_t count) {
  if (samples == nullptr || count == 0 || count > kMaxClipSamples) {
    return false;
  }
  std::lock_guard<std::mutex> lock(effects_mutex_);
  const auto existing = clips_.find(id);
  if (existing != clips_.end() &&
      existing->second->samples.size() == count &&
      std::equal(samples, samples + count, existing->second->samples.begin())) {
    return true;
  }
  const size_t bytes = count * sizeof(int16_t);
  if (owned_clip_bytes_ + bytes > kMaxClipBytesTotal) {
    return false;
  }
  auto clip = std::make_unique<Clip>();
  clip->samples.assign(samples, samples + count);
  clips_[id] = clip.get();
  owned_clips_.push_back(std::move(clip));
  owned_clip_bytes_ += bytes;
  return true;
}

RootEffectPlay RootAudioMixer::PlayEffect(uint32_t id, float gain) {
  if (std::isnan(gain)) {
    gain = 0.0f;
  }
  gain = std::min(kMaxEffectGain, std::max(0.0f, gain));
  std::lock_guard<std::mutex> lock(effects_mutex_);
  const auto clip = clips_.find(id);
  if (clip == clips_.end()) {
    return RootEffectPlay::kUnknownClip;
  }
  if (!platform_active_.load(std::memory_order_acquire)) {
    effects_dropped_.fetch_add(1, std::memory_order_relaxed);
    return RootEffectPlay::kIdle;
  }
  const uint64_t head = effect_head_.load(std::memory_order_relaxed);
  const uint64_t tail = effect_tail_.load(std::memory_order_acquire);
  if (head - tail >= kEffectSlots) {
    effects_dropped_.fetch_add(1, std::memory_order_relaxed);
    return RootEffectPlay::kDropped;
  }
  effect_ring_[head % kEffectSlots] = EffectRequest{
      clip->second, gain,
      effect_generation_.load(std::memory_order_relaxed), NowNs()};
  effect_head_.store(head + 1, std::memory_order_release);
  effects_played_.fetch_add(1, std::memory_order_relaxed);
  return RootEffectPlay::kQueued;
}

void RootAudioMixer::StopEffects() {
  std::lock_guard<std::mutex> lock(effects_mutex_);
  effect_generation_.fetch_add(1, std::memory_order_release);
}

void RootAudioMixer::ResetControls() {
  SetMasterVolume(1.0f);
  SetDeafened(false);
  {
    webrtc::MutexLock lock(&control_mutex_);
    gain_settings_.clear();
    gains_used_.store(false, std::memory_order_relaxed);
    const size_t limit = slot_limit_.load(std::memory_order_relaxed);
    for (size_t i = 0; i < limit; ++i) {
      Entry* entry = slots_[i].load(std::memory_order_relaxed);
      if (entry != nullptr) {
        entry->configured.store(false, std::memory_order_relaxed);
        entry->tile.store(1.0f, std::memory_order_relaxed);
        entry->paused.store(false, std::memory_order_relaxed);
      }
    }
  }
  StopEffects();
}

bool RootAudioMixer::RampGains(Entry& entry, float target) {
  if (!entry.ramp_started) {
    entry.ramp_started = true;
    entry.gain = target;
    entry.target = target;
    entry.remaining = 0;
  } else if (target != entry.target) {
    entry.target = target;
    entry.remaining = kRampSamples;
    entry.step = (target - entry.gain) / static_cast<float>(kRampSamples);
  }
  bool audible = false;
  for (size_t k = 0; k < kSamplesPerChannel; ++k) {
    if (entry.remaining > 0) {
      entry.gain += entry.step;
      if (--entry.remaining == 0) {
        entry.gain = entry.target;
      }
    }
    gains_[k] = entry.gain;
    audible |= entry.gain != 0.0f;
  }
  return audible;
}

void RootAudioMixer::Accumulate(const webrtc::AudioFrame& frame,
                                size_t mix_channels) {
  const int16_t* data = frame.data();
  const size_t source_channels = frame.num_channels();
  if (mix_channels == 1) {
    if (source_channels == 1) {
      for (size_t k = 0; k < kSamplesPerChannel; ++k) {
        accumulator_[k] += Contribution(gains_[k], data[k]);
      }
    } else {
      for (size_t k = 0; k < kSamplesPerChannel; ++k) {
        const float average = 0.5f * (static_cast<float>(data[2 * k]) +
                                      static_cast<float>(data[2 * k + 1]));
        accumulator_[k] += Contribution(gains_[k], average);
      }
    }
    return;
  }
  if (source_channels == 1) {
    for (size_t k = 0; k < kSamplesPerChannel; ++k) {
      const float value = Contribution(gains_[k], data[k]);
      accumulator_[2 * k] += value;
      accumulator_[2 * k + 1] += value;
    }
  } else {
    for (size_t k = 0; k < kSamplesPerChannel; ++k) {
      accumulator_[2 * k] += Contribution(gains_[k], data[2 * k]);
      accumulator_[2 * k + 1] += Contribution(gains_[k], data[2 * k + 1]);
    }
  }
}

// Render thread. Takes the queued effects of the current generation, drops
// voices of an older one and requests that waited too long, and adds every
// voice's next 10 ms.
bool RootAudioMixer::MixEffects(bool active,
                                size_t mix_channels,
                                int64_t now_ns) {
  const uint64_t generation =
      effect_generation_.load(std::memory_order_acquire);
  if (!active || generation != rendered_generation_) {
    rendered_generation_ = generation;
    voice_count_ = 0;
  }
  const int64_t max_wait_ns =
      max_effect_wait_ns_.load(std::memory_order_relaxed);
  uint64_t tail = effect_tail_.load(std::memory_order_relaxed);
  const uint64_t head = effect_head_.load(std::memory_order_acquire);
  for (; tail != head; ++tail) {
    const EffectRequest& request = effect_ring_[tail % kEffectSlots];
    if (!active || request.generation != generation) {
      continue;
    }
    if (voice_count_ == kMaxVoices ||
        now_ns - request.queued_ns > max_wait_ns) {
      effects_dropped_.fetch_add(1, std::memory_order_relaxed);
      continue;
    }
    voices_[voice_count_++] = Voice{request.clip, 0, request.gain};
  }
  effect_tail_.store(tail, std::memory_order_release);

  bool mixed = false;
  for (size_t v = 0; v < voice_count_;) {
    Voice& voice = voices_[v];
    const std::vector<int16_t>& samples = voice.clip->samples;
    const size_t count =
        std::min(kSamplesPerChannel, samples.size() - voice.position);
    const int16_t* source = samples.data() + voice.position;
    for (size_t k = 0; k < count; ++k) {
      const float value = voice.gain * static_cast<float>(source[k]);
      for (size_t c = 0; c < mix_channels; ++c) {
        accumulator_[k * mix_channels + c] += value;
      }
    }
    mixed |= count > 0;
    voice.position += count;
    if (voice.position >= samples.size()) {
      voices_[v] = voices_[--voice_count_];
    } else {
      ++v;
    }
  }
  return mixed;
}

void RootAudioMixer::Mix(size_t number_of_channels,
                         webrtc::AudioFrame* audio_frame_for_mixing) {
  const size_t out_channels =
      std::clamp<size_t>(number_of_channels, 1, kMaxOutputChannels);
  // The epoch rule below and the accumulator need a single mixer at a time.
  if (in_mix_.exchange(true, std::memory_order_acquire)) {
    overlapping_mixes_.fetch_add(1, std::memory_order_relaxed);
    audio_frame_for_mixing->UpdateFrame(
        0, nullptr, kSamplesPerChannel, kSampleRateHz,
        webrtc::AudioFrame::kUndefined, webrtc::AudioFrame::kVadUnknown,
        out_channels);
    return;
  }

  const int64_t now = NowNs();
  if (gap_reset_.exchange(false, std::memory_order_acquire)) {
    last_mix_ns_ = 0;
  }
  if (last_mix_ns_ != 0 && now > last_mix_ns_) {
    UpdateMax(max_gap_ns_, static_cast<uint64_t>(now - last_mix_ns_));
  }
  last_mix_ns_ = now;

  const size_t mix_channels = out_channels == 1 ? 1 : 2;
  std::fill(accumulator_.begin(),
            accumulator_.begin() + mix_channels * kSamplesPerChannel, 0.0f);

  const bool active = platform_active_.load(std::memory_order_acquire);
  const float master = master_volume_.load(std::memory_order_relaxed);
  const bool deafened = deafened_.load(std::memory_order_relaxed);
  const bool hold_unconfigured = gains_used_.load(std::memory_order_relaxed);
  const int64_t hold_ns =
      unconfigured_hold_ns_.load(std::memory_order_relaxed);

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
    float target = 1.0f;
    if (active) {
      const bool held = hold_unconfigured &&
                        !entry->configured.load(std::memory_order_acquire) &&
                        now - entry->added_ns < hold_ns;
      target = deafened || held ||
                       entry->paused.load(std::memory_order_relaxed)
                   ? 0.0f
                   : RootVolumeToGain(
                         entry->tile.load(std::memory_order_relaxed) * master);
    }
    // The ramp moves on with time even for a source that has nothing to play.
    const bool audible = RampGains(*entry, target);
    const auto info =
        entry->source->GetAudioFrameWithInfo(kSampleRateHz, &entry->frame);
    if (info == Source::AudioFrameInfo::kMuted) {
      CountSource(i, 0.0, kSamplesPerChannel);
      continue;
    }
    if (info != Source::AudioFrameInfo::kNormal) {
      continue;
    }
    webrtc::AudioFrame& frame = entry->frame;
    if (frame.sample_rate_hz() != kSampleRateHz ||
        frame.samples_per_channel() != kSamplesPerChannel ||
        frame.num_channels() < 1 || frame.num_channels() > 2) {
      continue;
    }
    {
      const size_t count = kSamplesPerChannel * frame.num_channels();
      const int16_t* data = frame.data();
      double source_sumsq = 0;
      for (size_t k = 0; k < count; ++k) {
        source_sumsq += static_cast<double>(data[k]) * data[k];
      }
      CountSource(i, source_sumsq, count);
    }
    if (audible) {
      Accumulate(frame, mix_channels);
    }
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

  const bool mixed_effects = MixEffects(active, mix_channels, now);

  out->UpdateFrame(timestamp, nullptr, kSamplesPerChannel, kSampleRateHz,
                   webrtc::AudioFrame::kUndefined,
                   webrtc::AudioFrame::kVadUnknown, out_channels);
  out->elapsed_time_ms_ = elapsed_time_ms;
  out->ntp_time_ms_ = ntp_time_ms;

  const uint64_t samples = kSamplesPerChannel * out_channels;
  double sumsq = 0;
  if (mixed_any || mixed_effects) {
    int16_t* dst = out->mutable_data();
    for (size_t k = 0; k < kSamplesPerChannel; ++k) {
      for (size_t c = 0; c < out_channels; ++c) {
        int16_t value = 0;
        if (c < mix_channels) {
          value = ToS16(
              RootSoftClip(accumulator_[k * mix_channels + c] * kInvS16Scale) *
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
  in_mix_.store(false, std::memory_order_release);
}

RootAudioMixerStats RootAudioMixer::ReadStats() {
  RootAudioMixerStats stats;
  stats.mix_calls = mix_calls_.load(std::memory_order_relaxed);
  stats.max_gap_us = max_gap_ns_.exchange(0, std::memory_order_relaxed) / 1000;
  stats.out_sumsq = out_sumsq_.load(std::memory_order_relaxed);
  stats.out_samples = out_samples_.load(std::memory_order_relaxed);
  stats.overlapping_mixes =
      overlapping_mixes_.load(std::memory_order_relaxed);
  stats.effects_played = effects_played_.load(std::memory_order_relaxed);
  stats.effects_dropped = effects_dropped_.load(std::memory_order_relaxed);
  const size_t limit = slot_limit_.load(std::memory_order_acquire);
  for (size_t i = 0; i < limit; ++i) {
    if (slots_[i].load(std::memory_order_acquire) != nullptr) {
      stats.ssrcs.push_back(slot_ssrcs_[i].load(std::memory_order_relaxed));
    }
  }
  stats.sources = static_cast<uint32_t>(stats.ssrcs.size());
  return stats;
}

// Render thread only, for a slot whose entry it holds.
void RootAudioMixer::CountSource(size_t slot, double sumsq, uint64_t samples) {
  slot_sumsq_[slot].store(
      slot_sumsq_[slot].load(std::memory_order_relaxed) + sumsq,
      std::memory_order_relaxed);
  slot_samples_[slot].fetch_add(samples, std::memory_order_relaxed);
}

std::vector<RootAudioMixerSourceLevel> RootAudioMixer::ReadSourceLevels()
    const {
  std::vector<RootAudioMixerSourceLevel> levels;
  const size_t limit = slot_limit_.load(std::memory_order_acquire);
  for (size_t i = 0; i < limit; ++i) {
    if (slots_[i].load(std::memory_order_acquire) == nullptr) {
      continue;
    }
    const uint64_t id = slot_ids_[i].load(std::memory_order_acquire);
    RootAudioMixerSourceLevel level;
    level.ssrc = slot_ssrcs_[i].load(std::memory_order_relaxed);
    level.source_id = id;
    level.sumsq = slot_sumsq_[i].load(std::memory_order_relaxed);
    level.samples = slot_samples_[i].load(std::memory_order_relaxed);
    if (slot_ids_[i].load(std::memory_order_acquire) != id ||
        slots_[i].load(std::memory_order_acquire) == nullptr) {
      continue;
    }
    levels.push_back(level);
  }
  return levels;
}

float root_audio_soft_clip(float sample) {
  return RootSoftClip(sample);
}

float root_audio_volume_to_gain(float linear_volume) {
  return RootVolumeToGain(linear_volume);
}

class RootMixerTestSource : public webrtc::AudioMixer::Source {
 public:
  RootMixerTestSource(const RootMixTestSource& spec, RootAudioMixer* mixer)
      : spec_(spec), mixer_(mixer) {}

  // Set when this source mixed again from inside the mix (as a second render
  // thread would) and that nested mix came back silent.
  bool nested_mix_silent() const { return nested_mix_silent_; }

  void set_value(int16_t left, int16_t right) {
    spec_.left = left;
    spec_.right = right;
  }

  AudioFrameInfo GetAudioFrameWithInfo(int sample_rate_hz,
                                       webrtc::AudioFrame* frame) override {
    if (spec_.reenter && !reentered_) {
      reentered_ = true;
      webrtc::AudioFrame nested;
      mixer_->Mix(2, &nested);
      nested_mix_silent_ = nested.muted() &&
                           nested.sample_rate_hz() == sample_rate_hz &&
                           nested.num_channels() == 2;
    }
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
  RootMixTestSource spec_;
  RootAudioMixer* const mixer_;
  bool reentered_ = false;
  bool nested_mix_silent_ = false;
};

RootMixTestResult root_audio_mixer_run_for_test(
    rust::Slice<const RootMixTestSource> sources,
    rust::Slice<const size_t> remove,
    size_t out_channels) {
  auto mixer = webrtc::make_ref_counted<RootAudioMixer>();
  std::vector<std::unique_ptr<RootMixerTestSource>> owned;
  for (const auto& spec : sources) {
    owned.push_back(std::make_unique<RootMixerTestSource>(spec, mixer.get()));
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
  result.overlapping_mixes = stats.overlapping_mixes;
  for (const auto& source : owned) {
    result.nested_mix_silent |= source->nested_mix_silent();
  }

  for (auto& source : owned) {
    mixer->RemoveSource(source.get());
  }
  return result;
}

RootMixerHarness::RootMixerHarness()
    : mixer_(webrtc::make_ref_counted<RootAudioMixer>()),
      sources_(std::make_unique<
               std::vector<std::unique_ptr<RootMixerTestSource>>>()) {}

RootMixerHarness::~RootMixerHarness() {
  for (auto& source : *sources_) {
    if (source) {
      mixer_->RemoveSource(source.get());
    }
  }
}

size_t RootMixerHarness::add_source(const RootMixTestSource& spec) const {
  sources_->push_back(std::make_unique<RootMixerTestSource>(spec, mixer_.get()));
  mixer_->AddSource(sources_->back().get());
  return sources_->size() - 1;
}

void RootMixerHarness::remove_source(size_t index) const {
  if (index < sources_->size() && (*sources_)[index]) {
    mixer_->RemoveSource((*sources_)[index].get());
    (*sources_)[index] = nullptr;
  }
}

void RootMixerHarness::set_source_value(size_t index,
                                        int16_t left,
                                        int16_t right) const {
  if (index < sources_->size() && (*sources_)[index]) {
    (*sources_)[index]->set_value(left, right);
  }
}

void RootMixerHarness::set_platform_active(bool active) const {
  mixer_->SetPlatformActive(active);
}

void RootMixerHarness::set_master_volume(float volume) const {
  mixer_->SetMasterVolume(volume);
}

void RootMixerHarness::set_deafened(bool deafened) const {
  mixer_->SetDeafened(deafened);
}

bool RootMixerHarness::set_source_gain(uint32_t ssrc,
                                       float tile_volume,
                                       bool paused) const {
  return mixer_->SetSourceGain(ssrc, tile_volume, paused);
}

void RootMixerHarness::clear_source_gain(uint32_t ssrc) const {
  mixer_->ClearSourceGain(ssrc);
}

bool RootMixerHarness::register_effect(
    uint32_t id,
    rust::Slice<const int16_t> samples) const {
  return mixer_->RegisterEffectClip(id, samples.data(), samples.size());
}

uint8_t RootMixerHarness::play_effect(uint32_t id, float gain) const {
  return static_cast<uint8_t>(mixer_->PlayEffect(id, gain));
}

void RootMixerHarness::stop_effects() const {
  mixer_->StopEffects();
}

void RootMixerHarness::reset_controls() const {
  mixer_->ResetControls();
}

RootMixFrame RootMixerHarness::mix(size_t out_channels) const {
  webrtc::AudioFrame frame;
  mixer_->Mix(out_channels, &frame);
  RootMixFrame result{};
  result.num_channels = frame.num_channels();
  result.muted = frame.muted();
  const int16_t* data = frame.data();
  for (size_t i = 0; i < frame.samples_per_channel() * frame.num_channels();
       ++i) {
    result.data.push_back(data[i]);
  }
  return result;
}

uint64_t RootMixerHarness::effects_played() const {
  return mixer_->ReadStats().effects_played;
}

uint64_t RootMixerHarness::effects_dropped() const {
  return mixer_->ReadStats().effects_dropped;
}

uint64_t RootMixerHarness::sources_added() const {
  return mixer_->sources_added();
}

rust::Vec<RootMixLevel> RootMixerHarness::source_levels() const {
  rust::Vec<RootMixLevel> result;
  for (const RootAudioMixerSourceLevel& level : mixer_->ReadSourceLevels()) {
    result.push_back(
        RootMixLevel{level.ssrc, level.source_id, level.sumsq, level.samples});
  }
  return result;
}

void RootMixerHarness::set_timeouts(int64_t unconfigured_hold_ms,
                                    int64_t max_effect_wait_ms) const {
  mixer_->SetTimeoutsForTest(unconfigured_hold_ms, max_effect_wait_ms);
}

std::unique_ptr<RootMixerHarness> new_root_mixer_harness() {
  return std::make_unique<RootMixerHarness>();
}

namespace {

RootMixTestSource StressSource(uint32_t ssrc, size_t channels) {
  RootMixTestSource spec{};
  spec.channels = channels;
  spec.left = 1000;
  spec.right = -1000;
  spec.ssrc = ssrc;
  spec.elapsed_time_ms = -1;
  spec.ntp_time_ms = -1;
  spec.preferred_rate = 48000;
  return spec;
}

}  // namespace

RootMixStress root_audio_mixer_stress_for_test(uint32_t mixes) {
  auto mixer = webrtc::make_ref_counted<RootAudioMixer>();
  mixer->SetPlatformActive(true);
  const std::vector<int16_t> clip(4800, 1000);
  mixer->RegisterEffectClip(1, clip.data(), clip.size());
  std::vector<std::unique_ptr<RootMixerTestSource>> fixed;
  for (uint32_t ssrc = 0; ssrc < 4; ++ssrc) {
    fixed.push_back(std::make_unique<RootMixerTestSource>(
        StressSource(ssrc, 1 + ssrc % 2), mixer.get()));
    mixer->AddSource(fixed.back().get());
  }

  std::atomic<bool> stop{false};
  std::thread control([&] {
    for (uint32_t n = 0; !stop.load(); ++n) {
      const uint32_t ssrc = n % 8;
      switch (n % 7) {
        case 0:
          mixer->SetSourceGain(ssrc, static_cast<float>(n % 5) * 0.5f,
                               n % 3 == 0);
          break;
        case 1:
          mixer->ClearSourceGain(ssrc);
          break;
        case 2:
          mixer->SetMasterVolume(static_cast<float>(n % 4) * 0.5f);
          break;
        case 3:
          mixer->SetDeafened(n % 2 == 0);
          break;
        case 4:
          mixer->PlayEffect(1, 0.2f);
          break;
        case 5:
          mixer->StopEffects();
          break;
        default:
          if (n % 50 == 6) {
            mixer->ResetControls();
          }
          break;
      }
    }
  });
  std::thread sources([&] {
    for (uint32_t n = 0; !stop.load(); ++n) {
      RootMixerTestSource source(StressSource(4 + n % 4, 1 + n % 2),
                                 mixer.get());
      mixer->AddSource(&source);
      std::this_thread::yield();
      mixer->RemoveSource(&source);
    }
  });
  std::thread activation([&] {
    for (uint32_t n = 0; !stop.load(); ++n) {
      mixer->SetPlatformActive(n % 2 == 0);
      std::this_thread::yield();
    }
  });

  RootMixStress result{};
  for (uint32_t i = 0; i < mixes; ++i) {
    webrtc::AudioFrame frame;
    mixer->Mix(2, &frame);
    if (frame.samples_per_channel() != RootAudioMixer::kSamplesPerChannel ||
        frame.num_channels() != 2 ||
        frame.sample_rate_hz() != RootAudioMixer::kSampleRateHz) {
      ++result.bad_frames;
    }
  }
  stop.store(true);
  control.join();
  sources.join();
  activation.join();

  const RootAudioMixerStats stats = mixer->ReadStats();
  result.mix_calls = stats.mix_calls;
  result.overlapping_mixes = stats.overlapping_mixes;
  result.sources = stats.sources;
  for (auto& source : fixed) {
    mixer->RemoveSource(source.get());
  }
  return result;
}

}  // namespace livekit_ffi
