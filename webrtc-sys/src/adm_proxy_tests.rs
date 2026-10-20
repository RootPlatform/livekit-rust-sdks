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
        new_adm_proxy_harness, new_adm_proxy_harness_with_apm,
        new_adm_proxy_harness_without_platform_adm, AdmProxyHarness, AudioDeviceOutcome,
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

fn uid(id: &str) -> AudioDeviceSelection {
    AudioDeviceSelection { kind: AudioDeviceSelectionKind::Uid, id: id.to_string(), index: 0 }
}

/// macOS: a CoreAudio UID is translated to the device's current AudioObjectID
/// (the ADM's GUID) at every open, so a coreaudiod restart that moves the ids
/// needs no new selection and never opens the device that took the old id.
#[test]
fn adm_proxy_uid_is_translated_to_the_current_guid_at_every_open() {
    let h = platform(&["default", "73", "91"], &["default", "74"]);
    let uids = strings(&["Speakers-UID", "AirPods-UID"]);
    h.set_uid_map(&uids, &strings(&["73", "91"]));
    let outcome = h.select(&unset(), false, &uid("AirPods-UID"), true);
    assert!(outcome.playout.found && !outcome.playout.attempted);
    h.hold(true);
    assert_eq!(calls(&h), ["SetPlayoutDevice(2)", "InitPlayout", "StartPlayout"]);

    // coreaudiod restarted: the AirPods are 95 now, and 91 is the speakers.
    h.set_devices(&strings(&["default", "91", "95"]), &strings(&["default", "74"]));
    h.set_uid_map(&uids, &strings(&["91", "95"]));
    let restarted = h.restart(false, true);
    assert!(restarted.playout.found && restarted.playout.attempted);
    assert_eq!(
        h.take_calls(),
        ["StopPlayout", "SetPlayoutDevice(2)", "InitPlayout", "StartPlayout"],
        "the AirPods' new id, not the old one"
    );

    let again = h.select(&unset(), false, &uid("AirPods-UID"), true);
    assert!(again.playout.found && !again.playout.attempted, "the same UID is no change");
    assert_eq!(h.take_calls(), Vec::<String>::new());

    h.set_uid_map(
        &strings(&["Speakers-UID", "AirPods-UID", "Mic-UID"]),
        &strings(&["91", "95", "74"]),
    );
    h.select(&uid("Mic-UID"), true, &unset(), false);
    h.monitor(true);
    assert_eq!(calls(&h), ["SetRecordingDevice(1)", "InitRecording", "StartRecording"]);
}

#[test]
fn adm_proxy_unknown_uid_is_device_not_found_never_another_device() {
    let h = platform(&["default", "73"], &["default", "74"]);
    h.set_uid_map(&strings(&["Speakers-UID", "Gone-UID"]), &strings(&["73", "99"]));
    h.hold(true);
    calls(&h);

    let outcome = h.select(&unset(), false, &uid("AirPods-UID"), true);
    assert!(!outcome.playout.found && outcome.playout.attempted);
    assert_eq!(outcome.playout.init_result, ADM_RESULT_DEVICE_NOT_FOUND);
    assert_eq!(h.take_calls(), ["StopPlayout"], "never the default instead");
    assert!(!h.platform_playing());

    for missing in ["", "speakers-uid", "Speakers-UID ", "Gone-UID"] {
        let outcome = h.select(&unset(), false, &uid(missing), true);
        assert!(!outcome.playout.found, "{missing:?}");
        assert_eq!(h.take_calls(), Vec::<String>::new(), "{missing:?}");
        assert_eq!(h.stats().playout_init_result, ADM_RESULT_DEVICE_NOT_FOUND, "{missing:?}");
    }

    // Gone-UID's device appears: the next open finds it without a new
    // selection.
    h.set_devices(&strings(&["default", "73", "99"]), &strings(&["default", "74"]));
    h.hold(false);
    h.hold(true);
    assert_eq!(calls(&h), ["SetPlayoutDevice(2)", "InitPlayout", "StartPlayout"]);
}

/// Windows and Linux have no UIDs: a UID selection is no device, never the
/// default.
#[cfg(not(target_os = "macos"))]
#[test]
fn adm_proxy_uid_selects_nothing_without_coreaudio() {
    let h = platform(&["{spk}"], &["{mic}"]);
    let outcome = h.select(&uid("{mic}"), true, &uid("{spk}"), true);
    assert!(!outcome.recording.found && !outcome.playout.found);
    h.hold(true);
    h.app_recording(true);
    assert_eq!(calls(&h), Vec::<String>::new());
    let stats = h.stats();
    assert_eq!(
        (stats.playout_init_result, stats.recording_init_result),
        (ADM_RESULT_DEVICE_NOT_FOUND, ADM_RESULT_DEVICE_NOT_FOUND)
    );
}

