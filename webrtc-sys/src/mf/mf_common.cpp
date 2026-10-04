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

#include "mf_common.h"

#include <d3d10_1.h>  // ID3D10Multithread
#include <dxgi.h>

// Instantiate the CODECAPI_* GUIDs (codecapi.h only declares them unless
// INITGUID is in effect). The definitions are DECLSPEC_SELECTANY, so a single
// translation unit doing this is sufficient and safe; every other file
// includes codecapi.h without initguid.h and links against these.
#include <initguid.h>

#include <codecapi.h>

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cwchar>
#include <initializer_list>
#include <iterator>
#include <mutex>
#include <utility>

#include "rtc_base/logging.h"

namespace livekit_ffi {

namespace {

struct ThreadComInit {
  bool ok = false;
  bool should_uninit = false;

  ThreadComInit() {
    HRESULT hr = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    if (SUCCEEDED(hr)) {
      // S_FALSE means COM was already initialized on this thread; either way
      // this thread now owns a reference that must be released on exit.
      ok = true;
      should_uninit = true;
    } else if (hr == RPC_E_CHANGED_MODE) {
      // Thread is already in an STA; the MF objects used here are
      // free-threaded, so proceed without taking a reference.
      ok = true;
    }
  }

  ~ThreadComInit() {
    if (should_uninit) {
      CoUninitialize();
    }
  }
};

}  // namespace

bool EnsureComInitialized() {
  thread_local ThreadComInit init;
  return init.ok;
}

bool IsMfPlatAvailable() {
  static const bool available = [] {
    // Loaded from System32 only, and kept loaded: it is the module the
    // delay-load helper will bind the MF imports to.
    HMODULE module =
        LoadLibraryExW(L"mfplat.dll", nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32);
    if (!module) {
      RTC_LOG(LS_WARNING)
          << "mfplat.dll is not available (error " << GetLastError()
          << "; Windows N/KN without the Media Feature Pack?). "
             "MediaFoundation video codecs are disabled.";
      return false;
    }
    return true;
  }();
  return available;
}

bool EnsureMFStarted() {
  static bool ok = [] {
    if (!IsMfPlatAvailable()) {
      return false;
    }
    HRESULT hr = MFStartup(MF_VERSION, MFSTARTUP_LITE);
    if (FAILED(hr)) {
      RTC_LOG(LS_ERROR) << "MFStartup failed: " << HResultToString(hr);
      return false;
    }
    return true;
  }();
  return ok;
}

std::string HResultToString(HRESULT hr) {
  char buf[16];
  std::snprintf(buf, sizeof(buf), "0x%08lX", static_cast<unsigned long>(hr));
  return buf;
}

std::optional<std::string> GetEnvVar(const char* name) {
  char buf[256];
  const DWORD n = GetEnvironmentVariableA(name, buf, sizeof(buf));
  if (n == 0 || n >= sizeof(buf)) {
    return std::nullopt;
  }
  return std::string(buf, n);
}

namespace {

bool EnvValueIn(const char* name, std::initializer_list<const char*> values) {
  std::optional<std::string> value = GetEnvVar(name);
  if (!value) {
    return false;
  }
  for (const char* candidate : values) {
    if (_stricmp(value->c_str(), candidate) == 0) {
      return true;
    }
  }
  return false;
}

}  // namespace

bool EnvFlagSet(const char* name) {
  return EnvValueIn(name, {"1", "true", "yes", "on"});
}

bool EnvFlagCleared(const char* name) {
  return EnvValueIn(name, {"0", "false", "no", "off"});
}

std::optional<int64_t> GetEnvInt(const char* name) {
  std::optional<std::string> value = GetEnvVar(name);
  if (!value || value->empty()) {
    return std::nullopt;
  }
  char* end = nullptr;
  const long long parsed = std::strtoll(value->c_str(), &end, 10);
  if (end == value->c_str() || *end != '\0') {
    return std::nullopt;
  }
  return parsed;
}

namespace {

// MFT_ENUM_ADAPTER_LUID from mfapi.h. A local copy so this neither depends
// on NTDDI_VERSION >= NTDDI_WIN10_RS1 nor on an import library defining it.
constexpr GUID kMftEnumAdapterLuid = {
    0x1d39518c,
    0xe220,
    0x4da8,
    {0xa0, 0x7f, 0xba, 0x17, 0x25, 0x52, 0xd6, 0xb1}};

std::string LuidToString(const LUID& luid) {
  char buf[32];
  std::snprintf(buf, sizeof(buf), "%08lX:%08lX",
                static_cast<unsigned long>(luid.HighPart),
                static_cast<unsigned long>(luid.LowPart));
  return buf;
}

HRESULT FindAdapterByLuid(const LUID& luid, ComPtr<IDXGIAdapter1>* out) {
  ComPtr<IDXGIFactory1> factory;
  HRESULT hr = CreateDXGIFactory1(IID_PPV_ARGS(&factory));
  if (FAILED(hr)) {
    return hr;
  }
  for (UINT i = 0;; i++) {
    ComPtr<IDXGIAdapter1> adapter;
    hr = factory->EnumAdapters1(i, &adapter);
    if (FAILED(hr)) {
      return hr;
    }
    DXGI_ADAPTER_DESC1 desc = {};
    if (SUCCEEDED(adapter->GetDesc1(&desc)) &&
        desc.AdapterLuid.LowPart == luid.LowPart &&
        desc.AdapterLuid.HighPart == luid.HighPart) {
      *out = adapter;
      return S_OK;
    }
  }
}

HRESULT DefaultAdapterLuid(LUID* out) {
  ComPtr<IDXGIFactory1> factory;
  HRESULT hr = CreateDXGIFactory1(IID_PPV_ARGS(&factory));
  if (FAILED(hr)) {
    return hr;
  }
  ComPtr<IDXGIAdapter1> adapter;
  hr = factory->EnumAdapters1(0, &adapter);
  if (FAILED(hr)) {
    return hr;
  }
  DXGI_ADAPTER_DESC1 desc = {};
  hr = adapter->GetDesc1(&desc);
  if (SUCCEEDED(hr)) {
    *out = desc.AdapterLuid;
  }
  return hr;
}

HRESULT CreateD3D11DeviceBundle(const LUID& luid, D3D11DeviceBundle* out) {
  ComPtr<IDXGIAdapter1> adapter;
  HRESULT hr = FindAdapterByLuid(luid, &adapter);
  if (FAILED(hr)) {
    RTC_LOG(LS_WARNING) << "No DXGI adapter with LUID " << LuidToString(luid)
                        << ": " << HResultToString(hr);
    return hr;
  }
  DXGI_ADAPTER_DESC1 desc = {};
  adapter->GetDesc1(&desc);
  std::string name;
  for (const WCHAR* c = desc.Description; *c; c++) {
    name.push_back(*c < 0x80 ? static_cast<char>(*c) : '?');
  }
  RTC_LOG(LS_INFO) << "Creating D3D11 device on adapter \"" << name
                   << "\" (LUID " << LuidToString(luid) << ")";

  const D3D_FEATURE_LEVEL feature_levels[] = {
      D3D_FEATURE_LEVEL_11_1,
      D3D_FEATURE_LEVEL_11_0,
      D3D_FEATURE_LEVEL_10_1,
      D3D_FEATURE_LEVEL_10_0,
  };
  const UINT flags =
      D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT;
  D3D11DeviceBundle bundle;
  hr = D3D11CreateDevice(adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr,
                         flags, feature_levels, ARRAYSIZE(feature_levels),
                         D3D11_SDK_VERSION, &bundle.device, nullptr,
                         &bundle.context);
  if (FAILED(hr)) {
    return hr;
  }

  ComPtr<ID3D10Multithread> multithread;
  hr = bundle.device.As(&multithread);
  if (FAILED(hr)) {
    return hr;
  }
  multithread->SetMultithreadProtected(TRUE);

  UINT reset_token = 0;
  hr = MFCreateDXGIDeviceManager(&reset_token, &bundle.manager);
  if (FAILED(hr)) {
    return hr;
  }
  hr = bundle.manager->ResetDevice(bundle.device.Get(), reset_token);
  if (FAILED(hr)) {
    return hr;
  }

  *out = std::move(bundle);
  return S_OK;
}

enum class SharingMode { kPerAdapter, kPerUser, kOff };

SharingMode D3D11SharingMode() {
  static const SharingMode mode = [] {
    std::optional<std::string> value = GetEnvVar("LK_MF_D3D11_SHARING");
    if (value && _stricmp(value->c_str(), "user") == 0) {
      return SharingMode::kPerUser;
    }
    if (value && _stricmp(value->c_str(), "off") == 0) {
      return SharingMode::kOff;
    }
    return SharingMode::kPerAdapter;
  }();
  return mode;
}

struct SharedDeviceEntry {
  uint64_t luid = 0;
  int user = 0;
  std::weak_ptr<const D3D11DeviceBundle> device;
};

std::mutex& SharedDeviceMutex() {
  static std::mutex* mutex = new std::mutex();
  return *mutex;
}

std::vector<SharedDeviceEntry>& SharedDevices() {
  static auto* entries = new std::vector<SharedDeviceEntry>();
  return *entries;
}

uint64_t LuidKey(const LUID& luid) {
  return (static_cast<uint64_t>(static_cast<uint32_t>(luid.HighPart)) << 32) |
         luid.LowPart;
}

}  // namespace

HRESULT AcquireD3D11Device(const LUID* luid,
                           D3D11DeviceUser user,
                           SharedD3D11Device* out) {
  out->reset();
  LUID resolved = {};
  if (luid) {
    resolved = *luid;
  } else {
    HRESULT hr = DefaultAdapterLuid(&resolved);
    if (FAILED(hr)) {
      RTC_LOG(LS_WARNING) << "No default DXGI adapter: " << HResultToString(hr);
      return hr;
    }
  }

  const SharingMode mode = D3D11SharingMode();
  if (mode == SharingMode::kOff) {
    auto bundle = std::make_shared<D3D11DeviceBundle>();
    HRESULT hr = CreateD3D11DeviceBundle(resolved, bundle.get());
    if (SUCCEEDED(hr)) {
      *out = std::move(bundle);
    }
    return hr;
  }

  const uint64_t key = LuidKey(resolved);
  const int user_key =
      mode == SharingMode::kPerUser ? static_cast<int>(user) : 0;
  std::lock_guard<std::mutex> lock(SharedDeviceMutex());
  std::vector<SharedDeviceEntry>& entries = SharedDevices();
  for (auto it = entries.begin(); it != entries.end();) {
    SharedD3D11Device existing = it->device.lock();
    if (!existing) {
      it = entries.erase(it);
      continue;
    }
    if (it->luid == key && it->user == user_key) {
      const HRESULT removed = existing->device->GetDeviceRemovedReason();
      if (SUCCEEDED(removed)) {
        *out = std::move(existing);
        return S_OK;
      }
      RTC_LOG(LS_WARNING) << "Shared D3D11 device on adapter "
                          << LuidToString(resolved) << " was removed ("
                          << HResultToString(removed) << "); creating a new one.";
      it = entries.erase(it);
      continue;
    }
    ++it;
  }

  auto bundle = std::make_shared<D3D11DeviceBundle>();
  HRESULT hr = CreateD3D11DeviceBundle(resolved, bundle.get());
  if (FAILED(hr)) {
    return hr;
  }
  SharedD3D11Device shared = std::move(bundle);
  entries.push_back({key, user_key, shared});
  *out = std::move(shared);
  return S_OK;
}

HRESULT AcquireD3D11DeviceForActivate(IMFActivate* activate,
                                      SharedD3D11Device* out) {
  LUID luid = {};
  bool has_luid = false;
  UINT64 luid_u64 = 0;
  if (SUCCEEDED(activate->GetUINT64(kMftEnumAdapterLuid, &luid_u64))) {
    luid.LowPart = static_cast<DWORD>(luid_u64 & 0xFFFFFFFF);
    luid.HighPart = static_cast<LONG>(luid_u64 >> 32);
    has_luid = true;
  } else if (SUCCEEDED(activate->GetBlob(kMftEnumAdapterLuid,
                                         reinterpret_cast<UINT8*>(&luid),
                                         sizeof(luid), nullptr))) {
    has_luid = true;
  }
  return AcquireD3D11Device(has_luid ? &luid : nullptr,
                            D3D11DeviceUser::kEncoder, out);
}

D3D11GpuFence::D3D11GpuFence(D3D11GpuFence&& other) noexcept
    : context_(std::move(other.context_)),
      fence_(std::move(other.fence_)),
      event_(std::exchange(other.event_, nullptr)),
      last_value_(std::exchange(other.last_value_, 0)) {}

D3D11GpuFence& D3D11GpuFence::operator=(D3D11GpuFence&& other) noexcept {
  if (this != &other) {
    Reset();
    context_ = std::move(other.context_);
    fence_ = std::move(other.fence_);
    event_ = std::exchange(other.event_, nullptr);
    last_value_ = std::exchange(other.last_value_, 0);
  }
  return *this;
}

D3D11GpuFence::~D3D11GpuFence() {
  Reset();
}

bool D3D11GpuFence::Init(const D3D11DeviceBundle& d3d) {
  Reset();
  ComPtr<ID3D11Device5> device5;
  if (SUCCEEDED(d3d.device.As(&device5)) &&
      SUCCEEDED(d3d.context.As(&context_)) &&
      SUCCEEDED(device5->CreateFence(0, D3D11_FENCE_FLAG_NONE,
                                     IID_PPV_ARGS(&fence_)))) {
    event_ = CreateEventW(nullptr, FALSE, FALSE, nullptr);
  }
  if (!event_) {
    Reset();
    return false;
  }
  return true;
}

void D3D11GpuFence::Reset() {
  // A wait that timed out leaves SetEventOnCompletion armed, and the GPU
  // sets the event when it gets there, so the handle is leaked rather than
  // closed and possibly recycled under it.
  if (event_ && (!fence_ || fence_->GetCompletedValue() >= last_value_)) {
    CloseHandle(event_);
  }
  event_ = nullptr;
  fence_.Reset();
  context_.Reset();
  last_value_ = 0;
}

UINT64 D3D11GpuFence::Signal() {
  if (!event_) {
    return 0;
  }
  const UINT64 value = last_value_ + 1;
  if (FAILED(context_->Signal(fence_.Get(), value))) {
    return 0;
  }
  last_value_ = value;
  return value;
}

bool D3D11GpuFence::Completed(UINT64 value) const {
  return !fence_ || fence_->GetCompletedValue() >= value;
}

bool D3D11GpuFence::WaitUntil(UINT64 value,
                              std::chrono::steady_clock::time_point deadline) {
  if (Completed(value)) {
    return true;
  }
  context_->Flush();
  if (FAILED(fence_->SetEventOnCompletion(value, event_))) {
    return false;
  }
  // The event is auto-reset and may carry a late signal from an earlier wait
  // that timed out, so the fence value decides.
  while (!Completed(value)) {
    const auto left = std::chrono::ceil<std::chrono::milliseconds>(
        deadline - std::chrono::steady_clock::now());
    const DWORD wait_ms =
        static_cast<DWORD>(std::max<int64_t>(left.count(), 0));
    if (WaitForSingleObject(event_, wait_ms) != WAIT_OBJECT_0) {
      return Completed(value);
    }
  }
  return true;
}

namespace {

using MFTEnum2Fn = HRESULT(WINAPI*)(GUID,
                                    UINT32,
                                    const MFT_REGISTER_TYPE_INFO*,
                                    const MFT_REGISTER_TYPE_INFO*,
                                    IMFAttributes*,
                                    IMFActivate***,
                                    UINT32*);

// MFTEnum2 (Windows 10 1703+) is resolved at runtime: the SDK only declares
// it for NTDDI_WIN10_RS1+, and a static import would keep the DLL from
// loading on older systems.
MFTEnum2Fn GetMFTEnum2() {
  static const MFTEnum2Fn fn = [] {
    HMODULE module = IsMfPlatAvailable() ? GetModuleHandleW(L"mfplat.dll")
                                         : nullptr;
    return module ? reinterpret_cast<MFTEnum2Fn>(
                        GetProcAddress(module, "MFTEnum2"))
                  : nullptr;
  }();
  return fn;
}

void AppendActivates(IMFActivate** activates,
                     UINT32 count,
                     std::vector<ComPtr<IMFActivate>>* out) {
  for (UINT32 i = 0; activates && i < count; i++) {
    out->emplace_back(activates[i]);
    activates[i]->Release();
  }
  if (activates) {
    CoTaskMemFree(activates);
  }
}

// Private activate attribute carrying the DXGI adapter's PCI vendor id, set
// when MFTEnum2 enumerated the MFT for a known adapter.
constexpr GUID kLkAdapterVendorId = {
    0x6b1f3c2a,
    0x52d4,
    0x4e0b,
    {0x9a, 0x61, 0x3e, 0x0c, 0x7d, 0x55, 0x8f, 0x21}};

bool IsAmdVendor(uint32_t vendor) {
  return vendor == kVendorAmd || vendor == kVendorAmdAlt;
}

const char* VendorName(uint32_t vendor) {
  switch (vendor) {
    case kVendorNvidia:
      return "nvidia";
    case kVendorAmd:
    case kVendorAmdAlt:
      return "amd";
    case kVendorIntel:
      return "intel";
    default:
      return "unknown";
  }
}

bool VendorMatches(uint32_t vendor, const std::string& preferred) {
  if (_stricmp(preferred.c_str(), "nvidia") == 0) {
    return vendor == kVendorNvidia;
  }
  if (_stricmp(preferred.c_str(), "amd") == 0) {
    return IsAmdVendor(vendor);
  }
  if (_stricmp(preferred.c_str(), "intel") == 0) {
    return vendor == kVendorIntel;
  }
  return false;
}

std::vector<ComPtr<IMFActivate>> FilterAndOrderEncoders(
    std::vector<ComPtr<IMFActivate>> activates) {
  // The browser engine turns hardware H.264 CBP encode off on AMD-primary
  // machines after AMD MFT output decoded black for remote viewers. Allowed
  // here by default until that is reproduced; LK_MF_ALLOW_AMD=0 restores
  // parity (AMD MFTs are skipped, so AMD-only machines encode in software).
  const bool allow_amd = !EnvFlagCleared("LK_MF_ALLOW_AMD");
  const std::optional<std::string> preferred =
      GetEnvVar("LK_MF_ENCODER_ADAPTER");
  if (preferred && _stricmp(preferred->c_str(), "nvidia") != 0 &&
      _stricmp(preferred->c_str(), "amd") != 0 &&
      _stricmp(preferred->c_str(), "intel") != 0) {
    RTC_LOG(LS_WARNING) << "Ignoring LK_MF_ENCODER_ADAPTER=\"" << *preferred
                        << "\"; expected nvidia, amd or intel.";
  }

  std::vector<ComPtr<IMFActivate>> kept;
  std::vector<ComPtr<IMFActivate>> rest;
  for (ComPtr<IMFActivate>& activate : activates) {
    const uint32_t vendor = GetActivateVendorId(activate.Get());
    if (!allow_amd && IsAmdVendor(vendor)) {
      RTC_LOG(LS_INFO) << "LK_MF_ALLOW_AMD=0: skipping H264 encoder MFT \""
                       << GetFriendlyName(activate.Get()) << "\"";
      continue;
    }
    if (preferred && VendorMatches(vendor, *preferred)) {
      kept.push_back(std::move(activate));
    } else {
      rest.push_back(std::move(activate));
    }
  }
  if (preferred && kept.empty()) {
    RTC_LOG(LS_WARNING) << "LK_MF_ENCODER_ADAPTER=" << *preferred
                        << ": no matching hardware H264 encoder MFT.";
  }
  kept.insert(kept.end(), std::make_move_iterator(rest.begin()),
              std::make_move_iterator(rest.end()));

  std::string order;
  for (const ComPtr<IMFActivate>& activate : kept) {
    order += (order.empty() ? "" : ", ") + GetFriendlyName(activate.Get()) +
             " [" + VendorName(GetActivateVendorId(activate.Get())) + "]";
  }
  RTC_LOG(LS_INFO) << "H264 encoder MFT order: "
                   << (order.empty() ? "(none)" : order);
  return kept;
}

}  // namespace

uint32_t GetActivateVendorId(IMFActivate* activate) {
  UINT32 vendor = 0;
  if (SUCCEEDED(activate->GetUINT32(kLkAdapterVendorId, &vendor))) {
    return vendor;
  }
  // Hardware MFT registrations carry "VEN_xxxx".
  WCHAR* value = nullptr;
  UINT32 length = 0;
  if (SUCCEEDED(activate->GetAllocatedString(MFT_ENUM_HARDWARE_VENDOR_ID_Attribute,
                                             &value, &length))) {
    if (length > 4 && _wcsnicmp(value, L"VEN_", 4) == 0) {
      vendor = static_cast<uint32_t>(std::wcstoul(value + 4, nullptr, 16));
    }
    CoTaskMemFree(value);
  }
  return vendor;
}

std::vector<ComPtr<IMFActivate>> EnumHardwareH264EncodersUnordered();

std::vector<ComPtr<IMFActivate>> EnumHardwareH264Encoders() {
  return FilterAndOrderEncoders(EnumHardwareH264EncodersUnordered());
}

std::vector<ComPtr<IMFActivate>> EnumHardwareH264EncodersUnordered() {
  std::vector<ComPtr<IMFActivate>> result;
  MFT_REGISTER_TYPE_INFO input_info = {MFMediaType_Video, MFVideoFormat_NV12};
  MFT_REGISTER_TYPE_INFO output_info = {MFMediaType_Video, MFVideoFormat_H264};
  const UINT32 flags = MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER;

  // MFTEnumEx does not say which adapter a hardware MFT belongs to, and a
  // D3D11-aware MFT rejects a device manager on any other adapter (the AMD
  // MFT fails MFT_MESSAGE_SET_D3D_MANAGER with E_FAIL). MFTEnum2 enumerates
  // per adapter, so walk the DXGI adapters in system order and tag each
  // activate with its adapter's LUID.
  MFTEnum2Fn mft_enum2 = GetMFTEnum2();
  ComPtr<IDXGIFactory1> factory;
  if (mft_enum2 && SUCCEEDED(CreateDXGIFactory1(IID_PPV_ARGS(&factory)))) {
    for (UINT i = 0;; i++) {
      ComPtr<IDXGIAdapter1> adapter;
      if (FAILED(factory->EnumAdapters1(i, &adapter))) {
        break;
      }
      DXGI_ADAPTER_DESC1 desc = {};
      if (FAILED(adapter->GetDesc1(&desc)) ||
          (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE)) {
        continue;
      }
      ComPtr<IMFAttributes> attributes;
      if (FAILED(MFCreateAttributes(&attributes, 1)) ||
          FAILED(attributes->SetBlob(
              kMftEnumAdapterLuid,
              reinterpret_cast<const UINT8*>(&desc.AdapterLuid),
              sizeof(desc.AdapterLuid)))) {
        continue;
      }
      IMFActivate** activates = nullptr;
      UINT32 count = 0;
      HRESULT hr = mft_enum2(MFT_CATEGORY_VIDEO_ENCODER, flags, &input_info,
                             &output_info, attributes.Get(), &activates,
                             &count);
      if (FAILED(hr)) {
        RTC_LOG(LS_WARNING) << "MFTEnum2(H264 encoders, adapter "
                            << LuidToString(desc.AdapterLuid)
                            << ") failed: " << HResultToString(hr);
        continue;
      }
      RTC_LOG(LS_INFO) << "Adapter " << LuidToString(desc.AdapterLuid)
                       << " (vendor " << desc.VendorId << "): " << count
                       << " hardware H264 encoder MFT(s)";
      const size_t first = result.size();
      AppendActivates(activates, count, &result);
      for (size_t j = first; j < result.size(); j++) {
        result[j]->SetBlob(kMftEnumAdapterLuid,
                           reinterpret_cast<const UINT8*>(&desc.AdapterLuid),
                           sizeof(desc.AdapterLuid));
        result[j]->SetUINT32(kLkAdapterVendorId, desc.VendorId);
      }
    }
    if (!result.empty()) {
      return result;
    }
  }

  IMFActivate** activates = nullptr;
  UINT32 count = 0;
  HRESULT hr = MFTEnumEx(MFT_CATEGORY_VIDEO_ENCODER, flags, &input_info,
                         &output_info, &activates, &count);
  if (FAILED(hr)) {
    RTC_LOG(LS_WARNING) << "MFTEnumEx(H264 encoders) failed: "
                        << HResultToString(hr);
  }
  AppendActivates(activates, count, &result);
  return result;
}

std::string GetFriendlyName(IMFActivate* activate) {
  std::string name;
  WCHAR* wname = nullptr;
  UINT32 len = 0;
  if (SUCCEEDED(activate->GetAllocatedString(MFT_FRIENDLY_NAME_Attribute,
                                             &wname, &len))) {
    // Log-only string; lossy narrowing is fine.
    for (UINT32 i = 0; i < len; i++) {
      name.push_back(wname[i] < 0x80 ? static_cast<char>(wname[i]) : '?');
    }
    CoTaskMemFree(wname);
  }
  return name;
}

HRESULT PrepareHardwareTransform(IMFActivate* activate,
                                 IMFTransform* transform,
                                 bool* is_async,
                                 SharedD3D11Device* device) {
  *is_async = false;
  device->reset();

  ComPtr<IMFAttributes> attributes;
  HRESULT hr = transform->GetAttributes(&attributes);
  if (FAILED(hr) || !attributes) {
    return S_OK;
  }

  UINT32 async = 0;
  attributes->GetUINT32(MF_TRANSFORM_ASYNC, &async);
  *is_async = async != 0;
  if (*is_async) {
    hr = attributes->SetUINT32(MF_TRANSFORM_ASYNC_UNLOCK, TRUE);
    if (FAILED(hr)) {
      RTC_LOG(LS_ERROR) << "Failed to unlock async MFT: " << HResultToString(hr);
      return hr;
    }
  }

  UINT32 d3d11_aware = 0;
  attributes->GetUINT32(MF_SA_D3D11_AWARE, &d3d11_aware);
  if (!d3d11_aware) {
    return S_OK;
  }

  SharedD3D11Device acquired;
  hr = AcquireD3D11DeviceForActivate(activate, &acquired);
  if (FAILED(hr)) {
    RTC_LOG(LS_WARNING) << "Failed to create D3D11 device for MFT adapter: "
                        << HResultToString(hr);
    return hr;
  }
  hr = transform->ProcessMessage(
      MFT_MESSAGE_SET_D3D_MANAGER,
      reinterpret_cast<ULONG_PTR>(acquired->manager.Get()));
  if (FAILED(hr)) {
    RTC_LOG(LS_WARNING) << "MFT rejected D3D11 device manager: "
                        << HResultToString(hr);
    return hr;
  }
  *device = std::move(acquired);
  return S_OK;
}

}  // namespace livekit_ffi
