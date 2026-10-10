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
#include <map>
#include <memory>
#include <mutex>
#include <unordered_map>
#include <vector>

#include "api/audio/audio_frame.h"
#include "api/audio/audio_mixer.h"
#include "rtc_base/synchronization/mutex.h"
#include "rust/cxx.h"

namespace livekit_ffi {
struct RootMixTestSource;
struct RootMixTestResult;
struct RootMixFrame;
struct RootMixStress;
class RootMixerHarness;
}  // namespace livekit_ffi
#include "webrtc-sys/src/root_audio_mixer.rs.h"

namespace livekit_ffi {

// Port of the app's SoftClipper: linear up to 0.8 of full scale, then a tanh
// knee into the remaining headroom. Works on samples normalized to [-1, 1].
float RootSoftClip(float sample);

// Port of the app's VolumeCurve: slider volumes are linear 0..2 and the applied
// gain is v^2.5, capped at 10. NaN and non-positive volumes are silent.
constexpr float kRootMaxVolume = 2.0f;
constexpr float kRootMaxGain = 10.0f;
float RootClampVolume(float volume);
float RootVolumeToGain(float linear_volume);

enum class RootEffectPlay : uint8_t {
  kQueued = 0,
  kUnknownClip = 1,
  kDropped = 2,
  kIdle = 3,
};

struct RootAudioMixerStats {
  uint64_t mix_calls = 0;
  uint64_t max_gap_us = 0;
  uint32_t sources = 0;
  std::vector<uint32_t> ssrcs;
  double out_sumsq = 0;
  uint64_t out_samples = 0;
  uint64_t overlapping_mixes = 0;
  uint64_t effects_played = 0;
  uint64_t effects_dropped = 0;
};

// The factory's playout mixer: asks every source for 48 kHz, mixes in float with
// no limiter, soft clips and writes int16 at the requested channel count. Its
// output is what the ADM plays and what the echo canceller gets as reference.
//
// While platform audio is in use (SetPlatformActive), each remote source plays
// at RootVolumeToGain(tile x master), 0 when deafened or paused, with 20 ms
// linear ramps, and is soft clipped on its own like the app's mixer; the effect
// bus joins after the remote sources, ignoring master volume and deafen, before
// the final soft clip. Otherwise every gain is unity and the bus is idle.
//
// Once the host has set a source gain (since the last ResetControls), a source
// that has none yet is held at 0 for its first kUnconfiguredHoldMs, so a stream
// that starts before its gain arrives never plays at the wrong volume; one that
// gets no gain by then plays at tile volume 1.
//
// AddSource/RemoveSource run on the worker while Mix runs on the device's render
// thread. Mix takes no lock: sources live in a fixed slot array, and
// RemoveSource only returns once no Mix that may still hold the source runs.
// One Mix runs at a time; a second one that starts meanwhile (a render thread
// the ADM failed to stop) returns silence without touching the sources.
//
// Control calls (gains, master, deafen, effects) come from any thread but the
// render thread; they store atomics or take control_mutex_/effects_mutex_,
// which Mix never takes. Per-source gains are keyed by Source::Ssrc() and kept
// for sources that have not been added yet.
class RootAudioMixer : public webrtc::AudioMixer {
 public:
  static constexpr int kSampleRateHz = 48000;
  static constexpr size_t kSamplesPerChannel = kSampleRateHz / 100;
  static constexpr size_t kMaxSources = 256;
  static constexpr size_t kMaxOutputChannels = 8;
  static constexpr size_t kRampSamples = kSampleRateHz / 50;
  static constexpr size_t kEffectSlots = 32;
  static constexpr size_t kMaxVoices = 32;
  static constexpr size_t kMaxGainSettings = 1024;
  static constexpr size_t kMaxClipSamples = static_cast<size_t>(kSampleRateHz) * 60;
  static constexpr size_t kMaxClipBytesTotal = 256u * 1024u * 1024u;
  static constexpr float kMaxEffectGain = 2.0f;
  static constexpr int64_t kUnconfiguredHoldMs = 500;
  // An effect that waited this long for the render thread (a stalled or
  // still-opening device) is dropped instead of played late.
  static constexpr int64_t kMaxEffectWaitMs = 1000;