/// macOS: AudioDeviceMac keeps converting to the output format it read at
/// InitPlayout. A Bluetooth headset entering its hands-free profile under the
/// hold (2 ch 48 kHz to 1 ch 24 kHz) is reopened once the new format held for
/// two reads, so the ADM reads it again.
#[test]
fn adm_proxy_reopens_playout_when_the_device_format_changes_under_it() {
    let h = platform(&["default", "73"], &["default", "74"]);
    h.set_device_format(true, "", 48_000, 2);
    h.hold(true);
    let open = [default_device("Playout"), "InitPlayout".into(), "StartPlayout".into()];
    assert_eq!(calls(&h), open);
    let stats = h.stats();
    assert!(stats.device_formats_available);
    assert_eq!((stats.playout_device_rate, stats.playout_device_channels), (48_000, 2));
    assert_eq!((stats.recording_device_rate, stats.recording_device_channels), (0, 0));
    h.poll_device_formats();
    h.poll_device_formats();
    assert_eq!(calls(&h), Vec::<String>::new(), "an unchanged format reopens nothing");

    h.set_device_format(true, "", 24_000, 1);
    h.poll_device_formats();
    assert_eq!(calls(&h), Vec::<String>::new(), "one read is not enough");
    assert_eq!((h.stats().playout_format_changes, h.stats().playout_format_reopens), (1, 0));
    h.poll_device_formats();
    let mut reopen = vec!["StopPlayout".to_string()];
    reopen.extend(open.iter().cloned());
    assert_eq!(calls(&h), reopen);
    let stats = h.stats();
    assert_eq!((stats.playout_device_rate, stats.playout_device_channels), (24_000, 1));
    assert_eq!((stats.playout_format_changes, stats.playout_format_reopens), (1, 1));
    assert!(h.platform_playing());
    h.poll_device_formats();
    h.poll_device_formats();
    assert_eq!(calls(&h), Vec::<String>::new(), "the new format is the opened one");

    h.hold(false);
    assert_eq!(calls(&h), ["StopPlayout"]);
    assert_eq!((h.stats().playout_device_rate, h.stats().playout_device_channels), (0, 0));
}

/// A profile switch that comes back to the opened format still reopens (the
/// ADM may have taken the format in between), a format must hold for two
/// reads, and reopens are at least a second apart.
#[test]
fn adm_proxy_reopens_playout_after_a_format_bounce_at_most_once_a_second() {
    let h = platform(&["default", "73"], &["default"]);
    h.set_device_format(true, "73", 48_000, 2);
    select_playout(&h, "73");
    h.hold(true);
    let open = ["SetPlayoutDevice(1)", "InitPlayout", "StartPlayout"];
    assert_eq!(calls(&h), open);

    h.set_device_format(true, "73", 16_000, 1);
    h.poll_device_formats();
    h.set_device_format(true, "73", 48_000, 2);
    h.poll_device_formats();
    assert_eq!(calls(&h), Vec::<String>::new(), "no format held for two reads yet");
    h.poll_device_formats();
    assert_eq!(calls(&h), ["StopPlayout", open[0], open[1], open[2]]);
    assert_eq!((h.stats().playout_format_changes, h.stats().playout_format_reopens), (1, 1));

    h.set_device_format(true, "73", 24_000, 1);
    h.poll_device_formats();
    h.poll_device_formats();
    assert_eq!(calls(&h), Vec::<String>::new(), "within a second of the last reopen");
    std::thread::sleep(Duration::from_millis(1_100));
    h.poll_device_formats();
    assert_eq!(calls(&h), ["StopPlayout", open[0], open[1], open[2]]);
    let stats = h.stats();
    assert_eq!((stats.playout_format_changes, stats.playout_format_reopens), (2, 2));
    assert_eq!((stats.playout_device_rate, stats.playout_device_channels), (24_000, 1));
}

