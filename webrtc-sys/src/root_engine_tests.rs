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

//! libwebrtc's own voice engine (WebRtcVoiceEngine, AudioState and
//! AudioTransportImpl in a ConnectionContext) over AdmProxy and a fake platform
//! ADM, wired as the factory wires it, with its voice engine hold
//! (`RootEngineHarness`). m150 initializes the engine with the first
//! PeerConnection's reference to the context and terminates it with the last;
//! the tests stand in for PeerConnections with those references and create
//! none. No audio device is opened.

use std::time::{Duration, Instant};

use crate::audio_device_controller::ffi::{
    new_root_engine_harness, new_root_engine_harness_with_lazy_platform, RootAudioControlUpdate,
    RootEngineHarness,
};

const RATE: i32 = 48_000;
const FRAME: usize = 480;
const HOLD_FRAMES: usize = 20;
const MONITOR_APPLIED: u8 = 0;
const EFFECT: u32 = 1;

/// Frame `index` of a 180 Hz voice with two harmonics in syllables (200 ms
/// on, 100 ms off, on when the startup hold ends), as a stereo device
/// delivers it (both channels).
fn voice(index: usize) -> Vec<i16> {
    (0..FRAME)
        .flat_map(|i| {
            let t = (index * FRAME + i) as f64 / f64::from(RATE);
            let on = ((t + 0.1) % 0.3) < 0.2;
            let w = 2.0 * std::f64::consts::PI * 180.0 * t;
            let s = if on {
                4000.0 * (w.sin() + 0.5 * (2.0 * w).sin() + 0.25 * (3.0 * w).sin()) / 1.75
            } else {
                0.0
            };
            let s = s.round() as i16;
            [s, s]
        })
        .collect()
}

fn silent(samples: &[i16]) -> bool {
    samples.iter().all(|&s| s == 0)
}

fn sumsq(samples: &[i16]) -> f64 {
    samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum()
}

fn wait_until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn control(h: &RootEngineHarness, update: RootAudioControlUpdate) {
    assert!(h.controller().set_root_audio_control(&update), "{update:?}");
}

/// The gate fully open, no voice gain and no speech gate, so what a call sends
/// follows the voice once the startup hold ends.
fn open_gate(h: &RootEngineHarness) {
    control(
        h,
        RootAudioControlUpdate {
            has_auto_gain: true,
            auto_gain: false,
            has_auto_sensitivity: true,
            auto_sensitivity: false,
            has_gate_threshold: true,
            gate_threshold: 0.0,
            has_speech_detection: true,
            speech_detection: 0,
            ..Default::default()
        },
    );
}

/// One render; its left channel, checked against the right.
fn render_left(h: &RootEngineHarness, what: &str) -> Vec<i16> {
    let played = h.render(RATE, 2);
    assert_eq!(played.len(), 2 * FRAME, "{what}");
    let left: Vec<i16> = played.iter().step_by(2).copied().collect();
    let right: Vec<i16> = played.iter().skip(1).step_by(2).copied().collect();
    assert_eq!(left, right, "{what}");
    left
}

/// Mic test frames `from..to`, one capture and one render each, as the
/// device's threads call; the left channel of every render.
fn mic_test(h: &RootEngineHarness, from: usize, to: usize) -> Vec<Vec<i16>> {
    (from..to)
        .map(|frame| {
            h.capture(RATE, 2, &voice(frame));
            render_left(h, &format!("frame {frame}"))
        })
        .collect()
}

/// A chime through the effect bus: the device plays exactly what the playout
/// mixer made, so the render went through libwebrtc's transport.
fn chime_plays(h: &RootEngineHarness, what: &str) {
    let c = h.controller();
    assert_eq!(c.play_root_audio_effect(EFFECT, 0.5), 0, "{what}: queued");
    let before = c.root_audio_stats();
    let played = h.render(RATE, 2);
    let after = c.root_audio_stats();
    assert_eq!(after.mix_calls - before.mix_calls, 1, "{what}: one mix per render");
    assert!(sumsq(&played) > 0.0, "{what}: the chime is heard");
    assert_eq!(
        sumsq(&played),
        after.mix_out_sumsq - before.mix_out_sumsq,
        "{what}: exactly the mixer's output"
    );
    c.stop_root_audio_effects();
}

