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

#ifndef WEBRTC_MF_H264_ENCODER_IMPL_H_
#define WEBRTC_MF_H264_ENCODER_IMPL_H_

#include "mf_common.h"

#include <codecapi.h>

#include <deque>
#include <memory>
#include <optional>
#include <string>
#include <vector>

#include "api/environment/environment.h"
#include "api/scoped_refptr.h"
#include "api/task_queue/pending_task_safety_flag.h"
#include "api/video/color_space.h"
#include "api/video/i420_buffer.h"
#include "api/video/video_rotation.h"
#include "api/video_codecs/h264_profile_level_id.h"
#include "api/video_codecs/video_encoder.h"
#include "common_video/h264/h264_bitstream_parser.h"
#include "modules/video_coding/codecs/h264/include/h264.h"

namespace webrtc {

class MFAsyncEventPump;

// H264 encoder on top of a hardware Media Foundation transform (MFT). The
// vendor's driver registers the MFT (Intel QuickSync, AMD VCN, NVIDIA NVENC),
// so a single implementation covers all of them with no third-party
// dependencies. Hardware encoder MFTs are asynchronous; Encode() runs a
// bounded, synchronous event pump so no extra threads are introduced.
class MFH264EncoderImpl : public VideoEncoder {
 public:
  struct LayerConfig {
    int simulcast_idx = 0;
    int width = -1;
    int height = -1;
    bool sending = true;
    bool key_frame_request = false;
    float max_frame_rate = 0;
    uint32_t target_bps = 0;
    uint32_t max_bps = 0;
    bool frame_dropping_on = false;
    int key_frame_interval = 0;

    void SetStreamState(bool send_stream);
  };

  MFH264EncoderImpl(const Environment& env, const SdpVideoFormat& format);
  ~MFH264EncoderImpl() override;

  int32_t InitEncode(const VideoCodec* codec_settings,
                     const Settings& settings) override;

  int32_t RegisterEncodeCompleteCallback(
      EncodedImageCallback* callback) override;

  int32_t Release() override;

  int32_t Encode(const VideoFrame& frame,
                 const std::vector<VideoFrameType>* frame_types) override;

  void SetRates(const RateControlParameters& rc_parameters) override;

  EncoderInfo GetEncoderInfo() const override;

 private:
  // Metadata for a frame that has been fed to the MFT but whose encoded
  // output has not been collected yet, keyed by the MF sample timestamp.
  struct PendingFrameInfo {
    int64_t sample_time_100ns = 0;
    uint32_t rtp_timestamp = 0;
    int64_t ntp_time_ms = 0;
    int64_t render_time_ms = 0;
    VideoRotation rotation = kVideoRotation_0;
    std::optional<webrtc::ColorSpace> color_space;
  };

