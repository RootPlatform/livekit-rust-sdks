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
#include <cstdint>
#include <functional>
#include <limits>
#include <memory>
#include <optional>
#include <span>
#include <string>
#include <string_view>
#include <thread>
#include <vector>

#include "absl/functional/any_invocable.h"
#include "api/audio/neural_residual_echo_estimator.h"
#include "api/scoped_refptr.h"
#include "rtc_base/synchronization/mutex.h"
#include "rust/cxx.h"

namespace livekit_ffi {
struct RootEchoModelSession;
struct RootEchoPolicyProbe;
struct RootEchoWindowProbe;
struct RootEchoHarnessProbe;
struct RootEchoStressProbe;
struct RootEchoGraceProbe;
class RootEchoHarness;
}  // namespace livekit_ffi
#include "webrtc-sys/src/root_echo_model.rs.h"

namespace livekit_ffi {

class RootApmDecorator;
struct RootCaptureState;

// The echo model (DTLN-aec-128 as AEC3's neural residual echo estimator) runs
// where the factory builds its APM (the DSP chain's condition).
#if defined(WEBRTC_IOS) || defined(__ANDROID__)
constexpr bool kRootEchoModelSupported = false;
#else
constexpr bool kRootEchoModelSupported = true;
#endif
bool root_echo_model_supported();

// Wire values of RootAudioEchoModel and friends (rootapp_audio.proto).
enum class RootEchoModelMode : uint32_t { kOff = 0, kOn = 1, kAuto = 2 };
enum class RootEchoModelState : uint32_t {
  kOff = 0,
  kOn = 1,
  kStarting = 2,
  kStopping = 3,
};
enum class RootEchoModelReason : uint32_t {
  kModeOff = 0,
  kModeOn = 1,
  kDeviceKind = 2,
  kMeasuredEcho = 3,
  kKillSwitch = 4,
  kUnavailable = 5,
};
enum class RootOutputDeviceKind : uint32_t {
  kNone = 0,
  kUnknown = 1,
  kSpeakers = 2,
  kLineOut = 3,
  kDisplayAudio = 4,
  kHeadphones = 5,
  kHeadset = 6,
  kHandset = 7,
  kBluetooth = 8,
};

RootEchoModelMode RootEchoModelModeFromWire(uint32_t value);
bool RootOutputKindIsLoudspeaker(RootOutputDeviceKind kind);

// The kind from what the OS says, as pure functions (tests feed fake values).
// Windows: EndpointFormFactor of the endpoint's property store, and whether
// one of its device's ancestors in the device tree is a Bluetooth enumerator.
RootOutputDeviceKind RootOutputKindFromWindows(uint32_t form_factor,
                                               bool bluetooth);
// A device instance id under a Bluetooth enumerator (BTHENUM, BTHHFENUM,
// BTHLEDEVICE, BTHLE...).
bool RootIsBluetoothInstanceId(std::string_view instance_id);
// macOS: the transport type, then for built-in devices the output data
// source and otherwise the first output stream's terminal type (FourCCs).
RootOutputDeviceKind RootOutputKindFromMac(uint32_t transport,
                                           std::optional<uint32_t> data_source,
                                           std::optional<uint32_t> terminal);

// The platform playout device AdmProxy opened: the OS default or the ADM's
// GUID/name of the selected device.
struct RootPlayoutIdentity {
  bool system_default = true;
  std::string guid;
  std::string name;
};

struct RootOutputDevice {
  // Tells devices apart for the measured-echo latch.
  std::string key;
  RootOutputDeviceKind kind = RootOutputDeviceKind::kNone;
};

// Read-only OS metadata queries (Windows: on a COM MTA thread, the AdmProxy
// device thread). Never activates an endpoint or changes anything. Linux and
// other systems: kUnknown.
RootOutputDevice ClassifyOutputDevice(const RootPlayoutIdentity& identity);

// What the estimators of one engine publish (written on the capture thread,
// relaxed atomics). The mean is read from two counters, so one read can be
// off by an inference.
struct RootEchoModelCounters {
  std::atomic<uint64_t> inferences{0};
  std::atomic<uint64_t> inference_us_total{0};
  // Since the controller's last read (it exchanges it).
  std::atomic<uint64_t> inference_max_us{0};
  std::atomic<float> inference_p99_us{std::numeric_limits<float>::quiet_NaN()};
  // Set by an estimator, cleared by a session reset.
  std::atomic<bool> kill_switch{false};
  // Moved by a session reset: the windows start over.
  std::atomic<uint64_t> session{0};
};

// The CPU kill switch's measurement: windows of kWindow inferences after the
// first kWarmup of an estimator; a window fails when more than kAllowedSlow of
// its inferences took over kSlowUs (p99 over 2 ms); kFailingWindows failing
// windows in a row trip the switch. A new session (counters.session) starts
// the windows and the streak over. Capture thread only.
class RootInferenceWindow {
 public:
  static constexpr uint32_t kWindow = 1000;
  static constexpr uint32_t kWarmup = 100;
  static constexpr uint64_t kSlowUs = 2000;
  static constexpr uint32_t kAllowedSlow = kWindow / 100;
  static constexpr uint32_t kFailingWindows = 2;
  static constexpr size_t kBuckets = 32;
  // Upper bounds (µs) of the histogram the p99 is read from; the last one is
  // open.
  static const std::array<uint32_t, kBuckets - 1> kBucketBounds;

