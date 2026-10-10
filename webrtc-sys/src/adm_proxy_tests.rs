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
        new_adm_proxy_harness, new_adm_proxy_harness_with_apm, AdmProxyHarness, AudioDeviceOutcome,
        AudioDeviceSelection, AudioDeviceSelectionKind, RootAudioControlUpdate,
        RootDenoiserCompletionItem, RootDenoiserReturnedItem, RootDenoiserStateInfo,
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

    h.reset_root_audio(false);
    let state = h.control_state();
    assert!(!state.mic_muted && !state.deafened);
    assert_eq!((state.push_to_talk, state.input_volume, state.master_volume), (0, 1.0, 1.0));
}

#[test]
fn adm_proxy_controller_reset_for_teardown_leaves_the_gate_closed() {
    let h = harness(&["{spk}"], &["{mic}"]);
    h.apply_control(&RootAudioControlUpdate {
        has_master_volume: true,
        master_volume: 0.3,
        has_deafened: true,
        deafened: true,
        ..Default::default()
    });
    h.reset_root_audio(true);
    let state = h.control_state();
    assert!(state.mic_muted);
    assert_eq!(state.push_to_talk, 2);
    assert!(!state.deafened);
    assert_eq!((state.input_volume, state.master_volume), (1.0, 1.0));

    h.reset_root_audio(false);
    let state = h.control_state();
    assert!(!state.mic_muted);
    assert_eq!(state.push_to_talk, 0);
}

#[test]
fn adm_proxy_controller_sets_both_gate_fields_together() {
    let h = harness(&["{spk}"], &["{mic}"]);
    h.apply_control(&RootAudioControlUpdate {
        has_mic_muted: true,
        mic_muted: true,
        ..Default::default()
    });
    assert!(h.apply_control(&RootAudioControlUpdate {
        has_mic_muted: true,
        mic_muted: false,
        has_push_to_talk: true,
        push_to_talk: 2,
        ..Default::default()
    }));
    let state = h.control_state();
    assert!(!state.mic_muted);
    assert_eq!(state.push_to_talk, 2);
}

#[test]
fn adm_proxy_controller_reports_fields_it_cannot_apply() {
    let h = harness(&["{spk}"], &["{mic}"]);
    assert!(!h.apply_control(&RootAudioControlUpdate {
        has_mic_muted: true,
        mic_muted: true,
        has_echo_cancellation: true,
        echo_cancellation: false,
        ..Default::default()
    }));
    assert!(h.control_state().mic_muted, "the other fields are applied");
    assert!(h.apply_control(&RootAudioControlUpdate {
        has_deafened: true,
        deafened: true,
        ..Default::default()
    }));
}

#[test]
fn adm_proxy_controller_drives_the_apm_policy_and_reads_its_statistics() {
    let h = new_adm_proxy_harness_with_apm(&strings(&["{spk}"]), &strings(&["{mic}"]));
    let stats = h.stats();
    assert_eq!((stats.erl_db, stats.erle_db), (12.0, 20.0));
    assert_eq!((stats.aec_delay_ms, stats.divergent_filter_fraction), (40, 0.25));
    assert!(!stats.echo_cancellation_active && !stats.webrtc_ns_active);

    assert!(h.apply_control(&RootAudioControlUpdate {
        has_echo_cancellation: true,
        echo_cancellation: true,
        has_noise_suppression: true,
        noise_suppression: true,
        ..Default::default()
    }));
    let stats = h.stats();
    assert!(stats.echo_cancellation_active && stats.webrtc_ns_active);

    h.reset_root_audio(true);
    let stats = h.stats();
    assert!(!stats.echo_cancellation_active && !stats.webrtc_ns_active, "the engine's request");
}

