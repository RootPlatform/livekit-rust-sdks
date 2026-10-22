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
#include <chrono>
#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <deque>
#include <functional>
#include <memory>
#include <mutex>
#include <optional>
#include <string>
#include <thread>
#include <vector>

#include "rust/cxx.h"

namespace livekit_ffi {
struct RootDenoiserNotificationCounts;
#if defined(ROOTAPP_HARNESS)
struct RootFakeFxSpec;
struct RootFakeFxCounts;
struct RootDenoiseTestFrame;
struct RootDenoiserSlotInfo;
struct RootDenoiserTestCompletion;
struct RootDenoiserTestReturned;
struct RootDenoiserTestReturnAll;
class RootDenoiserHarness;
#endif
}  // namespace livekit_ffi
#include "webrtc-sys/src/root_denoiser.rs.h"

namespace livekit_ffi {

// The Effects SDK's IPipeline / INotification / INotificationHandler, called
// through their vtables. Single inheritance from IRelease and no virtual
// destructors: slot i is the i-th declared virtual (audio_effects_sdk
// pipeline.h, notification.h). x64/arm64 member calls are C calls with `this`
// first.
struct RootFxPipelineVtbl {
  void (*release)(void*);                                              // 0
  bool (*is_noise_suppression_enabled)(const void*);                   // 1
  void (*set_noise_suppression_enabled)(void*, bool);                  // 2
  float (*noise_suppression_power)(const void*);                       // 3
  void (*set_noise_suppression_power)(void*, float);                   // 4
  void (*set_latency_mode)(void*, int32_t);                            // 5
  int32_t (*latency_mode)(const void*);                                // 6
  void (*set_notification_handler)(void*, void*);                      // 7
  uint32_t (*process)(void*, const void*, uint32_t, void*, uint32_t);  // 8
  uint32_t (*flush)(void*, void*, uint32_t);                           // 9
};
struct RootFxPipeline {
  const RootFxPipelineVtbl* vtbl;
};
struct RootFxNotificationVtbl {
  int32_t (*code)(const void*);
};
struct RootFxNotification {
  const RootFxNotificationVtbl* vtbl;
};
struct RootFxNotificationHandlerVtbl {
  void (*release)(void*);
  void (*receive)(void*, void*);
};
struct RootFxNotificationHandler {
  const RootFxNotificationHandlerVtbl* vtbl;
};

// The SDK is called through raw vtables only where its x64/arm64 calling
// convention holds.
constexpr bool kRootDenoiserSupported = sizeof(void*) == 8;

// kSkipped: the capture processor did not hand the frame to the slot (its raw
// capture was digital silence); the slot never returns it.
enum class RootDenoiseKind : uint8_t {
  kNotInUse = 0,
  kMissed = 1,
  kRan = 2,
  kSkipped = 3,
};
enum class RootDenoiserBypass : uint32_t {
  kNone = 0,
  kRateMismatch = 1,
  kOutputStalled = 2,
  kSlow = 3,
};
struct RootDenoiseResult {
  RootDenoiseKind kind = RootDenoiseKind::kNotInUse;
  size_t samples = 0;
  bool restart = false;
};

struct RootDenoiserSlotStats {
  bool installed = false;
  uint32_t rate = 0;
  RootDenoiserBypass bypass = RootDenoiserBypass::kNone;
  uint64_t generation = 0;
  uint64_t frames = 0;
  uint64_t misses = 0;
  uint64_t empty_outputs = 0;
  uint64_t slow_frames = 0;
  uint64_t bypassed_frames = 0;
  uint64_t installs = 0;
  int32_t frames_until_first_output = -1;
  uint64_t process_max_us = 0;
  std::array<uint64_t, 16> histogram{};
};

// The Effects SDK pipeline the capture processor runs, shared between the
// audio thread and the control side by an ownership word (Free, Audio,
// Control). The audio thread only ever tries to take it and passes the frame
// dry when it cannot; the control side takes it with 1 ms sleeps for a bounded
// time. The slot never releases a pipeline: whoever removes one hands it back
// to the app.
class RootDenoiserSlot {
 public:
  static constexpr size_t kOutputFrames = 8;
  static constexpr int64_t kSlowFrameUs = 5000;
  static constexpr int64_t kOverFrameUs = 10000;
  static constexpr int64_t kStalledFrameUs = 20000;
  static constexpr size_t kSlowFramesToBypass = 3;
  static constexpr int64_t kSlowWindowUs = 60'000'000;
  static constexpr int kControlTimeoutMs = 200;
  static constexpr size_t kHistogramBuckets = 16;
  static constexpr std::array<int64_t, kHistogramBuckets - 1>
      kHistogramBoundsUs = {50,   100,  150,  200,  300,   400,   500,  750,
                            1000, 1500, 2000, 3000, 5000, 10000, 20000};

