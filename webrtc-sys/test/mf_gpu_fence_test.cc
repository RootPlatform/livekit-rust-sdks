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

// Standalone unit test for D3D11GpuFence, the fence wait the MediaFoundation
// encoder's upload ring and decoder's readback use instead of a blocking Map.
// It runs on the WARP software rasterizer, so it needs no GPU:
//
//   cl /std:c++20 /EHsc /W4 /I ..\src\mf mf_gpu_fence_test.cc d3d11.lib

#include "mf_gpu_fence.h"

#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <utility>

namespace {

using Clock = std::chrono::steady_clock;
using Microsoft::WRL::ComPtr;
using std::chrono::milliseconds;

int g_failures = 0;

void Expect(bool condition, const char* what) {
  if (!condition) {
    std::fprintf(stderr, "FAIL: %s\n", what);
    ++g_failures;
  }
}

struct WarpDevice {
  ComPtr<ID3D11Device> device;
  ComPtr<ID3D11DeviceContext> context;
};

bool CreateWarpDevice(WarpDevice* out) {
  const D3D_FEATURE_LEVEL levels[] = {D3D_FEATURE_LEVEL_11_1,
                                      D3D_FEATURE_LEVEL_11_0};
  if (FAILED(D3D11CreateDevice(nullptr, D3D_DRIVER_TYPE_WARP, nullptr, 0,
                               levels, ARRAYSIZE(levels), D3D11_SDK_VERSION,
                               &out->device, nullptr, &out->context))) {
    return false;
  }
  ComPtr<ID3D11Multithread> multithread;
  if (SUCCEEDED(out->context.As(&multithread))) {
    multithread->SetMultithreadProtected(TRUE);
  }
  return true;
}

ComPtr<ID3D11Texture2D> CreateTexture(ID3D11Device* device, bool staging) {
  D3D11_TEXTURE2D_DESC desc = {};
  desc.Width = 1920;
  desc.Height = 1080;
  desc.MipLevels = 1;
  desc.ArraySize = 1;
  desc.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
  desc.SampleDesc.Count = 1;
  desc.Usage = staging ? D3D11_USAGE_STAGING : D3D11_USAGE_DEFAULT;
  desc.CPUAccessFlags = staging ? D3D11_CPU_ACCESS_WRITE : 0;
  desc.BindFlags = staging ? 0 : D3D11_BIND_SHADER_RESOURCE;
  ComPtr<ID3D11Texture2D> texture;
  device->CreateTexture2D(&desc, nullptr, &texture);
  return texture;
}

void TestUnavailableFenceNeverWaits() {
  livekit_ffi::D3D11GpuFence fence;
  Expect(!fence, "a fence without Init is unavailable");
  Expect(fence.Signal() == 0, "Signal on an unavailable fence returns 0");
  Expect(fence.Completed(0) && fence.Completed(42),
         "an unavailable fence reports every value complete");
  const Clock::time_point start = Clock::now();
  Expect(fence.WaitUntil(42, Clock::now() + milliseconds(500)),
         "WaitUntil on an unavailable fence succeeds");
  Expect(Clock::now() - start < milliseconds(20),
         "WaitUntil on an unavailable fence returns at once");
}

void TestSignalValuesIncreaseAndComplete(const WarpDevice& warp) {
  livekit_ffi::D3D11GpuFence fence;
  Expect(fence.Init(warp.device.Get(), warp.context.Get()), "Init on WARP");
  Expect(static_cast<bool>(fence), "an initialized fence is available");
  Expect(fence.Completed(0), "value 0 is complete before any signal");
  const UINT64 first = fence.Signal();
  const UINT64 second = fence.Signal();
  Expect(first == 1 && second == 2, "Signal returns 1, then 2");
  Expect(fence.WaitUntil(second, Clock::now() + milliseconds(2000)),
         "WaitUntil sees the second signal complete");
  Expect(fence.Completed(first) && fence.Completed(second),
         "both values complete after the wait");
}

void TestMapAfterFenceDoesNotWait(const WarpDevice& warp) {
  livekit_ffi::D3D11GpuFence fence;
  Expect(fence.Init(warp.device.Get(), warp.context.Get()), "Init on WARP");
  ComPtr<ID3D11Texture2D> staging = CreateTexture(warp.device.Get(), true);
  ComPtr<ID3D11Texture2D> target = CreateTexture(warp.device.Get(), false);
  Expect(staging && target, "textures created");
  if (!staging || !target) {
    return;
  }
  for (int frame = 0; frame < 20; frame++) {
    D3D11_MAPPED_SUBRESOURCE mapped = {};
    HRESULT hr = warp.context->Map(staging.Get(), 0, D3D11_MAP_WRITE,
                                   D3D11_MAP_FLAG_DO_NOT_WAIT, &mapped);
    Expect(SUCCEEDED(hr), "staging maps without waiting once its copy is done");
    if (FAILED(hr)) {
      return;
    }
    static_cast<uint8_t*>(mapped.pData)[0] = static_cast<uint8_t>(frame);
    warp.context->Unmap(staging.Get(), 0);
    warp.context->CopyResource(target.Get(), staging.Get());
    const UINT64 value = fence.Signal();
    Expect(value != 0, "Signal after the copy");
    Expect(fence.WaitUntil(value, Clock::now() + milliseconds(2000)),
           "the copy's fence value completes");
  }
}

void TestWaitTimesOutOnAnUnsignalledValue(const WarpDevice& warp) {
  livekit_ffi::D3D11GpuFence fence;
  Expect(fence.Init(warp.device.Get(), warp.context.Get()), "Init on WARP");
  const UINT64 done = fence.Signal();
  Expect(fence.WaitUntil(done, Clock::now() + milliseconds(2000)),
         "first value completes");
  const Clock::time_point start = Clock::now();
  Expect(!fence.WaitUntil(done + 1, start + milliseconds(50)),
         "WaitUntil for a value nobody signalled times out");
  const auto waited = Clock::now() - start;
  Expect(waited >= milliseconds(45) && waited < milliseconds(1000),
         "the timeout follows the deadline");
  const UINT64 next = fence.Signal();
  Expect(next == done + 1, "the next signal reuses the timed-out value");
  Expect(fence.WaitUntil(next, Clock::now() + milliseconds(2000)),
         "a wait after a timed-out one still completes");
  fence.Reset();
  Expect(!fence, "Reset leaves the fence unavailable");
  Expect(fence.Init(warp.device.Get(), warp.context.Get()),
         "a reset fence can be initialized again");
  Expect(fence.Signal() == 1, "values restart at 1 after Init");
}

void TestMoveKeepsTheSequence(const WarpDevice& warp) {
  livekit_ffi::D3D11GpuFence a;
  Expect(a.Init(warp.device.Get(), warp.context.Get()), "Init on WARP");
  Expect(a.Signal() == 1, "first value");
  livekit_ffi::D3D11GpuFence b = std::move(a);
  Expect(!a && static_cast<bool>(b), "move transfers the fence");
  Expect(a.Signal() == 0, "the moved-from fence no longer signals");
  Expect(b.Signal() == 2, "the moved-to fence continues the sequence");
  livekit_ffi::D3D11GpuFence c;
  c = std::move(b);
  Expect(!b && static_cast<bool>(c), "move assignment transfers the fence");
  const UINT64 value = c.Signal();
  Expect(value == 3, "move assignment keeps the sequence");
  Expect(c.WaitUntil(value, Clock::now() + milliseconds(2000)),
         "the moved fence still completes");
}

}  // namespace

int main() {
  TestUnavailableFenceNeverWaits();
  WarpDevice warp;
  if (!CreateWarpDevice(&warp)) {
    std::fprintf(stderr, "FAIL: could not create a WARP device\n");
    return EXIT_FAILURE;
  }
  TestSignalValuesIncreaseAndComplete(warp);
  TestMapAfterFenceDoesNotWait(warp);
  TestWaitTimesOutOnAnUnsignalledValue(warp);
  TestMoveKeepsTheSequence(warp);

  if (g_failures != 0) {
    std::fprintf(stderr, "%d test(s) failed\n", g_failures);
    return EXIT_FAILURE;
  }
  std::printf("All mf_gpu_fence tests passed\n");
  return EXIT_SUCCESS;
}
