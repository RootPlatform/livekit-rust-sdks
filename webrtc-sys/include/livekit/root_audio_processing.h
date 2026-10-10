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
#include <cstdio>
#include <functional>
#include <limits>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <vector>

#include "absl/functional/any_invocable.h"
#include "api/audio/audio_processing.h"
#include "api/environment/environment.h"
#include "api/scoped_refptr.h"
#include "rtc_base/synchronization/mutex.h"
#include "rust/cxx.h"

namespace webrtc {
class AudioBuffer;
class TaskQueueBase;
}  // namespace webrtc

namespace livekit_ffi {
struct RootDspSettings;
struct RootDspCommands;
struct RootDspPre;
struct RootDspOutputs;
struct RootDspChain;
struct RootProcessorFrame;
struct RootProcessorStats;
struct RootProcessorDsp;
struct RootFakeDenoiserProbe;
struct RootDecoratorProbe;
struct RootPolicyProbe;
class RootProcessorHarness;
class RootApmHarness;
}  // namespace livekit_ffi
#include "webrtc-sys/src/root_audio_processing.rs.h"

namespace livekit_ffi {

enum class RootPushToTalk : uint32_t {
  kVoiceActivity = 0,
  kHeld = 1,
  kReleased = 2,
};

// The app's VoiceGain only boosts, up to 18 dB.
constexpr float kRootMaxVoiceGainDb = 18.0f;

class RootDenoiserSlot;
class RootFakeFxPipeline;

struct RootDspStatsSnapshot {
  // Consistent with each other (seqlock).
  uint64_t processed_frames = 0;
  uint64_t stage_samples = 0;
  double apm_out_sumsq = 0;
  double denoised_sumsq = 0;
  double gate_in_sumsq = 0;
  double sent_sumsq = 0;

  uint64_t gate_open_frames = 0;
  uint64_t gate_input_above_frames = 0;
  uint64_t startup_hold_frames = 0;
  uint64_t transmit_frames = 0;
  uint64_t unsupported_frames = 0;
  uint64_t non_finite_samples = 0;
  uint64_t dsp_resets = 0;
  uint64_t dsp_flushes = 0;
  uint64_t ns_gap_frames = 0;
  uint64_t speaking_seq = 0;
  uint64_t gate_open_delay_us = 0;
  float threshold = 0;
  float voice_gain_db = std::numeric_limits<float>::quiet_NaN();
  bool voice_gain_settled = false;
  bool gate_speaking = false;
  bool speaking = false;
  // Maxima since the last take; NaN when no frame ran since.
  float gate_in_block_peak_db = std::numeric_limits<float>::quiet_NaN();
  float envelope_max = 0;
  bool aligner_primed = false;
  uint32_t aligner_buffered_samples = 0;
  uint64_t aligner_underruns = 0;
  uint64_t aligner_padded_samples = 0;
  uint64_t aligner_trimmed_samples = 0;
  uint64_t aligner_drained_samples = 0;
  float aligner_latency_ms = 0;
};

struct RootSpeakingSnapshot {
  bool speaking = false;
  uint64_t seq = 0;
};

// Shared by the capture post-processor (audio thread) and the control side:
// atomics only. Settings are written by the control side and read once per
// frame; outputs are written by the audio thread only.
struct RootCaptureState {
  // The gate in one word, so an update of both fields is never seen half
  // done: bit 0 mic muted, bits 1-2 push-to-talk (RootPushToTalk).
  static constexpr uint32_t kGateMuted = 1;
  static constexpr uint32_t kGatePushToTalkShift = 1;
  static constexpr uint32_t kGateClosed =
      kGateMuted |
      (static_cast<uint32_t>(RootPushToTalk::kReleased) << kGatePushToTalkShift);
  static bool GateOpen(uint32_t gate);

  // Settings that take effect now.
  std::atomic<uint32_t> gate{0};
  std::atomic<float> input_volume{1.0f};