/// A device that cannot be read is not watched, and recording format changes
/// are counted without reopening the microphone.
#[test]
fn adm_proxy_counts_recording_format_changes_and_skips_unreadable_devices() {
    let h = platform(&["default", "73"], &["default", "74"]);
    h.set_device_format(false, "74", 48_000, 1);
    h.select(&guid("74"), true, &guid("73"), true);
    h.hold(true);
    assert_eq!(calls(&h), ["SetPlayoutDevice(1)", "InitPlayout", "StartPlayout"]);
    h.app_recording(true);
    assert_eq!(calls(&h), ["SetRecordingDevice(1)", "InitRecording", "StartRecording"]);
    let stats = h.stats();
    assert_eq!((stats.playout_device_rate, stats.playout_device_channels), (0, 0));
    assert_eq!((stats.recording_device_rate, stats.recording_device_channels), (48_000, 1));

    h.set_device_format(true, "73", 24_000, 1);
    h.set_device_format(false, "74", 16_000, 1);
    for _ in 0..3 {
        h.poll_device_formats();
    }
    assert_eq!(calls(&h), Vec::<String>::new());
    let stats = h.stats();
    assert_eq!((stats.recording_format_changes, stats.playout_format_changes), (1, 0));
    assert_eq!(stats.recording_device_rate, 48_000, "as the ADM opened it");

    h.app_recording(false);
    assert_eq!(calls(&h), ["StopRecording"]);
    assert_eq!((h.stats().recording_device_rate, h.stats().recording_device_channels), (0, 0));
}

#[test]
fn adm_proxy_without_a_format_reader_reports_no_device_formats() {
    let h = platform(&["default"], &["default"]);
    h.hold(true);
    h.poll_device_formats();
    assert_eq!(calls(&h), [default_device("Playout"), "InitPlayout".into(), "StartPlayout".into()]);
    let stats = h.stats();
    assert!(!stats.device_formats_available);
    assert_eq!((stats.playout_device_rate, stats.playout_format_changes), (0, 0));
}

/// Linux serializes enumeration with device operations (the PulseAudio ADM
/// is not thread-safe) but never waits long for one: past the wait it reports
/// no devices, and the enumerations right after it give up at once.
#[test]
fn adm_proxy_serialized_enumeration_waits_for_a_device_operation_but_not_for_long() {
    let h = platform(&["{a}", "{b}"], &["{mic}"]);
    h.set_enumeration_lock(true, 200);
    let c = h.controller();
    assert_eq!(c.playout_devices(), 2);
    assert_eq!(c.playout_device_guid(1), "{b}");

    h.block_next_start_playout();
    h.hold(true);
    assert!(h.start_playout_blocked(), "the device thread opens playout");
    let started = Instant::now();
    assert_eq!(c.playout_devices(), 0, "gave up: no devices");
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(190) && waited < Duration::from_secs(3), "{waited:?}");
    let started = Instant::now();
    assert_eq!(c.recording_devices(), 0);
    assert_eq!(c.playout_device_name(0), "");
    assert!(started.elapsed() < Duration::from_millis(150), "the next ones give up at once");

    // Past that window it waits again, and an open that ends in time is
    // waited for.
    std::thread::sleep(Duration::from_millis(250));
    h.release_start_playout(50);
    assert_eq!(c.playout_devices(), 2, "waited for the open to finish");
    h.flush();
    assert!(h.platform_playing());

    // Unserialized (Windows, macOS), enumeration never waits.
    h.set_enumeration_lock(false, 200);
    h.hold(false);
    h.flush();
    h.block_next_start_playout();
    h.hold(true);
    assert!(h.start_playout_blocked());
    let started = Instant::now();
    assert_eq!(c.playout_devices(), 2);
    assert_eq!(c.playout_device_name(1), "Device {b}");
    assert!(started.elapsed() < Duration::from_millis(150));
    h.release_start_playout(0);
    h.flush();
    assert!(h.platform_playing());
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
fn adm_proxy_shutdown_stops_the_devices_before_clearing_the_callback() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.register_transport();
    h.engine_start_playout();
    h.engine_start_recording();
    calls(&h);

    h.shutdown();
    assert_eq!(
        h.take_calls(),
        ["StopPlayout", "StopRecording", "RegisterAudioCallback(null)", "Terminate"]
    );
    assert!(!h.playing() && !h.recording() && !h.synthetic_playing());
}

