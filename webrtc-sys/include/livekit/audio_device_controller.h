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

#include <map>
#include <memory>
#include <string>
#include <utility>
#include <vector>

#include "api/scoped_refptr.h"
#include "livekit/adm_proxy.h"
#include "livekit/root_audio_mixer.h"
#include "livekit/root_audio_monitor.h"
#include "livekit/root_audio_processing.h"
#include "livekit/root_denoiser.h"
#include "livekit/root_voice_engine_hold.h"
#include "rust/cxx.h"

namespace webrtc {
class Event;
class Thread;
}  // namespace webrtc

namespace livekit_ffi {
class AudioDeviceController;
#if defined(ROOTAPP_HARNESS)
class AdmProxyHarness;
class RootVoiceHarness;
class RootEngineHarness;
class RootMonitorHarness;
struct RootMonitorStats;
struct RootVoiceFakeCounts;
struct AdmDeviceRender;
struct RootControlState;
#endif
struct AudioDeviceSelection;
struct AudioDeviceRequestOutcome;
struct RootAudioNativeStats;
struct RootAudioControlUpdate;
struct RootAudioCaptureFormat;
struct RootDenoiserCompletionItem;
struct RootDenoiserReturnedItem;
struct RootReturnPipelinesResult;
struct RootDenoiserStateInfo;
struct RootSpeakingInfo;
}  // namespace livekit_ffi
#include "webrtc-sys/src/audio_device_controller.rs.h"

namespace livekit_ffi {

AdmDeviceSelection ToAdmSelection(const AudioDeviceSelection& selection);
AudioDeviceRequestOutcome ToRequestOutcome(const AdmRequestOutcome& outcome);

class AudioDeviceController {
 public:
  explicit AudioDeviceController(webrtc::scoped_refptr<AdmProxy> adm_proxy);
  // The native audio engine's pieces; any of them may be null (mobile, tests).
  // `engine_hold` keeps libwebrtc's voice engine initialized while a platform
  // reference is held.
  AudioDeviceController(
      webrtc::scoped_refptr<AdmProxy> adm_proxy,
      webrtc::scoped_refptr<RootAudioMixer> mixer,
      webrtc::scoped_refptr<RootApmDecorator> apm,
      std::shared_ptr<RootCaptureState> capture,
      std::shared_ptr<RootVoiceEngineHold> engine_hold = nullptr);

  // Device enumeration
  int16_t playout_devices() const;
  int16_t recording_devices() const;
  rust::String playout_device_name(uint16_t index) const;
  rust::String recording_device_name(uint16_t index) const;
  rust::String playout_device_guid(uint16_t index) const;
  rust::String recording_device_guid(uint16_t index) const;

  // Device selection
  bool set_playout_device(uint16_t index) const;
  bool set_recording_device(uint16_t index) const;
  bool set_playout_device_by_guid(rust::String guid) const;
  bool set_recording_device_by_guid(rust::String guid) const;

  // Selection and restart applied on the device thread; `callback` runs there
  // once done.
  void select_devices(const AudioDeviceSelection& recording,
                      bool set_recording,
                      const AudioDeviceSelection& playout,
                      bool set_playout,
                      rust::Box<AudioDeviceRequestCallback> callback) const;
  void restart_devices(bool recording,
                       bool playout,
                       rust::Box<AudioDeviceRequestCallback> callback) const;
  void set_playout_hold(bool hold) const;
  RootAudioNativeStats root_audio_stats() const;

  // Live control of the native audio engine; values arrive sanitised and are
  // clamped again here. Never blocks on an audio thread or the APM. False when
  // a set field has nothing to act on in this build (no capture processor,
  // mixer or APM decorator); the other fields are applied.
  bool set_root_audio_control(const RootAudioControlUpdate& control) const;
  bool set_root_audio_source_gain(uint32_t ssrc,
                                  float tile_volume,
                                  bool paused) const;
  void clear_root_audio_source_gain(uint32_t ssrc) const;
  bool register_root_audio_effect(uint32_t id,
                                  rust::Slice<const int16_t> samples) const;
  // 0 queued, 1 unknown clip, 2 dropped, 3 idle.
  uint8_t play_root_audio_effect(uint32_t id, float gain) const;
  void stop_root_audio_effects() const;
  RootAudioCaptureFormat root_audio_capture_format() const;
  // Moves whenever the playout mixer gets a new source.
  uint64_t root_audio_sources_added() const;
  // Controls back to their defaults (unity volumes, no gains or effects,
  // AEC/NS following the device track), with the capture gate open or closed
  // (muted, push-to-talk key up). Closing it (the last session ended) also
  // hands back every denoiser pipeline, ends the NS fallback and stops the
  // mic test monitor.
  void reset_root_audio(bool close_gate) const;
  // The mic test monitor (AdmProxy::SetMonitor): 0 applied, 1 not supported
  // by this build, 2 refused while a call sends from the platform microphone,
  // 3 no platform audio reference, 4 no platform device module.
  uint8_t set_root_audio_monitor(bool enabled) const;