  RootDenoiserSlot();
  ~RootDenoiserSlot();
  RootDenoiserSlot(const RootDenoiserSlot&) = delete;
  RootDenoiserSlot& operator=(const RootDenoiserSlot&) = delete;

  // Audio thread only. Never blocks, allocates or logs. `in` is the FloatS16
  // frame (frames = rate / 100); `out` receives up to 8 x frames samples.
  // `flush` asks for flush(nullptr, 0) before the next process() (kept until
  // one runs; a new pipeline needs none). `power` is applied when it changed.
  // A rate-mismatch bypass ends, with a flush, once the frames fit again.
  // Flush, power and process are timed together (the maximum and the
  // histogram; slow_frames counts calls over kSlowFrameUs);
  // kSlowFramesToBypass plain calls longer than the 10 ms frame
  // (kOverFrameUs) within kSlowWindowUs, or one over kStalledFrameUs, latch
  // kSlow. A call that fits in the frame keeps up, however slow (0.9.153: 11
  // of the 14 callers bypassed at 5 ms had no call over 9.6 ms). A plain
  // call is one with no flush, power change or (re)start: those pay one-off
  // costs that say nothing about the SDK keeping up.
  RootDenoiseResult ProcessAudio(const float* in,
                                 size_t frames,
                                 int sample_rate,
                                 bool flush,
                                 float power,
                                 float* out,
                                 size_t out_capacity);
  // Audio thread: bypass the pipeline that last ran from the next frame on
  // (ignored when it was replaced meanwhile).
  void LatchBypass(RootDenoiserBypass reason);

  // Control side (never the audio thread). Each takes the slot with 1 ms
  // sleeps for up to `timeout_ms` and returns false, changing nothing, when
  // the audio thread did not let go. The pipeline it replaced or removed (may
  // be null) goes to `previous`.
  bool Install(RootFxPipeline* pipeline,
               uint32_t rate,
               int timeout_ms,
               RootFxPipeline** previous);
  bool Remove(int timeout_ms, RootFxPipeline** previous);

  // Any thread.
  bool installed() const;
  uint32_t rate() const;
  RootDenoiserBypass bypass() const;
  // `take_max` resets process_max_us.
  RootDenoiserSlotStats ReadStats(bool take_max);

#if defined(ROOTAPP_HARNESS)
  // Tests: install or remove at once (the replaced pipeline is dropped).
  bool InstallForTest(void* pipeline, uint32_t rate);
  void* RemoveForTest();
  // Tests: hold the slot as the audio thread would during process().
  bool HoldForTest(bool hold);
#endif

 private:
  static constexpr uint32_t kFree = 0;
  static constexpr uint32_t kAudio = 1;
  static constexpr uint32_t kControl = 2;

  bool AcquireControl(int timeout_ms);
  void Publish(RootFxPipeline* pipeline, uint32_t rate);
  void ApplyPendingBypass();
  void CountProcess(int64_t elapsed_us, size_t samples);

  std::atomic<uint32_t> owner_{kFree};
  // Written only while the control side owns the slot, read only while the
  // audio thread does.
  RootFxPipeline* pipeline_ = nullptr;
  uint32_t pipeline_rate_ = 0;
  uint64_t generation_ = 0;

  std::atomic<bool> installed_{false};
  std::atomic<uint32_t> published_rate_{0};
  std::atomic<uint64_t> published_generation_{0};
  std::atomic<uint32_t> bypass_{0};

