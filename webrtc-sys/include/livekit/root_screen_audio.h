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
#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <deque>
#include <functional>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

#include "livekit/audio_track.h"
#include "livekit/root_screen_audio_backend.h"
#include "rust/cxx.h"

namespace webrtc {
template <typename T>
class PushResampler;
}

namespace livekit_ffi {
class RootScreenAudioCaptureHandle;
#if defined(ROOTAPP_HARNESS)
class RootScreenAudioPipeHarness;
class RootScreenAudioCaptureHarness;
#endif
}  // namespace livekit_ffi
#include "webrtc-sys/src/root_screen_audio.rs.h"

namespace livekit_ffi {

class ScreenAudioClock {
 public:
  virtual ~ScreenAudioClock() = default;
  virtual int64_t NowUs() const = 0;
};

// steady_clock; process-wide, never destroyed.
const ScreenAudioClock* ScreenAudioSteadyClock();

// Single producer, single consumer ring of interleaved stereo float frames.
// Neither side locks or allocates.
class ScreenAudioRing {
 public:
  explicit ScreenAudioRing(size_t min_frames);
  ScreenAudioRing(const ScreenAudioRing&) = delete;
  ScreenAudioRing& operator=(const ScreenAudioRing&) = delete;

  size_t capacity() const { return mask_ + 1; }
  size_t Count() const;
  // Producer. Returns the frames written (a full ring takes fewer).
  size_t Write(const float* stereo, size_t frames);
  size_t WriteZeros(size_t frames);
  // Consumer.
  size_t Read(float* stereo, size_t frames);
  size_t Peek(size_t offset, float* stereo, size_t frames) const;
  size_t Discard(size_t frames);

 private:
  std::vector<float> buffer_;
  size_t mask_ = 0;
  std::atomic<uint64_t> write_{0};
  std::atomic<uint64_t> read_{0};
};

// Clock drift corrector for interleaved stereo (port of the desktop's managed
// DriftResampler): a fractional read position steps by ratio input frames per
// output frame through a 6-point Lagrange interpolator on a 32.32 phase. At
// ratio 1 with a zero fraction the output is the input, three frames late (or
// on time after a reset with look-ahead).
class ScreenAudioDriftResampler {
 public:
  static constexpr int kTaps = 6;
  static constexpr int kDelayFrames = 3;
  static constexpr double kMaxRatioDeviation = 0.02;

  ScreenAudioDriftResampler();

  double ratio() const;
  void set_ratio(double ratio);
  void Reset(bool with_lookahead);
  // Input frames Process must be given to produce exactly `output_frames`.
  size_t InputFramesFor(size_t output_frames) const;
  // Consumes input while output room remains; returns the frames written.
  size_t Process(const float* input,
                 size_t input_frames,
                 float* output,
                 size_t output_frames,
                 size_t* consumed);

  static void ComputeWeights(double x, double* weights);

 private:
  void Push(const float* frame);
  void Interpolate(float* destination);

  static constexpr int64_t kOne = int64_t{1} << 32;
  static constexpr int kCenter = 2;

