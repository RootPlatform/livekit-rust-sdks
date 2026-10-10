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

#include "livekit/adm_proxy.h"

#include <chrono>
#include <cstring>
#include <utility>

#include "api/audio/audio_device.h"
#include "api/audio/create_audio_device_module.h"
#include "api/make_ref_counted.h"
#include "api/units/time_delta.h"
#include "livekit/audio_thread.h"
#include "livekit/root_audio_mixer.h"
#include "livekit/root_audio_processing.h"
#include "rtc_base/checks.h"
#include "rtc_base/event.h"
#include "rtc_base/logging.h"
#include "rtc_base/thread.h"

#if defined(WEBRTC_WIN)
#include <objbase.h>
#endif

#if defined(__ANDROID__)
#include <jni.h>
#include "sdk/android/native_api/audio_device_module/audio_device_android.h"
#include "sdk/android/native_api/base/init.h"
#endif

namespace livekit_ffi {
namespace {

#if !LK_ADM_MOBILE
constexpr webrtc::TimeDelta kPlayoutDelayRefresh =
    webrtc::TimeDelta::Millis(100);
constexpr webrtc::TimeDelta kStopFailureRetry = webrtc::TimeDelta::Seconds(1);
#endif

int64_t NowNs() {
  return std::chrono::duration_cast<std::chrono::nanoseconds>(
             std::chrono::steady_clock::now().time_since_epoch())
      .count();
}

void UpdateMax(std::atomic<uint64_t>& max, uint64_t value) {
  uint64_t current = max.load(std::memory_order_relaxed);
  while (value > current &&
         !max.compare_exchange_weak(current, value, std::memory_order_relaxed)) {
  }
}

void AddTo(std::atomic<double>& total, double value) {
  total.store(total.load(std::memory_order_relaxed) + value,
              std::memory_order_relaxed);
}

class AdmLock {
 public:
  explicit AdmLock(webrtc::Mutex* mutex) : mutex_(mutex) {
    if (IsAudioThread()) {
      CountAudioThreadLockViolation();
    }
    mutex_->Lock();
  }
  ~AdmLock() { mutex_->Unlock(); }

  AdmLock(const AdmLock&) = delete;
  AdmLock& operator=(const AdmLock&) = delete;

