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

// Include from .cpp files only: libwebrtc's rnn_vad headers raise C4244 and
// C4267 at /W4, so root_audio_processing.h holds the detector by a forward
// declaration.

#include <array>
#include <cstddef>
#include <cstdint>

#if defined(_MSC_VER)
#pragma warning(push)
#pragma warning(disable : 4244 4267)
#endif
#include "common_audio/resampler/include/push_resampler.h"
#include "modules/audio_processing/agc2/cpu_features.h"
#include "modules/audio_processing/agc2/rnn_vad/common.h"
#include "modules/audio_processing/agc2/rnn_vad/features_extraction.h"
#include "modules/audio_processing/agc2/rnn_vad/rnn.h"
#if defined(_MSC_VER)
#pragma warning(pop)
#endif

namespace livekit_ffi {

// libwebrtc's AGC2 speech detector (modules/audio_processing/agc2/rnn_vad, as
// VoiceActivityDetectorWrapper runs it, without the 1.5 s periodic reset: the
// RNN starts over only after 1 s in a row at p >= 0.3, which white hiss can
// otherwise hold) on the capture thread. Built off the audio thread with a
// resampler for each rate the APM's capture post-processor runs at (16, 32 and
// 48 kHz) to the detector's 24 kHz; Analyze and Reset never allocate, lock or
// log. The AVX2 code path needs FMA3 as well.
class RootSpeechDetector {
 public:
  RootSpeechDetector();
  RootSpeechDetector(const RootSpeechDetector&) = delete;
  RootSpeechDetector& operator=(const RootSpeechDetector&) = delete;

  // frame: channel 0, FloatS16, finite, rate / 100 samples. Returns the speech
  // probability 0..1, or -1 for a rate or length it cannot analyze. A rate
  // change resets the detector first.
  float Analyze(const float* frame, size_t samples, int rate);
  // Starts over: resamplers, features and RNN.
  void Reset();

 private:
  webrtc::PushResampler<float>* ResamplerFor(int rate, size_t samples);
  static uint32_t ResamplerBit(int rate);

  webrtc::PushResampler<float> from16_;
  webrtc::PushResampler<float> from32_;
  webrtc::PushResampler<float> from48_;
  std::array<float, webrtc::rnn_vad::kFrameSize10ms24kHz> frame24_{};
  std::array<float, webrtc::rnn_vad::kFeatureVectorSize> features_{};
  webrtc::rnn_vad::FeaturesExtractor extractor_;
  webrtc::rnn_vad::RnnVad rnn_;
  int rate_ = 0;
  // Frames in a row at p >= 0.3.
  int speech_like_ = 0;
  // ResamplerBit of each resampler used since the last Reset.
  uint32_t used_ = 0;
};

}  // namespace livekit_ffi
