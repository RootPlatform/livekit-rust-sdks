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
#include <cstdint>
#include <functional>
#include <memory>
#include <optional>
#include <string>
#include <vector>

#include "absl/functional/any_invocable.h"
#include "api/audio/audio_frame.h"
#include "api/environment/environment.h"
#include "api/scoped_refptr.h"
#include "livekit/adm_listener_router.h"
#include "livekit/synthetic_audio_device.h"
#include "modules/audio_device/include/audio_device.h"
#include "modules/audio_device/include/audio_device_defines.h"
#include "rtc_base/synchronization/mutex.h"

#if defined(WEBRTC_IOS) || defined(__ANDROID__)
#define LK_ADM_MOBILE 1
#else
#define LK_ADM_MOBILE 0
#endif

namespace webrtc {
class Thread;
}  // namespace webrtc

namespace livekit_ffi {

class RootAudioMixer;
class RootAudioMonitor;
struct RootCaptureState;

// Last Stop/Init/Start results: 0 is success, other values are ADM errors,
// except these two, which no ADM returns.
constexpr int32_t kAdmResultNotAttempted = 1;
constexpr int32_t kAdmResultDeviceNotFound = -10000;

enum class AdmSelectionKind : uint8_t {
  kDefault = 0,
  kGuid = 1,
  kName = 2,
  kIndex = 3,
  kUid = 4,
};

// Which device a direction opens. Default is the OS default (the console role
// on Windows, index 0 elsewhere); kIndex only serves the legacy index setters.
// kUid is a macOS CoreAudio device UID, translated to the device's current
// AudioObjectID (the ADM's GUID) at every open; elsewhere it matches nothing.
struct AdmDeviceSelection {
  AdmSelectionKind kind = AdmSelectionKind::kDefault;
  std::string id;
  uint16_t index = 0;

  bool operator==(const AdmDeviceSelection& other) const = default;
};

constexpr int kAdmResolveDefault = -1;
constexpr int kAdmResolveNotFound = -2;

// A platform playout device that opened: the OS default (the console role on
// Windows, ADM index 0 elsewhere, whose name is also given) or the selected
// device's ADM GUID and name, as the selection resolved them. With `open`
// false: the platform playout device closed (nothing else is set).
struct AdmPlayoutOpened {
  bool open = true;
  bool system_default = true;
  std::string guid;
  std::string name;
};
using AdmPlayoutOpenedSink = std::function<void(const AdmPlayoutOpened&)>;

// Maps a selection onto the current device list: an index, kAdmResolveDefault,
// or kAdmResolveNotFound (never a fallback to another device). GUIDs match
// exactly; names exactly, or by the part the ADM kept of a name it cut to
// its buffer (kAdmMaxDeviceNameSize - 1 bytes); the first match wins.
int ResolveAdmDeviceIndex(
    const AdmDeviceSelection& selection,
    int device_count,
    const std::function<bool(int index, std::string* name, std::string* guid)>&
        device_at);

// An ADM name or GUID buffer as a string, also when it is unterminated.
std::string AdmDeviceString(const char* buffer, size_t size);

// Buffers for an ADM's *DeviceName with a zeroed byte past each: the macOS
// ADM may fill the name without terminating it, and libwebrtc's ADM wrapper
// logs it as a C string.
struct AdmNameBuffers {
  char name[webrtc::kAdmMaxDeviceNameSize + 1] = {};
  char guid[webrtc::kAdmMaxGuidSize + 1] = {};

  std::string Name() const {
    return AdmDeviceString(name, webrtc::kAdmMaxDeviceNameSize);
  }
  std::string Guid() const {
    return AdmDeviceString(guid, webrtc::kAdmMaxGuidSize);
  }
};

// A device UID's current ADM GUID, or nullopt when no device has that UID.
using AdmUidTranslator =
    std::function<std::optional<std::string>(const std::string& uid)>;

struct AdmDeviceFormat {
  std::string device;
  double sample_rate = 0;
  uint32_t channels = 0;
  uint32_t bytes_per_frame = 0;
  uint32_t format_flags = 0;

  bool operator==(const AdmDeviceFormat& other) const = default;
};

// An empty `guid` reads the OS default device of the direction.
using AdmDeviceFormatReader = std::function<std::optional<AdmDeviceFormat>(
    bool playout,
    const std::string& guid)>;

// A device as CoreAudio names it: its AudioObjectID (the macOS ADM's GUID)
// and its UID.
struct AdmDeviceIdentity {
  std::string id;
  std::string uid;

