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

#include "livekit/root_denoiser.h"

#include <algorithm>
#include <chrono>
#include <cmath>
#include <limits>
#include <utility>

#include "rtc_base/platform_thread_types.h"

namespace livekit_ffi {

static_assert(offsetof(RootFxPipelineVtbl, set_noise_suppression_power) ==
              4 * sizeof(void*));
static_assert(offsetof(RootFxPipelineVtbl, set_notification_handler) ==
              7 * sizeof(void*));
static_assert(offsetof(RootFxPipelineVtbl, process) == 8 * sizeof(void*));
static_assert(offsetof(RootFxPipelineVtbl, flush) == 9 * sizeof(void*));
static_assert(offsetof(RootFxNotificationHandlerVtbl, receive) ==
              sizeof(void*));

namespace {

int64_t NowUs() {
  return std::chrono::duration_cast<std::chrono::microseconds>(
             std::chrono::steady_clock::now().time_since_epoch())
      .count();
}

int64_t NowMs() {
  return std::chrono::duration_cast<std::chrono::milliseconds>(
             std::chrono::steady_clock::now().time_since_epoch())
      .count();
}

uint64_t Address(const void* pointer) {
  return static_cast<uint64_t>(reinterpret_cast<uintptr_t>(pointer));
}

RootFxPipeline* PipelineAt(uint64_t address) {
  return reinterpret_cast<RootFxPipeline*>(static_cast<uintptr_t>(address));
}

std::atomic<uint64_t> g_slow_notifications{0};
std::atomic<uint64_t> g_other_notifications{0};

void HandlerRelease(void*) {}

void HandlerReceive(void*, void* notification) {
  if (!notification) {
    return;
  }
  auto* fx = static_cast<RootFxNotification*>(notification);
  const int32_t code = fx->vtbl->code(fx);
  (code == 1 ? g_slow_notifications : g_other_notifications)
      .fetch_add(1, std::memory_order_relaxed);
}

const RootFxNotificationHandlerVtbl kHandlerVtbl = {&HandlerRelease,
                                                    &HandlerReceive};
RootFxNotificationHandler g_handler = {&kHandlerVtbl};

}  // namespace

// =============================================================================
// RootDenoiserSlot
// =============================================================================

RootDenoiserSlot::RootDenoiserSlot() = default;
RootDenoiserSlot::~RootDenoiserSlot() = default;

RootDenoiseResult RootDenoiserSlot::ProcessAudio(const float* in,
                                                 size_t frames,
                                                 int sample_rate,
                                                 bool flush,
                                                 float power,
                                                 float* out,
                                                 size_t out_capacity) {
  if (flush) {
    flush_pending_ = true;
  }
  if (!installed_.load(std::memory_order_relaxed)) {
    pending_bypass_ = RootDenoiserBypass::kNone;
    return {};
  }
  uint32_t expected = kFree;
  if (!owner_.compare_exchange_strong(expected, kAudio,
                                      std::memory_order_acquire,
                                      std::memory_order_relaxed)) {
    misses_.fetch_add(1, std::memory_order_relaxed);
    RootDenoiseResult missed;
    missed.kind = RootDenoiseKind::kMissed;
    return missed;
  }
  ApplyPendingBypass();
  RootFxPipeline* const pipeline = pipeline_;
  const size_t rate = sample_rate > 0 ? static_cast<size_t>(sample_rate) : 0;
  const bool fits = rate == pipeline_rate_ && frames != 0 &&
                    frames == rate / 100 && in && out &&
                    out_capacity >= kOutputFrames * frames;
  uint32_t bypass = bypass_.load(std::memory_order_relaxed);
  if (pipeline && fits &&
      bypass == static_cast<uint32_t>(RootDenoiserBypass::kRateMismatch)) {
    // The rate is back (a device flapping between rates). The pipeline missed
    // everything since, so it starts over.
    bypass = 0;
    bypass_.store(0, std::memory_order_relaxed);
    flush_pending_ = true;
  }
  if (!pipeline || bypass != 0) {
    owner_.store(kFree, std::memory_order_release);
    if (pipeline) {
      bypassed_frames_.fetch_add(1, std::memory_order_relaxed);
    }
    return {};
  }
  if (!fits) {
    bypass_.store(static_cast<uint32_t>(RootDenoiserBypass::kRateMismatch),
                  std::memory_order_relaxed);
    owner_.store(kFree, std::memory_order_release);
    bypassed_frames_.fetch_add(1, std::memory_order_relaxed);
    return {};
  }

  bool restart = generation_ != seen_generation_;
  if (restart) {
    seen_generation_ = generation_;
    applied_power_ = std::numeric_limits<float>::quiet_NaN();
    first_output_seen_ = false;
    calls_before_output_ = 0;
    slow_count_ = 0;
  }
  // Everything the SDK does on this frame is timed: flush, power and process.
  const int64_t start_us = NowUs();
  if (flush_pending_) {
    if (!restart) {
      pipeline->vtbl->flush(pipeline, nullptr, 0);
    }
    restart = true;
    flush_pending_ = false;
  }
  const float wanted_power =
      std::isnan(power) ? 0.0f : std::min(1.0f, std::max(0.0f, power));
  bool power_changed = false;
  if (!(wanted_power == applied_power_)) {
    pipeline->vtbl->set_noise_suppression_power(pipeline, wanted_power);
    applied_power_ = wanted_power;
    power_changed = true;
  }

  const uint32_t capacity = static_cast<uint32_t>(kOutputFrames * frames);
  uint32_t samples = pipeline->vtbl->process(
      pipeline, in, static_cast<uint32_t>(frames), out, capacity);
  const int64_t end_us = NowUs();
  const int64_t elapsed_us = end_us - start_us;
  samples = std::min(samples, capacity);

  if (!first_output_seen_) {
    if (samples > 0) {
      first_output_seen_ = true;
      frames_until_first_output_.store(calls_before_output_,
                                       std::memory_order_relaxed);
    } else if (calls_before_output_ < std::numeric_limits<int32_t>::max()) {
      ++calls_before_output_;
    }
  }
  if (elapsed_us > kSlowFrameUs) {
    slow_frames_.fetch_add(1, std::memory_order_relaxed);
  }
  // A (re)start, flush or power change pays one-off costs: measured and
  // counted above like any other call, but only plain calls latch.
  if (elapsed_us > kOverFrameUs && !restart && !power_changed) {
    slow_times_us_[slow_count_ % kSlowFramesToBypass] = end_us;
    ++slow_count_;
    bool latch = elapsed_us > kStalledFrameUs;
    if (slow_count_ >= kSlowFramesToBypass &&
        end_us - slow_times_us_[slow_count_ % kSlowFramesToBypass] <=
            kSlowWindowUs) {
      latch = true;
    }
    if (latch) {
      bypass_.store(static_cast<uint32_t>(RootDenoiserBypass::kSlow),
                    std::memory_order_relaxed);
    }
  }
  owner_.store(kFree, std::memory_order_release);

  CountProcess(elapsed_us, samples);
  RootDenoiseResult result;
  result.kind = RootDenoiseKind::kRan;
  result.samples = samples;
  result.restart = restart;
  return result;
}

void RootDenoiserSlot::CountProcess(int64_t elapsed_us, size_t samples) {
  frames_.fetch_add(1, std::memory_order_relaxed);
  if (samples == 0) {
    empty_outputs_.fetch_add(1, std::memory_order_relaxed);
  }
  size_t bucket = 0;
  while (bucket < kHistogramBoundsUs.size() &&
         elapsed_us > kHistogramBoundsUs[bucket]) {
    ++bucket;
  }
  histogram_[bucket].fetch_add(1, std::memory_order_relaxed);
  const uint64_t elapsed =
      static_cast<uint64_t>(std::max<int64_t>(0, elapsed_us));
  uint64_t max = process_max_us_.load(std::memory_order_relaxed);
  while (elapsed > max && !process_max_us_.compare_exchange_weak(
                              max, elapsed, std::memory_order_relaxed)) {
  }
}

void RootDenoiserSlot::ApplyPendingBypass() {
  if (pending_bypass_ == RootDenoiserBypass::kNone) {
    return;
  }
  if (pipeline_ && generation_ == pending_bypass_generation_ &&
      bypass_.load(std::memory_order_relaxed) == 0) {
    bypass_.store(static_cast<uint32_t>(pending_bypass_),
                  std::memory_order_relaxed);
  }
  pending_bypass_ = RootDenoiserBypass::kNone;
}

void RootDenoiserSlot::LatchBypass(RootDenoiserBypass reason) {
  if (reason == RootDenoiserBypass::kNone) {
    return;
  }
  uint32_t expected = kFree;
  if (owner_.compare_exchange_strong(expected, kAudio,
                                     std::memory_order_acquire,
                                     std::memory_order_relaxed)) {
    if (pipeline_ && generation_ == seen_generation_ &&
        bypass_.load(std::memory_order_relaxed) == 0) {
      bypass_.store(static_cast<uint32_t>(reason), std::memory_order_relaxed);
    }
    owner_.store(kFree, std::memory_order_release);
    return;
  }
  pending_bypass_ = reason;
  pending_bypass_generation_ = seen_generation_;
}

bool RootDenoiserSlot::AcquireControl(int timeout_ms) {
  const auto deadline = std::chrono::steady_clock::now() +
                        std::chrono::milliseconds(std::max(0, timeout_ms));
  while (true) {
    uint32_t expected = kFree;
    if (owner_.compare_exchange_strong(expected, kControl,
                                       std::memory_order_acquire,
                                       std::memory_order_relaxed)) {
      return true;
    }
    if (std::chrono::steady_clock::now() >= deadline) {
      return false;
    }
    std::this_thread::sleep_for(std::chrono::milliseconds(1));
  }
}

void RootDenoiserSlot::Publish(RootFxPipeline* pipeline, uint32_t rate) {
  pipeline_ = pipeline;
  pipeline_rate_ = pipeline ? rate : 0;
  ++generation_;
  bypass_.store(0, std::memory_order_relaxed);
  installed_.store(pipeline != nullptr, std::memory_order_relaxed);
  published_rate_.store(pipeline_rate_, std::memory_order_relaxed);
  published_generation_.store(generation_, std::memory_order_relaxed);
  frames_until_first_output_.store(-1, std::memory_order_relaxed);
}

bool RootDenoiserSlot::Install(RootFxPipeline* pipeline,
                               uint32_t rate,
                               int timeout_ms,
                               RootFxPipeline** previous) {
  if (previous) {
    *previous = nullptr;
  }
  if (!AcquireControl(timeout_ms)) {
    return false;
  }
  if (previous) {
    *previous = pipeline_;
  }
  Publish(pipeline, rate);
  if (pipeline) {
    installs_.fetch_add(1, std::memory_order_relaxed);
  }
  owner_.store(kFree, std::memory_order_release);
  return true;
}

bool RootDenoiserSlot::Remove(int timeout_ms, RootFxPipeline** previous) {
  return Install(nullptr, 0, timeout_ms, previous);
}

bool RootDenoiserSlot::installed() const {
  return installed_.load(std::memory_order_relaxed);
}

uint32_t RootDenoiserSlot::rate() const {
  return published_rate_.load(std::memory_order_relaxed);
}

RootDenoiserBypass RootDenoiserSlot::bypass() const {
  return static_cast<RootDenoiserBypass>(
      bypass_.load(std::memory_order_relaxed));
}

RootDenoiserSlotStats RootDenoiserSlot::ReadStats(bool take_max) {
  RootDenoiserSlotStats stats;
  stats.installed = installed();
  stats.rate = rate();
  stats.bypass = bypass();
  stats.generation = published_generation_.load(std::memory_order_relaxed);
  stats.frames = frames_.load(std::memory_order_relaxed);
  stats.misses = misses_.load(std::memory_order_relaxed);
  stats.empty_outputs = empty_outputs_.load(std::memory_order_relaxed);
  stats.slow_frames = slow_frames_.load(std::memory_order_relaxed);
  stats.bypassed_frames = bypassed_frames_.load(std::memory_order_relaxed);
  stats.installs = installs_.load(std::memory_order_relaxed);
  stats.frames_until_first_output =
      frames_until_first_output_.load(std::memory_order_relaxed);
  stats.process_max_us =
      take_max ? process_max_us_.exchange(0, std::memory_order_relaxed)
               : process_max_us_.load(std::memory_order_relaxed);
  for (size_t i = 0; i < kHistogramBuckets; ++i) {
    stats.histogram[i] = histogram_[i].load(std::memory_order_relaxed);
  }
  return stats;
}

#if defined(ROOTAPP_HARNESS)
bool RootDenoiserSlot::InstallForTest(void* pipeline, uint32_t rate) {
  return Install(static_cast<RootFxPipeline*>(pipeline), rate,
                 kControlTimeoutMs, nullptr);
}

void* RootDenoiserSlot::RemoveForTest() {
  RootFxPipeline* previous = nullptr;
  Remove(kControlTimeoutMs, &previous);
  return previous;
}

bool RootDenoiserSlot::HoldForTest(bool hold) {
  if (!hold) {
    uint32_t expected = kAudio;
    return owner_.compare_exchange_strong(expected, kFree,
                                          std::memory_order_release,
                                          std::memory_order_relaxed);
  }
  uint32_t expected = kFree;
  return owner_.compare_exchange_strong(
      expected, kAudio, std::memory_order_acquire, std::memory_order_relaxed);
}
#endif

// =============================================================================
// RootNsFallback
// =============================================================================

RootNsFallback::RootNsFallback(int64_t grace_ms) : grace_ms_(grace_ms) {}

std::optional<bool> RootNsFallback::Update(int64_t now_ms,
                                           bool wanted,
                                           bool installed,
                                           RootDenoiserBypass bypass,
                                           uint32_t pipeline_rate,
                                           uint32_t process_rate,
                                           uint64_t processed_frames,
                                           uint64_t stream_epoch,
                                           bool arriving) {
  if (processed_frames != frames_) {
    frames_ = processed_frames;
    frames_moved_ms_ = now_ms;
  }
  const bool flowing =
      frames_moved_ms_.has_value() && now_ms - *frames_moved_ms_ <= kFlowingMs;
  if (stream_epoch != epoch_) {
    epoch_ = stream_epoch;
    inactive_since_ms_.reset();
  }
  const bool active = installed && bypass == RootDenoiserBypass::kNone &&
                      (process_rate == 0 || pipeline_rate == process_rate);
  bool on = false;
  if (wanted && !active) {
    // A pipeline waiting for a pause in speech lands within its install wait;
    // turning NS on (an AEC re-init) just before it would cost a second
    // re-init when it lands.
    if (!flowing || arriving) {
      inactive_since_ms_.reset();
    } else if (!inactive_since_ms_) {
      inactive_since_ms_ = now_ms;
    }
    const bool died =
        installed && (bypass == RootDenoiserBypass::kOutputStalled ||
                      bypass == RootDenoiserBypass::kSlow);
    on = on_ || died ||
         (inactive_since_ms_.has_value() &&
          now_ms - *inactive_since_ms_ >= grace_ms_);
  } else {
    inactive_since_ms_.reset();
  }
  if (on == on_) {
    return std::nullopt;
  }
  on_ = on;
  return on;
}

void RootNsFallback::Reset() {
  on_ = false;
  inactive_since_ms_.reset();
  frames_moved_ms_.reset();
}

// =============================================================================
// RootDenoiserControl
// =============================================================================

RootDenoiserControl::RootDenoiserControl(std::shared_ptr<RootDenoiserSlot> slot,
                                         Hooks hooks,
                                         Timing timing)
    : slot_(std::move(slot)),
      hooks_(std::move(hooks)),
      timing_(timing),
      fallback_(timing.fallback_grace_ms) {}

RootDenoiserControl::~RootDenoiserControl() {
  {
    std::lock_guard<std::mutex> lock(mutex_);
    stop_ = true;
  }
  wake_.notify_all();
  if (thread_.joinable()) {
    thread_.join();
  }
}

void RootDenoiserControl::EnsureThreadLocked() {
  if (!thread_.joinable() && !stop_) {
    thread_ = std::thread([this] { Run(); });
  }
}

void RootDenoiserControl::Set(uint64_t owner,
                              uint64_t async_id,
                              uint64_t pipeline,
                              uint32_t rate) {
  {
    std::lock_guard<std::mutex> lock(mutex_);
    Request request;
    request.seq = ++next_seq_;
    request.owner = owner;
    request.async_id = async_id;
    request.pipeline = pipeline;
    request.rate = rate;
    request.since_ms = NowMs();
    const auto on_returned_list = [pipeline](const RootDenoiserReturned& r) {
      return r.pipeline == pipeline;
    };
    if (pipeline != 0 && (rate < kMinRate || rate > kMaxRate)) {
      // An error has the app release the pipeline, so one native still holds
      // (or has yet to hand back) stays as it is and this changes nothing.
      if (HeldLocked(pipeline) ||
          std::any_of(returned_.begin(), returned_.end(), on_returned_list)) {
        CompleteLocked(request, std::string(), 0);
      } else {
        CompleteLocked(request,
                       "pipeline rate " + std::to_string(rate) +
                           " is outside 8000-48000 Hz",
                       pipeline);
      }
      return;
    }
    if (pipeline != 0) {
      returned_.erase(
          std::remove_if(returned_.begin(), returned_.end(), on_returned_list),
          returned_.end());
    }
    queue_.push_back(request);
    EnsureThreadLocked();
  }
  wake_.notify_all();
}

void RootDenoiserControl::Wake() {
  {
    std::lock_guard<std::mutex> lock(mutex_);
    EnsureThreadLocked();
  }
  wake_.notify_all();
}

void RootDenoiserControl::Run() {
  webrtc::SetCurrentThreadName("rootapp-effects");
  std::unique_lock<std::mutex> lock(mutex_);
  while (!stop_) {
    const int64_t now_ms = NowMs();
    if (!waiting_ && !queue_.empty()) {
      waiting_ = queue_.front();
      queue_.pop_front();
    }
    if (waiting_) {
      if (waiting_->pipeline != 0 && !queue_.empty()) {
        const Request superseded = *waiting_;
        waiting_.reset();
        FailLocked(superseded, kErrorSuperseded);
        continue;
      }
      // Re-installing the installed pipeline changes nothing: no wait.
      const bool ready =
          waiting_->pipeline == 0 ||
          waiting_->pipeline == installed_pipeline_ ||
          now_ms - waiting_->since_ms >= timing_.install_wait_ms ||
          !(hooks_.speaking && hooks_.speaking());
      if (ready) {
        CommitLocked(lock, waiting_->seq);
        continue;
      }
    }
    bool changed = false;
    const bool ticking = TickLocked(now_ms, &changed);
    if (changed) {
      lock.unlock();
      ApplyFallback();
      lock.lock();
      // A request or a stop may have come meanwhile.
      continue;
    }
    if (stop_) {
      break;
    }
    if (waiting_ || ticking) {
      wake_.wait_for(lock, std::chrono::milliseconds(timing_.tick_ms));
    } else {
      wake_.wait(lock);
    }
  }
}

void RootDenoiserControl::CommitLocked(std::unique_lock<std::mutex>& lock,
                                       uint64_t seq) {
  lock.unlock();
  std::lock_guard<std::timed_mutex> slot_lock(slot_mutex_);
  lock.lock();
  if (!waiting_ || waiting_->seq != seq) {
    return;
  }
  const Request request = *waiting_;
  waiting_.reset();
  if (request.pipeline != 0 && request.pipeline == installed_pipeline_) {
    // Handed over again while installed: nothing to swap or give back.
    installed_owner_ = request.owner;
    CompleteLocked(request, std::string(), 0);
    return;
  }
  in_flight_pipeline_ = request.pipeline;
  lock.unlock();
  RootFxPipeline* previous = nullptr;
  const bool done =
      request.pipeline != 0
          ? slot_->Install(PipelineAt(request.pipeline), request.rate,
                           timing_.control_timeout_ms, &previous)
          : slot_->Remove(timing_.control_timeout_ms, &previous);
  lock.lock();
  in_flight_pipeline_ = 0;
  if (!done) {
    FailLocked(request, kErrorBusy);
    return;
  }
  const uint64_t previous_owner = installed_owner_;
  installed_pipeline_ = request.pipeline;
  installed_owner_ = request.pipeline != 0 ? request.owner : 0;
  ReturnLocked(previous_owner, Address(previous));
  CompleteLocked(request, std::string(), 0);
}

bool RootDenoiserControl::TickLocked(int64_t now_ms, bool* changed) {
  const bool wanted = hooks_.wanted && hooks_.wanted();
  const bool installed = slot_->installed();
  const uint32_t process_rate = hooks_.process_rate ? hooks_.process_rate() : 0;
  const uint64_t frames =
      hooks_.processed_frames ? hooks_.processed_frames() : 0;
  const uint64_t epoch = hooks_.stream_epoch ? hooks_.stream_epoch() : 0;
  const std::optional<bool> change = fallback_.Update(
      now_ms, wanted, installed, slot_->bypass(), slot_->rate(), process_rate,
      frames, epoch, ArrivingLocked(now_ms));
  *changed = change.has_value();
  if (change) {
    DecideFallbackLocked(*change);
  }
  return wanted || installed || !queue_.empty() || waiting_.has_value() ||
         fallback_.on();
}

bool RootDenoiserControl::ArrivingLocked(int64_t now_ms) const {
  const auto arriving = [this, now_ms](const Request& request) {
    return request.pipeline != 0 &&
           now_ms - request.since_ms < timing_.install_wait_ms;
  };
  return (waiting_ && arriving(*waiting_)) ||
         std::any_of(queue_.begin(), queue_.end(), arriving);
}

void RootDenoiserControl::DecideFallbackLocked(bool on) {
  fallback_on_.store(on, std::memory_order_relaxed);
  if (hooks_.fallback_decided) {
    hooks_.fallback_decided(on);
  }
}

void RootDenoiserControl::ApplyFallback() {
  // Whoever applies last reads the latest decision, so the APM ends where
  // the decisions did whatever order the deciding threads get here in.
  std::lock_guard<std::mutex> lock(fallback_mutex_);
  const bool on = fallback_on_.load(std::memory_order_relaxed);
  if (on == fallback_applied_) {
    return;
  }
  fallback_applied_ = on;
  if (hooks_.set_fallback) {
    hooks_.set_fallback(on);
  }
}

bool RootDenoiserControl::HeldLocked(uint64_t pipeline) const {
  if (pipeline == 0) {
    return false;
  }
  if (pipeline == installed_pipeline_ || pipeline == in_flight_pipeline_ ||
      (waiting_ && waiting_->pipeline == pipeline)) {
    return true;
  }
  return std::any_of(
      queue_.begin(), queue_.end(),
      [pipeline](const Request& request) { return request.pipeline == pipeline; });
}

void RootDenoiserControl::CompleteLocked(const Request& request,
                                         std::string error,
                                         uint64_t released_pipeline) {
  RootDenoiserCompletion completion;
  completion.owner = request.owner;
  completion.async_id = request.async_id;
  completion.error = std::move(error);
  completion.pipeline = released_pipeline;
  completions_.push_back(std::move(completion));
}

void RootDenoiserControl::FailLocked(const Request& request,
                                     const char* error) {
  if (HeldLocked(request.pipeline)) {
    // The same pipeline is still held by another request or installed: an
    // error would have the app release it. It comes back with that one.
    CompleteLocked(request, std::string(), 0);
    return;
  }
  CompleteLocked(request, error, request.pipeline);
}

void RootDenoiserControl::ReturnLocked(uint64_t owner, uint64_t pipeline) {
  if (pipeline == 0 || HeldLocked(pipeline) ||
      std::any_of(returned_.begin(), returned_.end(),
                  [pipeline](const RootDenoiserReturned& returned) {
                    return returned.pipeline == pipeline;
                  })) {
    return;
  }
  returned_.push_back(RootDenoiserReturned{owner, pipeline});
}

namespace {

void AddUnique(std::vector<uint64_t>& pipelines, uint64_t pipeline) {
  if (pipeline != 0 &&
      std::find(pipelines.begin(), pipelines.end(), pipeline) ==
          pipelines.end()) {
    pipelines.push_back(pipeline);
  }
}

int MsUntil(std::chrono::steady_clock::time_point deadline) {
  const int64_t left = std::chrono::duration_cast<std::chrono::milliseconds>(
                           deadline - std::chrono::steady_clock::now())
                           .count();
  return static_cast<int>(std::max<int64_t>(0, left));
}

}  // namespace

void RootDenoiserControl::DrainRequestsLocked(const char* error,
                                              std::vector<uint64_t>* into) {
  std::deque<Request> drained = std::exchange(queue_, {});
  if (waiting_) {
    drained.push_front(*waiting_);
    waiting_.reset();
  }
  for (const Request& request : drained) {
    if (HeldLocked(request.pipeline)) {
      // Installed (or being installed): it comes back with the slot's copy.
      CompleteLocked(request, std::string(), 0);
      continue;
    }
    CompleteLocked(request, error, 0);
    if (into) {
      AddUnique(*into, request.pipeline);
    } else {
      ReturnLocked(request.owner, request.pipeline);
    }
  }
}

bool RootDenoiserControl::RemoveWithin(
    std::chrono::steady_clock::time_point deadline,
    uint64_t* previous) {
  std::unique_lock<std::timed_mutex> slot_lock(slot_mutex_, std::defer_lock);
  if (!slot_lock.try_lock_until(deadline)) {
    return false;
  }
  RootFxPipeline* removed = nullptr;
  if (!slot_->Remove(MsUntil(deadline), &removed)) {
    return false;
  }
  std::lock_guard<std::mutex> lock(mutex_);
  *previous = Address(removed);
  installed_pipeline_ = 0;
  return true;
}

RootDenoiserReturnAll RootDenoiserControl::ReturnAll() {
  const auto deadline = std::chrono::steady_clock::now() +
                        std::chrono::milliseconds(
                            std::max(0, timing_.control_timeout_ms));
  RootDenoiserReturnAll result;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    DrainRequestsLocked(kErrorReturned, &result.pipelines);
  }
  uint64_t previous = 0;
  const bool removed = RemoveWithin(deadline, &previous);
  {
    std::lock_guard<std::mutex> lock(mutex_);
    if (removed) {
      installed_owner_ = 0;
      if (!HeldLocked(previous)) {
        AddUnique(result.pipelines, previous);
      }
    } else {
      result.error =
          std::string(kErrorBusy) + "; the installed pipeline stays installed";
    }
    for (const RootDenoiserReturned& returned : returned_) {
      AddUnique(result.pipelines, returned.pipeline);
    }
    returned_.clear();
  }
  wake_.notify_all();
  return result;
}