  // Tries every hardware encoder MFT in preference order and keeps the first
  // one that accepts the configuration.
  int32_t CreateTransform();
  HRESULT ActivateTransform(IMFActivate* activate);
  int32_t ConfigureTransform();
  int32_t ApplyCodecApiSettings(bool log_failures);
  void ReleaseTransform();
  int32_t ReinitTransform();
  HRESULT NegotiateOutputType();
  void CacheSequenceHeader();
  HRESULT CreateInputSample(const I420BufferInterface& buffer,
                            int64_t sample_time_100ns,
                            int64_t duration_100ns,
                            IMFSample** sample_out);
  // D3D11-aware MFTs: uploads I420 into a pooled NV12 texture on the MFT's
  // adapter and wraps it in a DXGI surface buffer.
  HRESULT CreateD3DInputSample(const I420BufferInterface& buffer,
                               int64_t sample_time_100ns,
                               int64_t duration_100ns,
                               IMFSample** sample_out);
  HRESULT AcquireInputTexture(ID3D11Texture2D** texture_out);
  // Runs the async MFT event loop until the requested goals are met: an input
  // credit is available (when `until_need_input`) and at most
  // `until_pending_at_most` frames are in flight. Encoded output that becomes
  // available meanwhile is delivered to the callback. Returns an error when
  // the timeout expires with goals unmet.
  // With `fail_on_timeout` false an unmet goal at the deadline is not an
  // error.
  int32_t PumpEvents(int timeout_ms,
                     bool until_need_input,
                     size_t until_pending_at_most,
                     bool fail_on_timeout = true);
  // Has the event pump post a drain task to the calling (encoder) task queue
  // whenever the MFT signals output, so frames are delivered as soon as the
  // hardware finishes rather than inside the next Encode().
  void EnableOutputDrainTask();
  void DrainAsyncOutput();
  // Collects one encoded output from the MFT; WEBRTC_VIDEO_CODEC_NO_OUTPUT
  // means the transform needs more input.
  int32_t CollectOneOutput();
  // Sync-MFT path: collects outputs until the transform needs more input.
  int32_t CollectOutputsSync();
  int32_t ProcessEncodedFrame(std::vector<uint8_t>& packet);
  PendingFrameInfo TakePendingInfo(int64_t sample_time_100ns);
  void ApplyBitrate(uint32_t bitrate_bps);

  void ReportInit();
  void ReportError();

  const webrtc::Environment& env_;

  livekit_ffi::ComPtr<IMFActivate> activate_;
  livekit_ffi::ComPtr<IMFTransform> transform_;
  livekit_ffi::ComPtr<ICodecAPI> codec_api_;
  livekit_ffi::ComPtr<IMFMediaEventGenerator> event_generator_;
  livekit_ffi::ComPtr<MFAsyncEventPump> event_pump_;
  webrtc::scoped_refptr<PendingTaskSafetyFlag> drain_safety_;
  // Set when the drain task hit an MFT failure outside Encode(); the next
  // Encode() reports it so webrtc can switch encoders.
  bool async_failed_ = false;
  // InitEncode() succeeded and Release() has not been called since. A null
  // transform_ while this is set means a runtime re-init failed.
  bool initialized_ = false;
  int requested_keyframes_ = 0;
  livekit_ffi::D3D11DeviceBundle d3d_;
  livekit_ffi::ComPtr<ID3D11Texture2D> staging_texture_;
  // Input textures handed to the MFT. A texture is reused only once nothing
  // but this pool references it (the MFT released the sample wrapping it).
  std::vector<livekit_ffi::ComPtr<ID3D11Texture2D>> input_textures_;
  bool is_async_ = false;
  DWORD input_stream_id_ = 0;
  DWORD output_stream_id_ = 0;
  int need_input_credits_ = 0;
  std::deque<PendingFrameInfo> pending_frames_;
  PendingFrameInfo pending_output_info_;
  int64_t frame_count_ = 0;
  // Frames accepted by ProcessInput since InitEncode.
  int64_t frames_submitted_ = 0;
  // This encoder holds a hardware session (counted for LK_MF_MAX_SESSIONS).
  bool session_open_ = false;
  std::vector<uint8_t> sequence_header_;
  std::vector<uint8_t> packet_;
  std::string friendly_name_;

  uint32_t active_bitrate_bps_ = 0;
  bool dynamic_bitrate_supported_ = true;
  bool pending_bitrate_reinit_ = false;

  EncodedImageCallback* encoded_image_callback_ = nullptr;
  LayerConfig configuration_;
  EncodedImage encoded_image_;
  VideoCodec codec_;
  bool has_reported_init_ = false;
  bool has_reported_error_ = false;
  webrtc::H264BitstreamParser h264_bitstream_parser_;
  const SdpVideoFormat format_;
  H264Profile profile_ = H264Profile::kProfileConstrainedBaseline;
  H264Level level_ = H264Level::kLevel3_1;
};

}  // namespace webrtc

#endif  // WEBRTC_MF_H264_ENCODER_IMPL_H_
