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

#ifndef LIVEKIT_D3D11_TEXTURE_BUFFER_H_
#define LIVEKIT_D3D11_TEXTURE_BUFFER_H_

#if defined(_WIN32)

#include <d3d11.h>
#include <dxgi.h>
#include <wrl/client.h>

#include <memory>

#include "api/scoped_refptr.h"
#include "api/video/video_frame_buffer.h"

namespace livekit {

class D3D11TexturePool;

// A frame held in a D3D11 texture (RGBA or BGRA, 8 bits a channel) on the
// host's capture device, for the MF encoder to convert and scale on the GPU
// with no CPU round trip. CopyFrom copies the host's texture into a pooled
// one, so the host may reuse its texture as soon as the call returns; the
// pooled texture goes back to its pool when the last buffer referencing it is
// destroyed. CropAndScale is lazy (it only narrows the source rectangle and
// sets the output size), and ToI420 reads the region back for consumers that
// need memory, such as a software encoder after a fallback.
class D3D11TextureBuffer : public webrtc::VideoFrameBuffer {
 public:
  // Null when `source` is not on a multithread-protected device, its format
  // is not R8G8B8A8_UNORM or B8G8R8A8_UNORM, the region does not fit, or the
  // pool is out of textures (too many frames still queued downstream).
  static webrtc::scoped_refptr<D3D11TextureBuffer> CopyFrom(
      ID3D11Texture2D* source,
      int width,
      int height);

  // The D3D11TextureBuffer behind `buffer`, or null. libwebrtc is built
  // without RTTI, so live buffers are recognised through a registry.
  static const D3D11TextureBuffer* FromNative(
      const webrtc::VideoFrameBuffer* buffer);

  Type type() const override { return Type::kNative; }
  int width() const override { return width_; }
  int height() const override { return height_; }
  webrtc::scoped_refptr<webrtc::I420BufferInterface> ToI420() override;
  webrtc::scoped_refptr<webrtc::VideoFrameBuffer> CropAndScale(
      int offset_x,
      int offset_y,
      int crop_width,
      int crop_height,
      int scaled_width,
      int scaled_height) override;

  ID3D11Device* device() const;
  ID3D11Texture2D* texture() const;
  DXGI_FORMAT format() const;
  // The region of texture() this buffer shows, in texture pixels; width() x
  // height() is the size it is to be scaled to.
  const D3D11_RECT& source_rect() const { return source_rect_; }

  struct PooledTexture;

  D3D11TextureBuffer(std::shared_ptr<PooledTexture> texture,
                     D3D11_RECT source_rect,
                     int width,
                     int height);
  ~D3D11TextureBuffer() override;

 private:
  std::shared_ptr<PooledTexture> texture_;
  D3D11_RECT source_rect_;
  int width_;
  int height_;
};

}  // namespace livekit

#endif  // defined(_WIN32)

#endif  // LIVEKIT_D3D11_TEXTURE_BUFFER_H_