 private:
  webrtc::Mutex* const mutex_;
};

void Silence(void* audio_samples,
             size_t samples_per_channel,
             size_t bytes_per_frame,
             size_t channels,
             size_t& samples_out,
             int64_t* elapsed_time_ms,
             int64_t* ntp_time_ms) {
  std::memset(audio_samples, 0, samples_per_channel * bytes_per_frame);
  samples_out = samples_per_channel * channels;
  *elapsed_time_ms = -1;
  *ntp_time_ms = -1;
}

webrtc::scoped_refptr<webrtc::AudioDeviceModule> CreatePlatformAdm(
    const webrtc::Environment& env) {
#if defined(__ANDROID__)
  // Created on the first AcquirePlatformAdm instead: CreateAudioDeviceModule
  // needs JNI, which may not be initialized when the factory is built.
  return nullptr;
#else
  // Created eagerly, not on acquire: the iOS audio session needs early setup to
  // avoid KVO races.
  webrtc::scoped_refptr<webrtc::AudioDeviceModule> adm =
      webrtc::CreateAudioDeviceModule(
          env, webrtc::AudioDeviceModule::kPlatformDefaultAudio);
  if (!adm) {
    RTC_LOG(LS_ERROR) << "AdmProxy: CreateAudioDeviceModule returned nullptr";
  }
  return adm;
#endif
}

}  // namespace

int ResolveAdmDeviceIndex(
    const AdmDeviceSelection& selection,
    int device_count,
    const std::function<bool(int index, std::string* name, std::string* guid)>&
        device_at) {
  switch (selection.kind) {
    case AdmSelectionKind::kDefault:
      return kAdmResolveDefault;
    case AdmSelectionKind::kIndex:
      return selection.index < device_count ? selection.index
                                            : kAdmResolveNotFound;
    case AdmSelectionKind::kGuid:
    case AdmSelectionKind::kName:
      break;
  }
  if (selection.id.empty()) {
    return kAdmResolveNotFound;
  }
  for (int index = 0; index < device_count; ++index) {
    std::string name;
    std::string guid;
    if (!device_at(index, &name, &guid)) {
      continue;
    }
    const std::string& key =
        selection.kind == AdmSelectionKind::kGuid ? guid : name;
    if (key == selection.id) {
      return index;
    }
  }
  return kAdmResolveNotFound;
}

// =============================================================================
// AdmTransportTap
// =============================================================================

void AdmTransportTap::set_transport(webrtc::AudioTransport* transport) {
  transport_.store(transport, std::memory_order_release);
}

void AdmTransportTap::Count(Direction& direction,
                            size_t frames,
                            uint32_t sample_rate,
                            uint32_t delay_ms) {
  const int64_t now = NowNs();
  direction.last_ns.store(now, std::memory_order_relaxed);
  const int64_t previous =
      direction.previous_ns.exchange(now, std::memory_order_relaxed);
  if (previous > 0 && now > previous) {
    UpdateMax(direction.max_gap_ns, static_cast<uint64_t>(now - previous));
  }
  direction.callbacks.fetch_add(1, std::memory_order_relaxed);
  direction.frames.fetch_add(frames, std::memory_order_relaxed);
  if (sample_rate > 0) {
    const double seconds = static_cast<double>(frames) / sample_rate;
    AddTo(direction.duration_s, seconds);
    AddTo(direction.delay_s, static_cast<double>(frames) * delay_ms / 1000.0);
  }
}

int32_t AdmTransportTap::RecordedDataIsAvailable(
    const void* audio_samples,
    size_t samples_per_channel,
    size_t bytes_per_frame,
    size_t channels,
    uint32_t sample_rate,
    uint32_t total_delay_ms,
    int32_t clock_drift,
    uint32_t current_mic_level,
    bool key_pressed,
    uint32_t& new_mic_level) {
  return RecordedDataIsAvailable(audio_samples, samples_per_channel,
                                 bytes_per_frame, channels, sample_rate,
                                 total_delay_ms, clock_drift, current_mic_level,
                                 key_pressed, new_mic_level, std::nullopt);
}

int32_t AdmTransportTap::RecordedDataIsAvailable(
    const void* audio_samples,
    size_t samples_per_channel,
    size_t bytes_per_frame,
    size_t channels,
    uint32_t sample_rate,
    uint32_t total_delay_ms,
    int32_t clock_drift,
    uint32_t current_mic_level,
    bool key_pressed,
    uint32_t& new_mic_level,
    std::optional<int64_t> estimated_capture_time_ns) {
  MarkAudioThread();
  if (capture_busy_.exchange(true, std::memory_order_acquire)) {
    overlapping_.fetch_add(1, std::memory_order_relaxed);
    return 0;
  }
  const uint32_t playout_delay = playout_delay_ms_.load(std::memory_order_relaxed);
  Count(capture_, samples_per_channel, sample_rate,
        total_delay_ms > playout_delay ? total_delay_ms - playout_delay : 0);
  capture_rate_.store(sample_rate, std::memory_order_relaxed);
  capture_channels_.store(static_cast<uint32_t>(channels),
                          std::memory_order_relaxed);
  capture_total_delay_ms_.store(total_delay_ms, std::memory_order_relaxed);

  if (audio_samples != nullptr) {
    const int16_t* samples = static_cast<const int16_t*>(audio_samples);
    const size_t count = samples_per_channel * channels;
    double sumsq = 0;
    for (size_t i = 0; i < count; ++i) {
      sumsq += static_cast<double>(samples[i]) * samples[i];
    }
    AddTo(capture_sumsq_, sumsq);
    capture_samples_.fetch_add(count, std::memory_order_relaxed);
  }

  webrtc::AudioTransport* transport =
      transport_.load(std::memory_order_acquire);
  int32_t result = 0;
  if (transport != nullptr) {
    result = transport->RecordedDataIsAvailable(
        audio_samples, samples_per_channel, bytes_per_frame, channels,
        sample_rate, total_delay_ms, clock_drift, current_mic_level,
        key_pressed, new_mic_level, estimated_capture_time_ns);
  }
  capture_busy_.store(false, std::memory_order_release);
  return result;
}

int32_t AdmTransportTap::NeedMorePlayData(size_t samples_per_channel,
                                          size_t bytes_per_frame,
                                          size_t channels,
                                          uint32_t sample_rate,
                                          void* audio_samples,
                                          size_t& samples_out,
                                          int64_t* elapsed_time_ms,
                                          int64_t* ntp_time_ms) {
  MarkAudioThread();
  return Render(true, samples_per_channel, bytes_per_frame, channels,
                sample_rate, audio_samples, samples_out, elapsed_time_ms,
                ntp_time_ms);
}

int32_t AdmTransportTap::RenderUncounted(size_t samples_per_channel,
                                         size_t bytes_per_frame,
                                         size_t channels,
                                         uint32_t sample_rate,
                                         void* audio_samples,
                                         size_t& samples_out,
                                         int64_t* elapsed_time_ms,
                                         int64_t* ntp_time_ms) {
  return Render(false, samples_per_channel, bytes_per_frame, channels,
                sample_rate, audio_samples, samples_out, elapsed_time_ms,
                ntp_time_ms);
}

int32_t AdmTransportTap::Render(bool count,
                                size_t samples_per_channel,
                                size_t bytes_per_frame,
                                size_t channels,
                                uint32_t sample_rate,
                                void* audio_samples,
                                size_t& samples_out,
                                int64_t* elapsed_time_ms,
                                int64_t* ntp_time_ms) {
  if (render_busy_.exchange(true, std::memory_order_acquire)) {
    overlapping_.fetch_add(1, std::memory_order_relaxed);
    Silence(audio_samples, samples_per_channel, bytes_per_frame, channels,
            samples_out, elapsed_time_ms, ntp_time_ms);
    return 0;
  }
  if (count) {
    Count(render_, samples_per_channel, sample_rate,
          playout_delay_ms_.load(std::memory_order_relaxed));
  }

  webrtc::AudioTransport* transport =
      transport_.load(std::memory_order_acquire);
  int32_t result = 0;
  if (transport == nullptr) {
    Silence(audio_samples, samples_per_channel, bytes_per_frame, channels,
            samples_out, elapsed_time_ms, ntp_time_ms);
  } else {
    result = transport->NeedMorePlayData(
        samples_per_channel, bytes_per_frame, channels, sample_rate,
        audio_samples, samples_out, elapsed_time_ms, ntp_time_ms);
  }
  render_busy_.store(false, std::memory_order_release);
  return result;
}

void AdmTransportTap::PullRenderData(int bits_per_sample,
                                     int sample_rate,
                                     size_t number_of_channels,
                                     size_t number_of_frames,
                                     void* audio_data,
                                     int64_t* elapsed_time_ms,
                                     int64_t* ntp_time_ms) {
  webrtc::AudioTransport* transport =
      transport_.load(std::memory_order_acquire);
  if (transport != nullptr) {
    transport->PullRenderData(bits_per_sample, sample_rate, number_of_channels,
                              number_of_frames, audio_data, elapsed_time_ms,
                              ntp_time_ms);
  }
}

void AdmTransportTap::ResetCaptureClock() {
  capture_.previous_ns.store(0, std::memory_order_relaxed);
}

void AdmTransportTap::ResetRenderClock() {
  render_.previous_ns.store(0, std::memory_order_relaxed);
}

void AdmTransportTap::set_playout_delay_ms(uint16_t delay_ms) {
  playout_delay_ms_.store(delay_ms, std::memory_order_relaxed);
}

uint16_t AdmTransportTap::playout_delay_ms() const {
  return playout_delay_ms_.load(std::memory_order_relaxed);
}

AdmTapStats AdmTransportTap::Read() {
  const int64_t now = NowNs();
  auto age_us = [now](const Direction& direction) -> uint64_t {
    const int64_t last = direction.last_ns.load(std::memory_order_relaxed);
    return last > 0 && now > last ? static_cast<uint64_t>(now - last) / 1000
                                  : 0;
  };

  AdmTapStats stats;
  stats.capture_callbacks = capture_.callbacks.load(std::memory_order_relaxed);
  stats.render_callbacks = render_.callbacks.load(std::memory_order_relaxed);
  stats.capture_max_gap_us =
      capture_.max_gap_ns.exchange(0, std::memory_order_relaxed) / 1000;
  stats.render_max_gap_us =
      render_.max_gap_ns.exchange(0, std::memory_order_relaxed) / 1000;
  stats.last_capture_age_us = age_us(capture_);
  stats.last_render_age_us = age_us(render_);
  stats.capture_rate = capture_rate_.load(std::memory_order_relaxed);
  stats.capture_channels = capture_channels_.load(std::memory_order_relaxed);
  stats.capture_raw_sumsq = capture_sumsq_.load(std::memory_order_relaxed);
  stats.capture_raw_samples = capture_samples_.load(std::memory_order_relaxed);
  stats.playout_delay_ms = playout_delay_ms_.load(std::memory_order_relaxed);
  const uint32_t total_delay =
      capture_total_delay_ms_.load(std::memory_order_relaxed);
  stats.recording_delay_ms = total_delay > stats.playout_delay_ms
                                 ? total_delay - stats.playout_delay_ms
                                 : 0;
  stats.overlapping_callbacks = overlapping_.load(std::memory_order_relaxed);
  return stats;
}

std::optional<webrtc::AudioDeviceModule::Stats> AdmTransportTap::DeviceStats()
    const {
  const uint64_t render_frames = render_.frames.load(std::memory_order_relaxed);
  const uint64_t capture_frames =
      capture_.frames.load(std::memory_order_relaxed);
  if (render_frames == 0 && capture_frames == 0) {
    return std::nullopt;
  }
  webrtc::AudioDeviceModule::Stats stats;
  stats.total_samples_count = render_frames;
  stats.total_samples_duration_s =
      render_.duration_s.load(std::memory_order_relaxed);
  stats.total_playout_delay_s = render_.delay_s.load(std::memory_order_relaxed);
  stats.total_capture_samples_count = capture_frames;
  stats.total_capture_samples_duration_s =
      capture_.duration_s.load(std::memory_order_relaxed);
  stats.total_capture_delay_s =
      capture_.delay_s.load(std::memory_order_relaxed);
  return stats;
}

int32_t AdmSyntheticTransport::RecordedDataIsAvailable(
    const void* /* audio_samples */,
    size_t /* samples_per_channel */,
    size_t /* bytes_per_frame */,
    size_t /* channels */,
    uint32_t /* sample_rate */,
    uint32_t /* total_delay_ms */,
    int32_t /* clock_drift */,
    uint32_t /* current_mic_level */,
    bool /* key_pressed */,
    uint32_t& /* new_mic_level */) {
  return 0;
}

int32_t AdmSyntheticTransport::NeedMorePlayData(size_t samples_per_channel,
                                                size_t bytes_per_frame,
                                                size_t channels,
                                                uint32_t sample_rate,
                                                void* audio_samples,
                                                size_t& samples_out,
                                                int64_t* elapsed_time_ms,
                                                int64_t* ntp_time_ms) {
  return tap_->RenderUncounted(samples_per_channel, bytes_per_frame, channels,
                               sample_rate, audio_samples, samples_out,
                               elapsed_time_ms, ntp_time_ms);
}

void AdmSyntheticTransport::PullRenderData(int bits_per_sample,
                                           int sample_rate,
                                           size_t number_of_channels,
                                           size_t number_of_frames,
                                           void* audio_data,
                                           int64_t* elapsed_time_ms,
                                           int64_t* ntp_time_ms) {
  tap_->PullRenderData(bits_per_sample, sample_rate, number_of_channels,
                       number_of_frames, audio_data, elapsed_time_ms,
                       ntp_time_ms);
}

// =============================================================================
// AdmProxy
// =============================================================================

AdmProxy::AdmProxy(const webrtc::Environment& env,
                   webrtc::Thread* worker_thread,
                   webrtc::scoped_refptr<RootAudioMixer> mixer)
    : AdmProxy(env, worker_thread, std::move(mixer), CreatePlatformAdm(env)) {}

AdmProxy::AdmProxy(
    const webrtc::Environment& env,
    webrtc::Thread* worker_thread,
    webrtc::scoped_refptr<RootAudioMixer> mixer,
    webrtc::scoped_refptr<webrtc::AudioDeviceModule> platform_adm)
    : env_(env),
      worker_thread_(worker_thread),
      mixer_(std::move(mixer)),
      platform_adm_(std::move(platform_adm)) {
  // Create the synthetic ADM for synthetic mode. SyntheticAudioDevice pumps
  // the WebRTC audio pipeline without platform audio, allowing FFI callbacks
  // to receive decoded remote audio.
  synthetic_adm_ = webrtc::make_ref_counted<SyntheticAudioDevice>(env_);
  if (synthetic_adm_->Init() != 0) {
    RTC_LOG(LS_ERROR) << "AdmProxy: Failed to initialize synthetic ADM";
  }
  // It pulls through the tap's render guard, uncounted; with no transport
  // registered yet it gets silence.
  synthetic_adm_->RegisterAudioCallback(&synthetic_transport_);

  if (platform_adm_) {
    int32_t init_result = platform_adm_->Init();
    if (init_result != 0) {
      RTC_LOG(LS_ERROR) << "AdmProxy: Platform ADM Init() failed with error=" << init_result;
      platform_adm_ = nullptr;
    }
  }

  if (platform_adm_) {
#if !LK_ADM_MOBILE
    // Otherwise Windows can open the mic through the voice capture DMO (16 kHz,
    // WebRTC's echo canceller off).
    platform_adm_->EnableBuiltInAEC(false);
    platform_adm_->EnableBuiltInAGC(false);
    platform_adm_->EnableBuiltInNS(false);
#if defined(WEBRTC_WIN)
    platform_adm_->SetPlayoutDevice(webrtc::AudioDeviceModule::kDefaultDevice);
    platform_adm_->SetRecordingDevice(
        webrtc::AudioDeviceModule::kDefaultDevice);
#endif
#endif
    platform_raw_.store(platform_adm_.get(), std::memory_order_release);
  }

#if !LK_ADM_MOBILE
  device_thread_ = webrtc::Thread::Create();
  device_thread_->SetName("audio-device", nullptr);
  device_thread_->Start();
#if defined(WEBRTC_WIN)
  device_thread_->PostTask(
      [] { CoInitializeEx(nullptr, COINIT_MULTITHREADED); });
#endif
#endif
}

AdmProxy::~AdmProxy() {
  RTC_LOG(LS_VERBOSE) << "AdmProxy::~AdmProxy()";

  RunDeviceTaskAndWait([this] {
    {
      AdmLock lock(&mutex_);
      ShutdownLocked();
    }
#if defined(WEBRTC_WIN) && !LK_ADM_MOBILE
    CoUninitialize();
#endif
  });
  if (device_thread_) {
    device_thread_->Stop();
  }

  synthetic_adm_ = nullptr;
  platform_raw_.store(nullptr, std::memory_order_release);
  platform_adm_ = nullptr;
}

// =============================================================================
// Device thread
// =============================================================================

void AdmProxy::PostDeviceTask(absl::AnyInvocable<void() &&> task) {
#if LK_ADM_MOBILE
  std::move(task)();
#else
  device_thread_->PostTask(std::move(task));
#endif
}

void AdmProxy::RunDeviceTaskAndWait(absl::AnyInvocable<void() &&> task) {
#if LK_ADM_MOBILE
  std::move(task)();
#else
  if (device_thread_->IsCurrent()) {
    std::move(task)();
    return;
  }
  webrtc::Event done;
  device_thread_->PostTask([&task, &done] {
    std::move(task)();
    done.Set();
  });
  done.Wait(webrtc::Event::kForever, webrtc::Event::kForever);
#endif
}

AdmProxy::DesiredState AdmProxy::Desired() const {
  AdmLock lock(&state_mutex_);
  return desired_;
}

void AdmProxy::RequestReconcile() {
  if (reconcile_posted_.exchange(true, std::memory_order_acq_rel)) {
    return;
  }
  PostDeviceTask([this] {
    // An RMW, so a request that found the flag set is seen by this pass.
    reconcile_posted_.exchange(false, std::memory_order_acq_rel);
    AdmLock lock(&mutex_);
    ReconcileLocked(nullptr);
  });
}

void AdmProxy::PostRequest(bool recording,
                           bool playout,
                           AdmRequestCallback done) {
  PostDeviceTask([this, recording, playout, done = std::move(done)]() mutable {
    AdmRequestOutcome outcome;
    outcome.recording.requested = recording;
    outcome.playout.requested = playout;
    {
      AdmLock lock(&mutex_);
      ReconcileLocked(&outcome);
    }
    if (done) {
      done(outcome);
    }
  });
}

void AdmProxy::ReconcileLocked(AdmRequestOutcome* outcome) {
  const DesiredState desired = Desired();
  if (!terminated_) {
    ReconcilePlayoutLocked(desired, outcome ? &outcome->playout : nullptr);
    ReconcileRecordingLocked(desired, outcome ? &outcome->recording : nullptr);
  }
  PublishStateLocked(desired);
  if (outcome) {
    CompleteOutcomeLocked(desired, outcome);
  }
}

bool AdmProxy::StopDeviceLocked(bool playout, AdmDirectionOutcome* outcome) {
  webrtc::AudioDeviceModule* platform_adm = platform();
  const int32_t result =
      playout ? platform_adm->StopPlayout() : platform_adm->StopRecording();
  if (outcome) {
    outcome->stop_result = result;
  }
  if (!playout && capture_state_) {
    // No frame may clear it now. After a failed stop a device thread that
    // still runs publishes it again on its next frame.
    capture_state_->ClearSpeaking();
  }
  if (result == 0) {
    return true;
  }
  device_stop_failures_.fetch_add(1, std::memory_order_relaxed);
  RTC_LOG(LS_WARNING) << "AdmProxy: Stop" << (playout ? "Playout" : "Recording")
                      << " failed: " << result;
  if (outcome) {
    outcome->attempted = true;
  }
#if LK_ADM_MOBILE
  return true;
#else
  // The device thread it could not join may still call back, so no second
  // puller or recorder starts before the retry.
  ScheduleRetryLocked();
  return false;
#endif
}

void AdmProxy::ReconcilePlayoutLocked(const DesiredState& desired,
                                      AdmDirectionOutcome* outcome) {
  webrtc::AudioDeviceModule* platform_adm = platform();
  const bool platform_mode =
      platform_adm && desired.ref_count > 0 && desired.playout_enabled;
  const bool want_platform =
      platform_mode &&
      (desired.engine_playout || desired.hold || desired.app_playout);

  if (platform_adm &&
      (platform_adm->Playing() || platform_adm->PlayoutIsInitialized())) {
    const bool stale =
        playout_opened_selection_gen_ != desired.playout_selection_gen ||
        playout_opened_restart_gen_ != desired.playout_restart_gen;
    if ((!want_platform || stale) && !StopDeviceLocked(true, outcome)) {
      return;
    }
  }

  if (!platform_mode) {
    if (desired.engine_playout && !synthetic_adm_->Playing()) {
      if (mixer_) {
        mixer_->ResetGapClock();
      }
      synthetic_adm_->StartPlayout();
    } else if (!desired.engine_playout && synthetic_adm_->Playing()) {
      synthetic_adm_->StopPlayout();
    }
    return;
  }

  if (synthetic_adm_->Playing()) {
    synthetic_adm_->StopPlayout();
  }
  if (!want_platform) {
    return;
  }

  if (!platform_adm->PlayoutIsInitialized()) {
    playout_opened_selection_gen_ = desired.playout_selection_gen;
    playout_opened_restart_gen_ = desired.playout_restart_gen;
    int32_t result = ApplySelectionLocked(true, desired.playout_selection);
    if (result == 0) {
      result = platform_adm->InitPlayout();
    }
    playout_init_result_.store(result, std::memory_order_relaxed);
    if (outcome) {
      outcome->attempted = true;
      outcome->init_result = result;
      outcome->found = result != kAdmResultDeviceNotFound;
    }
    if (result != 0) {
      RTC_LOG(LS_WARNING) << "AdmProxy: InitPlayout failed: " << result;
      return;
    }
  }

  if (!platform_adm->Playing()) {
    tap_.ResetRenderClock();
    if (mixer_) {
      mixer_->ResetGapClock();
    }
    const int32_t result = platform_adm->StartPlayout();
    playout_start_result_.store(result, std::memory_order_relaxed);
    if (outcome) {
      outcome->attempted = true;
      outcome->start_result = result;
    }
    if (result != 0) {
      RTC_LOG(LS_WARNING) << "AdmProxy: StartPlayout failed: " << result;
      return;
    }
    uint16_t delay_ms = 0;
    if (platform_adm->PlayoutDelay(&delay_ms) == 0) {
      tap_.set_playout_delay_ms(delay_ms);
    }
    SchedulePlayoutDelayRefreshLocked();
  }
}

void AdmProxy::ReconcileRecordingLocked(const DesiredState& desired,
                                        AdmDirectionOutcome* outcome) {
  webrtc::AudioDeviceModule* platform_adm = platform();
  if (!platform_adm) {
    return;
  }
  const bool want = desired.ref_count > 0 && desired.recording_enabled &&
                    (desired.engine_recording || desired.app_recording);

  if (platform_adm->Recording() || platform_adm->RecordingIsInitialized()) {
    const bool stale =
        recording_opened_selection_gen_ != desired.recording_selection_gen ||
        recording_opened_restart_gen_ != desired.recording_restart_gen;
    if ((!want || stale) && !StopDeviceLocked(false, outcome)) {
      return;
    }
  }
  if (!want) {
    return;
  }

  if (!platform_adm->RecordingIsInitialized()) {
    recording_opened_selection_gen_ = desired.recording_selection_gen;
    recording_opened_restart_gen_ = desired.recording_restart_gen;
    int32_t result = ApplySelectionLocked(false, desired.recording_selection);
    if (result == 0) {
      result = platform_adm->InitRecording();
    }
    recording_init_result_.store(result, std::memory_order_relaxed);
    if (outcome) {
      outcome->attempted = true;
      outcome->init_result = result;
      outcome->found = result != kAdmResultDeviceNotFound;
    }
    if (result != 0) {
      RTC_LOG(LS_WARNING) << "AdmProxy: InitRecording failed: " << result;
      return;
    }
  }

  if (!platform_adm->Recording()) {
    tap_.ResetCaptureClock();
    if (capture_state_) {
      capture_state_->stream_epoch.fetch_add(1, std::memory_order_release);
    }
    const int32_t result = platform_adm->StartRecording();
    recording_start_result_.store(result, std::memory_order_relaxed);
    if (outcome) {
      outcome->attempted = true;
      outcome->start_result = result;
    }
    if (result != 0) {
      RTC_LOG(LS_WARNING) << "AdmProxy: StartRecording failed: " << result;
    }
  }
}

void AdmProxy::CompleteOutcomeLocked(const DesiredState& desired,
                                     AdmRequestOutcome* outcome) {
  if (outcome->playout.requested && !outcome->playout.attempted) {
    outcome->playout.found =
        ResolveSelectionLocked(true, desired.playout_selection) !=
        kAdmResolveNotFound;
  }
  if (outcome->recording.requested && !outcome->recording.attempted) {
    outcome->recording.found =
        ResolveSelectionLocked(false, desired.recording_selection) !=
        kAdmResolveNotFound;
  }
}

void AdmProxy::PublishStateLocked(const DesiredState& desired) {
  webrtc::AudioDeviceModule* platform_adm = terminated_ ? nullptr : platform();
  const bool playout_mode =
      platform_adm && desired.ref_count > 0 && desired.playout_enabled;
  const bool recording_mode =
      platform_adm && desired.ref_count > 0 && desired.recording_enabled;
  const bool platform_playing = platform_adm && platform_adm->Playing();
  const bool platform_playout_initialized =
      platform_adm && platform_adm->PlayoutIsInitialized();
  const bool platform_recording = platform_adm && platform_adm->Recording();
  const bool platform_recording_initialized =
      platform_adm && platform_adm->RecordingIsInitialized();

  if (!platform_playing) {
    tap_.set_playout_delay_ms(0);
  }
  platform_playing_.store(platform_playing, std::memory_order_release);
  platform_recording_.store(platform_recording, std::memory_order_release);
  platform_active_.store(platform_adm && desired.ref_count > 0,
                         std::memory_order_release);
  platform_playout_mode_.store(playout_mode, std::memory_order_release);
  device_playing_.store(
      playout_mode ? platform_playing
                   : !terminated_ && synthetic_adm_->Playing(),
      std::memory_order_release);
  device_playout_initialized_.store(
      playout_mode && platform_playout_initialized, std::memory_order_release);
  device_recording_.store(recording_mode && platform_recording,
                          std::memory_order_release);
  device_recording_initialized_.store(
      recording_mode && platform_recording_initialized,
      std::memory_order_release);
}

int AdmProxy::ResolveSelectionLocked(
    [[maybe_unused]] bool playout,
    [[maybe_unused]] const AdmDeviceSelection& selection) {
#if LK_ADM_MOBILE
  return kAdmResolveDefault;
#else
  webrtc::AudioDeviceModule* platform_adm = platform();
  if (!platform_adm) {
    return kAdmResolveNotFound;
  }
  const int count = playout ? platform_adm->PlayoutDevices()
                            : platform_adm->RecordingDevices();
  return ResolveAdmDeviceIndex(
      selection, count,
      [platform_adm, playout](int index, std::string* name, std::string* guid) {
        char name_buf[webrtc::kAdmMaxDeviceNameSize] = {0};
        char guid_buf[webrtc::kAdmMaxGuidSize] = {0};
        const uint16_t device = static_cast<uint16_t>(index);
        const int32_t result =
            playout
                ? platform_adm->PlayoutDeviceName(device, name_buf, guid_buf)
                : platform_adm->RecordingDeviceName(device, name_buf, guid_buf);
        if (result != 0) {
          return false;
        }
        *name = name_buf;
        *guid = guid_buf;
        return true;
      });
#endif
}

int32_t AdmProxy::ApplySelectionLocked(
    [[maybe_unused]] bool playout,
    [[maybe_unused]] const AdmDeviceSelection& selection) {
#if LK_ADM_MOBILE
  return 0;
#else
  webrtc::AudioDeviceModule* platform_adm = platform();
  if (!platform_adm) {
    return -1;
  }
  const int index = ResolveSelectionLocked(playout, selection);
  if (index == kAdmResolveNotFound) {
    RTC_LOG(LS_WARNING) << "AdmProxy: the selected "
                        << (playout ? "playout" : "recording")
                        << " device is not present";
    return kAdmResultDeviceNotFound;
  }
  if (index == kAdmResolveDefault) {
#if defined(WEBRTC_WIN)
    constexpr auto kDefault = webrtc::AudioDeviceModule::kDefaultDevice;
    return playout ? platform_adm->SetPlayoutDevice(kDefault)
                   : platform_adm->SetRecordingDevice(kDefault);
#else
    return playout ? platform_adm->SetPlayoutDevice(uint16_t{0})
                   : platform_adm->SetRecordingDevice(uint16_t{0});
#endif
  }
  const uint16_t device = static_cast<uint16_t>(index);
  return playout ? platform_adm->SetPlayoutDevice(device)
                 : platform_adm->SetRecordingDevice(device);
#endif
}

void AdmProxy::SchedulePlayoutDelayRefreshLocked() {
#if !LK_ADM_MOBILE
  if (delay_refresh_scheduled_) {
    return;
  }
  delay_refresh_scheduled_ = true;
  device_thread_->PostDelayedTask(
      [this] {
        AdmLock lock(&mutex_);
        delay_refresh_scheduled_ = false;
        webrtc::AudioDeviceModule* platform_adm = platform();
        // platform_playing_ rather than Playing(), which logs on every call.
        if (terminated_ || !platform_adm ||
            !platform_playout_mode_.load(std::memory_order_acquire) ||
            !platform_playing_.load(std::memory_order_acquire)) {
          return;
        }
        uint16_t delay_ms = 0;
        if (platform_adm->PlayoutDelay(&delay_ms) == 0) {
          tap_.set_playout_delay_ms(delay_ms);
        }
        SchedulePlayoutDelayRefreshLocked();
      },
      kPlayoutDelayRefresh);
#endif
}

void AdmProxy::ScheduleRetryLocked() {
#if !LK_ADM_MOBILE
  if (retry_scheduled_) {
    return;
  }
  retry_scheduled_ = true;
  device_thread_->PostDelayedTask(
      [this] {
        AdmLock lock(&mutex_);
        retry_scheduled_ = false;
        ReconcileLocked(nullptr);
      },
      kStopFailureRetry);
#endif
}

void AdmProxy::ShutdownLocked() {
  if (terminated_) {
    return;
  }
  terminated_ = true;
  // The Windows ADM's Terminate releases its clients without stopping the
  // device threads, so stop them first.
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    platform_adm->StopPlayout();
    platform_adm->StopRecording();
    if (capture_state_) {
      capture_state_->ClearSpeaking();
    }
    platform_adm->RegisterAudioCallback(nullptr);
    platform_adm->Terminate();
  }
  if (synthetic_adm_) {
    synthetic_adm_->StopPlayout();
    synthetic_adm_->Terminate();
  }
  tap_.set_transport(nullptr);
  audio_transport_ = nullptr;
  PublishStateLocked(Desired());
}

