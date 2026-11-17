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

#ifndef WEBRTC_MF_D3D11_VIDEO_CONVERTER_H_
#define WEBRTC_MF_D3D11_VIDEO_CONVERTER_H_

#include <d3d11.h>
#include <wrl/client.h>

#include <unordered_map>

namespace webrtc {

// Converts an RGBA or BGRA texture region to an NV12 texture on the GPU with
// the D3D11 video processor, scaling it to the target's size, in BT.601
// limited range like libyuv's ARGBToI420 (the memory path). One per encoder;
// processors and views are cached and rebuilt when a size or format changes.
// Callers hold the device's multithread lock.
class D3D11VideoConverter {
 public:
  HRESULT Convert(ID3D11Device* device,
                  ID3D11DeviceContext* context,
                  ID3D11Texture2D* source,
                  const D3D11_RECT& source_rect,
                  ID3D11Texture2D* target,
                  int target_width,
                  int target_height);
  void Reset();

 private:
  struct Config {
    ID3D11Device* device = nullptr;
    UINT input_width = 0;
    UINT input_height = 0;
    DXGI_FORMAT input_format = DXGI_FORMAT_UNKNOWN;
    UINT output_width = 0;
    UINT output_height = 0;
    bool operator==(const Config&) const = default;
  };

  HRESULT Configure(ID3D11Device* device,
                    ID3D11DeviceContext* context,
                    const Config& config);

  Config config_;
  Microsoft::WRL::ComPtr<ID3D11VideoDevice> video_device_;
  Microsoft::WRL::ComPtr<ID3D11VideoContext> video_context_;
  Microsoft::WRL::ComPtr<ID3D11VideoProcessorEnumerator> enumerator_;
  Microsoft::WRL::ComPtr<ID3D11VideoProcessor> processor_;
  // Keyed by texture; the views keep their textures alive, so the pointers
  // are never reused while cached.
  std::unordered_map<ID3D11Texture2D*,
                     Microsoft::WRL::ComPtr<ID3D11VideoProcessorInputView>>
      inputs_;
  std::unordered_map<ID3D11Texture2D*,
                     Microsoft::WRL::ComPtr<ID3D11VideoProcessorOutputView>>
      outputs_;
};

}  // namespace webrtc

#endif  // WEBRTC_MF_D3D11_VIDEO_CONVERTER_H_