  // Audio thread only.
  uint64_t seen_generation_ = 0;
  bool flush_pending_ = false;
  float applied_power_ = 0.0f;
  bool first_output_seen_ = false;
  int32_t calls_before_output_ = 0;
  RootDenoiserBypass pending_bypass_ = RootDenoiserBypass::kNone;
  uint64_t pending_bypass_generation_ = 0;
  std::array<int64_t, kSlowFramesToBypass> slow_times_us_{};
  size_t slow_count_ = 0;

  std::atomic<uint64_t> frames_{0};
  std::atomic<uint64_t> misses_{0};
  std::atomic<uint64_t> empty_outputs_{0};
  std::atomic<uint64_t> slow_frames_{0};
  std::atomic<uint64_t> bypassed_frames_{0};
  std::atomic<uint64_t> installs_{0};
  std::atomic<int32_t> frames_until_first_output_{-1};
  std::atomic<uint64_t> process_max_us_{0};
  std::array<std::atomic<uint64_t>, kHistogramBuckets> histogram_{};
};

// Native WebRTC NS covering a denoiser that is wanted but not running: on at
// once when the installed pipeline was bypassed for a stalled output or slow
// frames, on after `grace_ms` of frames when none runs for another reason
// (missing, removed, bypassed for its rate); off once it runs again or is not
// wanted. The grace only counts while frames flow (processed_frames moved in
// the last kFlowingMs) and no install is on its way (`arriving`: a request
// still waiting for a pause in speech, within its install wait), and starts
// over with each new stream.
class RootNsFallback {
 public:
  static constexpr int64_t kGraceMs = 1000;
  static constexpr int64_t kFlowingMs = 100;

  explicit RootNsFallback(int64_t grace_ms = kGraceMs);

  // The new state when it changes.
  std::optional<bool> Update(int64_t now_ms,
                             bool wanted,
                             bool installed,
                             RootDenoiserBypass bypass,
                             uint32_t pipeline_rate,
                             uint32_t process_rate,
                             uint64_t processed_frames,
                             uint64_t stream_epoch,
                             bool arriving);
  bool on() const { return on_; }
  void Reset();

 private:
  const int64_t grace_ms_;
  bool on_ = false;
  std::optional<int64_t> inactive_since_ms_;
  uint64_t frames_ = 0;
  std::optional<int64_t> frames_moved_ms_;
  uint64_t epoch_ = 0;
};

struct RootDenoiserCompletion {
  uint64_t owner = 0;
  uint64_t async_id = 0;
  // Empty on success.
  std::string error;
  // With an error, the request's pipeline when native let go of it and lists
  // it nowhere else (superseded, busy); 0 otherwise.
  uint64_t pipeline = 0;
};

struct RootDenoiserReturned {
  uint64_t owner = 0;
  uint64_t pipeline = 0;
};

struct RootDenoiserReturnAll {
  std::vector<uint64_t> pipelines;
  // Empty unless the installed pipeline could not be removed.
  std::string error;
};

// The control side of the slot: the `rootapp-effects` thread installs and
// removes pipelines (an install waits for a pause in the user's speech, at
// most install_wait_ms), runs the WebRTC NS fallback every tick, and keeps the
// completions and returned pipelines until the FFI layer takes them. Owners
// are the FFI handles that made the requests. The fallback is decided under
// mutex_ and applied (set_fallback, which can wait for an APM
// re-initialisation and a capture frame) outside it, latest decision last.
//
// A pipeline is named in an error completion, the returned list or a
// ReturnAll result only once native holds it nowhere (installed, being
// installed, waiting or queued), so a pipeline handed over twice is never
// given back while native may still call it: the request whose pipeline
// native still holds completes as a success instead.
class RootDenoiserControl {
 public:
  struct Hooks {
    // The user is audibly speaking (an install waits for a pause).
    std::function<bool()> speaking;
    std::function<bool()> wanted;
    // The capture processor's rate, 0 before any frame.
    std::function<uint32_t()> process_rate;
    // Frames the capture processor ran, and the recording (re)start count:
    // the missing-denoiser grace only counts while frames flow.
    std::function<uint64_t()> processed_frames;
    std::function<uint64_t()> stream_epoch;
    // The fallback was decided on or off (called under mutex_: never blocks).
    std::function<void(bool)> fallback_decided;
    // Turns the native WebRTC NS fallback on or off (never under mutex_).
    std::function<void(bool)> set_fallback;
  };
  struct Timing {
    int64_t install_wait_ms = 3000;
    int64_t fallback_grace_ms = RootNsFallback::kGraceMs;
    int64_t tick_ms = 10;
    int control_timeout_ms = RootDenoiserSlot::kControlTimeoutMs;
  };