// =============================================================================
// Platform ADM Lifecycle Management
// =============================================================================

#if defined(__ANDROID__)
// Lazily creates the Platform ADM on Android. Must be called with mutex held.
// Returns true if ADM is available (either already existed or successfully created).
bool AdmProxy::EnsurePlatformAdmCreated() {
  if (platform_adm_) {
    return true;  // Already created
  }

  // Use CreateAndroidAudioDeviceModule which properly uses GetAppContext()
  // to get the application context set via ContextUtils.initialize().
  platform_adm_ = webrtc::CreateAndroidAudioDeviceModule(
      env_, webrtc::AudioDeviceModule::kPlatformDefaultAudio);

  if (!platform_adm_) {
    RTC_LOG(LS_ERROR) << "AdmProxy: CreateAndroidAudioDeviceModule returned nullptr. "
                      << "Ensure ContextUtils.initialize() was called.";
    return false;
  }

  int32_t init_result = platform_adm_->Init();
  if (init_result != 0) {
    RTC_LOG(LS_ERROR) << "AdmProxy: Platform ADM Init() failed with error=" << init_result;
    platform_adm_ = nullptr;
    return false;
  }

  if (audio_transport_) {
    platform_adm_->RegisterAudioCallback(&tap_);
  }
  platform_raw_.store(platform_adm_.get(), std::memory_order_release);
  return true;
}
#endif