  bool operator==(const AdmDeviceIdentity& other) const = default;
};

// What the proxy asks CoreAudio about devices (macOS; none elsewhere).
struct AdmDeviceProbe {
  // The device a direction opens for an ADM GUID (empty: the OS default
  // now); nullopt when there is none.
  std::function<std::optional<AdmDeviceIdentity>(bool playout,
                                                 const std::string& guid)>
      identify;
  // Whether a device is still listed and alive.
  std::function<bool(const std::string& id)> alive;
};

// Why an open direction was stopped and opened again.
enum class AdmReopenReason : uint8_t {
  // SelectDevices, and the app's RestartDevices.
  kUserSelect = 0,
  kDefaultChange = 1,
  kDeviceLost = 2,
  kFormatChange = 3,
  kRenderStall = 4,
};
constexpr size_t kAdmReopenReasons = 5;

struct AdmDirectionOutcome {
  bool requested = false;
  bool found = true;
  bool attempted = false;
  int32_t stop_result = kAdmResultNotAttempted;
  int32_t init_result = kAdmResultNotAttempted;
  int32_t start_result = kAdmResultNotAttempted;
  // Why the platform ADM's Init failed (empty when it was not looked into),
  // also when the playout fallback opened the device instead.
  std::string init_detail;
};

// Why the platform ADM could not open playout, found by repeating its open
// on the same device (Windows): the step that failed, its HRESULT and the
// formats, and whether the device opens for a client whose format the audio
// engine converts.
struct AdmPlayoutDiagnosis {
  std::string text;
  bool converted_opens = false;
};
using AdmPlayoutDiagnoser =
    std::function<AdmPlayoutDiagnosis(const std::string& endpoint_id,
                                      bool stereo)>;

// Playout the proxy runs itself where the platform ADM cannot open a device
// (Windows: a shared WASAPI stream the audio engine converts to its mix
// format). It pulls 10 ms of 48 kHz int16 audio at a time through the
// transport it was created with, on a thread of its own.
class AdmPlayoutFallback {
 public:
  virtual ~AdmPlayoutFallback() = default;
  // An empty `endpoint_id` opens the default console output. 0 when open.
  virtual int32_t Init(const std::string& endpoint_id, size_t channels) = 0;
  virtual int32_t Start() = 0;
  // Stops and closes; non-zero while its thread did not stop (a later Stop
  // waits for it again).
  virtual int32_t Stop() = 0;
  virtual bool Playing() const = 0;
  virtual uint16_t DelayMs() const = 0;
  // What it opened, or the step that failed and its HRESULT.
  virtual std::string Describe() const = 0;
};
using AdmPlayoutFallbackFactory =
    std::function<std::unique_ptr<AdmPlayoutFallback>(
        webrtc::AudioTransport* transport)>;

struct AdmRequestOutcome {
  AdmDirectionOutcome recording;
  AdmDirectionOutcome playout;
};

using AdmRequestCallback = std::function<void(const AdmRequestOutcome&)>;

struct AdmTapStats {
  uint64_t capture_callbacks = 0;
  uint64_t render_callbacks = 0;
  uint64_t capture_max_gap_us = 0;
  uint64_t render_max_gap_us = 0;
  uint64_t last_capture_age_us = 0;
  uint64_t last_render_age_us = 0;
  uint32_t capture_rate = 0;
  uint32_t capture_channels = 0;
  double capture_raw_sumsq = 0;
  uint64_t capture_raw_samples = 0;
  uint32_t playout_delay_ms = 0;
  uint32_t recording_delay_ms = 0;
  uint64_t overlapping_callbacks = 0;
};

struct AdmNativeStats {
  AdmTapStats tap;
  bool recording = false;
  bool playing = false;
  int32_t recording_init_result = kAdmResultNotAttempted;
  int32_t recording_start_result = kAdmResultNotAttempted;
  int32_t playout_init_result = kAdmResultNotAttempted;
  int32_t playout_start_result = kAdmResultNotAttempted;
  uint64_t mix_calls = 0;
  uint64_t mix_max_gap_us = 0;
  uint32_t mix_sources = 0;
  std::vector<uint32_t> mix_ssrcs;
  double mix_out_sumsq = 0;
  uint64_t mix_out_samples = 0;
  uint64_t effects_played = 0;
  uint64_t effects_dropped = 0;
  bool echo_cancellation = false;
  bool noise_suppression = false;
  uint64_t audio_thread_lock_violations = 0;
  uint64_t audio_thread_logs_dropped = 0;
  uint64_t overlapping_callbacks = 0;
  uint64_t device_stop_failures = 0;
  bool device_formats_available = false;
  uint32_t playout_device_rate = 0;
  uint32_t playout_device_channels = 0;
  uint32_t recording_device_rate = 0;
  uint32_t recording_device_channels = 0;
  uint64_t playout_format_changes = 0;
  uint64_t playout_format_reopens = 0;
  uint64_t recording_format_changes = 0;
  // The proxy follows device changes itself (macOS): the open directions'
  // device UIDs (empty while closed), whether both are open on one device,
  // the ADM's CoreAudio notifications by property, each direction reopened
  // by reason, reopens that took the other direction along, and render
  // stalls recovered.
  bool device_changes_available = false;
  std::string playout_device_uid;
  std::string recording_device_uid;
  bool same_device = false;
  uint64_t device_list_notifications = 0;
  uint64_t default_output_notifications = 0;
  uint64_t default_input_notifications = 0;
  uint64_t stream_format_notifications = 0;
  uint64_t other_notifications = 0;
  std::array<uint64_t, kAdmReopenReasons> reopens{};
  uint64_t coupled_reopens = 0;
  uint64_t render_stall_recoveries = 0;
  // The proxy has a playout fallback (Windows): playout runs on it, the
  // opens that took it, and why the platform ADM's last playout Init failed
  // (empty when it succeeded).
  bool playout_fallback_available = false;
  bool playout_fallback = false;
  uint64_t playout_fallback_opens = 0;
  std::string playout_init_detail;
};

// Registered with the platform ADM in place of libwebrtc's AudioTransport, from
// the ADM's creation, so a device never pulls an AudioDeviceBuffer that has no
// transport (it plays its stale or uninitialized samples); while libwebrtc has
// none the tap plays silence. It runs on the device's capture and render
// threads: it forwards, counts and keeps the last-callback times, using only
// its own atomics.
//
// One capture and one render callback reach libwebrtc at a time. A callback
// that starts while another one of its direction is in flight (a device thread
// the ADM failed to stop, or the synthetic pump during a hand-over) is answered
// with silence and counted instead.
//
// The mic test monitor is mixed into device render callbacks after
// libwebrtc's NeedMorePlayData, which has already given its mix to the APM as
// the echo canceller's reference.
class AdmTransportTap : public webrtc::AudioTransport {
 public:
  void set_transport(webrtc::AudioTransport* transport);
  // Set once, before any device opens; may be null.
  void set_monitor(RootAudioMonitor* monitor);
  // While libwebrtc records for no send stream (the mic test, the app's own
  // start) the device's channels reach libwebrtc averaged to one, as a mono
  // send stream's RemixAndResample averages them, so the APM and the capture
  // processor run on what a call would send.
  void set_mono_capture(bool mono);

