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
#include <limits>
#include <utility>

#include "api/audio/builtin_audio_processing_builder.h"
#include "api/environment/environment_factory.h"
#include "api/make_ref_counted.h"
#include "api/task_queue/task_queue_base.h"
#include "api/units/time_delta.h"
#include "modules/audio_processing/audio_buffer.h"
#include "modules/audio_processing/include/aec_dump.h"

namespace livekit_ffi {
namespace {

constexpr float kMaxInputVolume = 2.0f;

float SanitizeVolume(float volume) {
  if (std::isnan(volume)) {
    return 0.0f;
  }
  return std::min(kMaxInputVolume, std::max(0.0f, volume));
}

double SumSquares(const float* samples, size_t count) {
  double sum = 0;
  for (size_t i = 0; i < count; ++i) {
    sum += static_cast<double>(samples[i]) * samples[i];
  }
  return sum;
}

// Single writer (the audio thread), so a load and a store is enough.
void AddTo(std::atomic<double>& total, double value) {
  total.store(total.load(std::memory_order_relaxed) + value,
              std::memory_order_relaxed);
}

double OrNaN(const std::optional<double>& value) {
  return value.value_or(std::numeric_limits<double>::quiet_NaN());
}

int64_t SteadyNowMs() {
  return std::chrono::duration_cast<std::chrono::milliseconds>(
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

void RootCaptureState::ResetControls() {
  mic_muted.store(false, std::memory_order_relaxed);
  push_to_talk.store(static_cast<uint32_t>(RootPushToTalk::kVoiceActivity),
                     std::memory_order_relaxed);
  input_volume.store(1.0f, std::memory_order_relaxed);
  denoise_power.store(0.75f, std::memory_order_relaxed);
  auto_gain.store(true, std::memory_order_relaxed);
  gate_threshold.store(0.025f, std::memory_order_relaxed);
  auto_sensitivity.store(true, std::memory_order_relaxed);
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
    : state_(std::move(state)) {}

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

void RootCaptureProcessor::ResetDsp() {
  ramp_samples_ = std::max<size_t>(1, static_cast<size_t>(rate_ / 50));
  volume_target_ =
      SanitizeVolume(state_->input_volume.load(std::memory_order_relaxed));
  volume_ = volume_target_;
  volume_step_ = 0.0f;
  volume_remaining_ = 0;
}

// The app's GainRamp: linear over 20 ms toward the newest target.
void RootCaptureProcessor::ApplyInputVolume(float* samples, size_t frames) {
  size_t i = 0;
  for (; i < frames && volume_remaining_ > 0; ++i) {
    volume_ += volume_step_;
    if (--volume_remaining_ == 0) {
      volume_ = volume_target_;
    }
    samples[i] *= volume_;
  }
  if (i == frames || volume_ == 1.0f) {
    return;
  }
  if (volume_ == 0.0f) {
    std::fill(samples + i, samples + frames, 0.0f);
    return;
  }
  for (; i < frames; ++i) {
    samples[i] *= volume_;
  }
}

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
    ResetDsp();
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

  float* const* data = audio->channels();
  float* samples = data[0];
  const double apm_out_sumsq = SumSquares(samples, frames);

  // Phase 2: the Rust DSP chain (voice gain, denoiser slot, auto sensitivity,
  // startup hold, noise gate) runs here on channel 0, between the levels above
  // and the mute/PTT gate below.

  const float target =
      SanitizeVolume(state_->input_volume.load(std::memory_order_relaxed));
  if (target != volume_target_) {
    volume_target_ = target;
    volume_remaining_ = ramp_samples_;
    volume_step_ =
        (target - volume_) / static_cast<float>(ramp_samples_);
  }
  const bool open =
      !state_->mic_muted.load(std::memory_order_relaxed) &&
      state_->push_to_talk.load(std::memory_order_relaxed) !=
          static_cast<uint32_t>(RootPushToTalk::kReleased);
  double sent_sumsq = 0;
  if (open) {
    ApplyInputVolume(samples, frames);
    sent_sumsq = SumSquares(samples, frames);
  } else {
    std::fill(samples, samples + frames, 0.0f);
    volume_ = volume_target_;
    volume_remaining_ = 0;
  }
  for (size_t c = 1; c < channels; ++c) {
    std::copy(samples, samples + frames, data[c]);
  }

  AddTo(state_->apm_out_sumsq, apm_out_sumsq);
  AddTo(state_->sent_sumsq, sent_sumsq);
  state_->stage_samples.fetch_add(frames, std::memory_order_relaxed);
  state_->processed_frames.fetch_add(1, std::memory_order_relaxed);
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
    Clock clock)
    : inner_(std::move(inner)),
      scheduler_(std::move(scheduler)),
      clock_(clock ? std::move(clock) : Clock(&SteadyNowMs)) {
  webrtc::MutexLock lock(&mutex_);
  requested_ = inner_->GetConfig();
  applied_ = requested_;
  ApplyLocked();
}

RootApmDecorator::~RootApmDecorator() = default;

webrtc::AudioProcessing::Config RootApmDecorator::MergeLocked(
    const Config& requested) const {
  Config merged = requested;
  merged.gain_controller1.enabled = false;
  merged.gain_controller2.enabled = false;
  merged.gain_controller2.input_volume_controller.enabled = false;
  merged.high_pass_filter.enabled = true;
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
}

void RootApmDecorator::ResetPolicy() {
  webrtc::MutexLock lock(&mutex_);
  desired_echo_cancellation_.reset();
  desired_noise_suppression_.reset();
  SchedulePolicyLocked();
}

void RootApmDecorator::SchedulePolicyLocked() {
  if (desired_echo_cancellation_ == effective_echo_cancellation_ &&
      desired_noise_suppression_ == effective_noise_suppression_) {
    return;
  }
  if (policy_apply_scheduled_) {
    return;
  }
  const int64_t now = clock_();
  if (!scheduler_) {
    effective_echo_cancellation_ = desired_echo_cancellation_;
    effective_noise_suppression_ = desired_noise_suppression_;
    last_policy_apply_ms_ = now;
    policy_applied_once_ = true;
    ApplyLocked();
    return;
  }
  int64_t delay_ms = 0;
  if (policy_applied_once_) {
    delay_ms = std::max<int64_t>(
        0, last_policy_apply_ms_ + kMinPolicyIntervalMs - now);
  }
  policy_apply_scheduled_ = true;
  scheduler_(delay_ms,
             [self = webrtc::scoped_refptr<RootApmDecorator>(this)]() mutable {
               self->ApplyPolicy();
             });
}

void RootApmDecorator::ApplyPolicy() {
  webrtc::MutexLock lock(&mutex_);
  policy_apply_scheduled_ = false;
  effective_echo_cancellation_ = desired_echo_cancellation_;
  effective_noise_suppression_ = desired_noise_suppression_;
  last_policy_apply_ms_ = clock_();
  policy_applied_once_ = true;
  ApplyLocked();
}

RootApmStats RootApmDecorator::ReadStats() {
  const webrtc::AudioProcessingStats stats = inner_->GetStatistics();
  RootApmStats result;
  result.erl_db = OrNaN(stats.echo_return_loss);
  result.erle_db = OrNaN(stats.echo_return_loss_enhancement);
  result.divergent_filter_fraction = OrNaN(stats.divergent_filter_fraction);
  result.delay_ms = stats.delay_ms.value_or(stats.delay_median_ms.value_or(0));
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
  return applied_;
}

webrtc::scoped_refptr<RootApmDecorator> CreateRootAudioProcessing(
    const webrtc::Environment& env,
    std::shared_ptr<RootCaptureState> capture_state,
    webrtc::TaskQueueBase* policy_queue) {
  webrtc::scoped_refptr<webrtc::AudioProcessing> apm =
      BuildRootApm(env, std::move(capture_state));
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
  return webrtc::make_ref_counted<RootApmDecorator>(std::move(apm),
                                                    std::move(scheduler),
                                                    RootApmDecorator::Clock());
}

// =============================================================================
// Test hooks
// =============================================================================

RootProcessorHarness::RootProcessorHarness()
    : state_(std::make_shared<RootCaptureState>()),
      processor_(std::make_unique<RootCaptureProcessor>(state_)) {}

RootProcessorHarness::~RootProcessorHarness() = default;

void RootProcessorHarness::initialize(int32_t rate, int32_t channels) const {
  processor_->Initialize(rate, channels);
}

void RootProcessorHarness::set_mic_muted(bool muted) const {
  state_->mic_muted.store(muted);
}

void RootProcessorHarness::set_push_to_talk(uint32_t state) const {
  state_->push_to_talk.store(state);
}

void RootProcessorHarness::set_input_volume(float volume) const {
  state_->input_volume.store(volume);
}

void RootProcessorHarness::reset_controls() const {
  state_->ResetControls();
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

std::unique_ptr<RootProcessorHarness> new_root_processor_harness() {
  return std::make_unique<RootProcessorHarness>();
}

// Records what reaches the APM behind the decorator.
class FakeApm : public webrtc::AudioProcessing {
 public:
  Config last_config;
  uint64_t applies = 0;
  uint64_t mute_calls = 0;
  uint64_t output_used_settings = 0;

  int Initialize() override { return kNoError; }
  int Initialize(const webrtc::ProcessingConfig&) override { return kNoError; }
  void ApplyConfig(const Config& config) override {
    last_config = config;
    ++applies;
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
  webrtc::AudioProcessingStats GetStatistics() override { return {}; }
  webrtc::AudioProcessingStats GetStatistics(bool) override { return {}; }
  Config GetConfig() const override { return last_config; }
};

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
      [now] { return *now; });
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
  probe.served_matches_inner = RootApmConfigsEqual(served, inner);
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
  capture_->mic_muted.store(muted);
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