/// m150 terminates the voice engine with the last PeerConnection, also while
/// the hold keeps playout open, and initializes it again (Init, then the
/// transport) with the next one. libwebrtc's own playout and recording end;
/// the held device plays the tap's silence, never the buffer's stale samples,
/// and neither ADM is terminated, so the next transport plays at once.
#[test]
fn adm_proxy_engine_terminate_under_the_hold_plays_silence_until_the_next_init() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.stale_device_buffer(i16::MAX);
    h.hold(true);
    h.engine_plays_mixer();
    h.register_transport();
    h.engine_start_playout();
    h.engine_start_recording();
    calls(&h);
    assert!(h.controller().register_root_audio_effect(1, &vec![8000i16; 4800]));

    h.engine_terminate();
    assert_eq!(calls(&h), ["StopRecording"], "the hold keeps playout, nothing is terminated");
    assert!(h.platform_playing() && !h.platform_recording());
    assert!(!h.playing() && !h.recording() && !h.synthetic_playing());
    assert_eq!(h.controller().play_root_audio_effect(1, 0.5), 0);
    let render = h.device_render();
    assert_eq!(render.frames, 480, "the tap answered");
    assert!(render.samples.iter().all(|&s| s == 0), "{:?}", &render.samples[..4]);

    // The next PeerConnection: engine init, then libwebrtc's transport.
    assert_eq!(h.engine_adm_init(), [0, 0, 0, 0]);
    h.register_transport();
    let render = h.device_render();
    assert!(render.samples.iter().all(|&s| (3999..=4000).contains(&s)), "the chime plays");
    h.engine_start_recording();
    assert_eq!(
        calls(&h),
        [default_device("Recording"), "InitRecording".into(), "StartRecording".into()]
    );
    assert!(h.recording());
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
fn adm_proxy_stops_a_failed_playout_start_before_it_starts_again() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.fail_next_start_playout(-1);
    h.hold(true);
    assert_eq!(
        calls(&h),
        [default_device("Playout"), "InitPlayout".into(), "StartPlayout".into(), "StopPlayout".into()]
    );
    assert!(!h.platform_playing() && !h.playout_initialized());

    h.app_playout(true);
    assert_eq!(calls(&h), [default_device("Playout"), "InitPlayout".into(), "StartPlayout".into()]);
    assert!(h.platform_playing());
}

#[test]
fn adm_proxy_stops_a_failed_recording_start_before_it_starts_again() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.fail_next_start_recording(-1);
    h.app_recording(true);
    assert_eq!(
        calls(&h),
        [
            default_device("Recording"),
            "InitRecording".into(),
            "StartRecording".into(),
            "StopRecording".into()
        ]
    );
    assert!(!h.platform_recording() && !h.recording_initialized());

    h.engine_start_recording();
    assert_eq!(calls(&h), [default_device("Recording"), "InitRecording".into(), "StartRecording".into()]);
    assert!(h.platform_recording());
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
    assert_eq!((state.gain_reset_db, state.gain_reset_seq), (50.0, 1), "AGC2's highest maximum");
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

#[test]
fn adm_proxy_controller_stores_speech_detection_until_a_reset_and_turns_unknown_modes_off() {
    let h = harness(&["{spk}"], &["{mic}"]);
    assert_eq!(h.control_state().speech_detection, 0, "off outside a session");
    let mode = |value: u32| {
        assert!(h.apply_control(&RootAudioControlUpdate {
            has_speech_detection: true,
            speech_detection: value,
            ..Default::default()
        }));
        h.control_state().speech_detection
    };
    assert_eq!(mode(2), 2);
    assert_eq!(mode(0), 0);
    assert_eq!(mode(1), 2, "the removed ring mode is gate");
    assert_eq!(mode(7), 0);
    assert_eq!(mode(2), 2);
    h.apply_control(&RootAudioControlUpdate {
        has_mic_muted: true,
        mic_muted: true,
        ..Default::default()
    });
    assert_eq!(h.control_state().speech_detection, 2, "an unset field keeps its value");
    mode(0);
    h.reset_root_audio(false);
    assert_eq!(h.control_state().speech_detection, 2, "a session starts with gate");
    let stats = h.stats();
    assert!(stats.speech_available);
    assert_eq!((stats.speech_detection, stats.speech_probability), (2, -1.0));
    h.reset_root_audio(true);
    assert_eq!(h.control_state().speech_detection, 0, "off once the session ends");
}

#[test]
fn adm_proxy_controller_stores_the_auto_sensitivity_minimum_until_a_reset_and_reports_it_clamped() {
    let h = harness(&["{spk}"], &["{mic}"]);
    assert!(h.control_state().auto_sensitivity_minimum_db.is_nan(), "none until the host sets one");
    assert_eq!(h.stats().auto_sensitivity_minimum_db, -54.0, "the chain's default");
    let set = |db: f32| {
        assert!(h.apply_control(&RootAudioControlUpdate {
            has_auto_sensitivity_minimum_db: true,
            auto_sensitivity_minimum_db: db,
            ..Default::default()
        }));
        h.stats().auto_sensitivity_minimum_db
    };
    assert_eq!(set(-50.0), -50.0);
    assert_eq!(set(-95.0), -80.0);
    assert_eq!(set(0.0), -30.0);
    assert_eq!(set(f32::NAN), -54.0);
    assert_eq!(set(-48.0), -48.0);
    h.apply_control(&RootAudioControlUpdate {
        has_mic_muted: true,
        mic_muted: true,
        ..Default::default()
    });
    assert_eq!(h.control_state().auto_sensitivity_minimum_db, -48.0, "an unset field keeps it");
    h.reset_root_audio(false);
    assert!(h.control_state().auto_sensitivity_minimum_db.is_nan());
    assert_eq!(h.stats().auto_sensitivity_minimum_db, -54.0);
}

