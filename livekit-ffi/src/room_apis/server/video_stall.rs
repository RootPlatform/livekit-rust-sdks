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

//! Keyframe requests for a remote video stream that receives media but
//! completes no frame.
//!
//! After a packet that never arrives, an H.264 receiver assembles no delta
//! frame until the next keyframe, while NACK keeps asking for the packet.
//! WebRTC asks the sender for a keyframe 3 s after the last decodable frame
//! and every 3 s after that (VideoReceiveStream2's max_wait_for_frame). Watch
//! Stream screen shares in desktop 0.9.153 (ROOT-DESKTOP-3RQ) froze 4-10 s
//! that way with 140-600 KB arriving every 2 s and one PLI sent. This asks as
//! soon as the stall is plain: no frame for a second while media bytes keep
//! arriving. Inbound stats are read only during such a gap.

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use livekit::{prelude::RemoteVideoTrack, webrtc::stats::RtcStats};
use tokio::{sync::oneshot, time::MissedTickBehavior};

use crate::server;

#[derive(Clone, Copy, Debug)]
struct Sample {
    at: Instant,
    bytes: u64,
    frames: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StallVerdict {
    /// The first reading of a gap; the next one decides.
    Baseline,
    /// Frames completed since the last reading.
    Decoding,
    /// Too few media bytes for a stall: a paused or still stream, or one that
    /// never completed a frame (WebRTC requests keyframes for those itself).
    Idle,
    /// Stalled, and a keyframe was requested less than `REPEAT` ago.
    Stalled,
    /// Stalled: request a keyframe now.
    RequestKeyframe,
}

/// Decides from inbound-rtp readings when a stalled stream gets a keyframe
/// request.
#[derive(Debug, Default)]
pub(crate) struct StallKeyframePolicy {
    last: Option<Sample>,
    last_request: Option<Instant>,
}

impl StallKeyframePolicy {
    /// How long without a frame before inbound stats are read.
    pub(crate) const GAP: Duration = Duration::from_millis(1000);
    /// Readings during a gap.
    pub(crate) const TICK: Duration = Duration::from_millis(500);
    /// Readings while the stream is idle.
    pub(crate) const IDLE_TICK: Duration = Duration::from_millis(2000);
    /// Media payload (padding and headers do not count) that would have
    /// completed frames: 128 kbps.
    pub(crate) const MIN_BYTES_PER_SECOND: u64 = 16_000;
    /// At most one request this often. WebRTC's own come every 3 s as well,
    /// and the SFU throttles what reaches the sender.
    pub(crate) const REPEAT: Duration = Duration::from_millis(3000);

    /// A frame arrived: the next gap starts from a new baseline.
    pub(crate) fn reset(&mut self) {
        self.last = None;
    }

    /// One inbound-rtp reading taken during a gap: total payload bytes and
    /// frames received (assembled, before decoding).
    pub(crate) fn on_sample(&mut self, at: Instant, bytes: u64, frames: u64) -> StallVerdict {
        let Some(previous) = self.last.replace(Sample { at, bytes, frames }) else {
            return StallVerdict::Baseline;
        };
        if frames != previous.frames || bytes < previous.bytes {
            return StallVerdict::Decoding;
        }
        let elapsed = at.saturating_duration_since(previous.at);
        if frames == 0 || elapsed.is_zero() {
            return StallVerdict::Idle;
        }
        let rate = (bytes - previous.bytes) as f64 / elapsed.as_secs_f64();
        if rate < Self::MIN_BYTES_PER_SECOND as f64 {
            return StallVerdict::Idle;
        }
        if self.last_request.is_some_and(|last| at.saturating_duration_since(last) < Self::REPEAT) {
            return StallVerdict::Stalled;
        }
        self.last_request = Some(at);
        StallVerdict::RequestKeyframe
    }
}

/// Watches one remote video track while its FFI video stream runs; stops when
/// dropped.
pub(crate) struct StallWatch {
    frames: Arc<AtomicU64>,
    _stop: oneshot::Sender<()>,
}

impl StallWatch {
    pub(crate) fn start(server: &'static server::FfiServer, track: RemoteVideoTrack) -> Self {
        let frames = Arc::new(AtomicU64::new(0));
        let (stop_tx, stop_rx) = oneshot::channel();
        server.async_runtime.spawn(watch(track, frames.clone(), stop_rx));
        Self { frames, _stop: stop_tx }
    }

    pub(crate) fn on_frame(&self) {
        self.frames.fetch_add(1, Ordering::Relaxed);
    }
}

async fn watch(track: RemoteVideoTrack, frames: Arc<AtomicU64>, mut stop: oneshot::Receiver<()>) {
    let mut policy = StallKeyframePolicy::default();
    let mut tick = tokio::time::interval(StallKeyframePolicy::TICK);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut frames_seen = 0;
    let mut last_frame_at = Instant::now();
    let mut next_reading_at = last_frame_at;
    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = tick.tick() => {}
        }
        let now = Instant::now();
        let count = frames.load(Ordering::Relaxed);
        if count != frames_seen {
            frames_seen = count;
            last_frame_at = now;
            policy.reset();
            continue;
        }
        if now.saturating_duration_since(last_frame_at) < StallKeyframePolicy::GAP
            || now < next_reading_at
        {
            continue;
        }
        let Some((bytes, received)) = inbound_video(&track).await else {
            continue;
        };
        let at = Instant::now();
        match policy.on_sample(at, bytes, received) {
            StallVerdict::Idle => next_reading_at = at + StallKeyframePolicy::IDLE_TICK,
            StallVerdict::RequestKeyframe => {
                log::warn!(
                    "video track {}: no frame for {} ms while media keeps arriving; asking the \
                     sender for a keyframe",
                    track.sid(),
                    at.saturating_duration_since(last_frame_at).as_millis()
                );
                track.rtc_track().request_key_frame();
            }
            StallVerdict::Baseline | StallVerdict::Decoding | StallVerdict::Stalled => {}
        }
    }
}