bool AdmProxy::AcquirePlatformAdm() {
#if defined(__ANDROID__)
  {
    // On Android, lazily create the Platform ADM on first acquire.
    // This ensures JNI is fully initialized before we try to create the ADM.
    AdmLock lock(&mutex_);
    if (!EnsurePlatformAdmCreated()) {
      RTC_LOG(LS_ERROR) << "AdmProxy::AcquirePlatformAdm() - Failed to create Platform ADM";
      return false;
    }
  }
#else
  if (!platform()) {
    RTC_LOG(LS_ERROR) << "AdmProxy::AcquirePlatformAdm() - Platform ADM not available";
    return false;
  }
#endif

  {
    AdmLock lock(&state_mutex_);
    desired_.ref_count++;
    // Right away, not after the device thread's pass: effects played just
    // after the acquire must not find the bus idle.
    if (mixer_) {
      mixer_->SetPlatformActive(true);
    }
  }
  RequestReconcile();
  return true;
}

void AdmProxy::ReleasePlatformAdm() {
  {
    AdmLock lock(&state_mutex_);
    if (desired_.ref_count <= 0) {
      RTC_LOG(LS_WARNING) << "AdmProxy::ReleasePlatformAdm() called with ref_count="
                          << desired_.ref_count;
      return;
    }
    // The Platform ADM is not terminated here; it stays alive until the
    // destructor, which avoids iOS KVO races from re-creating it.
    if (--desired_.ref_count == 0) {
      desired_.hold = false;
      desired_.app_playout = false;
      desired_.app_recording = false;
      if (mixer_) {
        mixer_->SetPlatformActive(false);
      }
    }
  }
  RequestReconcile();
}

