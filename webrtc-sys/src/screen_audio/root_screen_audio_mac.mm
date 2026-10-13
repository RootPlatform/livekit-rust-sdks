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

// macOS screen audio: an audio-only ScreenCaptureKit stream (macOS 13+), as the
// desktop's managed SckLoopbackCaptureService. A window or process target
// captures only its application; everything else captures all audio except this
// process tree (excludingApplications plus excludesCurrentProcessAudio). Needs
// Screen Recording, which the video share asks for: this file never prompts.
// Every ScreenCaptureKit symbol it references is newer than the deployment
// target, so the references are weak and the library still loads before 12.3.
// Manual reference counting, as the other .mm files.

#import <CoreGraphics/CoreGraphics.h>
#import <CoreMedia/CoreMedia.h>
#import <Foundation/Foundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>

#include <dispatch/dispatch.h>
#include <errno.h>
#include <libproc.h>
#include <signal.h>
#include <unistd.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <memory>
#include <string>
#include <thread>
#include <unordered_set>
#include <vector>

#include "livekit/root_screen_audio_backend.h"
#include "rtc_base/logging.h"

namespace livekit_ffi {
namespace {

using Clock = std::chrono::steady_clock;

constexpr int kSampleRate = 48000;
constexpr int kChannels = 2;
constexpr auto kStartBudget = std::chrono::seconds(5);
constexpr auto kStopBudget = std::chrono::seconds(2);
constexpr UInt32 kMaxBuffers = 8;
constexpr size_t kChunkFrames = 4096;
constexpr size_t kMaxProcessTree = 1024;
// A buffer that starts this far from where the previous one ended follows
// audio ScreenCaptureKit dropped.
constexpr double kDiscontinuitySeconds = 0.002;

// SCStreamErrorCode (SCError.h), in SCStreamErrorDomain.
constexpr NSInteger kErrorUserDeclined = -3801;
constexpr NSInteger kErrorNoCaptureSource = -3815;
constexpr NSInteger kErrorUserStopped = -3817;

// Completion::state: whether the completion ran or its waiter gave up first.
constexpr int kCompletionPending = 0;
constexpr int kCompletionDone = 1;
constexpr int kCompletionAbandoned = 2;

// The rate ScreenCaptureKit delivered when it was not the one a start reported
// (0: as asked). The restart that follows the mismatch reports it, so it
// converts instead of failing again; a start the host asked for forgets it.
std::atomic<int> g_delivered_rate{0};

int StartRate() {
  int rate = g_delivered_rate.load(std::memory_order_relaxed);
  return rate > 0 ? rate : kSampleRate;
}

dispatch_time_t Until(Clock::time_point deadline) {
  int64_t left =
      std::chrono::duration_cast<std::chrono::nanoseconds>(deadline - Clock::now()).count();
  return dispatch_time(DISPATCH_TIME_NOW, std::max<int64_t>(left, 0));
}

std::string Utf8(NSString* text) {
  const char* bytes = text ? [text UTF8String] : nullptr;
  return bytes ? std::string(bytes) : std::string();
}

std::string Describe(NSError* error) {
  if (!error)
    return "no error";
  return Utf8(error.domain) + " " + std::to_string(static_cast<long>(error.code)) + ": " +
         Utf8(error.localizedDescription);
}

API_AVAILABLE(macos(13.0))
bool IsSckError(NSError* error, NSInteger code) {
  return error != nil && error.code == code && error.domain != nil &&
         [error.domain isEqualToString:SCStreamErrorDomain];
}

API_AVAILABLE(macos(13.0))
ScreenAudioError StartError(NSError* error) {
  return IsSckError(error, kErrorUserDeclined) ? ScreenAudioError::kPermissionDenied
                                               : ScreenAudioError::kStartFailed;
}

// One completion handler's result, shared with the handler: a completion that
// arrives after its wait timed out writes into memory nobody reads any more.
struct Completion {
  Completion() : done(dispatch_semaphore_create(0)) {}
  ~Completion() {
    [content release];
    [error release];
    dispatch_release(done);
  }
  Completion(const Completion&) = delete;
  Completion& operator=(const Completion&) = delete;

