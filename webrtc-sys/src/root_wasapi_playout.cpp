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

// Windows playout the platform ADM cannot open. AudioDeviceWindowsCore (m150)
// initializes its render client as 16-bit PCM in shared mode without
// AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, so the audio engine accepts only its own
// mix format's rate and channel count; the ADM tries 48000, 44100, 16000,
// 96000, 32000 and 8000 Hz in one or two channels and initializes with the
// last of them when none matches, which fails with
// AUDCLNT_E_UNSUPPORTED_FORMAT on a 6 or 8 channel (surround) or an 88.2,
// 176.4 or 192 kHz mix format. Avrt is loaded from System32 at run time.

#include "livekit/root_wasapi_playout.h"

#include <windows.h>

#include <audioclient.h>
#include <mmdeviceapi.h>

#include <atomic>
#include <cstdio>
#include <cstring>
#include <memory>
#include <string>

#include "livekit/audio_thread.h"

namespace livekit_ffi {

namespace {

constexpr DWORD kRate = 48000;
constexpr UINT32 kFrames = 480;
// 30 ms, as the ADM asks at 44.1 kHz: 10 ms pulls keep 20 to 30 ms queued.
constexpr REFERENCE_TIME kBufferDuration = 300000;
constexpr DWORD kStartTimeoutMs = 1000;
constexpr DWORD kStopTimeoutMs = 2000;
constexpr DWORD kWaitTimeoutMs = 200;
constexpr DWORD kAdmRates[] = {48000, 44100, 16000, 96000, 32000, 8000};
constexpr HRESULT kNotFound = static_cast<HRESULT>(0x80070490);

using AvSetMmThreadCharacteristicsFn = HANDLE(WINAPI*)(LPCWSTR, LPDWORD);
using AvRevertMmThreadCharacteristicsFn = BOOL(WINAPI*)(HANDLE);
using SetThreadDescriptionFn = HRESULT(WINAPI*)(HANDLE, PCWSTR);

struct Exports {
  AvSetMmThreadCharacteristicsFn mmcss_set = nullptr;
  AvRevertMmThreadCharacteristicsFn mmcss_revert = nullptr;
  SetThreadDescriptionFn describe = nullptr;
};

const Exports& LoadExports() {
  static const Exports exports = [] {
    Exports e;
    if (HMODULE avrt = LoadLibraryExW(L"avrt.dll", nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32)) {
      e.mmcss_set = reinterpret_cast<AvSetMmThreadCharacteristicsFn>(
          reinterpret_cast<void*>(GetProcAddress(avrt, "AvSetMmThreadCharacteristicsW")));
      e.mmcss_revert = reinterpret_cast<AvRevertMmThreadCharacteristicsFn>(
          reinterpret_cast<void*>(GetProcAddress(avrt, "AvRevertMmThreadCharacteristics")));
    }
    if (HMODULE kernel = GetModuleHandleW(L"kernel32.dll"))
      e.describe = reinterpret_cast<SetThreadDescriptionFn>(
          reinterpret_cast<void*>(GetProcAddress(kernel, "SetThreadDescription")));
    return e;
  }();
  return exports;
}

template <class T>
void SafeRelease(T** object) {
  if (*object) {
    (*object)->Release();
    *object = nullptr;
  }
}

const char* HresultName(HRESULT hr) {
  struct Named {
    HRESULT hr;
    const char* name;
  };
  static const Named kNames[] = {
      {AUDCLNT_E_UNSUPPORTED_FORMAT, "AUDCLNT_E_UNSUPPORTED_FORMAT"},
      {AUDCLNT_E_DEVICE_IN_USE, "AUDCLNT_E_DEVICE_IN_USE"},
      {AUDCLNT_E_DEVICE_INVALIDATED, "AUDCLNT_E_DEVICE_INVALIDATED"},
      {AUDCLNT_E_ENDPOINT_CREATE_FAILED, "AUDCLNT_E_ENDPOINT_CREATE_FAILED"},
      {AUDCLNT_E_SERVICE_NOT_RUNNING, "AUDCLNT_E_SERVICE_NOT_RUNNING"},
      {AUDCLNT_E_CPUUSAGE_EXCEEDED, "AUDCLNT_E_CPUUSAGE_EXCEEDED"},
      {AUDCLNT_E_BUFFER_SIZE_ERROR, "AUDCLNT_E_BUFFER_SIZE_ERROR"},
      {AUDCLNT_E_INVALID_DEVICE_PERIOD, "AUDCLNT_E_INVALID_DEVICE_PERIOD"},
      {AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED, "AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED"},
      {AUDCLNT_E_ALREADY_INITIALIZED, "AUDCLNT_E_ALREADY_INITIALIZED"},
      {AUDCLNT_E_RESOURCES_INVALIDATED, "AUDCLNT_E_RESOURCES_INVALIDATED"},
      {AUDCLNT_E_EXCLUSIVE_MODE_ONLY, "AUDCLNT_E_EXCLUSIVE_MODE_ONLY"},
      {AUDCLNT_E_WRONG_ENDPOINT_TYPE, "AUDCLNT_E_WRONG_ENDPOINT_TYPE"},
      {E_ACCESSDENIED, "E_ACCESSDENIED"},
      {E_OUTOFMEMORY, "E_OUTOFMEMORY"},
      {E_INVALIDARG, "E_INVALIDARG"},
      {E_POINTER, "E_POINTER"},
      {E_FAIL, "E_FAIL"},
      {CO_E_NOTINITIALIZED, "CO_E_NOTINITIALIZED"},
      {kNotFound, "E_NOTFOUND"},
      {S_FALSE, "S_FALSE"},
  };
  for (const Named& named : kNames) {
    if (named.hr == hr)
      return named.name;
  }
  return nullptr;
}

std::string DescribeHr(HRESULT hr) {
  char text[16];
  std::snprintf(text, sizeof(text), "0x%08lX", static_cast<unsigned long>(hr));
  const char* name = HresultName(hr);
  return name ? std::string(text) + " " + name : std::string(text);
}

std::string DescribeFormat(const WAVEFORMATEX& format) {
  WORD tag = format.wFormatTag;
  std::string extensible;
  if (tag == WAVE_FORMAT_EXTENSIBLE && format.cbSize >= 22) {
    const auto& ext = reinterpret_cast<const WAVEFORMATEXTENSIBLE&>(format);
    // KSDATAFORMAT_SUBTYPE_PCM and _IEEE_FLOAT are the format tag in Data1.
    tag = static_cast<WORD>(ext.SubFormat.Data1);
    char mask[48];
    std::snprintf(mask, sizeof(mask), " (extensible, mask 0x%lX)",
                  static_cast<unsigned long>(ext.dwChannelMask));
    extensible = mask;
  }
  std::string kind = tag == WAVE_FORMAT_PCM          ? "PCM"
                     : tag == WAVE_FORMAT_IEEE_FLOAT ? "float"
                                                     : "tag " + std::to_string(tag);
  return std::to_string(format.nSamplesPerSec) + " Hz " + std::to_string(format.nChannels) +
         " ch " + std::to_string(format.wBitsPerSample) + "-bit " + kind + extensible;
}

WAVEFORMATEX Pcm16(DWORD rate, WORD channels) {
  WAVEFORMATEX format{};
  format.wFormatTag = WAVE_FORMAT_PCM;
  format.nChannels = channels;
  format.nSamplesPerSec = rate;
  format.wBitsPerSample = 16;
  format.nBlockAlign = static_cast<WORD>(channels * 2);
  format.nAvgBytesPerSec = rate * format.nBlockAlign;
  return format;
}

std::wstring Utf16(const std::string& text) {
  if (text.empty())
    return std::wstring();
  int length = MultiByteToWideChar(CP_UTF8, 0, text.data(), static_cast<int>(text.size()),
                                   nullptr, 0);
  std::wstring wide(length > 0 ? length : 0, L'\0');
  if (length > 0)
    MultiByteToWideChar(CP_UTF8, 0, text.data(), static_cast<int>(text.size()), wide.data(),
                        length);
  return wide;
}

// The render endpoint for an ADM GUID (the endpoint ID string), or the
// default console output (the ADM's kDefaultDevice) for an empty one.
HRESULT OpenEndpoint(const std::string& endpoint_id, IMMDevice** device, std::string* step) {
  IMMDeviceEnumerator* enumerator = nullptr;
  *step = "MMDeviceEnumerator";
  HRESULT hr = CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_ALL,
                                __uuidof(IMMDeviceEnumerator),
                                reinterpret_cast<void**>(&enumerator));
  if (SUCCEEDED(hr)) {
    if (endpoint_id.empty()) {
      *step = "GetDefaultAudioEndpoint";
      hr = enumerator->GetDefaultAudioEndpoint(eRender, eConsole, device);
    } else {
      *step = "GetDevice";
      hr = enumerator->GetDevice(Utf16(endpoint_id).c_str(), device);
    }
  }
  SafeRelease(&enumerator);
  return hr;
}

HRESULT ActivateClient(IMMDevice* device, IAudioClient** client) {
  return device->Activate(__uuidof(IAudioClient), CLSCTX_ALL, nullptr,
                          reinterpret_cast<void**>(client));
}

// The rest of the ADM's InitPlayout on an initialized client.
HRESULT FinishAdmOpen(IAudioClient* client, std::string* step) {
  HANDLE event = CreateEventW(nullptr, FALSE, FALSE, nullptr);
  *step = "SetEventHandle";
  HRESULT hr = event ? client->SetEventHandle(event) : E_OUTOFMEMORY;
  if (SUCCEEDED(hr)) {
    IAudioRenderClient* render = nullptr;
    *step = "GetService(IAudioRenderClient)";
    hr = client->GetService(__uuidof(IAudioRenderClient), reinterpret_cast<void**>(&render));
    SafeRelease(&render);
  }
  if (event)
    CloseHandle(event);
  return hr;
}

class WasapiPlayoutFallback final : public AdmPlayoutFallback {
 public:
  explicit WasapiPlayoutFallback(webrtc::AudioTransport* transport)
      : state_(std::make_shared<State>()) {
    state_->transport = transport;
  }