void RootDenoiserControl::Reset() {
  const auto deadline = std::chrono::steady_clock::now() +
                        std::chrono::milliseconds(
                            std::max(0, timing_.control_timeout_ms));
  {
    std::lock_guard<std::mutex> lock(mutex_);
    DrainRequestsLocked(kErrorSessionEnded, nullptr);
  }
  uint64_t previous = 0;
  const bool removed = RemoveWithin(deadline, &previous);
  {
    std::lock_guard<std::mutex> lock(mutex_);
    if (removed) {
      const uint64_t owner = installed_owner_;
      installed_owner_ = 0;
      ReturnLocked(owner, previous);
    }
    fallback_.Reset();
    DecideFallbackLocked(false);
  }
  ApplyFallback();
  wake_.notify_all();
}

std::vector<RootDenoiserCompletion> RootDenoiserControl::TakeCompletions() {
  std::lock_guard<std::mutex> lock(mutex_);
  return std::exchange(completions_, {});
}

std::vector<RootDenoiserReturned> RootDenoiserControl::TakeReturned() {
  std::lock_guard<std::mutex> lock(mutex_);
  return std::exchange(returned_, {});
}

bool RootDenoiserControl::fallback() const {
  return fallback_on_.load(std::memory_order_relaxed);
}

size_t RootDenoiserControl::pending() const {
  std::lock_guard<std::mutex> lock(mutex_);
  return queue_.size() + (waiting_ ? 1 : 0);
}