  bool Wait(Clock::time_point deadline) {
    return dispatch_semaphore_wait(done, Until(deadline)) == 0;
  }

  dispatch_semaphore_t done;
  id content = nil;
  NSError* error = nil;
  std::atomic<int> state{kCompletionPending};
};

// Waits (bounded) for the blocks already queued on `queue`.
void Drain(dispatch_queue_t queue, Clock::time_point deadline) {
  if (!queue)
    return;
  dispatch_semaphore_t done = dispatch_semaphore_create(0);
  dispatch_retain(done);
  dispatch_async(queue, ^{
    dispatch_semaphore_signal(done);
    dispatch_release(done);
  });
  dispatch_semaphore_wait(done, Until(deadline));
  dispatch_release(done);
}

template <typename T>
void ReleaseDispatch(T* object) {
  if (*object) {
    dispatch_release(*object);
    *object = nullptr;
  }
}

void CancelSource(dispatch_source_t* source) {
  if (*source) {
    dispatch_source_cancel(*source);
    ReleaseDispatch(source);
  }
}

// This process and its descendants (the desktop's SckSupport.OwnProcessTree).
std::unordered_set<pid_t> OwnProcessTree() {
  const pid_t self = getpid();
  std::unordered_set<pid_t> tree{self};
  std::vector<pid_t> pending{self};
  std::vector<pid_t> children(256);
  while (!pending.empty() && tree.size() < kMaxProcessTree) {
    pid_t parent = pending.back();
    pending.pop_back();
    int bytes = proc_listpids(PROC_PPID_ONLY, static_cast<uint32_t>(parent), children.data(),
                              static_cast<int>(children.size() * sizeof(pid_t)));
    size_t count =
        bytes > 0 ? std::min(children.size(), static_cast<size_t>(bytes) / sizeof(pid_t)) : 0;
    for (size_t i = 0; i < count; ++i) {
      if (children[i] > 0 && tree.insert(children[i]).second)
        pending.push_back(children[i]);
    }
  }
  return tree;
}

// What the ScreenCaptureKit callbacks touch. Owned by the stream's output
// object, which the stream, the dispatch sources and the backend keep alive;
// the gate keeps every sink call between Start and Stop.
class SckAudioTap {
 public:
  // `watched`: the captured application's pid (0 for everything but this
  // process tree).
  SckAudioTap(ScreenAudioSink* sink, int sample_rate, pid_t watched)
      : sink_(sink),
        sample_rate_(sample_rate),
        watched_(watched),
        interleaved_(kChunkFrames * kChannels, 0.0f) {}
  SckAudioTap(const SckAudioTap&) = delete;
  SckAudioTap& operator=(const SckAudioTap&) = delete;

  // The audio queue. Wait-free: no allocation, lock or log.
  void Audio(CMSampleBufferRef buffer) {
    if (!Enter())
      return;
    if (!reported_.load(std::memory_order_relaxed))
      Read(buffer);
    Leave();
  }

  // ScreenCaptureKit's delegate queue.
  API_AVAILABLE(macos(13.0)) void Stopped(NSError* error) {
    if (!Enter())
      return;
    ScreenAudioError kind = ScreenAudioError::kStreamFailed;
    std::string detail = "ScreenCaptureKit stopped the audio stream (" + Describe(error) + ")";
    if (IsSckError(error, kErrorUserDeclined)) {
      kind = ScreenAudioError::kPermissionDenied;
      detail = "Screen Recording permission was revoked (" + Describe(error) + ")";
    } else if (IsSckError(error, kErrorUserStopped)) {
      kind = ScreenAudioError::kPermissionDenied;
      detail = "screen capture was stopped from the macOS menu bar (" + Describe(error) + ")";
    } else if (watched_ > 0 && kill(watched_, 0) != 0 && errno == ESRCH) {
      // Gone, not failed: a restart would widen the share to all audio.
      kind = ScreenAudioError::kTargetGone;
      detail = "the captured application exited (" + Describe(error) + ")";
    } else if (IsSckError(error, kErrorNoCaptureSource)) {
      kind = ScreenAudioError::kDeviceLost;
      detail = "the captured display went away (" + Describe(error) + ")";
    }
    Report(kind, detail.c_str());
    Leave();
  }

