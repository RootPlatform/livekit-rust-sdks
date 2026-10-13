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

//! RootApp native screen-share audio (rootapp_audio.proto RootScreenAudio*,
//! capability SCREEN_AUDIO).
//!
//! A capture attaches to a NATIVE AudioSource the host created and publishes;
//! the OS capture, the 10 ms pacing and the restarts all run natively
//! (webrtc-sys root_screen_audio.h). Requests only queue work on the capture's
//! own thread and return; its events are polled every 20 ms while the handle
//! lives. Dropping the handle detaches the capture and never blocks.

use std::{
    sync::{Arc, Weak},
    time::Duration,
};

use livekit::webrtc::audio_source::RtcAudioSource;
use tokio::time::MissedTickBehavior;
use webrtc_sys::root_screen_audio::{ffi as sys, ScreenAudioCapture};

use super::{
    audio_source::FfiAudioSource,
    rootapp_audio::{CAP_SCREEN_AUDIO, ROOTAPP_AUDIO_CAPS},
    FfiHandle, FfiServer,
};
use crate::{proto, FfiHandleId, FfiResult};

const EVENT_POLL: Duration = Duration::from_millis(20);
/// The first poll waits this long, so the host listens for the handle's
/// events (it learns the handle from the response) before any is sent.
const FIRST_POLL_DELAY: Duration = Duration::from_millis(100);
const NOT_SUPPORTED: &str = "native screen audio is not supported by this build";
/// Window titles longer than this are cut (a lookup key only).
const MAX_TITLE_CHARS: usize = 512;

const EVENT_STARTED: u32 = 1;
const EVENT_FAILED: u32 = 2;
const EVENT_STOPPED: u32 = 3;
/// Native frames are 48 kHz.
const FRAMES_PER_MS: f32 = 48.0;

/// What a capture handle drives: the native capture (a recorder in tests).
pub(crate) trait ScreenAudioEngine: Send + Sync {
    fn start(&self, target: &sys::RootScreenAudioTargetSpec, async_id: u64);
    fn stop(&self, async_id: u64);
    fn stats(&self) -> sys::RootScreenAudioStatsData;
    fn take_events(&self) -> Vec<sys::RootScreenAudioEventData>;
}

impl ScreenAudioEngine for ScreenAudioCapture {
    fn start(&self, target: &sys::RootScreenAudioTargetSpec, async_id: u64) {
        ScreenAudioCapture::start(self, target, async_id);
    }

    fn stop(&self, async_id: u64) {
        ScreenAudioCapture::stop(self, async_id);
    }

    fn stats(&self) -> sys::RootScreenAudioStatsData {
        ScreenAudioCapture::stats(self)
    }

    fn take_events(&self) -> Vec<sys::RootScreenAudioEventData> {
        ScreenAudioCapture::take_events(self)
    }
}

pub struct FfiScreenAudio {
    pub handle_id: FfiHandleId,
    engine: Arc<dyn ScreenAudioEngine>,
}

impl FfiHandle for FfiScreenAudio {}

fn refused(error: impl Into<String>) -> proto::RootScreenAudioStartResponse {
    proto::RootScreenAudioStartResponse {
        capture_handle: None,
        async_id: None,
        error: Some(error.into()),
    }
}

/// A capture attached to the source, when it is a NATIVE source that can take
/// one.
fn new_capture(
    server: &'static FfiServer,
    handle: FfiHandleId,
) -> Result<ScreenAudioCapture, String> {
    let source =
        server.retrieve_handle::<FfiAudioSource>(handle).map_err(|e| format!("source: {e}"))?;
    #[allow(unreachable_patterns)]
    match &source.source {
        #[cfg(not(target_arch = "wasm32"))]
        RtcAudioSource::Native(native) => native.attach_screen_audio(),
        _ => Err("the audio source is not a NATIVE source".into()),
    }
}