  // One inference's wall time. Publishes each complete window's p99 and sets
  // counters.kill_switch when the switch trips.
  void Add(uint64_t us, RootEchoModelCounters& counters);
  uint32_t failing_streak() const { return failing_streak_; }
  float last_p99_us() const { return last_p99_us_; }

 private:
  void StartOver();

  uint64_t session_ = 0;
  uint32_t warmup_ = kWarmup;
  uint32_t count_ = 0;
  uint32_t slow_ = 0;
  uint32_t failing_streak_ = 0;
  uint64_t max_us_ = 0;
  float last_p99_us_ = std::numeric_limits<float>::quiet_NaN();
  std::array<uint32_t, kBuckets> histogram_{};
};

// Whether the far end is active, for the estimator (estimator-spec.md 3.2):
// the APM's reverse stream, which leads the echo, rather than the render
// block AEC3 hands the hook. The render paths store when (µs) the last frame
// above -60 dBFS (mean square over its channels) ran; the far end is active
// while a capture frame comes at most `hold_us` after it. Hold and onset
// guard follow the render-to-capture delay D. Shared by both APM variants of
// a decorator; atomics only.
struct RootEchoFarEnd {
  static constexpr int64_t kMinHoldMs = 100;
  static constexpr int64_t kHoldAfterDelayMs = 60;
  static constexpr int64_t kMinGuardMs = 60;
  static constexpr int64_t kGuardAfterDelayMs = 40;
  static constexpr int64_t kBlockMs = 4;
  // Mean square of a frame in -1..1 (and in int16 units) above -60 dBFS.
  static constexpr double kActivePower = 1e-6;
  static constexpr double kActivePowerInt16 = 1e-6 * 32768.0 * 32768.0;

  std::atomic<int64_t> last_active_render_us{
      std::numeric_limits<int64_t>::min() / 2};
  std::atomic<int64_t> hold_us{kMinHoldMs * 1000};
  std::atomic<uint32_t> onset_guard_blocks{
      static_cast<uint32_t>((kMinGuardMs + kBlockMs - 1) / kBlockMs)};
  std::atomic<int32_t> delay_ms{0};

  // Hold max(100 ms, D + 60 ms), onset guard max(60 ms, D + 40 ms) in 4 ms
  // blocks rounded up; a negative D counts as 0.
  void SetDelayMs(int32_t delay_ms);
  bool Active(int64_t capture_now_us) const;
};

// The estimator AEC3 calls every 4 ms block (capture thread):
// estimator-spec.md through the Rust session (rootapp-echo-model's
// Estimator) on render, capture and linear output channel 0 and the spectra
// of every capture channel, the far end's activity read from `far_end` at the
// capture frame's time (`capture_now_us`, stored by the decorator before each
// frame). Estimate never allocates, locks or logs.
class RootResidualEchoEstimator : public webrtc::NeuralResidualEchoEstimator {
 public:
  RootResidualEchoEstimator(
      rust::Box<RootEchoModelSession> session,
      std::shared_ptr<RootEchoModelCounters> counters,
      std::shared_ptr<const RootEchoFarEnd> far_end,
      std::shared_ptr<const std::atomic<int64_t>> capture_now_us);
  ~RootResidualEchoEstimator() override;