#[test]
fn adm_proxy_stats_report_unknown_echo_statistics_without_an_apm() {
    let h = harness(&["{spk}"], &["{mic}"]);
    let stats = h.stats();
    assert!(stats.erl_db.is_nan() && stats.erle_db.is_nan());
    assert!(stats.divergent_filter_fraction.is_nan());
    assert_eq!(stats.aec_delay_ms, -1);
    assert_eq!((stats.processed_frames, stats.effects_played), (0, 0));
}

#[test]
fn adm_proxy_moves_the_stream_epoch_before_every_recording_start() {
    let h = platform(&["{spk}"], &["{mic}"]);
    assert_eq!(h.control_state().stream_epoch, 0);
    h.app_recording(true);
    assert!(calls(&h).contains(&"StartRecording".to_string()));
    assert_eq!(h.control_state().stream_epoch, 1);

    let outcome = h.restart(true, false);
    assert!(outcome.recording.attempted);
    assert_eq!(h.control_state().stream_epoch, 2, "a restart is a new stream");

    h.engine_start_recording();
    calls(&h);
    assert_eq!(h.control_state().stream_epoch, 2, "an open device is not started again");
    h.app_recording(false);
    h.engine_stop_recording();
    calls(&h);
    assert_eq!(h.control_state().stream_epoch, 2);
    h.app_recording(true);
    calls(&h);
    assert_eq!(h.control_state().stream_epoch, 3);
}

#[test]
fn adm_proxy_clears_the_speaking_flag_when_recording_stops() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.app_recording(true);
    assert!(calls(&h).contains(&"StartRecording".to_string()));
    h.set_speaking(true);
    let speaking = h.controller().root_audio_speaking();
    assert!(speaking.speaking);

    h.app_recording(false);
    assert!(calls(&h).contains(&"StopRecording".to_string()));
    let stopped = h.controller().root_audio_speaking();
    assert!(!stopped.speaking, "no frame will clear it");
    assert_eq!(stopped.seq, speaking.seq + 1);

    h.set_speaking(true);
    h.reset_root_audio(true);
    assert!(!h.controller().root_audio_speaking().speaking, "nor after the last session");
    assert!(!h.stats().speaking);
}

#[test]
fn adm_proxy_clears_the_speaking_flag_after_a_failed_stop_and_when_frames_stop() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.app_recording(true);
    calls(&h);
    h.set_speaking(true);
    h.fail_next_stop_recording(-1);
    h.app_recording(false);
    assert!(calls(&h).contains(&"StopRecording".to_string()));
    assert_eq!(h.stats().device_stop_failures, 1);
    assert!(!h.controller().root_audio_speaking().speaking, "a failed stop clears it too");

    h.set_speaking(true);
    assert!(h.controller().root_audio_speaking().speaking, "a frame just now");
    std::thread::sleep(Duration::from_millis(320));
    let stale = h.controller().root_audio_speaking();
    assert!(!stale.speaking, "no frame for 300 ms: a device that died mid-word");
    assert!(!h.stats().speaking);
}

#[test]
fn adm_proxy_controller_stores_denoiser_wanted_until_a_reset() {
    let h = harness(&["{spk}"], &["{mic}"]);
    assert!(h.apply_control(&RootAudioControlUpdate {
        has_denoiser_wanted: true,
        denoiser_wanted: true,
        ..Default::default()
    }));
    assert!(h.control_state().denoiser_wanted);
    let resets = h.control_state().dsp_reset_seq;
    h.reset_root_audio(false);
    assert!(!h.control_state().denoiser_wanted);
    assert_eq!(h.control_state().dsp_reset_seq, resets + 1);
}

fn denoiser_completions(h: &AdmProxyHarness, count: usize) -> Vec<RootDenoiserCompletionItem> {
    let all = std::cell::RefCell::new(Vec::new());
    wait_until("denoiser completions", || {
        all.borrow_mut().extend(h.controller().take_root_denoiser_completions());
        all.borrow().len() >= count
    });
    all.into_inner()
}