/// Recorded frames reach the APM and the capture processor.
fn capture_runs(h: &RootEngineHarness, what: &str) {
    let c = h.controller();
    let before = c.root_audio_stats().processed_frames;
    for frame in 0..3 {
        h.capture(RATE, 2, &voice(frame));
    }
    assert_eq!(c.root_audio_stats().processed_frames - before, 3, "{what}: frames processed");
}

/// The Settings mic test in a process that never joined a call: the monitor
/// plays what a call would send (libwebrtc's APM and the capture processor),
/// outside the echo canceller's reference, with no PeerConnection ever made.
/// Without the hold libwebrtc has no transport registered: nothing is
/// processed and the device plays the tap's silence.
#[test]
fn root_engine_mic_test_plays_processed_capture_before_any_call() {
    let h = new_root_engine_harness();
    let c = h.controller();
    assert!(h.acquire());
    open_gate(&h);
    assert_eq!(h.monitor(true), MONITOR_APPLIED);
    assert!(h.platform_playing() && h.platform_recording(), "both devices open for it");

    const FRAMES: usize = 60;
    let start = c.root_audio_stats();
    let mut played = Vec::new();
    let mut sent_through = 0.0;
    for frame in 0..FRAMES {
        h.capture(RATE, 2, &voice(frame));
        if frame == FRAMES - 3 {
            sent_through = c.root_audio_stats().sent_sumsq - start.sent_sumsq;
        }
        played.push(render_left(&h, &format!("frame {frame}")));
    }
    let stats = c.root_audio_stats();
    assert_eq!(
        stats.processed_frames - start.processed_frames,
        FRAMES as u64,
        "every frame went through libwebrtc's APM and the capture processor"
    );
    assert_eq!((stats.process_rate, stats.process_channels), (48_000, 1), "as a call sends");
    assert_eq!(
        stats.mix_calls - start.mix_calls,
        FRAMES as u64,
        "every render went through libwebrtc's transport and the mixer"
    );
    assert_eq!((stats.monitor_frames, stats.monitor_dropped_frames), (FRAMES as u64 - 2, 0));
    assert_eq!(stats.startup_hold_frames, HOLD_FRAMES as u64);
    assert!(
        played[..HOLD_FRAMES + 2].iter().all(|f| silent(f)),
        "30 ms queued, then the startup hold"
    );
    let heard: f64 = played.iter().map(|f| sumsq(f)).sum();
    let heard_frames = played.iter().filter(|f| sumsq(f) > 0.0).count();
    assert!(heard_frames >= 20, "{heard_frames} frames carried the voice");
    // Frames 2.. played what was sent from frame 0 to FRAMES - 3, rounded.
    let rounding: f64 = played.iter().flatten().map(|&s| f64::from(s).abs() + 0.25).sum();
    assert!(
        (heard - sent_through).abs() <= rounding,
        "played {heard} vs sent {sent_through} (bound {rounding})"
    );

    // Muted, a call sends silence, and the monitor plays it.
    control(
        &h,
        RootAudioControlUpdate { has_mic_muted: true, mic_muted: true, ..Default::default() },
    );
    mic_test(&h, FRAMES, FRAMES + 4);
    assert!(mic_test(&h, FRAMES + 4, FRAMES + 14).iter().all(|f| silent(f)));

    assert_eq!(h.monitor(false), MONITOR_APPLIED);
    assert!(!h.platform_playing() && !h.platform_recording());
    h.release();
}

/// The last platform reference with no PeerConnection terminates the voice
/// engine; the next reference initializes it again and the mic test works.
/// Terminate used to shut the proxy and both ADMs down for good.
#[test]
fn root_engine_mic_test_works_again_after_the_engine_terminated() {
    let h = new_root_engine_harness();
    let c = h.controller();
    assert!(h.acquire());
    h.release();
    let calls = h.take_calls();
    assert!(!calls.iter().any(|c| c == "Terminate"), "the device module stays up: {calls:?}");

    assert!(h.acquire());
    open_gate(&h);
    assert_eq!(h.monitor(true), MONITOR_APPLIED);
    assert!(h.platform_playing() && h.platform_recording());
    let start = c.root_audio_stats();
    let played = mic_test(&h, 0, 40);
    let stats = c.root_audio_stats();
    assert_eq!(stats.processed_frames - start.processed_frames, 40);
    assert_eq!(stats.mix_calls - start.mix_calls, 40);
    assert_eq!(stats.monitor_frames - start.monitor_frames, 38);
    assert!(played.iter().filter(|f| sumsq(f) > 0.0).count() >= 10);
    h.monitor(false);
    h.release();
}