  void Estimate(const webrtc::Block& render,
                std::span<const std::array<float, 64>> y,
                std::span<const std::array<float, 64>> e,
                std::span<const std::array<float, 65>> S2,
                std::span<const std::array<float, 65>> Y2,
                std::span<const std::array<float, 65>> E2,
                bool dominant_nearend,
                std::span<std::array<float, 65>> R2,
                std::span<std::array<float, 65>> R2_unbounded) override;
  // Google's tuning; first hands the config's own enr_suppress per state to
  // the session (the floor's scale). AEC3 calls it once per EchoRemover,
  // before any Estimate on it.
  webrtc::EchoCanceller3Config::Suppressor AdjustConfig(
      const webrtc::EchoCanceller3Config::Suppressor& config) const override;
  void Reset() override;

  // Google's tuning for its neural estimator, which the research measured the
  // model with.
  static webrtc::EchoCanceller3Config::Suppressor Tuned(
      const webrtc::EchoCanceller3Config::Suppressor& config);

 private:
  mutable rust::Box<RootEchoModelSession> session_;
  const std::shared_ptr<RootEchoModelCounters> counters_;
  const std::shared_ptr<const RootEchoFarEnd> far_end_;
  const std::shared_ptr<const std::atomic<int64_t>> capture_now_us_;
  RootInferenceWindow window_;
};

// A new estimator over a fresh model session; null until the model is
// prepared (root_echo_model_prepare), so never call it before. Without
// `far_end` the far end is never active.
std::unique_ptr<webrtc::NeuralResidualEchoEstimator> NewRootResidualEchoEstimator(
    std::shared_ptr<RootEchoModelCounters> counters,
    std::shared_ptr<const RootEchoFarEnd> far_end,
    std::shared_ptr<const std::atomic<int64_t>> capture_now_us);

// The decisions, without threads or clocks (RootEchoModelController feeds
// it). The measured-echo rule samples AEC3's fullband ERLE every
// kSampleIntervalMs while AEC3 runs alone in AUTO: a sample counts only after
// kMinFramesPerSample processed capture frames since the previous one, is
// evidence at kEvidenceErleDb or more, and kEvidenceSamples of the last
// kWindowSamples latch the model on for the current device until the session
// resets. The latch survives switching to another device and back; a device
// without a key is never measured (nothing tells it from others). Calibrated
// on the AEC Challenge's real recordings (ROOTAPP-BUILD.md): without an echo
// path AEC3's ERLE stays under 0.5 dB.
class RootEchoModelPolicy {
 public:
  static constexpr int64_t kSampleIntervalMs = 250;
  static constexpr uint64_t kMinFramesPerSample = 20;
  static constexpr double kEvidenceErleDb = 3.0;
  static constexpr size_t kWindowSamples = 40;
  static constexpr size_t kEvidenceSamples = 8;
  static constexpr size_t kMaxLatchedDevices = 8;

  struct Decision {
    bool on = false;
    RootEchoModelReason reason = RootEchoModelReason::kModeOff;
  };

  void SetMode(RootEchoModelMode mode) { mode_ = mode; }
  // A playout open; a new key or kind starts the measurement over.
  void SetDevice(RootOutputDevice device);
  void SetKillSwitch() { kill_switch_ = true; }
  void SetUnavailable(bool unavailable) { unavailable_ = unavailable; }
  // Mode off, latches, measurement and kill switch cleared; the device kept.
  void ResetSession();
  // Back to no device.
  void ForgetDevice();
  // The measurement starts over (a swap).
  void RestartMeasurement();
  // Whether the sample latched the current device.
  bool AddErleSample(double erle_db, uint64_t processed_frames);
  // Whether a sample would be taken now: AUTO, a device with a key and no
  // latch, no kill switch.
  bool Measuring() const;
  Decision Decide() const;

  RootEchoModelMode mode() const { return mode_; }
  const RootOutputDevice& device() const { return device_; }
  bool kill_switch() const { return kill_switch_; }
  bool measured_echo() const;
  size_t evidence() const;
  size_t counted() const { return counted_; }