int AdmProxy::platform_adm_ref_count() const {
  AdmLock lock(&state_mutex_);
  return desired_.ref_count;
}

bool AdmProxy::is_platform_adm_active() const {
  AdmLock lock(&state_mutex_);
  return platform() != nullptr && desired_.ref_count > 0;
}

// =============================================================================
// Recording/Playout Control
// =============================================================================

void AdmProxy::set_recording_enabled(bool enabled) {
  {
    AdmLock lock(&state_mutex_);
    if (desired_.recording_enabled == enabled) {
      return;
    }
    desired_.recording_enabled = enabled;
  }
  RequestReconcile();
}

bool AdmProxy::recording_enabled() const {
  AdmLock lock(&state_mutex_);
  return desired_.recording_enabled;
}

void AdmProxy::set_playout_enabled(bool enabled) {
  {
    AdmLock lock(&state_mutex_);
    if (desired_.playout_enabled == enabled) {
      return;
    }
    desired_.playout_enabled = enabled;
  }
  RequestReconcile();
}

bool AdmProxy::playout_enabled() const {
  AdmLock lock(&state_mutex_);
  return desired_.playout_enabled;
}

void AdmProxy::SelectDevices(std::optional<AdmDeviceSelection> recording,
                             std::optional<AdmDeviceSelection> playout,
                             AdmRequestCallback done) {
  {
    AdmLock lock(&state_mutex_);
    if (recording && !(*recording == desired_.recording_selection)) {
      desired_.recording_selection = *recording;
      desired_.recording_selection_gen++;
    }
    if (playout && !(*playout == desired_.playout_selection)) {
      desired_.playout_selection = *playout;
      desired_.playout_selection_gen++;
    }
  }
  PostRequest(recording.has_value(), playout.has_value(), std::move(done));
}