  // The DSP chain's settings, read once per frame.
  std::atomic<float> denoise_power{0.75f};
  std::atomic<bool> auto_gain{true};
  std::atomic<float> gate_threshold{0.025f};
  std::atomic<bool> auto_sensitivity{true};
  std::atomic<bool> denoiser_wanted{false};
  // Command: the payload is stored before the sequence number is bumped.
  std::atomic<float> gain_reset_db{0.0f};
  std::atomic<uint64_t> gain_reset_seq{0};
  // Commands: a session start (bumped by ResetControls) and a recording
  // (re)start (bumped before each StartRecording).
  std::atomic<uint64_t> dsp_reset_seq{0};
  std::atomic<uint64_t> stream_epoch{0};
  // What the APM decorator has applied.
  std::atomic<bool> webrtc_ns_applied{false};
  // Set once before the APM is built; may be null.
  std::shared_ptr<RootDenoiserSlot> denoiser_slot;

  // From CustomProcessing::Initialize to Process.
  std::atomic<int> init_rate{0};
  std::atomic<int> init_channels{0};
  std::atomic<bool> reset_pending{false};

  // Outputs. format_seq moves when (process_rate, process_channels) changes;
  // both are written before it.
  std::atomic<uint64_t> format_seq{0};
  std::atomic<uint32_t> process_rate{0};
  std::atomic<uint32_t> process_channels{0};
  // Seqlock (odd while written) over processed_frames, stage_samples and the
  // four channel 0 sums of squares in int16 units: the APM output, after the
  // denoiser, the noise gate's input and what is sent.
  std::atomic<uint64_t> levels_seq{0};
  std::atomic<uint64_t> processed_frames{0};
  std::atomic<double> apm_out_sumsq{0.0};
  std::atomic<double> denoised_sumsq{0.0};
  std::atomic<double> gate_in_sumsq{0.0};
  std::atomic<double> sent_sumsq{0.0};
  std::atomic<uint64_t> stage_samples{0};

  // DSP chain outputs, written by the audio thread only.
  std::atomic<uint64_t> gate_open_frames{0};
  std::atomic<uint64_t> gate_input_above_frames{0};
  std::atomic<uint64_t> startup_hold_frames{0};
  std::atomic<uint64_t> transmit_frames{0};
  std::atomic<uint64_t> unsupported_frames{0};
  std::atomic<uint64_t> non_finite_samples{0};
  std::atomic<uint64_t> dsp_resets{0};
  std::atomic<uint64_t> dsp_flushes{0};
  std::atomic<uint64_t> ns_gap_frames{0};
  std::atomic<float> threshold{0.0f};
  std::atomic<float> voice_gain_db{0.0f};
  std::atomic<bool> voice_gain_settled{false};
  std::atomic<bool> gate_speaking{false};
  // The published speaking flag and its transition count in one word:
  // seq << 1 | speaking.
  std::atomic<uint64_t> speaking_word{0};
  // Maxima since the last ReadDspStats(true); NaN: no frame since.
  std::atomic<float> gate_in_block_peak_db_max{
      std::numeric_limits<float>::quiet_NaN()};
  std::atomic<float> envelope_max{0.0f};
  std::atomic<bool> aligner_primed{false};
  std::atomic<uint32_t> aligner_buffered_samples{0};
  std::atomic<uint64_t> aligner_underruns{0};
  std::atomic<uint64_t> aligner_padded_samples{0};
  std::atomic<uint64_t> aligner_trimmed_samples{0};
  std::atomic<uint64_t> aligner_drained_samples{0};
  std::atomic<float> aligner_latency_ms{0.0f};
  // SetGate stamps (steady clock, µs) the gate going from closed to open; the
  // first frame processed open turns it into gate_open_delay_us.
  std::atomic<int64_t> gate_open_stamp_us{0};
  std::atomic<uint64_t> gate_open_delay_us{0};

  bool mic_muted() const;
  uint32_t push_to_talk() const;
  // Unset fields keep their value; push-to-talk values past kReleased close
  // the gate.
  void SetGate(std::optional<bool> mic_muted,
               std::optional<uint32_t> push_to_talk);
  // Back to the defaults (unity input volume), with the gate open or closed
  // (muted, push-to-talk key up) in the same store, and the DSP chain reset;
  // outputs are kept.
  void ResetControls(bool close_gate);

