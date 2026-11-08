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

use std::fmt::Debug;

use crate::{
    imp::rtp_sender as imp_rs, media_stream_track::MediaStreamTrack, rtp_parameters::RtpParameters,
    stats::RtcStats, RtcError,
};

/// Preferred backend for video encoding on an [`RtpSender`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum VideoEncoderBackend {
    /// Use the SDK's default encoder selection.
    #[default]
    Auto,
    /// Prefer a software encoder.
    Software,
    /// Prefer any available hardware encoder.
    Hardware,
    /// Prefer NVIDIA NVENC when available.
    Nvenc,
    /// Prefer VAAPI when available.
    Vaapi,
    /// Prefer VideoToolbox on Apple platforms when available.
    VideoToolbox,
    /// Pass pre-encoded frames through without encoding raw video frames.
    PreEncoded,
}

impl VideoEncoderBackend {
    /// Returns the video encoder backends available in this process.
    ///
    /// The result reflects the current platform, build flags, and runtime
    /// hardware capability checks.
    ///
    /// ```
    /// use libwebrtc::rtp_sender::VideoEncoderBackend;
    ///
    /// let backends: Vec<_> = VideoEncoderBackend::list_available().into_iter().collect();
    /// assert!(backends.contains(&VideoEncoderBackend::Auto));
    /// assert!(backends.contains(&VideoEncoderBackend::Software));
    /// ```
    pub fn list_available() -> impl IntoIterator<Item = VideoEncoderBackend> {
        imp_rs::video_encoder_backend_list()
    }
}

#[derive(Clone)]
pub struct RtpSender {
    pub(crate) handle: imp_rs::RtpSender,
}

impl RtpSender {
    pub fn track(&self) -> Option<MediaStreamTrack> {
        self.handle.track()
    }

    pub async fn get_stats(&self) -> Result<Vec<RtcStats>, RtcError> {
        self.handle.get_stats().await
    }

    pub fn set_track(&self, track: Option<MediaStreamTrack>) -> Result<(), RtcError> {
        self.handle.set_track(track)
    }

    pub fn parameters(&self) -> RtpParameters {
        self.handle.parameters()
    }

    pub fn set_parameters(&self, parameters: RtpParameters) -> Result<(), RtcError> {
        self.handle.set_parameters(parameters)
    }

    /// Sets the preferred video encoder backend for this sender.
    ///
    /// If the requested backend is unavailable, libwebrtc falls back to another
    /// compatible encoder.
    pub fn set_video_encoder_backend(&self, backend: VideoEncoderBackend) {
        self.handle.set_video_encoder_backend(backend)
    }

    /// Destroys the video encoder of a sender whose track was removed.
    ///
    /// A transceiver that stopped sending keeps its send stream, and with it the encoder
    /// instances (hardware sessions included), until the PeerConnection closes. This recreates
    /// the send stream without a source, so no encoder exists until a track is set again.
    pub fn release_video_encoder(&self) {
        self.handle.release_video_encoder()
    }
}