void AdmProxy::RestartDevices(bool recording,
                              bool playout,
                              AdmRequestCallback done) {
  {
    AdmLock lock(&state_mutex_);
    if (recording) {
      desired_.recording_restart_gen++;
    }
    if (playout) {
      desired_.playout_restart_gen++;
    }
  }
  PostRequest(recording, playout, std::move(done));
}

void AdmProxy::SetPlayoutHold(bool hold) {
  {
    AdmLock lock(&state_mutex_);
    if (desired_.hold == hold) {
      return;
    }
    desired_.hold = hold;
  }
  RequestReconcile();
}

bool AdmProxy::playout_hold() const {
  AdmLock lock(&state_mutex_);
  return desired_.hold;
}

void AdmProxy::SetAppPlayout(bool start) {
  {
    AdmLock lock(&state_mutex_);
    desired_.app_playout = start;
  }
  RequestReconcile();
}

void AdmProxy::SetAppRecording(bool start) {
  {
    AdmLock lock(&state_mutex_);
    desired_.app_recording = start;
  }
  RequestReconcile();
}

void AdmProxy::SetDeviceTrackProcessing(bool echo_cancellation,
                                        bool noise_suppression) {
  device_track_echo_cancellation_.store(echo_cancellation,
                                        std::memory_order_relaxed);
  device_track_noise_suppression_.store(noise_suppression,
                                        std::memory_order_relaxed);
}

void AdmProxy::SetCaptureState(
    std::shared_ptr<RootCaptureState> capture_state) {
  AdmLock lock(&mutex_);
  capture_state_ = std::move(capture_state);
}

void AdmProxy::FlushDeviceTasksForTest() {
  RunDeviceTaskAndWait([] {});
}

bool AdmProxy::SyntheticPlayingForTest() const {
  return synthetic_adm_ && synthetic_adm_->Playing();
}

AdmNativeStats AdmProxy::ReadNativeStats() {
  AdmNativeStats stats;
  stats.tap = tap_.Read();
  stats.recording = platform_recording_.load(std::memory_order_acquire);
  stats.playing = platform_playing_.load(std::memory_order_acquire);
  stats.recording_init_result =
      recording_init_result_.load(std::memory_order_relaxed);
  stats.recording_start_result =
      recording_start_result_.load(std::memory_order_relaxed);
  stats.playout_init_result =
      playout_init_result_.load(std::memory_order_relaxed);
  stats.playout_start_result =
      playout_start_result_.load(std::memory_order_relaxed);
  stats.echo_cancellation =
      device_track_echo_cancellation_.load(std::memory_order_relaxed);
  stats.noise_suppression =
      device_track_noise_suppression_.load(std::memory_order_relaxed);
  stats.audio_thread_lock_violations = AudioThreadLockViolations();
  stats.audio_thread_logs_dropped = DroppedAudioThreadLogs();
  stats.overlapping_callbacks = stats.tap.overlapping_callbacks;
  stats.device_stop_failures =
      device_stop_failures_.load(std::memory_order_relaxed);
  if (mixer_) {
    RootAudioMixerStats mix = mixer_->ReadStats();
    stats.mix_calls = mix.mix_calls;
    stats.mix_max_gap_us = mix.max_gap_us;
    stats.mix_sources = mix.sources;
    stats.mix_ssrcs = std::move(mix.ssrcs);
    stats.mix_out_sumsq = mix.out_sumsq;
    stats.mix_out_samples = mix.out_samples;
    stats.effects_played = mix.effects_played;
    stats.effects_dropped = mix.effects_dropped;
    stats.overlapping_callbacks += mix.overlapping_mixes;
  }
  return stats;
}