  int32_t RecordedDataIsAvailable(const void* audio_samples,
                                  size_t samples_per_channel,
                                  size_t bytes_per_frame,
                                  size_t channels,
                                  uint32_t sample_rate,
                                  uint32_t total_delay_ms,
                                  int32_t clock_drift,
                                  uint32_t current_mic_level,
                                  bool key_pressed,
                                  uint32_t& new_mic_level) override;
  int32_t RecordedDataIsAvailable(
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
      std::optional<int64_t> estimated_capture_time_ns) override;
  int32_t NeedMorePlayData(size_t samples_per_channel,
                           size_t bytes_per_frame,
                           size_t channels,
                           uint32_t sample_rate,
                           void* audio_samples,
                           size_t& samples_out,
                           int64_t* elapsed_time_ms,
                           int64_t* ntp_time_ms) override;
  void PullRenderData(int bits_per_sample,
                      int sample_rate,
                      size_t number_of_channels,
                      size_t number_of_frames,
                      void* audio_data,
                      int64_t* elapsed_time_ms,
                      int64_t* ntp_time_ms) override;

  // A render pull that is not a device callback (the synthetic pump): guarded
  // like one, but not counted.
  int32_t RenderUncounted(size_t samples_per_channel,
                          size_t bytes_per_frame,
                          size_t channels,
                          uint32_t sample_rate,
                          void* audio_samples,
                          size_t& samples_out,
                          int64_t* elapsed_time_ms,
                          int64_t* ntp_time_ms);

  // The next callback after these does not count the time before it as a gap.
  void ResetCaptureClock();
  void ResetRenderClock();

  // Device callbacks counted so far.
  uint64_t capture_callbacks() const;
  uint64_t render_callbacks() const;

  void set_playout_delay_ms(uint16_t delay_ms);
  uint16_t playout_delay_ms() const;

  // Max gaps are reset on read.
  AdmTapStats Read();
  std::optional<webrtc::AudioDeviceModule::Stats> DeviceStats() const;

 private:
  struct Direction {
    std::atomic<uint64_t> callbacks{0};
    std::atomic<int64_t> last_ns{0};
    std::atomic<int64_t> previous_ns{0};
    std::atomic<uint64_t> max_gap_ns{0};
    std::atomic<uint64_t> frames{0};
    std::atomic<double> duration_s{0.0};
    std::atomic<double> delay_s{0.0};
  };

  static void Count(Direction& direction,
                    size_t frames,
                    uint32_t sample_rate,
                    uint32_t delay_ms);
  int32_t Render(bool count,
                 size_t samples_per_channel,
                 size_t bytes_per_frame,
                 size_t channels,
                 uint32_t sample_rate,
                 void* audio_samples,
                 size_t& samples_out,
                 int64_t* elapsed_time_ms,
                 int64_t* ntp_time_ms);

  std::atomic<webrtc::AudioTransport*> transport_{nullptr};
  std::atomic<RootAudioMonitor*> monitor_{nullptr};
  Direction capture_;
  Direction render_;
  std::atomic<bool> capture_busy_{false};
  std::atomic<bool> render_busy_{false};
  std::atomic<uint64_t> overlapping_{0};
  std::atomic<uint32_t> capture_rate_{0};
  std::atomic<uint32_t> capture_channels_{0};
  std::atomic<double> capture_sumsq_{0.0};
  std::atomic<uint64_t> capture_samples_{0};
  std::atomic<uint32_t> capture_total_delay_ms_{0};
  std::atomic<uint16_t> playout_delay_ms_{0};
  std::atomic<bool> mono_capture_{true};
  // Capture thread only (inside the capture guard).
  std::array<int16_t, webrtc::AudioFrame::kMaxDataSizeSamples> mono_{};
};

// What the synthetic ADM pulls through: the tap's render guard, so it never
// runs libwebrtc's render path next to a platform render thread.
class AdmSyntheticTransport : public webrtc::AudioTransport {
 public:
  explicit AdmSyntheticTransport(AdmTransportTap* tap) : tap_(tap) {}