  // The frame queue (a DISPATCH_PROC_EXIT source).
  void TargetExited() {
    if (!Enter())
      return;
    Report(ScreenAudioError::kTargetGone, "the captured application exited");
    Leave();
  }

  // Never the audio queue: what the audio queue only noted.
  void LogWarnings() {
    char text[256];
    if (format_pending_.exchange(false, std::memory_order_acquire)) {
      const AudioStreamBasicDescription& f = noted_format_;
      std::snprintf(text, sizeof(text),
                    "RootScreenAudio: unexpected ScreenCaptureKit audio format (id 0x%08X, "
                    "flags 0x%X, %u bits, %.0f Hz, %u channels); %s",
                    static_cast<unsigned>(f.mFormatID), static_cast<unsigned>(f.mFormatFlags),
                    static_cast<unsigned>(f.mBitsPerChannel), f.mSampleRate,
                    static_cast<unsigned>(f.mChannelsPerFrame),
                    noted_restart_ ? "restarting at that rate" : "the audio is dropped");
      RTC_LOG(LS_WARNING) << text;
    }
    if (list_pending_.exchange(false, std::memory_order_acquire)) {
      std::snprintf(text, sizeof(text),
                    "RootScreenAudio: reading a ScreenCaptureKit audio buffer list failed "
                    "(OSStatus %d, needs %zu bytes, room for %zu); the audio is dropped",
                    static_cast<int>(noted_status_), noted_needed_, sizeof(list_storage_));
      RTC_LOG(LS_WARNING) << text;
    }
  }

  // Control thread: no sink call starts after this. False when one is still
  // running at the deadline (the caller then keeps everything alive).
  bool Close(Clock::time_point deadline) {
    closed_.store(true);
    while (in_flight_.load() != 0) {
      if (Clock::now() >= deadline)
        return false;
      std::this_thread::yield();
    }
    return true;
  }

 private:
  bool Enter() {
    in_flight_.fetch_add(1);
    if (!closed_.load())
      return true;
    in_flight_.fetch_sub(1);
    return false;
  }

  void Leave() { in_flight_.fetch_sub(1); }

  // Inside Enter/Leave. Once per start; the audio queue stays quiet after.
  void Report(ScreenAudioError error, const char* detail) {
    if (!reported_.exchange(true))
      sink_->ReportError(error, detail);
  }

  void Read(CMSampleBufferRef buffer) {
    if (!buffer || !CMSampleBufferIsValid(buffer) || !CMSampleBufferDataIsReady(buffer))
      return;
    CMFormatDescriptionRef description = CMSampleBufferGetFormatDescription(buffer);
    const AudioStreamBasicDescription* format =
        description ? CMAudioFormatDescriptionGetStreamBasicDescription(description) : nullptr;
    if (!format)
      return;
    if (format->mFormatID != kAudioFormatLinearPCM ||
        (format->mFormatFlags & kAudioFormatFlagIsFloat) == 0 || format->mBitsPerChannel != 32 ||
        !(format->mSampleRate >= 8000 && format->mSampleRate <= 192000)) {
      NoteFormat(*format, false);
      return;
    }
    int rate = static_cast<int>(format->mSampleRate + 0.5);
    if (rate != sample_rate_) {
      if (rate % 100 != 0) {
        NoteFormat(*format, false);
        Report(ScreenAudioError::kUnsupported,
               "ScreenCaptureKit delivers a sample rate that is not a multiple of 100 Hz, which "
               "cannot be converted");
        return;
      }
      NoteFormat(*format, true);
      g_delivered_rate.store(rate, std::memory_order_relaxed);
      Report(ScreenAudioError::kStreamFailed,
             "ScreenCaptureKit delivers another sample rate than the stream reported; restarting "
             "at that rate");
      return;
    }
    NoteTimestamp(buffer, format->mSampleRate);

    size_t needed = 0;
    OSStatus status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        buffer, &needed, nullptr, 0, nullptr, nullptr, 0, nullptr);
    if (status != noErr || needed == 0 || needed > sizeof(list_storage_)) {
      NoteBufferList(status, needed);
      return;
    }
    AudioBufferList* list = reinterpret_cast<AudioBufferList*>(list_storage_);
    CMBlockBufferRef block = nullptr;
    status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        buffer, nullptr, list, needed, nullptr, nullptr, 0, &block);
    if (status == noErr)
      Deliver(*list, *format);
    else
      NoteBufferList(status, needed);
    if (block)
      CFRelease(block);
  }

