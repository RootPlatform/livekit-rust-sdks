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

#include "livekit/d3d11_texture_buffer.h"

#include <d3d10_1.h>  // ID3D10Multithread

#include <algorithm>
#include <chrono>
#include <mutex>
#include <thread>
#include <unordered_set>
#include <utility>
#include <vector>

#include "api/make_ref_counted.h"
#include "api/video/i420_buffer.h"
#include "rtc_base/logging.h"
#include "third_party/libyuv/include/libyuv/convert.h"

namespace livekit {

using Microsoft::WRL::ComPtr;

namespace {

// Frames a pool lets out at once: what the source, the encoder queue and the
// encoders still hold. Past it CopyFrom fails and the host sends that frame
// through memory instead.
constexpr int kMaxOutstanding = 12;
constexpr size_t kMaxFree = 4;
constexpr size_t kMaxPools = 4;
// How long ToI420 polls a readback before mapping it blocking: a blocking
// Map holds the shared device's lock, which every encoder and decoder waits
// on.
constexpr auto kReadbackPoll = std::chrono::milliseconds(200);

std::mutex& RegistryMutex() {
  static auto* mutex = new std::mutex();
  return *mutex;
}

std::unordered_set<const webrtc::VideoFrameBuffer*>& Registry() {
  static auto* registry = new std::unordered_set<const webrtc::VideoFrameBuffer*>();
  return *registry;
}

struct PoolKey {
  ID3D11Device* device;
  UINT width;
  UINT height;
  DXGI_FORMAT format;
  bool operator==(const PoolKey&) const = default;
};

}  // namespace

class D3D11TexturePool : public std::enable_shared_from_this<D3D11TexturePool> {
 public:
  D3D11TexturePool(ComPtr<ID3D11Device> device, PoolKey key)
      : device_(std::move(device)), key_(key) {}

  const PoolKey& key() const { return key_; }

  ComPtr<ID3D11Texture2D> Take() {
    std::lock_guard<std::mutex> lock(mutex_);
    if (!free_.empty()) {
      ComPtr<ID3D11Texture2D> texture = std::move(free_.back());
      free_.pop_back();
      outstanding_++;
      return texture;
    }
    if (outstanding_ >= kMaxOutstanding) {
      return nullptr;
    }
    D3D11_TEXTURE2D_DESC desc = {};
    desc.Width = key_.width;
    desc.Height = key_.height;
    desc.MipLevels = 1;
    desc.ArraySize = 1;
    desc.Format = key_.format;
    desc.SampleDesc.Count = 1;
    desc.Usage = D3D11_USAGE_DEFAULT;
    desc.BindFlags = D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET;
    ComPtr<ID3D11Texture2D> texture;
    HRESULT hr = device_->CreateTexture2D(&desc, nullptr, &texture);
    if (FAILED(hr)) {
      desc.BindFlags = D3D11_BIND_SHADER_RESOURCE;
      hr = device_->CreateTexture2D(&desc, nullptr, &texture);
    }
    if (FAILED(hr)) {
      RTC_LOG(LS_ERROR) << "D3D11 frame texture " << key_.width << "x"
                        << key_.height << " could not be created: 0x"
                        << std::hex << static_cast<uint32_t>(hr);
      return nullptr;
    }
    outstanding_++;
    return texture;
  }

  void Return(ComPtr<ID3D11Texture2D> texture) {
    std::lock_guard<std::mutex> lock(mutex_);
    outstanding_--;
    if (texture && free_.size() < kMaxFree) {
      free_.push_back(std::move(texture));
    }
  }

  ID3D11Device* device() const { return device_.Get(); }

 private:
  const ComPtr<ID3D11Device> device_;
  const PoolKey key_;
  std::mutex mutex_;
  std::vector<ComPtr<ID3D11Texture2D>> free_;
  int outstanding_ = 0;
};

struct D3D11TextureBuffer::PooledTexture {
  PooledTexture(std::shared_ptr<D3D11TexturePool> pool,
                ComPtr<ID3D11Texture2D> texture,
                DXGI_FORMAT format)
      : pool(std::move(pool)), texture(std::move(texture)), format(format) {}
  ~PooledTexture() { pool->Return(std::move(texture)); }