  using webrtc::AudioTransport::RecordedDataIsAvailable;
  int32_t RecordedDataIsAvailable(const void* audio_samples,
                                  size_t samples_per_channel,
                                  size_t bytes_per_frame,
                                  size_t channels,
                                  uint32_t sample_rate,
                                  uint32_t total_delay_ms,
                                  int32_t clock_drift,
                                  uint32_t current_mic_level,
                                  bool key_pressed,
                                  uint32_t& new_mic_level) override;
  int32_t NeedMorePlayData(size_t samples_per_channel,
                           size_t bytes_per_frame,
                           size_t channels,
                           uint32_t sample_rate,
                           void* audio_samples,
                           size_t& samples_out,
                           int64_t* elapsed_time_ms,
                           int64_t* ntp_time_ms) override;
  void PullRenderData(int bits_per_sample,
                      int sample_rate,
                      size_t number_of_channels,
                      size_t number_of_frames,
                      void* audio_data,
                      int64_t* elapsed_time_ms,
                      int64_t* ntp_time_ms) override;

 private:
  AdmTransportTap* const tap_;
};

static_assert(std::atomic<double>::is_always_lock_free);

enum class AdmMonitorResult : uint8_t {
  kOk = 0,
  kNotSupported = 1,
  // libwebrtc records for a send stream (a call publishes the microphone).
  kSending = 2,
  kNotAcquired = 3,
  // No platform ADM: it could not be created or initialized.
  kNoDevice = 4,
};

/// ADM Proxy that manages synthetic and platform audio modes.
///
/// - **Playout**: the Platform ADM plays while `ref_count > 0 &&
///   playout_enabled` and libwebrtc started playout, the app holds it
///   (`SetPlayoutHold`), the app started it (`SetAppPlayout`) or the mic test
///   monitor runs (`SetMonitor`); otherwise the synthetic ADM pumps the
///   pipeline while libwebrtc wants playout.
/// - **Recording**: the Platform ADM records while `ref_count > 0 &&
///   recording_enabled` and libwebrtc started recording (a device track's send
///   stream), the app started it (`SetAppRecording`) or the monitor runs.
///   Synthetic mode has no microphone.
///
/// libwebrtc's and the app's calls only record the desired state. On desktop a
/// dedicated `audio-device` thread brings the devices to it, so neither the
/// worker nor an FFI caller ever waits for a device to stop. The queries the
/// worker makes per audio packet read atomics. The selected device is stored
/// as {default | GUID | name | UID} per direction and applied before every
/// open.
///
/// On desktop the OS effects (Windows' voice capture DMO) are off and refused,
/// mute never stops recording or touches the endpoint mute, the speaker's
/// volume and mute are never set, and recording only opens for a device track
/// (`InitRecording` alone opens nothing). libwebrtc's stereo playout choice is
/// kept and applied before every playout open (stereo until it says
/// otherwise), since the hold can open playout before libwebrtc makes it. A device
/// that fails to stop (the Windows ADM gives up joining its thread after 2 s)
/// gets no second puller in that pass; the direction is retried a second later.
///
/// The macOS ADM keeps converting to the output format it read at InitPlayout
/// (its format-change handler refuses an output channel count that differs
/// from the recording one), so a device that changes format under a running
/// playout, e.g. a Bluetooth headset entering its hands-free profile, plays
/// the call at the wrong speed. There the device thread reads each open
/// direction's format every 100 ms and reopens playout once a change held
/// for two reads.
///
/// The macOS ADM shares one IOProc between the directions of one device and
/// chooses at each Init from what is open then: playout that joins the device
/// of a microphone with its own IOProc gets no IOProc and renders nothing
/// while Playing() is true. There playout always opens before a microphone
/// on its device (the microphone is stopped first and opened again after),
/// the proxy follows device-list and default-device changes itself instead
/// of the ADM's listener, and playout that renders nothing for 500 ms while
/// capture goes on is reopened.
///
/// The Windows ADM opens playout only as 16-bit PCM at 48, 44.1, 16, 96, 32
/// or 8 kHz in one or two channels, without the audio engine's conversion, so
/// it fails on a device whose shared-mode mix format is anything else (a
/// surround headset's 6 or 8 channels, 88.2 or 192 kHz). After a failed
/// InitPlayout the proxy repeats the ADM's open to report why, and plays
/// through a shared stream of its own that the engine converts
/// (AdmPlayoutFallback) when that opens.
class AdmProxy : public webrtc::AudioDeviceModule {
 public:
  AdmProxy(const webrtc::Environment& env,
           webrtc::Thread* worker_thread,
           webrtc::scoped_refptr<RootAudioMixer> mixer);
  // Uses `platform_adm` (already created, not yet initialized) as the Platform
  // ADM, e.g. a fake in tests.
  AdmProxy(const webrtc::Environment& env,
           webrtc::Thread* worker_thread,
           webrtc::scoped_refptr<RootAudioMixer> mixer,
           webrtc::scoped_refptr<webrtc::AudioDeviceModule> platform_adm);
  ~AdmProxy() override;

  // ===========================================================================
  // Platform ADM Lifecycle Management
  // ===========================================================================

  /// Acquires a reference to the Platform ADM. Returns false if no Platform
  /// ADM is available.
  bool AcquirePlatformAdm();

  /// Releases a reference to the Platform ADM. At zero the proxy returns to
  /// synthetic mode and the playout hold and app requests are dropped. False
  /// when there was no reference to release.
  bool ReleasePlatformAdm();

  int platform_adm_ref_count() const;
  bool is_platform_adm_active() const;

