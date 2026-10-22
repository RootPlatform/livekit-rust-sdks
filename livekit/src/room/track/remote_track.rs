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

use std::sync::Arc;

use libwebrtc::enum_dispatch;
use libwebrtc::{prelude::*, stats::RtcStats};
use livekit_protocol as proto;

use super::{track_dispatch, TrackInner};
use crate::prelude::*;

#[derive(Clone, Debug)]
pub enum RemoteTrack {
    Audio(RemoteAudioTrack),
    Video(RemoteVideoTrack),
}

impl RemoteTrack {
    track_dispatch!([Audio, Video]);

    #[inline]
    pub fn rtc_track(&self) -> MediaStreamTrack {
        match self {
            Self::Audio(track) => track.rtc_track().into(),
            Self::Video(track) => track.rtc_track().into(),
        }
    }

    pub async fn get_stats(&self) -> RoomResult<Vec<RtcStats>> {
        match self {
            Self::Audio(track) => track.get_stats().await,
            Self::Video(track) => track.get_stats().await,
        }
    }

    /// Stores whether the SFU forwards the track; true when that changed.
    pub(crate) fn set_stream_state(&self, state: StreamState) -> bool {
        match self {
            Self::Audio(track) => track.set_stream_state(state),
            Self::Video(track) => track.set_stream_state(state),
        }
    }
}

pub(super) fn set_stream_state(inner: &TrackInner, state: StreamState) -> bool {
    std::mem::replace(&mut inner.info.write().stream_state, state) != state
}

pub(super) async fn get_stats(inner: &Arc<TrackInner>) -> RoomResult<Vec<RtcStats>> {
    let transceiver = inner.info.read().transceiver.clone();
    let Some(transceiver) = transceiver.as_ref() else {
        return Err(RoomError::Internal("no transceiver found for track".into()));
    };

    Ok(transceiver.receiver().get_stats().await?)
}

pub(super) fn update_info(inner: &Arc<TrackInner>, track: &Track, new_info: proto::TrackInfo) {
    super::update_info(inner, track, new_info.clone());
    super::set_muted(inner, track, new_info.muted);
}

impl From<RemoteTrack> for Track {
    fn from(track: RemoteTrack) -> Self {
        match track {
            RemoteTrack::Audio(track) => Self::RemoteAudio(track),
            RemoteTrack::Video(track) => Self::RemoteVideo(track),
        }
    }
}

impl TryFrom<Track> for RemoteTrack {
    type Error = &'static str;

    fn try_from(track: Track) -> Result<Self, Self::Error> {
        match track {
            Track::RemoteAudio(track) => Ok(Self::Audio(track)),
            Track::RemoteVideo(track) => Ok(Self::Video(track)),
            _ => Err("not a local track"),
        }
    }
}

#[cfg(test)]
mod tests {
    use libwebrtc::{
        peer_connection_factory::native::PeerConnectionFactoryExt,
        video_source::native::NativeVideoSource,
    };

    use super::*;

    #[tokio::test]
    async fn stream_state_follows_the_sfu_and_reports_only_changes() {
        let factory = PeerConnectionFactory::default();
        let source = NativeVideoSource::new(VideoResolution { width: 1280, height: 720 }, true);
        let sid = TrackSid::try_from("TR_screenshare".to_string()).expect("track sid");
        let track = RemoteTrack::Video(RemoteVideoTrack::new(
            sid,
            "screen".into(),
            factory.create_video_track("screen", source),
        ));
        assert_eq!(track.stream_state(), StreamState::Active, "a subscription starts active");

        assert!(track.set_stream_state(proto::StreamState::Paused.into()));
        assert_eq!(track.stream_state(), StreamState::Paused);
        assert!(!track.set_stream_state(StreamState::Paused), "a repeated state is no change");

        assert!(track.set_stream_state(proto::StreamState::Active.into()));
        assert_eq!(Track::from(track).stream_state(), StreamState::Active);
    }
}