#[test]
fn adm_proxy_controller_stores_the_speech_lookahead_until_a_reset() {
    let h = harness(&["{spk}"], &["{mic}"]);
    assert!(h.control_state().speech_lookahead_ms.is_nan(), "none until the host sets one");
    assert_eq!(h.stats().speech_lookahead_ms, 0, "no frame has run");
    assert!(h.apply_control(&RootAudioControlUpdate {
        has_speech_lookahead_ms: true,
        speech_lookahead_ms: 30.0,
        ..Default::default()
    }));
    assert_eq!(h.control_state().speech_lookahead_ms, 30.0, "stored as set; the chain clamps it");
    h.apply_control(&RootAudioControlUpdate {
        has_mic_muted: true,
        mic_muted: true,
        ..Default::default()
    });
    assert_eq!(h.control_state().speech_lookahead_ms, 30.0, "an unset field keeps it");
    h.reset_root_audio(false);
    assert!(h.control_state().speech_lookahead_ms.is_nan());
}

#[test]
fn adm_proxy_controller_stores_the_voice_gain_tunables_until_a_reset_and_reports_them_clamped() {
    let h = harness(&["{spk}"], &["{mic}"]);
    let state = h.control_state();
    assert!(state.voice_gain_max_db.is_nan() && state.voice_gain_min_speech_probability.is_nan());
    let applied = |h: &AdmProxyHarness| {
        let s = h.stats();
        [
            s.voice_gain_max_db,
            s.voice_gain_target_db,
            s.voice_gain_rise_db_per_second,
            s.voice_gain_fall_db_per_second,
            s.voice_gain_level_time_constant_ms,
            s.voice_gain_min_speech_probability,
        ]
    };
    assert_eq!(applied(&h), [24.0, -25.0, 3.0, 2.0, 4000.0, 0.5], "the chain's defaults");
    assert!(h.apply_control(&RootAudioControlUpdate {
        has_voice_gain_max_db: true,
        voice_gain_max_db: 40.0,
        has_voice_gain_target_db: true,
        voice_gain_target_db: -22.0,
        has_voice_gain_rise_db_per_second: true,
        voice_gain_rise_db_per_second: 0.0,
        has_voice_gain_fall_db_per_second: true,
        voice_gain_fall_db_per_second: f32::NAN,
        has_voice_gain_level_time_constant_ms: true,
        voice_gain_level_time_constant_ms: 6000.0,
        has_voice_gain_min_speech_probability: true,
        voice_gain_min_speech_probability: 0.9,
        ..Default::default()
    }));
    assert_eq!(applied(&h), [24.0, -22.0, 0.5, 2.0, 6000.0, 0.9]);
    h.apply_control(&RootAudioControlUpdate {
        has_voice_gain_max_db: true,
        voice_gain_max_db: 9.0,
        ..Default::default()
    });
    let state = h.control_state();
    assert_eq!(
        (state.voice_gain_max_db, state.voice_gain_target_db),
        (9.0, -22.0),
        "unset fields keep theirs"
    );
    h.reset_root_audio(false);
    assert!(h.control_state().voice_gain_max_db.is_nan());
    assert!(h.control_state().voice_gain_level_time_constant_ms.is_nan());
    assert_eq!(applied(&h), [24.0, -25.0, 3.0, 2.0, 4000.0, 0.5]);
}