  static constexpr char kErrorSuperseded[] =
      "superseded by a newer denoiser request";
  static constexpr char kErrorBusy[] = "the audio thread did not let go";
  static constexpr char kErrorReturned[] = "returned before install";
  static constexpr char kErrorSessionEnded[] =
      "the last platform audio handle was dropped before install";
  // The rates the voice chain runs at.
  static constexpr uint32_t kMinRate = 8000;
  static constexpr uint32_t kMaxRate = 48000;

  RootDenoiserControl(std::shared_ptr<RootDenoiserSlot> slot,
                      Hooks hooks,
                      Timing timing);
  ~RootDenoiserControl();
  RootDenoiserControl(const RootDenoiserControl&) = delete;
  RootDenoiserControl& operator=(const RootDenoiserControl&) = delete;

  // Queues an install (`pipeline` non-zero) or a removal; completes once. A
  // pipeline returned but not taken back yet is held again. An install at a
  // rate outside kMinRate..kMaxRate fails at once, unless native still holds
  // that pipeline (or has it on the returned list): then nothing changes and
  // it completes as a success.
  void Set(uint64_t owner, uint64_t async_id, uint64_t pipeline, uint32_t rate);
  // Something the fallback depends on changed (denoiser_wanted).
  void Wake();
  // Every pipeline held, waiting or not yet taken back: the requests still
  // waiting complete with kErrorReturned and the installed one is removed.
  // Waiting for a change in progress and for the audio thread share one
  // control_timeout_ms budget.
  RootDenoiserReturnAll ReturnAll();
  // The last session ended: the installed and waiting pipelines go to the
  // returned list and the fallback is off (same budget as ReturnAll).
  void Reset();
  std::vector<RootDenoiserCompletion> TakeCompletions();
  std::vector<RootDenoiserReturned> TakeReturned();
  bool fallback() const;
  size_t pending() const;

 private:
  struct Request {
    uint64_t seq = 0;
    uint64_t owner = 0;
    uint64_t async_id = 0;
    uint64_t pipeline = 0;
    uint32_t rate = 0;
    int64_t since_ms = 0;
  };

  void Run();
  void EnsureThreadLocked();
  // Takes mutex_ again before returning.
  void CommitLocked(std::unique_lock<std::mutex>& lock, uint64_t seq);
  // Whether the thread keeps ticking; `changed` when the fallback decision
  // changed (apply it after unlocking).
  bool TickLocked(int64_t now_ms, bool* changed);
  // An install request waits for a pause in speech, within its install wait.
  bool ArrivingLocked(int64_t now_ms) const;
  void DecideFallbackLocked(bool on);
  // Without mutex_: hands the latest decision to set_fallback.
  void ApplyFallback();
  // Native holds `pipeline` (installed, being installed, waiting or queued).
  bool HeldLocked(uint64_t pipeline) const;
  void CompleteLocked(const Request& request,
                      std::string error,
                      uint64_t released_pipeline);
  // A request that ends without installing its pipeline.
  void FailLocked(const Request& request, const char* error);
  void ReturnLocked(uint64_t owner, uint64_t pipeline);
  void DrainRequestsLocked(const char* error, std::vector<uint64_t>* into);
  // Removes the installed pipeline within the shared budget (slot_mutex_ and
  // the audio thread); false when it stays installed.
  bool RemoveWithin(std::chrono::steady_clock::time_point deadline,
                    uint64_t* previous);

  const std::shared_ptr<RootDenoiserSlot> slot_;
  const Hooks hooks_;
  const Timing timing_;

