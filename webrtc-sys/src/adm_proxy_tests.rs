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

//! AdmProxy's desktop state machine over a fake platform ADM that records the
//! calls it gets. No audio device is opened.

#![cfg(not(any(target_os = "ios", target_os = "android")))]

use std::time::{Duration, Instant};

use cxx::UniquePtr;

use crate::audio_device_controller::{
    ffi::{
        new_adm_proxy_harness, AdmProxyHarness, AudioDeviceOutcome, AudioDeviceSelection,
        AudioDeviceSelectionKind, RootAudioControlUpdate,
    },
    ADM_RESULT_DEVICE_NOT_FOUND, ADM_RESULT_NOT_ATTEMPTED,
};

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

fn harness(playout: &[&str], recording: &[&str]) -> UniquePtr<AdmProxyHarness> {
    let h = new_adm_proxy_harness(&strings(playout), &strings(recording));
    h.take_calls();
    h
}

/// A harness in platform mode with both directions enabled, calls cleared.
fn platform(playout: &[&str], recording: &[&str]) -> UniquePtr<AdmProxyHarness> {
    let h = harness(playout, recording);
    h.acquire();
    h.enable(true, true);
    h.flush();
    assert_eq!(h.take_calls(), Vec::<String>::new());
    h
}

fn calls(h: &AdmProxyHarness) -> Vec<String> {
    h.flush();
    h.take_calls()
}

fn default_device(direction: &str) -> String {
    if cfg!(windows) {
        format!("Set{direction}Device(default)")
    } else {
        format!("Set{direction}Device(0)")
    }
}

fn guid(id: &str) -> AudioDeviceSelection {
    AudioDeviceSelection { kind: AudioDeviceSelectionKind::Guid, id: id.to_string(), index: 0 }
}

fn unset() -> AudioDeviceSelection {
    AudioDeviceSelection { kind: AudioDeviceSelectionKind::Default, id: String::new(), index: 0 }
}

fn select_playout(h: &AdmProxyHarness, id: &str) -> AudioDeviceOutcome {
    let outcome = h.select(&unset(), false, &guid(id), true);
    assert!(outcome.playout.requested);
    assert!(!outcome.recording.requested);
    outcome.playout
}

fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn adm_proxy_hold_alone_opens_playout_without_claiming_it() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.hold(true);
    assert_eq!(calls(&h), [default_device("Playout"), "InitPlayout".into(), "StartPlayout".into()]);
    assert!(h.platform_playing());
    assert!(!h.playing(), "libwebrtc did not start playout");
    assert!(!h.synthetic_playing());
}

#[test]
fn adm_proxy_engine_playout_opens_and_closes_the_device() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.engine_start_playout();
    assert_eq!(calls(&h), [default_device("Playout"), "InitPlayout".into(), "StartPlayout".into()]);
    assert!(h.playing() && h.playout_initialized());

    h.engine_stop_playout();
    assert_eq!(calls(&h), ["StopPlayout"]);
    assert!(!h.playing() && !h.platform_playing() && !h.synthetic_playing());
}

#[test]
fn adm_proxy_releasing_the_hold_leaves_engine_playout_running() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.hold(true);
    h.engine_start_playout();
    calls(&h);

    h.hold(false);
    assert_eq!(calls(&h), Vec::<String>::new());
    assert!(h.playing() && h.platform_playing());

    h.hold(true);
    h.engine_stop_playout();
    assert_eq!(calls(&h), Vec::<String>::new(), "the hold keeps the device open");
    assert!(!h.playing() && h.platform_playing());

    h.hold(false);
    assert_eq!(calls(&h), ["StopPlayout"]);
}

