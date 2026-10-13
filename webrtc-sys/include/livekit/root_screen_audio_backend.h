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

#include <cstddef>
#include <cstdint>
#include <memory>
#include <string>
#include <vector>

// The per-OS half of native screen-share audio. RootScreenAudioCapture (the
// shared core, root_screen_audio.h) owns one backend, starts and stops it on
// its control thread and paces what it writes into the screen-audio track's
// AudioSource at 10 ms. A backend only opens the OS capture, resolves the
// target and writes float32 frames into the sink from its own audio thread.
//
// Exactly one source file per target OS defines RootScreenAudioBackendBuilt()
// and CreateRootScreenAudioBackend() (webrtc-sys/build.rs picks it):
//   Windows  src/screen_audio/root_screen_audio_win.cpp    WASAPI process loopback
//   macOS    src/screen_audio/root_screen_audio_mac.mm     ScreenCaptureKit audio
//   Linux    src/screen_audio/root_screen_audio_linux.cpp  PipeWire capture stream
//   others   src/screen_audio/root_screen_audio_none.cpp   no backend

namespace livekit_ffi {

// What the host asked to capture (RootScreenAudioTarget in rootapp_audio.proto).
enum class ScreenAudioTargetKind : int32_t {
  // Everything the machine plays except this process and its children (the
  // call's own playout, so remote voices are never sent back).
  kSystem = 0,
  // Only process_id and its children (Windows include mode). macOS: the
  // application with that pid. Where unsupported: kSystem.
  kProcess = 1,
  // Only the application owning window_id (HWND on Windows, CGWindowID on
  // macOS), window_title as a second lookup. A window or owner that cannot be
  // found, or a platform without window scoping (Linux): kSystem (the core
  // accepts that on a first start only).
  kWindow = 2,
};

struct ScreenAudioTarget {
  ScreenAudioTargetKind kind = ScreenAudioTargetKind::kSystem;
  uint32_t process_id = 0;
  uint64_t window_id = 0;
  std::string window_title;
  // The core restarts the same capture after a failure (false for a start
  // the host asked for).
  bool restart = false;
};

// What a started backend captures once it resolved the target.
enum class ScreenAudioScope : int32_t {
  kSystem = 0,       // everything except this process tree
  kProcess = 1,      // one process tree
  kApplication = 2,  // one application
};

// RootScreenAudioError in rootapp_audio.proto, same values.
enum class ScreenAudioError : int32_t {
  kNone = 0,
  // No backend in this build, OS too old, API missing.
  kUnsupported = 1,
  // macOS Screen Recording (TCC) not granted; Linux PipeWire access refused.
  kPermissionDenied = 2,
  // The OS refused to open the capture (the detail says why).
  kStartFailed = 3,
  // The capture endpoint went away (device invalidated, coreaudiod or the
  // PipeWire daemon restarted). The core restarts the backend.
  kDeviceLost = 4,
  // Any other failure of a running stream. The core restarts the backend.
  kStreamFailed = 5,
  // The captured process or application exited. No restart.
  kTargetGone = 6,
  // A start that did not finish within its bound.
  kTimeout = 7,
  kInternal = 8,
};

constexpr double kScreenAudioIdleGapSeconds = 0.5;

// What Write delivers: interleaved float32, full scale ±1 (louder samples are
// clipped later and counted). The core converts any rate to the source's
// 48 kHz and downmixes to the source's channels (first two channels; mono is
// copied to both).
struct ScreenAudioFormat {
  int sample_rate = 48000;  // 8000..192000
  int channels = 2;         // 1..8
};

// RootScreenAudioLimitation in rootapp_audio.proto, same values.
enum class ScreenAudioLimitation : int32_t {
  kNone = 0,
  // The target is an ancestor of this process (Windows): nothing is captured.
  kTargetIsRootAncestor = 1,
};

struct ScreenAudioStartResult {
  ScreenAudioError error = ScreenAudioError::kNone;
  // The error, or on success how the target resolved, for the host's log
  // (e.g. "process 1234 (chrome.exe) and its children", "application
  // 'Safari'", "all audio except process 5678 and its children").
  std::string detail;
  ScreenAudioFormat format;
  ScreenAudioScope scope = ScreenAudioScope::kSystem;
  // The captured process (kProcess) or application's process (kApplication);
  // 0 for kSystem. A restart that resolves another scope or process than the
  // first start ends the capture with kTargetGone: it never widens.
  uint32_t process_id = 0;
  // A success that knowingly captures less than the target.
  ScreenAudioLimitation limitation = ScreenAudioLimitation::kNone;
};

// The core's side of a running backend.
//
// Write, WriteSilence and MarkDiscontinuity: only from the backend's audio
// thread (or serial queue), never two at once. They are wait-free: atomics and
// copies into preallocated memory, no locks, allocation, logging or syscalls.
// A backend's own code on that thread follows the same rules (preallocate at
// Start; no logging; no Objective-C or CoreFoundation allocation beyond what
// the OS callback itself does).
//
// ReportError: any thread, the audio thread included. It never blocks (it may
// make one syscall to wake the control thread). The first report after a
// Start counts; the core then calls Stop on its control thread and decides on
// a restart. A backend reports at most once per failure and keeps its audio
// thread quiet afterwards.
class ScreenAudioSink {
 public:
  virtual void Write(const float* interleaved, size_t frames) = 0;
  // Frames the OS marked silent (WASAPI AUDCLNT_BUFFERFLAGS_SILENT) without a
  // buffer to copy.
  virtual void WriteSilence(size_t frames) = 0;
  // The OS reported audio lost before the next Write (WASAPI
  // DATA_DISCONTINUITY, a PipeWire xrun). Counted only: the ring level
  // already shows what is missing. Backends that infer a loss from timing
  // (PipeWire, ScreenCaptureKit) take a gap of kScreenAudioIdleGapSeconds or
  // more for idle time (nothing was playing) and do not report it.
  virtual void MarkDiscontinuity() = 0;
  virtual void ReportError(ScreenAudioError error, const char* detail) = 0;

