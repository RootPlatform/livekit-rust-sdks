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

#include "d3d11_video_converter.h"

#include <d3d11_1.h>

#include "rtc_base/logging.h"

namespace webrtc {

using Microsoft::WRL::ComPtr;

namespace {

// Views for this many distinct textures are kept; the pools on both sides
// hold far fewer, so this only trims after size changes.
constexpr size_t kMaxCachedViews = 32;

}  // namespace

void D3D11VideoConverter::Reset() {
  inputs_.clear();
  outputs_.clear();
  processor_.Reset();
  enumerator_.Reset();
  video_context_.Reset();
  video_device_.Reset();
  config_ = {};
}

HRESULT D3D11VideoConverter::Configure(ID3D11Device* device,
                                       ID3D11DeviceContext* context,
                                       const Config& config) {
  if (processor_ && config == config_) {
    return S_OK;
  }
  if (config.device != config_.device) {
    Reset();
    HRESULT hr = device->QueryInterface(IID_PPV_ARGS(&video_device_));
    if (FAILED(hr)) {
      return hr;
    }
    hr = context->QueryInterface(IID_PPV_ARGS(&video_context_));
    if (FAILED(hr)) {
      video_device_.Reset();
      return hr;
    }
  }
  processor_.Reset();
  enumerator_.Reset();
  inputs_.clear();
  outputs_.clear();
  config_ = {};

  D3D11_VIDEO_PROCESSOR_CONTENT_DESC desc = {};
  desc.InputFrameFormat = D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE;
  desc.InputWidth = config.input_width;
  desc.InputHeight = config.input_height;
  desc.OutputWidth = config.output_width;
  desc.OutputHeight = config.output_height;
  desc.InputFrameRate = {30, 1};
  desc.OutputFrameRate = {30, 1};
  desc.Usage = D3D11_VIDEO_USAGE_OPTIMAL_SPEED;
  HRESULT hr = video_device_->CreateVideoProcessorEnumerator(&desc, &enumerator_);
  if (FAILED(hr)) {
    return hr;
  }
  UINT input_support = 0;
  UINT output_support = 0;
  if (FAILED(enumerator_->CheckVideoProcessorFormat(config.input_format,
                                                    &input_support)) ||
      !(input_support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT) ||
      FAILED(enumerator_->CheckVideoProcessorFormat(DXGI_FORMAT_NV12,
                                                    &output_support)) ||
      !(output_support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT)) {
    enumerator_.Reset();
    return DXGI_ERROR_UNSUPPORTED;
  }
  hr = video_device_->CreateVideoProcessor(enumerator_.Get(), 0, &processor_);
  if (FAILED(hr)) {
    enumerator_.Reset();
    return hr;
  }
  video_context_->VideoProcessorSetStreamFrameFormat(
      processor_.Get(), 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
  video_context_->VideoProcessorSetStreamAutoProcessingMode(processor_.Get(), 0,
                                                            FALSE);
  ComPtr<ID3D11VideoContext1> context1;
  if (SUCCEEDED(video_context_.As(&context1))) {
    context1->VideoProcessorSetStreamColorSpace1(
        processor_.Get(), 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
    context1->VideoProcessorSetOutputColorSpace1(
        processor_.Get(), DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601);
  } else {
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE input = {};
    input.RGB_Range = 0;
    D3D11_VIDEO_PROCESSOR_COLOR_SPACE output = {};
    output.YCbCr_Matrix = 0;
    output.Nominal_Range = D3D11_VIDEO_PROCESSOR_NOMINAL_RANGE_16_235;
    video_context_->VideoProcessorSetStreamColorSpace(processor_.Get(), 0,
                                                      &input);
    video_context_->VideoProcessorSetOutputColorSpace(processor_.Get(),
                                                      &output);
  }
  config_ = config;
  RTC_LOG(LS_INFO) << "D3D11 video processor ready: " << config.input_width
                   << "x" << config.input_height << " format "
                   << static_cast<int>(config.input_format) << " -> NV12 "
                   << config.output_width << "x" << config.output_height;
  return S_OK;
}

HRESULT D3D11VideoConverter::Convert(ID3D11Device* device,
                                     ID3D11DeviceContext* context,
                                     ID3D11Texture2D* source,
                                     const D3D11_RECT& source_rect,
                                     ID3D11Texture2D* target,
                                     int target_width,
                                     int target_height) {
  D3D11_TEXTURE2D_DESC source_desc = {};
  source->GetDesc(&source_desc);
  const Config config{device,
                      source_desc.Width,
                      source_desc.Height,
                      source_desc.Format,
                      static_cast<UINT>(target_width),
                      static_cast<UINT>(target_height)};
  HRESULT hr = Configure(device, context, config);
  if (FAILED(hr)) {
    return hr;
  }
  if (inputs_.size() > kMaxCachedViews) {
    inputs_.clear();
  }
  if (outputs_.size() > kMaxCachedViews) {
    outputs_.clear();
  }

  ComPtr<ID3D11VideoProcessorInputView>& input = inputs_[source];
  if (!input) {
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC view = {};
    view.ViewDimension = D3D11_VPIV_DIMENSION_TEXTURE2D;
    hr = video_device_->CreateVideoProcessorInputView(source, enumerator_.Get(),
                                                      &view, &input);
    if (FAILED(hr)) {
      inputs_.erase(source);
      return hr;
    }
  }
  ComPtr<ID3D11VideoProcessorOutputView>& output = outputs_[target];
  if (!output) {
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC view = {};
    view.ViewDimension = D3D11_VPOV_DIMENSION_TEXTURE2D;
    hr = video_device_->CreateVideoProcessorOutputView(
        target, enumerator_.Get(), &view, &output);
    if (FAILED(hr)) {
      outputs_.erase(target);
      return hr;
    }
  }

  const RECT source_region = {source_rect.left, source_rect.top,
                              source_rect.right, source_rect.bottom};
  const RECT target_region = {0, 0, target_width, target_height};
  video_context_->VideoProcessorSetStreamSourceRect(processor_.Get(), 0, TRUE,
                                                    &source_region);
  video_context_->VideoProcessorSetStreamDestRect(processor_.Get(), 0, TRUE,
                                                  &target_region);
  video_context_->VideoProcessorSetOutputTargetRect(processor_.Get(), TRUE,
                                                    &target_region);
  D3D11_VIDEO_PROCESSOR_STREAM stream = {};
  stream.Enable = TRUE;
  stream.pInputSurface = input.Get();
  return video_context_->VideoProcessorBlt(processor_.Get(), output.Get(), 0, 1,
                                           &stream);
}

}  // namespace webrtc