#[test]
fn adm_proxy_select_while_running_restarts_on_the_new_device() {
    let h = platform(&["{a}", "{b}"], &["{mic}"]);
    h.engine_start_playout();
    calls(&h);

    let outcome = select_playout(&h, "{b}");
    assert!(outcome.found && outcome.attempted);
    assert_eq!((outcome.stop_result, outcome.init_result, outcome.start_result), (0, 0, 0));
    assert_eq!(
        h.take_calls(),
        ["StopPlayout", "SetPlayoutDevice(1)", "InitPlayout", "StartPlayout"]
    );
    assert!(h.playing());

    let again = select_playout(&h, "{b}");
    assert!(again.found && !again.attempted, "an unchanged selection is no restart");
    assert_eq!(h.take_calls(), Vec::<String>::new());
}

#[test]
fn adm_proxy_select_while_stopped_only_checks_the_device() {
    let h = platform(&["{spk}"], &["{mic}"]);
    let outcome = h.select(&guid("{mic}"), true, &guid("{gone}"), true);
    assert!(outcome.recording.found && !outcome.recording.attempted);
    assert!(!outcome.playout.found && !outcome.playout.attempted);
    assert_eq!(outcome.playout.init_result, ADM_RESULT_NOT_ATTEMPTED);
    assert_eq!(h.take_calls(), Vec::<String>::new());
}

#[test]
fn adm_proxy_missing_device_fails_the_open_and_stays_closed() {
    let h = platform(&["{a}"], &["{mic}"]);
    h.engine_start_playout();
    calls(&h);

    let outcome = select_playout(&h, "{gone}");
    assert!(!outcome.found && outcome.attempted);
    assert_eq!(outcome.init_result, ADM_RESULT_DEVICE_NOT_FOUND);
    assert_eq!(h.take_calls(), ["StopPlayout"], "never index 0 instead");
    assert!(!h.platform_playing() && !h.playing());
    assert_eq!(h.stats().playout_init_result, ADM_RESULT_DEVICE_NOT_FOUND);

    h.set_devices(&strings(&["{a}", "{gone}"]), &strings(&["{mic}"]));
    h.engine_start_playout();
    assert_eq!(calls(&h), ["SetPlayoutDevice(1)", "InitPlayout", "StartPlayout"]);
    assert!(h.playing());
}

#[test]
fn adm_proxy_init_alone_opens_nothing() {
    let h = platform(&["{spk}"], &["{mic}"]);
    assert_eq!(h.engine_init_playout(), 0);
    assert_eq!(h.engine_init_recording(), 0);
    assert_eq!(calls(&h), Vec::<String>::new());
    assert!(!h.playout_initialized() && !h.recording_initialized());

    h.engine_start_recording();
    assert_eq!(
        calls(&h),
        [default_device("Recording"), "InitRecording".into(), "StartRecording".into()]
    );
    assert!(h.recording() && h.platform_recording());
}

#[test]
fn adm_proxy_never_forwards_mute_or_builtin_effects() {
    let h = new_adm_proxy_harness(&strings(&["{spk}"]), &strings(&["{mic}"]));
    let construction = calls(&h);
    for effect in ["AEC", "AGC", "NS"] {
        assert!(construction.contains(&format!("EnableBuiltIn{effect}(off)")), "{construction:?}");
    }
    assert!(!h.builtin_effects_available());
    assert!(!h.stop_on_mute());

    h.acquire();
    h.enable(true, true);
    h.engine_start_recording();
    calls(&h);
    assert_eq!(h.set_microphone_mute(true), 0);
    assert_eq!(h.set_microphone_mute(false), 0);
    for effect in 0..3 {
        assert_eq!(h.enable_builtin_effect(effect, true), -1, "effect {effect} refused");
        assert_eq!(h.enable_builtin_effect(effect, false), 0);
    }
    assert_eq!(calls(&h), Vec::<String>::new());
}

#[test]
fn adm_proxy_terminate_stops_the_devices_before_clearing_the_callback() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.register_transport();
    h.engine_start_playout();
    h.engine_start_recording();
    calls(&h);

    h.terminate();
    assert_eq!(
        h.take_calls(),
        ["StopPlayout", "StopRecording", "RegisterAudioCallback(null)", "Terminate"]
    );
    assert!(!h.playing() && !h.recording() && !h.synthetic_playing());
}