  // ===========================================================================
  // Recording/Playout Control
  // ===========================================================================

  void set_recording_enabled(bool enabled);
  bool recording_enabled() const;

  void set_playout_enabled(bool enabled);
  bool playout_enabled() const;

  /// Stores the selection per direction (unset directions keep theirs). A
  /// running direction whose selection changed is restarted on the new device.
  /// `done` runs on the device thread once that is applied.
  void SelectDevices(std::optional<AdmDeviceSelection> recording,
                     std::optional<AdmDeviceSelection> playout,
                     AdmRequestCallback done);

  /// Stops, re-selects, re-inits and restarts the running directions, playout
  /// even while held. Directions that are not running are left alone.
  void RestartDevices(bool recording, bool playout, AdmRequestCallback done);

  /// Keeps platform playout running without any receive stream.
  void SetPlayoutHold(bool hold);
  bool playout_hold() const;

  /// The app's own start/stop (PlatformAudio's start_* / stop_*), kept apart
  /// from libwebrtc's so neither can switch off what the other started.
  void SetAppPlayout(bool start);
  void SetAppRecording(bool start);

  /// The mic test: the monitor plays the processed capture, and while it runs
  /// both directions are wanted (a want of its own, OR-ed in like the hold).
  /// Refused while libwebrtc records for a send stream; a send stream that
  /// starts recording turns it off. Off again with the last reference.
  AdmMonitorResult SetMonitor(bool enabled);

  /// What the newest device track asked of the shared APM (reported in stats).
  void SetDeviceTrackProcessing(bool echo_cancellation, bool noise_suppression);

  /// The capture post-processor's state, whose stream epoch moves right before
  /// every recording start (the DSP chain starts the stream over), and its
  /// mic test monitor. Set once, before any device opens.
  void SetCaptureState(std::shared_ptr<RootCaptureState> capture_state);

  /// Called on the device thread, outside the device mutex, after every
  /// successful platform playout init (the echo model classifies the
  /// device) and stop. Set once, before any device opens.
  void SetPlayoutOpenedSink(AdmPlayoutOpenedSink sink);

  /// Atomics, and the device UIDs under a lock no device or audio thread
  /// waits on; the max gaps are reset on read.
  AdmNativeStats ReadNativeStats();

#if defined(ROOTAPP_HARNESS)
  /// Returns once every device task posted before the call has run.
  void FlushDeviceTasksForTest();
  /// The destructor's shutdown of both ADMs.
  void ShutdownForTest();
  bool SyntheticPlayingForTest() const;
  /// Replaces the UID translation (CoreAudio on macOS, none elsewhere).
  void SetUidTranslatorForTest(AdmUidTranslator translator);
  /// Replaces the device format reader (CoreAudio on macOS, none
  /// elsewhere); without `periodic` formats are read only by
  /// PollDeviceFormatsForTest.
  void SetDeviceFormatReaderForTest(AdmDeviceFormatReader reader,
                                    bool periodic);
  void PollDeviceFormatsForTest();
  /// Replaces the CoreAudio device probe (macOS; none elsewhere). With one,
  /// playout opens before a microphone on its device and the proxy follows
  /// device-list and default-device changes itself.
  void SetDeviceProbeForTest(AdmDeviceProbe probe);
  /// The render-stall guard (on on macOS); without `periodic` only
  /// PollRenderStallForTest runs it.
  void SetRenderStallGuardForTest(bool enabled,
                                  bool periodic,
                                  int stall_ms,
                                  int min_interval_ms);
  void PollRenderStallForTest();
  /// Whether enumeration serializes with device operations (Linux) and how
  /// long it waits for one in progress.
  void SetEnumerationLockForTest(bool serialize, int wait_ms);
  /// Replaces the diagnosis of a failed playout Init and the playout
  /// fallback (WASAPI on Windows, none elsewhere); null turns either off.
  void SetPlayoutFallbackForTest(AdmPlayoutFallbackFactory factory,
                                 AdmPlayoutDiagnoser diagnoser);
  uint64_t RoutedListenersForTest() const;
  uint64_t DeliveredListenerNotificationsForTest() const;
  void RunOnDeviceThreadForTest(absl::AnyInvocable<void() &&> task);
#endif

  // ===========================================================================
  // AudioDeviceModule Interface
  // ===========================================================================

  int32_t ActiveAudioLayer(AudioLayer* audioLayer) const override;
  int32_t RegisterAudioCallback(webrtc::AudioTransport* transport) override;

  // Both ADMs are initialized when created and terminated by the destructor.
  // libwebrtc's Terminate (m150: the voice engine's, with the last
  // PeerConnection) only ends what libwebrtc itself asked for, so a held
  // device keeps running on the tap's silence and the next Init (the next
  // PeerConnection, or a platform reference) finds the proxy working.
  int32_t Init() override;
  int32_t Terminate() override;
  bool Initialized() const override;

  int16_t PlayoutDevices() override;
  int16_t RecordingDevices() override;
  int32_t PlayoutDeviceName(uint16_t index,
                            char name[webrtc::kAdmMaxDeviceNameSize],
                            char guid[webrtc::kAdmMaxGuidSize]) override;
  int32_t RecordingDeviceName(uint16_t index,
                              char name[webrtc::kAdmMaxDeviceNameSize],
                              char guid[webrtc::kAdmMaxGuidSize]) override;