  RootAudioMixer();
  ~RootAudioMixer() override;

  bool AddSource(Source* source) override;
  void RemoveSource(Source* source) override;
  void Mix(size_t number_of_channels,
           webrtc::AudioFrame* audio_frame_for_mixing) override;

  // Called before a device starts pulling, so the idle time before it does not
  // count as a gap between Mix calls.
  void ResetGapClock();

  // A platform audio reference exists (AdmProxy's ref count is above zero).
  // Effects queued before a deactivation never play.
  void SetPlatformActive(bool active);
  bool platform_active() const;

  // Linear slider values, clamped to 0..2 (NaN is 0).
  void SetMasterVolume(float volume);
  void SetDeafened(bool deafened);
  float master_volume() const;
  bool deafened() const;
  // Returns false when the table of gains is full; the source then plays as
  // one without a gain.
  bool SetSourceGain(uint32_t ssrc, float tile_volume, bool paused);
  // Forgets the SSRC's gain. A stream with that SSRC that is still playing
  // keeps its last gain; one added later has none.
  void ClearSourceGain(uint32_t ssrc);
  // Counts every source added, so a poller can tell a new stream (or a
  // stream's new SSRC) appeared.
  uint64_t sources_added() const;

  // The clip (48 kHz mono int16) is copied and kept until the mixer is
  // destroyed, so the render thread never frees one. Registering an id again
  // with other samples points the id at a new copy.
  bool RegisterEffectClip(uint32_t id, const int16_t* samples, size_t count);
  RootEffectPlay PlayEffect(uint32_t id, float gain);
  // Cuts playing and queued effects.
  void StopEffects();

  // Back to the defaults: master 1, not deafened, no per-source gains (and no
  // hold of sources without one), no effects playing. Registered clips are
  // kept.
  void ResetControls();

  // Atomics only. Max gap is reset on read.
  RootAudioMixerStats ReadStats();

  // Tests only: shorter hold and effect wait, so they run in real time.
  void SetTimeoutsForTest(int64_t unconfigured_hold_ms,
                          int64_t max_effect_wait_ms);

 private:
  struct GainSetting {
    float tile = 1.0f;
    bool paused = false;
  };

  struct Entry {
    Entry(Source* source, uint32_t ssrc, int64_t added_ns)
        : source(source), ssrc(ssrc), added_ns(added_ns) {}
    Source* const source;
    const uint32_t ssrc;
    const int64_t added_ns;
    std::atomic<float> tile{1.0f};
    std::atomic<bool> paused{false};
    // Set (release) after tile and paused when the host gave this source a
    // gain.
    std::atomic<bool> configured{false};
    // Render thread only.
    bool ramp_started = false;
    float gain = 1.0f;
    float target = 1.0f;
    float step = 0.0f;
    size_t remaining = 0;
    webrtc::AudioFrame frame;
  };

  struct Clip {
    std::vector<int16_t> samples;
  };

  struct EffectRequest {
    const Clip* clip = nullptr;
    float gain = 0.0f;
    uint64_t generation = 0;
    int64_t queued_ns = 0;
  };

  struct Voice {
    const Clip* clip = nullptr;
    size_t position = 0;
    float gain = 0.0f;
  };

  // Fills gains_ with the gain of each sample of this 10 ms, ramping toward
  // `target` as the app's GainRamp does. Returns false when all are 0.
  bool RampGains(Entry& entry, float target);
  void Accumulate(const webrtc::AudioFrame& frame, size_t mix_channels);
  bool MixEffects(bool active, size_t mix_channels, int64_t now_ns);
  void WaitForMixToRelease();

  webrtc::Mutex control_mutex_;
  std::unordered_map<uint32_t, GainSetting> gain_settings_;
  std::array<std::atomic<Entry*>, kMaxSources> slots_{};
  std::array<std::atomic<uint32_t>, kMaxSources> slot_ssrcs_{};
  std::atomic<size_t> slot_limit_{0};
  std::atomic<uint64_t> mix_epoch_{0};
  std::atomic<uint32_t> source_count_{0};
  std::atomic<uint64_t> sources_added_{0};
  std::atomic<bool> in_mix_{false};