  // The Effects SDK hand-off (RootDenoiserControl); every call is a no-op
  // without a slot.
  bool root_denoiser_available() const;
  bool set_root_denoiser(uint64_t owner,
                         uint64_t async_id,
                         uint64_t pipeline,
                         uint32_t sample_rate) const;
  RootReturnPipelinesResult return_root_denoiser_pipelines() const;
  rust::Vec<RootDenoiserCompletionItem> take_root_denoiser_completions() const;
  rust::Vec<RootDenoiserReturnedItem> take_root_denoiser_returned() const;
  RootDenoiserStateInfo root_denoiser_state() const;
  RootSpeakingInfo root_audio_speaking() const;

  // Recording control. On desktop start/stop are the app's own request, apart
  // from libwebrtc's; init does nothing (devices open on start).
  bool stop_recording() const;
  bool init_recording() const;
  bool start_recording() const;
  bool recording_is_initialized() const;

  // Playout control, as recording.
  bool stop_playout() const;
  bool init_playout() const;
  bool start_playout() const;
  bool playout_is_initialized() const;

  // Built-in audio processing
  bool builtin_aec_is_available() const;
  bool builtin_agc_is_available() const;
  bool builtin_ns_is_available() const;
  bool enable_builtin_aec(bool enable) const;
  bool enable_builtin_agc(bool enable) const;
  bool enable_builtin_ns(bool enable) const;

  // ADM recording control
  void set_adm_recording_enabled(bool enabled) const;
  bool adm_recording_enabled() const;

  // ADM playout control
  void set_adm_playout_enabled(bool enabled) const;
  bool adm_playout_enabled() const;

  // Platform ADM lifecycle management
  bool acquire_platform_adm() const;
  void release_platform_adm() const;
  int platform_adm_ref_count() const;
  bool is_platform_adm_active() const;

 private:
  webrtc::scoped_refptr<AdmProxy> adm_proxy_;
  webrtc::scoped_refptr<RootAudioMixer> mixer_;
  webrtc::scoped_refptr<RootApmDecorator> apm_;
  std::shared_ptr<RootCaptureState> capture_;
  std::shared_ptr<RootVoiceEngineHold> engine_hold_;
  // With the capture state's slot: the rootapp-effects thread.
  std::unique_ptr<RootDenoiserControl> denoiser_;
};

#if defined(ROOTAPP_HARNESS)
int32_t resolve_audio_device_index_for_test(
    const AudioDeviceSelection& selection,
    rust::Slice<const rust::String> names,
    rust::Slice<const rust::String> guids);
// Whether a selection by `wanted` picks a device the ADM names `device`, raw
// bytes as in the ADM's buffer (unterminated when it fills it).
bool adm_name_matches_for_test(rust::Slice<const uint8_t> device,
                               rust::Str wanted);

// A RootAudioMonitor driven as its capture and render threads would (tests).
class RootMonitorHarness {
 public:
  RootMonitorHarness();
  ~RootMonitorHarness();

  void set_enabled(bool enabled) const;
  void resync() const;
  // One capture frame: rate / 100 samples of FloatS16.
  void push(int32_t rate, rust::Slice<const float> samples) const;
  // One render callback of rate / 100 frames over `base` (interleaved,
  // zero-padded); returns the buffer the device would play.
  rust::Vec<int16_t> mix(uint32_t rate,
                         size_t channels,
                         rust::Slice<const int16_t> base) const;
  RootMonitorStats stats() const;