// =============================================================================
// Notification handler
// =============================================================================

RootFxNotificationHandler* RootDenoiserNotificationHandler() {
  return &g_handler;
}

RootDenoiserNotificationCounts root_denoiser_notification_counts() {
  RootDenoiserNotificationCounts counts{};
  counts.slow = g_slow_notifications.load(std::memory_order_relaxed);
  counts.other = g_other_notifications.load(std::memory_order_relaxed);
  return counts;
}

size_t root_denoiser_notification_handler() {
  if constexpr (kRootDenoiserSupported) {
    return static_cast<size_t>(
        reinterpret_cast<uintptr_t>(RootDenoiserNotificationHandler()));
  } else {
    return 0;
  }
}

bool root_denoiser_supported() {
  return kRootDenoiserSupported;
}

#if defined(ROOTAPP_HARNESS)
namespace {

struct FakeNotification {
  const RootFxNotificationVtbl* vtbl;
  int32_t code;
};

int32_t FakeNotificationCode(const void* self) {
  return static_cast<const FakeNotification*>(self)->code;
}

const RootFxNotificationVtbl kFakeNotificationVtbl = {&FakeNotificationCode};

}  // namespace

void root_denoiser_notify_for_test(rust::Slice<const int32_t> codes,
                                   bool include_null) {
  RootFxNotificationHandler* handler = RootDenoiserNotificationHandler();
  if (include_null) {
    handler->vtbl->receive(handler, nullptr);
  }
  for (int32_t code : codes) {
    FakeNotification notification{&kFakeNotificationVtbl, code};
    handler->vtbl->receive(handler, &notification);
  }
}

