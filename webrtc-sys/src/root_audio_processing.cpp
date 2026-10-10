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

#include "livekit/root_audio_processing.h"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdlib>
#include <future>
#include <limits>
#include <thread>
#include <utility>

#include "api/audio/builtin_audio_processing_builder.h"
#include "api/environment/environment_factory.h"
#include "api/make_ref_counted.h"
#include "api/task_queue/task_queue_base.h"
#include "api/units/time_delta.h"
#include "livekit/root_denoiser.h"
#include "modules/audio_processing/audio_buffer.h"
#include "modules/audio_processing/include/aec_dump.h"

namespace livekit_ffi {
namespace {

// Single writer (the audio thread), so a load and a store is enough.
void AddTo(std::atomic<double>& total, double value) {
  total.store(total.load(std::memory_order_relaxed) + value,
              std::memory_order_relaxed);
}

void CountIf(std::atomic<uint64_t>& counter, bool count) {
  if (count) {
    counter.fetch_add(1, std::memory_order_relaxed);
  }
}

// The audio thread raises it, a reader may exchange it; NaN is "none yet".
void RaiseTo(std::atomic<float>& maximum, float value) {
  if (std::isnan(value)) {
    return;
  }
  float current = maximum.load(std::memory_order_relaxed);
  while ((std::isnan(current) || value > current) &&
         !maximum.compare_exchange_weak(current, value,
                                        std::memory_order_relaxed)) {
  }
}

double OrNaN(const std::optional<double>& value) {
  return value.value_or(std::numeric_limits<double>::quiet_NaN());
}

int64_t SteadyNowMs() {
  return std::chrono::duration_cast<std::chrono::milliseconds>(
             std::chrono::steady_clock::now().time_since_epoch())
      .count();
}

int64_t SteadyNowUs() {
  return std::chrono::duration_cast<std::chrono::microseconds>(
             std::chrono::steady_clock::now().time_since_epoch())
      .count();
}

webrtc::scoped_refptr<webrtc::AudioProcessing> BuildRootApm(
    const webrtc::Environment& env,
    std::shared_ptr<RootCaptureState> capture_state) {
  webrtc::AudioProcessing::Config config;
  config.high_pass_filter.enabled = true;
  webrtc::BuiltinAudioProcessingBuilder builder(config);
  builder.SetCapturePostProcessing(
      std::make_unique<RootCaptureProcessor>(std::move(capture_state)));
  return builder.Build(env);
}

}  // namespace

// =============================================================================
// RootCaptureState
// =============================================================================

bool RootCaptureState::GateOpen(uint32_t gate) {
  return (gate & kGateMuted) == 0 &&
         (gate >> kGatePushToTalkShift) !=
             static_cast<uint32_t>(RootPushToTalk::kReleased);
}

bool RootCaptureState::mic_muted() const {
  return (gate.load(std::memory_order_relaxed) & kGateMuted) != 0;
}

uint32_t RootCaptureState::push_to_talk() const {
  return gate.load(std::memory_order_relaxed) >> kGatePushToTalkShift;
}

void RootCaptureState::SetGate(std::optional<bool> muted,
                               std::optional<uint32_t> push_to_talk) {
  uint32_t current = gate.load(std::memory_order_relaxed);
  uint32_t next;
  do {
    next = current;
    if (muted.has_value()) {
      next = *muted ? (next | kGateMuted) : (next & ~kGateMuted);
    }
    if (push_to_talk.has_value()) {
      const uint32_t state = std::min<uint32_t>(
          *push_to_talk, static_cast<uint32_t>(RootPushToTalk::kReleased));
      next = (next & kGateMuted) | (state << kGatePushToTalkShift);
    }
  } while (!gate.compare_exchange_weak(current, next,
                                       std::memory_order_relaxed));
  if (!GateOpen(current) && GateOpen(next)) {
    gate_open_stamp_us.store(std::max<int64_t>(1, SteadyNowUs()),
                             std::memory_order_relaxed);
  }
}

void RootCaptureState::ResetControls(bool close_gate) {
  gate.store(close_gate ? kGateClosed : 0, std::memory_order_relaxed);
  input_volume.store(1.0f, std::memory_order_relaxed);
  denoise_power.store(0.75f, std::memory_order_relaxed);
  auto_gain.store(true, std::memory_order_relaxed);
  gate_threshold.store(0.025f, std::memory_order_relaxed);
  auto_sensitivity.store(true, std::memory_order_relaxed);
  denoiser_wanted.store(false, std::memory_order_relaxed);
  // A press of the last session that no frame saw is no delay of this one.
  gate_open_stamp_us.store(0, std::memory_order_relaxed);
  ClearSpeaking();
  dsp_reset_seq.fetch_add(1, std::memory_order_release);
}

void RootCaptureState::ClearSpeaking() {
  uint64_t word = speaking_word.load(std::memory_order_relaxed);
  while ((word & 1) != 0 &&
         !speaking_word.compare_exchange_weak(word, ((word >> 1) + 1) << 1,
                                              std::memory_order_release,
                                              std::memory_order_relaxed)) {
  }
}

bool RootCaptureState::AudiblySpeaking() const {
  return ReadSpeaking().speaking && !holding.load(std::memory_order_relaxed);
}

RootDspStatsSnapshot RootCaptureState::ReadDspStats(bool take_maxima) {
  constexpr auto kRelaxed = std::memory_order_relaxed;
  RootDspStatsSnapshot s;
  for (int attempt = 0; attempt < 8; ++attempt) {
    const uint64_t before = levels_seq.load(std::memory_order_acquire);
    s.processed_frames = processed_frames.load(kRelaxed);
    s.stage_samples = stage_samples.load(kRelaxed);
    s.apm_out_sumsq = apm_out_sumsq.load(kRelaxed);
    s.denoised_sumsq = denoised_sumsq.load(kRelaxed);
    s.gate_in_sumsq = gate_in_sumsq.load(kRelaxed);
    s.sent_sumsq = sent_sumsq.load(kRelaxed);
    std::atomic_thread_fence(std::memory_order_acquire);
    const uint64_t after = levels_seq.load(kRelaxed);
    if (before == after && (before & 1) == 0) {
      break;
    }
  }
  s.gate_open_frames = gate_open_frames.load(kRelaxed);
  s.gate_input_above_frames = gate_input_above_frames.load(kRelaxed);
  s.startup_hold_frames = startup_hold_frames.load(kRelaxed);
  s.transmit_frames = transmit_frames.load(kRelaxed);
  s.unsupported_frames = unsupported_frames.load(kRelaxed);
  s.non_finite_samples = non_finite_samples.load(kRelaxed);
  s.dsp_resets = dsp_resets.load(kRelaxed);
  s.dsp_flushes = dsp_flushes.load(kRelaxed);
  s.ns_gap_frames = ns_gap_frames.load(kRelaxed);
  const RootSpeakingSnapshot published = ReadSpeaking();
  s.speaking = published.speaking;
  s.speaking_seq = published.seq;
  s.gate_open_delay_us = gate_open_delay_us.load(kRelaxed);
  s.threshold = threshold.load(kRelaxed);
  s.voice_gain_db = voice_gain_db.load(kRelaxed);
  s.voice_gain_settled = voice_gain_settled.load(kRelaxed);
  s.gate_speaking = gate_speaking.load(kRelaxed);
  if (take_maxima) {
    s.gate_in_block_peak_db = gate_in_block_peak_db_max.exchange(
        std::numeric_limits<float>::quiet_NaN(), kRelaxed);
    s.envelope_max = envelope_max.exchange(0.0f, kRelaxed);
  } else {
    s.gate_in_block_peak_db = gate_in_block_peak_db_max.load(kRelaxed);
    s.envelope_max = envelope_max.load(kRelaxed);
  }
  s.aligner_primed = aligner_primed.load(kRelaxed);
  s.aligner_buffered_samples = aligner_buffered_samples.load(kRelaxed);
  s.aligner_underruns = aligner_underruns.load(kRelaxed);
  s.aligner_padded_samples = aligner_padded_samples.load(kRelaxed);
  s.aligner_trimmed_samples = aligner_trimmed_samples.load(kRelaxed);
  s.aligner_drained_samples = aligner_drained_samples.load(kRelaxed);
  s.aligner_latency_ms = aligner_latency_ms.load(kRelaxed);
  return s;
}

RootSpeakingSnapshot RootCaptureState::ReadSpeaking() const {
  const uint64_t word = speaking_word.load(std::memory_order_acquire);
  return RootSpeakingSnapshot{(word & 1) != 0, word >> 1};
}

RootCaptureFormatSnapshot ReadCaptureFormat(const RootCaptureState& state) {
  RootCaptureFormatSnapshot snapshot;
  for (int attempt = 0; attempt < 8; ++attempt) {
    const uint64_t before = state.format_seq.load(std::memory_order_acquire);
    snapshot.sample_rate = state.process_rate.load(std::memory_order_relaxed);
    snapshot.channels = state.process_channels.load(std::memory_order_relaxed);
    std::atomic_thread_fence(std::memory_order_acquire);
    const uint64_t after = state.format_seq.load(std::memory_order_relaxed);
    if (before == after && (before & 1) == 0) {
      snapshot.seq = before / 2;
      return snapshot;
    }
  }
  snapshot.seq = state.format_seq.load(std::memory_order_acquire) / 2;
  return snapshot;
}

// =============================================================================
// RootCaptureProcessor
// =============================================================================

RootCaptureProcessor::RootCaptureProcessor(
    std::shared_ptr<RootCaptureState> state)
    : state_(std::move(state)), dsp_(new_root_dsp_chain(kMaxRate)) {}

void RootCaptureProcessor::Initialize(int sample_rate_hz, int num_channels) {
  state_->init_rate.store(sample_rate_hz, std::memory_order_relaxed);
  state_->init_channels.store(num_channels, std::memory_order_relaxed);
  state_->reset_pending.store(true, std::memory_order_release);
}

std::string RootCaptureProcessor::ToString() const {
  return "RootCaptureProcessor";
}

void RootCaptureProcessor::SetRuntimeSetting(
    webrtc::AudioProcessing::RuntimeSetting /* setting */) {}

void RootCaptureProcessor::Process(webrtc::AudioBuffer* audio) {
  const size_t channels = audio->num_channels();
  const size_t frames = audio->num_frames();
  if (channels == 0 || frames == 0) {
    return;
  }
  if (state_->reset_pending.exchange(false, std::memory_order_acquire)) {
    init_rate_ = state_->init_rate.load(std::memory_order_relaxed);
  }
  const int rate = init_rate_ > 0 && static_cast<size_t>(init_rate_ / 100) == frames
                       ? init_rate_
                       : static_cast<int>(frames * 100);
  if (rate != rate_ || channels != channels_) {
    rate_ = rate;
    channels_ = channels;
    // Seqlock: odd while the format is being written.
    const uint64_t seq = state_->format_seq.load(std::memory_order_relaxed);
    state_->format_seq.store(seq + 1, std::memory_order_relaxed);
    std::atomic_thread_fence(std::memory_order_release);
    state_->process_rate.store(static_cast<uint32_t>(rate),
                               std::memory_order_relaxed);
    state_->process_channels.store(static_cast<uint32_t>(channels),
                                   std::memory_order_relaxed);
    state_->format_seq.store(seq + 2, std::memory_order_release);
  }

  RootCaptureState& state = *state_;
  constexpr auto kRelaxed = std::memory_order_relaxed;
  const uint32_t gate = state.gate.load(kRelaxed);
  RootDspSettings settings{};
  settings.sample_rate = static_cast<uint32_t>(rate);
  settings.mic_muted = (gate & RootCaptureState::kGateMuted) != 0;
  settings.push_to_talk = gate >> RootCaptureState::kGatePushToTalkShift;
  settings.input_volume = state.input_volume.load(kRelaxed);
  settings.auto_gain = state.auto_gain.load(kRelaxed);
  settings.gate_threshold = state.gate_threshold.load(kRelaxed);
  settings.auto_sensitivity = state.auto_sensitivity.load(kRelaxed);
  RootDenoiserSlot* slot = state.denoiser_slot.get();
  // Without a slot no denoiser can run, so the chain never waits for one.
  settings.denoiser_wanted =
      slot != nullptr && state.denoiser_wanted.load(kRelaxed);
  const float power = state.denoise_power.load(kRelaxed);

  // The gain reset first: a session reset stored before it is then seen in
  // the same frame, and the chain applies the reset before the gain, never a
  // frame after it (which would wipe it).
  constexpr auto kAcquire = std::memory_order_acquire;
  RootDspCommands commands{};
  const uint64_t gain_seq = state.gain_reset_seq.load(kAcquire);
  const uint64_t reset_seq = state.dsp_reset_seq.load(kAcquire);
  commands.reset = reset_seq != seen_reset_seq_;
  seen_reset_seq_ = reset_seq;
  const uint64_t epoch = state.stream_epoch.load(kAcquire);
  commands.flush = epoch != seen_stream_epoch_;
  seen_stream_epoch_ = epoch;
  if (gain_seq != seen_gain_reset_seq_) {
    seen_gain_reset_seq_ = gain_seq;
    commands.gain_reset = true;
    commands.gain_reset_db = state.gain_reset_db.load(kRelaxed);
  }

  float* const* data = audio->channels();
  float* samples = data[0];
  const rust::Slice<float> frame(samples, frames);
  RootDspOutputs outputs{};
  const RootDspPre pre = dsp_->pre_denoise(frame, settings, commands, outputs);

  RootDenoiseResult denoised;
  if (pre.supported && settings.denoiser_wanted) {
    denoised = slot->ProcessAudio(
        samples, frames, rate, pre.flush_denoiser || denoiser_flush_owed_,
        power, denoise_out_.data(),
        std::min(kDenoiseOutputFrames * frames, denoise_out_.size()));
    denoiser_flush_owed_ = false;
  } else if (pre.supported && slot != nullptr) {
    // Not wanted: an installed pipeline is not fed (NotInUse covers it).
    denoiser_flush_owed_ = true;
  }
  const size_t denoised_samples =
      denoised.kind == RootDenoiseKind::kRan
          ? std::min(denoised.samples, denoise_out_.size())
          : 0;
  dsp_->post_denoise(
      frame, static_cast<uint8_t>(denoised.kind),
      rust::Slice<const float>(denoise_out_.data(), denoised_samples),
      denoised.restart, settings, outputs);
  if (slot != nullptr && outputs.denoiser_stalled && !stalled_) {
    slot->LatchBypass(RootDenoiserBypass::kOutputStalled);
  }
  stalled_ = outputs.denoiser_stalled;

  for (size_t c = 1; c < channels; ++c) {
    std::copy(samples, samples + frames, data[c]);
  }
  Publish(outputs, frames, settings.denoiser_wanted);
}

void RootCaptureProcessor::Publish(const RootDspOutputs& outputs,
                                   size_t frames,
                                   bool denoiser_wanted) {
  RootCaptureState& state = *state_;
  constexpr auto kRelaxed = std::memory_order_relaxed;
  const bool supported = !outputs.unsupported;
  // An unsupported frame passes the denoiser and gate stages untouched.
  const double denoised_sumsq =
      supported ? outputs.denoised_sumsq : outputs.apm_out_sumsq;
  const double gate_in_sumsq =
      supported ? outputs.gate_in_sumsq : outputs.apm_out_sumsq;

  const uint64_t seq = state.levels_seq.load(kRelaxed);
  state.levels_seq.store(seq + 1, kRelaxed);
  std::atomic_thread_fence(std::memory_order_release);
  AddTo(state.apm_out_sumsq, outputs.apm_out_sumsq);
  AddTo(state.denoised_sumsq, denoised_sumsq);
  AddTo(state.gate_in_sumsq, gate_in_sumsq);
  AddTo(state.sent_sumsq, outputs.sent_sumsq);
  state.stage_samples.fetch_add(frames, kRelaxed);
  state.processed_frames.fetch_add(1, kRelaxed);
  state.levels_seq.store(seq + 2, std::memory_order_release);

  const bool held = supported && outputs.held;
  const bool ran = supported && outputs.denoiser_ran;
  CountIf(state.gate_open_frames, supported && outputs.gate_open);
  CountIf(state.gate_input_above_frames,
          supported && outputs.gate_input_above);
  CountIf(state.startup_hold_frames, held);
  CountIf(state.transmit_frames,
          supported ? outputs.transmitting : outputs.transmit_open);
  CountIf(state.unsupported_frames, !supported);
  CountIf(state.dsp_resets, outputs.reset);
  CountIf(state.dsp_flushes, outputs.flushed);
  CountIf(state.ns_gap_frames,
          outputs.transmit_open && denoiser_wanted && !held && !ran &&
              !state.webrtc_ns_applied.load(kRelaxed));
  if (outputs.non_finite_samples > 0) {
    state.non_finite_samples.fetch_add(outputs.non_finite_samples, kRelaxed);
  }

  state.threshold.store(outputs.threshold, kRelaxed);
  state.voice_gain_db.store(outputs.voice_gain_db, kRelaxed);
  state.voice_gain_settled.store(outputs.voice_gain_settled, kRelaxed);
  state.gate_speaking.store(outputs.gate_speaking, kRelaxed);
  if (supported) {
    RaiseTo(state.gate_in_block_peak_db_max, outputs.gate_in_block_peak_db);
    RaiseTo(state.envelope_max, outputs.envelope);
  }
  state.aligner_primed.store(outputs.aligner_primed, kRelaxed);
  state.aligner_buffered_samples.store(outputs.aligner_buffered_samples,
                                       kRelaxed);
  state.aligner_underruns.store(outputs.aligner_underruns, kRelaxed);
  state.aligner_padded_samples.store(outputs.aligner_padded_samples, kRelaxed);
  state.aligner_trimmed_samples.store(outputs.aligner_trimmed_samples,
                                      kRelaxed);
  state.aligner_drained_samples.store(outputs.aligner_drained_samples,
                                      kRelaxed);
  state.aligner_latency_ms.store(outputs.aligner_latency_ms, kRelaxed);

  state.holding.store(held, kRelaxed);
  // Against the stored flag, which ClearSpeaking may have changed; a clear
  // that lands in between wins and this value goes out on the next frame.
  uint64_t word = state.speaking_word.load(kRelaxed);
  if (((word & 1) != 0) != outputs.speaking) {
    state.speaking_word.compare_exchange_strong(
        word, (((word >> 1) + 1) << 1) | (outputs.speaking ? 1u : 0u),
        std::memory_order_release, kRelaxed);
  }
  if (outputs.transmit_open) {
    int64_t stamp = state.gate_open_stamp_us.load(kRelaxed);
    if (stamp != 0 &&
        state.gate_open_stamp_us.compare_exchange_strong(stamp, 0, kRelaxed)) {
      state.gate_open_delay_us.store(
          static_cast<uint64_t>(std::max<int64_t>(0, SteadyNowUs() - stamp)),
          kRelaxed);
    }
  }
}

// =============================================================================
// RootApmDecorator
// =============================================================================

bool RootApmConfigsEqual(const webrtc::AudioProcessing::Config& a,
                         const webrtc::AudioProcessing::Config& b) {
  return a.pipeline.maximum_internal_processing_rate ==
             b.pipeline.maximum_internal_processing_rate &&
         a.pipeline.multi_channel_render == b.pipeline.multi_channel_render &&
         a.pipeline.multi_channel_capture == b.pipeline.multi_channel_capture &&
         a.pipeline.capture_downmix_method ==
             b.pipeline.capture_downmix_method &&
         a.pipeline.capture_downmix_method_stereo_aec ==
             b.pipeline.capture_downmix_method_stereo_aec &&
         a.pre_amplifier.enabled == b.pre_amplifier.enabled &&
         a.pre_amplifier.fixed_gain_factor == b.pre_amplifier.fixed_gain_factor &&
         a.capture_level_adjustment == b.capture_level_adjustment &&
         a.high_pass_filter.enabled == b.high_pass_filter.enabled &&
         a.high_pass_filter.apply_in_full_band ==
             b.high_pass_filter.apply_in_full_band &&
         a.echo_canceller.enabled == b.echo_canceller.enabled &&
         a.echo_canceller.export_linear_aec_output ==
             b.echo_canceller.export_linear_aec_output &&
         a.echo_canceller.enforce_high_pass_filtering ==
             b.echo_canceller.enforce_high_pass_filtering &&
         a.noise_suppression.enabled == b.noise_suppression.enabled &&
         a.noise_suppression.level == b.noise_suppression.level &&
         a.noise_suppression.analyze_linear_aec_output_when_available ==
             b.noise_suppression.analyze_linear_aec_output_when_available &&
         a.transient_suppression.enabled == b.transient_suppression.enabled &&
         a.gain_controller1 == b.gain_controller1 &&
         a.gain_controller2 == b.gain_controller2;
}

RootApmDecorator::RootApmDecorator(
    webrtc::scoped_refptr<webrtc::AudioProcessing> inner,
    Scheduler scheduler,
    Clock clock,
    std::shared_ptr<RootCaptureState> capture_state)
    : inner_(std::move(inner)),
      scheduler_(std::move(scheduler)),
      clock_(clock ? std::move(clock) : Clock(&SteadyNowMs)),
      capture_(std::move(capture_state)) {
  webrtc::MutexLock lock(&mutex_);
  requested_ = inner_->GetConfig();
  applied_ = requested_;
  ApplyLocked();
}

RootApmDecorator::~RootApmDecorator() = default;

webrtc::AudioProcessing::Config RootApmDecorator::Forced(
    const Config& requested) {
  Config forced = requested;
  forced.gain_controller1.enabled = false;
  forced.gain_controller2.enabled = false;
  forced.gain_controller2.input_volume_controller.enabled = false;
  forced.high_pass_filter.enabled = true;
  return forced;
}

webrtc::AudioProcessing::Config RootApmDecorator::MergeLocked(
    const Config& requested) const {
  Config merged = Forced(requested);
  if (effective_echo_cancellation_.has_value()) {
    merged.echo_canceller.enabled = *effective_echo_cancellation_;
  }
  if (effective_noise_suppression_.has_value()) {
    merged.noise_suppression.enabled = *effective_noise_suppression_;
    if (*effective_noise_suppression_) {
      merged.noise_suppression.level = Config::NoiseSuppression::kHigh;
    }
  }
  return merged;
}

void RootApmDecorator::ApplyLocked() {
  const Config merged = MergeLocked(requested_);
  if (!RootApmConfigsEqual(merged, applied_)) {
    inner_->ApplyConfig(merged);
    applied_ = merged;
    inner_applies_.fetch_add(1, std::memory_order_relaxed);
  }
  echo_cancellation_active_.store(applied_.echo_canceller.enabled,
                                  std::memory_order_relaxed);
  noise_suppression_active_.store(applied_.noise_suppression.enabled,
                                  std::memory_order_relaxed);
  if (capture_) {
    capture_->webrtc_ns_applied.store(applied_.noise_suppression.enabled,
                                      std::memory_order_relaxed);
  }
}

void RootApmDecorator::SetPolicy(std::optional<bool> echo_cancellation,
                                 std::optional<bool> noise_suppression) {
  webrtc::MutexLock lock(&mutex_);
  if (echo_cancellation.has_value()) {
    desired_echo_cancellation_ = echo_cancellation;
  }
  if (noise_suppression.has_value()) {
    desired_noise_suppression_ = noise_suppression;
  }
  SchedulePolicyLocked();
  PublishPolicyStatsLocked();
}

void RootApmDecorator::ResetPolicy() {
  webrtc::MutexLock lock(&mutex_);
  desired_echo_cancellation_.reset();
  desired_noise_suppression_.reset();
  fallback_noise_suppression_ = false;
  SchedulePolicyLocked();
  PublishPolicyStatsLocked();
}

void RootApmDecorator::SetFallbackNoiseSuppression(bool on) {
  webrtc::MutexLock lock(&mutex_);
  if (fallback_noise_suppression_ == on) {
    return;
  }
  fallback_noise_suppression_ = on;
  SchedulePolicyLocked();
  PublishPolicyStatsLocked();
}

void RootApmDecorator::PublishPolicyStatsLocked() {
  constexpr auto kRelaxed = std::memory_order_relaxed;
  stats_applies_.store(policy_applies_, kRelaxed);
  stats_deferred_.store(policy_deferred_, kRelaxed);
  stats_forced_.store(policy_forced_, kRelaxed);
  stats_pending_.store(PolicyPendingLocked(), kRelaxed);
  stats_fallback_ns_.store(fallback_noise_suppression_, kRelaxed);
}

RootApmPolicyStats RootApmDecorator::ReadPolicyStats() const {
  constexpr auto kRelaxed = std::memory_order_relaxed;
  RootApmPolicyStats stats;
  stats.applies = stats_applies_.load(kRelaxed);
  stats.deferred = stats_deferred_.load(kRelaxed);
  stats.forced = stats_forced_.load(kRelaxed);
  stats.pending = stats_pending_.load(kRelaxed);
  stats.fallback_ns = stats_fallback_ns_.load(kRelaxed);
  return stats;
}

std::optional<bool> RootApmDecorator::DesiredNoiseSuppressionLocked() const {
  return fallback_noise_suppression_ ? std::optional<bool>(true)
                                     : desired_noise_suppression_;
}

bool RootApmDecorator::PolicyPendingLocked() const {
  return desired_echo_cancellation_ != effective_echo_cancellation_ ||
         DesiredNoiseSuppressionLocked() != effective_noise_suppression_;
}

bool RootApmDecorator::UrgentLocked() const {
  return fallback_noise_suppression_ && effective_noise_suppression_ != true;
}

void RootApmDecorator::SchedulePolicyLocked() {
  if (!PolicyPendingLocked()) {
    change_since_ms_.reset();
    policy_waited_ = false;
    return;
  }
  const int64_t now = clock_();
  if (!change_since_ms_.has_value()) {
    change_since_ms_ = now;
  }
  if (!scheduler_) {
    ApplyPolicyNowLocked(now, false);
    return;
  }
  // A dead denoiser leaves the user without noise suppression: no rate
  // limit, no waiting for a pause, and any scheduled application is replaced.
  if (UrgentLocked()) {
    PostPolicyLocked(0);
    return;
  }
  if (policy_apply_scheduled_) {
    return;
  }
  int64_t delay_ms = 0;
  if (policy_applied_once_) {
    delay_ms = std::max<int64_t>(
        0, last_policy_apply_ms_ + kMinPolicyIntervalMs - now);
  }
  PostPolicyLocked(delay_ms);
}

void RootApmDecorator::PostPolicyLocked(int64_t delay_ms) {
  policy_apply_scheduled_ = true;
  const uint64_t generation = ++policy_generation_;
  scheduler_(delay_ms, [self = webrtc::scoped_refptr<RootApmDecorator>(this),
                        generation]() mutable {
    self->ApplyPolicy(generation);
  });
}

void RootApmDecorator::ApplyPolicy(uint64_t generation) {
  webrtc::MutexLock lock(&mutex_);
  ApplyPolicyLocked(generation);
  PublishPolicyStatsLocked();
}

void RootApmDecorator::ApplyPolicyLocked(uint64_t generation) {
  if (generation != policy_generation_) {
    return;
  }
  policy_apply_scheduled_ = false;
  if (!PolicyPendingLocked()) {
    change_since_ms_.reset();
    policy_waited_ = false;
    return;
  }
  const int64_t now = clock_();
  const bool speaking =
      !UrgentLocked() && capture_ && capture_->AudiblySpeaking();
  if (speaking && change_since_ms_.has_value() &&
      now < *change_since_ms_ + kSpeechPauseMaxMs) {
    policy_waited_ = true;
    PostPolicyLocked(kSpeechPausePollMs);
    return;
  }
  ApplyPolicyNowLocked(now, speaking);
}

void RootApmDecorator::ApplyPolicyNowLocked(int64_t now_ms, bool forced) {
  effective_echo_cancellation_ = desired_echo_cancellation_;
  effective_noise_suppression_ = DesiredNoiseSuppressionLocked();
  last_policy_apply_ms_ = now_ms;
  policy_applied_once_ = true;
  ++policy_applies_;
  if (policy_waited_) {
    ++policy_deferred_;
  }
  if (forced) {
    ++policy_forced_;
  }
  policy_waited_ = false;
  change_since_ms_.reset();
  ApplyLocked();
}

RootApmStats RootApmDecorator::ReadStats() {
  const webrtc::AudioProcessingStats stats = inner_->GetStatistics();
  RootApmStats result;
  result.erl_db = OrNaN(stats.echo_return_loss);
  result.erle_db = OrNaN(stats.echo_return_loss_enhancement);
  result.divergent_filter_fraction = OrNaN(stats.divergent_filter_fraction);
  result.delay_ms =
      stats.delay_ms.value_or(stats.delay_median_ms.value_or(-1));
  result.echo_cancellation_active = echo_cancellation_active();
  result.noise_suppression_active = noise_suppression_active();
  return result;
}

bool RootApmDecorator::echo_cancellation_active() const {
  return echo_cancellation_active_.load(std::memory_order_relaxed);
}

bool RootApmDecorator::noise_suppression_active() const {
  return noise_suppression_active_.load(std::memory_order_relaxed);
}

uint64_t RootApmDecorator::inner_apply_count() const {
  return inner_applies_.load(std::memory_order_relaxed);
}

uint64_t RootApmDecorator::swallowed_mute_count() const {
  return swallowed_mutes_.load(std::memory_order_relaxed);
}

int RootApmDecorator::Initialize() {
  return inner_->Initialize();
}

int RootApmDecorator::Initialize(
    const webrtc::ProcessingConfig& processing_config) {
  return inner_->Initialize(processing_config);
}

void RootApmDecorator::ApplyConfig(const Config& config) {
  webrtc::MutexLock lock(&mutex_);
  requested_ = config;
  ApplyLocked();
}

int RootApmDecorator::proc_sample_rate_hz() const {
  return inner_->proc_sample_rate_hz();
}

int RootApmDecorator::proc_split_sample_rate_hz() const {
  return inner_->proc_split_sample_rate_hz();
}

size_t RootApmDecorator::num_input_channels() const {
  return inner_->num_input_channels();
}

size_t RootApmDecorator::num_proc_channels() const {
  return inner_->num_proc_channels();
}

size_t RootApmDecorator::num_output_channels() const {
  return inner_->num_output_channels();
}

size_t RootApmDecorator::num_reverse_channels() const {
  return inner_->num_reverse_channels();
}

void RootApmDecorator::set_output_will_be_muted(bool /* muted */) {
  swallowed_mutes_.fetch_add(1, std::memory_order_relaxed);
}

bool RootApmDecorator::get_output_will_be_muted() {
  return inner_->get_output_will_be_muted();
}

void RootApmDecorator::SetRuntimeSetting(RuntimeSetting setting) {
  if (setting.type() == RuntimeSetting::Type::kCaptureOutputUsed) {
    swallowed_mutes_.fetch_add(1, std::memory_order_relaxed);
    return;
  }
  inner_->SetRuntimeSetting(setting);
}

bool RootApmDecorator::PostRuntimeSetting(RuntimeSetting setting) {
  if (setting.type() == RuntimeSetting::Type::kCaptureOutputUsed) {
    swallowed_mutes_.fetch_add(1, std::memory_order_relaxed);
    return true;
  }
  return inner_->PostRuntimeSetting(setting);
}

int RootApmDecorator::ProcessStream(const int16_t* const src,
                                    const webrtc::StreamConfig& input_config,
                                    const webrtc::StreamConfig& output_config,
                                    int16_t* const dest) {
  return inner_->ProcessStream(src, input_config, output_config, dest);
}

int RootApmDecorator::ProcessStream(const float* const* src,
                                    const webrtc::StreamConfig& input_config,
                                    const webrtc::StreamConfig& output_config,
                                    float* const* dest) {
  return inner_->ProcessStream(src, input_config, output_config, dest);
}

int RootApmDecorator::ProcessReverseStream(
    const int16_t* const src,
    const webrtc::StreamConfig& input_config,
    const webrtc::StreamConfig& output_config,
    int16_t* const dest) {
  return inner_->ProcessReverseStream(src, input_config, output_config, dest);
}

int RootApmDecorator::ProcessReverseStream(
    const float* const* src,
    const webrtc::StreamConfig& input_config,
    const webrtc::StreamConfig& output_config,
    float* const* dest) {
  return inner_->ProcessReverseStream(src, input_config, output_config, dest);
}

int RootApmDecorator::AnalyzeReverseStream(
    const float* const* data,
    const webrtc::StreamConfig& reverse_config) {
  return inner_->AnalyzeReverseStream(data, reverse_config);
}

bool RootApmDecorator::GetLinearAecOutput(
    std::span<std::array<float, 160>> linear_output) const {
  return inner_->GetLinearAecOutput(linear_output);
}

void RootApmDecorator::set_stream_analog_level(int level) {
  inner_->set_stream_analog_level(level);
}

int RootApmDecorator::recommended_stream_analog_level() const {
  return inner_->recommended_stream_analog_level();
}

int RootApmDecorator::set_stream_delay_ms(int delay) {
  return inner_->set_stream_delay_ms(delay);
}

int RootApmDecorator::stream_delay_ms() const {
  return inner_->stream_delay_ms();
}

void RootApmDecorator::set_stream_key_pressed(bool key_pressed) {
  inner_->set_stream_key_pressed(key_pressed);
}

bool RootApmDecorator::CreateAndAttachAecDump(
    absl::string_view file_name,
    int64_t max_log_size_bytes,
    webrtc::TaskQueueBase* worker_queue) {
  return inner_->CreateAndAttachAecDump(file_name, max_log_size_bytes,
                                        worker_queue);
}

bool RootApmDecorator::CreateAndAttachAecDump(
    FILE* handle,
    int64_t max_log_size_bytes,
    webrtc::TaskQueueBase* worker_queue) {
  return inner_->CreateAndAttachAecDump(handle, max_log_size_bytes,
                                        worker_queue);
}

void RootApmDecorator::AttachAecDump(std::unique_ptr<webrtc::AecDump> aec_dump) {
  inner_->AttachAecDump(std::move(aec_dump));
}

void RootApmDecorator::DetachAecDump() {
  inner_->DetachAecDump();
}

webrtc::AudioProcessingStats RootApmDecorator::GetStatistics() {
  return inner_->GetStatistics();
}

webrtc::AudioProcessingStats RootApmDecorator::GetStatistics(
    bool has_remote_tracks) {
  return inner_->GetStatistics(has_remote_tracks);
}

webrtc::AudioProcessing::Config RootApmDecorator::GetConfig() const {
  webrtc::MutexLock lock(&mutex_);
  return Forced(requested_);
}

webrtc::scoped_refptr<RootApmDecorator> CreateRootAudioProcessing(
    const webrtc::Environment& env,
    std::shared_ptr<RootCaptureState> capture_state,
    webrtc::TaskQueueBase* policy_queue) {
  webrtc::scoped_refptr<webrtc::AudioProcessing> apm =
      BuildRootApm(env, capture_state);
  if (!apm) {
    return nullptr;
  }
  RootApmDecorator::Scheduler scheduler;
  if (policy_queue != nullptr) {
    scheduler = [policy_queue](int64_t delay_ms,
                               absl::AnyInvocable<void() &&> task) {
      policy_queue->PostDelayedTask(std::move(task),
                                    webrtc::TimeDelta::Millis(delay_ms));
    };
  }
  return webrtc::make_ref_counted<RootApmDecorator>(
      std::move(apm), std::move(scheduler), RootApmDecorator::Clock(),
      std::move(capture_state));
}

// =============================================================================
// Test hooks
// =============================================================================

namespace {

std::shared_ptr<RootCaptureState> NewCaptureState(bool with_slot) {
  auto state = std::make_shared<RootCaptureState>();
  if (with_slot) {
    state->denoiser_slot = std::make_shared<RootDenoiserSlot>();
  }
  return state;
}

void SetDsp(RootCaptureState& state,
            bool auto_gain,
            bool auto_sensitivity,
            float gate_threshold) {
  state.auto_gain.store(auto_gain);
  state.auto_sensitivity.store(auto_sensitivity);
  state.gate_threshold.store(gate_threshold);
}

}  // namespace

RootProcessorHarness::RootProcessorHarness(bool with_slot)
    : state_(NewCaptureState(with_slot)),
      processor_(std::make_unique<RootCaptureProcessor>(state_)) {}

RootProcessorHarness::~RootProcessorHarness() {
  processor_.reset();
  if (state_->denoiser_slot) {
    state_->denoiser_slot->RemoveForTest();
  }
}

void RootProcessorHarness::initialize(int32_t rate, int32_t channels) const {
  processor_->Initialize(rate, channels);
}

void RootProcessorHarness::set_mic_muted(bool muted) const {
  state_->SetGate(muted, std::nullopt);
}

void RootProcessorHarness::set_push_to_talk(uint32_t state) const {
  state_->SetGate(std::nullopt, state);
}

void RootProcessorHarness::set_gate(int8_t mic_muted,
                                    int32_t push_to_talk) const {
  state_->SetGate(
      mic_muted < 0 ? std::nullopt : std::optional<bool>(mic_muted != 0),
      push_to_talk < 0
          ? std::nullopt
          : std::optional<uint32_t>(static_cast<uint32_t>(push_to_talk)));
}

void RootProcessorHarness::set_input_volume(float volume) const {
  state_->input_volume.store(volume);
}

void RootProcessorHarness::reset_controls() const {
  state_->ResetControls(false);
}

void RootProcessorHarness::set_dsp(bool auto_gain,
                                   bool auto_sensitivity,
                                   float gate_threshold) const {
  SetDsp(*state_, auto_gain, auto_sensitivity, gate_threshold);
}

void RootProcessorHarness::set_denoiser_wanted(bool wanted) const {
  state_->denoiser_wanted.store(wanted);
}

void RootProcessorHarness::set_denoise_power(float power) const {
  state_->denoise_power.store(power);
}

void RootProcessorHarness::set_webrtc_ns_applied(bool applied) const {
  state_->webrtc_ns_applied.store(applied);
}

void RootProcessorHarness::bump_stream_epoch() const {
  state_->stream_epoch.fetch_add(1, std::memory_order_release);
}

void RootProcessorHarness::gain_reset(float db) const {
  state_->gain_reset_db.store(db, std::memory_order_relaxed);
  state_->gain_reset_seq.fetch_add(1, std::memory_order_release);
}

RootProcessorFrame RootProcessorHarness::process(
    int32_t rate,
    size_t channels,
    rust::Slice<const float> input) const {
  const size_t rate_hz = static_cast<size_t>(rate);
  webrtc::AudioBuffer buffer(rate_hz, channels, rate_hz, channels, rate_hz,
                             channels);
  const size_t frames = buffer.num_frames();
  float* const* data = buffer.channels();
  for (size_t c = 0; c < channels; ++c) {
    for (size_t i = 0; i < frames; ++i) {
      data[c][i] = c == 0 ? (i < input.size() ? input[i] : 0.0f)
                          : 12345.0f + static_cast<float>(c);
    }
  }
  processor_->Process(&buffer);

  RootProcessorFrame result{};
  result.copies_match = true;
  for (size_t i = 0; i < frames; ++i) {
    result.samples.push_back(data[0][i]);
    for (size_t c = 1; c < channels; ++c) {
      result.copies_match &= data[c][i] == data[0][i];
    }
  }
  return result;
}

RootProcessorStats RootProcessorHarness::stats() const {
  const RootCaptureFormatSnapshot format = ReadCaptureFormat(*state_);
  RootProcessorStats stats{};
  stats.format_seq = format.seq;
  stats.process_rate = format.sample_rate;
  stats.process_channels = format.channels;
  stats.processed_frames = state_->processed_frames.load();
  stats.apm_out_sumsq = state_->apm_out_sumsq.load();
  stats.sent_sumsq = state_->sent_sumsq.load();
  stats.stage_samples = state_->stage_samples.load();
  return stats;
}

RootProcessorDsp RootProcessorHarness::dsp(bool take_maxima) const {
  const RootDspStatsSnapshot s = state_->ReadDspStats(take_maxima);
  const RootSpeakingSnapshot published = state_->ReadSpeaking();
  RootProcessorDsp dsp{};
  dsp.processed_frames = s.processed_frames;
  dsp.stage_samples = s.stage_samples;
  dsp.apm_out_sumsq = s.apm_out_sumsq;
  dsp.denoised_sumsq = s.denoised_sumsq;
  dsp.gate_in_sumsq = s.gate_in_sumsq;
  dsp.sent_sumsq = s.sent_sumsq;
  dsp.gate_open_frames = s.gate_open_frames;
  dsp.gate_input_above_frames = s.gate_input_above_frames;
  dsp.startup_hold_frames = s.startup_hold_frames;
  dsp.transmit_frames = s.transmit_frames;
  dsp.unsupported_frames = s.unsupported_frames;
  dsp.non_finite_samples = s.non_finite_samples;
  dsp.dsp_resets = s.dsp_resets;
  dsp.dsp_flushes = s.dsp_flushes;
  dsp.ns_gap_frames = s.ns_gap_frames;
  dsp.speaking_seq = s.speaking_seq;
  dsp.gate_open_delay_us = s.gate_open_delay_us;
  dsp.threshold = s.threshold;
  dsp.voice_gain_db = s.voice_gain_db;
  dsp.voice_gain_settled = s.voice_gain_settled;
  dsp.gate_speaking = s.gate_speaking;
  dsp.speaking = s.speaking;
  dsp.gate_in_block_peak_db = s.gate_in_block_peak_db;
  dsp.envelope_max = s.envelope_max;
  dsp.aligner_primed = s.aligner_primed;
  dsp.aligner_buffered_samples = s.aligner_buffered_samples;
  dsp.aligner_underruns = s.aligner_underruns;
  dsp.aligner_padded_samples = s.aligner_padded_samples;
  dsp.aligner_trimmed_samples = s.aligner_trimmed_samples;
  dsp.aligner_drained_samples = s.aligner_drained_samples;
  dsp.aligner_latency_ms = s.aligner_latency_ms;
  dsp.published_speaking = published.speaking;
  dsp.published_seq = published.seq;
  return dsp;
}

bool RootProcessorHarness::install_fake_denoiser(uint32_t rate,
                                                 float scale,
                                                 uint32_t delay_samples,
                                                 uint32_t burst_call,
                                                 uint32_t empty_from,
                                                 uint32_t empty_count) const {
  RootFakeFxConfig config;
  config.scale = scale;
  config.delay_samples = delay_samples;
  config.burst_call = burst_call;
  config.empty_from = empty_from;
  config.empty_count = empty_count;
  auto fake = std::make_unique<RootFakeFxPipeline>(config);
  const bool installed =
      state_->denoiser_slot->InstallForTest(fake->pipeline(), rate);
  fakes_.push_back(std::move(fake));
  return installed;
}

bool RootProcessorHarness::remove_fake_denoiser() const {
  return state_->denoiser_slot->RemoveForTest() != nullptr;
}

RootFakeDenoiserProbe RootProcessorHarness::fake_denoiser() const {
  RootFakeDenoiserProbe probe{};
  if (!fakes_.empty()) {
    const RootFakeFxPipeline& fake = *fakes_.back();
    probe.process_calls = fake.process_calls();
    probe.flush_calls = fake.flush_calls();
    probe.power_sets = fake.power_sets();
    probe.last_power = fake.last_power();
  }
  if (state_->denoiser_slot) {
    probe.installed = state_->denoiser_slot->installed();
    probe.bypass = static_cast<uint32_t>(state_->denoiser_slot->bypass());
    probe.misses = state_->denoiser_slot->ReadStats(false).misses;
  }
  return probe;
}

bool RootProcessorHarness::hold_denoiser_slot(bool hold) const {
  return state_->denoiser_slot->HoldForTest(hold);
}

void RootProcessorHarness::set_published_speaking(bool speaking) const {
  const uint64_t transitions = state_->ReadSpeaking().seq + 1;
  state_->speaking_word.store((transitions << 1) | (speaking ? 1u : 0u),
                              std::memory_order_release);
}

int64_t RootProcessorHarness::gate_open_stamp_us() const {
  return state_->gate_open_stamp_us.load();
}

bool RootProcessorHarness::audibly_speaking() const {
  return state_->AudiblySpeaking();
}

std::unique_ptr<RootProcessorHarness> new_root_processor_harness() {
  return std::make_unique<RootProcessorHarness>(true);
}

std::unique_ptr<RootProcessorHarness> new_root_processor_harness_without_slot() {
  return std::make_unique<RootProcessorHarness>(false);
}

// Records what reaches the APM behind the decorator.
class FakeApm : public webrtc::AudioProcessing {
 public:
  Config last_config;
  uint64_t applies = 0;
  // Configs that had AGC on or the high-pass filter off.
  uint64_t unforced_applies = 0;
  uint64_t mute_calls = 0;
  uint64_t output_used_settings = 0;
  webrtc::AudioProcessingStats stats;
  // Runs inside ApplyConfig, as a re-initialisation would take time there.
  std::function<void()> apply_hook;