  // libwebrtc's own selection at engine init; the app selects through
  // SelectDevices, so on desktop these are ignored.
  int32_t SetPlayoutDevice(uint16_t index) override;
  int32_t SetPlayoutDevice(WindowsDeviceType device) override;
  int32_t SetRecordingDevice(uint16_t index) override;
  int32_t SetRecordingDevice(WindowsDeviceType device) override;

  int32_t PlayoutIsAvailable(bool* available) override;
  int32_t InitPlayout() override;
  bool PlayoutIsInitialized() const override;
  int32_t RecordingIsAvailable(bool* available) override;
  int32_t InitRecording() override;
  bool RecordingIsInitialized() const override;

  int32_t StartPlayout() override;
  int32_t StopPlayout() override;
  bool Playing() const override;
  int32_t StartRecording() override;
  int32_t StopRecording() override;
  bool Recording() const override;

  int32_t InitSpeaker() override;
  bool SpeakerIsInitialized() const override;
  int32_t InitMicrophone() override;
  bool MicrophoneIsInitialized() const override;

  int32_t SpeakerVolumeIsAvailable(bool* available) override;
  int32_t SetSpeakerVolume(uint32_t volume) override;
  int32_t SpeakerVolume(uint32_t* volume) const override;
  int32_t MaxSpeakerVolume(uint32_t* maxVolume) const override;
  int32_t MinSpeakerVolume(uint32_t* minVolume) const override;

  int32_t MicrophoneVolumeIsAvailable(bool* available) override;
  int32_t SetMicrophoneVolume(uint32_t volume) override;
  int32_t MicrophoneVolume(uint32_t* volume) const override;
  int32_t MaxMicrophoneVolume(uint32_t* maxVolume) const override;
  int32_t MinMicrophoneVolume(uint32_t* minVolume) const override;

  int32_t SpeakerMuteIsAvailable(bool* available) override;
  int32_t SetSpeakerMute(bool enable) override;
  int32_t SpeakerMute(bool* enabled) const override;

  int32_t MicrophoneMuteIsAvailable(bool* available) override;
  int32_t SetMicrophoneMute(bool enable) override;
  int32_t MicrophoneMute(bool* enabled) const override;

  int32_t StereoPlayoutIsAvailable(bool* available) const override;
  int32_t SetStereoPlayout(bool enable) override;
  int32_t StereoPlayout(bool* enabled) const override;
  int32_t StereoRecordingIsAvailable(bool* available) const override;
  int32_t SetStereoRecording(bool enable) override;
  int32_t StereoRecording(bool* enabled) const override;

  int32_t PlayoutDelay(uint16_t* delayMS) const override;

  bool BuiltInAECIsAvailable() const override;
  bool BuiltInAGCIsAvailable() const override;
  bool BuiltInNSIsAvailable() const override;

  int32_t EnableBuiltInAEC(bool enable) override;
  int32_t EnableBuiltInAGC(bool enable) override;
  int32_t EnableBuiltInNS(bool enable) override;

  std::optional<Stats> GetStats() const override;
  bool IsStopOnMuteModeEnabled() const override;

#if defined(WEBRTC_IOS)
  int GetPlayoutAudioParameters(webrtc::AudioParameters* params) const override;
  int GetRecordAudioParameters(webrtc::AudioParameters* params) const override;
#endif

  int32_t SetObserver(webrtc::AudioDeviceObserver* observer) override;

 private:
  struct DesiredState {
    int ref_count = 0;
    bool recording_enabled = false;
    bool playout_enabled = false;
    bool engine_playout = false;
    bool engine_recording = false;
    bool hold = false;
    bool app_playout = false;
    bool app_recording = false;
    bool monitor = false;
    AdmDeviceSelection playout_selection;
    AdmDeviceSelection recording_selection;
    uint64_t playout_selection_gen = 0;
    uint64_t recording_selection_gen = 0;
    uint64_t playout_restart_gen = 0;
    uint64_t recording_restart_gen = 0;
    AdmReopenReason playout_restart_reason = AdmReopenReason::kUserSelect;
    AdmReopenReason recording_restart_reason = AdmReopenReason::kUserSelect;
  };

  // The microphone's IOProc as the macOS ADM chose it when it opened: the
  // one it shares with playout on its device, its own, or unknown (playout
  // was closed, or a device could not be identified).
  enum class MicIoProc : uint8_t { kNone, kShared, kOwn, kUnknown };

  static bool PlayoutWanted(const DesiredState& desired);
  static bool RecordingWanted(const DesiredState& desired);

  webrtc::AudioDeviceModule* platform() const {
    return platform_raw_.load(std::memory_order_acquire);
  }

  // Desktop: queued on the device thread. Mobile: run inline. Tasks take
  // mutex_ themselves; callers must not hold it.
  void PostDeviceTask(absl::AnyInvocable<void() &&> task);
  void RunDeviceTaskAndWait(absl::AnyInvocable<void() &&> task);