  // Any thread but the audio thread. take_maxima resets the block peak and
  // envelope maxima.
  RootDspStatsSnapshot ReadDspStats(bool take_maxima);
  RootSpeakingSnapshot ReadSpeaking() const;
};

struct RootCaptureFormatSnapshot {
  uint64_t seq = 0;
  uint32_t sample_rate = 0;
  uint32_t channels = 0;
};

// Reads (seq, rate, channels) consistently while the audio thread may be
// changing them.
RootCaptureFormatSnapshot ReadCaptureFormat(const RootCaptureState& state);

// The APM's capture post-processor. Its buffer is FloatS16 (+-32768) at the
// APM's full-band rate (8, 16, 32 or 48 kHz) with the output stream's channel
// count. It runs the voice chain (rootapp-voice-dsp) on channel 0 around the
// denoiser slot and copies the result to the other channels:
//   sanitise, voice gain level -> denoiser slot (Effects SDK) -> aligner ->
//   voice gain -> auto sensitivity -> startup hold -> noise gate -> input
//   volume (20 ms ramp) -> mute/PTT gate (5 ms fade each way).
// Real-time code: no locks, allocation, logging or APM calls. Initialize runs
// under both APM locks on up to four threads and only stores the format.
class RootCaptureProcessor : public webrtc::CustomProcessing {
 public:
  static constexpr int kMaxRate = 48000;
  // The denoiser's output capacity per frame (the app's wrapper: 8 frames).
  static constexpr size_t kDenoiseOutputFrames = 8;

  explicit RootCaptureProcessor(std::shared_ptr<RootCaptureState> state);

  void Initialize(int sample_rate_hz, int num_channels) override;
  void Process(webrtc::AudioBuffer* audio) override;
  std::string ToString() const override;
  void SetRuntimeSetting(
      webrtc::AudioProcessing::RuntimeSetting setting) override;

 private:
  void Publish(const RootDspOutputs& outputs,
               size_t frames,
               bool denoiser_wanted);

  const std::shared_ptr<RootCaptureState> state_;
  // Built at the largest rate here, never on the audio thread.
  rust::Box<RootDspChain> dsp_;
  // Audio thread only.
  int rate_ = 0;
  size_t channels_ = 0;
  int init_rate_ = 0;
  uint64_t seen_reset_seq_ = 0;
  uint64_t seen_stream_epoch_ = 0;
  uint64_t seen_gain_reset_seq_ = 0;
  bool stalled_ = false;
  bool published_speaking_ = false;
  std::array<float, kDenoiseOutputFrames * kMaxRate / 100> denoise_out_{};
};

struct RootApmStats {
  double erl_db = 0;
  double erle_db = 0;
  // -1 while the APM has no estimate.
  int32_t delay_ms = -1;
  double divergent_filter_fraction = 0;
  bool echo_cancellation_active = false;
  bool noise_suppression_active = false;
};

struct RootApmPolicyStats {
  uint64_t applies = 0;
  uint64_t deferred = 0;
  uint64_t forced = 0;
  bool pending = false;
  bool fallback_ns = false;
};

// Wraps the factory's APM. Forwards every AudioProcessing call, with three
// exceptions:
//  - ApplyConfig merges our policy into whatever the voice engine asks for
//    (AGC1 and AGC2 off, high-pass filter on, AEC and NS from the policy when
//    one is set) and calls the inner ApplyConfig only when the merged config
//    differs. GetConfig serves the engine's own request with only the forced
//    parts (AGC off, HPF on), so the engine's read-modify-write never copies
//    the AEC/NS policy into its request and ResetPolicy restores exactly what
//    it asked for; echo_cancellation_active/noise_suppression_active report
//    what is applied.
//  - set_output_will_be_muted and the kCaptureOutputUsed runtime setting are
//    swallowed, so the capture output is always used and the post-processor
//    runs every frame; mute and PTT are the post-processor's gate.
//  - Policy changes (SetPolicy) are applied from the scheduler's thread, at
//    most once per kMinPolicyIntervalMs, coalesced, at a pause in speech
//    (at most kSpeechPauseMaxMs late); toggling AEC or NS re-initializes
//    them. The denoiser fallback turning NS on is applied at once.
// Lock order: mutex_ -> APM render -> APM capture. No lock is added to the
// per-frame calls, and mutex_ is never taken on an audio thread.
class RootApmDecorator : public webrtc::AudioProcessing {
 public:
  static constexpr int64_t kMinPolicyIntervalMs = 1000;
  // A policy change waits for the published speaking flag to clear, polling
  // every kSpeechPausePollMs, at most kSpeechPauseMaxMs after it was asked.
  static constexpr int64_t kSpeechPauseMaxMs = 3000;
  static constexpr int64_t kSpeechPausePollMs = 50;