#[test]
fn adm_proxy_controller_hands_pipelines_to_the_slot_and_back() {
    let h = new_adm_proxy_harness_with_apm(&strings(&["{spk}"]), &strings(&["{mic}"]));
    let controller = h.controller();
    assert!(controller.root_denoiser_available());
    let first = h.add_fake_pipeline();
    assert!(controller.set_root_denoiser(7, 70, first, 48_000));
    assert_eq!(
        denoiser_completions(&h, 1),
        [RootDenoiserCompletionItem { owner: 7, async_id: 70, error: String::new(), pipeline: 0 }]
    );
    assert_eq!(
        controller.root_denoiser_state(),
        RootDenoiserStateInfo {
            installed: true,
            rate: 48_000,
            bypass_reason: 0,
            webrtc_ns_fallback: false
        }
    );
    assert_eq!(h.run_denoiser(48_000), 2);

    let stats = h.stats();
    assert!(stats.dsp_available && stats.denoiser_available && stats.policy_available);
    assert!(stats.denoiser_installed);
    assert_eq!(
        (stats.denoiser_rate, stats.denoiser_installs, stats.denoiser_frames),
        (48_000, 1, 1)
    );
    assert_eq!(stats.denoiser_process_us_buckets.len(), 16);
    assert_eq!(stats.denoiser_process_us_buckets.iter().sum::<u64>(), 1);
    assert_eq!(stats.denoiser_frames_until_first_output, 0);
    assert_eq!(h.stats().denoiser_process_max_us, 0, "the maximum resets on read");

    let second = h.add_fake_pipeline();
    controller.set_root_denoiser(8, 80, second, 48_000);
    denoiser_completions(&h, 1);
    assert_eq!(
        controller.take_root_denoiser_returned(),
        [RootDenoiserReturnedItem { owner: 7, pipeline: first }]
    );

    h.reset_root_audio(false);
    assert!(controller.root_denoiser_state().installed, "a session start keeps it");
    h.reset_root_audio(true);
    assert!(!controller.root_denoiser_state().installed);
    assert_eq!(
        controller.take_root_denoiser_returned(),
        [RootDenoiserReturnedItem { owner: 8, pipeline: second }],
        "the last session's end hands it back"
    );
}

#[test]
fn adm_proxy_controller_returns_every_pipeline_at_once() {
    let h = harness(&["{spk}"], &["{mic}"]);
    let controller = h.controller();
    let first = h.add_fake_pipeline();
    let second = h.add_fake_pipeline();
    controller.set_root_denoiser(1, 10, first, 48_000);
    denoiser_completions(&h, 1);
    controller.set_root_denoiser(1, 11, second, 48_000);
    denoiser_completions(&h, 1);

    let returned = controller.return_root_denoiser_pipelines();
    assert!(returned.error.is_empty());
    let mut pipelines = returned.pipelines;
    pipelines.sort_unstable();
    let mut expected = vec![first, second];
    expected.sort_unstable();
    assert_eq!(pipelines, expected);
    assert!(!controller.root_denoiser_state().installed);
    assert!(controller.take_root_denoiser_returned().is_empty());
}

#[test]
fn adm_proxy_controller_covers_a_stalled_denoiser_with_webrtc_ns() {
    let h = new_adm_proxy_harness_with_apm(&strings(&["{spk}"]), &strings(&["{mic}"]));
    let controller = h.controller();
    let pipeline = h.add_fake_pipeline();
    controller.set_root_denoiser(1, 10, pipeline, 48_000);
    denoiser_completions(&h, 1);
    assert!(h.apply_control(&RootAudioControlUpdate {
        has_denoiser_wanted: true,
        denoiser_wanted: true,
        ..Default::default()
    }));
    assert_eq!(h.run_denoiser(48_000), 2);
    h.latch_denoiser_bypass(2);
    wait_until("the fallback", || controller.root_denoiser_state().webrtc_ns_fallback);
    wait_until("WebRTC NS applied", || h.stats().webrtc_ns_active);
    let stats = h.stats();
    assert!(stats.webrtc_ns_fallback);
    assert_eq!(stats.denoiser_bypass_reason, 2);
    assert!(stats.policy_available && stats.apm_policy_applies >= 1);
    assert_eq!(h.run_denoiser(48_000), 0, "the stalled pipeline is bypassed");

    h.apply_control(&RootAudioControlUpdate {
        has_denoiser_wanted: true,
        denoiser_wanted: false,
        ..Default::default()
    });
    wait_until("the fallback ends", || !controller.root_denoiser_state().webrtc_ns_fallback);
}