pub(crate) fn target_spec(
    target: Option<&proto::RootScreenAudioTarget>,
) -> sys::RootScreenAudioTargetSpec {
    use proto::root_screen_audio_target::Target;
    let mut spec = sys::RootScreenAudioTargetSpec::default();
    match target.and_then(|t| t.target.as_ref()) {
        Some(Target::ProcessId(pid)) => {
            spec.kind = 1;
            spec.process_id = *pid;
        }
        Some(Target::WindowId(window)) => {
            spec.kind = 2;
            spec.window_id = *window;
        }
        Some(Target::System(_)) | None => {}
    }
    if let Some(title) = target.and_then(|t| t.window_title.as_deref()) {
        spec.window_title = title.chars().take(MAX_TITLE_CHARS).collect();
    }
    spec
}

pub(crate) fn event_message(
    event: &sys::RootScreenAudioEventData,
) -> Option<proto::root_screen_audio_event::Message> {
    match event.kind {
        EVENT_STARTED => Some(
            proto::RootScreenAudioStarted {
                async_id: event.async_id,
                scope: scope(event.scope) as i32,
                description: event.description.clone(),
                sample_rate: event.sample_rate,
                channels: event.channels,
            }
            .into(),
        ),
        EVENT_FAILED => Some(
            proto::RootScreenAudioFailed {
                async_id: event.async_id,
                error: error(event.error) as i32,
                message: event.message.clone(),
                retrying: event.retrying,
            }
            .into(),
        ),
        EVENT_STOPPED => Some(proto::RootScreenAudioStopped { async_id: event.async_id }.into()),
        _ => None,
    }
}

fn scope(value: i32) -> proto::RootScreenAudioScope {
    proto::RootScreenAudioScope::try_from(value).unwrap_or(proto::RootScreenAudioScope::System)
}

fn error(value: i32) -> proto::RootScreenAudioError {
    proto::RootScreenAudioError::try_from(value).unwrap_or(proto::RootScreenAudioError::Internal)
}

fn state(value: i32) -> proto::RootScreenAudioState {
    proto::RootScreenAudioState::try_from(value).unwrap_or(proto::RootScreenAudioState::Failed)
}

pub(crate) fn stats_message(stats: &sys::RootScreenAudioStatsData) -> proto::RootScreenAudioStats {
    proto::RootScreenAudioStats {
        state: state(stats.state) as i32,
        scope: scope(stats.scope) as i32,
        capture_rate: stats.capture_rate,
        capture_channels: stats.capture_channels,
        captured_frames: stats.captured_frames,
        silent_capture_frames: stats.silent_capture_frames,
        discontinuities: stats.discontinuities,
        overflow_frames: stats.overflow_frames,
        write_max_gap_us: stats.write_max_gap_us,
        capture_stalls: stats.capture_stalls,
        pulls: stats.pulls,
        pull_max_gap_us: stats.pull_max_gap_us,
        sent_frames: stats.sent_frames,
        silence_frames: stats.silence_frames,
        underruns: stats.underruns,
        gaps: stats.gaps,
        trimmed_frames: stats.trimmed_frames,
        skipped_frames: stats.skipped_frames,
        clipped_samples: stats.clipped_samples,
        sent_sumsq: stats.sent_sumsq as f64,
        sent_samples: stats.sent_samples,
        level_ms: if stats.level_frames < 0 {
            -1.0
        } else {
            stats.level_frames as f32 / FRAMES_PER_MS
        },
        target_ms: stats.target_frames as f32 / FRAMES_PER_MS,
        drift_ppm: stats.drift_ppm,
        restarts: stats.restarts,
        last_error: error(stats.last_error) as i32,
        last_error_message: if stats.last_error_message.is_empty() {
            None
        } else {
            Some(stats.last_error_message.clone())
        },
    }
}

/// Stores the capture's handle, starts it and polls its events into `emit`
/// while the handle lives.
pub(crate) fn start_capture(
    server: &'static FfiServer,
    engine: Arc<dyn ScreenAudioEngine>,
    target: Option<&proto::RootScreenAudioTarget>,
    emit: impl Fn(proto::RootScreenAudioEvent) + Send + 'static,
) -> proto::RootScreenAudioStartResponse {
    let handle_id = server.next_id();
    let async_id = server.next_id();
    engine.start(&target_spec(target), async_id);
    spawn_event_pump(server, handle_id, Arc::downgrade(&engine), emit);
    server.store_handle(handle_id, FfiScreenAudio { handle_id, engine });
    proto::RootScreenAudioStartResponse {
        capture_handle: Some(handle_id),
        async_id: Some(async_id),
        error: None,
    }
}

