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

//! D3D11 texture frames through a real send pipeline: a native video source, the H.264 encoder
//! stack (SimulcastEncoderAdapter, software fallback wrapper, the MediaFoundation encoder), RTP to
//! a second peer connection in this process, and its decoder. The receiver checks the colours.

use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_stream::StreamExt;

use crate::{
    peer_connection_factory::native::PeerConnectionFactoryExt,
    prelude::*,
    stats::RtcStats,
    video_frame::native::{NativeBuffer, VideoFrameBufferExt},
    video_source::native::NativeVideoSource,
    video_stream::native::NativeVideoStream,
};

extern "C" {
    fn lk_mf_test_create_bar_texture(width: i32, height: i32) -> *mut std::ffi::c_void;
    fn lk_mf_test_release_texture(texture: *mut std::ffi::c_void);
    fn lk_mf_test_gpu_frames_total() -> i64;
    fn lk_mf_test_frame_types(text: *mut std::ffi::c_char, length: usize);
    fn lk_mf_test_gpu_load_start(iterations: u32) -> *mut std::ffi::c_void;
    fn lk_mf_test_gpu_load_stop(load: *mut std::ffi::c_void) -> f64;
}

fn encoder_frame_types() -> String {
    let mut text = vec![0 as std::ffi::c_char; 512];
    // SAFETY: the harness writes at most `text.len()` bytes, NUL terminated.
    unsafe {
        lk_mf_test_frame_types(text.as_mut_ptr(), text.len());
        std::ffi::CStr::from_ptr(text.as_ptr()).to_string_lossy().into_owned()
    }
}

/// The bars `lk_mf_test_create_bar_texture` draws, as R, G, B.
const BARS: [[i32; 3]; 8] = [
    [255, 255, 255],
    [255, 255, 0],
    [0, 255, 255],
    [0, 255, 0],
    [255, 0, 255],
    [255, 0, 0],
    [0, 0, 255],
    [16, 16, 16],
];

/// libyuv's BT.601 limited-range ARGBToI420 for one colour.
fn expected_yuv([r, g, b]: [i32; 3]) -> [i32; 3] {
    [
        (66 * r + 129 * g + 25 * b + 0x1080) >> 8,
        (112 * b - 74 * g - 38 * r + 0x8080) >> 8,
        (112 * r - 94 * g - 18 * b + 0x8080) >> 8,
    ]
}

/// The same bars written straight into I420, as the memory path delivers them.
fn bar_i420(width: u32, height: u32) -> I420Buffer {
    let mut buffer = I420Buffer::new(width, height);
    let (sy, su, sv) = buffer.strides();
    let (y, u, v) = buffer.data_mut();
    for row in 0..height as usize {
        for col in 0..width as usize {
            let yuv = expected_yuv(BARS[col * 8 / width as usize]);
            y[row * sy as usize + col] = yuv[0] as u8;
            if row % 2 == 0 && col % 2 == 0 {
                u[row / 2 * su as usize + col / 2] = yuv[1] as u8;
                v[row / 2 * sv as usize + col / 2] = yuv[2] as u8;
            }
        }
    }
    buffer
}