  // Stereo out whatever the layout: planar takes the first two buffers (mono
  // copied to both), interleaved the first two channels.
  void Deliver(const AudioBufferList& list, const AudioStreamBasicDescription& format) {
    const UInt32 count = std::min(list.mNumberBuffers, kMaxBuffers);
    const AudioBuffer* buffers = list.mBuffers;
    if (count == 0 || buffers[0].mData == nullptr)
      return;
    float* out = interleaved_.data();
    if ((format.mFormatFlags & kAudioFormatFlagIsNonInterleaved) != 0) {
      const AudioBuffer& first = buffers[0];
      const AudioBuffer& second = count > 1 && buffers[1].mData ? buffers[1] : buffers[0];
      const float* left = static_cast<const float*>(first.mData);
      const float* right = static_cast<const float*>(second.mData);
      const size_t frames = std::min(first.mDataByteSize, second.mDataByteSize) / sizeof(float);
      for (size_t done = 0; done < frames;) {
        const size_t n = std::min(frames - done, kChunkFrames);
        for (size_t i = 0; i < n; ++i) {
          out[2 * i] = left[done + i];
          out[2 * i + 1] = right[done + i];
        }
        sink_->Write(out, n);
        done += n;
      }
      return;
    }
    const size_t channels =
        buffers[0].mNumberChannels != 0 ? buffers[0].mNumberChannels : format.mChannelsPerFrame;
    if (channels == 0)
      return;
    const float* samples = static_cast<const float*>(buffers[0].mData);
    const size_t frames = buffers[0].mDataByteSize / sizeof(float) / channels;
    if (frames == 0)
      return;
    if (channels == static_cast<size_t>(kChannels)) {
      sink_->Write(samples, frames);
      return;
    }
    const size_t right = channels > 1 ? 1 : 0;
    for (size_t done = 0; done < frames;) {
      const size_t n = std::min(frames - done, kChunkFrames);
      for (size_t i = 0; i < n; ++i) {
        const float* frame = samples + (done + i) * channels;
        out[2 * i] = frame[0];
        out[2 * i + 1] = frame[right];
      }
      sink_->Write(out, n);
      done += n;
    }
  }

  // A buffer that does not start where the previous one ended follows audio
  // ScreenCaptureKit dropped (its handler queue backed up). A forward gap of
  // kScreenAudioIdleGapSeconds or more is delivery paused while nothing played.
  void NoteTimestamp(CMSampleBufferRef buffer, double rate) {
    const CMTime start = CMSampleBufferGetPresentationTimeStamp(buffer);
    const CMItemCount frames = CMSampleBufferGetNumSamples(buffer);
    if ((start.flags & kCMTimeFlags_Valid) == 0 || frames <= 0) {
      have_expected_ = false;
      return;
    }
    const double seconds = CMTimeGetSeconds(start);
    const double gap = seconds - expected_seconds_;
    if (have_expected_ && std::fabs(gap) > kDiscontinuitySeconds &&
        gap < kScreenAudioIdleGapSeconds)
      sink_->MarkDiscontinuity();
    expected_seconds_ = seconds + static_cast<double>(frames) / rate;
    have_expected_ = true;
  }

  void NoteFormat(const AudioStreamBasicDescription& format, bool restarting) {
    if (format_noted_.exchange(true, std::memory_order_relaxed))
      return;
    noted_format_ = format;
    noted_restart_ = restarting;
    format_pending_.store(true, std::memory_order_release);
  }

  void NoteBufferList(OSStatus status, size_t needed) {
    if (list_noted_.exchange(true, std::memory_order_relaxed))
      return;
    noted_status_ = status;
    noted_needed_ = needed;
    list_pending_.store(true, std::memory_order_release);
  }

  ScreenAudioSink* const sink_;
  const int sample_rate_;
  const pid_t watched_;
  std::vector<float> interleaved_;
  alignas(AudioBufferList) unsigned char
      list_storage_[sizeof(AudioBufferList) + (kMaxBuffers - 1) * sizeof(AudioBuffer)];