// =============================================================================
// RootFakeFxPipeline
// =============================================================================

const RootFxPipelineVtbl RootFakeFxPipeline::kVtbl = {
    &RootFakeFxPipeline::Release,
    &RootFakeFxPipeline::IsEnabled,
    &RootFakeFxPipeline::SetEnabled,
    &RootFakeFxPipeline::Power,
    &RootFakeFxPipeline::SetPower,
    &RootFakeFxPipeline::SetLatencyMode,
    &RootFakeFxPipeline::LatencyMode,
    &RootFakeFxPipeline::SetNotificationHandler,
    &RootFakeFxPipeline::Process,
    &RootFakeFxPipeline::Flush,
};

RootFakeFxPipeline::RootFakeFxPipeline(const RootFakeFxConfig& config)
    : object_{&kVtbl, this},
      config_(config),
      fifo_(std::make_unique<float[]>(kCapacity)),
      sleep_us_(config.sleep_us) {
  Restart();
}

RootFakeFxPipeline::~RootFakeFxPipeline() = default;

void* RootFakeFxPipeline::pipeline() {
  return &object_;
}

void RootFakeFxPipeline::set_sleep_us(uint32_t sleep_us) {
  sleep_us_.store(sleep_us, std::memory_order_relaxed);
}

void RootFakeFxPipeline::set_empty(bool empty) {
  empty_.store(empty, std::memory_order_relaxed);
}