  // A render thread that did not stop keeps the state, its events and its
  // handle (they are leaked with it).
  ~WasapiPlayoutFallback() override { Stop(); }

  int32_t Init(const std::string& endpoint_id, size_t channels) override {
    if (state_->client)
      return 0;
    state_->channels = channels == 1 ? 1 : 2;
    std::string step;
    const HRESULT hr = Open(endpoint_id, &step);
    if (FAILED(hr)) {
      state_->Close();
      describe_ = step + " " + DescribeHr(hr);
      return -1;
    }
    return 0;
  }

  int32_t Start() override {
    if (!state_->client)
      return -1;
    if (thread_)
      return 0;
    stop_event_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    started_event_ = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (!stop_event_ || !started_event_) {
      CloseEvents();
      return -1;
    }
    auto* thread_state = new Thread{state_, stop_event_, started_event_};
    thread_ = CreateThread(nullptr, 0, &WasapiPlayoutFallback::ThreadMain, thread_state, 0,
                           nullptr);
    if (!thread_) {
      delete thread_state;
      CloseEvents();
      return -1;
    }
    if (WaitForSingleObject(started_event_, kStartTimeoutMs) != WAIT_OBJECT_0 ||
        FAILED(state_->start_hr.load())) {
      if (FAILED(state_->start_hr.load()))
        describe_ += "; Start " + DescribeHr(state_->start_hr.load());
      Stop();
      return -1;
    }
    return 0;
  }