// =============================================================================
// AudioDeviceModule Interface Implementation
// =============================================================================

int32_t AdmProxy::ActiveAudioLayer(AudioLayer* audioLayer) const {
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->ActiveAudioLayer(audioLayer);
  }
  *audioLayer = AudioLayer::kDummyAudio;
  return 0;
}

int32_t AdmProxy::RegisterAudioCallback(webrtc::AudioTransport* transport) {
  RunDeviceTaskAndWait([this, transport] {
    AdmLock lock(&mutex_);
    audio_transport_ = transport;
    tap_.set_transport(transport);
    if (webrtc::AudioDeviceModule* platform_adm = platform()) {
      platform_adm->RegisterAudioCallback(transport ? &tap_ : nullptr);
    }
  });
  return 0;
}

int32_t AdmProxy::Init() {
  // Init is a no-op: both ADMs are initialized when they are created.
  return 0;
}

int32_t AdmProxy::Terminate() {
  RunDeviceTaskAndWait([this] {
    AdmLock lock(&mutex_);
    ShutdownLocked();
  });
  return 0;
}

bool AdmProxy::Initialized() const {
  // We're initialized if at least one ADM is initialized
  webrtc::AudioDeviceModule* platform_adm = platform();
  bool synthetic_init = synthetic_adm_ && synthetic_adm_->Initialized();
  bool platform_init = platform_adm && platform_adm->Initialized();
  return synthetic_init || platform_init;
}

// Enumeration goes straight to the Platform ADM, which serializes it internally,
// so a device list is never stuck behind a device being stopped.
int16_t AdmProxy::PlayoutDevices() {
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->PlayoutDevices();
  }
  // In synthetic mode, return 0 devices (no platform audio)
  return 0;
}

int16_t AdmProxy::RecordingDevices() {
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->RecordingDevices();
  }
  // In synthetic mode, return 0 devices (no platform audio)
  return 0;
}

int32_t AdmProxy::PlayoutDeviceName(uint16_t index,
                                    char name[webrtc::kAdmMaxDeviceNameSize],
                                    char guid[webrtc::kAdmMaxGuidSize]) {
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->PlayoutDeviceName(index, name, guid);
  }
  return -1;
}

int32_t AdmProxy::RecordingDeviceName(uint16_t index,
                                      char name[webrtc::kAdmMaxDeviceNameSize],
                                      char guid[webrtc::kAdmMaxGuidSize]) {
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->RecordingDeviceName(index, name, guid);
  }
  return -1;
}

int32_t AdmProxy::SetPlayoutDevice([[maybe_unused]] uint16_t index) {
#if LK_ADM_MOBILE
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->SetPlayoutDevice(index);
  }
#endif
  return 0;
}

int32_t AdmProxy::SetPlayoutDevice(
    [[maybe_unused]] WindowsDeviceType device) {
#if LK_ADM_MOBILE
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->SetPlayoutDevice(device);
  }
#endif
  return 0;
}

int32_t AdmProxy::SetRecordingDevice([[maybe_unused]] uint16_t index) {
#if LK_ADM_MOBILE
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->SetRecordingDevice(index);
  }
#endif
  return 0;
}

int32_t AdmProxy::SetRecordingDevice(
    [[maybe_unused]] WindowsDeviceType device) {
#if LK_ADM_MOBILE
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->SetRecordingDevice(device);
  }
#endif
  return 0;
}

int32_t AdmProxy::PlayoutIsAvailable(bool* available) {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->PlayoutIsAvailable(available);
  }
  *available = true;  // Synthetic playout is always available
  return 0;
}

// Opening happens on StartPlayout/StartRecording. libwebrtc's early
// InitRecording on SetSend runs for every send channel, including ones that
// only carry an external source, so it must not open the mic.
int32_t AdmProxy::InitPlayout() {
  return 0;
}

bool AdmProxy::PlayoutIsInitialized() const {
  return device_playout_initialized_.load(std::memory_order_acquire);
}

int32_t AdmProxy::RecordingIsAvailable(bool* available) {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->RecordingIsAvailable(available);
  }
  *available = false;  // Recording not available in synthetic mode
  return 0;
}

int32_t AdmProxy::InitRecording() {
  return 0;
}

bool AdmProxy::RecordingIsInitialized() const {
  return device_recording_initialized_.load(std::memory_order_acquire);
}

int32_t AdmProxy::StartPlayout() {
  {
    AdmLock lock(&state_mutex_);
    desired_.engine_playout = true;
    engine_playout_.store(true, std::memory_order_release);
  }
  RequestReconcile();
  return 0;
}

int32_t AdmProxy::StopPlayout() {
  {
    AdmLock lock(&state_mutex_);
    desired_.engine_playout = false;
    engine_playout_.store(false, std::memory_order_release);
  }
  RequestReconcile();
  return 0;
}

// What libwebrtc started and the device delivers. A device kept running only by
// the hold or the app reports false, so AudioState still starts playout for its
// streams.
bool AdmProxy::Playing() const {
  return engine_playout_.load(std::memory_order_acquire) &&
         device_playing_.load(std::memory_order_acquire);
}

int32_t AdmProxy::StartRecording() {
  {
    AdmLock lock(&state_mutex_);
    desired_.engine_recording = true;
    engine_recording_.store(true, std::memory_order_release);
  }
  RequestReconcile();
  return 0;
}

int32_t AdmProxy::StopRecording() {
  {
    AdmLock lock(&state_mutex_);
    desired_.engine_recording = false;
    engine_recording_.store(false, std::memory_order_release);
  }
  RequestReconcile();
  return 0;
}

bool AdmProxy::Recording() const {
  return engine_recording_.load(std::memory_order_acquire) &&
         device_recording_.load(std::memory_order_acquire);
}

int32_t AdmProxy::InitSpeaker() {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->InitSpeaker();
  }
  return 0;
}

bool AdmProxy::SpeakerIsInitialized() const {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->SpeakerIsInitialized();
  }
  return true;
}

int32_t AdmProxy::InitMicrophone() {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->InitMicrophone();
  }
  return 0;
}

bool AdmProxy::MicrophoneIsInitialized() const {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->MicrophoneIsInitialized();
  }
  return false;
}

int32_t AdmProxy::SpeakerVolumeIsAvailable(bool* available) {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->SpeakerVolumeIsAvailable(available);
  }
  *available = false;
  return 0;
}

int32_t AdmProxy::SetSpeakerVolume(uint32_t volume) {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->SetSpeakerVolume(volume);
  }
  return -1;
}