  std::atomic<bool> closed_{false};
  std::atomic<int32_t> in_flight_{0};
  std::atomic<bool> reported_{false};

  // The audio queue only.
  bool have_expected_ = false;
  double expected_seconds_ = 0;

  // Written once by the audio queue, read after the pending flag.
  std::atomic<bool> format_noted_{false};
  std::atomic<bool> format_pending_{false};
  AudioStreamBasicDescription noted_format_{};
  bool noted_restart_ = false;
  std::atomic<bool> list_noted_{false};
  std::atomic<bool> list_pending_{false};
  OSStatus noted_status_ = 0;
  size_t noted_needed_ = 0;
};

}  // namespace
}  // namespace livekit_ffi

API_AVAILABLE(macos(13.0))
@interface LKRootScreenAudioOutput : NSObject <SCStreamOutput, SCStreamDelegate> {
 @private
  livekit_ffi::SckAudioTap* _tap;
}
- (instancetype)initWithTap:(livekit_ffi::SckAudioTap*)tap;
- (livekit_ffi::SckAudioTap*)tap;
- (void)logWarnings;
- (void)targetExited;
@end

@implementation LKRootScreenAudioOutput

- (instancetype)initWithTap:(livekit_ffi::SckAudioTap*)tap {
  self = [super init];
  if (self)
    _tap = tap;
  else
    delete tap;
  return self;
}

- (void)dealloc {
  delete _tap;
  [super dealloc];
}

- (livekit_ffi::SckAudioTap*)tap {
  return _tap;
}

- (void)stream:(SCStream*)stream
    didOutputSampleBuffer:(CMSampleBufferRef)sampleBuffer
                   ofType:(SCStreamOutputType)type {
  (void)stream;
  if (type == SCStreamOutputTypeAudio)
    _tap->Audio(sampleBuffer);
}

- (void)stream:(SCStream*)stream didStopWithError:(NSError*)error {
  (void)stream;
  @autoreleasepool {
    _tap->Stopped(error);
  }
}

- (void)logWarnings {
  _tap->LogWarnings();
}

- (void)targetExited {
  _tap->TargetExited();
}

@end

namespace livekit_ffi {
namespace {

API_AVAILABLE(macos(13.0))
SCDisplay* MainDisplay(SCShareableContent* content) {
  const CGDirectDisplayID main = CGMainDisplayID();
  for (SCDisplay* display in content.displays) {
    if (display.displayID == main)
      return display;
  }
  return content.displays.firstObject;
}

// By CGWindowID, then exact title, then case-insensitive title (managed parity).
API_AVAILABLE(macos(13.0))
SCWindow* SharedWindow(SCShareableContent* content, const ScreenAudioTarget& target) {
  NSArray<SCWindow*>* windows = content.windows;
  if (target.window_id != 0) {
    for (SCWindow* window in windows) {
      if (static_cast<uint64_t>(window.windowID) == target.window_id)
        return window;
    }
  }
  if (target.window_title.empty())
    return nil;
  NSString* title = [NSString stringWithUTF8String:target.window_title.c_str()];
  if (!title)
    return nil;
  for (SCWindow* window in windows) {
    if (window.title && [window.title isEqualToString:title])
      return window;
  }
  for (SCWindow* window in windows) {
    if (window.title && [window.title caseInsensitiveCompare:title] == NSOrderedSame)
      return window;
  }
  return nil;
}

API_AVAILABLE(macos(13.0))
std::string Name(SCRunningApplication* application) {
  std::string name = Utf8(application.applicationName);
  return name.empty() ? Utf8(application.bundleIdentifier) : name;
}

// The application a window or process target captures, or nil (then
// `missing` says why) for everything but this process tree.
API_AVAILABLE(macos(13.0))
SCRunningApplication* TargetApplication(SCShareableContent* content,
                                        const ScreenAudioTarget& target,
                                        const std::unordered_set<pid_t>& tree,
                                        std::string* missing) {
  SCRunningApplication* application = nil;
  if (target.kind == ScreenAudioTargetKind::kWindow) {
    SCWindow* window = SharedWindow(content, target);
    if (window) {
      application = window.owningApplication;
      if (!application)
        *missing = "window " + std::to_string(window.windowID) + " has no owning application";
    } else {
      *missing = "window " +
                 (target.window_id != 0 ? std::to_string(target.window_id) + " " : std::string()) +
                 (target.window_title.empty() ? std::string() : "'" + target.window_title + "' ") +
                 "not found";
    }
  } else if (target.kind == ScreenAudioTargetKind::kProcess) {
    for (SCRunningApplication* candidate in content.applications) {
      if (candidate.processID == static_cast<pid_t>(target.process_id)) {
        application = candidate;
        break;
      }
    }
    if (!application)
      *missing = "process " + std::to_string(target.process_id) + " is not an application";
  }
  if (application && tree.count(application.processID) != 0) {
    application = nil;
    *missing = "the target is this process";
  }
  return application;
}

class SckScreenAudioBackend final : public ScreenAudioBackend {
 public:
  ~SckScreenAudioBackend() override { Stop(); }

