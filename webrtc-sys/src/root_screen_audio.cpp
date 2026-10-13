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

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstring>
#include <limits>
#include <utility>

#include "api/audio/audio_view.h"
#include "common_audio/resampler/include/push_resampler.h"

namespace livekit_ffi {

namespace {

constexpr float kPi = 3.14159265358979f;

class SteadyClock final : public ScreenAudioClock {
 public:
  int64_t NowUs() const override {
    return std::chrono::duration_cast<std::chrono::microseconds>(
               std::chrono::steady_clock::now().time_since_epoch())
        .count();
  }
};

void RaiseMax(std::atomic<uint64_t>& slot, uint64_t value) {
  uint64_t previous = slot.load(std::memory_order_relaxed);
  while (value > previous &&
         !slot.compare_exchange_weak(previous, value, std::memory_order_relaxed)) {
  }
}

uint64_t ReadMax(std::atomic<uint64_t>& slot, bool reset) {
  return reset ? slot.exchange(0, std::memory_order_relaxed)
               : slot.load(std::memory_order_relaxed);
}

// Raised-cosine fade over the first (rising) or last (falling) frames.
void Fade(float* stereo, size_t frames, bool rising) {
  size_t length = std::min(frames, ScreenAudioPipe::kFadeFrames);
  size_t start = rising ? 0 : frames - length;
  for (size_t j = 0; j < length; ++j) {
    float t = (static_cast<float>(j) + 1.0f) / (static_cast<float>(length) + 1.0f);
    float gain = rising ? 0.5f - 0.5f * std::cos(kPi * t)
                        : 0.5f + 0.5f * std::cos(kPi * t);
    stereo[(start + j) * 2] *= gain;
    stereo[(start + j) * 2 + 1] *= gain;
  }
}

int16_t ToInt16(float value, uint64_t* clipped) {
  if (!std::isfinite(value)) {
    value = 0.0f;
  } else if (value > 1.0f) {
    value = 1.0f;
    ++*clipped;
  } else if (value < -1.0f) {
    value = -1.0f;
    ++*clipped;
  }
  return static_cast<int16_t>(value * 32767.0f);
}

std::atomic<bool> g_fake_backend{false};

}  // namespace

const ScreenAudioClock* ScreenAudioSteadyClock() {
  static const SteadyClock* clock = new SteadyClock();
  return clock;
}

// ScreenAudioRing

ScreenAudioRing::ScreenAudioRing(size_t min_frames) {
  size_t capacity = 1;
  while (capacity < min_frames)
    capacity <<= 1;
  buffer_.assign(capacity * 2, 0.0f);
  mask_ = capacity - 1;
}

size_t ScreenAudioRing::Count() const {
  uint64_t read = read_.load(std::memory_order_acquire);
  uint64_t write = write_.load(std::memory_order_acquire);
  return static_cast<size_t>(write - read);
}

size_t ScreenAudioRing::Write(const float* stereo, size_t frames) {
  uint64_t write = write_.load(std::memory_order_relaxed);
  uint64_t read = read_.load(std::memory_order_acquire);
  size_t free = capacity() - static_cast<size_t>(write - read);
  size_t count = std::min(free, frames);
  if (count == 0)
    return 0;
  size_t start = static_cast<size_t>(write) & mask_;
  size_t first = std::min(count, capacity() - start);
  std::memcpy(&buffer_[start * 2], stereo, first * 2 * sizeof(float));
  if (count > first)
    std::memcpy(&buffer_[0], stereo + first * 2,
                (count - first) * 2 * sizeof(float));
  write_.store(write + count, std::memory_order_release);
  return count;
}

size_t ScreenAudioRing::WriteZeros(size_t frames) {
  uint64_t write = write_.load(std::memory_order_relaxed);
  uint64_t read = read_.load(std::memory_order_acquire);
  size_t free = capacity() - static_cast<size_t>(write - read);
  size_t count = std::min(free, frames);
  if (count == 0)
    return 0;
  size_t start = static_cast<size_t>(write) & mask_;
  size_t first = std::min(count, capacity() - start);
  std::fill_n(&buffer_[start * 2], first * 2, 0.0f);
  if (count > first)
    std::fill_n(&buffer_[0], (count - first) * 2, 0.0f);
  write_.store(write + count, std::memory_order_release);
  return count;
}

size_t ScreenAudioRing::Read(float* stereo, size_t frames) {
  uint64_t read = read_.load(std::memory_order_relaxed);
  uint64_t write = write_.load(std::memory_order_acquire);
  size_t count = std::min(static_cast<size_t>(write - read), frames);
  if (count == 0)
    return 0;
  size_t start = static_cast<size_t>(read) & mask_;
  size_t first = std::min(count, capacity() - start);
  std::memcpy(stereo, &buffer_[start * 2], first * 2 * sizeof(float));
  if (count > first)
    std::memcpy(stereo + first * 2, &buffer_[0],
                (count - first) * 2 * sizeof(float));
  read_.store(read + count, std::memory_order_release);
  return count;
}

size_t ScreenAudioRing::Peek(size_t offset, float* stereo, size_t frames) const {
  uint64_t read = read_.load(std::memory_order_relaxed) + offset;
  uint64_t write = write_.load(std::memory_order_acquire);
  if (write <= read)
    return 0;
  size_t count = std::min(static_cast<size_t>(write - read), frames);
  size_t start = static_cast<size_t>(read) & mask_;
  size_t first = std::min(count, capacity() - start);
  std::memcpy(stereo, &buffer_[start * 2], first * 2 * sizeof(float));
  if (count > first)
    std::memcpy(stereo + first * 2, &buffer_[0],
                (count - first) * 2 * sizeof(float));
  return count;
}

size_t ScreenAudioRing::Discard(size_t frames) {
  uint64_t read = read_.load(std::memory_order_relaxed);
  uint64_t write = write_.load(std::memory_order_acquire);
  size_t count = std::min(static_cast<size_t>(write - read), frames);
  read_.store(read + count, std::memory_order_release);
  return count;
}

// ScreenAudioDriftResampler

ScreenAudioDriftResampler::ScreenAudioDriftResampler() {
  Reset(false);
}

double ScreenAudioDriftResampler::ratio() const {
  return static_cast<double>(step_) / static_cast<double>(kOne);
}

void ScreenAudioDriftResampler::set_ratio(double ratio) {
  double clamped = std::isfinite(ratio)
                       ? std::min(1 + kMaxRatioDeviation,
                                  std::max(1 - kMaxRatioDeviation, ratio))
                       : 1.0;
  step_ = std::llround(clamped * static_cast<double>(kOne));
}

void ScreenAudioDriftResampler::Reset(bool with_lookahead) {
  history_.fill(0.0f);
  position_ = with_lookahead ? (kDelayFrames + 1) * kOne : kOne;
  weights_fraction_ = -1;
}

size_t ScreenAudioDriftResampler::InputFramesFor(size_t output_frames) const {
  if (output_frames == 0)
    return 0;
  int64_t position =
      position_ + static_cast<int64_t>(output_frames - 1) * step_;
  return static_cast<size_t>(position >> 32);
}

size_t ScreenAudioDriftResampler::Process(const float* input,
                                          size_t input_frames,
                                          float* output,
                                          size_t output_frames,
                                          size_t* consumed) {
  size_t i = 0;
  size_t o = 0;
  while (true) {
    while (position_ >= kOne) {
      if (i == input_frames) {
        *consumed = i;
        return o;
      }
      Push(input + i * 2);
      ++i;
      position_ -= kOne;
    }
    if (o == output_frames)
      break;
    Interpolate(output + o * 2);
    ++o;
    position_ += step_;
  }
  *consumed = i;
  return o;
}

void ScreenAudioDriftResampler::Push(const float* frame) {
  std::memmove(history_.data(), history_.data() + 2,
               (kTaps - 1) * 2 * sizeof(float));
  history_[(kTaps - 1) * 2] = frame[0];
  history_[(kTaps - 1) * 2 + 1] = frame[1];
}

void ScreenAudioDriftResampler::Interpolate(float* destination) {
  int64_t fraction = position_ & (kOne - 1);
  if (fraction == 0) {
    destination[0] = history_[kCenter * 2];
    destination[1] = history_[kCenter * 2 + 1];
    return;
  }
  if (fraction != weights_fraction_) {
    ComputeWeights(static_cast<double>(fraction) / static_cast<double>(kOne),
                   weights_.data());
    weights_fraction_ = fraction;
  }
  for (int c = 0; c < 2; ++c) {
    double sum = 0;
    for (int k = 0; k < kTaps; ++k)
      sum += weights_[k] * history_[k * 2 + c];
    destination[c] = static_cast<float>(sum);
  }
}

// Lagrange basis over the points -2..3 evaluated at x in [0, 1).
void ScreenAudioDriftResampler::ComputeWeights(double x, double* weights) {
  double a = x + 2, b = x + 1, c = x, d = x - 1, e = x - 2, f = x - 3;
  double ab = a * b, abc = ab * c, abcd = abc * d;
  double ef = e * f, def = d * ef, cdef = c * def;
  weights[0] = -(b * cdef) / 120;
  weights[1] = a * cdef / 24;
  weights[2] = -(ab * def) / 12;
  weights[3] = abc * ef / 12;
  weights[4] = -(abcd * f) / 24;
  weights[5] = abcd * e / 120;
}

// Splice

size_t ScreenAudioBestCut(const float* head,
                          size_t head_frames,
                          const float* candidates,
                          size_t candidate_frames) {
  if (candidate_frames < head_frames)
    return 0;
  size_t offsets = candidate_frames - head_frames;
  size_t length = head_frames * 2;
  size_t best = offsets;
  double best_score = -std::numeric_limits<double>::infinity();
  for (size_t offset = 0; offset <= offsets; ++offset) {
    const float* candidate = candidates + offset * 2;
    double dot = 0;
    double energy = 1e-12;
    for (size_t i = 0; i < length; ++i) {
      dot += static_cast<double>(head[i]) * candidate[i];
      energy += static_cast<double>(candidate[i]) * candidate[i];
    }
    double score = dot / std::sqrt(energy);
    if (score > best_score) {
      best_score = score;
      best = offset;
    }
  }
  return best;
}

void ScreenAudioCrossfade(const float* from,
                          const float* to,
                          float* destination,
                          size_t frames) {
  for (size_t frame = 0; frame < frames; ++frame) {
    float rise = 0.5f - 0.5f * std::cos(kPi * (static_cast<float>(frame) + 1) /
                                        (static_cast<float>(frames) + 1));
    for (size_t c = 0; c < 2; ++c) {
      size_t i = frame * 2 + c;
      destination[i] = from[i] * (1 - rise) + to[i] * rise;
    }
  }
}

// ScreenAudioPipe

ScreenAudioPipe::ScreenAudioPipe(const ScreenAudioClock* clock)
    : clock_(clock),
      ring_(kRingFrames),
      stereo_chunk_(kChunkFrames * 2, 0.0f),
      zeros_(kChunkFrames * 2, 0.0f),
      gather_(kGatherFrames * 2, 0.0f),
      out_(kFrame * 2, 0.0f),
      head_(kFadeFrames * 2, 0.0f),
      candidates_((kSearchFrames + kFadeFrames) * 2, 0.0f),
      tail_(kFadeFrames * 2, 0.0f),
      pending_(kFadeFrames * 2, 0.0f) {
  resampler_.Reset(true);
}

ScreenAudioPipe::~ScreenAudioPipe() = default;

bool ScreenAudioPipe::FormatSupported(const ScreenAudioFormat& format) {
  return format.sample_rate >= 8000 && format.sample_rate <= 192000 &&
         format.sample_rate % 100 == 0 && format.channels >= 1 &&
         format.channels <= 8;
}

bool ScreenAudioPipe::Configure(const ScreenAudioFormat& format) {
  if (!FormatSupported(format))
    return false;
  rate_ = format.sample_rate;
  channels_ = format.channels;
  in_fill_ = 0;
  if (rate_ != kRate) {
    in_frames_ = static_cast<size_t>(rate_ / 100);
    in_chunk_.assign(in_frames_ * 2, 0.0f);
    out_chunk_.assign(kFrame * 2, 0.0f);
    converter_ =
        std::make_unique<webrtc::PushResampler<float>>(in_frames_, kFrame, 2);
  } else {
    converter_.reset();
    in_frames_ = kFrame;
  }
  largest_write_frames_.store(0, std::memory_order_relaxed);
  stalls_.fetch_add(1, std::memory_order_release);
  return true;
}

// The largest delivery decays by 1/64 per delivery (an outlier, such as the
// backlog of the first packet, is forgotten in about 100 deliveries). A wait
// counts as a capture stall from 60 ms, or from twice the largest delivery's
// duration for a capture that delivers in larger packets (a PipeWire quantum
// of 4096 frames), at most 500 ms.
void ScreenAudioPipe::NoteArrival(size_t frames) {
  uint64_t frames48 = static_cast<uint64_t>(frames) * kRate /
                      static_cast<uint64_t>(rate_);
  uint64_t largest = largest_write_frames_.load(std::memory_order_relaxed);
  largest_write_frames_.store(std::max(frames48, largest - largest / 64),
                              std::memory_order_relaxed);
  int64_t now = clock_->NowUs();
  int64_t last = last_write_us_.exchange(now, std::memory_order_relaxed);
  if (last == 0 || now <= last)
    return;
  uint64_t gap = static_cast<uint64_t>(now - last);
  RaiseMax(write_max_gap_us_, gap);
  uint64_t packet_us =
      largest_write_frames_.load(std::memory_order_relaxed) * 1'000'000 / kRate;
  uint64_t stall = std::min<uint64_t>(
      kMaxStallUs, std::max<uint64_t>(kStallUs, 2 * packet_us));
  if (gap >= stall)
    stalls_.fetch_add(1, std::memory_order_release);
}

void ScreenAudioPipe::CountOverflow(size_t offered, size_t written) {
  if (written < offered)
    overflow_frames_.fetch_add(offered - written, std::memory_order_relaxed);
}

void ScreenAudioPipe::PushStereo(const float* stereo, size_t frames) {
  if (!converter_) {
    CountOverflow(frames, ring_.Write(stereo, frames));
    return;
  }
  while (frames > 0) {
    size_t take = std::min(frames, in_frames_ - in_fill_);
    std::memcpy(&in_chunk_[in_fill_ * 2], stereo, take * 2 * sizeof(float));
    in_fill_ += take;
    stereo += take * 2;
    frames -= take;
    if (in_fill_ == in_frames_) {
      converter_->Resample(
          webrtc::InterleavedView<const float>(in_chunk_.data(), in_frames_, 2),
          webrtc::InterleavedView<float>(out_chunk_.data(), kFrame, 2));
      CountOverflow(kFrame, ring_.Write(out_chunk_.data(), kFrame));
      in_fill_ = 0;
    }
  }
}

void ScreenAudioPipe::Write(const float* interleaved, size_t frames) {
  NoteArrival(frames);
  captured_frames_.fetch_add(frames, std::memory_order_relaxed);
  const size_t channels = static_cast<size_t>(channels_);
  while (frames > 0) {
    size_t n = std::min(frames, kChunkFrames);
    float* stereo = stereo_chunk_.data();
    if (channels == 2) {
      std::memcpy(stereo, interleaved, n * 2 * sizeof(float));
    } else if (channels == 1) {
      for (size_t i = 0; i < n; ++i) {
        stereo[i * 2] = interleaved[i];
        stereo[i * 2 + 1] = interleaved[i];
      }
    } else {
      for (size_t i = 0; i < n; ++i) {
        stereo[i * 2] = interleaved[i * channels];
        stereo[i * 2 + 1] = interleaved[i * channels + 1];
      }
    }
    PushStereo(stereo, n);
    interleaved += n * channels;
    frames -= n;
  }
}

void ScreenAudioPipe::WriteSilence(size_t frames) {
  NoteArrival(frames);
  captured_frames_.fetch_add(frames, std::memory_order_relaxed);
  silent_capture_frames_.fetch_add(frames, std::memory_order_relaxed);
  while (frames > 0) {
    size_t n = std::min(frames, kChunkFrames);
    PushStereo(zeros_.data(), n);
    frames -= n;
  }
}

void ScreenAudioPipe::MarkDiscontinuity() {
  discontinuities_.fetch_add(1, std::memory_order_relaxed);
}

size_t ScreenAudioPipe::Available() const {
  return ring_.Count() + pending_frames_;
}

size_t ScreenAudioPipe::Gather(float* destination, size_t frames) {
  size_t got = 0;
  if (pending_frames_ > 0) {
    got = std::min(frames, pending_frames_);
    std::memcpy(destination, &pending_[pending_offset_ * 2],
                got * 2 * sizeof(float));
    pending_offset_ += got;
    pending_frames_ -= got;
    if (pending_frames_ == 0)
      pending_offset_ = 0;
  }
  return got + ring_.Read(destination + got * 2, frames - got);
}

void ScreenAudioPipe::DropOldest(size_t frames) {
  size_t from_pending = std::min(frames, pending_frames_);
  pending_frames_ -= from_pending;
  pending_offset_ = pending_frames_ == 0 ? 0 : pending_offset_ + from_pending;
  ring_.Discard(frames - from_pending);
}

void ScreenAudioPipe::Pull(int16_t* out, int channels) {
  int64_t now = clock_->NowUs();
  if (last_pull_us_ != 0 && now > last_pull_us_)
    RaiseMax(pull_max_gap_us_, static_cast<uint64_t>(now - last_pull_us_));
  last_pull_us_ = now;
  pulls_.fetch_add(1, std::memory_order_relaxed);

  size_t produced = running_ ? PullRunning() : PullWaiting();
  Emit(out, channels, produced);
  running_published_.store(running_, std::memory_order_relaxed);
}

size_t ScreenAudioPipe::PullWaiting() {
  // Starting needs the target margin to remain when the next capture packet
  // is due: one pull plus whatever a packet holds beyond one pull. Audio more
  // than one packet older than that is a backlog (a burst after a stall) and
  // is not sent late.
  size_t need = resampler_.InputFramesFor(kFrame);
  size_t packet =
      static_cast<size_t>(largest_write_frames_.load(std::memory_order_relaxed));
  size_t threshold = target_ + need + (packet > kFrame ? packet - kFrame : 0);
  size_t available = Available();
  if (available < threshold)
    return 0;
  size_t keep = threshold + packet;
  size_t excess = available > keep ? available - keep : 0;
  if (excess > 0) {
    DropOldest(excess);
    skipped_frames_.fetch_add(excess, std::memory_order_relaxed);
  }
  if (underrun_pending_) {
    underrun_pending_ = false;
    if (stalls_.load(std::memory_order_acquire) != stall_mark_) {
      gaps_.fetch_add(1, std::memory_order_relaxed);
    } else {
      target_ = std::min(target_ + kTargetStepFrames, kMaxTargetFrames);
      pulls_since_jitter_ = 0;
      target_frames_.store(static_cast<int32_t>(target_),
                           std::memory_order_relaxed);
    }
  }
  running_ = true;
  fade_in_ = true;
  smoothed_ = -1;
  window_min_ = SIZE_MAX;
  window_count_ = 0;
  trim_request_ = 0;
  return PullRunning();
}

void ScreenAudioPipe::EnterWaiting() {
  running_ = false;
  fade_in_ = false;
  underrun_pending_ = true;
  stall_mark_ = stalls_.load(std::memory_order_acquire);
  trim_request_ = 0;
  resampler_.Reset(true);
}

size_t ScreenAudioPipe::PullRunning() {
  if (trim_request_ > 0) {
    Trim(trim_request_);
    trim_request_ = 0;
  }
  size_t need = resampler_.InputFramesFor(kFrame);
  size_t available = Available();
  size_t consumed = 0;
  if (available < need) {
    size_t got = Gather(gather_.data(), std::min(available, kGatherFrames));
    size_t produced =
        resampler_.Process(gather_.data(), got, out_.data(), kFrame, &consumed);
    Fade(out_.data(), produced, false);
    underruns_.fetch_add(1, std::memory_order_relaxed);
    EnterWaiting();
    return produced;
  }
  size_t got = Gather(gather_.data(), std::min(need, kGatherFrames));
  size_t produced =
      resampler_.Process(gather_.data(), got, out_.data(), kFrame, &consumed);
  if (fade_in_) {
    Fade(out_.data(), produced, true);
    fade_in_ = false;
  }
  if (++pulls_since_jitter_ >= kTargetDecayPulls) {
    pulls_since_jitter_ = 0;
    if (target_ > kInitialTargetFrames) {
      target_ -= kTargetStepFrames;
      target_frames_.store(static_cast<int32_t>(target_),
                           std::memory_order_relaxed);
    }
  }
  UpdateLevel(available - need);
  return produced;
}

void ScreenAudioPipe::UpdateLevel(size_t margin) {
  window_min_ = std::min(window_min_, margin);
  if (++window_count_ < kWindowPulls)
    return;
  double lowest = static_cast<double>(window_min_);
  smoothed_ = smoothed_ < 0 ? lowest
                            : smoothed_ + (lowest - smoothed_) * kLevelSmoothing;
  window_min_ = SIZE_MAX;
  window_count_ = 0;
  level_frames_.store(static_cast<int32_t>(std::lround(smoothed_)),
                      std::memory_order_relaxed);

  double error = smoothed_ - static_cast<double>(target_);
  if (error > static_cast<double>(kTrimAboveFrames)) {
    // The smoothed level says the excess persists; the window's own lowest
    // level says how much of it there is now.
    trim_request_ = static_cast<size_t>(std::min(
        std::max(0.0, lowest - static_cast<double>(target_)),
        static_cast<double>(kMaxTrimFrames)));
    return;
  }
  double magnitude = std::fabs(error);
  double beyond = std::max(0.0, magnitude - kDeadbandFrames);
  double correction =
      std::min(kMaxCorrection, beyond / kFullScaleFrames * kMaxCorrection);
  double runaway = magnitude - kRunawayFrames;
  if (runaway > 0)
    correction = std::min(kRunawayCorrection,
                          kMaxCorrection + runaway / kRunawayFrames *
                                               (kRunawayCorrection - kMaxCorrection));
  double sign = error > 0 ? 1.0 : (error < 0 ? -1.0 : 0.0);
  resampler_.set_ratio(1 + sign * correction);
  drift_ppm_.store(
      static_cast<int32_t>(std::lround((resampler_.ratio() - 1) * 1e6)),
      std::memory_order_relaxed);
}

// Starts with the head of the queue faded into the audio at the best cut
// within the excess, so the frames in between are skipped click-free (the
// joined audio goes out first, through pending_).
void ScreenAudioPipe::Trim(size_t excess) {
  if (pending_frames_ > 0)
    return;
  size_t need = resampler_.InputFramesFor(kFrame);
  size_t available = ring_.Count();
  size_t reserve = need + 2 * kFadeFrames + kSearchFrames;
  if (available <= reserve)
    return;
  size_t drop = std::min(excess, available - reserve);
  if (drop < kSearchFrames + kFadeFrames)
    return;
  ring_.Peek(0, head_.data(), kFadeFrames);
  ring_.Peek(drop - kSearchFrames, candidates_.data(),
             kSearchFrames + kFadeFrames);
  size_t cut = ScreenAudioBestCut(head_.data(), kFadeFrames, candidates_.data(),
                                  kSearchFrames + kFadeFrames) +
               drop - kSearchFrames;
  ring_.Peek(cut, tail_.data(), kFadeFrames);
  ScreenAudioCrossfade(head_.data(), tail_.data(), pending_.data(), kFadeFrames);
  ring_.Discard(cut + kFadeFrames);
  pending_frames_ = kFadeFrames;
  pending_offset_ = 0;
  trimmed_frames_.fetch_add(cut, std::memory_order_relaxed);
  smoothed_ = static_cast<double>(target_) +
              std::max(0.0, static_cast<double>(excess) - static_cast<double>(cut));
  window_min_ = SIZE_MAX;
  window_count_ = 0;
}

void ScreenAudioPipe::Emit(int16_t* out, int channels, size_t produced) {
  if (produced < kFrame)
    std::fill(out_.begin() + static_cast<ptrdiff_t>(produced * 2), out_.end(),
              0.0f);
  uint64_t clipped = 0;
  uint64_t sumsq = 0;
  if (channels == 1) {
    for (size_t f = 0; f < kFrame; ++f) {
      int16_t s = ToInt16((out_[f * 2] + out_[f * 2 + 1]) * 0.5f, &clipped);
      out[f] = s;
      if (f < produced)
        sumsq += static_cast<uint64_t>(static_cast<int32_t>(s) * s);
    }
  } else {
    for (size_t i = 0; i < kFrame * 2; ++i) {
      int16_t s = ToInt16(out_[i], &clipped);
      out[i] = s;
      if (i < produced * 2)
        sumsq += static_cast<uint64_t>(static_cast<int32_t>(s) * s);
    }
  }
  sent_frames_.fetch_add(produced, std::memory_order_relaxed);
  silence_frames_.fetch_add(kFrame - produced, std::memory_order_relaxed);
  clipped_samples_.fetch_add(clipped, std::memory_order_relaxed);
  sent_sumsq_.fetch_add(sumsq, std::memory_order_relaxed);
  sent_samples_.fetch_add(produced * static_cast<size_t>(channels == 1 ? 1 : 2),
                          std::memory_order_relaxed);
}

ScreenAudioPipeStats ScreenAudioPipe::Read(bool reset_maxima) {
  ScreenAudioPipeStats stats;
  stats.running = running_published_.load(std::memory_order_relaxed);
  stats.captured_frames = captured_frames_.load(std::memory_order_relaxed);
  stats.silent_capture_frames =
      silent_capture_frames_.load(std::memory_order_relaxed);
  stats.discontinuities = discontinuities_.load(std::memory_order_relaxed);
  stats.overflow_frames = overflow_frames_.load(std::memory_order_relaxed);
  stats.write_max_gap_us = ReadMax(write_max_gap_us_, reset_maxima);
  stats.capture_stalls = stalls_.load(std::memory_order_relaxed);
  stats.pulls = pulls_.load(std::memory_order_relaxed);
  stats.pull_max_gap_us = ReadMax(pull_max_gap_us_, reset_maxima);
  stats.sent_frames = sent_frames_.load(std::memory_order_relaxed);
  stats.silence_frames = silence_frames_.load(std::memory_order_relaxed);
  stats.underruns = underruns_.load(std::memory_order_relaxed);
  stats.gaps = gaps_.load(std::memory_order_relaxed);
  stats.trimmed_frames = trimmed_frames_.load(std::memory_order_relaxed);
  stats.skipped_frames = skipped_frames_.load(std::memory_order_relaxed);
  stats.clipped_samples = clipped_samples_.load(std::memory_order_relaxed);
  stats.sent_sumsq = sent_sumsq_.load(std::memory_order_relaxed);
  stats.sent_samples = sent_samples_.load(std::memory_order_relaxed);
  stats.level_frames = level_frames_.load(std::memory_order_relaxed);
  stats.target_frames = target_frames_.load(std::memory_order_relaxed);
  stats.drift_ppm = drift_ppm_.load(std::memory_order_relaxed);
  return stats;
}

// RootScreenAudioCapture

// What a backend writes into. Calls that arrive after Close (a backend that
// stopped late) are dropped without touching the capture, so a leaked backend
// thread can never reach freed memory: a gate whose backend could not be
// stopped is leaked with it.
class RootScreenAudioCapture::Gate final : public ScreenAudioSink {
 public:
  Gate(RootScreenAudioCapture* owner, ScreenAudioPipe* pipe)
      : owner_(owner), pipe_(pipe) {}

