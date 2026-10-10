// Copyright 2026 LiveKit, Inc.
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

#[cfg(feature = "__lk-e2e-test")]
use {
    anyhow::Result,
    common::{
        audio::{SineParameters, SineTrack},
        test_rooms,
        video::{SolidColorParams, SolidColorTrack},
    },
    libwebrtc::{
        audio_source::native::NativeAudioSource,
        prelude::{AudioFrame, AudioSourceOptions, RtcAudioSource},
        stats::RtcStats,
    },
    livekit::{
        options::{AudioEncoding, TrackPublishOptions, VideoCodec},
        prelude::*,
    },
    std::{collections::BTreeSet, sync::Arc, time::Duration},
};

mod common;

#[cfg(feature = "__lk-e2e-test")]
async fn audio_send_codec(track: &LocalAudioTrack) -> Result<Option<String>> {
    let stats = track.get_stats().await?;
    let codec_id = stats.iter().find_map(|stats| match stats {
        RtcStats::OutboundRtp(o) => Some(o.stream.codec_id.clone()),
        _ => None,
    });
    Ok(codec_id.and_then(|codec_id| {
        stats.into_iter().find_map(|stats| match stats {
            RtcStats::Codec(c) if c.rtc.id == codec_id => Some(c.codec.sdp_fmtp_line),
            _ => None,
        })
    }))
}

#[cfg(feature = "__lk-e2e-test")]
async fn record_codec(
    track: &LocalAudioTrack,
    seen: &mut BTreeSet<String>,
    for_ms: u64,
) -> Result<()> {
    for _ in 0..for_ms / 50 {
        if let Some(fmtp) = audio_send_codec(track).await? {
            if seen.insert(fmtp.clone()) {
                log::info!("screen-audio send codec fmtp: {fmtp}");
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

/// Screen-share audio keeps the send codec of its first answer through the negotiations after it
/// (the screen video right behind it, then the other participant's and our own track changes),
/// although LiveKit server's answers give it `maxaveragebitrate=510000` only when the server
/// resolves the track for that answer. With the pinned libwebrtc a send-codec change while it sends
/// would register this external stream with AudioState. A changed answer is logged by the
/// publisher transport as it keeps the first codec.
#[cfg(feature = "__lk-e2e-test")]
#[test_log::test(tokio::test)]
async fn test_screen_audio_send_codec_is_stable() -> Result<()> {
    let mut rooms = test_rooms(2).await?;
    let (other, _other_events) = rooms.pop().unwrap();
    let (room, _events) = rooms.pop().unwrap();
    let (room, other) = (Arc::new(room), Arc::new(other));
    log::info!("single peer connection: {}", room.is_single_peer_connection_active());

    let sine = SineParameters { sample_rate: 48_000, freq: 440.0, amplitude: 0.2, num_channels: 1 };
    let mut other_mic = SineTrack::new(other.clone(), sine.clone());
    other_mic.publish().await?;
    let mut mic = SineTrack::new(room.clone(), sine.clone());
    mic.publish().await?;
    tokio::time::sleep(Duration::from_millis(500)).await;

    let source = NativeAudioSource::new(AudioSourceOptions::default(), 48_000, 2, 100);
    let feeder_source = source.clone();
    let feeder = tokio::spawn(async move {
        let silence = vec![0i16; 960];
        loop {
            let frame = AudioFrame {
                data: silence.as_slice().into(),
                sample_rate: 48_000,
                num_channels: 2,
                samples_per_channel: 480,
            };
            if feeder_source.capture_frame(&frame).await.is_err() {
                break;
            }
        }
    });
    let audio = LocalAudioTrack::create_audio_track("screen-audio", RtcAudioSource::Native(source));
    room.local_participant()
        .publish_track(
            LocalTrack::Audio(audio.clone()),
            TrackPublishOptions {
                source: TrackSource::ScreenshareAudio,
                dtx: false,
                red: false,
                force_stereo: true,
                audio_encoding: Some(AudioEncoding { max_bitrate: 128_000 }),
                ..Default::default()
            },
        )
        .await?;

    let mut seen = BTreeSet::new();
    record_codec(&audio, &mut seen, 1_000).await?;
    let params = SolidColorParams { width: 640, height: 360, luma: 128 };
    let mut screen = SolidColorTrack::new(room.clone(), params.clone());
    screen.publish(VideoCodec::VP8, false).await?;
    record_codec(&audio, &mut seen, 1_000).await?;
    for _ in 0..3 {
        other_mic.unpublish().await?;
        record_codec(&audio, &mut seen, 500).await?;
        other_mic.publish().await?;
        record_codec(&audio, &mut seen, 500).await?;
        let mut camera = SolidColorTrack::new(room.clone(), params.clone());
        camera.publish(VideoCodec::VP8, false).await?;
        record_codec(&audio, &mut seen, 500).await?;
        camera.unpublish().await?;
        record_codec(&audio, &mut seen, 500).await?;
    }

    feeder.abort();
    assert_eq!(seen.len(), 1, "the screen-audio send codec changed: {seen:?}");
    assert!(seen.iter().all(|fmtp| fmtp.contains("stereo=1")), "{seen:?}");
    Ok(())
}