  int32_t Stop() override {
    if (thread_) {
      SetEvent(stop_event_);
      if (WaitForSingleObject(thread_, kStopTimeoutMs) != WAIT_OBJECT_0)
        return -1;
      CloseHandle(thread_);
      thread_ = nullptr;
    }
    CloseEvents();
    state_->Close();
    return 0;
  }

  bool Playing() const override { return state_->playing.load(std::memory_order_acquire); }

  uint16_t DelayMs() const override { return state_->delay_ms.load(std::memory_order_relaxed); }

  std::string Describe() const override { return describe_; }

 private:
  // Shared with the render thread, which keeps it when a Stop gives up on it.
  struct State {
    webrtc::AudioTransport* transport = nullptr;
    WORD channels = 2;
    IAudioClient* client = nullptr;
    IAudioRenderClient* render = nullptr;
    HANDLE ready = nullptr;
    UINT32 buffer_frames = 0;
    uint32_t latency_ms = 0;
    std::atomic<HRESULT> start_hr{S_OK};
    std::atomic<bool> playing{false};
    std::atomic<uint16_t> delay_ms{0};

    void Close() {
      playing.store(false, std::memory_order_release);
      delay_ms.store(0, std::memory_order_relaxed);
      SafeRelease(&render);
      SafeRelease(&client);
      if (ready)
        CloseHandle(ready);
      ready = nullptr;
    }
    ~State() { Close(); }
  };

  struct Thread {
    std::shared_ptr<State> state;
    HANDLE stop;
    HANDLE started;
  };