  int Initialize() override { return kNoError; }
  int Initialize(const webrtc::ProcessingConfig&) override { return kNoError; }
  void ApplyConfig(const Config& config) override {
    if (apply_hook) {
      apply_hook();
    }
    last_config = config;
    ++applies;
    if (config.gain_controller1.enabled || config.gain_controller2.enabled ||
        config.gain_controller2.input_volume_controller.enabled ||
        !config.high_pass_filter.enabled) {
      ++unforced_applies;
    }
  }
  int proc_sample_rate_hz() const override { return 48000; }
  int proc_split_sample_rate_hz() const override { return 16000; }
  size_t num_input_channels() const override { return 1; }
  size_t num_proc_channels() const override { return 1; }
  size_t num_output_channels() const override { return 1; }
  size_t num_reverse_channels() const override { return 1; }
  void set_output_will_be_muted(bool) override { ++mute_calls; }
  bool get_output_will_be_muted() override { return false; }
  void SetRuntimeSetting(RuntimeSetting setting) override {
    PostRuntimeSetting(setting);
  }
  bool PostRuntimeSetting(RuntimeSetting setting) override {
    if (setting.type() == RuntimeSetting::Type::kCaptureOutputUsed) {
      ++output_used_settings;
    }
    return true;
  }
  int ProcessStream(const int16_t* const,
                    const webrtc::StreamConfig&,
                    const webrtc::StreamConfig&,
                    int16_t* const) override {
    return kNoError;
  }
  int ProcessStream(const float* const*,
                    const webrtc::StreamConfig&,
                    const webrtc::StreamConfig&,
                    float* const*) override {
    return kNoError;
  }
  int ProcessReverseStream(const int16_t* const,
                           const webrtc::StreamConfig&,
                           const webrtc::StreamConfig&,
                           int16_t* const) override {
    return kNoError;
  }
  int ProcessReverseStream(const float* const*,
                           const webrtc::StreamConfig&,
                           const webrtc::StreamConfig&,
                           float* const*) override {
    return kNoError;
  }
  int AnalyzeReverseStream(const float* const*,
                           const webrtc::StreamConfig&) override {
    return kNoError;
  }
  bool GetLinearAecOutput(std::span<std::array<float, 160>>) const override {
    return false;
  }
  void set_stream_analog_level(int) override {}
  int recommended_stream_analog_level() const override { return 0; }
  int set_stream_delay_ms(int) override { return kNoError; }
  int stream_delay_ms() const override { return 0; }
  void set_stream_key_pressed(bool) override {}
  bool CreateAndAttachAecDump(absl::string_view,
                              int64_t,
                              webrtc::TaskQueueBase*) override {
    return false;
  }
  bool CreateAndAttachAecDump(FILE*,
                              int64_t,
                              webrtc::TaskQueueBase*) override {
    return false;
  }
  void AttachAecDump(std::unique_ptr<webrtc::AecDump>) override {}
  void DetachAecDump() override {}
  webrtc::AudioProcessingStats GetStatistics() override { return stats; }
  webrtc::AudioProcessingStats GetStatistics(bool) override { return stats; }
  Config GetConfig() const override { return last_config; }
};

webrtc::scoped_refptr<RootApmDecorator> NewRootApmOverFakeForTest() {
  auto fake = webrtc::make_ref_counted<FakeApm>();
  fake->stats.echo_return_loss = 12.0;
  fake->stats.echo_return_loss_enhancement = 20.0;
  fake->stats.delay_ms = 40;
  fake->stats.divergent_filter_fraction = 0.25;
  return webrtc::make_ref_counted<RootApmDecorator>(
      fake, RootApmDecorator::Scheduler(), RootApmDecorator::Clock());
}

bool root_apm_decorator_race_for_test(uint32_t iterations) {
  auto fake = webrtc::make_ref_counted<FakeApm>();
  auto decorator = webrtc::make_ref_counted<RootApmDecorator>(
      fake, RootApmDecorator::Scheduler(), RootApmDecorator::Clock());
  std::thread engine([&] {
    for (uint32_t i = 0; i < iterations; ++i) {
      webrtc::AudioProcessing::Config config = decorator->GetConfig();
      config.echo_canceller.enabled = i % 2 == 0;
      config.noise_suppression.enabled = i % 3 == 0;
      config.gain_controller1.enabled = true;
      config.gain_controller2.enabled = i % 4 == 0;
      config.high_pass_filter.enabled = false;
      decorator->ApplyConfig(config);
    }
  });
  std::thread policy([&] {
    for (uint32_t i = 0; i < iterations; ++i) {
      if (i % 5 == 4) {
        decorator->ResetPolicy();
      } else {
        decorator->SetPolicy(i % 2 == 0, i % 3 == 0
                                             ? std::optional<bool>(true)
                                             : std::nullopt);
      }
      decorator->ReadStats();
    }
  });
  engine.join();
  policy.join();

  decorator->SetPolicy(false, true);
  webrtc::AudioProcessing::Config config = decorator->GetConfig();
  config.echo_canceller.enabled = true;
  config.noise_suppression.enabled = false;
  decorator->ApplyConfig(config);
  const webrtc::AudioProcessing::Config served = decorator->GetConfig();
  const webrtc::AudioProcessing::Config& inner = fake->last_config;
  return fake->unforced_applies == 0 && !inner.echo_canceller.enabled &&
         inner.noise_suppression.enabled && served.echo_canceller.enabled &&
         !served.noise_suppression.enabled &&
         decorator->echo_cancellation_active() ==
             inner.echo_canceller.enabled &&
         decorator->noise_suppression_active() ==
             inner.noise_suppression.enabled;
}

bool root_apm_policy_stats_never_wait_for_an_apply_for_test() {
  auto fake = webrtc::make_ref_counted<FakeApm>();
  auto decorator = webrtc::make_ref_counted<RootApmDecorator>(
      fake, RootApmDecorator::Scheduler(), RootApmDecorator::Clock());
  std::promise<void> entered;
  std::promise<void> release;
  std::shared_future<void> released = release.get_future().share();
  std::atomic<bool> armed{true};
  fake->apply_hook = [&] {
    if (armed.exchange(false)) {
      entered.set_value();
      released.wait();
    }
  };
  std::thread apply([&] { decorator->SetPolicy(false, true); });
  entered.get_future().wait();
  // The decorator's mutex is held across the inner ApplyConfig now.
  auto read = std::async(std::launch::async,
                         [&] { return decorator->ReadPolicyStats(); });
  const bool quick =
      read.wait_for(std::chrono::seconds(2)) == std::future_status::ready;
  release.set_value();
  apply.join();
  const RootApmPolicyStats during = read.get();
  const RootApmPolicyStats after = decorator->ReadPolicyStats();
  return quick && during.applies == 0 && after.applies == 1 && !after.pending;
}

RootApmHarness::RootApmHarness(bool real_apm)
    : now_ms_(std::make_shared<int64_t>(100000)),
      tasks_(std::make_shared<std::vector<Task>>()),
      capture_(std::make_shared<RootCaptureState>()) {
  if (real_apm) {
    inner_ = BuildRootApm(webrtc::CreateEnvironment(), capture_);
  } else {
    fake_ = webrtc::make_ref_counted<FakeApm>();
    inner_ = fake_;
  }
  auto now = now_ms_;
  auto tasks = tasks_;
  decorator_ = webrtc::make_ref_counted<RootApmDecorator>(
      inner_,
      [now, tasks](int64_t delay_ms, absl::AnyInvocable<void() &&> task) {
        tasks->push_back(Task{*now + delay_ms, std::move(task)});
      },
      [now] { return *now; }, capture_);
}

RootApmHarness::~RootApmHarness() {
  tasks_->clear();
  decorator_ = nullptr;
}

void RootApmHarness::engine_apply(bool echo_cancellation,
                                  bool noise_suppression,
                                  bool auto_gain_control,
                                  bool gain_controller2,
                                  bool high_pass_filter) const {
  webrtc::AudioProcessing::Config config = decorator_->GetConfig();
  config.echo_canceller.enabled = echo_cancellation;
  config.noise_suppression.enabled = noise_suppression;
  config.gain_controller1.enabled = auto_gain_control;
  config.gain_controller1.mode =
      webrtc::AudioProcessing::Config::GainController1::kAdaptiveAnalog;
  config.gain_controller2.enabled = gain_controller2;
  config.gain_controller2.input_volume_controller.enabled = gain_controller2;
  config.high_pass_filter.enabled = high_pass_filter;
  decorator_->ApplyConfig(config);
}

void RootApmHarness::engine_apply_echo_cancellation(
    bool echo_cancellation) const {
  webrtc::AudioProcessing::Config config = decorator_->GetConfig();
  config.echo_canceller.enabled = echo_cancellation;
  decorator_->ApplyConfig(config);
}

void RootApmHarness::set_output_will_be_muted(bool muted) const {
  decorator_->set_output_will_be_muted(muted);
}

bool RootApmHarness::post_capture_output_used(bool used) const {
  decorator_->SetRuntimeSetting(
      webrtc::AudioProcessing::RuntimeSetting::CreateCaptureOutputUsedSetting(
          used));
  return decorator_->PostRuntimeSetting(
      webrtc::AudioProcessing::RuntimeSetting::CreateCaptureOutputUsedSetting(
          used));
}

void RootApmHarness::set_policy(int8_t echo_cancellation,
                                int8_t noise_suppression) const {
  auto policy = [](int8_t value) -> std::optional<bool> {
    if (value < 0) {
      return std::nullopt;
    }
    return value != 0;
  };
  decorator_->SetPolicy(policy(echo_cancellation), policy(noise_suppression));
}

void RootApmHarness::reset_policy() const {
  decorator_->ResetPolicy();
}

void RootApmHarness::advance_clock(int64_t ms) const {
  *now_ms_ += ms;
}

uint32_t RootApmHarness::run_due_tasks() const {
  std::vector<absl::AnyInvocable<void() &&>> due;
  for (auto it = tasks_->begin(); it != tasks_->end();) {
    if (it->due_ms <= *now_ms_) {
      due.push_back(std::move(it->task));
      it = tasks_->erase(it);
    } else {
      ++it;
    }
  }
  for (auto& task : due) {
    std::move(task)();
  }
  return static_cast<uint32_t>(due.size());
}

uint32_t RootApmHarness::pending_tasks() const {
  return static_cast<uint32_t>(tasks_->size());
}

RootDecoratorProbe RootApmHarness::probe() const {
  const webrtc::AudioProcessing::Config inner =
      fake_ ? fake_->last_config : inner_->GetConfig();
  RootDecoratorProbe probe{};
  probe.inner_applies = decorator_->inner_apply_count();
  probe.fake_applies = fake_ ? fake_->applies : 0;
  probe.inner_aec = inner.echo_canceller.enabled;
  probe.inner_ns = inner.noise_suppression.enabled;
  probe.inner_agc1 = inner.gain_controller1.enabled;
  probe.inner_agc2 = inner.gain_controller2.enabled ||
                     inner.gain_controller2.input_volume_controller.enabled;
  probe.inner_hpf = inner.high_pass_filter.enabled;
  probe.fake_mute_calls = fake_ ? fake_->mute_calls : 0;
  probe.fake_output_used_settings = fake_ ? fake_->output_used_settings : 0;
  probe.output_will_be_muted = decorator_->get_output_will_be_muted();
  probe.swallowed = decorator_->swallowed_mute_count();
  probe.aec_active = decorator_->echo_cancellation_active();
  probe.ns_active = decorator_->noise_suppression_active();
  const webrtc::AudioProcessing::Config served = decorator_->GetConfig();
  probe.served_aec = served.echo_canceller.enabled;
  probe.served_ns = served.noise_suppression.enabled;
  probe.served_agc = served.gain_controller1.enabled ||
                     served.gain_controller2.enabled ||
                     served.gain_controller2.input_volume_controller.enabled;
  probe.served_hpf = served.high_pass_filter.enabled;
  return probe;
}

int16_t RootApmHarness::process_frame(int32_t rate,
                                      size_t channels,
                                      int16_t amplitude) const {
  const size_t frames = static_cast<size_t>(rate / 100);
  std::vector<int16_t> data(frames * channels);
  for (size_t i = 0; i < frames; ++i) {
    const double phase = 2.0 * 3.14159265358979 * 1000.0 *
                         static_cast<double>(i) / static_cast<double>(rate);
    const int16_t sample =
        static_cast<int16_t>(std::lround(amplitude * std::sin(phase)));
    for (size_t c = 0; c < channels; ++c) {
      data[i * channels + c] = sample;
    }
  }
  const webrtc::StreamConfig config(rate, channels);
  decorator_->set_stream_delay_ms(0);
  decorator_->ProcessStream(data.data(), config, config, data.data());
  int peak = 0;
  for (int16_t sample : data) {
    peak = std::max(peak, std::abs(static_cast<int>(sample)));
  }
  return static_cast<int16_t>(std::min(peak, 32767));
}

void RootApmHarness::set_mic_muted(bool muted) const {
  capture_->SetGate(muted, std::nullopt);
}

void RootApmHarness::set_dsp(bool auto_gain,
                             bool auto_sensitivity,
                             float gate_threshold) const {
  SetDsp(*capture_, auto_gain, auto_sensitivity, gate_threshold);
}

bool RootApmHarness::speaking() const {
  return capture_->ReadSpeaking().speaking;
}

void RootApmHarness::set_speaking(bool speaking) const {
  const uint64_t transitions = capture_->ReadSpeaking().seq + 1;
  capture_->speaking_word.store((transitions << 1) | (speaking ? 1u : 0u));
}

void RootApmHarness::set_fallback_ns(bool on) const {
  decorator_->SetFallbackNoiseSuppression(on);
}

RootPolicyProbe RootApmHarness::policy() const {
  const RootApmPolicyStats stats = decorator_->ReadPolicyStats();
  RootPolicyProbe probe{};
  probe.applies = stats.applies;
  probe.deferred = stats.deferred;
  probe.forced = stats.forced;
  probe.pending = stats.pending;
  probe.fallback_ns = stats.fallback_ns;
  probe.webrtc_ns_applied = capture_->webrtc_ns_applied.load();
  return probe;
}

uint64_t RootApmHarness::processed_frames() const {
  return capture_->processed_frames.load();
}

uint32_t RootApmHarness::processor_rate() const {
  return ReadCaptureFormat(*capture_).sample_rate;
}

uint32_t RootApmHarness::processor_channels() const {
  return ReadCaptureFormat(*capture_).channels;
}

std::unique_ptr<RootApmHarness> new_root_apm_harness(bool real_apm) {
  return std::make_unique<RootApmHarness>(real_apm);
}

}  // namespace livekit_ffi