  std::array<float, kTaps * 2> history_{};
  std::array<double, kTaps> weights_{};
  int64_t position_ = kOne;
  int64_t step_ = kOne;
  int64_t weights_fraction_ = -1;
};

// Click-free removal of a stretch of stereo audio (port of the desktop's
// AudioSplice): the cut is aligned by normalised cross-correlation and joined
// with a raised-cosine crossfade.
size_t ScreenAudioBestCut(const float* head,
                          size_t head_frames,
                          const float* candidates,
                          size_t candidate_frames);
// `destination` may alias `from`.
void ScreenAudioCrossfade(const float* from,
                          const float* to,
                          float* destination,
                          size_t frames);

struct ScreenAudioPipeStats {
  bool running = false;
  uint64_t captured_frames = 0;
  uint64_t silent_capture_frames = 0;
  uint64_t discontinuities = 0;
  uint64_t overflow_frames = 0;
  uint64_t write_max_gap_us = 0;
  uint64_t capture_stalls = 0;
  uint64_t pulls = 0;
  uint64_t pull_max_gap_us = 0;
  uint64_t sent_frames = 0;
  uint64_t silence_frames = 0;
  uint64_t underruns = 0;
  uint64_t gaps = 0;
  uint64_t trimmed_frames = 0;
  uint64_t skipped_frames = 0;
  uint64_t clipped_samples = 0;
  uint64_t sent_sumsq = 0;
  uint64_t sent_samples = 0;
  int32_t level_frames = -1;
  int32_t target_frames = 0;
  int32_t drift_ppm = 0;
};

// The capture's audio path: the backend writes (producer), the source's 10 ms
// task pulls (consumer), a ring in between. See design: priming to a target
// margin, drift correction on the exact ring level, gaps and jitter told
// apart by capture stalls, WSOLA trim of a backlog.
class ScreenAudioPipe {
 public:
  static constexpr int kRate = 48000;
  static constexpr size_t kFrame = 480;
  static constexpr size_t kRingFrames = 65536;
  static constexpr size_t kFadeFrames = 144;
  static constexpr size_t kTailFrames = kFadeFrames + 1;
  static constexpr size_t kInitialTargetFrames = 960;
  static constexpr size_t kTargetStepFrames = 480;
  static constexpr size_t kMaxTargetFrames = 9600;
  static constexpr uint32_t kTargetDecayPulls = 6000;
  static constexpr int64_t kStallUs = 60000;
  static constexpr int64_t kMaxStallUs = 500000;
  static constexpr uint32_t kWindowPulls = 25;
  static constexpr double kLevelSmoothing = 0.25;
  static constexpr double kDeadbandFrames = 96;
  static constexpr double kFullScaleFrames = 480;
  static constexpr double kMaxCorrection = 0.002;
  static constexpr double kRunawayFrames = 1920;
  static constexpr double kRunawayCorrection = 0.01;
  static constexpr size_t kTrimAboveFrames = 1440;
  static constexpr size_t kMaxTrimFrames = 24000;
  static constexpr size_t kSearchFrames = 2 * kFadeFrames;
  // Producer chunk: downmix and conversion work in pieces of this size.
  static constexpr size_t kChunkFrames = 1024;
  static constexpr size_t kGatherFrames = 1024;

  explicit ScreenAudioPipe(const ScreenAudioClock* clock);
  ~ScreenAudioPipe();
  ScreenAudioPipe(const ScreenAudioPipe&) = delete;
  ScreenAudioPipe& operator=(const ScreenAudioPipe&) = delete;

  static bool FormatSupported(const ScreenAudioFormat& format);

  // Control side, while no producer runs: the format the next producer
  // writes. Allocates the rate converter. Counts a capture stall (whatever
  // ran dry meanwhile was a pause of the capture, not jitter).
  bool Configure(const ScreenAudioFormat& format);

  // Producer.
  void Write(const float* interleaved, size_t frames);
  void WriteSilence(size_t frames);
  void MarkDiscontinuity();

  // Consumer: exactly kFrame frames of `channels` (1 or 2) interleaved int16.
  void Pull(int16_t* out, int channels);

  // Any thread. The *_max_* fields reset when `reset_maxima`.
  ScreenAudioPipeStats Read(bool reset_maxima);

  // Consumer side, for tests.
  double ratio() const { return resampler_.ratio(); }

 private:
  void NoteArrival(size_t frames);
  void PushStereo(const float* stereo, size_t frames);
  void CountOverflow(size_t offered, size_t written);

  size_t Available() const;
  size_t Gather(float* destination, size_t frames);
  void DropOldest(size_t frames);
  size_t PullWaiting();
  size_t PullRunning();
  void EnterWaiting();
  void UpdateLevel(size_t margin);
  void Trim(size_t excess);
  size_t FadeOut(size_t produced);
  void Emit(int16_t* out, int channels, size_t produced);

  const ScreenAudioClock* clock_;
  ScreenAudioRing ring_;