  using Clock = std::function<int64_t()>;
  using Scheduler =
      std::function<void(int64_t delay_ms, absl::AnyInvocable<void() &&>)>;

  // Without a scheduler policy changes are applied inline. The capture state
  // (may be null) gives the published speaking flag and receives what NS is
  // applied.
  RootApmDecorator(webrtc::scoped_refptr<webrtc::AudioProcessing> inner,
                   Scheduler scheduler,
                   Clock clock,
                   std::shared_ptr<RootCaptureState> capture_state = nullptr);
  ~RootApmDecorator() override;

  // Unset values leave that part of the policy as it is.
  void SetPolicy(std::optional<bool> echo_cancellation,
                 std::optional<bool> noise_suppression);
  // Back to following what the voice engine asks.
  void ResetPolicy();
  // The native denoiser fallback: on forces WebRTC NS on.
  void SetFallbackNoiseSuppression(bool on);
  RootApmPolicyStats ReadPolicyStats() const;

  // Off the audio threads. GetStatistics takes the APM's stats lock, which the
  // capture thread holds briefly once per frame (as for libwebrtc's own
  // getStats); everything else here is atomics.
  RootApmStats ReadStats();
  bool echo_cancellation_active() const;
  bool noise_suppression_active() const;
  uint64_t inner_apply_count() const;
  uint64_t swallowed_mute_count() const;

  // AudioProcessing.
  int Initialize() override;
  int Initialize(const webrtc::ProcessingConfig& processing_config) override;
  void ApplyConfig(const Config& config) override;
  int proc_sample_rate_hz() const override;
  int proc_split_sample_rate_hz() const override;
  size_t num_input_channels() const override;
  size_t num_proc_channels() const override;
  size_t num_output_channels() const override;
  size_t num_reverse_channels() const override;
  void set_output_will_be_muted(bool muted) override;
  bool get_output_will_be_muted() override;
  void SetRuntimeSetting(RuntimeSetting setting) override;
  bool PostRuntimeSetting(RuntimeSetting setting) override;
  int ProcessStream(const int16_t* const src,
                    const webrtc::StreamConfig& input_config,
                    const webrtc::StreamConfig& output_config,
                    int16_t* const dest) override;
  int ProcessStream(const float* const* src,
                    const webrtc::StreamConfig& input_config,
                    const webrtc::StreamConfig& output_config,
                    float* const* dest) override;
  int ProcessReverseStream(const int16_t* const src,
                           const webrtc::StreamConfig& input_config,
                           const webrtc::StreamConfig& output_config,
                           int16_t* const dest) override;
  int ProcessReverseStream(const float* const* src,
                           const webrtc::StreamConfig& input_config,
                           const webrtc::StreamConfig& output_config,
                           float* const* dest) override;
  int AnalyzeReverseStream(const float* const* data,
                           const webrtc::StreamConfig& reverse_config) override;
  bool GetLinearAecOutput(
      std::span<std::array<float, 160>> linear_output) const override;
  void set_stream_analog_level(int level) override;
  int recommended_stream_analog_level() const override;
  int set_stream_delay_ms(int delay) override;
  int stream_delay_ms() const override;
  void set_stream_key_pressed(bool key_pressed) override;
  bool CreateAndAttachAecDump(absl::string_view file_name,
                              int64_t max_log_size_bytes,
                              webrtc::TaskQueueBase* worker_queue) override;
  bool CreateAndAttachAecDump(FILE* handle,
                              int64_t max_log_size_bytes,
                              webrtc::TaskQueueBase* worker_queue) override;
  void AttachAecDump(std::unique_ptr<webrtc::AecDump> aec_dump) override;
  void DetachAecDump() override;
  webrtc::AudioProcessingStats GetStatistics() override;
  webrtc::AudioProcessingStats GetStatistics(bool has_remote_tracks) override;
  Config GetConfig() const override;

