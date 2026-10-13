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

// Windows screen audio: WASAPI process loopback (Windows 10 2004, build 19041,
// and later). Everything
// but this process tree (EXCLUDE_TARGET_PROCESS_TREE of our own pid), or only
// one process tree (INCLUDE_TARGET_PROCESS_TREE). Mmdevapi and avrt are loaded
// from System32 at run time, so the DLL links nothing new and loads where they
// are missing.

#include <windows.h>

#include <audioclient.h>
#include <mmdeviceapi.h>
#include <objidl.h>
#include <propidl.h>

#include <algorithm>
#include <atomic>
#include <cstdio>
#include <iterator>
#include <string>

#include "livekit/audio_thread.h"
#include "livekit/root_screen_audio_backend.h"

namespace livekit_ffi {

namespace {

// AUDIOCLIENT_ACTIVATION_PARAMS (audioclientactivationparams.h, NTDDI_WIN10_FE),
// declared here so the build does not depend on the SDK's NTDDI gate.
constexpr DWORD kActivationTypeProcessLoopback = 1;
constexpr DWORD kLoopbackIncludeTree = 0;
constexpr DWORD kLoopbackExcludeTree = 1;
constexpr wchar_t kProcessLoopbackDevice[] = L"VAD\\Process_Loopback";
// Windows 10 2004; the SDK documents 20348, but process loopback works from
// here (OBS gates its application audio capture on it too).
constexpr DWORD kProcessLoopbackMinBuild = 19041;

struct ProcessLoopbackParams {
  DWORD target_process_id;
  DWORD mode;
};

struct ActivationParams {
  DWORD activation_type;
  ProcessLoopbackParams loopback;
};

static_assert(sizeof(ActivationParams) == 12, "AUDIOCLIENT_ACTIVATION_PARAMS layout");

constexpr int kSampleRate = 48000;
constexpr int kChannels = 2;
// Start waits this long; the capture thread's open (activations, retries and
// Initialize) gets a little less, so its own timeout normally answers first.
constexpr DWORD kStartTimeoutMs = 5000;
constexpr DWORD kOpenBudgetMs = 4800;
constexpr DWORD kStopTimeoutMs = 2000;
constexpr DWORD kWaitTimeoutMs = 200;
constexpr ULONGLONG kExitPollMs = 500;
constexpr int kInitializeAttempts = 3;
constexpr DWORD kRetryDelayMs = 200;

using ActivateAudioInterfaceAsyncFn =
    HRESULT(WINAPI*)(LPCWSTR,
                     REFIID,
                     PROPVARIANT*,
                     IActivateAudioInterfaceCompletionHandler*,
                     IActivateAudioInterfaceAsyncOperation**);
using AvSetMmThreadCharacteristicsFn = HANDLE(WINAPI*)(LPCWSTR, LPDWORD);
using AvRevertMmThreadCharacteristicsFn = BOOL(WINAPI*)(HANDLE);
using SetThreadDescriptionFn = HRESULT(WINAPI*)(HANDLE, PCWSTR);
using RtlGetVersionFn = LONG(WINAPI*)(OSVERSIONINFOW*);

struct Exports {
  ActivateAudioInterfaceAsyncFn activate = nullptr;
  AvSetMmThreadCharacteristicsFn mmcss_set = nullptr;
  AvRevertMmThreadCharacteristicsFn mmcss_revert = nullptr;
  SetThreadDescriptionFn describe = nullptr;
  DWORD build = 0;
};

const Exports& LoadExports() {
  static const Exports exports = [] {
    Exports e;
    if (HMODULE mmdevapi = LoadLibraryExW(L"Mmdevapi.dll", nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32))
      e.activate = reinterpret_cast<ActivateAudioInterfaceAsyncFn>(
          reinterpret_cast<void*>(GetProcAddress(mmdevapi, "ActivateAudioInterfaceAsync")));
    if (HMODULE avrt = LoadLibraryExW(L"avrt.dll", nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32)) {
      e.mmcss_set = reinterpret_cast<AvSetMmThreadCharacteristicsFn>(
          reinterpret_cast<void*>(GetProcAddress(avrt, "AvSetMmThreadCharacteristicsW")));
      e.mmcss_revert = reinterpret_cast<AvRevertMmThreadCharacteristicsFn>(
          reinterpret_cast<void*>(GetProcAddress(avrt, "AvRevertMmThreadCharacteristics")));
    }
    if (HMODULE kernel = GetModuleHandleW(L"kernel32.dll"))
      e.describe = reinterpret_cast<SetThreadDescriptionFn>(
          reinterpret_cast<void*>(GetProcAddress(kernel, "SetThreadDescription")));
    if (HMODULE ntdll = GetModuleHandleW(L"ntdll.dll")) {
      auto get_version = reinterpret_cast<RtlGetVersionFn>(
          reinterpret_cast<void*>(GetProcAddress(ntdll, "RtlGetVersion")));
      OSVERSIONINFOW info{};
      info.dwOSVersionInfoSize = sizeof(info);
      if (get_version && get_version(&info) == 0)
        e.build = info.dwBuildNumber;
    }
    return e;
  }();
  return exports;
}

std::string Hex(HRESULT hr) {
  char text[16];
  std::snprintf(text, sizeof(text), "0x%08lX", static_cast<unsigned long>(hr));
  return text;
}

std::string Utf8(const wchar_t* text, int length) {
  if (length <= 0)
    return std::string();
  int bytes = WideCharToMultiByte(CP_UTF8, 0, text, length, nullptr, 0, nullptr, nullptr);
  if (bytes <= 0)
    return std::string();
  std::string out(static_cast<size_t>(bytes), '\0');
  WideCharToMultiByte(CP_UTF8, 0, text, length, out.data(), bytes, nullptr, nullptr);
  return out;
}

std::string ProcessName(DWORD pid) {
  HANDLE process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
  if (!process)
    return std::string();
  wchar_t path[MAX_PATH];
  DWORD size = MAX_PATH;
  std::string name;
  if (QueryFullProcessImageNameW(process, 0, path, &size)) {
    const wchar_t* file = path;
    for (DWORD i = 0; i < size; ++i) {
      if (path[i] == L'\\' || path[i] == L'/')
        file = path + i + 1;
    }
    name = Utf8(file, static_cast<int>(size - static_cast<DWORD>(file - path)));
  }
  CloseHandle(process);
  return name;
}

// Whether `pid` names a process that runs. Access denied (a protected or
// elevated process) still means it runs.
bool ProcessRunning(DWORD pid) {
  HANDLE process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
  if (!process)
    return GetLastError() == ERROR_ACCESS_DENIED;
  DWORD code = 0;
  bool running = !GetExitCodeProcess(process, &code) || code == STILL_ACTIVE;
  CloseHandle(process);
  return running;
}

bool DeviceLost(HRESULT hr) {
  return hr == AUDCLNT_E_DEVICE_INVALIDATED || hr == AUDCLNT_E_SERVICE_NOT_RUNNING ||
         hr == AUDCLNT_E_RESOURCES_INVALIDATED || hr == RPC_E_DISCONNECTED ||
         hr == HRESULT_FROM_WIN32(RPC_S_SERVER_UNAVAILABLE);
}

// A failed activation, Initialize or client start. Only "not supported" codes
// are UNSUPPORTED (the build was checked before); anything else can pass (the
// audio service restarting) and stays restartable.
ScreenAudioError OpenError(HRESULT hr) {
  if (hr == E_ACCESSDENIED)
    return ScreenAudioError::kPermissionDenied;
  if (hr == E_NOTIMPL || hr == HRESULT_FROM_WIN32(ERROR_NOT_SUPPORTED) ||
      hr == HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND))
    return ScreenAudioError::kUnsupported;
  if (DeviceLost(hr))
    return ScreenAudioError::kDeviceLost;
  return ScreenAudioError::kStartFailed;
}

// The activation completes on a COM worker thread; the handler only signals.
class ActivationHandler final : public IActivateAudioInterfaceCompletionHandler,
                                public IAgileObject {
 public:
  ActivationHandler() : event_(CreateEventW(nullptr, TRUE, FALSE, nullptr)) {}

  HANDLE event() const { return event_; }

  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID riid, void** out) override {
    if (out == nullptr)
      return E_POINTER;
    if (riid == __uuidof(IUnknown) ||
        riid == __uuidof(IActivateAudioInterfaceCompletionHandler)) {
      *out = static_cast<IActivateAudioInterfaceCompletionHandler*>(this);
    } else if (riid == __uuidof(IAgileObject)) {
      *out = static_cast<IAgileObject*>(this);
    } else {
      *out = nullptr;
      return E_NOINTERFACE;
    }
    AddRef();
    return S_OK;
  }