/// A call ends while its lease (and the playout hold) stays: the last
/// PeerConnection goes, the leave chime still plays through libwebrtc's
/// transport, and the next call on the same device module plays and captures.
#[test]
fn root_engine_a_call_ending_under_a_lease_leaves_the_device_working() {
    let h = new_root_engine_harness();
    let c = h.controller();
    assert!(h.acquire());
    assert!(c.register_root_audio_effect(EFFECT, &vec![8000i16; 4800]));
    h.hold(true);
    chime_plays(&h, "join chime before the call's PeerConnection");

    h.open_peer_connection();
    h.engine_playout(true);
    h.engine_recording(true);
    assert!(h.platform_playing() && h.platform_recording());
    chime_plays(&h, "first call");
    capture_runs(&h, "first call");

    // AudioState stops the call's streams, then the PeerConnection goes.
    h.engine_playout(false);
    h.engine_recording(false);
    h.close_peer_connection();
    assert!(h.platform_playing(), "the hold keeps playout open");
    assert!(!h.platform_recording());
    chime_plays(&h, "leave chime after the last PeerConnection");

    h.open_peer_connection();
    h.engine_playout(true);
    h.engine_recording(true);
    assert!(h.platform_playing() && h.platform_recording());
    chime_plays(&h, "next call");
    capture_runs(&h, "next call");

    h.engine_playout(false);
    h.engine_recording(false);
    h.close_peer_connection();
    h.release();
    assert!(!h.platform_playing() && !h.platform_recording());
}

/// The mic test runs while a call's PeerConnection goes away (one closing
/// late): the monitor goes on hearing processed capture.
#[test]
fn root_engine_mic_test_runs_on_across_a_call_ending() {
    let h = new_root_engine_harness();
    let c = h.controller();
    assert!(h.acquire());
    open_gate(&h);
    h.open_peer_connection();
    assert_eq!(h.monitor(true), MONITOR_APPLIED);
    let start = c.root_audio_stats();
    mic_test(&h, 0, 30);

    h.close_peer_connection();
    let played = mic_test(&h, 30, 60);
    let stats = c.root_audio_stats();
    assert!(stats.monitor_active);
    assert_eq!(stats.processed_frames - start.processed_frames, 60);
    assert_eq!(stats.mix_calls - start.mix_calls, 60);
    assert_eq!(
        (stats.monitor_frames - start.monitor_frames, stats.monitor_dropped_frames),
        (58, 0)
    );
    assert!(played.iter().filter(|f| sumsq(f) > 0.0).count() >= 10, "heard after the call");
    h.monitor(false);
    h.release();
}

/// Managed calls (no platform reference) one after the other on one factory:
/// each gets the synthetic pump through libwebrtc's transport, and the
/// platform device is never opened.
#[test]
fn root_engine_managed_calls_never_open_the_device_and_each_gets_the_pump() {
    let h = new_root_engine_harness();
    let c = h.controller();
    h.take_calls();
    for call in 0..3 {
        h.open_peer_connection();
        h.engine_playout(true);
        h.engine_recording(true);
        assert!(h.synthetic_playing(), "call {call}");
        assert!(!h.platform_playing() && !h.platform_recording(), "call {call}");
        let before = c.root_audio_stats().mix_calls;
        wait_until(&format!("call {call}'s pump pulls the mixer"), || {
            c.root_audio_stats().mix_calls >= before + 3
        });
        h.engine_playout(false);
        h.engine_recording(false);
        assert!(!h.synthetic_playing(), "call {call}");
        h.close_peer_connection();
    }
    let calls = h.take_calls();
    assert!(
        calls.iter().all(|c| !c.starts_with("Init") && !c.starts_with("Start") && c != "Terminate"),
        "{calls:?}"
    );
}

