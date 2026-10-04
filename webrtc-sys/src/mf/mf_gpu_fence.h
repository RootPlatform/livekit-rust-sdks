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

#ifndef WEBRTC_MF_GPU_FENCE_H_
#define WEBRTC_MF_GPU_FENCE_H_

#include <windows.h>

#include <d3d11_4.h>
#include <wrl/client.h>

#include <algorithm>
#include <chrono>
#include <cstdint>
#include <utility>

namespace livekit_ffi {

// Waits for GPU work on a shared, multithread-protected device without
// holding its lock, which a blocking Map keeps for the whole wait. Needs
// ID3D11Device5 (WDDM 2.0+); without it Init() fails and callers poll Map
// with D3D11_MAP_FLAG_DO_NOT_WAIT instead.
class D3D11GpuFence {
 public:
  D3D11GpuFence() = default;
  D3D11GpuFence(D3D11GpuFence&& other) noexcept
      : context_(std::move(other.context_)),
        fence_(std::move(other.fence_)),
        event_(std::exchange(other.event_, nullptr)),
        last_value_(std::exchange(other.last_value_, 0)) {}
  D3D11GpuFence& operator=(D3D11GpuFence&& other) noexcept {
    if (this != &other) {
      Reset();
      context_ = std::move(other.context_);
      fence_ = std::move(other.fence_);
      event_ = std::exchange(other.event_, nullptr);
      last_value_ = std::exchange(other.last_value_, 0);
    }
    return *this;
  }
  ~D3D11GpuFence() { Reset(); }

  bool Init(ID3D11Device* device, ID3D11DeviceContext* context) {
    Reset();
    Microsoft::WRL::ComPtr<ID3D11Device5> device5;
    if (SUCCEEDED(device->QueryInterface(IID_PPV_ARGS(&device5))) &&
        SUCCEEDED(context->QueryInterface(IID_PPV_ARGS(&context_))) &&
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

  void Reset() {
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

  explicit operator bool() const { return event_ != nullptr; }

  // Queues a signal behind the work already recorded on the device context;
  // 0 when the fence is unavailable or the signal failed.
  UINT64 Signal() {
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

  // True for 0 and whenever the fence is unavailable.
  bool Completed(UINT64 value) const {
    return !fence_ || fence_->GetCompletedValue() >= value;
  }

  // Flushes the context, then waits until `value` completes or `deadline`
  // passes.
  bool WaitUntil(UINT64 value, std::chrono::steady_clock::time_point deadline) {
    if (Completed(value)) {
      return true;
    }
    context_->Flush();
    if (FAILED(fence_->SetEventOnCompletion(value, event_))) {
      return false;
    }
    // The event is auto-reset and may carry a late signal from an earlier
    // wait that timed out, so the fence value decides.
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

 private:
  Microsoft::WRL::ComPtr<ID3D11DeviceContext4> context_;
  Microsoft::WRL::ComPtr<ID3D11Fence> fence_;
  HANDLE event_ = nullptr;
  UINT64 last_value_ = 0;
};

}  // namespace livekit_ffi

#endif  // WEBRTC_MF_GPU_FENCE_H_