  // The *Locked methods run with mutex_ held.
  void RequestReconcile();
  void PostRequest(bool recording, bool playout, AdmRequestCallback done);
  void ReconcileLocked(AdmRequestOutcome* outcome);
  void ReconcilePlayoutLocked(const DesiredState& desired,
                              AdmDirectionOutcome* outcome);
  void ReconcileRecordingLocked(const DesiredState& desired,
                                AdmDirectionOutcome* outcome);
  // Returns false when the stop failed and the direction must wait.
  bool StopDeviceLocked(bool playout, AdmDirectionOutcome* outcome);
  // Playout as it is open: on the platform ADM or on the fallback.
  bool PlayoutPlayingLocked() const;
  bool PlayoutInitializedLocked() const;
  // After the platform ADM's InitPlayout failed: why, and the fallback's open
  // in its place. 0 when the fallback opened.
  int32_t FallBackPlayoutLocked(int32_t result, std::string* detail);
  // The open playout device's delay into the tap.
  void RefreshPlayoutDelayLocked();
  // With a device probe: stops the microphone (and playout on its device)
  // before playout opens where the microphone's IOProc does not suit it, so
  // playout opens first. False when a stop failed (the pass waits for the
  // retry).
  bool StopForSameDeviceLocked(const DesiredState& desired,
                               AdmDirectionOutcome* recording,
                               AdmDirectionOutcome* playout);
  // The device playout would open on now: nullopt when its selection
  // resolves to none, empty when the device cannot be identified.
  std::optional<std::string> PlayoutTargetLocked(const DesiredState& desired);
  bool MicIoProcSuitsLocked(const std::string& playout_device) const;
  bool PointSpeakerAwayLocked();
  bool StaleLocked(bool playout, const DesiredState& desired) const;
  void CountReopenLocked(bool playout, const DesiredState& desired);
  // The selected device went away: until the selection changes, a selection
  // that resolves to no device opens the system default.
  bool FallbackLocked(bool playout, const DesiredState& desired) const;
  void IdentifyOpenedLocked(bool playout);
  void ForgetOpenedLocked(bool playout);
  void PublishDevicesLocked();
  void RequestRestartLocked(bool playout, AdmReopenReason reason);
  void FollowDeviceChangesLocked(bool devices,
                                 bool default_output,
                                 bool default_input);
  void PollRenderStallLocked();
  void ScheduleRenderStallWatchLocked();
  void ResyncMonitorLocked();
  void CompleteOutcomeLocked(const DesiredState& desired,
                             AdmRequestOutcome* outcome);
  void PublishStateLocked(const DesiredState& desired);
  // The device thread, after releasing mutex_: reports a playout open or
  // close.
  void NotifyPlayoutOpened();
  int32_t ApplySelectionLocked(bool playout,
                               const AdmDeviceSelection& selection,
                               bool fallback);
  // With `matched`, also the identity of the device it resolved, read with
  // the resolution (index 0's name for the default).
  int ResolveSelectionLocked(bool playout,
                             const AdmDeviceSelection& selection,
                             AdmPlayoutOpened* matched = nullptr);
  void SchedulePlayoutDelayRefreshLocked();
  void ScheduleRetryLocked();

  struct FormatWatch {
    std::optional<AdmDeviceFormat> opened;
    std::optional<AdmDeviceFormat> pending;
    bool unsettled = false;
  };
  std::optional<AdmDeviceFormat> ReadFormatLocked(bool playout,
                                                  const std::string& guid);
  // After the ADM opened a direction: `before` is the device's format read
  // just before the open.
  void WatchOpenedLocked(bool playout,
                         const std::optional<AdmDeviceFormat>& before);
  void ForgetFormatLocked(bool playout);
  void PollDeviceFormatsLocked();
  bool PollFormatLocked(bool playout);
  void ScheduleFormatWatchLocked();
  void ShutdownLocked();
  void RunListenerNotification(AdmListenerNotification notification);
  DesiredState Desired() const;

  class EnumerationGuard;
  // Takes mutex_ for an enumeration, waiting a bounded time for a device
  // operation in progress; false when it gave up.
  bool LockForEnumeration();

#if defined(__ANDROID__)
  bool EnsurePlatformAdmCreated();
#endif

  const webrtc::Environment env_;
  webrtc::Thread* worker_thread_;
  const webrtc::scoped_refptr<RootAudioMixer> mixer_;
  std::unique_ptr<webrtc::Thread> device_thread_;
  std::shared_ptr<AdmListenerRouter> listener_router_;

  // Held for every device operation (on desktop only the device thread runs
  // those) and for the rarely used settings calls that forward to an ADM.
  // Never taken by a query libwebrtc makes per packet, nor on audio threads.
  mutable webrtc::Mutex mutex_;
  // Desired state; taken briefly by every thread, never across an ADM call.
  mutable webrtc::Mutex state_mutex_;
  DesiredState desired_;
  std::atomic<bool> reconcile_posted_{false};
  // Enumeration and mutex_ (see EnumerationGuard).
  std::atomic<bool> serialize_enumeration_{false};
  std::atomic<int64_t> enumeration_wait_ms_{0};
  std::atomic<int64_t> enumeration_busy_until_ns_{0};

  webrtc::scoped_refptr<SyntheticAudioDevice> synthetic_adm_;
  webrtc::scoped_refptr<webrtc::AudioDeviceModule> platform_adm_;
  std::atomic<webrtc::AudioDeviceModule*> platform_raw_{nullptr};
  AdmTransportTap tap_;
  AdmSyntheticTransport synthetic_transport_{&tap_};