  void Write(const float* interleaved, size_t frames) override {
    in_flight_.fetch_add(1);
    if (audio_.load())
      pipe_->Write(interleaved, frames);
    in_flight_.fetch_sub(1, std::memory_order_release);
  }

  void WriteSilence(size_t frames) override {
    in_flight_.fetch_add(1);
    if (audio_.load())
      pipe_->WriteSilence(frames);
    in_flight_.fetch_sub(1, std::memory_order_release);
  }

  void MarkDiscontinuity() override {
    in_flight_.fetch_add(1);
    if (audio_.load())
      pipe_->MarkDiscontinuity();
    in_flight_.fetch_sub(1, std::memory_order_release);
  }

  void ReportError(ScreenAudioError error, const char* detail) override {
    in_flight_.fetch_add(1);
    if (errors_.load())
      owner_->OnBackendError(error, detail);
    in_flight_.fetch_sub(1, std::memory_order_release);
  }

  void OpenErrors() { errors_.store(true); }
  void OpenAudio() { audio_.store(true); }

  void Close() {
    audio_.store(false);
    errors_.store(false);
    // Sink calls are wait-free, so whatever is inside one leaves in
    // microseconds.
    for (int spins = 0; in_flight_.load() != 0 && spins < 100000; ++spins)
      std::this_thread::yield();
  }