  ULONG STDMETHODCALLTYPE AddRef() override { return ++references_; }

  ULONG STDMETHODCALLTYPE Release() override {
    ULONG left = --references_;
    if (left == 0)
      delete this;
    return left;
  }

  HRESULT STDMETHODCALLTYPE
  ActivateCompleted(IActivateAudioInterfaceAsyncOperation* operation) override {
    (void)operation;
    SetEvent(event_);
    return S_OK;
  }

 private:
  ~ActivationHandler() {
    if (event_)
      CloseHandle(event_);
  }

  std::atomic<ULONG> references_{1};
  HANDLE event_;
};

template <typename T>
void SafeRelease(T** pointer) {
  if (*pointer) {
    (*pointer)->Release();
    *pointer = nullptr;
  }
}

struct Resolved {
  DWORD pid = 0;
  DWORD mode = kLoopbackExcludeTree;
  ScreenAudioScope scope = ScreenAudioScope::kSystem;
  std::string detail;
};

Resolved ResolveTarget(const ScreenAudioTarget& target) {
  const DWORD self = GetCurrentProcessId();
  Resolved system;
  system.pid = self;
  system.detail = "all audio except process " + std::to_string(self) + " and its children";

  DWORD pid = 0;
  std::string missing;
  if (target.kind == ScreenAudioTargetKind::kProcess) {
    pid = target.process_id;
    missing = "process " + std::to_string(pid) + " not found";
  } else if (target.kind == ScreenAudioTargetKind::kWindow) {
    HWND window = reinterpret_cast<HWND>(static_cast<uintptr_t>(target.window_id));
    if (target.window_id != 0 && IsWindow(window))
      GetWindowThreadProcessId(window, &pid);
    missing = "window not found";
  }
  if (target.kind == ScreenAudioTargetKind::kSystem)
    return system;
  if (pid == 0 || pid == self) {
    system.detail = (pid == self ? std::string("the target is this process") : missing) +
                    "; " + system.detail;
    return system;
  }
  if (!ProcessRunning(pid)) {
    system.detail = missing + "; " + system.detail;
    return system;
  }
  Resolved include;
  include.pid = pid;
  include.mode = kLoopbackIncludeTree;
  include.scope = ScreenAudioScope::kProcess;
  std::string name = ProcessName(pid);
  include.detail = "process " + std::to_string(pid) +
                   (name.empty() ? std::string() : " (" + name + ")") + " and its children";
  return include;
}