/// The host leaves the Effects SDK for WebRTC NS in one control: the policy
/// lands before denoiser_wanted falls, so the fallback hands NS over without
/// turning it off in between.
#[test]
fn adm_proxy_controller_hands_the_fallback_over_to_the_hosts_ns_without_a_gap() {
    let h = new_adm_proxy_harness_with_apm(&strings(&["{spk}"]), &strings(&["{mic}"]));
    let controller = h.controller();
    let pipeline = h.add_fake_pipeline();
    controller.set_root_denoiser(1, 10, pipeline, 48_000);
    denoiser_completions(&h, 1);
    assert!(h.apply_control(&RootAudioControlUpdate {
        has_denoiser_wanted: true,
        denoiser_wanted: true,
        ..Default::default()
    }));
    assert_eq!(h.run_denoiser(48_000), 2);
    h.latch_denoiser_bypass(2);
    wait_until("WebRTC NS applied", || h.stats().webrtc_ns_active);
    std::thread::sleep(Duration::from_millis(1100));

    assert!(h.apply_control(&RootAudioControlUpdate {
        has_denoiser_wanted: true,
        denoiser_wanted: false,
        has_noise_suppression: true,
        noise_suppression: true,
        ..Default::default()
    }));
    let until = Instant::now() + Duration::from_millis(300);
    while Instant::now() < until {
        assert!(h.stats().webrtc_ns_active, "NS never goes off between the two");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!controller.root_denoiser_state().webrtc_ns_fallback);
    let stats = h.stats();
    assert!(stats.webrtc_ns_active && !stats.apm_policy_pending);
}

const MONITOR_APPLIED: u8 = 0;
const MONITOR_SENDING: u8 = 2;
const MONITOR_NOT_ACQUIRED: u8 = 3;

fn both_open() -> Vec<String> {
    vec![
        default_device("Playout"),
        "InitPlayout".into(),
        "StartPlayout".into(),
        default_device("Recording"),
        "InitRecording".into(),
        "StartRecording".into(),
    ]
}

#[test]
fn adm_proxy_monitor_opens_both_devices_without_claiming_them() {
    let h = platform(&["{spk}"], &["{mic}"]);
    assert_eq!(h.monitor(true), MONITOR_APPLIED);
    assert_eq!(calls(&h), both_open());
    assert!(h.platform_playing() && h.platform_recording());
    assert!(!h.playing() && !h.recording(), "libwebrtc started neither");
    assert!(!h.synthetic_playing());
    let stats = h.stats();
    assert!(stats.monitor_available && stats.monitor_active);
    assert_eq!(h.control_state().stream_epoch, 1, "the DSP chain starts a stream");

    assert_eq!(h.monitor(true), MONITOR_APPLIED);
    assert_eq!(calls(&h), Vec::<String>::new(), "already on");

    assert_eq!(h.monitor(false), MONITOR_APPLIED);
    assert_eq!(calls(&h), ["StopPlayout", "StopRecording"]);
    assert!(!h.platform_playing() && !h.platform_recording());
    assert!(!h.stats().monitor_active);
    assert_eq!(h.monitor(false), MONITOR_APPLIED, "stopping a stopped monitor is no error");
}

