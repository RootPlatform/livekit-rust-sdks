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

#include "livekit/root_screen_audio.h"

#include <chrono>
#include <cmath>
#include <deque>
#include <mutex>
#include <thread>
#include <vector>

#include "api/media_stream_interface.h"

namespace livekit_ffi {

namespace {

// Writes a 440 Hz tone at -12 dBFS in 10 ms packets from its own thread, as an
// OS capture thread would.
class ToneWriter {
 public:
  ToneWriter(ScreenAudioSink* sink, ScreenAudioFormat format)
      : sink_(sink), format_(format) {
    thread_ = std::thread([this] { Run(); });
  }

  ~ToneWriter() {
    stop_.store(true);
    if (thread_.joinable())
      thread_.join();
  }

  void Pause(bool paused) { paused_.store(paused); }
  void Fail(int32_t error) { fail_.store(error); }

 private:
  void Run() {
    const size_t frames = static_cast<size_t>(format_.sample_rate / 100);
    const size_t channels = static_cast<size_t>(format_.channels);
    std::vector<float> packet(frames * channels);
    uint64_t position = 0;
    auto next = std::chrono::steady_clock::now();
    while (!stop_.load()) {
      int32_t fail = fail_.exchange(0);
      if (fail != 0) {
        sink_->ReportError(static_cast<ScreenAudioError>(fail), "fake failure");
        failed_ = true;
      }
      if (!failed_ && !paused_.load()) {
        for (size_t f = 0; f < frames; ++f) {
          double t = static_cast<double>(position + f) / format_.sample_rate;
          float value = static_cast<float>(0.25 * std::sin(2 * 3.14159265358979 * 440 * t));
          for (size_t c = 0; c < channels; ++c)
            packet[f * channels + c] = value;
        }
        position += frames;
        sink_->Write(packet.data(), frames);
      }
      next += std::chrono::milliseconds(10);
      std::this_thread::sleep_until(next);
    }
  }

  ScreenAudioSink* sink_;
  ScreenAudioFormat format_;
  std::atomic<bool> stop_{false};
  std::atomic<bool> paused_{false};
  std::atomic<int32_t> fail_{0};
  bool failed_ = false;
  std::thread thread_;
};

class ToneBackend final : public ScreenAudioBackend {
 public:
  ScreenAudioStartResult Start(const ScreenAudioTarget& target,
                               ScreenAudioSink* sink) override {
    (void)target;
    ScreenAudioStartResult result;
    result.detail = "fake tone backend";
    writer_ = std::make_unique<ToneWriter>(sink, result.format);
    return result;
  }

  bool Stop() override {
    writer_.reset();
    return true;
  }