class WasapiProcessLoopbackBackend final : public ScreenAudioBackend {
 public:
  ~WasapiProcessLoopbackBackend() override { Stop(); }

  ScreenAudioStartResult Start(const ScreenAudioTarget& target,
                               ScreenAudioSink* sink) override {
    ScreenAudioStartResult result;
    Stop();
    const Exports& exports = LoadExports();
    if (exports.build != 0 && exports.build < kProcessLoopbackMinBuild) {
      result.error = ScreenAudioError::kUnsupported;
      result.detail = "process loopback needs Windows 10 version 2004 (build 19041) or later; this "
                      "is build " +
                      std::to_string(exports.build);
      return result;
    }
    if (!exports.activate) {
      result.error = ScreenAudioError::kUnsupported;
      result.detail = "Mmdevapi.dll has no ActivateAudioInterfaceAsync";
      return result;
    }
    resolved_ = ResolveTarget(target);
    sink_ = sink;
    open_deadline_ = GetTickCount64() + kOpenBudgetMs;
    stop_event_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    started_event_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (!stop_event_ || !started_event_) {
      CloseEvents();
      result.error = ScreenAudioError::kInternal;
      result.detail = "CreateEvent failed";
      return result;
    }
    thread_ = CreateThread(nullptr, 0, &WasapiProcessLoopbackBackend::ThreadMain, this, 0, nullptr);
    if (!thread_) {
      CloseEvents();
      result.error = ScreenAudioError::kInternal;
      result.detail = "CreateThread failed";
      return result;
    }
    if (WaitForSingleObject(started_event_, kStartTimeoutMs) != WAIT_OBJECT_0) {
      Stop();
      result.error = ScreenAudioError::kTimeout;
      result.detail = "the process loopback client did not start within 5 s";
      return result;
    }
    result = start_result_;
    if (result.error != ScreenAudioError::kNone)
      Stop();
    return result;
  }

  bool Stop() override {
    if (leaked_)
      return false;
    if (thread_) {
      SetEvent(stop_event_);
      if (WaitForSingleObject(thread_, kStopTimeoutMs) != WAIT_OBJECT_0) {
        leaked_ = true;
        return false;
      }
      CloseHandle(thread_);
      thread_ = nullptr;
    }
    CloseEvents();
    sink_ = nullptr;
    return true;
  }

