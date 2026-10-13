/*
 * Copyright 2025 LiveKit, Inc.
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

#include <memory>
#include <vector>

#include "api/audio/audio_frame.h"
#include "api/audio_options.h"
#include "api/task_queue/task_queue_factory.h"
#include "common_audio/resampler/include/push_resampler.h"
#include "livekit/helper.h"
#include "livekit/media_stream_track.h"
#include "livekit/webrtc.h"
#include "pc/local_audio_source.h"
#include "rtc_base/synchronization/mutex.h"
#include "api/task_queue/task_queue_base.h"
#include "rtc_base/task_utils/repeating_task.h"
#include "rtc_base/thread_annotations.h"
#include "rust/cxx.h"

namespace livekit_ffi {
class AudioTrack;
class NativeAudioSink;
class AudioTrackSource;
class SourceContext;

using CompleteCallback = void (*)(const livekit_ffi::SourceContext*);
}  // namespace livekit_ffi
#include "webrtc-sys/src/audio_track.rs.h"

namespace livekit_ffi {

class AudioTrack : public MediaStreamTrack {
 private:
  friend RtcRuntime;
  AudioTrack(std::shared_ptr<RtcRuntime> rtc_runtime,
             webrtc::scoped_refptr<webrtc::AudioTrackInterface> track);

 public:
  ~AudioTrack();

  void add_sink(const std::shared_ptr<NativeAudioSink>& sink) const;
  void remove_sink(const std::shared_ptr<NativeAudioSink>& sink) const;

 private:
  webrtc::AudioTrackInterface* track() const {
    return static_cast<webrtc::AudioTrackInterface*>(track_.get());
  }

  mutable webrtc::Mutex mutex_;

  // Same for VideoTrack:
  // Keep a strong reference to the added sinks, so we don't need to
  // manage the lifetime safety on the Rust side
  mutable std::vector<std::shared_ptr<NativeAudioSink>> sinks_;
};

class NativeAudioSink : public webrtc::AudioTrackSinkInterface {
 public:
  explicit NativeAudioSink(rust::Box<AudioSinkWrapper> observer,
                           int sample_rate,
                           int num_channels);
  void OnData(const void* audio_data,
              int bits_per_sample,
              int sample_rate,
              size_t number_of_channels,
              size_t number_of_frames) override;

 private:
  rust::Box<AudioSinkWrapper> observer_;

  int sample_rate_;
  int num_channels_;

  webrtc::AudioFrame frame_;
  webrtc::PushResampler<int16_t> resampler_;
};

std::shared_ptr<NativeAudioSink> new_native_audio_sink(
    rust::Box<AudioSinkWrapper> observer,
    int sample_rate,
    int num_channels);

// A native producer that owns a queued source's 10 ms frames (screen audio,
// root_screen_audio.h). Pull runs on the source's 10 ms task with the source's
// mutex held and fills `frames` frames of `channels` interleaved int16 at
// `sample_rate`; it must not lock, allocate or log.
class AudioSourcePuller {
 public:
  virtual void Pull(int16_t* interleaved,
                    int sample_rate,
                    int channels,
                    int frames) = 0;

 protected:
  ~AudioSourcePuller() = default;
};

class AudioTrackSource {
  class InternalSource : public webrtc::LocalAudioSource {
   public:
    InternalSource(const AudioSourceOptions& options,
                   int sample_rate,
                   int num_channels,
                   int buffer_size_ms,
                   webrtc::TaskQueueFactory* task_queue_factory);

    ~InternalSource() override;

    SourceState state() const override;
    bool remote() const override;

    // What libwebrtc applies when this source is sent.
    const webrtc::AudioOptions options() const override;

    void AddSink(webrtc::AudioTrackSinkInterface* sink) override;
    void RemoveSink(webrtc::AudioTrackSinkInterface* sink) override;

    AudioSourceOptions requested_options() const;
    void set_requested_options(const AudioSourceOptions& options);

    bool capture_frame(rust::Slice<const int16_t> audio_data,
                       uint32_t sample_rate,
                       uint32_t number_of_channels,
                       size_t number_of_frames,
                       const SourceContext* ctx,
                       void (*on_complete)(const SourceContext*));

    void clear_buffer();

    // While a puller is attached the 10 ms task sends what it pulls instead
    // of the queue, and capture_frame refuses frames. Needs the queue (a
    // queue_size_ms above 0). Attaching fails when another puller is
    // attached; after detaching (nullptr) returns, Pull is not running and
    // never runs again.
    bool set_puller(AudioSourcePuller* puller);
    bool queued() const { return queue_size_samples_ != 0; }
    int sample_rate() const { return sample_rate_; }
    int num_channels() const { return num_channels_; }

    // Indicate this is an external audio source (when external_audio_source.patch is applied).
    // This prevents AudioState from sending device audio to streams using this source.
    // Note: Omit 'override' to allow builds without the patch applied.
    // When the patch is applied, this will correctly override the base class virtual method.
    bool is_external_source() const { return true; }

   private:
    mutable webrtc::Mutex mutex_;
    std::unique_ptr<webrtc::TaskQueueBase, webrtc::TaskQueueDeleter> audio_queue_;
    webrtc::RepeatingTaskHandle audio_task_;

    std::vector<webrtc::AudioTrackSinkInterface*> sinks_ RTC_GUARDED_BY(mutex_);
    std::vector<int16_t> buffer_ RTC_GUARDED_BY(mutex_);

    const SourceContext* capture_userdata_ RTC_GUARDED_BY(mutex_);
    void (*on_complete_)(const SourceContext*) RTC_GUARDED_BY(mutex_);

    std::vector<int16_t> silence_buffer_;
    std::vector<int16_t> pull_buffer_;
    AudioSourcePuller* puller_ RTC_GUARDED_BY(mutex_) = nullptr;

    int sample_rate_ = 0;
    int num_channels_ = 0;
    int queue_size_samples_ = 0;
    int notify_threshold_samples_ = 0;

    AudioSourceOptions requested_options_ RTC_GUARDED_BY(mutex_);
  };

 public:
  AudioTrackSource(AudioSourceOptions options,
                   int sample_rate,
                   int num_channels,
                   int queue_size_ms,
                   webrtc::TaskQueueFactory* task_queue_factory);

  AudioSourceOptions audio_options() const;

  void set_audio_options(const AudioSourceOptions& options) const;

  bool capture_frame(rust::Slice<const int16_t> audio_data,
                     uint32_t sample_rate,
                     uint32_t number_of_channels,
                     size_t number_of_frames,
                     const SourceContext* ctx,
                     CompleteCallback on_complete) const;

  void clear_buffer() const;

  bool set_puller(AudioSourcePuller* puller) const;
  bool queued() const;
  int sample_rate() const;
  int num_channels() const;

  webrtc::scoped_refptr<InternalSource> get() const;

 private:
  webrtc::scoped_refptr<InternalSource> source_;
};

std::shared_ptr<AudioTrackSource> new_audio_track_source(
    AudioSourceOptions options,
    int sample_rate,
    int num_channels,
    int queue_size_ms);

static std::shared_ptr<MediaStreamTrack> audio_to_media(
    std::shared_ptr<AudioTrack> track) {
  return track;
}

static std::shared_ptr<AudioTrack> media_to_audio(
    std::shared_ptr<MediaStreamTrack> track) {
  return std::static_pointer_cast<AudioTrack>(track);
}

static std::shared_ptr<AudioTrack> _shared_audio_track() {
  return nullptr;  // Ignore
}

static std::shared_ptr<AudioTrackSource> _shared_audio_track_source() {
  return nullptr;  // Ignore
}

}  // namespace livekit_ffi