void RootFakeFxPipeline::Poison() {
  poisoned_.store(true, std::memory_order_relaxed);
}

uint64_t RootFakeFxPipeline::process_calls() const {
  return process_calls_.load(std::memory_order_relaxed);
}

uint64_t RootFakeFxPipeline::silent_calls() const {
  return silent_calls_.load(std::memory_order_relaxed);
}

uint64_t RootFakeFxPipeline::flush_calls() const {
  return flush_calls_.load(std::memory_order_relaxed);
}

uint64_t RootFakeFxPipeline::power_sets() const {
  return power_sets_.load(std::memory_order_relaxed);
}

float RootFakeFxPipeline::last_power() const {
  return power_.load(std::memory_order_relaxed);
}

uint64_t RootFakeFxPipeline::calls_after_poison() const {
  return calls_after_poison_.load(std::memory_order_relaxed);
}

uint64_t RootFakeFxPipeline::releases() const {
  return releases_.load(std::memory_order_relaxed);
}

RootFakeFxPipeline* RootFakeFxPipeline::Self(const void* object) {
  return static_cast<const Object*>(object)->self;
}

void RootFakeFxPipeline::Touch() {
  if (poisoned_.load(std::memory_order_relaxed)) {
    calls_after_poison_.fetch_add(1, std::memory_order_relaxed);
  }
}