 private:
  static DWORD WINAPI ThreadMain(LPVOID self) {
    static_cast<WasapiProcessLoopbackBackend*>(self)->Run();
    return 0;
  }

  void CloseEvents() {
    if (stop_event_)
      CloseHandle(stop_event_);
    if (started_event_)
      CloseHandle(started_event_);
    stop_event_ = nullptr;
    started_event_ = nullptr;
  }

  void Fail(ScreenAudioError error, std::string detail) {
    start_result_.error = error;
    start_result_.detail = std::move(detail);
  }

  // What is left of the open's budget, in ms.
  DWORD Remaining() const {
    ULONGLONG now = GetTickCount64();
    return now >= open_deadline_ ? 0 : static_cast<DWORD>(open_deadline_ - now);
  }

  void Run() {
    const Exports& exports = LoadExports();
    if (exports.describe)
      exports.describe(GetCurrentThread(), L"RootScreenAudio");
    HRESULT com = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    DWORD task_index = 0;
    HANDLE mmcss = exports.mmcss_set ? exports.mmcss_set(L"Audio", &task_index) : nullptr;

    start_result_ = ScreenAudioStartResult();
    watch_ = Watch::kNone;
    watch_note_.clear();
    if (Open(exports)) {
      start_result_.format = ScreenAudioFormat{kSampleRate, kChannels};
      start_result_.scope = resolved_.scope;
      start_result_.detail = resolved_.detail + watch_note_;
      if (resolved_.scope == ScreenAudioScope::kProcess)
        start_result_.process_id = resolved_.pid;
      SetEvent(started_event_);
      MarkAudioThread();
      Loop();
    } else {
      SetEvent(started_event_);
    }
    Close();
    if (mmcss && exports.mmcss_revert)
      exports.mmcss_revert(mmcss);
    if (SUCCEEDED(com))
      CoUninitialize();
  }

  bool Activate(const Exports& exports, HRESULT* failure) {
    ActivationParams params{};
    params.activation_type = kActivationTypeProcessLoopback;
    params.loopback.target_process_id = resolved_.pid;
    params.loopback.mode = resolved_.mode;
    PROPVARIANT variant{};
    variant.vt = VT_BLOB;
    variant.blob.cbSize = sizeof(params);
    variant.blob.pBlobData = reinterpret_cast<BYTE*>(&params);

    ActivationHandler* handler = new ActivationHandler();
    IActivateAudioInterfaceAsyncOperation* operation = nullptr;
    HRESULT hr = handler->event() ? exports.activate(kProcessLoopbackDevice, __uuidof(IAudioClient),
                                                     &variant, handler, &operation)
                                  : E_OUTOFMEMORY;
    if (SUCCEEDED(hr)) {
      HANDLE waits[2] = {handler->event(), stop_event_};
      DWORD signaled = WaitForMultipleObjects(2, waits, FALSE, Remaining());
      if (signaled == WAIT_OBJECT_0 + 1)
        hr = HRESULT_FROM_WIN32(ERROR_CANCELLED);
      else if (signaled == WAIT_TIMEOUT)
        hr = HRESULT_FROM_WIN32(ERROR_TIMEOUT);
      else if (signaled != WAIT_OBJECT_0)
        hr = HRESULT_FROM_WIN32(GetLastError());
    }
    if (SUCCEEDED(hr)) {
      HRESULT activated = E_FAIL;
      IUnknown* unknown = nullptr;
      hr = operation->GetActivateResult(&activated, &unknown);
      if (SUCCEEDED(hr))
        hr = activated;
      if (SUCCEEDED(hr) && unknown)
        hr = unknown->QueryInterface(__uuidof(IAudioClient), reinterpret_cast<void**>(&client_));
      else if (SUCCEEDED(hr))
        hr = E_NOINTERFACE;
      SafeRelease(&unknown);
    }
    SafeRelease(&operation);
    handler->Release();
    *failure = hr;
    return SUCCEEDED(hr);
  }

  bool Initialize(REFERENCE_TIME duration, HRESULT* failure) {
    WAVEFORMATEX format{};
    format.wFormatTag = WAVE_FORMAT_IEEE_FLOAT;
    format.nChannels = kChannels;
    format.nSamplesPerSec = kSampleRate;
    format.wBitsPerSample = 32;
    format.nBlockAlign = static_cast<WORD>(kChannels * sizeof(float));
    format.nAvgBytesPerSec = kSampleRate * format.nBlockAlign;
    HRESULT hr = client_->Initialize(AUDCLNT_SHAREMODE_SHARED,
                                     AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                                     duration, 0, &format, nullptr);
    *failure = hr;
    return SUCCEEDED(hr);
  }

