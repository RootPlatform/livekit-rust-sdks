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

#include "livekit/root_speech_detector.h"

#include <algorithm>
#include <cmath>
#include <cstdint>
#include <memory>
#include <span>
#include <vector>

#include "api/audio/audio_view.h"
#include "livekit/root_audio_processing.h"
#if defined(_MSC_VER)
#pragma warning(push)
#pragma warning(disable : 4244 4267)
#endif
#include "modules/audio_processing/agc2/vad_wrapper.h"
#if defined(_MSC_VER)
#pragma warning(pop)
#endif

namespace livekit_ffi {

namespace {

constexpr size_t kFrame24 = webrtc::rnn_vad::kFrameSize10ms24kHz;

}  // namespace

RootSpeechDetector::RootSpeechDetector()
    : from16_(160, kFrame24, 1),
      from32_(320, kFrame24, 1),
      from48_(480, kFrame24, 1),
      extractor_(webrtc::GetAvailableCpuFeatures()),
      rnn_(webrtc::GetAvailableCpuFeatures()) {}

webrtc::PushResampler<float>* RootSpeechDetector::ResamplerFor(
    int rate,
    size_t samples) {
  switch (rate) {
    case 16000:
      return samples == 160 ? &from16_ : nullptr;
    case 32000:
      return samples == 320 ? &from32_ : nullptr;
    case 48000:
      return samples == 480 ? &from48_ : nullptr;
    default:
      return nullptr;
  }
}

float RootSpeechDetector::Analyze(const float* frame,
                                  size_t samples,
                                  int rate) {
  webrtc::PushResampler<float>* resampler = ResamplerFor(rate, samples);
  if (frame == nullptr || resampler == nullptr) {
    return -1.0f;
  }
  if (rate != rate_) {
    rate_ = rate;
    Reset();
  }
  resampler->Resample(webrtc::MonoView<const float>(frame, samples),
                      webrtc::MonoView<float>(frame24_.data(), kFrame24));
  const bool silence = extractor_.CheckSilenceComputeFeatures(
      std::span<const float, kFrame24>(frame24_), features_);
  return rnn_.ComputeVadProbability(features_, silence);
}

void RootSpeechDetector::Reset() {
  extractor_.Reset();
  rnn_.Reset();
}

// =============================================================================
// Test hooks
// =============================================================================

namespace {

constexpr double kPi = 3.14159265358979;

// The `signal` of root_speech_test_signal and root_speech_detector_probe.
enum class RootSpeechTestSignal : uint32_t {
  // A formant-filtered pulse train at 120 Hz +-20 Hz with 4 Hz syllables,
  // about -29 dBFS RMS.
  kVowel = 0,
  // Steady, about -35 dBFS RMS.
  kWhiteNoise = 1,
  // Loud 40-sample bursts, 7 a second.
  kClicks = 2,
  // 440 Hz.
  kTone = 3,
  kSilence = 4,
  kBreath = 5,
  kKeys = 6,
};

class Resonator {
 public:
  Resonator(double hz, double bandwidth, double rate) {
    const double r = std::exp(-kPi * bandwidth / rate);
    a1_ = 2 * r * std::cos(2 * kPi * hz / rate);
    a2_ = -r * r;
    gain_ = 1 - r;
  }

  double Step(double x) {
    const double y = gain_ * x + a1_ * y1_ + a2_ * y2_;
    y2_ = y1_;
    y1_ = y;
    return y;
  }

 private:
  double a1_ = 0;
  double a2_ = 0;
  double gain_ = 0;
  double y1_ = 0;
  double y2_ = 0;
};

class Noise {
 public:
  // Uniform in [-1, 1).
  double Next() {
    seed_ ^= seed_ << 13;
    seed_ ^= seed_ >> 7;
    seed_ ^= seed_ << 17;
    return static_cast<double>(seed_ >> 11) /
               static_cast<double>(uint64_t{1} << 53) * 2 -
           1;
  }