#[test]
fn adm_proxy_monitor_stop_leaves_the_devices_others_keep() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.hold(true);
    h.app_recording(true);
    calls(&h);
    assert_eq!(h.monitor(true), MONITOR_APPLIED);
    assert_eq!(calls(&h), Vec::<String>::new(), "both already open");
    assert_eq!(h.monitor(false), MONITOR_APPLIED);
    assert_eq!(calls(&h), Vec::<String>::new(), "the hold and the app keep them");

    h.hold(false);
    h.app_recording(false);
    calls(&h);
    h.engine_start_playout();
    calls(&h);
    h.monitor(true);
    assert_eq!(
        calls(&h),
        [default_device("Recording"), "InitRecording".into(), "StartRecording".into()]
    );
    h.monitor(false);
    assert_eq!(calls(&h), ["StopRecording"], "libwebrtc's playout stays");
    assert!(h.playing());
}

#[test]
fn adm_proxy_monitor_is_refused_while_libwebrtc_records_and_ends_when_it_starts() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.engine_start_recording();
    calls(&h);
    assert_eq!(h.monitor(true), MONITOR_SENDING);
    assert_eq!(calls(&h), Vec::<String>::new());
    assert!(!h.stats().monitor_active);

    h.engine_stop_recording();
    calls(&h);
    assert_eq!(h.monitor(true), MONITOR_APPLIED);
    calls(&h);
    let epoch = h.control_state().stream_epoch;

    // A call publishes: its send stream takes the microphone over.
    h.engine_start_recording();
    assert_eq!(calls(&h), ["StopPlayout"], "the recording device stays open");
    assert!(h.recording() && h.platform_recording());
    assert!(!h.platform_playing());
    assert!(!h.stats().monitor_active, "native turned the monitor off");
    assert_eq!(h.control_state().stream_epoch, epoch, "no restart");
    assert_eq!(h.monitor(true), MONITOR_SENDING);

    h.engine_stop_recording();
    assert_eq!(calls(&h), ["StopRecording"]);
    assert!(!h.stats().monitor_active, "it stays off");
}

#[test]
fn adm_proxy_monitor_needs_a_reference_and_ends_with_the_last_one() {
    let h = harness(&["{spk}"], &["{mic}"]);
    assert_eq!(h.monitor(true), MONITOR_NOT_ACQUIRED);
    h.acquire();
    h.enable(true, true);
    calls(&h);
    assert_eq!(h.monitor(true), MONITOR_APPLIED);
    assert_eq!(calls(&h), both_open());

    h.release();
    assert_eq!(calls(&h), ["StopPlayout", "StopRecording"]);
    assert!(!h.stats().monitor_active);
    h.acquire();
    assert_eq!(calls(&h), Vec::<String>::new(), "the next reference does not reopen them");
}

#[test]
fn adm_proxy_monitor_moves_with_a_device_selection() {
    let h = platform(&["{a}", "{b}"], &["{m1}", "{m2}"]);
    h.monitor(true);
    calls(&h);
    let outcome = h.select(&guid("{m2}"), true, &guid("{b}"), true);
    assert!(outcome.playout.attempted && outcome.recording.attempted);
    assert_eq!(
        h.take_calls(),
        [
            "StopPlayout",
            "SetPlayoutDevice(1)",
            "InitPlayout",
            "StartPlayout",
            "StopRecording",
            "SetRecordingDevice(1)",
            "InitRecording",
            "StartRecording"
        ]
    );
    assert!(h.stats().monitor_active);
}

#[test]
fn adm_proxy_controller_reset_for_teardown_stops_the_monitor() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.monitor(true);
    calls(&h);
    h.reset_root_audio(false);
    assert_eq!(calls(&h), Vec::<String>::new(), "a session start leaves it running");
    assert!(h.stats().monitor_active);
    h.reset_root_audio(true);
    assert_eq!(calls(&h), ["StopPlayout", "StopRecording"]);
    assert!(!h.stats().monitor_active);
}