fn spawn_event_pump(
    server: &'static FfiServer,
    handle_id: FfiHandleId,
    engine: Weak<dyn ScreenAudioEngine>,
    emit: impl Fn(proto::RootScreenAudioEvent) + Send + 'static,
) {
    server.async_runtime.spawn(async move {
        let mut poll =
            tokio::time::interval_at(tokio::time::Instant::now() + FIRST_POLL_DELAY, EVENT_POLL);
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            poll.tick().await;
            let Some(engine) = engine.upgrade() else {
                break;
            };
            for event in engine.take_events() {
                if let Some(message) = event_message(&event) {
                    emit(proto::RootScreenAudioEvent {
                        capture_handle: handle_id,
                        message: Some(message),
                    });
                }
            }
        }
    });
}

/// Attaches a native capture to the source and starts it (returns at once).
pub fn on_start(
    server: &'static FfiServer,
    req: proto::RootScreenAudioStartRequest,
) -> FfiResult<proto::RootScreenAudioStartResponse> {
    if ROOTAPP_AUDIO_CAPS & CAP_SCREEN_AUDIO == 0 {
        return Ok(refused(NOT_SUPPORTED));
    }
    let capture = match new_capture(server, req.source_handle) {
        Ok(capture) => capture,
        Err(error) => return Ok(refused(error)),
    };
    let engine: Arc<dyn ScreenAudioEngine> = Arc::new(capture);
    Ok(start_capture(server, engine, req.target.as_ref(), move |event| {
        if let Err(e) = server.send_event(event.into()) {
            log::warn!("failed to send RootScreenAudioEvent: {}", e);
        }
    }))
}

/// Stops the OS capture; RootScreenAudioEvent.stopped follows.
pub fn on_stop(
    server: &'static FfiServer,
    req: proto::RootScreenAudioStopRequest,
) -> FfiResult<proto::RootScreenAudioStopResponse> {
    let capture = match server.retrieve_handle::<FfiScreenAudio>(req.capture_handle) {
        Ok(capture) => capture,
        Err(e) => {
            return Ok(proto::RootScreenAudioStopResponse {
                async_id: None,
                error: Some(format!("capture: {e}")),
            })
        }
    };
    let async_id = server.next_id();
    capture.engine.stop(async_id);
    Ok(proto::RootScreenAudioStopResponse { async_id: Some(async_id), error: None })
}

pub fn on_stats(
    server: &'static FfiServer,
    req: proto::RootScreenAudioStatsRequest,
) -> FfiResult<proto::RootScreenAudioStatsResponse> {
    Ok(match server.retrieve_handle::<FfiScreenAudio>(req.capture_handle) {
        Ok(capture) => proto::RootScreenAudioStatsResponse {
            stats: Some(stats_message(&capture.engine.stats())),
            error: None,
        },
        Err(e) => proto::RootScreenAudioStatsResponse {
            stats: None,
            error: Some(format!("capture: {e}")),
        },
    })
}