  HRESULT Open(const std::string& endpoint_id, std::string* step) {
    State& s = *state_;
    IMMDevice* device = nullptr;
    HRESULT hr = OpenEndpoint(endpoint_id, &device, step);
    if (SUCCEEDED(hr)) {
      *step = "Activate(IAudioClient)";
      hr = ActivateClient(device, &s.client);
    }
    SafeRelease(&device);
    std::string mix = "unknown";
    if (SUCCEEDED(hr)) {
      WAVEFORMATEX* format = nullptr;
      if (SUCCEEDED(s.client->GetMixFormat(&format)) && format)
        mix = DescribeFormat(*format);
      CoTaskMemFree(format);
      const WAVEFORMATEX pcm = Pcm16(kRate, s.channels);
      *step = "Initialize";
      hr = s.client->Initialize(AUDCLNT_SHAREMODE_SHARED,
                                AUDCLNT_STREAMFLAGS_EVENTCALLBACK |
                                    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM |
                                    AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY |
                                    AUDCLNT_STREAMFLAGS_NOPERSIST,
                                kBufferDuration, 0, &pcm, nullptr);
    }
    if (SUCCEEDED(hr)) {
      *step = "GetBufferSize";
      hr = s.client->GetBufferSize(&s.buffer_frames);
    }
    if (SUCCEEDED(hr) && s.buffer_frames < kFrames) {
      hr = AUDCLNT_E_BUFFER_SIZE_ERROR;
    }
    if (SUCCEEDED(hr)) {
      s.ready = CreateEventW(nullptr, FALSE, FALSE, nullptr);
      *step = "SetEventHandle";
      hr = s.ready ? s.client->SetEventHandle(s.ready) : E_OUTOFMEMORY;
    }
    if (SUCCEEDED(hr)) {
      *step = "GetService(IAudioRenderClient)";
      hr = s.client->GetService(__uuidof(IAudioRenderClient),
                                reinterpret_cast<void**>(&s.render));
    }
    if (SUCCEEDED(hr)) {
      REFERENCE_TIME latency = 0;
      if (SUCCEEDED(s.client->GetStreamLatency(&latency)))
        s.latency_ms = static_cast<uint32_t>(latency / 10000);
      describe_ = "48000 Hz " + std::to_string(s.channels) +
                  " ch 16-bit PCM converted to the mix format " + mix + ", " +
                  std::to_string(s.buffer_frames) + " frame buffer";
    }
    return hr;
  }

  void CloseEvents() {
    if (stop_event_)
      CloseHandle(stop_event_);
    if (started_event_)
      CloseHandle(started_event_);
    stop_event_ = nullptr;
    started_event_ = nullptr;
  }

  static DWORD WINAPI ThreadMain(LPVOID param) {
    std::unique_ptr<Thread> thread(static_cast<Thread*>(param));
    Run(*thread->state, thread->stop, thread->started);
    return 0;
  }

  static void Run(State& s, HANDLE stop, HANDLE started) {
    const Exports& exports = LoadExports();
    if (exports.describe)
      exports.describe(GetCurrentThread(), L"RootPlayoutFallback");
    const HRESULT com = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    DWORD task_index = 0;
    HANDLE mmcss = exports.mmcss_set ? exports.mmcss_set(L"Pro Audio", &task_index) : nullptr;

    // Silence first, so the engine has a buffer when the stream starts.
    BYTE* data = nullptr;
    if (SUCCEEDED(s.render->GetBuffer(s.buffer_frames, &data)))
      s.render->ReleaseBuffer(s.buffer_frames, AUDCLNT_BUFFERFLAGS_SILENT);
    const HRESULT hr = s.client->Start();
    s.start_hr.store(hr);
    if (SUCCEEDED(hr))
      s.playing.store(true, std::memory_order_release);
    SetEvent(started);
    if (SUCCEEDED(hr)) {
      MarkAudioThread();
      HANDLE waits[2] = {stop, s.ready};
      for (;;) {
        const DWORD signaled = WaitForMultipleObjects(2, waits, FALSE, kWaitTimeoutMs);
        if (signaled == WAIT_TIMEOUT)
          continue;
        if (signaled != WAIT_OBJECT_0 + 1 || !Fill(s))
          break;
      }
      s.client->Stop();
      s.playing.store(false, std::memory_order_release);
    }
    if (mmcss && exports.mmcss_revert)
      exports.mmcss_revert(mmcss);
    if (SUCCEEDED(com))
      CoUninitialize();
  }