  // Producer state (configured by the control side between producers).
  int rate_ = kRate;
  int channels_ = 2;
  size_t in_frames_ = kFrame;
  size_t in_fill_ = 0;
  std::unique_ptr<webrtc::PushResampler<float>> converter_;
  std::vector<float> stereo_chunk_;
  std::vector<float> in_chunk_;
  std::vector<float> out_chunk_;
  std::vector<float> zeros_;

  std::atomic<int64_t> last_write_us_{0};
  std::atomic<uint64_t> captured_frames_{0};
  std::atomic<uint64_t> silent_capture_frames_{0};
  std::atomic<uint64_t> discontinuities_{0};
  std::atomic<uint64_t> overflow_frames_{0};
  std::atomic<uint64_t> write_max_gap_us_{0};
  std::atomic<uint64_t> stalls_{0};
  // The largest recent delivery in 48 kHz frames (producer-written, decaying).
  std::atomic<uint64_t> largest_write_frames_{0};

  // Consumer state.
  bool running_ = false;
  bool fade_in_ = false;
  bool underrun_pending_ = false;
  uint64_t stall_mark_ = 0;
  size_t target_ = kInitialTargetFrames;
  uint32_t pulls_since_jitter_ = 0;
  double smoothed_ = -1;
  size_t window_min_ = SIZE_MAX;
  uint32_t window_count_ = 0;
  size_t trim_request_ = 0;
  int64_t last_pull_us_ = 0;
  ScreenAudioDriftResampler resampler_;
  std::vector<float> gather_;
  std::vector<float> out_;
  std::vector<float> head_;
  std::vector<float> candidates_;
  std::vector<float> tail_;
  // The last kTailFrames frames the previous running pull sent.
  std::vector<float> sent_tail_;
  std::vector<float> pending_;
  size_t pending_frames_ = 0;
  size_t pending_offset_ = 0;