int32_t AdmProxy::SpeakerVolume(uint32_t* volume) const {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->SpeakerVolume(volume);
  }
  return -1;
}

int32_t AdmProxy::MaxSpeakerVolume(uint32_t* maxVolume) const {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->MaxSpeakerVolume(maxVolume);
  }
  return -1;
}

int32_t AdmProxy::MinSpeakerVolume(uint32_t* minVolume) const {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->MinSpeakerVolume(minVolume);
  }
  return -1;
}

int32_t AdmProxy::MicrophoneVolumeIsAvailable(bool* available) {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->MicrophoneVolumeIsAvailable(available);
  }
  *available = false;
  return 0;
}

int32_t AdmProxy::SetMicrophoneVolume(uint32_t volume) {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->SetMicrophoneVolume(volume);
  }
  return -1;
}

int32_t AdmProxy::MicrophoneVolume(uint32_t* volume) const {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->MicrophoneVolume(volume);
  }
  return -1;
}

int32_t AdmProxy::MaxMicrophoneVolume(uint32_t* maxVolume) const {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->MaxMicrophoneVolume(maxVolume);
  }
  return -1;
}

int32_t AdmProxy::MinMicrophoneVolume(uint32_t* minVolume) const {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->MinMicrophoneVolume(minVolume);
  }
  return -1;
}

int32_t AdmProxy::SpeakerMuteIsAvailable(bool* available) {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->SpeakerMuteIsAvailable(available);
  }
  *available = false;
  return 0;
}

int32_t AdmProxy::SetSpeakerMute(bool enable) {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->SetSpeakerMute(enable);
  }
  return -1;
}

int32_t AdmProxy::SpeakerMute(bool* enabled) const {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->SpeakerMute(enabled);
  }
  return -1;
}

int32_t AdmProxy::MicrophoneMuteIsAvailable(bool* available) {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->MicrophoneMuteIsAvailable(available);
  }
  *available = false;
  return 0;
}

// On desktop libwebrtc calls this on every mute once stop-on-mute is off, and
// on Windows it is the system-wide endpoint mute.
int32_t AdmProxy::SetMicrophoneMute([[maybe_unused]] bool enable) {
#if LK_ADM_MOBILE
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->SetMicrophoneMute(enable);
  }
  return -1;
#else
  return 0;
#endif
}

int32_t AdmProxy::MicrophoneMute(bool* enabled) const {
#if LK_ADM_MOBILE
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->MicrophoneMute(enabled);
  }
  return -1;
#else
  *enabled = false;
  return 0;
#endif
}

int32_t AdmProxy::StereoPlayoutIsAvailable(bool* available) const {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->StereoPlayoutIsAvailable(available);
  }
  *available = true;
  return 0;
}

int32_t AdmProxy::SetStereoPlayout(bool enable) {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->SetStereoPlayout(enable);
  }
  return 0;
}

int32_t AdmProxy::StereoPlayout(bool* enabled) const {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->StereoPlayout(enabled);
  }
  *enabled = true;
  return 0;
}

int32_t AdmProxy::StereoRecordingIsAvailable(bool* available) const {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->StereoRecordingIsAvailable(available);
  }
  *available = false;
  return 0;
}

int32_t AdmProxy::SetStereoRecording(bool enable) {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->SetStereoRecording(enable);
  }
  return 0;
}

int32_t AdmProxy::StereoRecording(bool* enabled) const {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->StereoRecording(enabled);
  }
  *enabled = false;
  return 0;
}

// Called by the worker for every audio RTP packet. On desktop it reads the
// delay the device thread refreshes while platform playout runs.
int32_t AdmProxy::PlayoutDelay(uint16_t* delayMS) const {
#if LK_ADM_MOBILE
  webrtc::AudioDeviceModule* adm =
      platform_playout_mode_.load(std::memory_order_acquire)
          ? platform()
          : static_cast<webrtc::AudioDeviceModule*>(synthetic_adm_.get());
  if (adm) {
    return adm->PlayoutDelay(delayMS);
  }
  *delayMS = 0;
  return 0;
#else
  *delayMS = platform_playout_mode_.load(std::memory_order_acquire) &&
                     platform_playing_.load(std::memory_order_acquire)
                 ? tap_.playout_delay_ms()
                 : 0;
  return 0;
#endif
}

bool AdmProxy::BuiltInAECIsAvailable() const {
#if LK_ADM_MOBILE
  webrtc::AudioDeviceModule* platform_adm = platform();
  return platform_adm && platform_adm->BuiltInAECIsAvailable();
#else
  return false;
#endif
}

bool AdmProxy::BuiltInAGCIsAvailable() const {
#if LK_ADM_MOBILE
  webrtc::AudioDeviceModule* platform_adm = platform();
  return platform_adm && platform_adm->BuiltInAGCIsAvailable();
#else
  return false;
#endif
}

bool AdmProxy::BuiltInNSIsAvailable() const {
#if LK_ADM_MOBILE
  webrtc::AudioDeviceModule* platform_adm = platform();
  return platform_adm && platform_adm->BuiltInNSIsAvailable();
#else
  return false;
#endif
}

int32_t AdmProxy::EnableBuiltInAEC(bool enable) {
#if LK_ADM_MOBILE
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->EnableBuiltInAEC(enable);
  }
  return -1;
#else
  return enable ? -1 : 0;
#endif
}

int32_t AdmProxy::EnableBuiltInAGC(bool enable) {
#if LK_ADM_MOBILE
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->EnableBuiltInAGC(enable);
  }
  return -1;
#else
  return enable ? -1 : 0;
#endif
}

int32_t AdmProxy::EnableBuiltInNS(bool enable) {
#if LK_ADM_MOBILE
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->EnableBuiltInNS(enable);
  }
  return -1;
#else
  return enable ? -1 : 0;
#endif
}

// Only while platform audio is in use, so a managed call after a platform one
// does not report the platform device's frozen totals.
std::optional<webrtc::AudioDeviceModule::Stats> AdmProxy::GetStats() const {
  if (!platform_active_.load(std::memory_order_acquire)) {
    return std::nullopt;
  }
  return tap_.DeviceStats();
}

// With stop-on-mute, muting any audio track stops the shared mic and
// publishing any other track (screen audio) starts it. Recording instead
// follows AudioState's device send streams.
bool AdmProxy::IsStopOnMuteModeEnabled() const {
  return LK_ADM_MOBILE != 0;
}

#if defined(WEBRTC_IOS)
int AdmProxy::GetPlayoutAudioParameters(webrtc::AudioParameters* params) const {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->GetPlayoutAudioParameters(params);
  }
  return -1;
}

int AdmProxy::GetRecordAudioParameters(webrtc::AudioParameters* params) const {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->GetRecordAudioParameters(params);
  }
  return -1;
}
#endif

int32_t AdmProxy::SetObserver(webrtc::AudioDeviceObserver* observer) {
  AdmLock lock(&mutex_);
  if (webrtc::AudioDeviceModule* platform_adm = platform()) {
    return platform_adm->SetObserver(observer);
  }
  return 0;
}

}  // namespace livekit_ffi