  std::shared_ptr<D3D11TexturePool> pool;
  ComPtr<ID3D11Texture2D> texture;
  DXGI_FORMAT format;
};

namespace {

std::shared_ptr<D3D11TexturePool> PoolFor(ID3D11Device* device,
                                          UINT width,
                                          UINT height,
                                          DXGI_FORMAT format) {
  static auto* mutex = new std::mutex();
  static auto* pools = new std::vector<std::shared_ptr<D3D11TexturePool>>();
  const PoolKey key{device, width, height, format};
  std::lock_guard<std::mutex> lock(*mutex);
  for (auto it = pools->begin(); it != pools->end(); ++it) {
    if ((*it)->key() == key) {
      std::shared_ptr<D3D11TexturePool> pool = *it;
      pools->erase(it);
      pools->push_back(pool);
      return pool;
    }
  }
  // Textures still out keep their own reference to an evicted pool.
  if (pools->size() >= kMaxPools) {
    pools->erase(pools->begin());
  }
  pools->push_back(std::make_shared<D3D11TexturePool>(ComPtr<ID3D11Device>(device), key));
  return pools->back();
}

class DeviceLock {
 public:
  explicit DeviceLock(ID3D10Multithread* multithread)
      : multithread_(multithread) {
    multithread_->Enter();
  }
  ~DeviceLock() { multithread_->Leave(); }