 private:
  std::unique_ptr<RootAudioMonitor> monitor_;
};

std::unique_ptr<RootMonitorHarness> new_root_monitor_harness();

class FakePlatformAdm;
class HarnessTransport;

// Drives an AdmProxy whose platform ADM is a fake that records the calls it
// gets, so the proxy's state machine is tested without any audio device.
// "engine_*" calls are what libwebrtc calls; app_* go through the controller.
class AdmProxyHarness {
 public:
  // With `with_apm`, the controller also gets an APM decorator over a fake APM
  // (NewRootApmOverFakeForTest). Without `with_platform` the proxy has no
  // platform ADM (one that failed to initialize).
  AdmProxyHarness(rust::Slice<const rust::String> playout_guids,
                  rust::Slice<const rust::String> recording_guids,
                  bool with_apm,
                  bool with_platform = true);
  ~AdmProxyHarness();

  // Calls the fake got since the last take, e.g. "StartPlayout".
  rust::Vec<rust::String> take_calls() const;
  // The playout opens AdmProxy reported since the last take: "default" or
  // "<guid>|<name>".
  rust::Vec<rust::String> take_playout_opened() const;
  // Its channel, speaker and microphone calls since the last take, and the
  // channels of every InitPlayout ("InitPlayout stereo"), kept apart.
  rust::Vec<rust::String> take_channel_calls() const;
  void set_devices(rust::Slice<const rust::String> playout_guids,
                   rust::Slice<const rust::String> recording_guids) const;
  // UID translation as CoreAudio would answer it: uids[i] is the device with
  // GUID guids[i]; any other UID is no device.
  void set_uid_map(rust::Slice<const rust::String> uids,
                   rust::Slice<const rust::String> guids) const;
  // Enumeration serialized with device operations (as on Linux), waiting at
  // most wait_ms for one in progress; or never waiting.
  void set_enumeration_lock(bool serialize, int32_t wait_ms) const;
  // Device formats as CoreAudio would report them now (`guid` empty: the OS
  // default); the first call gives the proxy a format reader that only
  // poll_device_formats runs. A device without a format cannot be read.
  void set_device_format(bool playout,
                         rust::Str guid,
                         uint32_t rate,
                         uint32_t channels) const;
  void poll_device_formats() const;
  // The next StartPlayout blocks the device thread (holding the device
  // mutex) until released.
  void block_next_start_playout() const;
  // Whether the device thread got there within 5 s.
  bool start_playout_blocked() const;
  // Releases it after_ms from now, from another thread.
  void release_start_playout(uint32_t after_ms) const;
  // The next StopPlayout fails with `result` and leaves the device closed, as
  // the Windows ADM does when it gives up joining its render thread.
  void fail_next_stop_playout(int32_t result) const;
  // The same for StopRecording.
  void fail_next_stop_recording(int32_t result) const;
  // The next StartPlayout / StartRecording fails with `result` after the
  // device was opened, leaving its worker as the macOS ADM does.
  void fail_next_start_playout(int32_t result) const;
  void fail_next_start_recording(int32_t result) const;
  void set_device_playout_delay(uint16_t delay_ms) const;

  void acquire() const;
  void release() const;
  void enable(bool recording, bool playout) const;
  void hold(bool hold) const;
  bool held() const;
  void app_playout(bool start) const;
  void app_recording(bool start) const;
  // The controller's set_root_audio_monitor.
  uint8_t monitor(bool enabled) const;
  // One frame into the monitor, as the capture post-processor gives it.
  void monitor_push(int32_t rate, rust::Slice<const float> samples) const;
  // One 10 ms 48 kHz stereo render callback through the tap from a device
  // thread; returns what the device plays.
  rust::Vec<int16_t> render_frame() const;
  // The fake's AudioDeviceBuffer holds `level` in its playout samples.
  void stale_device_buffer(int16_t level) const;
  // One 10 ms render callback from a device thread through the fake's
  // AudioDeviceBuffer, as the Windows ADM's render thread makes it.
  AdmDeviceRender device_render() const;
  // libwebrtc's transport plays the mixer from now on.
  void engine_plays_mixer() const;
  void engine_start_playout() const;
  void engine_stop_playout() const;
  void engine_start_recording() const;
  void engine_stop_recording() const;
  int32_t engine_init_playout() const;
  int32_t engine_init_recording() const;
  // libwebrtc's adm_helpers::Init on the proxy (speaker, stereo playout,
  // microphone): each call's result, in that order.
  rust::Vec<int32_t> engine_adm_init() const;
  bool engine_stereo_playout() const;
  int32_t engine_set_stereo_playout(bool enable) const;
  int32_t engine_set_speaker_volume(uint32_t volume) const;
  int32_t engine_set_speaker_mute(bool mute) const;
  void register_transport() const;
  // WebRtcVoiceEngine::Terminate on the proxy: StopPlayout, StopRecording,
  // RegisterAudioCallback(nullptr), Terminate.
  void engine_terminate() const;
  // What the proxy's destructor does to the devices.
  void shutdown() const;
  AudioDeviceRequestOutcome select(const AudioDeviceSelection& recording,
                                   bool set_recording,
                                   const AudioDeviceSelection& playout,
                                   bool set_playout) const;
  AudioDeviceRequestOutcome restart(bool recording, bool playout) const;
  void flush() const;