 private:
  std::unique_ptr<ToneWriter> writer_;
};

}  // namespace

std::unique_ptr<ScreenAudioBackend> NewToneScreenAudioBackend() {
  return std::make_unique<ToneBackend>();
}

// Pipe harness

RootScreenAudioPipeHarness::RootScreenAudioPipeHarness() : pipe_(&clock_) {}

bool RootScreenAudioPipeHarness::configure(int32_t sample_rate,
                                           int32_t channels) {
  if (!pipe_.Configure(ScreenAudioFormat{sample_rate, channels}))
    return false;
  channels_ = channels;
  return true;
}

void RootScreenAudioPipeHarness::advance_us(int64_t us) {
  clock_.Advance(us);
}

int64_t RootScreenAudioPipeHarness::now_us() const {
  return clock_.NowUs();
}

void RootScreenAudioPipeHarness::write(rust::Slice<const float> interleaved) {
  pipe_.Write(interleaved.data(),
              interleaved.size() / static_cast<size_t>(channels_));
}

void RootScreenAudioPipeHarness::write_silence(size_t frames) {
  pipe_.WriteSilence(frames);
}

void RootScreenAudioPipeHarness::discontinuity() {
  pipe_.MarkDiscontinuity();
}

rust::Vec<int16_t> RootScreenAudioPipeHarness::pull(int32_t channels) {
  std::vector<int16_t> out(ScreenAudioPipe::kFrame *
                           static_cast<size_t>(channels));
  pipe_.Pull(out.data(), channels);
  rust::Vec<int16_t> result;
  result.reserve(out.size());
  for (int16_t sample : out)
    result.push_back(sample);
  return result;
}

static RootScreenAudioStatsData PipeStatsData(const ScreenAudioPipeStats& pipe) {
  RootScreenAudioStatsData stats{};
  stats.state = static_cast<int32_t>(pipe.running ? ScreenAudioState::kRunning
                                                  : ScreenAudioState::kWaiting);
  stats.captured_frames = pipe.captured_frames;
  stats.silent_capture_frames = pipe.silent_capture_frames;
  stats.discontinuities = pipe.discontinuities;
  stats.overflow_frames = pipe.overflow_frames;
  stats.write_max_gap_us = pipe.write_max_gap_us;
  stats.capture_stalls = pipe.capture_stalls;
  stats.pulls = pipe.pulls;
  stats.pull_max_gap_us = pipe.pull_max_gap_us;
  stats.sent_frames = pipe.sent_frames;
  stats.silence_frames = pipe.silence_frames;
  stats.underruns = pipe.underruns;
  stats.gaps = pipe.gaps;
  stats.trimmed_frames = pipe.trimmed_frames;
  stats.skipped_frames = pipe.skipped_frames;
  stats.clipped_samples = pipe.clipped_samples;
  stats.sent_sumsq = pipe.sent_sumsq;
  stats.sent_samples = pipe.sent_samples;
  stats.level_frames = pipe.level_frames;
  stats.target_frames = pipe.target_frames;
  stats.drift_ppm = pipe.drift_ppm;
  return stats;
}

RootScreenAudioStatsData RootScreenAudioPipeHarness::stats() {
  return PipeStatsData(pipe_.Read(true));
}

double RootScreenAudioPipeHarness::ratio() const {
  return pipe_.ratio();
}

std::unique_ptr<RootScreenAudioPipeHarness> new_root_screen_audio_pipe_harness() {
  return std::make_unique<RootScreenAudioPipeHarness>();
}

// Capture harness

struct ScriptedScreenAudioBackends {
  struct StopScript {
    bool ok = true;
    int32_t delay_ms = 0;
  };

  std::mutex mutex;
  std::deque<ScreenAudioStartResult> starts;
  std::deque<StopScript> stops;
  uint32_t start_count = 0;
  uint32_t stop_count = 0;
  int32_t last_target_kind = -1;
  ToneWriter* running = nullptr;

  std::mutex events_mutex;
  std::vector<RootScreenAudioEventData> events;
};

namespace {

class ScriptedBackend final : public ScreenAudioBackend {
 public:
  explicit ScriptedBackend(std::shared_ptr<ScriptedScreenAudioBackends> script)
      : script_(std::move(script)) {}

  ScreenAudioStartResult Start(const ScreenAudioTarget& target,
                               ScreenAudioSink* sink) override {
    ScreenAudioStartResult result;
    result.detail = "fake: all audio except this process";
    {
      std::lock_guard<std::mutex> lock(script_->mutex);
      ++script_->start_count;
      script_->last_target_kind = static_cast<int32_t>(target.kind);
      if (!script_->starts.empty()) {
        result = script_->starts.front();
        script_->starts.pop_front();
      }
    }
    if (result.error != ScreenAudioError::kNone)
      return result;
    writer_ = std::make_unique<ToneWriter>(sink, result.format);
    std::lock_guard<std::mutex> lock(script_->mutex);
    script_->running = writer_.get();
    return result;
  }