 private:
  // The parts of the engine's request we always override.
  static Config Forced(const Config& requested);
  Config MergeLocked(const Config& requested) const;
  void ApplyLocked();
  std::optional<bool> DesiredNoiseSuppressionLocked() const;
  bool PolicyPendingLocked() const;
  // The fallback needs WebRTC NS and the policy does not have it on yet.
  bool UrgentLocked() const;
  void SchedulePolicyLocked();
  void PostPolicyLocked(int64_t delay_ms);
  void ApplyPolicy(uint64_t generation);
  void ApplyPolicyNowLocked(int64_t now_ms, bool forced);

  const webrtc::scoped_refptr<webrtc::AudioProcessing> inner_;
  const Scheduler scheduler_;
  const Clock clock_;
  const std::shared_ptr<RootCaptureState> capture_;

  mutable webrtc::Mutex mutex_;
  Config requested_;
  Config applied_;
  std::optional<bool> desired_echo_cancellation_;
  std::optional<bool> desired_noise_suppression_;
  std::optional<bool> effective_echo_cancellation_;
  std::optional<bool> effective_noise_suppression_;
  bool fallback_noise_suppression_ = false;
  bool policy_apply_scheduled_ = false;
  bool policy_applied_once_ = false;
  int64_t last_policy_apply_ms_ = 0;
  // Only the newest posted application runs; older ones find another number.
  uint64_t policy_generation_ = 0;
  // When the desired policy first differed from the effective one.
  std::optional<int64_t> change_since_ms_;
  bool policy_waited_ = false;
  uint64_t policy_applies_ = 0;
  uint64_t policy_deferred_ = 0;
  uint64_t policy_forced_ = 0;

  std::atomic<bool> echo_cancellation_active_{false};
  std::atomic<bool> noise_suppression_active_{false};
  std::atomic<uint64_t> inner_applies_{0};
  std::atomic<uint64_t> swallowed_mutes_{0};
};

// The factory's APM: libwebrtc's builtin APM with RootCaptureProcessor as its
// capture post-processor, wrapped in RootApmDecorator. Policy changes are
// applied on `policy_queue` (the worker thread).
webrtc::scoped_refptr<RootApmDecorator> CreateRootAudioProcessing(
    const webrtc::Environment& env,
    std::shared_ptr<RootCaptureState> capture_state,
    webrtc::TaskQueueBase* policy_queue);

// Every field ApplyConfig acts on.
bool RootApmConfigsEqual(const webrtc::AudioProcessing::Config& a,
                         const webrtc::AudioProcessing::Config& b);

// Tests only: the decorator over a fake APM that reports fixed echo
// statistics (ERL 12 dB, ERLE 20 dB, delay 40 ms, divergent fraction 0.25),
// with policy changes applied inline.
webrtc::scoped_refptr<RootApmDecorator> NewRootApmOverFakeForTest();

// Runs a RootCaptureProcessor over AudioBuffers (tests only).
class RootProcessorHarness {
 public:
  RootProcessorHarness();
  ~RootProcessorHarness();

