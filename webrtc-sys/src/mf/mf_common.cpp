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

#include <cstdio>
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

bool EnsureMFStarted() {
  static bool ok = [] {
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

}  // namespace

HRESULT CreateD3D11DeviceBundle(const LUID* luid, D3D11DeviceBundle* out) {
  ComPtr<IDXGIAdapter1> adapter;
  if (luid) {
    HRESULT hr = FindAdapterByLuid(*luid, &adapter);
    if (FAILED(hr)) {
      RTC_LOG(LS_WARNING) << "No DXGI adapter with LUID " << LuidToString(*luid)
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
                     << "\" (LUID " << LuidToString(*luid) << ")";
  } else {
    RTC_LOG(LS_INFO) << "Creating D3D11 device on the default adapter";
  }

  const D3D_FEATURE_LEVEL feature_levels[] = {
      D3D_FEATURE_LEVEL_11_1,
      D3D_FEATURE_LEVEL_11_0,
      D3D_FEATURE_LEVEL_10_1,
      D3D_FEATURE_LEVEL_10_0,
  };
  const UINT flags =
      D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT;
  D3D11DeviceBundle bundle;
  HRESULT hr = D3D11CreateDevice(
      adapter.Get(),
      adapter ? D3D_DRIVER_TYPE_UNKNOWN : D3D_DRIVER_TYPE_HARDWARE, nullptr,
      flags, feature_levels, ARRAYSIZE(feature_levels), D3D11_SDK_VERSION,
      &bundle.device, nullptr, &bundle.context);
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

HRESULT CreateD3D11DeviceBundleForActivate(IMFActivate* activate,
                                           D3D11DeviceBundle* out) {
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
  return CreateD3D11DeviceBundle(has_luid ? &luid : nullptr, out);
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
    HMODULE module = GetModuleHandleW(L"mfplat.dll");
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

}  // namespace

std::vector<ComPtr<IMFActivate>> EnumHardwareH264Encoders() {
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
                                 D3D11DeviceBundle* bundle) {
  *is_async = false;
  *bundle = D3D11DeviceBundle();

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

  D3D11DeviceBundle created;
  hr = CreateD3D11DeviceBundleForActivate(activate, &created);
  if (FAILED(hr)) {
    RTC_LOG(LS_WARNING) << "Failed to create D3D11 device for MFT adapter: "
                        << HResultToString(hr);
    return hr;
  }
  hr = transform->ProcessMessage(
      MFT_MESSAGE_SET_D3D_MANAGER,
      reinterpret_cast<ULONG_PTR>(created.manager.Get()));
  if (FAILED(hr)) {
    RTC_LOG(LS_WARNING) << "MFT rejected D3D11 device manager: "
                        << HResultToString(hr);
    return hr;
  }
  *bundle = std::move(created);
  return S_OK;
}

}  // namespace livekit_ffi