  ScreenAudioStartResult Start(const ScreenAudioTarget& target, ScreenAudioSink* sink) override {
    if (!Stop()) {
      ScreenAudioStartResult result;
      result.error = ScreenAudioError::kInternal;
      result.detail = "an earlier ScreenCaptureKit audio stream could not be stopped";
      return result;
    }
    if (@available(macOS 13.0, *)) {
      @autoreleasepool {
        return StartStream(target, sink);
      }
    }
    ScreenAudioStartResult result;
    result.error = ScreenAudioError::kUnsupported;
    result.detail = "ScreenCaptureKit audio needs macOS 13 or later";
    return result;
  }

  bool Stop() override {
    if (leaked_)
      return false;
    if (@available(macOS 13.0, *)) {
      @autoreleasepool {
        return StopStream();
      }
    }
    return true;
  }

  bool StopConfirmed() const override { return stop_confirmed_; }

 private:
  API_AVAILABLE(macos(13.0))
  ScreenAudioStartResult Fail(ScreenAudioError error, std::string detail) {
    StopStream();
    ScreenAudioStartResult result;
    result.error = error;
    result.detail = std::move(detail);
    return result;
  }

  API_AVAILABLE(macos(13.0))
  ScreenAudioStartResult StartStream(const ScreenAudioTarget& target, ScreenAudioSink* sink) {
    const Clock::time_point deadline = Clock::now() + kStartBudget;
    if (!CGPreflightScreenCaptureAccess())
      return Fail(ScreenAudioError::kPermissionDenied,
                  "Screen Recording permission is not granted (CGPreflightScreenCaptureAccess)");

    auto listed = std::make_shared<Completion>();
    [SCShareableContent
        getShareableContentExcludingDesktopWindows:YES
                               onScreenWindowsOnly:NO
                                 completionHandler:^(SCShareableContent* content,
                                                     NSError* failure) {
                                   listed->content = [content retain];
                                   listed->error = [failure retain];
                                   dispatch_semaphore_signal(listed->done);
                                 }];
    if (!listed->Wait(deadline))
      return Fail(ScreenAudioError::kTimeout,
                  "ScreenCaptureKit did not list the shareable content within 5 s");
    SCShareableContent* content = listed->content;
    if (!content)
      return Fail(StartError(listed->error),
                  "listing the shareable content failed (" + Describe(listed->error) + ")");

    SCDisplay* display = MainDisplay(content);
    if (!display)
      return Fail(ScreenAudioError::kStartFailed, "ScreenCaptureKit lists no display");

    ScreenAudioStartResult result;
    const std::unordered_set<pid_t> tree = OwnProcessTree();
    std::string missing;
    SCRunningApplication* application = TargetApplication(content, target, tree, &missing);
    NSArray* no_windows = [NSArray array];
    SCContentFilter* filter = nil;
    pid_t watched = 0;
    if (application) {
      filter = [[SCContentFilter alloc] initWithDisplay:display
                                  includingApplications:[NSArray arrayWithObject:application]
                                       exceptingWindows:no_windows];
      watched = application.processID;
      result.scope = ScreenAudioScope::kApplication;
      result.detail =
          "application '" + Name(application) + "' (pid " + std::to_string(watched) + ")";
    } else {
      NSMutableArray* excluded = [NSMutableArray array];
      for (SCRunningApplication* candidate in content.applications) {
        if (tree.count(candidate.processID) != 0)
          [excluded addObject:candidate];
      }
      filter = [[SCContentFilter alloc] initWithDisplay:display
                                  excludingApplications:excluded
                                       exceptingWindows:no_windows];
      result.scope = ScreenAudioScope::kSystem;
      result.detail = (missing.empty() ? std::string() : missing + "; ") +
                      "all audio except process " + std::to_string(getpid()) +
                      " and its children (" + std::to_string(excluded.count) +
                      " applications excluded)";
    }
    if (!filter)
      return Fail(ScreenAudioError::kStartFailed, "SCContentFilter could not be created");

    if (!target.restart)
      g_delivered_rate.store(0, std::memory_order_relaxed);
    const int rate = StartRate();
    SCStreamConfiguration* config = [[SCStreamConfiguration alloc] init];
    config.capturesAudio = YES;
    config.sampleRate = kSampleRate;
    config.channelCount = kChannels;
    config.excludesCurrentProcessAudio = YES;
    config.showsCursor = NO;
    config.width = 2;
    config.height = 2;
    config.minimumFrameInterval = CMTimeMake(1, 1);
    config.queueDepth = 3;

    LKRootScreenAudioOutput* output =
        [[LKRootScreenAudioOutput alloc] initWithTap:new SckAudioTap(sink, rate, watched)];
    output_ = output;
    audio_queue_ = dispatch_queue_create(
        "com.rootcommunications.root.screen-audio",
        dispatch_queue_attr_make_with_qos_class(DISPATCH_QUEUE_SERIAL, QOS_CLASS_USER_INTERACTIVE,
                                                0));
    frame_queue_ = dispatch_queue_create(
        "com.rootcommunications.root.screen-audio-frames",
        dispatch_queue_attr_make_with_qos_class(DISPATCH_QUEUE_SERIAL, QOS_CLASS_UTILITY, 0));
    SCStream* stream = [[SCStream alloc] initWithFilter:filter
                                          configuration:config
                                               delegate:output];
    [filter release];
    [config release];
    stream_ = stream;
    if (!output || !stream || !audio_queue_ || !frame_queue_)
      return Fail(ScreenAudioError::kInternal,
                  "the ScreenCaptureKit audio stream could not be created");

    NSError* added = nil;
    if (![stream addStreamOutput:output
                            type:SCStreamOutputTypeAudio
              sampleHandlerQueue:audio_queue_
                           error:&added])
      return Fail(ScreenAudioError::kStartFailed,
                  "adding the audio output failed (" + Describe(added) + ")");
    audio_added_ = true;
    // ScreenCaptureKit complains about every frame it has no output for.
    frames_added_ = [stream addStreamOutput:output
                                       type:SCStreamOutputTypeScreen
                         sampleHandlerQueue:frame_queue_
                                      error:nil];

    // The blocks keep the stream, its output and queues alive until
    // ScreenCaptureKit is done with them. A start whose waiter gave up is
    // stopped as soon as it completes, so it never runs unowned.
    auto started = std::make_shared<Completion>();
    capturing_ = true;
    dispatch_queue_t audio_queue = audio_queue_;
    dispatch_queue_t frame_queue = frame_queue_;
    [stream startCaptureWithCompletionHandler:^(NSError* failure) {
      started->error = [failure retain];
      if (started->state.exchange(kCompletionDone) == kCompletionAbandoned && !failure) {
        [stream stopCaptureWithCompletionHandler:^(NSError* ignored) {
          (void)ignored;
          (void)stream;
          (void)output;
          (void)audio_queue;
          (void)frame_queue;
        }];
      }
      dispatch_semaphore_signal(started->done);
    }];
    if (!started->Wait(deadline)) {
      int pending = kCompletionPending;
      if (started->state.compare_exchange_strong(pending, kCompletionAbandoned)) {
        start_outstanding_ = true;
        return Fail(ScreenAudioError::kTimeout,
                    "ScreenCaptureKit did not start the audio stream within 5 s (it is stopped "
                    "when that start completes)");
      }
    }
    if (started->error)
      return Fail(StartError(started->error),
                  "starting the audio stream failed (" + Describe(started->error) + ")");

    if (watched > 0) {
      exit_source_ = dispatch_source_create(DISPATCH_SOURCE_TYPE_PROC,
                                            static_cast<uintptr_t>(watched), DISPATCH_PROC_EXIT,
                                            frame_queue_);
      if (exit_source_) {
        dispatch_source_set_event_handler(exit_source_, ^{
          [output targetExited];
        });
        dispatch_resume(exit_source_);
      }
      if (kill(watched, 0) != 0 && errno == ESRCH)
        return Fail(ScreenAudioError::kTargetGone,
                    "application pid " + std::to_string(watched) + " exited while starting");
    }

    warnings_ = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER, 0, 0, frame_queue_);
    if (warnings_) {
      dispatch_source_set_timer(warnings_, dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC),
                                NSEC_PER_SEC, NSEC_PER_SEC / 4);
      dispatch_source_set_event_handler(warnings_, ^{
        [output logWarnings];
      });
      dispatch_resume(warnings_);
    }