 private:
  RootEchoModelMode mode_ = RootEchoModelMode::kOff;
  RootOutputDevice device_;
  bool kill_switch_ = false;
  bool unavailable_ = false;
  std::vector<std::string> latched_;
  std::array<bool, kWindowSamples> window_{};
  size_t next_ = 0;
  size_t counted_ = 0;
  std::optional<uint64_t> last_frames_;
};

struct RootEchoModelStatsSnapshot {
  RootEchoModelMode mode = RootEchoModelMode::kOff;
  RootEchoModelState state = RootEchoModelState::kOff;
  RootEchoModelReason reason = RootEchoModelReason::kModeOff;
  RootOutputDeviceKind kind = RootOutputDeviceKind::kNone;
  uint64_t swaps = 0;
  uint64_t inferences = 0;
  float inference_mean_us = std::numeric_limits<float>::quiet_NaN();
  uint64_t inference_max_us = 0;
  float inference_p99_us = std::numeric_limits<float>::quiet_NaN();
  bool measured_echo = false;
  bool kill_switch = false;
};

// Owns the decisions of one engine and drives the decorator's variant switch
// from the scheduler's thread (the worker thread): every posted decision, and
// a poll every kPollMs (kSwapPollMs while a switch is under way) while the
// mode is not OFF or the model APM still runs. Mode, device and reset come
// from other threads and post a decision; the audio threads only write the
// counters. A switch waits for a pause in the user's speech, at most
// kSpeechPauseMaxMs; with no audio running the scheduler's thread makes it.
// The model is prepared the first time a decision wants it, and is off
// (UNAVAILABLE) while the APM's echo canceller is: it has nothing to run on.
class RootEchoModelController {
 public:
  using Clock = std::function<int64_t()>;
  using Scheduler =
      std::function<void(int64_t delay_ms, absl::AnyInvocable<void() &&>)>;
  // Prepares the model off the audio threads (blocking); true when ready.
  using Prepare = std::function<bool()>;

  static constexpr int64_t kPollMs = 250;
  static constexpr int64_t kSwapPollMs = 20;
  static constexpr int64_t kSpeechPauseMaxMs = 3000;

  RootEchoModelController(RootApmDecorator* decorator,
                          Scheduler scheduler,
                          Clock clock,
                          std::shared_ptr<RootCaptureState> capture_state,
                          Prepare prepare);
  ~RootEchoModelController();

  void SetMode(RootEchoModelMode mode);
  // AdmProxy's device thread, after a playout open: classifies and decides.
  void OnPlayoutOpened(const RootPlayoutIdentity& identity);
  // The same with a classification already made (tests, other platforms).
  void OnOutputDevice(RootOutputDevice device);
  // The platform playout device closed.
  void OnPlayoutClosed();
  // The device is kept only while a platform playout is open (a call's open
  // classifies again).
  void ResetSession();
  // Any thread but the audio threads; takes the since-last-read values.
  RootEchoModelStatsSnapshot ReadStats();

  const std::shared_ptr<RootEchoModelCounters>& counters() const {
    return counters_;
  }
  // The scheduler's thread: one decision and poll now (tests drive it).
  void Run();
  // The measured-echo evidence in the current window (tests).
  uint32_t evidence() const {
    return evidence_.load(std::memory_order_relaxed);
  }

 private:
  // Replaces any scheduled run: only the newest one runs.
  void Schedule(int64_t delay_ms);
  void RunGeneration(uint64_t generation);
  void RunLocked(int64_t now_ms);
  void StartPrepareLocked();
  RootEchoModelPolicy::Decision DecideLocked() const;

  RootApmDecorator* const decorator_;
  const Scheduler scheduler_;
  const Clock clock_;
  const std::shared_ptr<RootCaptureState> capture_;
  const Prepare prepare_;
  const std::shared_ptr<RootEchoModelCounters> counters_ =
      std::make_shared<RootEchoModelCounters>();