  // Serialises the control side's slot changes; taken before mutex_.
  std::timed_mutex slot_mutex_;
  mutable std::mutex mutex_;
  std::condition_variable wake_;
  std::deque<Request> queue_;
  std::optional<Request> waiting_;
  uint64_t next_seq_ = 0;
  // What the control side installed, and the pipeline it is installing.
  uint64_t installed_pipeline_ = 0;
  uint64_t in_flight_pipeline_ = 0;
  uint64_t installed_owner_ = 0;
  std::vector<RootDenoiserCompletion> completions_;
  std::vector<RootDenoiserReturned> returned_;
  RootNsFallback fallback_;
  // The decision (written under mutex_).
  std::atomic<bool> fallback_on_{false};
  // Serialises set_fallback calls; what was last handed to it.
  std::mutex fallback_mutex_;
  bool fallback_applied_ = false;
  bool stop_ = false;
  std::thread thread_;
};

// The native INotificationHandler the app installs on every pipeline it hands
// over: static, so a notification that arrives after its pipeline is gone is
// still safe. It only counts.
RootFxNotificationHandler* RootDenoiserNotificationHandler();
// slowProcessing notifications (code 1) and the others, process-wide.
RootDenoiserNotificationCounts root_denoiser_notification_counts();
// The handler's address as an integer, 0 where the denoiser is not built.
size_t root_denoiser_notification_handler();
bool root_denoiser_supported();

#if defined(ROOTAPP_HARNESS)
// Tests: one fake INotification per code through the handler's vtable, and a
// null notification first with `include_null`.
void root_denoiser_notify_for_test(rust::Slice<const int32_t> codes,
                                   bool include_null);

// Tests: a C++ object with the SDK's vtable layout. Output = round(scale x
// input) delayed by delay_samples (a FIFO that starts, and restarts on a flush,
// with that many zeros). Call numbers count from 1 after construction or a
// flush; burst_call pops two frames' worth, calls [empty_from, empty_from +
// empty_count) return nothing. Process, flush and the power setter each
// busy-wait sleep_us.
struct RootFakeFxConfig {
  float scale = 1.0f;
  uint32_t delay_samples = 0;
  uint32_t burst_call = 0;
  uint32_t empty_from = 0;
  uint32_t empty_count = 0;
  uint32_t sleep_us = 0;
};

class RootFakeFxPipeline {
 public:
  static constexpr size_t kCapacity = 16384;

  explicit RootFakeFxPipeline(const RootFakeFxConfig& config);
  ~RootFakeFxPipeline();
  RootFakeFxPipeline(const RootFakeFxPipeline&) = delete;
  RootFakeFxPipeline& operator=(const RootFakeFxPipeline&) = delete;

  void* pipeline();
  void set_sleep_us(uint32_t sleep_us);
  // Every output from now on is empty.
  void set_empty(bool empty);
  // From now on any call is a use after return (counted).
  void Poison();

  uint64_t process_calls() const;
  // Process calls whose input was all zeros.
  uint64_t silent_calls() const;
  uint64_t flush_calls() const;
  uint64_t power_sets() const;
  float last_power() const;
  uint64_t calls_after_poison() const;
  uint64_t releases() const;

 private:
  struct Object {
    const RootFxPipelineVtbl* vtbl;
    RootFakeFxPipeline* self;
  };
  static RootFakeFxPipeline* Self(const void* object);
  static void Release(void* object);
  static bool IsEnabled(const void* object);
  static void SetEnabled(void* object, bool enabled);
  static float Power(const void* object);
  static void SetPower(void* object, float power);
  static void SetLatencyMode(void* object, int32_t mode);
  static int32_t LatencyMode(const void* object);
  static void SetNotificationHandler(void* object, void* handler);
  static uint32_t Process(void* object,
                          const void* in,
                          uint32_t in_frames,
                          void* out,
                          uint32_t out_frames);
  static uint32_t Flush(void* object, void* out, uint32_t out_frames);
  static const RootFxPipelineVtbl kVtbl;