#[test]
fn adm_proxy_last_release_falls_back_to_synthetic() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.hold(true);
    h.engine_start_playout();
    h.engine_start_recording();
    calls(&h);
    assert!(!h.synthetic_playing());

    h.release();
    assert_eq!(calls(&h), ["StopPlayout", "StopRecording"]);
    assert!(!h.held(), "the hold ends with the last reference");
    assert!(h.synthetic_playing() && h.playing());
    assert!(!h.recording());

    h.acquire();
    assert_eq!(
        calls(&h),
        [
            default_device("Playout"),
            "InitPlayout".into(),
            "StartPlayout".into(),
            default_device("Recording"),
            "InitRecording".into(),
            "StartRecording".into()
        ]
    );
    assert!(!h.synthetic_playing());
}

#[test]
fn adm_proxy_failed_stop_starts_no_second_puller_until_the_retry() {
    let h = platform(&["{a}", "{b}"], &["{mic}"]);
    h.engine_start_playout();
    calls(&h);

    h.fail_next_stop_playout(-1);
    let outcome = select_playout(&h, "{b}");
    assert!(outcome.attempted);
    assert_eq!(outcome.stop_result, -1);
    assert_eq!(outcome.init_result, ADM_RESULT_NOT_ATTEMPTED);
    assert_eq!(h.take_calls(), ["StopPlayout"]);
    assert!(!h.platform_playing() && !h.synthetic_playing());
    assert_eq!(h.stats().device_stop_failures, 1);

    wait_until("the retry reopens playout", || h.platform_playing());
    assert_eq!(calls(&h), ["SetPlayoutDevice(1)", "InitPlayout", "StartPlayout"]);

    h.fail_next_stop_playout(-1);
    h.release();
    assert_eq!(calls(&h), ["StopPlayout"]);
    assert!(!h.synthetic_playing(), "no synthetic pump next to a render thread that may live");
    wait_until("the retry starts the synthetic pump", || h.synthetic_playing());
    assert_eq!(h.stats().device_stop_failures, 2);
}

#[test]
fn adm_proxy_keeps_the_apps_requests_apart_from_libwebrtcs() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.app_playout(true);
    assert_eq!(calls(&h), [default_device("Playout"), "InitPlayout".into(), "StartPlayout".into()]);
    assert!(h.platform_playing() && !h.playing());

    h.engine_start_playout();
    h.app_playout(false);
    assert_eq!(calls(&h), Vec::<String>::new());
    assert!(h.playing());
    h.engine_stop_playout();
    assert_eq!(calls(&h), ["StopPlayout"]);

    h.app_recording(true);
    h.engine_start_recording();
    calls(&h);
    h.engine_stop_recording();
    assert_eq!(calls(&h), Vec::<String>::new(), "the app's recording outlives the send stream");
    assert!(h.platform_recording() && !h.recording());
    h.app_recording(false);
    assert_eq!(calls(&h), ["StopRecording"]);
}

#[test]
fn adm_proxy_reports_delay_and_device_stats_only_for_platform_playout() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.register_transport();
    h.set_device_playout_delay(40);
    assert!(!h.has_device_stats());

    h.engine_start_playout();
    calls(&h);
    assert_eq!(h.playout_delay(), 40);
    assert_eq!(h.device_callback(false, false), 1);
    assert!(h.has_device_stats());

    h.engine_stop_playout();
    calls(&h);
    assert_eq!(h.playout_delay(), 0, "no delay from a stopped device");
    assert_eq!(h.stats().playout_delay_ms, 0);

    h.release();
    calls(&h);
    assert!(!h.has_device_stats(), "managed calls see no platform totals");
}

#[test]
fn adm_proxy_answers_overlapping_device_callbacks_with_silence() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.register_transport();
    h.engine_start_playout();
    h.engine_start_recording();
    calls(&h);

    assert_eq!(h.device_callback(false, true), 1, "the second render pull never reached libwebrtc");
    assert_eq!(h.stats().overlapping_callbacks, 1);
    assert_eq!(h.device_callback(false, false), 1);
    assert_eq!(h.device_callback(true, true), 1, "nor the second capture callback");
    let stats = h.stats();
    assert_eq!(stats.overlapping_callbacks, 2);
    assert_eq!(stats.render_callbacks, 2);
    assert_eq!(stats.capture_callbacks, 1);
}