#[test]
fn adm_proxy_controller_stores_the_gain_engine_and_agc2_settings_until_a_reset() {
    let h = harness(&["{spk}"], &["{mic}"]);
    let state = h.control_state();
    assert_eq!(state.gain_engine, 0, "the chain's voice gain outside a session");
    assert!(state.agc2_headroom_db.is_nan() && state.agc2_max_output_noise_dbfs.is_nan());
    let applied = |h: &AdmProxyHarness| {
        let s = h.stats();
        [
            s.agc2_headroom_db,
            s.agc2_max_gain_db,
            s.agc2_max_gain_change_db_per_second,
            s.agc2_max_output_noise_dbfs,
        ]
    };
    assert_eq!(applied(&h), [5.0, 30.0, 6.0, -50.0], "Chrome's, at most 30 dB");
    assert!(h.apply_control(&RootAudioControlUpdate {
        has_gain_engine: true,
        gain_engine: 1,
        has_agc2_headroom_db: true,
        agc2_headroom_db: 40.0,
        has_agc2_max_gain_db: true,
        agc2_max_gain_db: 0.0,
        has_agc2_max_gain_change_db_per_second: true,
        agc2_max_gain_change_db_per_second: f32::NAN,
        has_agc2_max_output_noise_dbfs: true,
        agc2_max_output_noise_dbfs: -10.0,
        ..Default::default()
    }));
    let state = h.control_state();
    assert_eq!((state.gain_engine, state.agc2_headroom_db, state.agc2_max_gain_db), (1, 40.0, 0.0));
    assert_eq!(applied(&h), [20.0, 1.0, 6.0, -30.0], "clamped where they apply");
    h.apply_control(&RootAudioControlUpdate {
        has_gain_engine: true,
        gain_engine: 7,
        ..Default::default()
    });
    assert_eq!(h.control_state().gain_engine, 0, "an unknown engine is the chain's");
    h.reset_root_audio(false);
    let state = h.control_state();
    assert_eq!(state.gain_engine, 1, "a session starts with AGC2");
    assert!(state.agc2_headroom_db.is_nan() && state.agc2_max_gain_db.is_nan());
    assert_eq!(applied(&h), [5.0, 30.0, 6.0, -50.0]);
    h.reset_root_audio(true);
    assert_eq!(h.control_state().gain_engine, 0, "the chain's once the session ends");
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
const MONITOR_NO_DEVICE: u8 = 4;

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
fn adm_proxy_monitor_without_a_device_module_says_so() {
    let h = new_adm_proxy_harness_without_platform_adm();
    h.acquire();
    assert_eq!(h.monitor(true), MONITOR_NO_DEVICE);
    assert!(!h.stats().monitor_active);
    assert_eq!(h.monitor(false), MONITOR_APPLIED);
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

fn level(value: f32) -> Vec<f32> {
    vec![value; 480]
}

/// Playout restarts (the watchdog, a device change) while capture goes on:
/// what the monitor queued before is stale, and frames that found its ring
/// full while nothing rendered are no loss.
#[test]
fn adm_proxy_monitor_starts_over_after_a_playout_restart() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.register_transport();
    assert_eq!(h.monitor(true), MONITOR_APPLIED);
    calls(&h);
    h.monitor_push(48_000, &level(1000.0));
    h.monitor_push(48_000, &level(1000.0));
    let mut played = Vec::new();
    for _ in 0..10 {
        h.monitor_push(48_000, &level(1000.0));
        played = h.render_frame();
    }
    assert!(played.iter().all(|&s| s == 1000), "both channels");
    assert_eq!(h.stats().monitor_dropped_frames, 0);

    for _ in 0..5 {
        h.monitor_push(48_000, &level(2000.0));
    }
    let restart = h.restart(false, true);
    assert!(restart.playout.attempted);
    assert_eq!(
        h.take_calls(),
        [
            "StopPlayout".to_string(),
            default_device("Playout"),
            "InitPlayout".into(),
            "StartPlayout".into()
        ]
    );
    let mut after = Vec::new();
    for _ in 0..5 {
        h.monitor_push(48_000, &level(3000.0));
        after.push(h.render_frame());
    }
    assert!(after[..2].iter().all(|f| f.iter().all(|&s| s == 0)), "it primes on fresh frames");
    assert!(after[2].iter().all(|&s| s > 0 && s <= 3000) && after[2][959] == 3000, "faded in");
    assert!(after[3..].iter().all(|f| f.iter().all(|&s| s == 3000)), "never what queued before");
    assert_eq!(h.stats().monitor_dropped_frames, 0);

    // 300 ms without playout: the ring fills, nothing is counted.
    for _ in 0..30 {
        h.monitor_push(48_000, &level(4000.0));
    }
    h.restart(false, true);
    for _ in 0..5 {
        h.monitor_push(48_000, &level(5000.0));
        h.render_frame();
    }
    assert!(h.render_frame().iter().all(|&s| s == 5000));
    let stats = h.stats();
    assert_eq!(stats.monitor_dropped_frames, 0);
    assert!(stats.monitor_active);
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

/// The live test's engine init under the playout hold: libwebrtc's
/// adm_helpers::Init found playout open ("Unable to access speaker", "unable to
/// set stereo mode while playing side is initialized"). The hold now opens
/// playout stereo, engine init succeeds without reaching the device, and the
/// choice applies at the next open.
#[test]
fn adm_proxy_playout_opens_stereo_and_takes_libwebrtcs_channels_at_the_next_open() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.hold(true);
    calls(&h);
    assert_eq!(h.take_channel_calls(), ["SetStereoPlayout(on)", "InitPlayout stereo"]);
    assert!(h.platform_playing());

    assert_eq!(h.engine_adm_init(), [0, 0, 0, 0]);
    assert!(h.engine_stereo_playout());
    assert_eq!(h.take_channel_calls(), Vec::<String>::new(), "nothing reaches the open device");

    assert_eq!(h.engine_set_stereo_playout(false), 0);
    assert!(!h.engine_stereo_playout());
    h.flush();
    assert_eq!(h.take_channel_calls(), Vec::<String>::new(), "the running device keeps stereo");
    h.restart(false, true);
    assert_eq!(h.take_channel_calls(), ["SetStereoPlayout(off)", "InitPlayout mono"]);

    h.engine_set_stereo_playout(true);
    h.hold(false);
    calls(&h);
    h.engine_start_playout();
    calls(&h);
    assert_eq!(h.take_channel_calls(), ["SetStereoPlayout(on)", "InitPlayout stereo"]);
}

/// The 2026-10-12 live test's blast: the first platform call of a session
/// played loud noise from the lease until libwebrtc registered its transport
/// about 0.65 s later (m150 initializes the voice engine with the first
/// PeerConnection, after SessionCreate and the room connect). The hold had
/// opened the device, and with no transport AudioDeviceModuleImpl's buffer
/// plays its playout samples as they are, every 10 ms: on the first pull an
/// uninitialized heap block ("Invalid audio transport", "nSamples(0) !=
/// _playBlockSize480"). The fake device pulls through libwebrtc's own
/// AudioDeviceBuffer, its samples left full scale in place of that block.
#[test]
fn adm_proxy_device_plays_silence_until_libwebrtc_has_a_transport() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.stale_device_buffer(i16::MAX);
    h.hold(true);
    calls(&h);
    assert!(h.platform_playing());
    for _ in 0..3 {
        let render = h.device_render();
        assert_eq!(render.samples.len(), 960, "stereo");
        assert!(
            render.samples.iter().all(|&s| s == 0),
            "silence, never the buffer's stale samples: {:?}",
            &render.samples[..4]
        );
        assert_eq!(render.frames, 480, "the device's buffer has a transport to pull");
    }
    assert_eq!(h.stats().render_callbacks, 3, "the tap answered them");

    // Engine init registers libwebrtc's transport, then the join chime plays.
    assert_eq!(h.engine_adm_init(), [0, 0, 0, 0]);
    h.engine_plays_mixer();
    h.register_transport();
    assert!(h.controller().register_root_audio_effect(1, &vec![8000i16; 4800]));
    assert_eq!(h.controller().play_root_audio_effect(1, 0.5), 0);
    let before = h.stats();
    let render = h.device_render();
    let after = h.stats();
    assert_eq!(render.frames, 480);
    assert!(render.samples.iter().all(|&s| (3999..=4000).contains(&s)), "the chime, both channels");
    let sumsq: f64 = render.samples.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    assert_eq!(sumsq, after.mix_out_sumsq - before.mix_out_sumsq, "exactly the mixer's output");
    assert_eq!(after.mix_out_samples - before.mix_out_samples, 960);
}