 private:
  RootScreenAudioCapture* owner_;
  ScreenAudioPipe* pipe_;
  std::atomic<bool> audio_{false};
  std::atomic<bool> errors_{false};
  std::atomic<int32_t> in_flight_{0};
};

std::shared_ptr<RootScreenAudioCapture> RootScreenAudioCapture::Create(
    std::shared_ptr<AudioTrackSource> source,
    ScreenAudioCaptureOptions options,
    std::string* error) {
  if (!source) {
    *error = "no audio source";
    return nullptr;
  }
  if (!source->queued()) {
    *error = "the audio source has no queue (queue_size_ms 0)";
    return nullptr;
  }
  if (source->sample_rate() != ScreenAudioPipe::kRate ||
      source->num_channels() < 1 || source->num_channels() > 2) {
    *error = "the audio source must be 48 kHz with 1 or 2 channels";
    return nullptr;
  }
  std::shared_ptr<RootScreenAudioCapture> capture(
      new RootScreenAudioCapture(source, std::move(options)));
  if (!source->set_puller(capture.get())) {
    *error = "the audio source already has a screen-audio capture";
    return nullptr;
  }
  capture->attached_.store(true);
  capture->thread_ = std::thread([self = capture] { self->Run(); });
  return capture;
}

RootScreenAudioCapture::RootScreenAudioCapture(
    std::shared_ptr<AudioTrackSource> source,
    ScreenAudioCaptureOptions options)
    : source_(std::move(source)),
      options_(std::move(options)),
      clock_(options_.clock ? options_.clock : ScreenAudioSteadyClock()),
      pipe_(clock_) {}

RootScreenAudioCapture::~RootScreenAudioCapture() {
  if (attached_.exchange(false))
    source_->set_puller(nullptr);
  if (thread_.joinable()) {
    if (thread_.get_id() == std::this_thread::get_id())
      thread_.detach();
    else
      thread_.join();
  }
}

void RootScreenAudioCapture::Start(const ScreenAudioTarget& target,
                                   uint64_t async_id) {
  {
    std::lock_guard<std::mutex> lock(mutex_);
    if (shutdown_requested_)
      return;
    commands_.push_back({Command::Kind::kStart, target, async_id});
  }
  wake_.notify_all();
}

void RootScreenAudioCapture::Stop(uint64_t async_id) {
  {
    std::lock_guard<std::mutex> lock(mutex_);
    if (shutdown_requested_)
      return;
    commands_.push_back({Command::Kind::kStop, ScreenAudioTarget(), async_id});
  }
  wake_.notify_all();
}

void RootScreenAudioCapture::Shutdown(bool wait) {
  if (attached_.exchange(false))
    source_->set_puller(nullptr);
  {
    std::lock_guard<std::mutex> lock(mutex_);
    if (!shutdown_requested_) {
      shutdown_requested_ = true;
      commands_.push_back({Command::Kind::kShutdown, ScreenAudioTarget(), 0});
    }
  }
  wake_.notify_all();
  if (wait) {
    std::unique_lock<std::mutex> lock(mutex_);
    finished_cv_.wait(lock, [this] { return finished_; });
  }
}

void RootScreenAudioCapture::Pull(int16_t* interleaved,
                                  int sample_rate,
                                  int channels,
                                  int frames) {
  if (sample_rate != ScreenAudioPipe::kRate ||
      frames != static_cast<int>(ScreenAudioPipe::kFrame) || channels < 1 ||
      channels > 2) {
    std::fill_n(interleaved, static_cast<size_t>(std::max(0, frames * channels)),
                int16_t{0});
    return;
  }
  pipe_.Pull(interleaved, channels);
}

void RootScreenAudioCapture::OnBackendError(ScreenAudioError error,
                                            const char* detail) {
  int32_t code = static_cast<int32_t>(error == ScreenAudioError::kNone
                                          ? ScreenAudioError::kInternal
                                          : error);
  int32_t expected = 0;
  if (!error_code_.compare_exchange_strong(expected, code))
    return;
  size_t i = 0;
  if (detail != nullptr) {
    for (; i + 1 < error_detail_.size() && detail[i] != 0; ++i)
      error_detail_[i] = detail[i];
  }
  error_detail_[i] = 0;
  error_ready_.store(true, std::memory_order_release);
  wake_.notify_all();
}

int64_t RootScreenAudioCapture::NowMs() const {
  return std::chrono::duration_cast<std::chrono::milliseconds>(
             std::chrono::steady_clock::now().time_since_epoch())
      .count();
}

void RootScreenAudioCapture::Run() {
  std::unique_lock<std::mutex> lock(mutex_);
  while (true) {
    if (error_ready_.load(std::memory_order_acquire)) {
      lock.unlock();
      HandleError();
      lock.lock();
      continue;
    }
    if (!commands_.empty()) {
      Command command = std::move(commands_.front());
      commands_.pop_front();
      lock.unlock();
      if (command.kind == Command::Kind::kShutdown) {
        StopBackend();
        restart_pending_ = false;
        lock.lock();
        break;
      }
      if (command.kind == Command::Kind::kStart)
        RunStart(command.target, command.async_id);
      else
        RunStop(command.async_id);
      lock.lock();
      continue;
    }
    if (restart_pending_) {
      int64_t now = NowMs();
      if (now >= restart_at_ms_) {
        lock.unlock();
        RunRestart();
        lock.lock();
        continue;
      }
      wake_.wait_for(lock, std::chrono::milliseconds(restart_at_ms_ - now));
      continue;
    }
    // A wake from ReportError can be missed (it does not take the lock); the
    // timeout bounds how late its handling starts.
    wake_.wait_for(lock, std::chrono::milliseconds(100));
  }
  finished_ = true;
  lock.unlock();
  finished_cv_.notify_all();
}

static std::unique_ptr<ScreenAudioBackend> MakeScreenAudioBackend(
    const ScreenAudioCaptureOptions& options) {
  if (options.make_backend)
    return options.make_backend();
  if (auto factory = ScreenAudioBackendFactoryOverride())
    return factory();
  return CreateRootScreenAudioBackend();
}

bool RootScreenAudioCapture::TryStart(const ScreenAudioTarget& target,
                                      ScreenAudioStartResult* result) {
  std::unique_ptr<ScreenAudioBackend> backend = MakeScreenAudioBackend(options_);
  if (!backend) {
    result->error = ScreenAudioError::kUnsupported;
    result->detail = "this build has no screen-audio backend for this OS";
    return false;
  }
  Gate* gate = new Gate(this, &pipe_);
  gate->OpenErrors();
  *result = backend->Start(target, gate);
  if (result->error == ScreenAudioError::kNone &&
      !ScreenAudioPipe::FormatSupported(result->format)) {
    result->error = ScreenAudioError::kInternal;
    result->detail = "the backend delivers an unsupported format (" +
                     std::to_string(result->format.sample_rate) + " Hz, " +
                     std::to_string(result->format.channels) + " channels)";
  }
  if (result->error == ScreenAudioError::kNone && target.restart &&
      started_scope_ != ScreenAudioScope::kSystem &&
      (result->scope != started_scope_ ||
       result->process_id != started_process_id_)) {
    result->error = ScreenAudioError::kTargetGone;
    result->detail =
        "the shared target is gone; not widening the capture to " +
        result->detail;
  }
  if (result->error != ScreenAudioError::kNone) {
    gate->Close();
    if (backend->Stop())
      delete gate;
    else
      (void)backend.release();
    ClearError();
    return false;
  }
  pipe_.Configure(result->format);
  gate->OpenAudio();
  running_.backend = std::move(backend);
  running_.gate = gate;
  {
    std::lock_guard<std::mutex> lock(state_mutex_);
    scope_ = result->scope;
    format_ = result->format;
    state_ = ScreenAudioState::kWaiting;
  }
  return true;
}

void RootScreenAudioCapture::StopBackend() {
  if (!running_.backend)
    return;
  running_.gate->Close();
  if (running_.backend->Stop()) {
    running_.backend.reset();
    delete running_.gate;
  } else {
    (void)running_.backend.release();
  }
  running_.gate = nullptr;
  ClearError();
}

// Only once the gate is closed: no report can arrive any more.
void RootScreenAudioCapture::ClearError() {
  error_code_.store(0);
  error_ready_.store(false);
}

void RootScreenAudioCapture::SetState(ScreenAudioState state) {
  std::lock_guard<std::mutex> lock(state_mutex_);
  state_ = state;
}

void RootScreenAudioCapture::Push(RootScreenAudioEventData event) {
  std::lock_guard<std::mutex> lock(events_mutex_);
  events_.push_back(std::move(event));
}

namespace {

RootScreenAudioEventData StartedEvent(uint64_t async_id,
                                      const ScreenAudioStartResult& result) {
  RootScreenAudioEventData event;
  event.kind = static_cast<uint32_t>(ScreenAudioEventKind::kStarted);
  event.async_id = async_id;
  event.scope = static_cast<int32_t>(result.scope);
  event.description =
      rust::String::lossy(result.detail.data(), result.detail.size());
  event.sample_rate = static_cast<uint32_t>(result.format.sample_rate);
  event.channels = static_cast<uint32_t>(result.format.channels);
  event.error = 0;
  event.retrying = false;
  return event;
}

RootScreenAudioEventData FailedEvent(uint64_t async_id,
                                     ScreenAudioError error,
                                     const std::string& message,
                                     bool retrying) {
  RootScreenAudioEventData event;
  event.kind = static_cast<uint32_t>(ScreenAudioEventKind::kFailed);
  event.async_id = async_id;
  event.scope = 0;
  event.sample_rate = 0;
  event.channels = 0;
  event.error = static_cast<int32_t>(error);
  event.message = rust::String::lossy(message.data(), message.size());
  event.retrying = retrying;
  return event;
}

bool Restartable(ScreenAudioError error) {
  switch (error) {
    case ScreenAudioError::kDeviceLost:
    case ScreenAudioError::kStreamFailed:
    case ScreenAudioError::kTimeout:
    case ScreenAudioError::kStartFailed:
    case ScreenAudioError::kInternal:
      return true;
    default:
      return false;
  }
}

}  // namespace

void RootScreenAudioCapture::RunStart(const ScreenAudioTarget& target,
                                      uint64_t async_id) {
  StopBackend();
  restart_pending_ = false;
  restart_times_ms_.clear();
  target_ = target;
  have_target_ = true;
  SetState(ScreenAudioState::kStarting);
  ScreenAudioStartResult result;
  if (TryStart(target, &result)) {
    started_scope_ = result.scope;
    started_process_id_ = result.process_id;
    Push(StartedEvent(async_id, result));
    return;
  }
  {
    std::lock_guard<std::mutex> lock(state_mutex_);
    state_ = ScreenAudioState::kFailed;
    last_error_ = result.error;
    last_error_message_ = result.detail;
  }
  Push(FailedEvent(async_id, result.error, result.detail, false));
}

void RootScreenAudioCapture::RunStop(uint64_t async_id) {
  StopBackend();
  restart_pending_ = false;
  have_target_ = false;
  SetState(ScreenAudioState::kStopped);
  RootScreenAudioEventData event;
  event.kind = static_cast<uint32_t>(ScreenAudioEventKind::kStopped);
  event.async_id = async_id;
  event.scope = 0;
  event.sample_rate = 0;
  event.channels = 0;
  event.error = 0;
  event.retrying = false;
  Push(std::move(event));
}

void RootScreenAudioCapture::HandleError() {
  ScreenAudioError error =
      static_cast<ScreenAudioError>(error_code_.load(std::memory_order_acquire));
  std::string detail(error_detail_.data());
  bool was_running = static_cast<bool>(running_.backend);
  StopBackend();
  ClearError();
  if (!was_running)
    return;
  ScheduleRestart(error, detail);
}

void RootScreenAudioCapture::ScheduleRestart(ScreenAudioError error,
                                             const std::string& message) {
  int64_t now = NowMs();
  while (!restart_times_ms_.empty() &&
         now - restart_times_ms_.front() >= options_.restart_window_ms)
    restart_times_ms_.pop_front();
  bool retrying = Restartable(error) && have_target_ &&
                  restart_times_ms_.size() < options_.restart_delays_ms.size();
  {
    std::lock_guard<std::mutex> lock(state_mutex_);
    last_error_ = error;
    last_error_message_ = message;
    state_ = retrying ? ScreenAudioState::kRestarting : ScreenAudioState::kFailed;
  }
  if (retrying) {
    int delay = options_.restart_delays_ms[restart_times_ms_.size()];
    restart_times_ms_.push_back(now);
    restart_pending_ = true;
    restart_at_ms_ = now + delay;
  } else {
    restart_pending_ = false;
  }
  Push(FailedEvent(0, error, message, retrying));
}

void RootScreenAudioCapture::RunRestart() {
  restart_pending_ = false;
  ScreenAudioTarget target = target_;
  target.restart = true;
  ScreenAudioStartResult result;
  if (TryStart(target, &result)) {
    {
      std::lock_guard<std::mutex> lock(state_mutex_);
      ++restarts_;
    }
    Push(StartedEvent(0, result));
    return;
  }
  ScheduleRestart(result.error, result.detail);
}

RootScreenAudioStatsData RootScreenAudioCapture::Stats() {
  ScreenAudioPipeStats pipe = pipe_.Read(true);
  RootScreenAudioStatsData stats;
  {
    std::lock_guard<std::mutex> lock(state_mutex_);
    ScreenAudioState state = state_;
    if (state == ScreenAudioState::kWaiting && pipe.running)
      state = ScreenAudioState::kRunning;
    stats.state = static_cast<int32_t>(state);
    stats.scope = static_cast<int32_t>(scope_);
    stats.capture_rate = static_cast<uint32_t>(format_.sample_rate);
    stats.capture_channels = static_cast<uint32_t>(format_.channels);
    stats.restarts = restarts_;
    stats.last_error = static_cast<int32_t>(last_error_);
    stats.last_error_message = rust::String::lossy(last_error_message_.data(),
                                                   last_error_message_.size());
  }
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

rust::Vec<RootScreenAudioEventData> RootScreenAudioCapture::TakeEvents() {
  std::vector<RootScreenAudioEventData> events;
  {
    std::lock_guard<std::mutex> lock(events_mutex_);
    events.swap(events_);
  }
  rust::Vec<RootScreenAudioEventData> out;
  for (auto& event : events)
    out.push_back(std::move(event));
  return out;
}

// Handle and bridge

RootScreenAudioCaptureHandle::RootScreenAudioCaptureHandle(
    std::shared_ptr<RootScreenAudioCapture> capture)
    : capture_(std::move(capture)) {}

RootScreenAudioCaptureHandle::~RootScreenAudioCaptureHandle() {
  capture_->Shutdown(false);
}

ScreenAudioTarget ScreenAudioTargetFromSpec(
    const RootScreenAudioTargetSpec& spec) {
  ScreenAudioTarget target;
  switch (spec.kind) {
    case static_cast<int32_t>(ScreenAudioTargetKind::kProcess):
      target.kind = spec.process_id != 0 ? ScreenAudioTargetKind::kProcess
                                         : ScreenAudioTargetKind::kSystem;
      break;
    case static_cast<int32_t>(ScreenAudioTargetKind::kWindow):
      target.kind = spec.window_id != 0 || !spec.window_title.empty()
                        ? ScreenAudioTargetKind::kWindow
                        : ScreenAudioTargetKind::kSystem;
      break;
    default:
      target.kind = ScreenAudioTargetKind::kSystem;
      break;
  }
  target.process_id = spec.process_id;
  target.window_id = spec.window_id;
  target.window_title = std::string(spec.window_title);
  return target;
}

void RootScreenAudioCaptureHandle::start(const RootScreenAudioTargetSpec& target,
                                         uint64_t async_id) const {
  capture_->Start(ScreenAudioTargetFromSpec(target), async_id);
}

void RootScreenAudioCaptureHandle::stop(uint64_t async_id) const {
  capture_->Stop(async_id);
}

RootScreenAudioStatsData RootScreenAudioCaptureHandle::stats() const {
  return capture_->Stats();
}

rust::Vec<RootScreenAudioEventData> RootScreenAudioCaptureHandle::take_events()
    const {
  return capture_->TakeEvents();
}

bool root_screen_audio_backend_built() {
  return RootScreenAudioBackendBuilt();
}

std::unique_ptr<RootScreenAudioCaptureHandle> new_root_screen_audio_capture(
    std::shared_ptr<AudioTrackSource> source,
    rust::String& error) {
  std::string reason;
  auto capture = RootScreenAudioCapture::Create(
      std::move(source), ScreenAudioCaptureOptions(), &reason);
  if (!capture) {
    error = rust::String::lossy(reason.data(), reason.size());
    return nullptr;
  }
  return std::make_unique<RootScreenAudioCaptureHandle>(std::move(capture));
}

void root_screen_audio_use_fake_backend(bool enabled) {
  g_fake_backend.store(enabled);
}

std::unique_ptr<ScreenAudioBackend> NewToneScreenAudioBackend();

std::function<std::unique_ptr<ScreenAudioBackend>()>
ScreenAudioBackendFactoryOverride() {
  if (!g_fake_backend.load())
    return nullptr;
  return [] { return NewToneScreenAudioBackend(); };
}

}  // namespace livekit_ffi