  std::atomic<bool> platform_active_{false};
  std::atomic<float> master_volume_{1.0f};
  std::atomic<bool> deafened_{false};
  // The host sets source gains, so sources without one are held for a while.
  std::atomic<bool> gains_used_{false};
  std::atomic<int64_t> unconfigured_hold_ns_{kUnconfiguredHoldMs * 1000000};
  std::atomic<int64_t> max_effect_wait_ns_{kMaxEffectWaitMs * 1000000};

  // Effect bus: clips and the producer side of the ring under effects_mutex_
  // (which also orders PlayEffect against SetPlatformActive); the render
  // thread only reads the ring and owns the voices.
  std::mutex effects_mutex_;
  std::map<uint32_t, const Clip*> clips_;
  std::vector<std::unique_ptr<Clip>> owned_clips_;
  size_t owned_clip_bytes_ = 0;
  std::array<EffectRequest, kEffectSlots> effect_ring_{};
  std::atomic<uint64_t> effect_head_{0};
  std::atomic<uint64_t> effect_tail_{0};
  std::atomic<uint64_t> effect_generation_{0};
  std::array<Voice, kMaxVoices> voices_{};
  size_t voice_count_ = 0;
  uint64_t rendered_generation_ = 0;

  std::array<float, kSamplesPerChannel * 2> accumulator_{};
  std::array<float, kSamplesPerChannel> gains_{};
  int64_t last_mix_ns_ = 0;

  std::atomic<bool> gap_reset_{false};
  std::atomic<uint64_t> mix_calls_{0};
  std::atomic<uint64_t> max_gap_ns_{0};
  std::atomic<double> out_sumsq_{0.0};
  std::atomic<uint64_t> out_samples_{0};
  std::atomic<uint64_t> overlapping_mixes_{0};
  std::atomic<uint64_t> effects_played_{0};
  std::atomic<uint64_t> effects_dropped_{0};
};

static_assert(std::atomic<double>::is_always_lock_free);
static_assert(std::atomic<float>::is_always_lock_free);

float root_audio_soft_clip(float sample);
float root_audio_volume_to_gain(float linear_volume);

RootMixTestResult root_audio_mixer_run_for_test(
    rust::Slice<const RootMixTestSource> sources,
    rust::Slice<const size_t> remove,
    size_t out_channels);

class RootMixerTestSource;

// Drives a RootAudioMixer with constant-valued test sources, one Mix at a time.
class RootMixerHarness {
 public:
  RootMixerHarness();
  ~RootMixerHarness();

  size_t add_source(const RootMixTestSource& spec) const;
  void remove_source(size_t index) const;
  void set_source_value(size_t index, int16_t left, int16_t right) const;
  void set_platform_active(bool active) const;
  void set_master_volume(float volume) const;
  void set_deafened(bool deafened) const;
  bool set_source_gain(uint32_t ssrc, float tile_volume, bool paused) const;
  void clear_source_gain(uint32_t ssrc) const;
  bool register_effect(uint32_t id, rust::Slice<const int16_t> samples) const;
  uint8_t play_effect(uint32_t id, float gain) const;
  void stop_effects() const;
  void reset_controls() const;
  RootMixFrame mix(size_t out_channels) const;
  uint64_t effects_played() const;
  uint64_t effects_dropped() const;
  uint64_t sources_added() const;
  void set_timeouts(int64_t unconfigured_hold_ms,
                    int64_t max_effect_wait_ms) const;

 private:
  webrtc::scoped_refptr<RootAudioMixer> mixer_;
  std::unique_ptr<std::vector<std::unique_ptr<RootMixerTestSource>>> sources_;
};

std::unique_ptr<RootMixerHarness> new_root_mixer_harness();

// Mixes `mixes` times while other threads change gains, master, deafen and the
// platform reference, play and stop effects, and add and remove sources.
RootMixStress root_audio_mixer_stress_for_test(uint32_t mixes);

}  // namespace livekit_ffi