async fn inbound_video(track: &RemoteVideoTrack) -> Option<(u64, u64)> {
    let stats = track.get_stats().await.ok()?;
    stats.iter().find_map(|stats| match stats {
        RtcStats::InboundRtp(inbound) if inbound.stream.kind == "video" => {
            Some((inbound.inbound.bytes_received, inbound.inbound.frames_received))
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const KB: u64 = 1000;

    fn ms(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn media_without_frames_requests_a_keyframe_on_the_second_reading() {
        let start = Instant::now();
        let mut policy = StallKeyframePolicy::default();
        assert_eq!(policy.on_sample(start, 500 * KB, 120), StallVerdict::Baseline);
        assert_eq!(policy.on_sample(ms(start, 500), 540 * KB, 120), StallVerdict::RequestKeyframe);
    }

    #[test]
    fn a_stall_is_asked_about_again_only_after_the_repeat_interval() {
        let start = Instant::now();
        let mut policy = StallKeyframePolicy::default();
        policy.on_sample(start, 0, 10);
        assert_eq!(policy.on_sample(ms(start, 500), 40 * KB, 10), StallVerdict::RequestKeyframe);
        assert_eq!(policy.on_sample(ms(start, 1000), 80 * KB, 10), StallVerdict::Stalled);
        assert_eq!(policy.on_sample(ms(start, 3000), 240 * KB, 10), StallVerdict::Stalled);
        assert_eq!(policy.on_sample(ms(start, 3500), 280 * KB, 10), StallVerdict::RequestKeyframe);
    }

    #[test]
    fn a_still_or_paused_stream_is_idle() {
        let start = Instant::now();
        let mut policy = StallKeyframePolicy::default();
        policy.on_sample(start, 100 * KB, 50);
        assert_eq!(policy.on_sample(ms(start, 500), 100 * KB, 50), StallVerdict::Idle);
        // 4 KB in 500 ms is 64 kbps, below the floor.
        assert_eq!(policy.on_sample(ms(start, 1000), 104 * KB, 50), StallVerdict::Idle);
        // An idle reading two seconds later is judged by its rate.
        assert_eq!(policy.on_sample(ms(start, 3000), 124 * KB, 50), StallVerdict::Idle);
        assert_eq!(policy.on_sample(ms(start, 5000), 164 * KB, 50), StallVerdict::RequestKeyframe);
    }

    #[test]
    fn frames_completing_is_not_a_stall_until_they_stop() {
        let start = Instant::now();
        let mut policy = StallKeyframePolicy::default();
        policy.on_sample(start, 0, 10);
        assert_eq!(policy.on_sample(ms(start, 500), 40 * KB, 11), StallVerdict::Decoding);
        assert_eq!(policy.on_sample(ms(start, 1000), 80 * KB, 11), StallVerdict::RequestKeyframe);
    }

    #[test]
    fn a_stream_that_never_completed_a_frame_is_left_to_webrtc() {
        let start = Instant::now();
        let mut policy = StallKeyframePolicy::default();
        policy.on_sample(start, 0, 0);
        assert_eq!(policy.on_sample(ms(start, 500), 200 * KB, 0), StallVerdict::Idle);
    }

    #[test]
    fn counters_that_go_back_start_a_new_baseline() {
        let start = Instant::now();
        let mut policy = StallKeyframePolicy::default();
        policy.on_sample(start, 900 * KB, 300);
        assert_eq!(policy.on_sample(ms(start, 500), 10 * KB, 300), StallVerdict::Decoding);
        assert_eq!(policy.on_sample(ms(start, 1000), 50 * KB, 300), StallVerdict::RequestKeyframe);
    }

    #[test]
    fn a_frame_resets_the_baseline() {
        let start = Instant::now();
        let mut policy = StallKeyframePolicy::default();
        policy.on_sample(start, 0, 10);
        policy.reset();
        assert_eq!(policy.on_sample(ms(start, 500), 40 * KB, 10), StallVerdict::Baseline);
    }

    // ROOT-DESKTOP-3RQ's windows: 136-601 KB in 2 s and no frame.
    #[test]
    fn the_3rq_windows_are_stalls() {
        for kb_per_2s in [136, 397, 601] {
            let start = Instant::now();
            let mut policy = StallKeyframePolicy::default();
            policy.on_sample(start, 0, 3965);
            assert_eq!(
                policy.on_sample(ms(start, 500), kb_per_2s * KB / 4, 3965),
                StallVerdict::RequestKeyframe,
                "{kb_per_2s} KB per 2 s"
            );
        }
    }
}