  std::atomic<bool> running_published_{false};
  std::atomic<uint64_t> pulls_{0};
  std::atomic<uint64_t> pull_max_gap_us_{0};
  std::atomic<uint64_t> sent_frames_{0};
  std::atomic<uint64_t> silence_frames_{0};
  std::atomic<uint64_t> underruns_{0};
  std::atomic<uint64_t> gaps_{0};
  std::atomic<uint64_t> trimmed_frames_{0};
  std::atomic<uint64_t> skipped_frames_{0};
  std::atomic<uint64_t> clipped_samples_{0};
  std::atomic<uint64_t> sent_sumsq_{0};
  std::atomic<uint64_t> sent_samples_{0};
  std::atomic<int32_t> level_frames_{-1};
  std::atomic<int32_t> target_frames_{static_cast<int32_t>(kInitialTargetFrames)};
  std::atomic<int32_t> drift_ppm_{0};
};

// RootScreenAudioState in rootapp_audio.proto.
enum class ScreenAudioState : int32_t {
  kStarting = 0,
  kWaiting = 1,
  kRunning = 2,
  kRestarting = 3,
  kFailed = 4,
  kStopped = 5,
};

enum class ScreenAudioEventKind : uint32_t {
  kStarted = 1,
  kFailed = 2,
  kStopped = 3,
};

struct ScreenAudioCaptureOptions {
  const ScreenAudioClock* clock = nullptr;
  // CreateRootScreenAudioBackend (or the testing override) when empty.
  std::function<std::unique_ptr<ScreenAudioBackend>()> make_backend;
  std::array<int, 5> restart_delays_ms{{200, 1000, 3000, 3000, 3000}};
  int64_t restart_window_ms = 60000;
};

// One screen-audio capture attached to one queued AudioTrackSource. The
// control thread starts, stops and restarts the backend; the source's 10 ms
// task pulls. Shut down by its handle: the puller is detached at once, the
// rest finishes on the control thread, which then frees the capture.
class RootScreenAudioCapture final
    : public AudioSourcePuller,
      public std::enable_shared_from_this<RootScreenAudioCapture> {
 public:
  // Null with `error` set when the source cannot take a capture.
  static std::shared_ptr<RootScreenAudioCapture> Create(
      std::shared_ptr<AudioTrackSource> source,
      ScreenAudioCaptureOptions options,
      std::string* error);

  ~RootScreenAudioCapture();

  void Start(const ScreenAudioTarget& target, uint64_t async_id);
  void Stop(uint64_t async_id);
  // Detaches from the source; the control thread stops the backend and
  // exits. `wait` blocks until it did (tests).
  void Shutdown(bool wait);

  void Pull(int16_t* interleaved,
            int sample_rate,
            int channels,
            int frames) override;

  RootScreenAudioStatsData Stats();
  rust::Vec<RootScreenAudioEventData> TakeEvents();

  // Any thread, wait-free (the gate forwards ReportError here).
  void OnBackendError(ScreenAudioError error, const char* detail);

 private:
  class Gate;
  struct Command {
    enum class Kind { kStart, kStop, kShutdown } kind;
    ScreenAudioTarget target;
    uint64_t async_id = 0;
  };
  struct Running {
    std::unique_ptr<ScreenAudioBackend> backend;
    Gate* gate = nullptr;
  };
  enum class BackendStop {
    kStopped,
    // Stopped feeding the source, but the OS did not confirm its stop.
    kUnconfirmed,
    // Could not stop: the backend is leaked and its OS capture may still run.
    kLeaked,
  };

  RootScreenAudioCapture(std::shared_ptr<AudioTrackSource> source,
                         ScreenAudioCaptureOptions options);

  void Run();
  void RunStart(const ScreenAudioTarget& target, uint64_t async_id);
  void RunStop(uint64_t async_id);
  bool TryStart(const ScreenAudioTarget& target, ScreenAudioStartResult* result);
  BackendStop StopBackend();
  void ClearError();
  void HandleError();
  void ScheduleRestart(ScreenAudioError error, const std::string& message);
  void RunRestart();
  void Push(RootScreenAudioEventData event);
  void SetState(ScreenAudioState state);
  int64_t NowMs() const;

  std::shared_ptr<AudioTrackSource> source_;
  ScreenAudioCaptureOptions options_;
  const ScreenAudioClock* clock_;
  ScreenAudioPipe pipe_;

  std::mutex mutex_;
  std::condition_variable wake_;
  std::deque<Command> commands_;
  bool shutdown_requested_ = false;
  bool finished_ = false;
  std::condition_variable finished_cv_;
  std::thread thread_;
  std::atomic<bool> attached_{false};

  // Wait-free error report from the backend.
  std::atomic<int32_t> error_code_{0};
  std::atomic<bool> error_ready_{false};
  std::array<char, 256> error_detail_{};

  // Control thread only.
  Running running_;
  bool have_target_ = false;
  ScreenAudioTarget target_;
  // What the last successful start (not a restart) captured.
  ScreenAudioScope started_scope_ = ScreenAudioScope::kSystem;
  uint32_t started_process_id_ = 0;
  bool restart_pending_ = false;
  int64_t restart_at_ms_ = 0;
  std::deque<int64_t> restart_times_ms_;

  std::mutex state_mutex_;
  ScreenAudioState state_ = ScreenAudioState::kStarting;
  ScreenAudioScope scope_ = ScreenAudioScope::kSystem;
  ScreenAudioFormat format_{0, 0};
  uint32_t restarts_ = 0;
  ScreenAudioError last_error_ = ScreenAudioError::kNone;
  std::string last_error_message_;

  std::mutex events_mutex_;
  std::vector<RootScreenAudioEventData> events_;
};

// What livekit-ffi owns. Dropping it shuts the capture down without blocking.
class RootScreenAudioCaptureHandle {
 public:
  explicit RootScreenAudioCaptureHandle(
      std::shared_ptr<RootScreenAudioCapture> capture);
  ~RootScreenAudioCaptureHandle();

  void start(const RootScreenAudioTargetSpec& target, uint64_t async_id) const;
  void stop(uint64_t async_id) const;
  RootScreenAudioStatsData stats() const;
  rust::Vec<RootScreenAudioEventData> take_events() const;