 private:
  ID3D10Multithread* multithread_;
};

}  // namespace

webrtc::scoped_refptr<D3D11TextureBuffer> D3D11TextureBuffer::CopyFrom(
    ID3D11Texture2D* source,
    int width,
    int height) {
  if (!source || width <= 0 || height <= 0) {
    return nullptr;
  }
  D3D11_TEXTURE2D_DESC desc = {};
  source->GetDesc(&desc);
  if ((desc.Format != DXGI_FORMAT_R8G8B8A8_UNORM &&
       desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM) ||
      desc.SampleDesc.Count != 1 || static_cast<UINT>(width) > desc.Width ||
      static_cast<UINT>(height) > desc.Height) {
    return nullptr;
  }
  ComPtr<ID3D11Device> device;
  source->GetDevice(&device);
  ComPtr<ID3D11DeviceContext> context;
  device->GetImmediateContext(&context);
  ComPtr<ID3D10Multithread> multithread;
  if (FAILED(context.As(&multithread)) ||
      !multithread->GetMultithreadProtected()) {
    return nullptr;
  }
  std::shared_ptr<D3D11TexturePool> pool =
      PoolFor(device.Get(), static_cast<UINT>(width),
              static_cast<UINT>(height), desc.Format);
  ComPtr<ID3D11Texture2D> texture = pool->Take();
  if (!texture) {
    return nullptr;
  }
  auto pooled = std::make_shared<PooledTexture>(pool, texture, desc.Format);
  {
    DeviceLock lock(multithread.Get());
    const D3D11_BOX box = {0, 0, 0, static_cast<UINT>(width),
                           static_cast<UINT>(height), 1};
    context->CopySubresourceRegion(texture.Get(), 0, 0, 0, 0, source, 0, &box);
  }
  return webrtc::make_ref_counted<D3D11TextureBuffer>(
      std::move(pooled), D3D11_RECT{0, 0, width, height}, width, height);
}

D3D11TextureBuffer::D3D11TextureBuffer(std::shared_ptr<PooledTexture> texture,
                                       D3D11_RECT source_rect,
                                       int width,
                                       int height)
    : texture_(std::move(texture)),
      source_rect_(source_rect),
      width_(width),
      height_(height) {
  std::lock_guard<std::mutex> lock(RegistryMutex());
  Registry().insert(this);
}

D3D11TextureBuffer::~D3D11TextureBuffer() {
  std::lock_guard<std::mutex> lock(RegistryMutex());
  Registry().erase(this);
}

const D3D11TextureBuffer* D3D11TextureBuffer::FromNative(
    const webrtc::VideoFrameBuffer* buffer) {
  if (!buffer || buffer->type() != Type::kNative) {
    return nullptr;
  }
  std::lock_guard<std::mutex> lock(RegistryMutex());
  return Registry().count(buffer) > 0
             ? static_cast<const D3D11TextureBuffer*>(buffer)
             : nullptr;
}

ID3D11Device* D3D11TextureBuffer::device() const {
  return texture_->pool->device();
}

ID3D11Texture2D* D3D11TextureBuffer::texture() const {
  return texture_->texture.Get();
}

DXGI_FORMAT D3D11TextureBuffer::format() const {
  return texture_->format;
}

webrtc::scoped_refptr<webrtc::VideoFrameBuffer> D3D11TextureBuffer::CropAndScale(
    int offset_x,
    int offset_y,
    int crop_width,
    int crop_height,
    int scaled_width,
    int scaled_height) {
  const LONG source_width = source_rect_.right - source_rect_.left;
  const LONG source_height = source_rect_.bottom - source_rect_.top;
  auto to_source_x = [&](int x) {
    return source_rect_.left +
           static_cast<LONG>(static_cast<int64_t>(x) * source_width / width_);
  };
  auto to_source_y = [&](int y) {
    return source_rect_.top +
           static_cast<LONG>(static_cast<int64_t>(y) * source_height / height_);
  };
  const D3D11_RECT rect = {to_source_x(offset_x), to_source_y(offset_y),
                           to_source_x(offset_x + crop_width),
                           to_source_y(offset_y + crop_height)};
  return webrtc::make_ref_counted<D3D11TextureBuffer>(texture_, rect,
                                                      scaled_width,
                                                      scaled_height);
}

webrtc::scoped_refptr<webrtc::I420BufferInterface>
D3D11TextureBuffer::ToI420() {
  ID3D11Device* dev = device();
  ComPtr<ID3D11DeviceContext> context;
  dev->GetImmediateContext(&context);
  ComPtr<ID3D10Multithread> multithread;
  if (FAILED(context.As(&multithread))) {
    return nullptr;
  }
  const int region_width = source_rect_.right - source_rect_.left;
  const int region_height = source_rect_.bottom - source_rect_.top;
  if (region_width <= 0 || region_height <= 0) {
    return nullptr;
  }
  D3D11_TEXTURE2D_DESC desc = {};
  desc.Width = static_cast<UINT>(region_width);
  desc.Height = static_cast<UINT>(region_height);
  desc.MipLevels = 1;
  desc.ArraySize = 1;
  desc.Format = format();
  desc.SampleDesc.Count = 1;
  desc.Usage = D3D11_USAGE_STAGING;
  desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
  ComPtr<ID3D11Texture2D> staging;
  if (FAILED(dev->CreateTexture2D(&desc, nullptr, &staging))) {
    return nullptr;
  }
  {
    DeviceLock lock(multithread.Get());
    const D3D11_BOX box = {static_cast<UINT>(source_rect_.left),
                           static_cast<UINT>(source_rect_.top),
                           0,
                           static_cast<UINT>(source_rect_.right),
                           static_cast<UINT>(source_rect_.bottom),
                           1};
    context->CopySubresourceRegion(staging.Get(), 0, 0, 0, 0, texture(), 0,
                                   &box);
    context->Flush();
  }
  D3D11_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = DXGI_ERROR_WAS_STILL_DRAWING;
  const auto deadline = std::chrono::steady_clock::now() + kReadbackPoll;
  while (hr == DXGI_ERROR_WAS_STILL_DRAWING &&
         std::chrono::steady_clock::now() < deadline) {
    hr = context->Map(staging.Get(), 0, D3D11_MAP_READ,
                      D3D11_MAP_FLAG_DO_NOT_WAIT, &mapped);
    if (hr == DXGI_ERROR_WAS_STILL_DRAWING) {
      std::this_thread::sleep_for(std::chrono::milliseconds(1));
    }
  }
  if (hr == DXGI_ERROR_WAS_STILL_DRAWING) {
    hr = context->Map(staging.Get(), 0, D3D11_MAP_READ, 0, &mapped);
  }
  if (FAILED(hr)) {
    RTC_LOG(LS_ERROR) << "D3D11 frame readback failed: 0x" << std::hex
                      << static_cast<uint32_t>(hr);
    return nullptr;
  }
  webrtc::scoped_refptr<webrtc::I420Buffer> i420 =
      webrtc::I420Buffer::Create(region_width, region_height);
  // libyuv names formats by their little-endian word: "ABGR" is R,G,B,A in
  // memory (R8G8B8A8) and "ARGB" is B,G,R,A (B8G8R8A8).
  const auto convert = format() == DXGI_FORMAT_R8G8B8A8_UNORM
                           ? &libyuv::ABGRToI420
                           : &libyuv::ARGBToI420;
  const int result = convert(
      static_cast<const uint8_t*>(mapped.pData),
      static_cast<int>(mapped.RowPitch), i420->MutableDataY(), i420->StrideY(),
      i420->MutableDataU(), i420->StrideU(), i420->MutableDataV(),
      i420->StrideV(), region_width, region_height);
  context->Unmap(staging.Get(), 0);
  if (result != 0) {
    return nullptr;
  }
  if (region_width == width_ && region_height == height_) {
    return i420;
  }
  webrtc::scoped_refptr<webrtc::I420Buffer> scaled =
      webrtc::I420Buffer::Create(width_, height_);
  scaled->ScaleFrom(*i420);
  return scaled;
}

}  // namespace livekit