  // libwebrtc's view of the proxy.
  bool playing() const;
  bool recording() const;
  bool playout_initialized() const;
  bool recording_initialized() const;
  uint16_t playout_delay() const;
  bool has_device_stats() const;
  bool stop_on_mute() const;
  bool builtin_effects_available() const;
  int32_t set_microphone_mute(bool mute) const;
  // 0 AEC, 1 AGC, 2 NS.
  int32_t enable_builtin_effect(uint8_t effect, bool enable) const;

  // The devices behind it.
  bool platform_playing() const;
  bool platform_recording() const;
  bool synthetic_playing() const;

  // Runs one device callback on a thread of its own, as the ADM would; with
  // `reenter`, libwebrtc's side starts a second one before the first returns.
  // Returns how often libwebrtc's transport was entered.
  uint32_t device_callback(bool capture, bool reenter) const;
  // Takes an AdmProxy lock on a thread marked as an audio thread and returns
  // how many violations that added.
  uint64_t lock_on_audio_thread() const;

  RootAudioNativeStats stats() const;

  // The engine controls behind the controller.
  bool mixer_platform_active() const;
  bool apply_control(const RootAudioControlUpdate& control) const;
  void reset_root_audio(bool close_gate) const;
  RootControlState control_state() const;
  const AudioDeviceController& controller() const;
  uint64_t add_fake_pipeline() const;
  uint8_t run_denoiser(uint32_t rate) const;
  void latch_denoiser_bypass(uint32_t reason) const;
  // Stores the speaking flag as the capture processor publishes it.
  void set_speaking(bool speaking) const;

 private:
  webrtc::scoped_refptr<FakePlatformAdm> fake_;
  webrtc::scoped_refptr<RootAudioMixer> mixer_;
  std::shared_ptr<RootCaptureState> capture_;
  webrtc::scoped_refptr<RootApmDecorator> apm_;
  webrtc::scoped_refptr<AdmProxy> proxy_;
  std::unique_ptr<std::vector<std::unique_ptr<RootFakeFxPipeline>>> fakes_;
  std::unique_ptr<AudioDeviceController> controller_;
  std::unique_ptr<HarnessTransport> transport_;
  struct OpenedLog {
    webrtc::Mutex mutex;
    std::vector<std::string> entries;
  };
  std::shared_ptr<OpenedLog> opened_ = std::make_shared<OpenedLog>();
  struct DeviceFormats {
    webrtc::Mutex mutex;
    std::map<std::pair<bool, std::string>, AdmDeviceFormat> formats;
    bool installed = false;
  };
  std::shared_ptr<DeviceFormats> formats_ = std::make_shared<DeviceFormats>();
};

std::unique_ptr<AdmProxyHarness> new_adm_proxy_harness(
    rust::Slice<const rust::String> playout_guids,
    rust::Slice<const rust::String> recording_guids);
std::unique_ptr<AdmProxyHarness> new_adm_proxy_harness_with_apm(
    rust::Slice<const rust::String> playout_guids,
    rust::Slice<const rust::String> recording_guids);
std::unique_ptr<AdmProxyHarness> new_adm_proxy_harness_without_platform_adm();

class HarnessApmTransport;

// The native voice path end to end over one RootCaptureState, wired as the
// factory wires it: AdmProxy over a fake platform ADM, libwebrtc's builtin APM
// with RootCaptureProcessor behind RootApmDecorator (policy changes on a
// worker thread), the denoiser slot with fake Effects SDK pipelines, and the
// controller with its rootapp-effects thread. A recorded frame goes device ->
// AdmProxy -> the APM on a device thread, as the voice engine runs it. Tests
// only; no audio device is opened.
class RootVoiceHarness {
 public:
  RootVoiceHarness();
  ~RootVoiceHarness();