  bool Stop() override {
    ScriptedScreenAudioBackends::StopScript stop;
    {
      std::lock_guard<std::mutex> lock(script_->mutex);
      ++script_->stop_count;
      if (!script_->stops.empty()) {
        stop = script_->stops.front();
        script_->stops.pop_front();
      }
      if (script_->running == writer_.get())
        script_->running = nullptr;
    }
    writer_.reset();
    if (stop.delay_ms > 0)
      std::this_thread::sleep_for(std::chrono::milliseconds(stop.delay_ms));
    return stop.ok;
  }

 private:
  std::shared_ptr<ScriptedScreenAudioBackends> script_;
  std::unique_ptr<ToneWriter> writer_;
};

}  // namespace

class RecordingScreenAudioSink final : public webrtc::AudioTrackSinkInterface {
 public:
  using webrtc::AudioTrackSinkInterface::OnData;

  void OnData(const void* audio_data,
              int bits_per_sample,
              int sample_rate,
              size_t number_of_channels,
              size_t number_of_frames) override {
    (void)bits_per_sample;
    (void)sample_rate;
    frames_.fetch_add(number_of_frames);
    const int16_t* samples = static_cast<const int16_t*>(audio_data);
    for (size_t i = 0; i < number_of_channels * number_of_frames; ++i) {
      if (samples[i] > 64 || samples[i] < -64) {
        loud_frames_.fetch_add(number_of_frames);
        break;
      }
    }
  }

  uint64_t frames() const { return frames_.load(); }
  uint64_t loud_frames() const { return loud_frames_.load(); }