 private:
  std::shared_ptr<RootScreenAudioCapture> capture_;
};

bool root_screen_audio_backend_built();
std::unique_ptr<RootScreenAudioCaptureHandle> new_root_screen_audio_capture(
    std::shared_ptr<AudioTrackSource> source,
    rust::String& error);

ScreenAudioTarget ScreenAudioTargetFromSpec(
    const RootScreenAudioTargetSpec& spec);

#if defined(ROOTAPP_HARNESS)
// Testing: every capture made after this (without its own make_backend) uses
// the fake backend: a 48 kHz stereo 440 Hz tone at -12 dBFS in 10 ms packets
// from its own thread.
void root_screen_audio_use_fake_backend(bool enabled);
std::function<std::unique_ptr<ScreenAudioBackend>()>
ScreenAudioBackendFactoryOverride();

// Test harnesses (root_screen_audio_harness.cpp).

class ManualScreenAudioClock final : public ScreenAudioClock {
 public:
  int64_t NowUs() const override { return now_.load(); }
  void Advance(int64_t us) { now_.fetch_add(us); }

 private:
  std::atomic<int64_t> now_{1'000'000};
};

// The pipe alone, single-threaded, on a manual clock (device-free tests of the
// pacing, drift, gap and trim logic).
class RootScreenAudioPipeHarness {
 public:
  RootScreenAudioPipeHarness();

  bool configure(int32_t sample_rate, int32_t channels);
  void advance_us(int64_t us);
  int64_t now_us() const;
  void write(rust::Slice<const float> interleaved);
  void write_silence(size_t frames);
  void discontinuity();
  rust::Vec<int16_t> pull(int32_t channels);
  RootScreenAudioStatsData stats();
  double ratio() const;

 private:
  ManualScreenAudioClock clock_;
  ScreenAudioPipe pipe_;
  int32_t channels_ = 2;
};

std::unique_ptr<RootScreenAudioPipeHarness> new_root_screen_audio_pipe_harness();

struct ScriptedScreenAudioBackends;
class RecordingScreenAudioSink;

// A real queued AudioTrackSource with a recording sink, and a capture whose
// backends are scripted fakes (device-free, real threads and time).
class RootScreenAudioCaptureHarness {
 public:
  RootScreenAudioCaptureHarness(int32_t channels, int32_t restart_delay_ms);
  ~RootScreenAudioCaptureHarness();

  void script_start(int32_t error,
                    int32_t sample_rate,
                    int32_t channels,
                    int32_t scope) const;
  void script_start_for(int32_t scope, uint32_t process_id) const;
  void script_start_limited(int32_t scope,
                            uint32_t process_id,
                            int32_t limitation) const;
  void script_stop(bool ok, int32_t delay_ms) const;
  void script_stop_unconfirmed() const;
  void fail_running(int32_t error) const;
  void pause_running(bool paused) const;
  void start(const RootScreenAudioTargetSpec& target, uint64_t async_id) const;
  void stop(uint64_t async_id) const;
  rust::Vec<RootScreenAudioEventData> wait_events(size_t count,
                                                  int32_t timeout_ms) const;
  RootScreenAudioStatsData stats() const;
  uint32_t backend_starts() const;
  uint32_t backend_stops() const;
  int32_t last_target_kind() const;
  bool last_target_restart() const;
  uint64_t sink_frames() const;
  uint64_t sink_loud_frames() const;
  int64_t shutdown(bool wait) const;
  bool source_accepts_capture() const;
  rust::String second_capture_error() const;

 private:
  std::shared_ptr<ScriptedScreenAudioBackends> backends_;
  std::shared_ptr<AudioTrackSource> source_;
  std::unique_ptr<RecordingScreenAudioSink> sink_;
  std::shared_ptr<RootScreenAudioCapture> capture_;
};

std::unique_ptr<RootScreenAudioCaptureHarness>
new_root_screen_audio_capture_harness(int32_t channels, int32_t restart_delay_ms);

rust::String capture_error_for_source(int32_t sample_rate,
                                      int32_t channels,
                                      int32_t queue_size_ms);

rust::Vec<uint32_t> screen_audio_ancestors(uint32_t self_pid,
                                           rust::Slice<const uint32_t> pids,
                                           rust::Slice<const uint32_t> parents,
                                           rust::Slice<const uint64_t> created);
#endif

}  // namespace livekit_ffi