void RootFakeFxPipeline::Spin() const {
  // A sleep can last a timer tick (15.6 ms on Windows) longer than asked.
  const uint32_t sleep_us = sleep_us_.load(std::memory_order_relaxed);
  const int64_t until_us = NowUs() + sleep_us;
  while (sleep_us > 0 && NowUs() < until_us) {
  }
}

void RootFakeFxPipeline::Restart() {
  const size_t delay = std::min<size_t>(config_.delay_samples, kCapacity);
  std::fill(fifo_.get(), fifo_.get() + delay, 0.0f);
  head_ = 0;
  size_ = delay;
  calls_since_restart_ = 0;
}

void RootFakeFxPipeline::Release(void* object) {
  Self(object)->releases_.fetch_add(1, std::memory_order_relaxed);
}

bool RootFakeFxPipeline::IsEnabled(const void* object) {
  Self(object)->Touch();
  return true;
}

void RootFakeFxPipeline::SetEnabled(void* object, bool) {
  Self(object)->Touch();
}

float RootFakeFxPipeline::Power(const void* object) {
  RootFakeFxPipeline* self = Self(object);
  self->Touch();
  return self->power_.load(std::memory_order_relaxed);
}

void RootFakeFxPipeline::SetPower(void* object, float power) {
  RootFakeFxPipeline* self = Self(object);
  self->Touch();
  self->Spin();
  self->power_sets_.fetch_add(1, std::memory_order_relaxed);
  self->power_.store(power, std::memory_order_relaxed);
}

void RootFakeFxPipeline::SetLatencyMode(void* object, int32_t) {
  Self(object)->Touch();
}

int32_t RootFakeFxPipeline::LatencyMode(const void* object) {
  Self(object)->Touch();
  return 1;
}

void RootFakeFxPipeline::SetNotificationHandler(void* object, void*) {
  Self(object)->Touch();
}

uint32_t RootFakeFxPipeline::Process(void* object,
                                     const void* in,
                                     uint32_t in_frames,
                                     void* out,
                                     uint32_t out_frames) {
  RootFakeFxPipeline* self = Self(object);
  self->Touch();
  self->process_calls_.fetch_add(1, std::memory_order_relaxed);
  self->Spin();
  const auto* input = static_cast<const float*>(in);
  if (input != nullptr && std::all_of(input, input + in_frames,
                                      [](float v) { return v == 0.0f; })) {
    self->silent_calls_.fetch_add(1, std::memory_order_relaxed);
  }
  for (uint32_t i = 0; i < in_frames && self->size_ < kCapacity; ++i) {
    const float sample = input ? input[i] : 0.0f;
    self->fifo_[(self->head_ + self->size_) % kCapacity] =
        std::round(self->config_.scale * sample);
    ++self->size_;
  }
  const uint32_t call = ++self->calls_since_restart_;
  const RootFakeFxConfig& config = self->config_;
  size_t take = in_frames;
  if (self->empty_.load(std::memory_order_relaxed) ||
      (config.empty_count > 0 && call >= config.empty_from &&
       call - config.empty_from < config.empty_count)) {
    take = 0;
  } else if (config.burst_call != 0 && call == config.burst_call) {
    take = 2 * static_cast<size_t>(in_frames);
  }
  auto* output = static_cast<float*>(out);
  take = output ? std::min({take, self->size_, static_cast<size_t>(out_frames)})
                : 0;
  for (size_t i = 0; i < take; ++i) {
    output[i] = self->fifo_[self->head_];
    self->head_ = (self->head_ + 1) % kCapacity;
    --self->size_;
  }
  return static_cast<uint32_t>(take);
}

uint32_t RootFakeFxPipeline::Flush(void* object, void*, uint32_t) {
  RootFakeFxPipeline* self = Self(object);
  self->Touch();
  self->Spin();
  self->flush_calls_.fetch_add(1, std::memory_order_relaxed);
  self->Restart();
  return 0;
}

// =============================================================================
// RootDenoiserHarness
// =============================================================================