  void Touch();
  // Busy-waits sleep_us (process, flush and the power setter).
  void Spin() const;
  void Restart();

  Object object_;
  const RootFakeFxConfig config_;
  std::unique_ptr<float[]> fifo_;
  size_t head_ = 0;
  size_t size_ = 0;
  uint32_t calls_since_restart_ = 0;
  std::atomic<uint32_t> sleep_us_;
  std::atomic<bool> empty_{false};
  std::atomic<bool> poisoned_{false};
  std::atomic<uint64_t> process_calls_{0};
  std::atomic<uint64_t> silent_calls_{0};
  std::atomic<uint64_t> flush_calls_{0};
  std::atomic<uint64_t> power_sets_{0};
  std::atomic<float> power_{1.0f};
  std::atomic<uint64_t> calls_after_poison_{0};
  std::atomic<uint64_t> releases_{0};
};

// Tests: a slot with fake pipelines, an optional thread calling ProcessAudio
// as the capture thread would, and the control side over hooks the test sets.
class RootDenoiserHarness {
 public:
  RootDenoiserHarness(int64_t install_wait_ms, int64_t fallback_grace_ms);
  ~RootDenoiserHarness();

  size_t add_fake(const RootFakeFxSpec& spec) const;
  uint64_t fake_pipeline(size_t index) const;
  RootFakeFxCounts fake_counts(size_t index) const;
  void poison_fake(size_t index) const;
  void set_fake_sleep_us(size_t index, uint32_t sleep_us) const;
  void set_fake_empty(size_t index, bool empty) const;

  bool install_now(size_t index, uint32_t rate) const;
  uint64_t remove_now() const;
  // Install with the control side's timeout; returns the replaced pipeline,
  // or u64::MAX when the audio thread did not let go.
  uint64_t swap(uint64_t pipeline, uint32_t rate) const;
  bool hold_slot(bool hold) const;
  RootDenoiseTestFrame process(uint32_t rate,
                               rust::Slice<const float> input,
                               bool flush,
                               float power,
                               size_t out_capacity) const;
  void latch_bypass(uint32_t reason) const;
  RootDenoiserSlotInfo slot_info(bool take_max) const;

  // A thread calling ProcessAudio back to back on a tone at `rate`, and
  // LatchBypass(kOutputStalled) after every `latch_every`-th frame (0 never),
  // as the capture processor does for a stalled output.
  void start_audio(uint32_t rate, uint32_t latch_every) const;
  // Stops it; returns how many frames it ran.
  uint64_t stop_audio() const;

  void set_speaking(bool speaking) const;
  void set_wanted(bool wanted) const;
  void set_process_rate(uint32_t rate) const;
  // Whether the processed-frames hook moves (it does from the start).
  void set_frames_flowing(bool flowing) const;
  void bump_stream_epoch() const;
  void request(uint64_t owner,
               uint64_t async_id,
               uint64_t pipeline,
               uint32_t rate) const;
  void wake() const;
  RootDenoiserTestReturnAll return_all() const;
  void reset() const;
  rust::Vec<RootDenoiserTestCompletion> take_completions() const;
  rust::Vec<RootDenoiserTestReturned> take_returned() const;
  // Every fallback change the control side made since the last take.
  rust::Vec<uint8_t> take_fallback_changes() const;
  bool fallback() const;
  // What the fallback_decided hook last stored.
  bool fallback_decided() const;
  // While set, set_fallback waits (an APM re-initialisation that hangs).
  void block_fallback(bool block) const;
  size_t pending() const;

  // The fallback state machine alone: -1 no change, 0 off, 1 on.
  int8_t fallback_update(int64_t now_ms,
                         bool wanted,
                         bool installed,
                         uint32_t bypass,
                         uint32_t pipeline_rate,
                         uint32_t process_rate,
                         uint64_t processed_frames,
                         uint64_t stream_epoch,
                         bool arriving) const;

 private:
  struct State;
  std::unique_ptr<State> state_;
};

std::unique_ptr<RootDenoiserHarness> new_root_denoiser_harness(
    int64_t install_wait_ms,
    int64_t fallback_grace_ms);
#endif

}  // namespace livekit_ffi
