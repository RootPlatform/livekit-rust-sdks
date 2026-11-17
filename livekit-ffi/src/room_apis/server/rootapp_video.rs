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

//! RootApp video requests (rootapp_video.proto, oneof cases from 1015).

use livekit::prelude::*;

use super::{room::FfiTrack, video_source::FfiVideoSource, FfiServer};
use crate::{conversion::room::degradation_preference_from_proto, proto, FfiResult};

/// RootSetSenderEncodingRequest.
pub const CAP_SENDER_ENCODING: u64 = 1 << 0;

/// RootCaptureD3d11FrameRequest (Windows only).
pub const CAP_D3D11_CAPTURE: u64 = 1 << 1;

/// What `livekit_ffi_rootapp_video_caps` reports.
#[cfg(target_os = "windows")]
pub const ROOTAPP_VIDEO_CAPS: u64 = CAP_SENDER_ENCODING | CAP_D3D11_CAPTURE;
#[cfg(not(target_os = "windows"))]
pub const ROOTAPP_VIDEO_CAPS: u64 = CAP_SENDER_ENCODING;

/// Captures a D3D11 texture into a native video source. Runs on the caller's
/// thread: the GPU copy is queued before it returns.
pub fn on_capture_d3d11_frame(
    server: &'static FfiServer,
    req: proto::RootCaptureD3d11FrameRequest,
) -> FfiResult<proto::RootCaptureD3d11FrameResponse> {
    let source = server.retrieve_handle::<FfiVideoSource>(req.source_handle)?;
    #[cfg(target_os = "windows")]
    let error = {
        let timestamp_us = req.timestamp_us.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_micros() as i64)
                .unwrap_or(0)
        });
        // SAFETY: the host passes a live ID3D11Texture2D* it keeps for the duration of the
        // request (RootCaptureD3d11FrameRequest).
        unsafe {
            source.capture_d3d11_texture(
                req.texture as usize as *mut std::ffi::c_void,
                req.width,
                req.height,
                timestamp_us,
            )
        }
        .err()
    };
    #[cfg(not(target_os = "windows"))]
    let error = {
        let _ = (&source, &req);
        Some("D3D11 textures are only taken on Windows".to_owned())
    };
    Ok(proto::RootCaptureD3d11FrameResponse { error })
}

/// Changes a published local video track's encoder in place. Runs on the
/// caller's thread and blocks on WebRTC's signaling thread for the sender's
/// getParameters/setParameters and the track's content hint.
pub fn on_set_sender_encoding(
    server: &'static FfiServer,
    req: proto::RootSetSenderEncodingRequest,
) -> FfiResult<proto::RootSetSenderEncodingResponse> {
    let track = server.retrieve_handle::<FfiTrack>(req.track_handle)?.track.clone();
    let error = match (track, sender_encoding_update(&req)) {
        (Track::LocalVideo(track), Ok(update)) => {
            track.set_sender_encoding(&update).err().map(|e| e.to_string())
        }
        (Track::LocalVideo(_), Err(e)) => Some(e),
        _ => Some("track is not a local video track".to_owned()),
    };
    if let Some(error) = &error {
        log::warn!("RootSetSenderEncoding failed: {error}");
    }
    Ok(proto::RootSetSenderEncodingResponse { error })
}

fn sender_encoding_update(
    req: &proto::RootSetSenderEncodingRequest,
) -> Result<SenderEncodingUpdate, String> {
    let degradation_preference = match req.degradation_preference {
        None => None,
        Some(value) => Some(
            degradation_preference_from_proto(Some(value))
                .ok_or_else(|| format!("unknown degradation_preference {value}"))?,
        ),
    };
    let content_hint = match req.content_hint {
        None => None,
        Some(value) => Some(
            proto::RootVideoContentHint::try_from(value)
                .map(content_hint_from_proto)
                .map_err(|_| format!("unknown content_hint {value}"))?,
        ),
    };
    Ok(SenderEncodingUpdate {
        max_bitrate_bps: req.max_bitrate_bps,
        max_framerate: req.max_framerate,
        degradation_preference,
        content_hint,
    })
}

fn content_hint_from_proto(hint: proto::RootVideoContentHint) -> VideoContentHint {
    match hint {
        proto::RootVideoContentHint::None => VideoContentHint::None,
        proto::RootVideoContentHint::Motion => VideoContentHint::Motion,
        proto::RootVideoContentHint::Detail => VideoContentHint::Detail,
        proto::RootVideoContentHint::Text => VideoContentHint::Text,
    }
}

#[cfg(test)]
mod tests {
    use livekit::options::DegradationPreference;

    use super::*;

    #[test]
    fn rootapp_video_sender_encoding_request_converts() {
        let update = sender_encoding_update(&proto::RootSetSenderEncodingRequest {
            track_handle: 1,
            max_bitrate_bps: Some(6_000_000),
            max_framerate: Some(60.0),
            degradation_preference: Some(proto::DegradationPreference::MaintainResolution as i32),
            content_hint: Some(proto::RootVideoContentHint::Text as i32),
        })
        .unwrap();
        assert_eq!(
            update,
            SenderEncodingUpdate {
                max_bitrate_bps: Some(6_000_000),
                max_framerate: Some(60.0),
                degradation_preference: Some(DegradationPreference::MaintainResolution),
                content_hint: Some(VideoContentHint::Text),
            }
        );

        let empty = sender_encoding_update(&proto::RootSetSenderEncodingRequest {
            track_handle: 1,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(empty, SenderEncodingUpdate::default());
    }

    #[test]
    fn rootapp_video_sender_encoding_rejects_unknown_enums() {
        let bad_preference = proto::RootSetSenderEncodingRequest {
            track_handle: 1,
            degradation_preference: Some(3),
            ..Default::default()
        };
        assert!(sender_encoding_update(&bad_preference).is_err());
        let bad_hint = proto::RootSetSenderEncodingRequest {
            track_handle: 1,
            content_hint: Some(9),
            ..Default::default()
        };
        assert!(sender_encoding_update(&bad_hint).is_err());
    }

    #[test]
    fn rootapp_video_caps_export_reports_sender_encoding() {
        assert_eq!(crate::cabi::livekit_ffi_rootapp_video_caps(), ROOTAPP_VIDEO_CAPS);
        assert_eq!(ROOTAPP_VIDEO_CAPS & CAP_SENDER_ENCODING, CAP_SENDER_ENCODING);
    }

    #[test]
    fn rootapp_video_content_hints_map() {
        for (proto_hint, hint) in [
            (proto::RootVideoContentHint::None, VideoContentHint::None),
            (proto::RootVideoContentHint::Motion, VideoContentHint::Motion),
            (proto::RootVideoContentHint::Detail, VideoContentHint::Detail),
            (proto::RootVideoContentHint::Text, VideoContentHint::Text),
        ] {
            assert_eq!(content_hint_from_proto(proto_hint), hint);
        }
    }
}