/// Average Y, U, V over the middle half of each bar.
fn measure_bars(frame: &I420Buffer) -> Vec<[i32; 3]> {
    let (width, height) = (frame.width() as usize, frame.height() as usize);
    let (y, u, v) = frame.data();
    let (sy, su, sv) = frame.strides();
    (0..8)
        .map(|bar| {
            let (x0, x1) = (bar * width / 8 + width / 32, (bar + 1) * width / 8 - width / 32);
            let mut sum = [0i64; 3];
            let mut count = [0i64; 3];
            for row in height / 4..height * 3 / 4 {
                for col in x0..x1 {
                    sum[0] += y[row * sy as usize + col] as i64;
                    count[0] += 1;
                    if row % 2 == 0 && col % 2 == 0 {
                        sum[1] += u[row / 2 * su as usize + col / 2] as i64;
                        sum[2] += v[row / 2 * sv as usize + col / 2] as i64;
                        count[1] += 1;
                        count[2] += 1;
                    }
                }
            }
            [0, 1, 2].map(|i| (sum[i] / count[i].max(1)) as i32)
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

struct Texture(*mut std::ffi::c_void);

// SAFETY: the texture is only used through D3D11TextureBuffer::CopyFrom, which takes the device's
// multithread lock.
unsafe impl Send for Texture {}

impl Drop for Texture {
    fn drop(&mut self) {
        // SAFETY: the pointer came from lk_mf_test_create_bar_texture with one reference.
        unsafe { lk_mf_test_release_texture(self.0) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Frames {
    /// D3D11 textures, the desktop's new path.
    Texture,
    /// I420 in memory, the path every frame took before.
    Memory,
}

struct Run {
    frames: Frames,
    width: u32,
    height: u32,
    degradation: DegradationPreference,
    /// GPU load (`lk_mf_test_gpu_load_start` iterations) while it runs.
    load: Option<u32>,
    duration: Duration,
}

#[derive(Debug, Default)]
struct Outcome {
    received: u32,
    received_fps: f64,
    smallest_height: u32,
    last: Option<I420Buffer>,
    encoded: u32,
    encoder: String,
    resolution_changes: u32,
    cpu_limited_s: f64,
    gpu_frames: i64,
    load_fps: f64,
}

/// One sender, one receiver in this process; frames at 30 fps for `run.duration`.
async fn run_loopback(run: Run) -> Option<Outcome> {
    // SAFETY: plain harness call; null when the machine has no MF hardware H.264 encoder. It also
    // holds the encoder device for the run, as the desktop's capture does.
    let texture =
        Texture(unsafe { lk_mf_test_create_bar_texture(run.width as i32, run.height as i32) });
    if texture.0.is_null() {
        return None;
    }

    let factory = PeerConnectionFactory::default();
    let alice = factory.create_peer_connection(RtcConfiguration::default()).unwrap();
    let bob = factory.create_peer_connection(RtcConfiguration::default()).unwrap();
    forward_candidates(&alice, bob.clone());
    forward_candidates(&bob, alice.clone());

    let (track_tx, mut track_rx) = mpsc::unbounded_channel::<RtcVideoTrack>();
    bob.on_track(Some(Box::new(move |event| {
        if let MediaStreamTrack::Video(track) = event.track {
            let _ = track_tx.send(track);
        }
    })));

    let source =
        NativeVideoSource::new(VideoResolution { width: run.width, height: run.height }, false);
    let track = factory.create_video_track("screen", source.clone());
    let transceiver = alice
        .add_transceiver(
            MediaStreamTrack::Video(track),
            RtpTransceiverInit {
                direction: RtpTransceiverDirection::SendOnly,
                stream_ids: vec!["screen".to_owned()],
                send_encodings: vec![RtpEncodingParameters {
                    max_bitrate: Some(2_500_000),
                    max_framerate: Some(30.0),
                    ..Default::default()
                }],
            },
        )
        .unwrap();
    // Constrained baseline first, as livekit offers it (rtc_session.rs).
    let mut h264: Vec<_> = factory
        .get_rtp_sender_capabilities(MediaType::Video)
        .codecs
        .into_iter()
        .filter(|codec| codec.mime_type.eq_ignore_ascii_case("video/h264"))
        .collect();
    h264.sort_by_key(|codec| {
        !codec.sdp_fmtp_line.as_deref().unwrap_or("").contains("profile-level-id=42e01f")
    });
    transceiver.set_codec_preferences(h264).unwrap();
    let sender = transceiver.sender();
    sender.set_video_encoder_backend(crate::rtp_sender::VideoEncoderBackend::Hardware);
    let mut params = sender.parameters();
    params.set_degradation_preference(run.degradation);
    sender.set_parameters(params).unwrap();

    let offer = alice.create_offer(OfferOptions::default()).await.unwrap();
    alice.set_local_description(offer.clone()).await.unwrap();
    bob.set_remote_description(offer).await.unwrap();
    let answer = bob.create_answer(AnswerOptions::default()).await.unwrap();
    bob.set_local_description(answer.clone()).await.unwrap();
    alice.set_remote_description(answer).await.unwrap();

    // SAFETY: plain harness calls.
    let load = run.load.map(|iterations| unsafe { lk_mf_test_gpu_load_start(iterations) });
    let gpu_before = unsafe { lk_mf_test_gpu_frames_total() };
    let (width, height, kind) = (run.width, run.height, run.frames);
    let pump = tokio::spawn(async move {
        let texture = texture;
        let memory = bar_i420(width, height);
        let start = Instant::now();
        let mut tick = tokio::time::interval(Duration::from_millis(33));
        loop {
            tick.tick().await;
            let timestamp_us = start.elapsed().as_micros() as i64;
            match kind {
                Frames::Texture => {
                    // SAFETY: the texture lives as long as this task.
                    let Some(buffer) =
                        (unsafe { NativeBuffer::from_d3d11_texture(texture.0, width, height) })
                    else {
                        continue;
                    };
                    source.capture_frame(&VideoFrame {
                        rotation: VideoRotation::VideoRotation0,
                        timestamp_us,
                        frame_metadata: None,
                        buffer,
                    });
                }
                Frames::Memory => {
                    let mut buffer = I420Buffer::new(width, height);
                    let (dy, du, dv) = buffer.data_mut();
                    let (sy, su, sv) = memory.data();
                    dy.copy_from_slice(sy);
                    du.copy_from_slice(su);
                    dv.copy_from_slice(sv);
                    source.capture_frame(&VideoFrame {
                        rotation: VideoRotation::VideoRotation0,
                        timestamp_us,
                        frame_metadata: None,
                        buffer,
                    });
                }
            }
        }
    });

    let remote = tokio::time::timeout(Duration::from_secs(10), track_rx.recv())
        .await
        .expect("bob never got the track")
        .expect("track channel closed");
    let mut stream = NativeVideoStream::new(remote);
    let mut outcome = Outcome { smallest_height: u32::MAX, ..Default::default() };
    let started = Instant::now();
    let deadline = started + run.duration;
    while let Ok(Some(frame)) = tokio::time::timeout_at(deadline.into(), stream.next()).await {
        outcome.received += 1;
        let i420 = frame.buffer.to_i420();
        outcome.smallest_height = outcome.smallest_height.min(i420.height());
        outcome.last = Some(i420);
    }
    outcome.received_fps = outcome.received as f64 / started.elapsed().as_secs_f64();

    for stats in alice.get_stats().await.unwrap() {
        if let RtcStats::OutboundRtp(o) = stats {
            if o.stream.kind == "video" {
                outcome.encoded = o.outbound.frames_encoded;
                outcome.encoder = o.outbound.encoder_implementation.clone();
                outcome.resolution_changes = o.outbound.quality_limitation_resolution_changes;
                outcome.cpu_limited_s =
                    o.outbound.quality_limitation_durations.get("cpu").copied().unwrap_or(0.0);
            }
        }
    }
    // SAFETY: plain harness calls; `load` came from lk_mf_test_gpu_load_start.
    outcome.gpu_frames = unsafe { lk_mf_test_gpu_frames_total() } - gpu_before;
    if let Some(load) = load {
        outcome.load_fps = unsafe { lk_mf_test_gpu_load_stop(load) };
    }
    pump.abort();
    alice.close();
    bob.close();
    Some(outcome)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn d3d11_frames_encode_on_the_gpu_and_decode_with_their_colours() {
    let Some(outcome) = run_loopback(Run {
        frames: Frames::Texture,
        width: 640,
        height: 360,
        degradation: DegradationPreference::MaintainFramerateAndResolution,
        load: None,
        duration: Duration::from_secs(3),
    })
    .await
    else {
        eprintln!("skipped: no MediaFoundation hardware H.264 encoder");
        return;
    };

    assert!(outcome.received >= 45, "only {} frames arrived", outcome.received);
    let last = outcome.last.as_ref().unwrap();
    assert_eq!((last.width(), last.height()), (640, 360));
    assert!(outcome.encoder.contains("MediaFoundation"), "encoded by {}", outcome.encoder);
    assert!(
        outcome.gpu_frames as u32 + 2 >= outcome.encoded,
        "{} of {} encoded frames were converted on the GPU; the MF encoder got: {}",
        outcome.gpu_frames,
        outcome.encoded,
        encoder_frame_types()
    );
    let measured = measure_bars(last);
    for (bar, (got, rgb)) in measured.iter().zip(BARS).enumerate() {
        let want = expected_yuv(rgb);
        for plane in 0..3 {
            assert!(
                (got[plane] - want[plane]).abs() <= 8,
                "bar {bar} plane {plane}: got {} want {} (all bars {measured:?})",
                got[plane],
                want[plane]
            );
        }
    }
    eprintln!(
        "{} frames received, {} encoded by {}, {} on the GPU, bars {measured:?}",
        outcome.received, outcome.encoded, outcome.encoder, outcome.gpu_frames
    );
}

/// A 720p share while another device keeps the same GPU busy, as a game does: the old settings
/// (memory frames, CPU adaptation that may shrink the picture) next to the new ones (texture
/// frames, no CPU adaptation for a hardware encoder). The new ones must keep the full size and
/// the hardware encoder; the old ones are reported for comparison. About a minute; run with
/// `cargo test -p libwebrtc --lib --release d3d11 -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "about a minute of GPU load"]
async fn a_share_under_gpu_load_keeps_its_size_with_the_new_settings() {
    // About 15 ms of GPU work per "game frame" on an RTX 5090; LK_TEST_GPU_LOAD overrides it.
    let iterations: u32 =
        std::env::var("LK_TEST_GPU_LOAD").ok().and_then(|v| v.parse().ok()).unwrap_or(20_000);
    let configs = [
        (
            "before: memory + MaintainFramerate",
            Frames::Memory,
            DegradationPreference::MaintainFramerate,
        ),
        ("texture + MaintainFramerate", Frames::Texture, DegradationPreference::MaintainFramerate),
        (
            "after: texture + MaintainFramerateAndResolution",
            Frames::Texture,
            DegradationPreference::MaintainFramerateAndResolution,
        ),
    ];
    let mut after = None;
    for (name, frames, degradation) in configs {
        let Some(outcome) = run_loopback(Run {
            frames,
            width: 1280,
            height: 720,
            degradation,
            load: Some(iterations),
            duration: Duration::from_secs(20),
        })
        .await
        else {
            eprintln!("skipped: no MediaFoundation hardware H.264 encoder");
            return;
        };
        eprintln!(
            "{name}: received {} ({:.1} fps), smallest {}p, encoded {} by {}, {} on the GPU, \
             {} resolution changes, {:.1} s CPU-limited; GPU load ran at {:.0} fps",
            outcome.received,
            outcome.received_fps,
            outcome.smallest_height,
            outcome.encoded,
            outcome.encoder,
            outcome.gpu_frames,
            outcome.resolution_changes,
            outcome.cpu_limited_s,
            outcome.load_fps
        );
        if degradation == DegradationPreference::MaintainFramerateAndResolution {
            after = Some(outcome);
        }
    }
    let after = after.unwrap();
    assert_eq!(after.smallest_height, 720, "the new settings let the picture shrink");
    assert_eq!(after.resolution_changes, 0);
    assert!(after.encoder.contains("MediaFoundation"), "encoded by {}", after.encoder);
    assert!(after.gpu_frames as u32 + 2 >= after.encoded);
    assert!(after.received_fps >= 20.0, "only {:.1} fps arrived", after.received_fps);
}
