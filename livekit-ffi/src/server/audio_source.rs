// Copyright 2025 LiveKit, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::{borrow::Cow, slice};

use livekit::webrtc::prelude::*;
use tokio::sync::mpsc;

use super::FfiHandle;
use crate::{proto, server, FfiError, FfiHandleId, FfiResult};

pub struct FfiAudioSource {
    pub handle_id: FfiHandleId,
    pub source_type: proto::AudioSourceType,
    pub source: RtcAudioSource,
    capture_tx: Option<mpsc::UnboundedSender<CaptureJob>>,
}

impl FfiHandle for FfiAudioSource {}

struct CaptureJob {
    frame: AudioFrame<'static>,
    async_id: u64,
}

impl FfiAudioSource {
    pub fn setup(
        server: &'static server::FfiServer,
        new_source: proto::NewAudioSourceRequest,
    ) -> FfiResult<proto::OwnedAudioSource> {
        let source_type = new_source.r#type();
        #[allow(unreachable_patterns)]
        let source_inner = match source_type {
            #[cfg(not(target_arch = "wasm32"))]
            proto::AudioSourceType::AudioSourceNative => {
                use livekit::webrtc::audio_source::native::NativeAudioSource;

                let audio_source = NativeAudioSource::new(
                    new_source.options.map(Into::into).unwrap_or_default(),
                    new_source.sample_rate.unwrap_or(48000),
                    new_source.num_channels.unwrap_or(1),
                    new_source.queue_size_ms.unwrap_or(1000),
                );
                RtcAudioSource::Native(audio_source)
            }
            #[cfg(not(target_arch = "wasm32"))]
            proto::AudioSourceType::AudioSourcePlatform => {
                // Platform ADM-based source - captures from microphone automatically
                // PlatformAudio must be created first to enable ADM recording

                // If options and platform_audio_handle are provided, configure audio processing
                if let (Some(ref options), Some(handle)) =
                    (&new_source.options, new_source.platform_audio_handle)
                {
                    if let Ok(ffi_audio) =
                        server.retrieve_handle::<super::platform_audio::FfiPlatformAudio>(handle)
                    {
                        let processing_options = livekit::AudioProcessingOptions {
                            echo_cancellation: options.echo_cancellation,
                            noise_suppression: options.noise_suppression,
                            auto_gain_control: options.auto_gain_control,
                            prefer_hardware_processing: options.prefer_hardware.unwrap_or(true),
                        };
                        if let Err(e) =
                            ffi_audio.audio.configure_audio_processing(processing_options)
                        {
                            log::warn!(
                                "Failed to configure audio processing for platform source: {}",
                                e
                            );
                        }
                    }
                }

                RtcAudioSource::Device
            }
            _ => return Err(FfiError::InvalidRequest("unsupported audio source type".into())),
        };

        #[allow(unreachable_patterns)]
        let capture_tx = match source_inner {
            #[cfg(not(target_arch = "wasm32"))]
            RtcAudioSource::Native(ref native) => {
                let (tx, rx) = mpsc::unbounded_channel();
                let handle =
                    server.audio_runtime.spawn(Self::capture_task(server, native.clone(), rx));
                server.watch_panic(handle);
                Some(tx)
            }
            _ => None,
        };

        let handle_id = server.next_id();
        let source = Self { handle_id, source_type, source: source_inner, capture_tx };

        let info = proto::AudioSourceInfo::from(&source);
        server.store_handle(source.handle_id, source);

        Ok(proto::OwnedAudioSource { handle: proto::FfiOwnedHandle { id: handle_id }, info: info })
    }

    /// Captures run one after another on a single task per source, so frames reach the source in
    /// the order they were submitted. A task per request can run out of order on the
    /// multi-threaded runtime even with one worker, which swaps adjacent 10 ms frames.
    #[cfg(not(target_arch = "wasm32"))]
    async fn capture_task(
        server: &'static server::FfiServer,
        source: livekit::webrtc::audio_source::native::NativeAudioSource,
        mut jobs: mpsc::UnboundedReceiver<CaptureJob>,
    ) {
        while let Some(job) = jobs.recv().await {
            let res = source.capture_frame(&job.frame).await;
            send_capture_callback(server, job.async_id, res.err().map(|e| e.to_string()));
        }
    }