struct RootDenoiserHarness::State {
  std::shared_ptr<RootDenoiserSlot> slot = std::make_shared<RootDenoiserSlot>();
  std::vector<std::unique_ptr<RootFakeFxPipeline>> fakes;
  std::atomic<bool> speaking{false};
  std::atomic<bool> wanted{false};
  std::atomic<uint32_t> process_rate{0};
  std::atomic<bool> frames_flowing{true};
  std::atomic<uint64_t> frames{0};
  std::atomic<uint64_t> epoch{0};
  std::atomic<bool> block_fallback{false};
  std::atomic<bool> fallback_decided{false};
  std::mutex changes_mutex;
  std::vector<uint8_t> changes;
  RootNsFallback fallback;
  std::thread audio;
  std::atomic<bool> audio_running{false};
  std::atomic<uint64_t> audio_frames{0};
  std::unique_ptr<RootDenoiserControl> control;

  explicit State(int64_t fallback_grace_ms) : fallback(fallback_grace_ms) {}
};

RootDenoiserHarness::RootDenoiserHarness(int64_t install_wait_ms,
                                         int64_t fallback_grace_ms)
    : state_(std::make_unique<State>(fallback_grace_ms)) {
  State* state = state_.get();
  RootDenoiserControl::Hooks hooks;
  hooks.speaking = [state] {
    return state->speaking.load(std::memory_order_relaxed);
  };
  hooks.wanted = [state] {
    return state->wanted.load(std::memory_order_relaxed);
  };
  hooks.process_rate = [state] {
    return state->process_rate.load(std::memory_order_relaxed);
  };
  hooks.processed_frames = [state] {
    return state->frames_flowing.load(std::memory_order_relaxed)
               ? state->frames.fetch_add(1, std::memory_order_relaxed) + 1
               : state->frames.load(std::memory_order_relaxed);
  };
  hooks.stream_epoch = [state] {
    return state->epoch.load(std::memory_order_relaxed);
  };
  hooks.fallback_decided = [state](bool on) {
    state->fallback_decided.store(on, std::memory_order_relaxed);
  };
  hooks.set_fallback = [state](bool on) {
    // As the decorator waiting for an APM re-initialisation.
    while (state->block_fallback.load(std::memory_order_relaxed)) {
      std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
    std::lock_guard<std::mutex> lock(state->changes_mutex);
    state->changes.push_back(on ? 1 : 0);
  };
  RootDenoiserControl::Timing timing;
  timing.install_wait_ms = install_wait_ms;
  timing.fallback_grace_ms = fallback_grace_ms;
  state_->control = std::make_unique<RootDenoiserControl>(
      state_->slot, std::move(hooks), timing);
}

RootDenoiserHarness::~RootDenoiserHarness() {
  state_->control = nullptr;
  stop_audio();
}

size_t RootDenoiserHarness::add_fake(const RootFakeFxSpec& spec) const {
  RootFakeFxConfig config;
  config.scale = spec.scale;
  config.delay_samples = spec.delay_samples;
  config.burst_call = spec.burst_call;
  config.empty_from = spec.empty_from;
  config.empty_count = spec.empty_count;
  config.sleep_us = spec.sleep_us;
  state_->fakes.push_back(std::make_unique<RootFakeFxPipeline>(config));
  return state_->fakes.size() - 1;
}

uint64_t RootDenoiserHarness::fake_pipeline(size_t index) const {
  return Address(state_->fakes.at(index)->pipeline());
}

RootFakeFxCounts RootDenoiserHarness::fake_counts(size_t index) const {
  const RootFakeFxPipeline& fake = *state_->fakes.at(index);
  RootFakeFxCounts counts{};
  counts.process_calls = fake.process_calls();
  counts.flush_calls = fake.flush_calls();
  counts.power_sets = fake.power_sets();
  counts.last_power = fake.last_power();
  counts.calls_after_poison = fake.calls_after_poison();
  counts.releases = fake.releases();
  return counts;
}

void RootDenoiserHarness::poison_fake(size_t index) const {
  state_->fakes.at(index)->Poison();
}

void RootDenoiserHarness::set_fake_sleep_us(size_t index,
                                            uint32_t sleep_us) const {
  state_->fakes.at(index)->set_sleep_us(sleep_us);
}

void RootDenoiserHarness::set_fake_empty(size_t index, bool empty) const {
  state_->fakes.at(index)->set_empty(empty);
}

bool RootDenoiserHarness::install_now(size_t index, uint32_t rate) const {
  return state_->slot->InstallForTest(state_->fakes.at(index)->pipeline(),
                                      rate);
}

uint64_t RootDenoiserHarness::remove_now() const {
  return Address(state_->slot->RemoveForTest());
}

uint64_t RootDenoiserHarness::swap(uint64_t pipeline, uint32_t rate) const {
  RootFxPipeline* previous = nullptr;
  const bool done =
      pipeline != 0
          ? state_->slot->Install(PipelineAt(pipeline), rate,
                                  RootDenoiserSlot::kControlTimeoutMs,
                                  &previous)
          : state_->slot->Remove(RootDenoiserSlot::kControlTimeoutMs,
                                 &previous);
  return done ? Address(previous) : std::numeric_limits<uint64_t>::max();
}

bool RootDenoiserHarness::hold_slot(bool hold) const {
  return state_->slot->HoldForTest(hold);
}

RootDenoiseTestFrame RootDenoiserHarness::process(
    uint32_t rate,
    rust::Slice<const float> input,
    bool flush,
    float power,
    size_t out_capacity) const {
  std::vector<float> out(std::max<size_t>(out_capacity, 1));
  const RootDenoiseResult result = state_->slot->ProcessAudio(
      input.data(), input.size(), static_cast<int>(rate), flush, power,
      out.data(), out_capacity);
  RootDenoiseTestFrame frame{};
  frame.kind = static_cast<uint8_t>(result.kind);
  frame.samples = result.samples;
  frame.restart = result.restart;
  for (size_t i = 0; i < result.samples && i < out.size(); ++i) {
    frame.out.push_back(out[i]);
  }
  return frame;
}

void RootDenoiserHarness::latch_bypass(uint32_t reason) const {
  state_->slot->LatchBypass(static_cast<RootDenoiserBypass>(reason));
}

RootDenoiserSlotInfo RootDenoiserHarness::slot_info(bool take_max) const {
  const RootDenoiserSlotStats stats = state_->slot->ReadStats(take_max);
  RootDenoiserSlotInfo info{};
  info.installed = stats.installed;
  info.rate = stats.rate;
  info.bypass = static_cast<uint32_t>(stats.bypass);
  info.generation = stats.generation;
  info.frames = stats.frames;
  info.misses = stats.misses;
  info.empty_outputs = stats.empty_outputs;
  info.slow_frames = stats.slow_frames;
  info.bypassed_frames = stats.bypassed_frames;
  info.installs = stats.installs;
  info.frames_until_first_output = stats.frames_until_first_output;
  info.process_max_us = stats.process_max_us;
  for (uint64_t count : stats.histogram) {
    info.histogram.push_back(count);
  }
  return info;
}

void RootDenoiserHarness::start_audio(uint32_t rate,
                                      uint32_t latch_every) const {
  if (state_->audio.joinable()) {
    return;
  }
  state_->audio_running.store(true, std::memory_order_relaxed);
  State* state = state_.get();
  state_->audio = std::thread([state, rate, latch_every] {
    const size_t frames = rate / 100;
    std::vector<float> in(frames);
    for (size_t i = 0; i < frames; ++i) {
      in[i] = static_cast<float>(
          1000.0 * std::sin(6.283185307179586 * 440.0 * static_cast<double>(i) /
                            static_cast<double>(rate)));
    }
    std::vector<float> out(RootDenoiserSlot::kOutputFrames * frames + 1);
    while (state->audio_running.load(std::memory_order_relaxed)) {
      state->slot->ProcessAudio(in.data(), frames, static_cast<int>(rate),
                                false, 0.5f, out.data(),
                                RootDenoiserSlot::kOutputFrames * frames);
      const uint64_t done =
          state->audio_frames.fetch_add(1, std::memory_order_relaxed) + 1;
      if (latch_every != 0 && done % latch_every == 0) {
        state->slot->LatchBypass(RootDenoiserBypass::kOutputStalled);
      }
      // Back to back, but leaving the slot free between frames as the 10 ms
      // cadence of a real capture thread does.
      const int64_t resume_us = NowUs() + 50;
      while (NowUs() < resume_us) {
        std::this_thread::yield();
      }
    }
  });
}

uint64_t RootDenoiserHarness::stop_audio() const {
  state_->audio_running.store(false, std::memory_order_relaxed);
  if (state_->audio.joinable()) {
    state_->audio.join();
  }
  return state_->audio_frames.load(std::memory_order_relaxed);
}

void RootDenoiserHarness::set_speaking(bool speaking) const {
  state_->speaking.store(speaking, std::memory_order_relaxed);
}

void RootDenoiserHarness::set_wanted(bool wanted) const {
  state_->wanted.store(wanted, std::memory_order_relaxed);
}

void RootDenoiserHarness::set_process_rate(uint32_t rate) const {
  state_->process_rate.store(rate, std::memory_order_relaxed);
}

void RootDenoiserHarness::set_frames_flowing(bool flowing) const {
  state_->frames_flowing.store(flowing, std::memory_order_relaxed);
}

void RootDenoiserHarness::bump_stream_epoch() const {
  state_->epoch.fetch_add(1, std::memory_order_relaxed);
}

void RootDenoiserHarness::request(uint64_t owner,
                                  uint64_t async_id,
                                  uint64_t pipeline,
                                  uint32_t rate) const {
  state_->control->Set(owner, async_id, pipeline, rate);
}

void RootDenoiserHarness::wake() const {
  state_->control->Wake();
}

RootDenoiserTestReturnAll RootDenoiserHarness::return_all() const {
  const RootDenoiserReturnAll all = state_->control->ReturnAll();
  RootDenoiserTestReturnAll result{};
  for (uint64_t pipeline : all.pipelines) {
    result.pipelines.push_back(pipeline);
  }
  result.error = rust::String(all.error);
  return result;
}

void RootDenoiserHarness::reset() const {
  state_->control->Reset();
}

rust::Vec<RootDenoiserTestCompletion> RootDenoiserHarness::take_completions()
    const {
  rust::Vec<RootDenoiserTestCompletion> result;
  for (const RootDenoiserCompletion& completion :
       state_->control->TakeCompletions()) {
    RootDenoiserTestCompletion item{};
    item.owner = completion.owner;
    item.async_id = completion.async_id;
    item.error = rust::String(completion.error);
    item.pipeline = completion.pipeline;
    result.push_back(std::move(item));
  }
  return result;
}

rust::Vec<RootDenoiserTestReturned> RootDenoiserHarness::take_returned() const {
  rust::Vec<RootDenoiserTestReturned> result;
  for (const RootDenoiserReturned& returned : state_->control->TakeReturned()) {
    result.push_back(
        RootDenoiserTestReturned{returned.owner, returned.pipeline});
  }
  return result;
}

rust::Vec<uint8_t> RootDenoiserHarness::take_fallback_changes() const {
  std::lock_guard<std::mutex> lock(state_->changes_mutex);
  rust::Vec<uint8_t> result;
  for (uint8_t change : state_->changes) {
    result.push_back(change);
  }
  state_->changes.clear();
  return result;
}

bool RootDenoiserHarness::fallback() const {
  return state_->control->fallback();
}

bool RootDenoiserHarness::fallback_decided() const {
  return state_->fallback_decided.load(std::memory_order_relaxed);
}

void RootDenoiserHarness::block_fallback(bool block) const {
  state_->block_fallback.store(block, std::memory_order_relaxed);
}

size_t RootDenoiserHarness::pending() const {
  return state_->control->pending();
}

int8_t RootDenoiserHarness::fallback_update(int64_t now_ms,
                                            bool wanted,
                                            bool installed,
                                            uint32_t bypass,
                                            uint32_t pipeline_rate,
                                            uint32_t process_rate,
                                            uint64_t processed_frames,
                                            uint64_t stream_epoch,
                                            bool arriving) const {
  const std::optional<bool> change = state_->fallback.Update(
      now_ms, wanted, installed, static_cast<RootDenoiserBypass>(bypass),
      pipeline_rate, process_rate, processed_frames, stream_epoch, arriving);
  if (!change) {
    return -1;
  }
  return *change ? 1 : 0;
}

std::unique_ptr<RootDenoiserHarness> new_root_denoiser_harness(
    int64_t install_wait_ms,
    int64_t fallback_grace_ms) {
  return std::make_unique<RootDenoiserHarness>(install_wait_ms,
                                               fallback_grace_ms);
}
#endif

}  // namespace livekit_ffi