/// AGC2 from the controls, through the factory's wiring: the session start
/// builds the session's AGC2, a control with a gain reset and settings builds
/// another at once (off the audio thread), the captured voice runs through it,
/// and the stats report it with its settings until a session start puts the
/// defaults back.
#[test]
fn root_engine_runs_agc2_from_the_controls() {
    let h = new_root_engine_harness();
    let c = h.controller();
    assert!(h.acquire());
    assert_eq!(c.root_audio_stats().agc2_builds, 1, "a session starts with AGC2");
    control(
        &h,
        RootAudioControlUpdate {
            has_auto_sensitivity: true,
            auto_sensitivity: false,
            has_gate_threshold: true,
            gate_threshold: 0.0,
            has_gain_engine: true,
            gain_engine: 1,
            has_gain_reset_db: true,
            gain_reset_db: 12.0,
            has_agc2_max_gain_db: true,
            agc2_max_gain_db: 99.0,
            ..Default::default()
        },
    );
    let built = c.root_audio_stats();
    assert_eq!(built.agc2_builds, 2, "built with the control, before any frame");
    assert_eq!(
        (built.agc2_headroom_db, built.agc2_max_gain_db),
        (5.0, 50.0),
        "the default headroom, the maximum clamped"
    );
    assert_eq!(
        (built.agc2_max_gain_change_db_per_second, built.agc2_max_output_noise_dbfs),
        (6.0, -50.0)
    );
    assert_eq!(h.monitor(true), MONITOR_APPLIED);
    mic_test(&h, 0, 60);
    let stats = c.root_audio_stats();
    assert_eq!((stats.gain_engine, stats.agc2_builds), (1, 2));
    assert!(
        stats.voice_gain_db > 0.0 && stats.voice_gain_db <= 12.5,
        "AGC2's gain from the 12 dB reset: {}",
        stats.voice_gain_db
    );
    assert!(stats.voice_gain_floor_db.is_finite());
    h.monitor(false);
    c.reset_root_audio(false);
    assert!(c.root_audio_stats().agc2_max_gain_db == 30.0, "a session start forgets the settings");
    h.release();
}

/// A PeerConnection's voice engine init (m150: adm_helpers::Init, the APM
/// options, RegisterAudioCallback, all on the worker) while the device thread
/// is inside a device open that holds the device mutex (StartPlayout here; a
/// Windows InitPlayout or a 2 s StopPlayout in the field): it never waits for
/// the device, so a room connect is not held up by one.
#[test]
fn root_engine_a_peer_connection_never_waits_for_a_device_open() {
    let h = new_root_engine_harness();
    let taken_ms = h.peer_connection_while_playout_opens(2_000);
    assert!(taken_ms >= 0, "the voice engine's init waited for the device open");
    assert!(taken_ms < 1_000, "{taken_ms} ms");
    assert!(h.platform_playing(), "the open went on once released");

    // The engine works once the device is open: the call's streams play
    // through libwebrtc's transport.
    h.engine_playout(true);
    let played = h.render(RATE, 2);
    assert_eq!(played.len(), 2 * FRAME);
    h.engine_playout(false);
    h.close_peer_connection();
}

/// The factory's platform ADM is created at the first platform reference, on
/// the device thread, not with the factory (whose creation a room connect
/// waits for). A PeerConnection made while that creation takes long (the
/// Windows ADM's two endpoint sweeps: over 10 s on a machine with 25
/// endpoints) does not wait for it; the handle's creation does.
#[test]
fn root_engine_the_platform_adm_is_created_with_the_first_reference_without_holding_up_a_peer_connection(
) {
    let h = new_root_engine_harness_with_lazy_platform();
    h.open_peer_connection();
    h.engine_playout(true);
    assert!(h.synthetic_playing(), "a managed call");
    h.engine_playout(false);
    h.close_peer_connection();
    assert_eq!(h.platform_creations(), 0, "a managed call never creates the platform ADM");
    assert_eq!(h.controller().playout_devices(), 0, "nothing to list before it exists");

    let timing = h.peer_connection_while_the_platform_adm_is_created(2_000);
    assert!(timing.creation_held, "the handle's creation reached the platform ADM's");
    assert!(timing.peer_connection_ms >= 0, "the PeerConnection waited for the creation");
    assert!(timing.peer_connection_ms < 1_000, "{} ms", timing.peer_connection_ms);
    assert!(timing.acquire_waited_for_the_creation);
    assert!(timing.acquired);
    assert_eq!(h.platform_creations(), 1);
    assert_eq!(h.controller().playout_devices(), 1, "listed once created");

    // The platform engine works as with an eager platform ADM.
    h.hold(true);
    assert!(h.platform_playing());
    h.engine_playout(true);
    assert_eq!(h.render(RATE, 2).len(), 2 * FRAME);
    h.engine_playout(false);
    h.hold(false);
    h.close_peer_connection();
    h.release();
    assert!(h.acquire(), "a second handle reuses it");
    assert_eq!(h.platform_creations(), 1);
    h.release();
}
