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

#include "d3d11_video_converter.h"
#include "livekit/d3d11_texture_buffer.h"
#include "mf_common.h"
#include "mf_reinit_policy.h"

#include <codecapi.h>

#include <chrono>
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
class MFInputSamplePool;

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
  // one that accepts the configuration. When none does while sessions are
  // closing, it succeeds without a transform and Encode() drops frames until
  // OpenDeferredSession() can try again after their final release.
  int32_t CreateTransform();
  int32_t OpenSession();
  int32_t OpenDeferredSession();
  HRESULT ActivateTransform(IMFActivate* activate);
  int32_t ConfigureTransform();
  int32_t ApplyCodecApiSettings(bool log_failures);
  // With `defer` the MFT, its session and the device references are torn
  // down on the deferred release thread instead of the calling thread.
  void ReleaseTransform(bool defer);
  int32_t ReinitTransform();
  HRESULT NegotiateOutputType();
  void CacheSequenceHeader();
  // `buffer` is I420 or NV12.
  HRESULT CreateInputSample(const VideoFrameBuffer& buffer,
                            int64_t sample_time_100ns,
                            int64_t duration_100ns,
                            IMFSample** sample_out);
  // D3D11-aware MFTs: uploads the frame into a pooled NV12 texture on the
  // MFT's adapter, wrapped in a tracked DXGI surface sample.
  HRESULT CreateD3DInputSample(const VideoFrameBuffer& buffer,
                               int64_t sample_time_100ns,
                               int64_t duration_100ns,
                               IMFSample** sample_out);
  HRESULT AcquireInputSample(IMFSample** sample_out,
                             ID3D11Texture2D** texture_out);
  // A D3D11TextureBuffer on d3d_'s device: the video processor converts and
  // scales it straight into a pooled NV12 input texture.
  HRESULT CreateGpuInputSample(const livekit::D3D11TextureBuffer& buffer,
                               int64_t sample_time_100ns,
                               int64_t duration_100ns,
                               IMFSample** sample_out);
  // `buffer` at this layer's size in NV12 or I420, reading a native frame
  // back; null when it cannot be converted.
  webrtc::scoped_refptr<VideoFrameBuffer> ToEncoderMemory(
      webrtc::scoped_refptr<VideoFrameBuffer> buffer);
  HRESULT AddStagingSlot(size_t index);
  // Maps the oldest upload slot for writing. A slot whose copy the GPU has
  // not run yet is never mapped with a blocking Map, which would hold the
  // shared device's lock and stall every other encoder and decoder on it:
  // the ring grows to kMaxStagingTextures, then waits on the upload fence.
  HRESULT MapStagingSlot(size_t* slot_out, D3D11_MAPPED_SUBRESOURCE* mapped);
  // VBV buffer size (and, for screen content, max bitrate) for `target_bps`.
  // False when the MFT rejected one of them.
  bool ApplyRateControlBuffer(uint32_t target_bps, bool final_pass);
  void UpdateRateControlBuffer(uint32_t target_bps);
  void RequestReinitOnDrift(uint32_t reference_bps, uint32_t target_bps);
  void CheckRateControlStarvation();
  HRESULT SetRateControl(const GUID& property, UINT32 value);
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
  livekit_ffi::SharedD3D11Device d3d_;
  struct StagingSlot {
    livekit_ffi::ComPtr<ID3D11Texture2D> texture;
    // Signalled on upload_fence_ after the GPU copy that reads `texture`.
    UINT64 copy_fence_value = 0;
  };
  std::vector<StagingSlot> staging_;
  size_t next_staging_ = 0;
  livekit_ffi::D3D11GpuFence upload_fence_;
  livekit_ffi::ComPtr<MFInputSamplePool> input_pool_;
  D3D11VideoConverter converter_;
  // Set once a GPU conversion failed; this encoder then reads D3D11 frames
  // back to memory.
  bool gpu_input_disabled_ = false;
  int64_t gpu_frames_ = 0;
  bool buffer_size_supported_ = true;
  bool max_bitrate_supported_ = true;
  // Capture timestamp of the first frame since InitEncode; sample times are
  // relative to it.
  std::optional<int64_t> first_timestamp_us_;
  std::optional<int64_t> last_sample_time_100ns_;
  bool is_async_ = false;
  DWORD input_stream_id_ = 0;
  DWORD output_stream_id_ = 0;
  int need_input_credits_ = 0;
  std::deque<PendingFrameInfo> pending_frames_;
  PendingFrameInfo pending_output_info_;
  int64_t frame_count_ = 0;
  // Frames accepted by ProcessInput since InitEncode.
  int64_t frames_submitted_ = 0;
  // When the MFT stopped making progress (no input request and no output
  // since); unset while it keeps up. PumpEvents drops frames until it has been
  // stuck for kStallFallbackMs, and only then falls back to software.
  std::optional<std::chrono::steady_clock::time_point> stalled_since_;
  int64_t frames_dropped_slow_ = 0;
  livekit_ffi::LogRepeatLimiter slow_mft_log_;
  // True when a test hook (SetMfEncoderStallForTest) holds this encoder's MFT
  // events back; PumpEvents then sleeps `wait` and reports a timeout.
  bool StallForTest(std::chrono::steady_clock::duration wait);
  // This encoder holds a hardware session (counted for LK_MF_MAX_SESSIONS).
  bool session_open_ = false;
  // Set while the session waits for closing sessions; past it, the encoder
  // falls back to software.
  std::optional<int64_t> reopen_deadline_ms_;
  std::vector<uint8_t> sequence_header_;
  std::vector<uint8_t> packet_;
  std::string friendly_name_;

  uint32_t active_bitrate_bps_ = 0;
  bool dynamic_bitrate_supported_ = true;
  bool pending_bitrate_reinit_ = false;
  // Cleared when the MFT rejects a runtime VBV / max bitrate update; kept
  // across re-inits so they are not retried.
  bool dynamic_rc_buffer_supported_ = true;
  livekit_ffi::EncoderReinitPolicy reinit_policy_;
  // From InitEncode's max frame rate; SetRates overwrites
  // configuration_.max_frame_rate with the input frame rate, which drops
  // towards 1 fps on static screen content.
  uint32_t max_fps_ = 30;
  uint32_t vbv_ms_ = 100;
  bool transform_configured_ = false;
  std::optional<uint32_t> fault_mean_bps_;
  std::optional<uint32_t> fault_max_bps_;
  std::optional<uint32_t> fault_buffer_bits_;
  // Lines that repeat for as long as their cause lasts (every frame or every
  // SetRates).
  livekit_ffi::LogRepeatLimiter staging_wait_log_;
  livekit_ffi::LogRepeatLimiter dropped_frame_log_;
  livekit_ffi::LogRepeatLimiter missing_info_log_;
  livekit_ffi::LogRepeatLimiter uninitialized_rates_log_;
  livekit_ffi::LogRepeatLimiter invalid_rate_log_;

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

#if defined(ROOTAPP_HARNESS)
namespace livekit_ffi {
// Harness only: on the calling thread, from the encoder's `after_frames`th
// submitted frame, MFT events are held back for `stall_ms` as if the GPU were
// busy. stall_ms 0 turns it off.
void SetMfEncoderStallForTest(int64_t after_frames, int stall_ms);
// Harness only: frames MF encoders converted on the GPU on the calling thread.
int64_t MfEncoderGpuFramesForTest();
// Harness only: frames every MF encoder in the process converted on the GPU.
int64_t MfEncoderGpuFramesTotalForTest();
// Harness only: the frame buffer types MF encoders were given ("D3D11" for
// D3D11TextureBuffer), and why one was not converted on the GPU.
std::string MfEncoderFrameTypesForTest();
// Harness only: why the last D3D11 frame on this thread was not converted on
// the GPU, then cleared; empty when it was.
std::string MfEncoderGpuNoteForTest();
}  // namespace livekit_ffi
#endif

#endif  // WEBRTC_MF_H264_ENCODER_IMPL_H_