    result.format.sample_rate = rate;
    result.format.channels = kChannels;
    result.process_id = static_cast<uint32_t>(watched);
    return result;
  }

  // A start that is still outstanding is stopped by its own completion; a
  // stop that takes longer than its budget keeps the stream, output and
  // queues alive in its completion.
  API_AVAILABLE(macos(13.0))
  bool StopStream() {
    const Clock::time_point deadline = Clock::now() + kStopBudget;
    LKRootScreenAudioOutput* output = output_;
    SCStream* stream = stream_;
    const bool stop = stream && capturing_ && !start_outstanding_;
    stop_confirmed_ = true;
    if (output && ![output tap]->Close(deadline)) {
      leaked_ = true;
      if (stop) {
        [stream stopCaptureWithCompletionHandler:^(NSError* failure) {
          (void)failure;
        }];
      }
      return false;
    }
    CancelSource(&warnings_);
    CancelSource(&exit_source_);
    if (stop) {
      auto stopped = std::make_shared<Completion>();
      dispatch_queue_t audio_queue = audio_queue_;
      dispatch_queue_t frame_queue = frame_queue_;
      [stream stopCaptureWithCompletionHandler:^(NSError* failure) {
        (void)failure;
        (void)stream;
        (void)output;
        (void)audio_queue;
        (void)frame_queue;
        dispatch_semaphore_signal(stopped->done);
      }];
      stop_confirmed_ = stopped->Wait(deadline);
    }
    if (stream && audio_added_ && !start_outstanding_)
      [stream removeStreamOutput:output type:SCStreamOutputTypeAudio error:nil];
    if (stream && frames_added_ && !start_outstanding_)
      [stream removeStreamOutput:output type:SCStreamOutputTypeScreen error:nil];
    Drain(audio_queue_, deadline);
    Drain(frame_queue_, deadline);
    if (output)
      [output tap]->LogWarnings();
    [stream release];
    [output release];
    stream_ = nil;
    output_ = nil;
    ReleaseDispatch(&audio_queue_);
    ReleaseDispatch(&frame_queue_);
    capturing_ = false;
    audio_added_ = false;
    frames_added_ = false;
    start_outstanding_ = false;
    return true;
  }

  // Typed in the methods above; ScreenCaptureKit types here would need the
  // whole class to be macOS 13+.
  id stream_ = nil;
  id output_ = nil;
  dispatch_queue_t audio_queue_ = nullptr;
  dispatch_queue_t frame_queue_ = nullptr;
  dispatch_source_t exit_source_ = nullptr;
  dispatch_source_t warnings_ = nullptr;
  bool capturing_ = false;
  bool audio_added_ = false;
  bool frames_added_ = false;
  bool start_outstanding_ = false;
  bool leaked_ = false;
  bool stop_confirmed_ = true;
};

}  // namespace

bool RootScreenAudioBackendBuilt() {
  return true;
}

std::unique_ptr<ScreenAudioBackend> CreateRootScreenAudioBackend() {
  return std::make_unique<SckScreenAudioBackend>();
}

}  // namespace livekit_ffi