  uint64_t playout_opened_selection_gen_ = 0;
  uint64_t playout_opened_restart_gen_ = 0;
  uint64_t recording_opened_selection_gen_ = 0;
  uint64_t recording_opened_restart_gen_ = 0;
  // Guarded by mutex_.
  std::shared_ptr<RootCaptureState> capture_state_;
  AdmUidTranslator uid_translator_;
  // The playout device the last ApplySelectionLocked resolved, and what the
  // device thread reports to the sink once mutex_ is released.
  AdmPlayoutOpened last_playout_identity_;
  std::optional<AdmPlayoutOpened> playout_opened_;
  AdmPlayoutOpenedSink playout_opened_sink_;
  // Guarded by mutex_. The recording GUID is only resolved with a reader.
  AdmDeviceFormatReader format_reader_;
  bool format_watch_periodic_ = false;
  std::string last_recording_guid_;
  FormatWatch playout_watch_;
  FormatWatch recording_watch_;
  int64_t last_format_reopen_ns_ = 0;
  std::atomic<uint32_t> playout_device_rate_{0};
  std::atomic<uint32_t> playout_device_channels_{0};
  std::atomic<uint32_t> recording_device_rate_{0};
  std::atomic<uint32_t> recording_device_channels_{0};
  std::atomic<uint64_t> playout_format_changes_{0};
  std::atomic<uint64_t> playout_format_reopens_{0};
  std::atomic<uint64_t> recording_format_changes_{0};
  std::atomic<bool> device_formats_available_{false};
  // Guarded by mutex_: the device probe, the devices the open directions
  // opened on, whether they follow the system default, the microphone's
  // IOProc and the selection generations whose device went away.
  AdmDeviceProbe probe_;
  AdmDeviceIdentity playout_device_;
  AdmDeviceIdentity recording_device_;
  bool playout_follows_default_ = false;
  bool recording_follows_default_ = false;
  bool last_recording_default_ = false;
  MicIoProc mic_ioproc_ = MicIoProc::kNone;
  std::optional<uint64_t> playout_fallback_gen_;
  std::optional<uint64_t> recording_fallback_gen_;
  bool force_coupled_ = false;
  struct RenderStallWatch {
    uint64_t render = 0;
    uint64_t capture = 0;
    uint64_t last_capture = 0;
    int64_t since_ns = 0;
  };
  RenderStallWatch render_stall_;
  int64_t last_render_stall_recovery_ns_ = 0;
  bool render_stall_guard_ = false;
  bool render_stall_periodic_ = false;
  int64_t render_stall_ns_ = 0;
  int64_t render_stall_interval_ns_ = 0;
  mutable webrtc::Mutex devices_mutex_;
  std::string playout_uid_;
  std::string recording_uid_;
  std::atomic<bool> same_device_{false};
  std::atomic<bool> device_changes_available_{false};
  std::atomic<uint64_t> device_list_notifications_{0};
  std::atomic<uint64_t> default_output_notifications_{0};
  std::atomic<uint64_t> default_input_notifications_{0};
  std::atomic<uint64_t> stream_format_notifications_{0};
  std::atomic<uint64_t> other_notifications_{0};
  std::array<std::atomic<uint64_t>, kAdmReopenReasons> reopens_{};
  std::atomic<uint64_t> coupled_reopens_{0};
  std::atomic<uint64_t> render_stall_recoveries_{0};
  // Guarded by mutex_: the diagnosis, the fallback's factory and the open
  // fallback (playout runs on it while set); the last Init's diagnosis is
  // under devices_mutex_ for the stats.
  AdmPlayoutDiagnoser playout_diagnoser_;
  AdmPlayoutFallbackFactory playout_fallback_factory_;
  std::unique_ptr<AdmPlayoutFallback> playout_fallback_;
  std::string playout_init_detail_;
  std::atomic<bool> playout_fallback_available_{false};
  std::atomic<bool> playout_fallback_active_{false};
  std::atomic<uint64_t> playout_fallback_opens_{0};
  // capture_state_'s monitor, set with it.
  std::atomic<RootAudioMonitor*> monitor_{nullptr};
#if !LK_ADM_MOBILE
  bool delay_refresh_scheduled_ = false;
  bool retry_scheduled_ = false;
  bool format_watch_scheduled_ = false;
  bool render_stall_scheduled_ = false;
#endif
  bool terminated_ = false;

  std::atomic<bool> engine_playout_{false};
  std::atomic<bool> engine_recording_{false};
  std::atomic<bool> platform_active_{false};
  std::atomic<bool> platform_playout_mode_{false};
  std::atomic<bool> device_playing_{false};
  std::atomic<bool> device_playout_initialized_{false};
  std::atomic<bool> device_recording_{false};
  std::atomic<bool> device_recording_initialized_{false};
  std::atomic<bool> platform_playing_{false};
  std::atomic<bool> platform_recording_{false};
  std::atomic<int32_t> playout_init_result_{kAdmResultNotAttempted};
  std::atomic<int32_t> playout_start_result_{kAdmResultNotAttempted};
  std::atomic<int32_t> recording_init_result_{kAdmResultNotAttempted};
  std::atomic<int32_t> recording_start_result_{kAdmResultNotAttempted};
  std::atomic<uint64_t> device_stop_failures_{0};
  std::atomic<bool> device_track_echo_cancellation_{false};
  std::atomic<bool> device_track_noise_suppression_{false};
  // Desktop: libwebrtc's SetStereoPlayout, applied before every playout open.
  std::atomic<bool> stereo_playout_{true};
};

}  // namespace livekit_ffi