 private:
  uint64_t seed_ = 0x2545F4914F6CDD1Dull;
};

// 0.5 - 0.5 cos over `on` seconds of every `period`, 0 for the rest.
double Swell(double t, double on, double period) {
  const double phase = std::fmod(t, period);
  return phase < on ? 0.5 - 0.5 * std::cos(2 * kPi * phase / on) : 0.0;
}

std::vector<float> TestSignal(RootSpeechTestSignal signal,
                              int rate,
                              size_t samples) {
  std::vector<float> out(samples, 0.0f);
  Noise noise;
  const double hz = static_cast<double>(rate);
  switch (signal) {
    case RootSpeechTestSignal::kVowel: {
      Resonator f1(700, 80, hz);
      Resonator f2(1220, 90, hz);
      Resonator f3(2600, 120, hz);
      double phase = 0;
      for (size_t n = 0; n < samples; ++n) {
        const double t = static_cast<double>(n) / hz;
        phase += (120 + 20 * std::sin(2 * kPi * 0.7 * t)) / hz;
        double pulse = 0;
        if (phase >= 1) {
          phase -= 1;
          pulse = 1;
        }
        const double source = pulse + 0.02 * noise.Next();
        const double v = f1.Step(source) + f2.Step(source) * 0.5 +
                         f3.Step(source) * 0.25;
        const double syllable = 0.5 + 0.5 * std::sin(2 * kPi * 4 * t);
        out[n] = static_cast<float>(v * syllable * 90000);
      }
      break;
    }
    case RootSpeechTestSignal::kWhiteNoise:
      for (float& v : out) {
        v = static_cast<float>(noise.Next() * 1000);
      }
      break;
    case RootSpeechTestSignal::kClicks: {
      const size_t step = static_cast<size_t>(rate / 7);
      for (size_t i = 0; i < samples; i += step) {
        for (size_t k = 0; k < 40 && i + k < samples; ++k) {
          out[i + k] = static_cast<float>(noise.Next() * 20000);
        }
      }
      break;
    }
    case RootSpeechTestSignal::kTone:
      for (size_t n = 0; n < samples; ++n) {
        out[n] = static_cast<float>(
            8000 * std::sin(2 * kPi * 440 * static_cast<double>(n) / hz));
      }
      break;
    case RootSpeechTestSignal::kSilence:
      break;
    case RootSpeechTestSignal::kBreath: {
      // High-frequency turbulence (differentiated white noise) swelling over
      // 1 s of every 2 s, about -50 dBFS RMS.
      double previous = 0;
      for (size_t n = 0; n < samples; ++n) {
        const double x = noise.Next();
        const double t = static_cast<double>(n) / hz;
        out[n] = static_cast<float>((x - previous) * Swell(t, 1.0, 2.0) * 300);
        previous = x;
      }
      break;
    }
    case RootSpeechTestSignal::kKeys: {
      // 2 ms decaying bursts, 12 a second.
      const size_t step = static_cast<size_t>(rate / 12);
      const size_t width = static_cast<size_t>(rate / 500);
      for (size_t i = 0; i < samples; i += step) {
        for (size_t k = 0; k < width && i + k < samples; ++k) {
          const double decay =
              std::exp(-static_cast<double>(k) / (static_cast<double>(width) / 4));
          out[i + k] = static_cast<float>(noise.Next() * 12000 * decay);
        }
      }
      break;
    }
  }
  return out;
}

}  // namespace

rust::Vec<float> root_speech_test_signal(uint32_t signal,
                                         int32_t rate,
                                         uint32_t frames) {
  rust::Vec<float> out;
  if (rate <= 0 || signal > static_cast<uint32_t>(RootSpeechTestSignal::kKeys)) {
    return out;
  }
  const size_t samples = static_cast<size_t>(rate / 100) * frames;
  const std::vector<float> generated =
      TestSignal(static_cast<RootSpeechTestSignal>(signal), rate, samples);
  out.reserve(generated.size());
  for (float v : generated) {
    out.push_back(v);
  }
  return out;
}

rust::Vec<float> root_speech_detector_probabilities(
    int32_t rate,
    rust::Slice<const float> input) {
  rust::Vec<float> out;
  if (rate <= 0) {
    return out;
  }
  const size_t frame = static_cast<size_t>(rate / 100);
  RootSpeechDetector detector;
  for (size_t i = 0; frame > 0 && i + frame <= input.size(); i += frame) {
    out.push_back(detector.Analyze(input.data() + i, frame, rate));
  }
  return out;
}

RootSpeechDetectorProbe root_speech_detector_probe(uint32_t signal,
                                                   int32_t rate,
                                                   uint32_t frames) {
  RootSpeechDetectorProbe probe{};
  if (rate <= 0 || signal > static_cast<uint32_t>(RootSpeechTestSignal::kKeys)) {
    return probe;
  }
  const size_t frame = static_cast<size_t>(rate / 100);
  const std::vector<float> x = TestSignal(
      static_cast<RootSpeechTestSignal>(signal), rate, frame * frames);
  RootSpeechDetector detector;
  // libwebrtc's own wrapper, without the periodic reset, where the APM runs.
  std::unique_ptr<webrtc::VoiceActivityDetectorWrapper> reference;
  if (rate == 16000 || rate == 32000 || rate == 48000) {
    reference = std::make_unique<webrtc::VoiceActivityDetectorWrapper>(
        1'000'000'000, webrtc::GetAvailableCpuFeatures(), rate);
  }
  // The detector's first 200 ms are its warm-up.
  constexpr uint32_t kWarmup = 20;
  double sum = 0;
  uint32_t run = 0;
  for (uint32_t i = 0; i < frames; ++i) {
    const float* f = x.data() + static_cast<size_t>(i) * frame;
    const float p = detector.Analyze(f, frame, rate);
    ++probe.frames;
    if (p < 0) {
      ++probe.unsupported;
      continue;
    }
    if (reference) {
      const float expected =
          reference->Analyze(webrtc::DeinterleavedView<const float>(f, frame, 1));
      probe.max_diff = std::max(probe.max_diff, std::fabs(expected - p));
    }
    if (i < kWarmup) {
      continue;
    }
    ++probe.judged;
    sum += p;
    probe.max = std::max(probe.max, p);
    run = p >= 0.5f ? run + 1 : 0;
    probe.above += p >= 0.5f ? 1 : 0;
    probe.longest_run = std::max(probe.longest_run, run);
  }
  probe.mean =
      probe.judged > 0 ? static_cast<float>(sum / probe.judged) : 0.0f;
  return probe;
}

}  // namespace livekit_ffi