#[cfg(test)]
mod tests {
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };

    use parking_lot::Mutex;

    use super::*;
    use crate::FFI_SERVER;

    #[derive(Default)]
    struct Recorder {
        starts: Mutex<Vec<(sys::RootScreenAudioTargetSpec, u64)>>,
        stops: Mutex<Vec<u64>>,
        events: Mutex<Vec<sys::RootScreenAudioEventData>>,
    }

    impl ScreenAudioEngine for Recorder {
        fn start(&self, target: &sys::RootScreenAudioTargetSpec, async_id: u64) {
            self.starts.lock().push((target.clone(), async_id));
            self.events.lock().push(sys::RootScreenAudioEventData {
                kind: EVENT_STARTED,
                async_id,
                scope: 1,
                description: "process 7 (game.exe) and its children".into(),
                sample_rate: 48_000,
                channels: 2,
                ..Default::default()
            });
        }

        fn stop(&self, async_id: u64) {
            self.stops.lock().push(async_id);
            self.events.lock().push(sys::RootScreenAudioEventData {
                kind: EVENT_STOPPED,
                async_id,
                ..Default::default()
            });
        }

        fn stats(&self) -> sys::RootScreenAudioStatsData {
            sys::RootScreenAudioStatsData {
                state: 2,
                level_frames: 960,
                target_frames: 1440,
                ..Default::default()
            }
        }

        fn take_events(&self) -> Vec<sys::RootScreenAudioEventData> {
            std::mem::take(&mut *self.events.lock())
        }
    }

    fn new_source(rate: u32, channels: u32, queue_ms: u32) -> FfiHandleId {
        FfiAudioSource::setup(
            &FFI_SERVER,
            proto::NewAudioSourceRequest {
                r#type: proto::AudioSourceType::AudioSourceNative as i32,
                sample_rate: Some(rate),
                num_channels: Some(channels),
                queue_size_ms: Some(queue_ms),
                ..Default::default()
            },
        )
        .unwrap()
        .handle
        .id
    }

    fn recv_until(
        rx: &mpsc::Receiver<proto::RootScreenAudioEvent>,
        timeout: Duration,
        mut matches: impl FnMut(&proto::root_screen_audio_event::Message) -> bool,
    ) -> Option<proto::RootScreenAudioEvent> {
        let deadline = Instant::now() + timeout;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(event) if event.message.as_ref().is_some_and(&mut matches) => {
                    return Some(event)
                }
                Ok(_) => {}
                Err(_) => return None,
            }
        }
        None
    }

    #[test]
    fn screen_audio_target_maps_every_kind() {
        use proto::root_screen_audio_target::Target;
        let system = target_spec(None);
        assert_eq!((system.kind, system.process_id, system.window_id), (0, 0, 0));
        let explicit = target_spec(Some(&proto::RootScreenAudioTarget {
            target: Some(Target::System(true)),
            window_title: None,
        }));
        assert_eq!(explicit.kind, 0);
        let process = target_spec(Some(&proto::RootScreenAudioTarget {
            target: Some(Target::ProcessId(4242)),
            window_title: None,
        }));
        assert_eq!((process.kind, process.process_id), (1, 4242));
        let window = target_spec(Some(&proto::RootScreenAudioTarget {
            target: Some(Target::WindowId(0x1234)),
            window_title: Some("x".repeat(2000)),
        }));
        assert_eq!((window.kind, window.window_id), (2, 0x1234));
        assert_eq!(window.window_title.chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn screen_audio_events_map_to_proto() {
        use proto::root_screen_audio_event::Message;
        let started = event_message(&sys::RootScreenAudioEventData {
            kind: EVENT_STARTED,
            async_id: 5,
            scope: 2,
            description: "application 'Music'".into(),
            sample_rate: 44_100,
            channels: 1,
            ..Default::default()
        });
        assert_eq!(
            started,
            Some(Message::Started(proto::RootScreenAudioStarted {
                async_id: 5,
                scope: proto::RootScreenAudioScope::Application as i32,
                description: "application 'Music'".into(),
                sample_rate: 44_100,
                channels: 1,
            }))
        );
        let failed = event_message(&sys::RootScreenAudioEventData {
            kind: EVENT_FAILED,
            error: 4,
            message: "device lost".into(),
            retrying: true,
            ..Default::default()
        });
        assert_eq!(
            failed,
            Some(Message::Failed(proto::RootScreenAudioFailed {
                async_id: 0,
                error: proto::RootScreenAudioError::DeviceLost as i32,
                message: "device lost".into(),
                retrying: true,
            }))
        );
        assert_eq!(
            event_message(&sys::RootScreenAudioEventData {
                kind: EVENT_STOPPED,
                async_id: 9,
                ..Default::default()
            }),
            Some(Message::Stopped(proto::RootScreenAudioStopped { async_id: 9 }))
        );
        assert_eq!(
            event_message(&sys::RootScreenAudioEventData { kind: 77, ..Default::default() }),
            None
        );
        // Unknown native codes map to safe values.
        let Some(Message::Failed(odd)) = event_message(&sys::RootScreenAudioEventData {
            kind: EVENT_FAILED,
            error: 99,
            ..Default::default()
        }) else {
            panic!("not a failure");
        };
        assert_eq!(odd.error, proto::RootScreenAudioError::Internal as i32);
    }

    #[test]
    fn screen_audio_stats_convert_frames_to_ms() {
        let stats = stats_message(&sys::RootScreenAudioStatsData {
            state: 2,
            scope: 1,
            capture_rate: 48_000,
            capture_channels: 2,
            sent_sumsq: 12_345,
            level_frames: 1_200,
            target_frames: 960,
            drift_ppm: -150,
            restarts: 2,
            last_error: 4,
            last_error_message: "device lost".into(),
            ..Default::default()
        });
        assert_eq!(stats.state, proto::RootScreenAudioState::Running as i32);
        assert_eq!(stats.scope, proto::RootScreenAudioScope::Process as i32);
        assert_eq!(stats.level_ms, 25.0);
        assert_eq!(stats.target_ms, 20.0);
        assert_eq!(stats.drift_ppm, -150);
        assert_eq!(stats.sent_sumsq, 12_345.0);
        assert_eq!(stats.last_error, proto::RootScreenAudioError::DeviceLost as i32);
        assert_eq!(stats.last_error_message.as_deref(), Some("device lost"));
        let before = stats_message(&sys::RootScreenAudioStatsData {
            level_frames: -1,
            ..Default::default()
        });
        assert_eq!(before.level_ms, -1.0);
        assert_eq!(before.state, proto::RootScreenAudioState::Starting as i32);
        assert_eq!(before.last_error_message, None);
    }

    #[test]
    fn screen_audio_handle_routes_requests_and_events() {
        let recorder = Arc::new(Recorder::default());
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let response = start_capture(
            &FFI_SERVER,
            recorder.clone(),
            Some(&proto::RootScreenAudioTarget {
                target: Some(proto::root_screen_audio_target::Target::ProcessId(7)),
                window_title: None,
            }),
            move |event| {
                let _ = tx.lock().send(event);
            },
        );
        let handle = response.capture_handle.unwrap();
        let async_id = response.async_id.unwrap();
        assert_eq!(response.error, None);
        assert_eq!(recorder.starts.lock()[0].0.process_id, 7);
        assert_eq!(recorder.starts.lock()[0].1, async_id);

        let started = recv_until(&rx, Duration::from_secs(2), |m| {
            matches!(m, proto::root_screen_audio_event::Message::Started(_))
        })
        .expect("started");
        assert_eq!(started.capture_handle, handle);

        let stats =
            on_stats(&FFI_SERVER, proto::RootScreenAudioStatsRequest { capture_handle: handle })
                .unwrap();
        assert_eq!(stats.stats.unwrap().target_ms, 30.0);

        let stop =
            on_stop(&FFI_SERVER, proto::RootScreenAudioStopRequest { capture_handle: handle })
                .unwrap();
        let stopped = recv_until(&rx, Duration::from_secs(2), |m| {
            matches!(m, proto::root_screen_audio_event::Message::Stopped(_))
        })
        .expect("stopped");
        let Some(proto::root_screen_audio_event::Message::Stopped(stopped)) = stopped.message
        else {
            unreachable!()
        };
        assert_eq!(Some(stopped.async_id), stop.async_id);

        // Dropping the handle ends the pump and frees the engine.
        assert!(FFI_SERVER.drop_handle(handle));
        let deadline = Instant::now() + Duration::from_secs(2);
        while Arc::strong_count(&recorder) > 1 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(Arc::strong_count(&recorder), 1);
        assert!(on_stop(&FFI_SERVER, proto::RootScreenAudioStopRequest { capture_handle: handle })
            .unwrap()
            .error
            .is_some());
        assert!(on_stats(
            &FFI_SERVER,
            proto::RootScreenAudioStatsRequest { capture_handle: handle }
        )
        .unwrap()
        .error
        .is_some());
    }

    #[test]
    fn screen_audio_start_refuses_unusable_sources() {
        if ROOTAPP_AUDIO_CAPS & CAP_SCREEN_AUDIO == 0 {
            let response = on_start(
                &FFI_SERVER,
                proto::RootScreenAudioStartRequest { source_handle: 1, target: None },
            )
            .unwrap();
            assert_eq!(response.error.as_deref(), Some(NOT_SUPPORTED));
            return;
        }
        let start = |source_handle| {
            on_start(
                &FFI_SERVER,
                proto::RootScreenAudioStartRequest { source_handle, target: None },
            )
            .unwrap()
        };
        let unknown = start(987_654_321);
        assert!(unknown.error.unwrap().starts_with("source: "));
        assert_eq!(unknown.capture_handle, None);

        let platform = FfiAudioSource::setup(
            &FFI_SERVER,
            proto::NewAudioSourceRequest {
                r#type: proto::AudioSourceType::AudioSourcePlatform as i32,
                ..Default::default()
            },
        )
        .unwrap()
        .handle
        .id;
        assert_eq!(
            start(platform).error.as_deref(),
            Some("the audio source is not a NATIVE source")
        );

        let unqueued = new_source(48_000, 2, 0);
        assert_eq!(
            start(unqueued).error.as_deref(),
            Some("the audio source has no queue (queue_size_ms 0)")
        );
        let narrow = new_source(16_000, 1, 1000);
        assert_eq!(
            start(narrow).error.as_deref(),
            Some("the audio source must be 48 kHz with 1 or 2 channels")
        );
        for handle in [platform, unqueued, narrow] {
            FFI_SERVER.drop_handle(handle);
        }
    }

    // The real request path end to end with the fake tone backend (the only
    // test here that uses it).
    #[test]
    fn screen_audio_native_capture_runs_end_to_end_with_a_fake_backend() {
        if ROOTAPP_AUDIO_CAPS & CAP_SCREEN_AUDIO == 0 {
            return;
        }
        sys::root_screen_audio_use_fake_backend(true);
        let source = new_source(48_000, 2, 1000);
        let capture = new_capture(&FFI_SERVER, source).unwrap();
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let response = start_capture(&FFI_SERVER, Arc::new(capture), None, move |event| {
            let _ = tx.lock().send(event);
        });
        let handle = response.capture_handle.unwrap();
        let started = recv_until(&rx, Duration::from_secs(3), |m| {
            matches!(m, proto::root_screen_audio_event::Message::Started(_))
        })
        .expect("started");
        let Some(proto::root_screen_audio_event::Message::Started(started)) = started.message
        else {
            unreachable!()
        };
        assert_eq!(started.async_id, response.async_id.unwrap());
        assert_eq!(started.description, "fake tone backend");

        // A second capture on the same source is refused by the request.
        let second = on_start(
            &FFI_SERVER,
            proto::RootScreenAudioStartRequest { source_handle: source, target: None },
        )
        .unwrap();
        assert_eq!(
            second.error.as_deref(),
            Some("the audio source already has a screen-audio capture")
        );

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut stats = proto::RootScreenAudioStats::default();
        while Instant::now() < deadline {
            stats = on_stats(
                &FFI_SERVER,
                proto::RootScreenAudioStatsRequest { capture_handle: handle },
            )
            .unwrap()
            .stats
            .unwrap();
            if stats.sent_frames >= 9_600 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(stats.sent_frames >= 9_600, "{stats:?}");
        assert_eq!(stats.capture_rate, 48_000);
        assert!(stats.sent_sumsq > 0.0);

        let stop =
            on_stop(&FFI_SERVER, proto::RootScreenAudioStopRequest { capture_handle: handle })
                .unwrap();
        let stopped = recv_until(&rx, Duration::from_secs(3), |m| {
            matches!(m, proto::root_screen_audio_event::Message::Stopped(_))
        })
        .expect("stopped");
        let Some(proto::root_screen_audio_event::Message::Stopped(stopped)) = stopped.message
        else {
            unreachable!()
        };
        assert_eq!(Some(stopped.async_id), stop.async_id);
        let begin = Instant::now();
        assert!(FFI_SERVER.drop_handle(handle));
        assert!(begin.elapsed() < Duration::from_millis(100));
        FFI_SERVER.drop_handle(source);
    }
}