impl Debug for RtpSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RtpReceiver").field("cname", &self.parameters().rtcp.cname).finish()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::time::{Duration, Instant};

    use tokio::sync::mpsc;

    use crate::{
        peer_connection_factory::native::PeerConnectionFactoryExt, prelude::*, stats::RtcStats,
        video_source::native::NativeVideoSource,
    };

    async fn encoded_sizes(pc: &PeerConnection) -> Vec<(u32, u32)> {
        pc.get_stats()
            .await
            .unwrap()
            .into_iter()
            .filter_map(|stats| match stats {
                RtcStats::OutboundRtp(o) if o.stream.kind == "video" => {
                    Some((o.outbound.frame_width, o.outbound.frame_height))
                }
                _ => None,
            })
            .collect()
    }

    fn forward_candidates(from: &PeerConnection, to: PeerConnection) {
        let (tx, mut rx) = mpsc::unbounded_channel::<IceCandidate>();
        from.on_ice_candidate(Some(Box::new(move |candidate| {
            let _ = tx.send(candidate);
        })));
        tokio::spawn(async move {
            while let Some(candidate) = rx.recv().await {
                let _ = to.add_ice_candidate(candidate).await;
            }
        });
    }

    #[tokio::test]
    async fn release_video_encoder_replaces_the_send_stream_of_a_removed_track() {
        let factory = PeerConnectionFactory::default();
        let alice = factory.create_peer_connection(RtcConfiguration::default()).unwrap();
        let bob = factory.create_peer_connection(RtcConfiguration::default()).unwrap();
        forward_candidates(&alice, bob.clone());
        forward_candidates(&bob, alice.clone());

        let source = NativeVideoSource::new(VideoResolution { width: 320, height: 240 }, false);
        let track = factory.create_video_track("camera", source.clone());
        let transceiver = alice
            .add_transceiver(
                MediaStreamTrack::Video(track),
                RtpTransceiverInit {
                    direction: RtpTransceiverDirection::SendOnly,
                    stream_ids: Vec::new(),
                    send_encodings: Vec::new(),
                },
            )
            .unwrap();
        let vp8 = factory
            .get_rtp_sender_capabilities(MediaType::Video)
            .codecs
            .into_iter()
            .filter(|codec| codec.mime_type.eq_ignore_ascii_case("video/vp8"))
            .collect();
        transceiver.set_codec_preferences(vp8).unwrap();

        let offer = alice.create_offer(OfferOptions::default()).await.unwrap();
        alice.set_local_description(offer.clone()).await.unwrap();
        bob.set_remote_description(offer).await.unwrap();
        let answer = bob.create_answer(AnswerOptions::default()).await.unwrap();
        bob.set_local_description(answer.clone()).await.unwrap();
        alice.set_remote_description(answer).await.unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut timestamp_us = 0;
        while encoded_sizes(&alice).await != [(320, 240)] {
            assert!(Instant::now() < deadline, "the video encoder never produced a frame");
            timestamp_us += 33_333;
            source.capture_frame(&VideoFrame {
                rotation: VideoRotation::VideoRotation0,
                timestamp_us,
                frame_metadata: None,
                buffer: I420Buffer::new(320, 240),
            });
            tokio::time::sleep(Duration::from_millis(33)).await;
        }

        // The stats cache lives 50 ms; each read below must see a fresh report.
        let sender = transceiver.sender();
        alice.remove_track(sender.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            encoded_sizes(&alice).await,
            [(320, 240)],
            "the send stream survives remove_track"
        );

        sender.release_video_encoder();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(encoded_sizes(&alice).await, [(0, 0)], "a new send stream without an encoder");

        alice.close();
        bob.close();
    }

    async fn frames_encoded(pc: &PeerConnection) -> u32 {
        pc.get_stats()
            .await
            .unwrap()
            .into_iter()
            .find_map(|stats| match stats {
                RtcStats::OutboundRtp(o) if o.stream.kind == "video" => {
                    Some(o.outbound.frames_encoded)
                }
                _ => None,
            })
            .unwrap_or(0)
    }

    // A sending sender takes new caps, a degradation preference and a content hint
    // without renegotiation, and keeps encoding.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_sender_takes_new_encoding_and_content_hint() {
        use crate::video_track::VideoContentHint;

        let factory = PeerConnectionFactory::default();
        let alice = factory.create_peer_connection(RtcConfiguration::default()).unwrap();
        let bob = factory.create_peer_connection(RtcConfiguration::default()).unwrap();
        forward_candidates(&alice, bob.clone());
        forward_candidates(&bob, alice.clone());

        let source = NativeVideoSource::new(VideoResolution { width: 320, height: 240 }, true);
        let track = factory.create_video_track("screen", source.clone());
        let transceiver = alice
            .add_transceiver(
                MediaStreamTrack::Video(track.clone()),
                RtpTransceiverInit {
                    direction: RtpTransceiverDirection::SendOnly,
                    stream_ids: Vec::new(),
                    send_encodings: vec![RtpEncodingParameters {
                        max_bitrate: Some(300_000),
                        max_framerate: Some(15.0),
                        ..Default::default()
                    }],
                },
            )
            .unwrap();

        let offer = alice.create_offer(OfferOptions::default()).await.unwrap();
        alice.set_local_description(offer.clone()).await.unwrap();
        bob.set_remote_description(offer).await.unwrap();
        let answer = bob.create_answer(AnswerOptions::default()).await.unwrap();
        bob.set_local_description(answer.clone()).await.unwrap();
        alice.set_remote_description(answer).await.unwrap();

        let pump = tokio::spawn(async move {
            let start = Instant::now();
            let mut tick = tokio::time::interval(Duration::from_millis(33));
            loop {
                tick.tick().await;
                source.capture_frame(&VideoFrame {
                    rotation: VideoRotation::VideoRotation0,
                    timestamp_us: start.elapsed().as_micros() as i64,
                    frame_metadata: None,
                    buffer: I420Buffer::new(320, 240),
                });
            }
        });

        let deadline = Instant::now() + Duration::from_secs(10);
        while frames_encoded(&alice).await == 0 {
            assert!(Instant::now() < deadline, "the video encoder never produced a frame");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let sender = transceiver.sender();
        let mut params = sender.parameters();
        params.encodings[0].max_bitrate = Some(2_000_000);
        params.encodings[0].max_framerate = Some(30.0);
        params.set_degradation_preference(DegradationPreference::MaintainResolution);
        sender.set_parameters(params).unwrap();
        track.set_content_hint(VideoContentHint::Text);

        let applied = sender.parameters();
        assert_eq!(applied.encodings[0].max_bitrate, Some(2_000_000));
        assert_eq!(applied.encodings[0].max_framerate, Some(30.0));
        assert_eq!(
            applied.degradation_preference(),
            Some(DegradationPreference::MaintainResolution)
        );
        assert_eq!(track.content_hint(), VideoContentHint::Text);

        let before = frames_encoded(&alice).await;
        let deadline = Instant::now() + Duration::from_secs(5);
        while frames_encoded(&alice).await <= before {
            assert!(Instant::now() < deadline, "encoding stopped after the change");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        pump.abort();
        alice.close();
        bob.close();
    }
}