 private:
  std::atomic<uint64_t> frames_{0};
  std::atomic<uint64_t> loud_frames_{0};
};

RootScreenAudioCaptureHarness::RootScreenAudioCaptureHarness(
    int32_t channels,
    int32_t restart_delay_ms)
    : backends_(std::make_shared<ScriptedScreenAudioBackends>()),
      sink_(std::make_unique<RecordingScreenAudioSink>()) {
  source_ = new_audio_track_source(AudioSourceOptions{false, false, false},
                                   ScreenAudioPipe::kRate, channels, 1000);
  source_->get()->AddSink(sink_.get());
  ScreenAudioCaptureOptions options;
  auto script = backends_;
  options.make_backend = [script] {
    return std::make_unique<ScriptedBackend>(script);
  };
  options.restart_delays_ms.fill(restart_delay_ms);
  std::string error;
  capture_ = RootScreenAudioCapture::Create(source_, std::move(options), &error);
}

RootScreenAudioCaptureHarness::~RootScreenAudioCaptureHarness() {
  if (capture_)
    capture_->Shutdown(true);
  capture_.reset();
  source_->get()->RemoveSink(sink_.get());
}

void RootScreenAudioCaptureHarness::script_start(int32_t error,
                                                 int32_t sample_rate,
                                                 int32_t channels,
                                                 int32_t scope) const {
  ScreenAudioStartResult result;
  result.error = static_cast<ScreenAudioError>(error);
  result.detail = error == 0 ? "fake: scripted" : "fake: scripted failure";
  result.format = ScreenAudioFormat{sample_rate, channels};
  result.scope = static_cast<ScreenAudioScope>(scope);
  std::lock_guard<std::mutex> lock(backends_->mutex);
  backends_->starts.push_back(result);
}

void RootScreenAudioCaptureHarness::script_stop(bool ok, int32_t delay_ms) const {
  std::lock_guard<std::mutex> lock(backends_->mutex);
  backends_->stops.push_back({ok, delay_ms});
}

void RootScreenAudioCaptureHarness::fail_running(int32_t error) const {
  std::lock_guard<std::mutex> lock(backends_->mutex);
  if (backends_->running)
    backends_->running->Fail(error);
}

void RootScreenAudioCaptureHarness::pause_running(bool paused) const {
  std::lock_guard<std::mutex> lock(backends_->mutex);
  if (backends_->running)
    backends_->running->Pause(paused);
}

void RootScreenAudioCaptureHarness::start(const RootScreenAudioTargetSpec& target,
                                          uint64_t async_id) const {
  capture_->Start(ScreenAudioTargetFromSpec(target), async_id);
}

void RootScreenAudioCaptureHarness::stop(uint64_t async_id) const {
  capture_->Stop(async_id);
}

rust::Vec<RootScreenAudioEventData> RootScreenAudioCaptureHarness::wait_events(
    size_t count,
    int32_t timeout_ms) const {
  auto deadline = std::chrono::steady_clock::now() +
                  std::chrono::milliseconds(timeout_ms);
  std::lock_guard<std::mutex> lock(backends_->events_mutex);
  while (true) {
    for (auto& event : capture_->TakeEvents())
      backends_->events.push_back(std::move(event));
    if (backends_->events.size() >= count ||
        std::chrono::steady_clock::now() >= deadline)
      break;
    std::this_thread::sleep_for(std::chrono::milliseconds(2));
  }
  rust::Vec<RootScreenAudioEventData> out;
  for (auto& event : backends_->events)
    out.push_back(std::move(event));
  backends_->events.clear();
  return out;
}

RootScreenAudioStatsData RootScreenAudioCaptureHarness::stats() const {
  return capture_->Stats();
}

uint32_t RootScreenAudioCaptureHarness::backend_starts() const {
  std::lock_guard<std::mutex> lock(backends_->mutex);
  return backends_->start_count;
}

uint32_t RootScreenAudioCaptureHarness::backend_stops() const {
  std::lock_guard<std::mutex> lock(backends_->mutex);
  return backends_->stop_count;
}

int32_t RootScreenAudioCaptureHarness::last_target_kind() const {
  std::lock_guard<std::mutex> lock(backends_->mutex);
  return backends_->last_target_kind;
}

uint64_t RootScreenAudioCaptureHarness::sink_frames() const {
  return sink_->frames();
}

uint64_t RootScreenAudioCaptureHarness::sink_loud_frames() const {
  return sink_->loud_frames();
}

int64_t RootScreenAudioCaptureHarness::shutdown(bool wait) const {
  auto begin = std::chrono::steady_clock::now();
  capture_->Shutdown(wait);
  return std::chrono::duration_cast<std::chrono::microseconds>(
             std::chrono::steady_clock::now() - begin)
      .count();
}

static void NoopComplete(const SourceContext* context) {
  (void)context;
}

bool RootScreenAudioCaptureHarness::source_accepts_capture() const {
  std::vector<int16_t> silence(
      ScreenAudioPipe::kFrame * static_cast<size_t>(source_->num_channels()));
  return source_->capture_frame(
      rust::Slice<const int16_t>(silence.data(), silence.size()),
      ScreenAudioPipe::kRate, static_cast<uint32_t>(source_->num_channels()),
      ScreenAudioPipe::kFrame, nullptr, &NoopComplete);
}

rust::String RootScreenAudioCaptureHarness::second_capture_error() const {
  std::string error;
  auto second = RootScreenAudioCapture::Create(source_, ScreenAudioCaptureOptions(),
                                               &error);
  if (second)
    second->Shutdown(true);
  return rust::String::lossy(error.data(), error.size());
}

std::unique_ptr<RootScreenAudioCaptureHarness>
new_root_screen_audio_capture_harness(int32_t channels, int32_t restart_delay_ms) {
  return std::make_unique<RootScreenAudioCaptureHarness>(channels,
                                                         restart_delay_ms);
}

rust::String capture_error_for_source(int32_t sample_rate,
                                      int32_t channels,
                                      int32_t queue_size_ms) {
  auto source = new_audio_track_source(AudioSourceOptions{false, false, false},
                                       sample_rate, channels, queue_size_ms);
  std::string error;
  auto capture =
      RootScreenAudioCapture::Create(source, ScreenAudioCaptureOptions(), &error);
  if (capture)
    capture->Shutdown(true);
  return rust::String::lossy(error.data(), error.size());
}

}  // namespace livekit_ffi
