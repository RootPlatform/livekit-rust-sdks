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
#include <memory>

#include "rust/cxx.h"

namespace livekit_ffi {
struct RootDenoiserNotificationCounts;
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

enum class RootDenoiseKind : uint8_t { kNotInUse = 0, kMissed = 1, kRan = 2 };
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
// dry when it cannot; the control side takes it with bounded 1 ms sleeps. The
// slot never releases a pipeline: whoever removes one hands it back to the app.
class RootDenoiserSlot {
 public:
  static constexpr size_t kOutputFrames = 8;
  static constexpr uint32_t kSlowFrameUs = 5000;
  static constexpr uint32_t kStalledFrameUs = 20000;
  static constexpr int kSlowFramesToBypass = 3;
  static constexpr int64_t kSlowWindowUs = 60'000'000;
  static constexpr int kControlTries = 200;
  static constexpr size_t kHistogramBuckets = 16;
  static constexpr std::array<uint32_t, kHistogramBuckets - 1>
      kHistogramBoundsUs = {50,   100,  150,  200,  300,   400,   500,  750,
                            1000, 1500, 2000, 3000, 5000, 10000, 20000};

  RootDenoiserSlot();
  ~RootDenoiserSlot();
  RootDenoiserSlot(const RootDenoiserSlot&) = delete;
  RootDenoiserSlot& operator=(const RootDenoiserSlot&) = delete;

  // Audio thread only. Never blocks, allocates or logs. `in` is the FloatS16
  // frame (frames = rate / 100); `out` receives up to 8 x frames samples.
  // `flush` asks for flush(nullptr, 0) before the next process() (kept until
  // one runs).
  RootDenoiseResult ProcessAudio(const float* in,
                                 size_t frames,
                                 int sample_rate,
                                 bool flush,
                                 float power,
                                 float* out,
                                 size_t out_capacity);
  // Audio thread: bypass the installed pipeline from the next frame on.
  void LatchBypass(RootDenoiserBypass reason);

  // Control side (never the audio thread). Each takes the slot with up to
  // `tries` 1 ms sleeps and returns false, changing nothing, when the audio
  // thread did not let go. The pipeline it replaced or removed (may be null)
  // goes to `previous`.
  bool Install(RootFxPipeline* pipeline,
               uint32_t rate,
               int tries,
               RootFxPipeline** previous);
  bool Remove(int tries, RootFxPipeline** previous);

  // Any thread.
  bool installed() const;
  uint32_t rate() const;
  RootDenoiserBypass bypass() const;
  // `take_max` resets process_max_us.
  RootDenoiserSlotStats ReadStats(bool take_max);

  // Tests: install or remove at once.
  bool InstallForTest(void* pipeline, uint32_t rate);
  void* RemoveForTest();

 private:
  static constexpr uint32_t kFree = 0;
  static constexpr uint32_t kAudio = 1;
  static constexpr uint32_t kControl = 2;

  bool AcquireControl(int tries);
  void ReleaseControl();

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
  uint32_t calls_before_output_ = 0;
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

// The native INotificationHandler the app installs on every pipeline it hands
// over: static, so a notification that arrives after its pipeline is gone is
// still safe. It only counts.
RootFxNotificationHandler* RootDenoiserNotificationHandler();
// slowProcessing notifications (code 1) and the others, process-wide.
RootDenoiserNotificationCounts root_denoiser_notification_counts();
// The handler's address as an integer, 0 where the denoiser is not built.
size_t root_denoiser_notification_handler();
bool root_denoiser_supported();

// Tests: a C++ object with the SDK's vtable layout. Output = round(scale x
// input) delayed by delay_samples (a FIFO that starts, and restarts on a flush,
// with that many zeros). Call numbers count from 1 after construction or a
// flush; burst_call pops two frames' worth, calls [empty_from, empty_from +
// empty_count) return nothing.
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
  std::atomic<uint64_t> flush_calls_{0};
  std::atomic<uint64_t> power_sets_{0};
  std::atomic<float> power_{1.0f};
  std::atomic<uint64_t> calls_after_poison_{0};
  std::atomic<uint64_t> releases_{0};
};

}  // namespace livekit_ffi