#[test]
fn adm_proxy_counts_locks_taken_on_audio_threads() {
    let h = platform(&["{spk}"], &["{mic}"]);
    assert_eq!(h.lock_on_audio_thread(), 1);
    assert!(h.stats().audio_thread_lock_violations >= 1);
}

#[test]
fn adm_proxy_turns_the_mixers_gains_on_with_the_first_reference() {
    let h = harness(&["{spk}"], &["{mic}"]);
    assert!(!h.mixer_platform_active());
    h.acquire();
    assert!(h.mixer_platform_active(), "before the device thread's pass");
    h.acquire();
    h.release();
    assert!(h.mixer_platform_active());
    h.release();
    assert!(!h.mixer_platform_active());
}

#[test]
fn adm_proxy_controller_applies_only_the_fields_that_are_set() {
    let h = harness(&["{spk}"], &["{mic}"]);
    let before = h.control_state();
    h.apply_control(&RootAudioControlUpdate {
        has_mic_muted: true,
        mic_muted: true,
        has_master_volume: true,
        master_volume: 0.5,
        ..Default::default()
    });
    let after = h.control_state();
    assert!(after.mic_muted);
    assert_eq!(after.master_volume, 0.5);
    assert_eq!(after.push_to_talk, before.push_to_talk);
    assert_eq!(after.input_volume, before.input_volume);
    assert_eq!(after.deafened, before.deafened);
    assert_eq!(after.gain_reset_seq, before.gain_reset_seq);
}

#[test]
fn adm_proxy_controller_clamps_what_reaches_the_audio_threads() {
    let h = harness(&["{spk}"], &["{mic}"]);
    h.apply_control(&RootAudioControlUpdate {
        has_push_to_talk: true,
        push_to_talk: 9,
        has_input_volume: true,
        input_volume: f32::NAN,
        has_master_volume: true,
        master_volume: 50.0,
        has_deafened: true,
        deafened: true,
        has_denoise_power: true,
        denoise_power: 4.0,
        has_gate_threshold: true,
        gate_threshold: -1.0,
        has_gain_reset_db: true,
        gain_reset_db: 99.0,
        has_auto_gain: true,
        auto_gain: false,
        has_auto_sensitivity: true,
        auto_sensitivity: false,
        ..Default::default()
    });
    let state = h.control_state();
    assert_eq!(state.push_to_talk, 2, "an unknown state closes the gate");
    assert_eq!(state.input_volume, 0.0);
    assert_eq!(state.master_volume, 2.0);
    assert!(state.deafened);
    assert_eq!((state.denoise_power, state.gate_threshold), (1.0, 0.0));
    assert_eq!((state.gain_reset_db, state.gain_reset_seq), (18.0, 1));
    assert!(!state.auto_gain && !state.auto_sensitivity);

    h.apply_control(&RootAudioControlUpdate {
        has_gain_reset_db: true,
        gain_reset_db: f32::INFINITY,
        ..Default::default()
    });
    assert_eq!(h.control_state().gain_reset_seq, 1, "a non-finite command is dropped");

    h.reset_root_audio();
    let state = h.control_state();
    assert!(!state.mic_muted && !state.deafened);
    assert_eq!((state.push_to_talk, state.input_volume, state.master_volume), (0, 1.0, 1.0));
}

#[test]
fn adm_proxy_stats_report_unknown_echo_statistics_without_an_apm() {
    let h = harness(&["{spk}"], &["{mic}"]);
    let stats = h.stats();
    assert!(stats.erl_db.is_nan() && stats.erle_db.is_nan());
    assert!(stats.divergent_filter_fraction.is_nan());
    assert_eq!((stats.processed_frames, stats.effects_played), (0, 0));
}