 protected:
  ~ScreenAudioSink() = default;
};

// One OS capture. Start and Stop are called only on the core's control thread,
// alternately (Start, Stop, Start, ...; Stop also without a successful Start
// and twice in a row). The control thread has no COM apartment, run loop or
// main-thread affinity; a backend sets up what it needs on its own threads.
class ScreenAudioBackend {
 public:
  virtual ~ScreenAudioBackend() = default;

  // Resolves the target, opens the OS capture and starts writing to `sink`
  // from the backend's audio thread. Blocks until audio can flow or the start
  // failed, at most about 5 s (then kTimeout). On failure the backend has
  // released everything and calls no sink method afterwards. The sink stays
  // valid until Stop returns.
  virtual ScreenAudioStartResult Start(const ScreenAudioTarget& target,
                                       ScreenAudioSink* sink) = 0;

  // Stops the OS capture and joins the audio thread: once it returns true no
  // sink method runs or will run. Bounded at about 2 s; a backend that could
  // not stop its thread in time returns false and must leave everything that
  // thread uses alive (the core then never destroys the backend or the sink).
  // Idempotent.
  virtual bool Stop() = 0;

  // After a Stop that returned true: false when the OS did not confirm within
  // the budget that its capture stopped (nothing reaches the sink any more,
  // but the OS capture may still be stopping). The core reports it.
  virtual bool StopConfirmed() const { return true; }
};

// A process as a snapshot lists it.
struct ScreenAudioProcessEntry {
  uint32_t pid = 0;
  uint32_t parent = 0;
  // Creation time in any monotonic unit; 0 when unknown.
  uint64_t created = 0;
};

// The ancestors of `self`, its parent first. The walk stops at a parent that
// is missing, already seen, or created after its child (its pid was reused by
// a newer process).
std::vector<uint32_t> ScreenAudioAncestors(
    uint32_t self,
    const std::vector<ScreenAudioProcessEntry>& processes);

// Defined by the one backend file of the target OS.
// True when this build has a backend for the target OS (compile time; the OS
// version and permissions are checked by Start).
bool RootScreenAudioBackendBuilt();
// A new backend, or nullptr when RootScreenAudioBackendBuilt() is false.
std::unique_ptr<ScreenAudioBackend> CreateRootScreenAudioBackend();

}  // namespace livekit_ffi
