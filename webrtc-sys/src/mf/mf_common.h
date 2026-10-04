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

#ifndef WEBRTC_MF_COMMON_H_
#define WEBRTC_MF_COMMON_H_

#include <windows.h>

#include <d3d11.h>
#include <d3d11_4.h>
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <mftransform.h>
#include <strmif.h>  // ICodecAPI
#include <wrl/client.h>

#include <chrono>
#include <cstdint>
#include <memory>
#include <optional>
#include <string>
#include <vector>

namespace livekit_ffi {

using Microsoft::WRL::ComPtr;

// A D3D11 device plus the DXGI device manager that hands it to an MFT.
struct D3D11DeviceBundle {
  ComPtr<ID3D11Device> device;
  ComPtr<ID3D11DeviceContext> context;
  ComPtr<IMFDXGIDeviceManager> manager;
};

using SharedD3D11Device = std::shared_ptr<const D3D11DeviceBundle>;

// Waits for GPU work on a shared, multithread-protected device without
// holding its lock, which a blocking Map keeps for the whole wait. Needs
// ID3D11Device5 (WDDM 2.0+); without it Init() fails and callers poll Map
// with D3D11_MAP_FLAG_DO_NOT_WAIT instead.
class D3D11GpuFence {
 public:
  D3D11GpuFence() = default;
  D3D11GpuFence(D3D11GpuFence&& other) noexcept;
  D3D11GpuFence& operator=(D3D11GpuFence&& other) noexcept;
  ~D3D11GpuFence();

  bool Init(const D3D11DeviceBundle& d3d);
  void Reset();
  explicit operator bool() const { return event_ != nullptr; }

  // Queues a signal behind the work already recorded on the device context;
  // 0 when the fence is unavailable or the signal failed.
  UINT64 Signal();
  // True for 0 and whenever the fence is unavailable.
  bool Completed(UINT64 value) const;
  // Flushes the context, then waits until `value` completes or `deadline`
  // passes.
  bool WaitUntil(UINT64 value, std::chrono::steady_clock::time_point deadline);

 private:
  ComPtr<ID3D11DeviceContext4> context_;
  ComPtr<ID3D11Fence> fence_;
  HANDLE event_ = nullptr;
  UINT64 last_value_ = 0;
};

enum class D3D11DeviceUser { kEncoder, kDecoder };

// A multithread-protected, video-capable D3D11 device and its DXGI device
// manager on the adapter identified by `luid` (the default adapter when
// null), shared by every MF encoder and decoder on that adapter: each device
// costs the NVIDIA driver 37 worker threads. The device is released when the
// last holder drops it, and a removed device is replaced on the next call.
HRESULT AcquireD3D11Device(const LUID* luid,
                           D3D11DeviceUser user,
                           SharedD3D11Device* out);

// Hardware MFTs on multi-adapter systems are bound to one adapter, recorded
// on the activate as MFT_ENUM_ADAPTER_LUID; the device manager handed to the
// MFT must live on that same adapter. No LUID means the default adapter.
HRESULT AcquireD3D11DeviceForActivate(IMFActivate* activate,
                                      SharedD3D11Device* out);

constexpr uint32_t kVendorNvidia = 0x10DE;
constexpr uint32_t kVendorAmd = 0x1002;
constexpr uint32_t kVendorAmdAlt = 0x1022;
constexpr uint32_t kVendorIntel = 0x8086;

// Hardware H264 encoder MFTs accepting NV12, best first. Starts from the
// per-adapter MFT_ENUM_FLAG_SORTANDFILTER order, then drops AMD MFTs when
// LK_MF_ALLOW_AMD=0 and moves the vendor named by
// LK_MF_ENCODER_ADAPTER=nvidia|amd|intel to the front. The factory probe and
// the encoder both use this, so they always pick the same MFT.
std::vector<ComPtr<IMFActivate>> EnumHardwareH264Encoders();

// PCI vendor id of the adapter a hardware MFT belongs to, 0 when unknown.
uint32_t GetActivateVendorId(IMFActivate* activate);

// True when Media Foundation (mfplat.dll) is installed. Windows N/KN without
// the Media Feature Pack lacks it; livekit_ffi.dll delay-loads mfplat.dll, so
// no MF entry point may be called when this is false.
bool IsMfPlatAvailable();

// Reads a process environment variable through the Win32 environment, so a
// value set by the host after this DLL loaded (where the CRT getenv snapshot
// would miss it) is still seen.
std::optional<std::string> GetEnvVar(const char* name);
// "1"/"true"/"yes"/"on" (any case).
bool EnvFlagSet(const char* name);
// "0"/"false"/"no"/"off" (any case).
bool EnvFlagCleared(const char* name);
std::optional<int64_t> GetEnvInt(const char* name);

std::string GetFriendlyName(IMFActivate* activate);

// Unlocks an async MFT (required before any media type can be set) and, when
// the MFT is D3D11-aware, attaches a device manager on the MFT's own adapter.
// `device` is left empty for MFTs that take system-memory input.
HRESULT PrepareHardwareTransform(IMFActivate* activate,
                                 IMFTransform* transform,
                                 bool* is_async,
                                 SharedD3D11Device* device);

// Ensures COM is initialized (MTA) on the calling thread. webrtc invokes the
// encoder/decoder on its own task-queue threads which are not guaranteed to
// have called CoInitializeEx. The initialization is dropped automatically when
// the thread exits. Safe to call repeatedly and from threads that already
// initialized COM in either apartment mode.
bool EnsureComInitialized();

// Process-wide Media Foundation startup. MFStartup is called on first use and
// intentionally never balanced with MFShutdown: encoders/decoders are created
// and destroyed on different webrtc threads throughout the process lifetime,
// and tearing MF down while another thread is mid-create is racy. The OS
// reclaims MF state at process exit. False without mfplat.dll.
bool EnsureMFStarted();

// Formats an HRESULT as "0x8007000E" for log output.
std::string HResultToString(HRESULT hr);

}  // namespace livekit_ffi

#endif  // WEBRTC_MF_COMMON_H_
