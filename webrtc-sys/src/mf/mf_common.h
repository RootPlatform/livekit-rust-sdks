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
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <mftransform.h>
#include <strmif.h>  // ICodecAPI
#include <wrl/client.h>

#include <cstdint>
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

// Creates a multithread-protected, video-capable D3D11 device on the adapter
// identified by `luid` (the default hardware adapter when null) and wraps it
// in an IMFDXGIDeviceManager.
HRESULT CreateD3D11DeviceBundle(const LUID* luid, D3D11DeviceBundle* out);

// Hardware MFTs on multi-adapter systems are bound to one adapter, recorded
// on the activate as MFT_ENUM_ADAPTER_LUID; the device manager handed to the
// MFT must live on that same adapter. No LUID means the default adapter.
HRESULT CreateD3D11DeviceBundleForActivate(IMFActivate* activate,
                                           D3D11DeviceBundle* out);

// Hardware H264 encoder MFTs accepting NV12, best first
// (MFT_ENUM_FLAG_SORTANDFILTER order).
std::vector<ComPtr<IMFActivate>> EnumHardwareH264Encoders();

std::string GetFriendlyName(IMFActivate* activate);

// Unlocks an async MFT (required before any media type can be set) and, when
// the MFT is D3D11-aware, attaches a device manager on the MFT's own adapter.
// `bundle` is left empty for MFTs that take system-memory input.
HRESULT PrepareHardwareTransform(IMFActivate* activate,
                                 IMFTransform* transform,
                                 bool* is_async,
                                 D3D11DeviceBundle* bundle);

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
// reclaims MF state at process exit.
bool EnsureMFStarted();

// Formats an HRESULT as "0x8007000E" for log output.
std::string HResultToString(HRESULT hr);

}  // namespace livekit_ffi

#endif  // WEBRTC_MF_COMMON_H_
