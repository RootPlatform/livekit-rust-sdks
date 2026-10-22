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
    imp::video_track as imp_vt,
    media_stream_track::{media_stream_track, RtcTrackState},
};

#[cfg(not(target_arch = "wasm32"))]
use crate::native::packet_trailer::PacketTrailerHandler;

#[derive(Clone)]
pub struct RtcVideoTrack {
    pub(crate) handle: imp_vt::RtcVideoTrack,
}

impl RtcVideoTrack {
    media_stream_track!();

    /// Set the packet trailer handler for this track.
    ///
    /// When set, any `NativeVideoStream` created from this track will
    /// automatically use this handler to populate `user_timestamp`
    /// on each decoded frame.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_packet_trailer_handler(&self, handler: PacketTrailerHandler) {
        self.handle.set_packet_trailer_handler(handler);
    }

    /// Get the packet trailer handler, if one has been set.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn packet_trailer_handler(&self) -> Option<PacketTrailerHandler> {
        self.handle.packet_trailer_handler()
    }

    /// On a remote track, asks the sender for a keyframe (an RTCP PLI), as
    /// WebRTC does on its own 3 s after the last decodable frame. Blocks on
    /// WebRTC's worker thread; does nothing on a local track.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn request_key_frame(&self) {
        self.handle.request_key_frame();
    }
}

impl Debug for RtcVideoTrack {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RtcVideoTrack")
            .field("id", &self.id())
            .field("enabled", &self.enabled())
            .field("state", &self.state())
            .finish()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::time::{Duration, Instant};

    use tokio::sync::mpsc;

    use crate::{
        ice_candidate::IceCandidate,
        media_stream_track::MediaStreamTrack,
        peer_connection::{AnswerOptions, OfferOptions, PeerConnection, TrackEvent},
        peer_connection_factory::{
            native::PeerConnectionFactoryExt, PeerConnectionFactory, RtcConfiguration,
        },
        stats::RtcStats,
        video_frame::{I420Buffer, VideoFrame, VideoRotation},
        video_source::{native::NativeVideoSource, VideoResolution},
    };

    fn forward_candidates(from: &PeerConnection) -> mpsc::UnboundedReceiver<IceCandidate> {
        let (tx, rx) = mpsc::unbounded_channel();
        from.on_ice_candidate(Some(Box::new(move |candidate| {
            let _ = tx.send(candidate);
        })));
        rx
    }

    fn add_candidates(to: PeerConnection, mut candidates: mpsc::UnboundedReceiver<IceCandidate>) {
        tokio::spawn(async move {
            while let Some(candidate) = candidates.recv().await {
                let _ = to.add_ice_candidate(candidate).await;
            }
        });
    }

    async fn wait_for<T>(
        what: &str,
        timeout: Duration,
        mut probe: impl FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<T>>>>,
    ) -> T {
        let start = Instant::now();
        loop {
            if let Some(value) = probe().await {
                return value;
            }
            assert!(start.elapsed() < timeout, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn frames_decoded(pc: &PeerConnection) -> u32 {
        pc.get_stats()
            .await
            .unwrap_or_default()
            .iter()
            .find_map(|s| match s {
                RtcStats::InboundRtp(inbound) if inbound.stream.kind == "video" => {
                    Some(inbound.inbound.frames_decoded)
                }
                _ => None,
            })
            .unwrap_or(0)
    }

    async fn plis_received(pc: &PeerConnection) -> u32 {
        pc.get_stats()
            .await
            .unwrap_or_default()
            .iter()
            .find_map(|s| match s {
                RtcStats::OutboundRtp(outbound) if outbound.stream.kind == "video" => {
                    Some(outbound.outbound.pli_count)
                }
                _ => None,
            })
            .unwrap_or(0)
    }

    // In-process sender and receiver over loopback ICE: a key frame request on
    // the received track reaches the sender as an RTCP PLI.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn remote_track_key_frame_request_reaches_the_sender() {
        let _ = env_logger::builder().is_test(true).try_init();

        let factory = PeerConnectionFactory::default();
        let sender = factory.create_peer_connection(RtcConfiguration::default()).unwrap();
        let receiver = factory.create_peer_connection(RtcConfiguration::default()).unwrap();
        let sender_candidates = forward_candidates(&sender);
        let receiver_candidates = forward_candidates(&receiver);

        let (track_tx, mut track_rx) = mpsc::unbounded_channel::<TrackEvent>();
        receiver.on_track(Some(Box::new(move |event| {
            let _ = track_tx.send(event);
        })));

        let source = NativeVideoSource::new(VideoResolution { width: 320, height: 240 }, false);
        let local = factory.create_video_track("video", source.clone());
        sender.add_track(MediaStreamTrack::Video(local), &["stream"]).unwrap();

        let offer = sender.create_offer(OfferOptions::default()).await.unwrap();
        sender.set_local_description(offer.clone()).await.unwrap();
        receiver.set_remote_description(offer).await.unwrap();
        let answer = receiver.create_answer(AnswerOptions::default()).await.unwrap();
        receiver.set_local_description(answer.clone()).await.unwrap();
        sender.set_remote_description(answer).await.unwrap();
        add_candidates(receiver.clone(), sender_candidates);
        add_candidates(sender.clone(), receiver_candidates);

        let pump = tokio::spawn(async move {
            let start = Instant::now();
            let mut tick = tokio::time::interval(Duration::from_millis(33));
            loop {
                tick.tick().await;
                let mut frame =
                    VideoFrame::new(VideoRotation::VideoRotation0, I420Buffer::new(320, 240));
                frame.timestamp_us = start.elapsed().as_micros() as i64;
                source.capture_frame(&frame);
            }
        });

        let event = tokio::time::timeout(Duration::from_secs(10), track_rx.recv())
            .await
            .expect("no remote track")
            .unwrap();
        let MediaStreamTrack::Video(remote) = event.track else { panic!("not a video track") };

        wait_for("decoded frames", Duration::from_secs(15), || {
            let receiver = receiver.clone();
            Box::pin(async move { (frames_decoded(&receiver).await > 0).then_some(()) })
        })
        .await;
        let before = plis_received(&sender).await;

        remote.request_key_frame();

        let after = wait_for("the sender to receive a PLI", Duration::from_secs(5), || {
            let sender = sender.clone();
            Box::pin(async move {
                let plis = plis_received(&sender).await;
                (plis > before).then_some(plis)
            })
        })
        .await;
        assert!(after > before);

        pump.abort();
        receiver.close();
        sender.close();
    }
}