    pub fn clear_buffer(&self) {
        match self.source {
            #[cfg(not(target_arch = "wasm32"))]
            RtcAudioSource::Native(ref source) => source.clear_buffer(),
            _ => {}
        }
    }

    pub fn capture_frame(
        &self,
        server: &'static server::FfiServer,
        capture: proto::CaptureAudioFrameRequest,
    ) -> FfiResult<proto::CaptureAudioFrameResponse> {
        let buffer = capture.buffer;
        let async_id = server.resolve_async_id(capture.request_async_id);

        let Some(capture_tx) = self.capture_tx.as_ref() else {
            return Ok(proto::CaptureAudioFrameResponse { async_id });
        };

        let data = unsafe {
            let len = buffer.num_channels * buffer.samples_per_channel;
            slice::from_raw_parts(buffer.data_ptr as *const i16, len as usize)
        }
        .to_vec();

        let frame = AudioFrame {
            data: Cow::Owned(data),
            sample_rate: buffer.sample_rate,
            num_channels: buffer.num_channels,
            samples_per_channel: buffer.samples_per_channel,
        };
        if capture_tx.send(CaptureJob { frame, async_id }).is_err() {
            server.async_runtime.spawn(async move {
                send_capture_callback(server, async_id, Some("audio capture task stopped".into()));
            });
        }

        Ok(proto::CaptureAudioFrameResponse { async_id })
    }
}

fn send_capture_callback(server: &'static server::FfiServer, async_id: u64, error: Option<String>) {
    if let Err(e) = server.send_event(proto::CaptureAudioFrameCallback { async_id, error }.into()) {
        log::error!("[AUDIO_CAPTURE] Failed to send callback async_id={}: {}", async_id, e);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    use super::FfiAudioSource;
    use crate::{proto, server::FfiConfig, FFI_SERVER};

    #[test]
    fn back_to_back_captures_complete_in_submission_order() {
        let completed = Arc::new(Mutex::new(Vec::new()));
        let sink = completed.clone();
        FFI_SERVER.setup(FfiConfig {
            callback_fn: Arc::new(move |event| {
                if let Some(proto::ffi_event::Message::CaptureAudioFrame(cb)) = event.message {
                    sink.lock().unwrap().push((cb.async_id, cb.error));
                }
            }),
            capture_logs: false,
            sdk: "test".into(),
            sdk_version: "0".into(),
        });

        let owned = FfiAudioSource::setup(
            &FFI_SERVER,
            proto::NewAudioSourceRequest {
                r#type: proto::AudioSourceType::AudioSourceNative as i32,
                sample_rate: Some(48000),
                num_channels: Some(1),
                queue_size_ms: Some(0),
                ..Default::default()
            },
        )
        .unwrap();

        let samples = vec![0i16; 480];
        let base = 1u64 << 40;
        let count = 3000u64;
        for burst in 0..count / 10 {
            for i in 0..10 {
                let source = FFI_SERVER.retrieve_handle::<FfiAudioSource>(owned.handle.id).unwrap();
                source
                    .capture_frame(
                        &FFI_SERVER,
                        proto::CaptureAudioFrameRequest {
                            source_handle: owned.handle.id,
                            buffer: proto::AudioFrameBufferInfo {
                                data_ptr: samples.as_ptr() as u64,
                                num_channels: 1,
                                sample_rate: 48000,
                                samples_per_channel: 480,
                            },
                            request_async_id: Some(base + burst * 10 + i),
                        },
                    )
                    .unwrap();
            }
        }

        let deadline = Instant::now() + Duration::from_secs(10);
        while completed.lock().unwrap().len() < count as usize && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        FFI_SERVER.drop_handle(owned.handle.id);

        let completed = completed.lock().unwrap();
        assert_eq!(completed.len(), count as usize);
        assert!(completed.iter().all(|(_, error)| error.is_none()));
        let first_out_of_order =
            completed.iter().enumerate().find(|(i, (id, _))| *id != base + *i as u64);
        assert!(first_out_of_order.is_none(), "out of order at {:?}", first_out_of_order);
    }
}