/// The Settings mic test opens the devices through the monitor, where no call
/// may have initialized libwebrtc's voice engine yet.
#[test]
fn adm_proxy_mic_test_device_plays_silence_without_libwebrtcs_transport() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.stale_device_buffer(i16::MAX);
    assert_eq!(h.monitor(true), MONITOR_APPLIED);
    calls(&h);
    assert!(h.platform_playing());
    let render = h.device_render();
    assert!(render.samples.iter().all(|&s| s == 0), "{:?}", &render.samples[..4]);
    assert_eq!(render.frames, 480);
}

/// The speaker's volume and mute are the session or endpoint volume: the
/// proxy never sets them, and engine init's speaker and microphone access
/// never reaches the device.
#[test]
fn adm_proxy_never_touches_the_speaker_volume_or_mute() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.hold(true);
    h.app_recording(true);
    calls(&h);
    h.take_channel_calls();

    assert_eq!(h.engine_set_speaker_volume(255), -1);
    assert_eq!(h.engine_set_speaker_mute(true), -1);
    assert_eq!(h.engine_adm_init(), [0, 0, 0, 0]);
    h.flush();
    assert_eq!(h.take_channel_calls(), Vec::<String>::new());
    assert_eq!(calls(&h), Vec::<String>::new());
}

#[test]
fn adm_proxy_stats_count_mixer_source_levels() {
    let h = platform(&["{spk}"], &["{mic}"]);
    let stats = h.stats();
    assert!(stats.mix_source_levels_available);
    assert!(stats.mix_source_levels.is_empty());
}