  // Whole 10 ms pulls while the buffer has room. False when the device went
  // away; the stream then renders nothing until playout is reopened.
  static bool Fill(State& s) {
    UINT32 padding = 0;
    if (FAILED(s.client->GetCurrentPadding(&padding)))
      return false;
    s.delay_ms.store(static_cast<uint16_t>(padding * 1000 / kRate + s.latency_ms),
                     std::memory_order_relaxed);
    UINT32 room = s.buffer_frames - padding;
    while (room >= kFrames) {
      BYTE* data = nullptr;
      if (FAILED(s.render->GetBuffer(kFrames, &data)))
        return false;
      size_t samples_out = 0;
      int64_t elapsed_time_ms = 0;
      int64_t ntp_time_ms = 0;
      s.transport->NeedMorePlayData(kFrames, s.channels * sizeof(int16_t), s.channels, kRate,
                                    data, samples_out, &elapsed_time_ms, &ntp_time_ms);
      if (FAILED(s.render->ReleaseBuffer(kFrames, 0)))
        return false;
      room -= kFrames;
    }
    return true;
  }

  std::shared_ptr<State> state_;
  HANDLE thread_ = nullptr;
  HANDLE stop_event_ = nullptr;
  HANDLE started_event_ = nullptr;
  std::string describe_;
};

}  // namespace

AdmPlayoutDiagnosis DiagnoseWasapiPlayout(const std::string& endpoint_id, bool stereo) {
  AdmPlayoutDiagnosis diagnosis;
  std::string step;
  IMMDevice* device = nullptr;
  IAudioClient* client = nullptr;
  HRESULT hr = OpenEndpoint(endpoint_id, &device, &step);
  if (SUCCEEDED(hr)) {
    step = "Activate(IAudioClient)";
    hr = ActivateClient(device, &client);
  }
  if (FAILED(hr)) {
    diagnosis.text = step + " " + DescribeHr(hr);
    SafeRelease(&device);
    return diagnosis;
  }

  std::string mix = "unknown";
  WAVEFORMATEX* mix_format = nullptr;
  const HRESULT mix_hr = client->GetMixFormat(&mix_format);
  if (SUCCEEDED(mix_hr) && mix_format)
    mix = DescribeFormat(*mix_format);
  else
    mix += " (GetMixFormat " + DescribeHr(mix_hr) + ")";
  CoTaskMemFree(mix_format);

  const WORD order[2] = {static_cast<WORD>(stereo ? 2 : 1), static_cast<WORD>(stereo ? 1 : 2)};
  WAVEFORMATEX asked{};
  bool supported = false;
  std::string closest;
  for (DWORD rate : kAdmRates) {
    for (WORD channels : order) {
      asked = Pcm16(rate, channels);
      WAVEFORMATEX* match = nullptr;
      const HRESULT supports = client->IsFormatSupported(AUDCLNT_SHAREMODE_SHARED, &asked, &match);
      if (match && closest.empty())
        closest = DescribeFormat(*match);
      CoTaskMemFree(match);
      if (supports == S_OK) {
        supported = true;
        break;
      }
    }
    if (supported)
      break;
  }

  // As the ADM: whatever the search ended on, a larger buffer at 44.1 kHz.
  step = "Initialize";
  hr = client->Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                          supported && asked.nSamplesPerSec == 44100 ? 300000 : 0, 0, &asked,
                          nullptr);
  if (SUCCEEDED(hr))
    hr = FinishAdmOpen(client, &step);
  SafeRelease(&client);

  std::string text = SUCCEEDED(hr) ? std::string("the ADM's open succeeded when repeated")
                                   : step + " " + DescribeHr(hr);
  text += ", asking " + DescribeFormat(asked);
  if (!supported) {
    text += " (no 16-bit PCM format of the ADM's list is supported";
    text += closest.empty() ? ")" : "; closest " + closest + ")";
  }
  text += "; mix format " + mix;

  IAudioClient* converted = nullptr;
  HRESULT converted_hr = ActivateClient(device, &converted);
  if (SUCCEEDED(converted_hr)) {
    const WAVEFORMATEX pcm = Pcm16(kRate, stereo ? 2 : 1);
    converted_hr = converted->Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM |
            AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY | AUDCLNT_STREAMFLAGS_NOPERSIST,
        kBufferDuration, 0, &pcm, nullptr);
  }
  SafeRelease(&converted);
  SafeRelease(&device);
  diagnosis.converted_opens = SUCCEEDED(converted_hr);
  text += diagnosis.converted_opens ? "; opens with the engine's conversion"
                                    : "; with the engine's conversion: " +
                                          DescribeHr(converted_hr);
  diagnosis.text = std::move(text);
  return diagnosis;
}

std::unique_ptr<AdmPlayoutFallback> CreateWasapiPlayoutFallback(
    webrtc::AudioTransport* transport) {
  return std::make_unique<WasapiPlayoutFallback>(transport);
}

}  // namespace livekit_ffi