  webrtc::Mutex mutex_;
  RootEchoModelPolicy policy_;
  bool playout_open_ = false;
  bool prepare_started_ = false;
  std::atomic<bool> prepared_{false};
  std::atomic<bool> prepare_failed_{false};
  std::thread prepare_thread_;
  uint64_t generation_ = 0;
  uint64_t seen_swaps_ = 0;
  int64_t last_sample_ms_ = std::numeric_limits<int64_t>::min() / 2;
  std::optional<int64_t> change_since_ms_;
  std::atomic<bool> target_on_{false};
  std::atomic<uint32_t> reason_{
      static_cast<uint32_t>(RootEchoModelReason::kModeOff)};
  std::atomic<uint32_t> mode_{static_cast<uint32_t>(RootEchoModelMode::kOff)};
  std::atomic<uint32_t> kind_{static_cast<uint32_t>(RootOutputDeviceKind::kNone)};
  std::atomic<bool> measured_{false};
  std::atomic<uint32_t> evidence_{0};
  webrtc::Mutex read_mutex_;
  uint64_t read_inferences_ = 0;
  uint64_t read_us_total_ = 0;
};

// Test hooks (root_echo_model_tests.rs).
uint32_t root_output_kind_from_windows(uint32_t form_factor, bool bluetooth);
bool root_is_bluetooth_instance_id(rust::Str instance_id);
uint32_t root_output_kind_from_mac(uint32_t transport,
                                   bool has_data_source,
                                   uint32_t data_source,
                                   bool has_terminal,
                                   uint32_t terminal);
// The policy over a script: mode, kind, then ERLE samples (dB) with the
// processed-frame counter advancing `frames_per_sample` each; returns the
// decision and the sample that latched (-1 none).
RootEchoPolicyProbe root_echo_policy_probe(uint32_t mode,
                                           uint32_t kind,
                                           rust::Slice<const double> erle_db,
                                           uint64_t frames_per_sample);
// The kill switch over a script of inference durations (µs), with a session
// reset before inference `reset_at` (-1: none).
RootEchoWindowProbe root_echo_window_probe(rust::Slice<const uint64_t> us,
                                           int64_t reset_at);
// AdjustConfig over the default config: the masks and controls it sets.
bool root_echo_adjust_config_is_googles_tuning();

// The factory's APM over real libwebrtc APMs with a test clock and scheduler
// (tests only; no device). `today` builds it as before the echo model (the
// processor directly in the APM, no variant builder, no controller).
class RootEchoHarness {
 public:
  explicit RootEchoHarness(bool today);
  ~RootEchoHarness();

  // One 10 ms frame: the render (`render_channels` channels, one after the
  // other) through ProcessReverseStream, the stream delay, then the mono
  // capture through ProcessStream; returns the capture output. Floats in
  // int16 scale. Without capture samples only the render runs.
  rust::Vec<float> process(int32_t rate,
                           uint32_t render_channels,
                           rust::Slice<const float> render,
                           rust::Slice<const float> capture) const;
  // The same through the int16 interface the voice engine uses (interleaved,
  // in place); samples are rounded and clamped to int16.
  rust::Vec<float> process_int16(int32_t rate,
                                 uint32_t render_channels,
                                 rust::Slice<const float> render,
                                 rust::Slice<const float> capture) const;
  // Initializes the APM for process's formats, as a staged variant is (a
  // first call initializes for its own direction only, and the other
  // direction's first call again).
  void initialize(int32_t rate, uint32_t render_channels) const;
  // The stream delay process and process_int16 set before each capture
  // frame (0 until set).
  void set_stream_delay(int32_t delay_ms) const;
  // The DSP chain passes the APM output on (no voice gain, gate open).
  void open_dsp_chain() const;
  // AGC2 as the voice gain from gain_reset_db, built as the controller does.
  void use_agc2(float gain_reset_db) const;
  void prepare_model() const;
  void set_mode(uint32_t mode) const;
  // A playout open classified as `kind`.
  void set_device(uint32_t kind, rust::Str key) const;
  void close_playout() const;
  // The host's echo cancellation policy (applied at a pause, as in calls).
  void set_echo_cancellation(bool on) const;
  void reset_session() const;
  void trip_kill_switch() const;
  void set_speaking(bool speaking) const;
  // Moves the clock on in 10 ms steps, running the tasks that come due.
  void advance(int64_t ms) const;
  RootEchoHarnessProbe probe() const;
  // A render and a capture thread run frames, and a third reads statistics,
  // while this thread switches the variant `switches` times (stage, then
  // collect); every APM a switch replaced is released.
  RootEchoStressProbe stress(uint32_t switches) const;
  // The grace period against a reader stalled between reading the epoch and
  // counting itself, across two switches (no audio may have run): the second
  // switch's APM is released only after the reader leaves.
  RootEchoGraceProbe grace_replay() const;

 private:
  struct Task {
    int64_t due_ms;
    absl::AnyInvocable<void() &&> task;
  };
  void RunDue() const;

  std::shared_ptr<int64_t> now_ms_;
  mutable int stream_delay_ms_ = 0;
  std::shared_ptr<std::vector<Task>> tasks_;
  std::shared_ptr<RootCaptureState> capture_;
  webrtc::scoped_refptr<RootApmDecorator> decorator_;
};

std::unique_ptr<RootEchoHarness> new_root_echo_harness(bool today);

}  // namespace livekit_ffi