  static bool BufferRefused(HRESULT hr) {
    return hr == AUDCLNT_E_BUFFER_SIZE_ERROR || hr == AUDCLNT_E_INVALID_DEVICE_PERIOD ||
           hr == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED || hr == E_INVALIDARG;
  }

  bool Open(const Exports& exports) {
    // 100 ms of headroom for a late capture thread; 20 ms (the managed
    // capture's) on a fresh client if the first is refused.
    constexpr REFERENCE_TIME kDurations[] = {1000000, 200000};
    size_t duration = 0;
    HRESULT hr = E_FAIL;
    std::string step;
    for (int attempt = 1; attempt <= kInitializeAttempts; ++attempt) {
      step = "activation";
      if (Activate(exports, &hr)) {
        step = "Initialize";
        if (Initialize(kDurations[duration], &hr))
          break;
      }
      Close();
      if (hr == HRESULT_FROM_WIN32(ERROR_CANCELLED)) {
        Fail(ScreenAudioError::kStartFailed, "stopped while starting");
        return false;
      }
      if (hr == HRESULT_FROM_WIN32(ERROR_TIMEOUT)) {
        Fail(ScreenAudioError::kTimeout,
             "process loopback " + step + " did not complete within the 5 s start budget");
        return false;
      }
      if (step == "Initialize" && BufferRefused(hr) && duration + 1 < std::size(kDurations)) {
        ++duration;
        --attempt;
        continue;
      }
      if (hr != E_UNEXPECTED || attempt == kInitializeAttempts || Remaining() == 0) {
        Fail(OpenError(hr), "process loopback " + step + " failed (" + Hex(hr) + ")");
        return false;
      }
      DWORD delay = std::min<DWORD>(kRetryDelayMs * static_cast<DWORD>(attempt), Remaining());
      if (WaitForSingleObject(stop_event_, delay) == WAIT_OBJECT_0) {
        Fail(ScreenAudioError::kStartFailed, "stopped while starting");
        return false;
      }
    }

    audio_event_ = CreateEventW(nullptr, FALSE, FALSE, nullptr);
    if (!audio_event_) {
      Fail(ScreenAudioError::kInternal, "CreateEvent failed");
      return false;
    }
    if (resolved_.mode == kLoopbackIncludeTree && !WatchTarget())
      return false;
    hr = client_->SetEventHandle(audio_event_);
    if (SUCCEEDED(hr))
      hr = client_->GetService(__uuidof(IAudioCaptureClient), reinterpret_cast<void**>(&capture_));
    if (SUCCEEDED(hr))
      hr = client_->Start();
    if (FAILED(hr)) {
      Fail(OpenError(hr), "process loopback start failed (" + Hex(hr) + ")");
      return false;
    }
    return true;
  }

  // The captured process's exit ends the capture (TARGET_GONE). Its handle is
  // waited on, or its exit code polled where the handle cannot be waited on
  // (an anti-cheat driver may strip SYNCHRONIZE from it).
  bool WatchTarget() {
    const DWORD pid = resolved_.pid;
    HANDLE process = OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
    if (!process)
      process = OpenProcess(SYNCHRONIZE, FALSE, pid);
    if (!process)
      process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
    if (!process) {
      DWORD error = GetLastError();
      if (error == ERROR_INVALID_PARAMETER) {
        Fail(ScreenAudioError::kTargetGone,
             "process " + std::to_string(pid) + " exited while starting");
        return false;
      }
      watch_note_ =
          "; its exit is not watched (OpenProcess failed, error " + std::to_string(error) + ")";
      return true;
    }
    target_process_ = process;
    DWORD state = WaitForSingleObject(process, 0);
    DWORD code = 0;
    bool queried = GetExitCodeProcess(process, &code) != 0;
    if (state == WAIT_OBJECT_0 || (queried && code != STILL_ACTIVE)) {
      Fail(ScreenAudioError::kTargetGone,
           "process " + std::to_string(pid) + " exited while starting");
      return false;
    }
    if (state == WAIT_TIMEOUT)
      watch_ = Watch::kWait;
    else if (queried)
      watch_ = Watch::kPoll;
    else
      watch_note_ = "; its exit is not watched (no SYNCHRONIZE or query access)";
    return true;
  }