  void initialize(int32_t rate, int32_t channels) const;
  void set_mic_muted(bool muted) const;
  void set_push_to_talk(uint32_t state) const;
  // Both gate fields in one update; -1 keeps a field.
  void set_gate(int8_t mic_muted, int32_t push_to_talk) const;
  void set_input_volume(float volume) const;
  void reset_controls() const;
  void set_dsp(bool auto_gain,
               bool auto_sensitivity,
               float gate_threshold) const;
  void set_denoiser_wanted(bool wanted) const;
  void set_denoise_power(float power) const;
  void set_webrtc_ns_applied(bool applied) const;
  void bump_stream_epoch() const;
  void gain_reset(float db) const;
  // One frame through Process on a buffer of `rate` / 100 frames and
  // `channels` channels: channel 0 gets `input` (zero-padded), the others
  // something Process must overwrite. Returns channel 0.
  RootProcessorFrame process(int32_t rate,
                             size_t channels,
                             rust::Slice<const float> input) const;
  RootProcessorStats stats() const;
  RootProcessorDsp dsp(bool take_maxima) const;
  // A fake Effects SDK pipeline in the processor's denoiser slot.
  bool install_fake_denoiser(uint32_t rate,
                             float scale,
                             uint32_t delay_samples,
                             uint32_t burst_call,
                             uint32_t empty_from,
                             uint32_t empty_count) const;
  bool remove_fake_denoiser() const;
  RootFakeDenoiserProbe fake_denoiser() const;

 private:
  std::shared_ptr<RootCaptureState> state_;
  std::unique_ptr<RootCaptureProcessor> processor_;
  // Replaced pipelines are kept until the harness goes.
  mutable std::vector<std::unique_ptr<RootFakeFxPipeline>> fakes_;
};

std::unique_ptr<RootProcessorHarness> new_root_processor_harness();

class FakeApm;

// The decorator over a recording fake APM, or over the real builtin APM with
// the capture processor (tests only; no device).
class RootApmHarness {
 public:
  explicit RootApmHarness(bool real_apm);
  ~RootApmHarness();

  // As the voice engine does: GetConfig, change, ApplyConfig.
  void engine_apply(bool echo_cancellation,
                    bool noise_suppression,
                    bool auto_gain_control,
                    bool gain_controller2,
                    bool high_pass_filter) const;
  // GetConfig, change the echo canceller only, ApplyConfig.
  void engine_apply_echo_cancellation(bool echo_cancellation) const;
  void set_output_will_be_muted(bool muted) const;
  bool post_capture_output_used(bool used) const;
  void set_policy(int8_t echo_cancellation, int8_t noise_suppression) const;
  void reset_policy() const;
  void advance_clock(int64_t ms) const;
  // Runs the policy applications whose delay has passed; returns how many.
  uint32_t run_due_tasks() const;
  uint32_t pending_tasks() const;
  RootDecoratorProbe probe() const;
  // Real APM only: one 10 ms int16 frame of a 1 kHz tone of amplitude
  // `amplitude` through ProcessStream. Returns the output's peak.
  int16_t process_frame(int32_t rate, size_t channels, int16_t amplitude) const;
  void set_mic_muted(bool muted) const;
  void set_dsp(bool auto_gain,
               bool auto_sensitivity,
               float gate_threshold) const;
  uint64_t processed_frames() const;
  uint32_t processor_rate() const;
  uint32_t processor_channels() const;
  bool speaking() const;
  void set_speaking(bool speaking) const;
  void set_fallback_ns(bool on) const;
  RootPolicyProbe policy() const;

 private:
  struct Task {
    int64_t due_ms;
    absl::AnyInvocable<void() &&> task;
  };

  std::shared_ptr<int64_t> now_ms_;
  std::shared_ptr<std::vector<Task>> tasks_;
  webrtc::scoped_refptr<FakeApm> fake_;
  std::shared_ptr<RootCaptureState> capture_;
  webrtc::scoped_refptr<webrtc::AudioProcessing> inner_;
  webrtc::scoped_refptr<RootApmDecorator> decorator_;
};

std::unique_ptr<RootApmHarness> new_root_apm_harness(bool real_apm);

// The engine's ApplyConfig on one thread while another changes the policy
// (applied inline). True when every config the fake APM got had AGC off and
// the high-pass filter on, and the last one is the engine's last request with
// the last policy.
bool root_apm_decorator_race_for_test(uint32_t iterations);

}  // namespace livekit_ffi