fn settle(h: &AdmProxyHarness) -> Vec<String> {
    h.flush();
    h.flush();
    h.take_calls()
}

#[test]
fn adm_proxy_runs_the_platform_adms_listeners_on_the_device_thread() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.hold(true);
    assert_eq!(calls(&h), [default_device("Playout"), "InitPlayout".into(), "StartPlayout".into()]);
    assert_eq!(
        h.routed_listeners(),
        2,
        "Init's devices listener and InitPlayout's format listener"
    );

    h.notify_devices_changed();
    assert_eq!(
        settle(&h),
        ["listener: devices", "StopPlayout", "SetPlayoutDevice(0)", "InitPlayout", "StartPlayout"]
    );
    assert!(h.listeners_ran_on_device_thread());
    assert!(h.platform_playing());
    assert_eq!(h.overlapping_device_calls(), 0);
}

#[test]
fn adm_proxy_holds_a_listener_notification_until_the_device_operation_in_progress_ends() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.block_next_start_playout();
    h.hold(true);
    assert!(h.start_playout_blocked());

    h.notify_devices_changed();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(h.listener_calls(), 0, "the notification waits for the device thread");

    h.release_start_playout(0);
    assert_eq!(
        settle(&h),
        [
            default_device("Playout"),
            "InitPlayout".into(),
            "StartPlayout".into(),
            "listener: devices".into(),
            "StopPlayout".into(),
            "SetPlayoutDevice(0)".into(),
            "InitPlayout".into(),
            "StartPlayout".into()
        ]
    );
    assert_eq!(h.overlapping_device_calls(), 0);
    assert!(h.platform_playing());
}

#[test]
fn adm_proxy_reopens_what_the_platform_adms_listener_closed() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.hold(true);
    calls(&h);
    h.set_listener_restarts(false);

    h.notify_devices_changed();
    assert_eq!(
        settle(&h),
        [
            "listener: devices".into(),
            "StopPlayout".into(),
            default_device("Playout"),
            "InitPlayout".into(),
            "StartPlayout".into()
        ]
    );
    assert!(h.platform_playing());
}

#[test]
fn adm_proxy_drops_a_notification_whose_listener_was_removed_before_it_ran() {
    let h = platform(&["{spk}"], &["{mic}"]);
    h.block_next_start_playout();
    h.hold(true);
    assert!(h.start_playout_blocked());
    h.hold(false);
    h.notify_format_changed();
    assert_eq!(h.delivered_listener_notifications(), 1);

    h.release_start_playout(0);
    assert_eq!(
        settle(&h),
        [
            default_device("Playout"),
            "InitPlayout".into(),
            "StartPlayout".into(),
            "StopPlayout".into()
        ]
    );
    assert_eq!(h.listener_calls(), 0, "StopPlayout removed the format listener before it ran");
}

#[test]
fn adm_proxy_runs_no_listener_after_shutdown() {
    let h = platform(&["{spk}"], &["{mic}"]);
    assert!(h.add_listener_from_test_thread(true));
    h.shutdown();
    h.take_calls();

    h.notify_default_output_changed();
    assert_eq!(h.delivered_listener_notifications(), 1);
    assert_eq!(settle(&h), Vec::<String>::new());
    assert_eq!(h.listener_calls(), 0);
    h.remove_listener_from_test_thread(true);
    assert_eq!(h.core_audio_listeners(), 0);
}

#[test]
fn adm_listener_router_routes_only_the_platform_adms_listeners() {
    let h = platform(&["{spk}"], &["{mic}"]);
    assert!(!h.add_listener_from_test_thread(false), "another listener goes to CoreAudio as it is");
    assert!(
        h.add_listener_from_test_thread(true),
        "the ADM's own listener is routed wherever it is added"
    );
    assert!(h.add_listener_from_test_thread(true));
    assert_eq!(h.core_audio_listeners(), 3, "Init's, the foreign one and one routed listener");

    h.notify_devices_changed();
    assert_eq!(h.foreign_listener_calls(), 1, "called on CoreAudio's thread before it returned");
    h.notify_default_output_changed();
    assert_eq!(settle(&h), ["listener: devices", "listener: default output"]);
    assert!(h.listeners_ran_on_device_thread());

    h.remove_listener_from_test_thread(false);
    h.remove_listener_from_test_thread(true);
    assert_eq!(h.core_audio_listeners(), 1, "Init's devices listener");
}