  // Polled at most every kExitPollMs.
  bool TargetExited(ULONGLONG* next_poll) {
    ULONGLONG now = GetTickCount64();
    if (now < *next_poll)
      return false;
    *next_poll = now + kExitPollMs;
    DWORD code = 0;
    if (!GetExitCodeProcess(target_process_, &code)) {
      watch_ = Watch::kNone;
      return false;
    }
    return code != STILL_ACTIVE;
  }

  // Wait-free per packet: the packet goes to the sink straight from the
  // client's buffer.
  HRESULT Drain() {
    while (true) {
      UINT32 packet = 0;
      HRESULT hr = capture_->GetNextPacketSize(&packet);
      if (FAILED(hr))
        return hr;
      if (packet == 0)
        return S_OK;
      BYTE* data = nullptr;
      UINT32 frames = 0;
      DWORD flags = 0;
      hr = capture_->GetBuffer(&data, &frames, &flags, nullptr, nullptr);
      if (FAILED(hr))
        return hr;
      if (hr == AUDCLNT_S_BUFFER_EMPTY)
        return S_OK;
      if (flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY)
        sink_->MarkDiscontinuity();
      if (frames > 0) {
        if ((flags & AUDCLNT_BUFFERFLAGS_SILENT) || data == nullptr)
          sink_->WriteSilence(frames);
        else
          sink_->Write(reinterpret_cast<const float*>(data), frames);
      }
      hr = capture_->ReleaseBuffer(frames);
      if (FAILED(hr))
        return hr;
    }
  }

  void Loop() {
    HANDLE waits[3] = {stop_event_, audio_event_, target_process_};
    DWORD count = watch_ == Watch::kWait ? 3 : 2;
    ULONGLONG next_poll = 0;
    while (true) {
      DWORD signaled = WaitForMultipleObjects(count, waits, FALSE, kWaitTimeoutMs);
      if (signaled == WAIT_OBJECT_0)
        return;
      if (count == 3 && signaled == WAIT_OBJECT_0 + 2) {
        sink_->ReportError(ScreenAudioError::kTargetGone, "the captured process exited");
        break;
      }
      if (signaled == WAIT_FAILED) {
        if (count == 3) {
          count = 2;
          watch_ = Watch::kPoll;
          continue;
        }
        sink_->ReportError(ScreenAudioError::kInternal, "WaitForMultipleObjects failed");
        break;
      }
      if (watch_ == Watch::kPoll && TargetExited(&next_poll)) {
        sink_->ReportError(ScreenAudioError::kTargetGone, "the captured process exited");
        break;
      }
      HRESULT hr = Drain();
      if (FAILED(hr)) {
        char detail[64];
        std::snprintf(detail, sizeof(detail), "process loopback capture failed (0x%08lX)",
                      static_cast<unsigned long>(hr));
        sink_->ReportError(DeviceLost(hr) ? ScreenAudioError::kDeviceLost
                                          : ScreenAudioError::kStreamFailed,
                           detail);
        break;
      }
    }
    WaitForSingleObject(stop_event_, INFINITE);
  }

  void Close() {
    if (client_)
      client_->Stop();
    SafeRelease(&capture_);
    SafeRelease(&client_);
    if (audio_event_)
      CloseHandle(audio_event_);
    audio_event_ = nullptr;
    if (target_process_)
      CloseHandle(target_process_);
    target_process_ = nullptr;
  }

  Resolved resolved_;
  ScreenAudioSink* sink_ = nullptr;
  ScreenAudioStartResult start_result_;
  ULONGLONG open_deadline_ = 0;
  HANDLE thread_ = nullptr;
  HANDLE stop_event_ = nullptr;
  HANDLE started_event_ = nullptr;
  bool leaked_ = false;

  // Capture thread only.
  enum class Watch { kNone, kWait, kPoll };
  IAudioClient* client_ = nullptr;
  IAudioCaptureClient* capture_ = nullptr;
  HANDLE audio_event_ = nullptr;
  HANDLE target_process_ = nullptr;
  Watch watch_ = Watch::kNone;
  std::string watch_note_;
};

}  // namespace

bool RootScreenAudioBackendBuilt() {
  return true;
}

std::unique_ptr<ScreenAudioBackend> CreateRootScreenAudioBackend() {
  return std::make_unique<WasapiProcessLoopbackBackend>();
}

}  // namespace livekit_ffi