  const AudioDeviceController& controller() const;
  // The app starts or stops recording (a start bumps the stream epoch).
  void recording(bool start) const;
  // libwebrtc starts or stops recording for a send stream.
  void engine_recording(bool start) const;
  // The controller's set_root_audio_monitor; returns once the device thread
  // applied it.
  uint8_t monitor(bool enabled) const;
  bool platform_playing() const;
  bool platform_recording() const;
  uint64_t stream_epoch() const;
  // One 10 ms frame of interleaved int16 channels from the device; returns
  // what the APM sends.
  rust::Vec<int16_t> capture(int32_t rate,
                             size_t channels,
                             rust::Slice<const int16_t> input) const;
  // libwebrtc's render side plays `level` on every sample and hands that
  // frame to the APM as the echo canceller's reference.
  void set_remote_level(int16_t level) const;
  // One 10 ms render callback from the device; returns what it plays.
  rust::Vec<int16_t> render(int32_t rate, size_t channels) const;
  // The last frame the APM got as its reverse stream (the AEC reference).
  rust::Vec<int16_t> reverse_input() const;
  // A fake Effects SDK pipeline owned by the harness: output round(scale x
  // input) delayed by delay_samples.
  uint64_t add_fake_pipeline(float scale, uint32_t delay_samples) const;
  // Every output of that pipeline is empty from now on (or again not).
  void set_fake_empty(uint64_t pipeline, bool empty) const;
  RootVoiceFakeCounts fake_counts(uint64_t pipeline) const;
  // Holds the worker thread, so policy applications wait, until released.
  void hold_worker(bool hold) const;
  // What the decorator applied, as the capture processor reads it.
  bool webrtc_ns_applied() const;

 private:
  RootFakeFxPipeline* Fake(uint64_t pipeline) const;

  std::unique_ptr<webrtc::Thread> worker_;
  webrtc::scoped_refptr<FakePlatformAdm> fake_;
  webrtc::scoped_refptr<RootAudioMixer> mixer_;
  std::shared_ptr<RootCaptureState> capture_;
  webrtc::scoped_refptr<RootApmDecorator> apm_;
  std::unique_ptr<HarnessApmTransport> transport_;
  webrtc::scoped_refptr<AdmProxy> proxy_;
  std::unique_ptr<AudioDeviceController> controller_;
  mutable std::vector<std::unique_ptr<RootFakeFxPipeline>> fakes_;
  std::shared_ptr<webrtc::Event> worker_hold_;
};

std::unique_ptr<RootVoiceHarness> new_root_voice_harness();

// libwebrtc's own voice engine (WebRtcVoiceEngine in a ConnectionContext, with
// its AudioState and AudioTransportImpl) over AdmProxy and a fake platform
// ADM, with the factory's APM, capture processor, monitor, mixer, controller
// and voice engine hold. A PeerConnection is stood in for by the reference it
// takes to the ConnectionContext (PeerConnection::Initialize, and its
// destructor), a call's streams by AudioState's StartPlayout/StartRecording.
// Tests only; no audio device is opened and no PeerConnection is created.
class RootEngineHarness {
 public:
  RootEngineHarness();
  ~RootEngineHarness();

  const AudioDeviceController& controller() const;
  // As a PlatformAudio handle's creation (a platform reference, both
  // directions enabled, a session start) and its drop (the session's reset
  // with the gate closed, then the reference).
  bool acquire() const;
  void release() const;
  void open_peer_connection() const;
  void close_peer_connection() const;
  void engine_playout(bool start) const;
  void engine_recording(bool start) const;
  void hold(bool hold) const;
  uint8_t monitor(bool enabled) const;
  // Returns once the worker and device threads ran what was posted to them.
  void flush() const;
  // One 10 ms frame of interleaved channels from the device's capture thread.
  void capture(int32_t rate,
               size_t channels,
               rust::Slice<const int16_t> input) const;
  // One 10 ms render callback from the device's render thread; returns what
  // the device plays.
  rust::Vec<int16_t> render(int32_t rate, size_t channels) const;
  bool platform_playing() const;
  bool platform_recording() const;
  bool synthetic_playing() const;
  rust::Vec<rust::String> take_calls() const;

 private:
  struct Parts;
  std::unique_ptr<Parts> parts_;
};

std::unique_ptr<RootEngineHarness> new_root_engine_harness();
#endif

}  // namespace livekit_ffi
