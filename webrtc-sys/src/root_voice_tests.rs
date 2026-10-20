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

//! The native voice path end to end over one capture state, wired as the
//! factory wires it (`RootVoiceHarness`): device frames through AdmProxy and
//! libwebrtc's builtin APM into the capture processor and the DSP crate, the
//! denoiser slot with fake Effects SDK pipelines, the controller with its
//! rootapp-effects thread, and the APM decorator applying policy on a worker
//! thread. No audio device is opened.

use std::time::{Duration, Instant};

use cxx::UniquePtr;

use crate::{
    audio_device_controller::ffi::{
        new_root_voice_harness, AudioDeviceController, RootAudioControlUpdate,
        RootDenoiserCompletionItem, RootSpeakingInfo, RootVoiceHarness,
    },
    root_audio_processing::ffi::root_speech_test_signal,
    root_denoiser::ffi::root_denoiser_supported,
};

const NOT_SPEAKING_AFTER_FRAMES: usize = 100;
const HOLD_FRAMES: usize = 20;
/// The transmit gate's fade at 48 kHz (5 ms).
const FADE: usize = 240;

/// A deterministic test signal at one rate, continuous across frames.
struct Signal {
    rate: u32,
    position: u64,
    noise: u32,
}

impl Signal {
    fn new(rate: u32) -> Self {
        Self { rate, position: 0, noise: 0x1234_5678 }
    }

    fn frames(&self) -> usize {
        (self.rate / 100) as usize
    }

    fn noise(&mut self) -> f64 {
        self.noise = self.noise.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        f64::from(self.noise >> 8) / f64::from(1u32 << 24) * 2.0 - 1.0
    }

    /// `floor`: white noise of that peak amplitude added.
    fn frame(&mut self, floor: f64, sample: impl Fn(f64) -> f64) -> Vec<i16> {
        let start = self.position;
        self.position += self.frames() as u64;
        (0..self.frames())
            .map(|i| {
                let t = (start + i as u64) as f64 / f64::from(self.rate);
                let noise = floor * self.noise();
                (sample(t) + noise).round().clamp(-32768.0, 32767.0) as i16
            })
            .collect()
    }

    /// Syllables of a 180 Hz voice with two harmonics, 200 ms on and 100 ms
    /// off with 10 ms edges (on when the startup hold ends), over a noise
    /// floor near -75 dBFS.
    fn speech(&mut self, amplitude: f64) -> Vec<i16> {
        self.frame(10.0, |t| {
            let phase = (t + 0.1) % 0.3;
            let edge = (phase / 0.01).min((0.2 - phase) / 0.01).clamp(0.0, 1.0);
            let w = 2.0 * std::f64::consts::PI * 180.0 * t;
            amplitude * edge * (w.sin() + 0.5 * (2.0 * w).sin() + 0.25 * (3.0 * w).sin()) / 1.75
        })
    }

    /// 1 kHz without noise: every 10 ms frame the same at 8-48 kHz.
    fn tone(&mut self, amplitude: f64) -> Vec<i16> {
        self.frame(0.0, |t| amplitude * (2.0 * std::f64::consts::PI * 1000.0 * t).sin())
    }

    /// The noise floor alone.
    fn quiet(&mut self) -> Vec<i16> {
        self.frame(10.0, |_| 0.0)
    }
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
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A session has started and the app records.
fn started() -> UniquePtr<RootVoiceHarness> {
    let h = new_root_voice_harness();
    level_gate_and_chain_gain(&h);
    h.recording(true);
    h
}

fn control(h: &RootVoiceHarness, update: RootAudioControlUpdate) {
    assert!(h.controller().set_root_audio_control(&update), "{update:?}");
}

/// The level gate alone and the chain's own voice gain, which these tests'
/// synthetic voices are written for (a session starts with GATE and AGC2).
fn level_gate_and_chain_gain(h: &RootVoiceHarness) {
    control(
        h,
        RootAudioControlUpdate {
            has_speech_detection: true,
            speech_detection: 0,
            has_gain_engine: true,
            gain_engine: 0,
            ..Default::default()
        },
    );
}

fn denoiser_wanted(h: &RootVoiceHarness, wanted: bool) {
    control(
        h,
        RootAudioControlUpdate {
            has_denoiser_wanted: true,
            denoiser_wanted: wanted,
            ..Default::default()
        },
    );
}

fn speaking(c: &AudioDeviceController) -> bool {
    c.root_audio_speaking().speaking
}

fn ok(owner: u64, async_id: u64) -> RootDenoiserCompletionItem {
    RootDenoiserCompletionItem { owner, async_id, error: String::new(), pipeline: 0 }
}

fn completions(c: &AudioDeviceController, count: usize) -> Vec<RootDenoiserCompletionItem> {
    let all = std::cell::RefCell::new(Vec::new());
    wait_until("denoiser completions", || {
        all.borrow_mut().extend(c.take_root_denoiser_completions());
        all.borrow().len() >= count
    });
    all.into_inner()
}

/// A fake pipeline at `rate`, installed before any frame (nobody speaks).
fn install(h: &RootVoiceHarness, rate: u32, scale: f32) -> u64 {
    let c = h.controller();
    let pipeline = h.add_fake_pipeline(scale, 0);
    assert!(c.set_root_denoiser(1, 10, pipeline, rate));
    assert_eq!(completions(c, 1), [ok(1, 10)]);
    assert!(c.root_denoiser_state().installed);
    pipeline
}

/// Speech until it is published as speaking; returns the frame it rose on.
fn speak_until_speaking(h: &RootVoiceHarness, signal: &mut Signal) -> usize {
    let rate = signal.rate as i32;
    for frame in 0..NOT_SPEAKING_AFTER_FRAMES {
        h.capture(rate, 1, &signal.speech(3000.0));
        if speaking(h.controller()) {
            return frame;
        }
    }
    panic!("speech was never published as speaking");
}

/// Frames of speech in real time (one every few ms, as a device delivers
/// them) for `duration`; the user talks throughout.
fn keep_talking(h: &RootVoiceHarness, signal: &mut Signal, duration: Duration) {
    let rate = signal.rate as i32;
    let until = Instant::now() + duration;
    while Instant::now() < until {
        h.capture(rate, 1, &signal.speech(3000.0));
        assert!(speaking(h.controller()), "talking throughout");
        std::thread::sleep(Duration::from_millis(8));
    }
}

/// Quiet until speaking is released; returns how many frames that took.
fn quiet_until_released(h: &RootVoiceHarness, signal: &mut Signal) -> usize {
    let rate = signal.rate as i32;
    for frame in 1..=NOT_SPEAKING_AFTER_FRAMES {
        h.capture(rate, 1, &signal.quiet());
        if !speaking(h.controller()) {
            return frame;
        }
    }
    panic!("speaking was never released");
}

#[test]
fn root_voice_speech_is_published_as_speaking_and_released_after_the_gate() {
    let h = started();
    let c = h.controller();
    let mut signal = Signal::new(48_000);
    let start = c.root_audio_speaking();
    assert!(!start.speaking);
    for _ in 0..HOLD_FRAMES {
        assert!(silent(&h.capture(48_000, 1, &signal.speech(3000.0))), "the startup hold");
    }
    let rose = HOLD_FRAMES + speak_until_speaking(&h, &mut signal);
    assert!(rose < HOLD_FRAMES + 3, "five gate blocks after the hold: frame {rose}");
    let mut audible = 0;
    for _ in 0..100 {
        let out = h.capture(48_000, 1, &signal.speech(3000.0));
        if sumsq(&out) > 480.0 * 500.0 * 500.0 {
            audible += 1;
        }
    }
    assert!(audible >= 60, "{audible} loud frames of 100");
    let talking = c.root_audio_speaking();
    assert_eq!(talking.seq, start.seq + 1, "one rise across the syllables");
    let stats = c.root_audio_stats();
    assert!(stats.speaking && stats.gate_speaking);
    assert_eq!(stats.speaking_seq, talking.seq);

    let quiet = quiet_until_released(&h, &mut signal);
    assert!((40..=80).contains(&quiet), "400 ms release and 300 ms hold: {quiet} frames");
    let released = c.root_audio_speaking();
    assert_eq!(released.seq, start.seq + 2);
    let stats = c.root_audio_stats();
    assert!(!stats.speaking && !stats.gate_speaking);
    assert_eq!(stats.startup_hold_frames, HOLD_FRAMES as u64);
    assert_eq!((stats.dsp_resets, stats.dsp_flushes), (1, 1), "the session start");
}

/// `frames` of `root_speech_test_signal(signal)` at 48 kHz as device frames.
fn speech_fixture(signal: u32, frames: u32) -> Vec<Vec<i16>> {
    root_speech_test_signal(signal, 48_000, frames)
        .chunks(480)
        .map(|frame| frame.iter().map(|&v| v.round().clamp(-32768.0, 32767.0) as i16).collect())
        .collect()
}

#[test]
fn root_voice_speech_detection_rings_for_keys_only_while_sent_and_rings_for_a_voice() {
    const KEYS: u32 = 6;
    const VOWEL: u32 = 0;
    let h = started();
    let c = h.controller();
    control(
        &h,
        RootAudioControlUpdate {
            has_auto_gain: true,
            auto_gain: false,
            has_auto_sensitivity: true,
            auto_sensitivity: false,
            has_gate_threshold: true,
            gate_threshold: 0.003,
            has_speech_detection: true,
            speech_detection: 2,
            ..Default::default()
        },
    );
    for frame in speech_fixture(KEYS, 300) {
        let sent = h.capture(48_000, 1, &frame);
        if sent.iter().any(|&s| s != 0) {
            assert!(speaking(c), "typing that is sent rings");
        }
    }
    let keys = c.root_audio_stats();
    assert_eq!((keys.speech_detection, keys.speech_frames), (2, 0));
    assert!(keys.speech_detector_running && keys.speech_closed_frames > 10);
    assert!(keys.speech_probability >= 0.0 && keys.speech_unavailable_frames == 0);

    let mut rose = None;
    let vowel = speech_fixture(VOWEL, 100);
    for (i, frame) in vowel.iter().enumerate() {
        h.capture(48_000, 1, frame);
        if rose.is_none() && speaking(c) {
            rose = Some(i);
        }
    }
    let rose = rose.expect("the voice rings");
    assert!(rose <= 10, "rings {} ms into the voice", rose * 10);
    let stats = c.root_audio_stats();
    assert!(stats.speech && stats.speaking && stats.speech_probability > 0.9);
    assert!(stats.speech_frames - keys.speech_frames >= 80);
    let quiet = quiet_until_released(&h, &mut Signal::new(48_000));
    assert!((25..=40).contains(&quiet), "the ring ends {} ms after the voice", quiet * 10);
}

#[test]
fn root_voice_mute_and_push_to_talk_key_up_send_silence_and_no_speaking() {
    let h = started();
    let c = h.controller();
    let mut signal = Signal::new(48_000);
    speak_until_speaking(&h, &mut signal);
    let seq = c.root_audio_speaking().seq;

    control(
        &h,
        RootAudioControlUpdate { has_mic_muted: true, mic_muted: true, ..Default::default() },
    );
    let muting = h.capture(48_000, 1, &signal.speech(3000.0));
    assert!(silent(&muting[FADE..]), "silent after the 5 ms fade");
    assert_eq!(
        c.root_audio_speaking(),
        RootSpeakingInfo { speaking: false, seq: seq + 1 },
        "muting stops speaking on the same frame"
    );
    let transmitted = c.root_audio_stats().transmit_frames;
    for _ in 0..50 {
        assert!(silent(&h.capture(48_000, 1, &signal.speech(3000.0))));
        assert!(!speaking(c));
    }
    let stats = c.root_audio_stats();
    assert!(stats.gate_speaking, "the gate keeps listening while muted");
    assert!(!stats.speaking);
    assert_eq!(stats.transmit_frames, transmitted);
    assert_eq!(stats.speaking_seq, seq + 1);

    control(
        &h,
        RootAudioControlUpdate { has_mic_muted: true, mic_muted: false, ..Default::default() },
    );
    assert!(!silent(&h.capture(48_000, 1, &signal.speech(3000.0))));
    assert!(speaking(c), "unmuted mid-word speaks at once");

    control(
        &h,
        RootAudioControlUpdate { has_push_to_talk: true, push_to_talk: 2, ..Default::default() },
    );
    let key_up = h.capture(48_000, 1, &signal.speech(3000.0));
    assert!(silent(&key_up[FADE..]));
    assert!(!speaking(c), "key up stops speaking on the same frame");
    for _ in 0..50 {
        assert!(silent(&h.capture(48_000, 1, &signal.speech(3000.0))));
        assert!(!speaking(c));
    }
    control(
        &h,
        RootAudioControlUpdate {
            has_mic_muted: true,
            mic_muted: true,
            has_push_to_talk: true,
            push_to_talk: 1,
            ..Default::default()
        },
    );
    for _ in 0..10 {
        assert!(silent(&h.capture(48_000, 1, &signal.speech(3000.0))), "muted with the key down");
        assert!(!speaking(c));
    }
}

#[test]
fn root_voice_push_to_talk_press_is_heard_within_one_frame_and_the_fade() {
    let h = started();
    let c = h.controller();
    control(
        &h,
        RootAudioControlUpdate {
            has_auto_gain: true,
            auto_gain: false,
            has_auto_sensitivity: true,
            auto_sensitivity: false,
            has_gate_threshold: true,
            gate_threshold: 0.01,
            ..Default::default()
        },
    );
    let mut signal = Signal::new(48_000);
    let mut open = Vec::new();
    for _ in 0..HOLD_FRAMES + 10 {
        open = h.capture(48_000, 1, &signal.tone(3000.0));
    }
    assert!(sumsq(&open) > 480.0 * 1500.0 * 1500.0);

    control(
        &h,
        RootAudioControlUpdate { has_push_to_talk: true, push_to_talk: 2, ..Default::default() },
    );
    for _ in 0..10 {
        h.capture(48_000, 1, &signal.tone(3000.0));
    }
    assert!(silent(&h.capture(48_000, 1, &signal.tone(3000.0))));
    assert!(!speaking(c));

    control(
        &h,
        RootAudioControlUpdate { has_push_to_talk: true, push_to_talk: 1, ..Default::default() },
    );
    std::thread::sleep(Duration::from_millis(2));
    let pressed = h.capture(48_000, 1, &signal.tone(3000.0));
    assert!(pressed[0].abs() <= 20, "the fade starts near 0: {}", pressed[0]);
    assert!(pressed[..FADE].iter().any(|&s| s != 0), "audio within the first 5 ms");
    // From the end of the fade (and of the gate's two-block attack) on, the
    // frame is what an open mic sends.
    for i in 256..480 {
        assert!(
            (i32::from(pressed[i]) - i32::from(open[i])).abs() <= 1,
            "{i}: {} vs {}",
            pressed[i],
            open[i]
        );
    }
    assert!(speaking(c), "a press speaks on its first frame");
    let delay = c.root_audio_stats().gate_open_delay_us;
    assert!((2_000..1_000_000).contains(&delay), "{delay} us from the press to its frame");
}

#[test]
fn root_voice_a_stalled_denoiser_gets_webrtc_ns_at_once_through_the_worker() {
    if !root_denoiser_supported() {
        return;
    }
    let h = started();
    let c = h.controller();
    control(
        &h,
        RootAudioControlUpdate {
            has_auto_gain: true,
            auto_gain: false,
            has_denoiser_wanted: true,
            denoiser_wanted: true,
            ..Default::default()
        },
    );
    let pipeline = install(&h, 48_000, 0.5);
    // A host AEC change first: applied on the worker, opening its 1 s window.
    control(
        &h,
        RootAudioControlUpdate {
            has_echo_cancellation: true,
            echo_cancellation: false,
            ..Default::default()
        },
    );
    wait_until("the host's change", || {
        let stats = c.root_audio_stats();
        stats.apm_policy_applies == 1 && !stats.apm_policy_pending
    });
    let aec_applied = Instant::now();
    let mut signal = Signal::new(48_000);
    speak_until_speaking(&h, &mut signal);
    let before = c.root_audio_stats();
    assert_eq!(before.ns_gap_frames, 0);
    assert!(!before.webrtc_ns_active && !before.webrtc_ns_fallback && !h.webrtc_ns_applied());
    assert!(h.fake_counts(pipeline).process_calls > HOLD_FRAMES as u64);

    h.hold_worker(true);
    h.set_fake_empty(pipeline, true);
    let mut empty = 0;
    while c.root_denoiser_state().bypass_reason == 0 {
        h.capture(48_000, 1, &signal.speech(3000.0));
        empty += 1;
        assert!(empty <= 100);
    }
    assert_eq!(empty, 100, "bypassed after 100 empty outputs in a row");
    assert_eq!(c.root_denoiser_state().bypass_reason, 2);
    assert_eq!(c.root_audio_stats().ns_gap_frames, 0, "the empty outputs were the denoiser's");
    let calls = h.fake_counts(pipeline).process_calls;
    wait_until("the native fallback", || c.root_denoiser_state().webrtc_ns_fallback);

    // The worker is held: every frame sent dry until WebRTC NS is applied is
    // a gap. The user is heard talking again.
    let mut dry = 0;
    while dry < 5 || !speaking(c) {
        h.capture(48_000, 1, &signal.speech(3000.0));
        dry += 1;
        assert!(dry < 40, "dry speech is heard");
    }
    assert_eq!(c.root_audio_stats().ns_gap_frames, dry);
    assert!(!h.webrtc_ns_applied());

    let released = Instant::now();
    h.hold_worker(false);
    wait_until("WebRTC NS applied", || h.webrtc_ns_applied());
    let waited = released.elapsed();
    assert!(waited < Duration::from_millis(500), "no pause: {waited:?}");
    let since_aec = aec_applied.elapsed();
    assert!(
        since_aec < Duration::from_secs(1),
        "inside the AEC change's 1 s window: {since_aec:?}"
    );
    let stats = c.root_audio_stats();
    assert!(stats.webrtc_ns_active && stats.webrtc_ns_fallback);
    assert_eq!(stats.denoiser_bypass_reason, 2);
    assert_eq!(stats.apm_policy_applies, before.apm_policy_applies + 1);
    assert_eq!(
        (stats.apm_policy_deferred, stats.apm_policy_forced),
        (before.apm_policy_deferred, before.apm_policy_forced),
        "applied while the user spoke, without waiting for a pause"
    );
    for _ in 0..5 {
        h.capture(48_000, 1, &signal.speech(3000.0));
    }
    assert_eq!(c.root_audio_stats().ns_gap_frames, dry, "WebRTC NS covers the bypassed denoiser");
    assert_eq!(h.fake_counts(pipeline).process_calls, calls, "never called again");
}

/// A hardware mute (the device sends int16 zeros) for longer than the stall
/// count and the fallback's grace: the decorator flags the raw frames, the
/// pipeline never sees them, nothing stalls, bypasses or falls back to WebRTC
/// NS, and speech after it is denoised from its first frame.
#[test]
fn root_voice_a_hardware_mute_never_reaches_the_denoiser_and_nothing_falls_back() {
    if !root_denoiser_supported() {
        return;
    }
    let h = started();
    let c = h.controller();
    control(
        &h,
        RootAudioControlUpdate {
            has_auto_gain: true,
            auto_gain: false,
            has_denoiser_wanted: true,
            denoiser_wanted: true,
            ..Default::default()
        },
    );
    let pipeline = install(&h, 48_000, 0.5);
    let mut signal = Signal::new(48_000);
    speak_until_speaking(&h, &mut signal);
    quiet_until_released(&h, &mut signal);
    let (fed, before) = (h.fake_counts(pipeline), c.root_audio_stats());
    let zeros = vec![0i16; 480];
    let muted = Instant::now();
    let mut frames = 0;
    while frames < 150 || muted.elapsed() < Duration::from_millis(1500) {
        assert!(silent(&h.capture(48_000, 1, &zeros)));
        frames += 1;
        std::thread::sleep(Duration::from_millis(2));
    }
    let counts = h.fake_counts(pipeline);
    assert_eq!(counts.process_calls, fed.process_calls, "the pipeline never saw the mute");
    assert_eq!(counts.silent_calls, fed.silent_calls);
    let stats = c.root_audio_stats();
    assert!(stats.denoiser_installed && stats.denoiser_bypass_reason == 0, "{stats:?}");
    assert!(!stats.webrtc_ns_fallback && !c.root_denoiser_state().webrtc_ns_fallback);
    assert_eq!(stats.ns_gap_frames, before.ns_gap_frames, "silence needs no noise suppression");
    assert_eq!(stats.denoiser_empty_outputs, before.denoiser_empty_outputs);

    let rose = speak_until_speaking(&h, &mut signal);
    assert!(rose < 5, "speech after the mute rings at once: frame {rose}");
    assert_eq!(h.fake_counts(pipeline).process_calls, fed.process_calls + rose as u64 + 1);
    assert_eq!(h.fake_counts(pipeline).silent_calls, fed.silent_calls);
}

#[test]
fn root_voice_an_install_asked_while_speaking_lands_at_the_next_pause() {
    if !root_denoiser_supported() {
        return;
    }
    let h = started();
    let c = h.controller();
    let mut signal = Signal::new(48_000);
    // Past the stream's first second, so wanting a denoiser holds nothing.
    for _ in 0..110 {
        h.capture(48_000, 1, &signal.speech(3000.0));
    }
    assert!(speaking(c));
    denoiser_wanted(&h, true);
    let pipeline = h.add_fake_pipeline(0.5, 0);
    let asked = Instant::now();
    assert!(c.set_root_denoiser(3, 30, pipeline, 48_000));
    keep_talking(&h, &mut signal, Duration::from_millis(300));
    assert!(!c.root_denoiser_state().installed, "not mid-word");
    assert!(c.take_root_denoiser_completions().is_empty());
    assert!(!c.root_denoiser_state().webrtc_ns_fallback, "no grace while it is on its way");

    quiet_until_released(&h, &mut signal);
    let paused = Instant::now();
    wait_until("the install", || c.root_denoiser_state().installed);
    let landed = paused.elapsed();
    assert!(landed < Duration::from_millis(500), "at the pause: {landed:?}");
    assert!(asked.elapsed() < Duration::from_secs(3));
    assert_eq!(completions(c, 1), [ok(3, 30)]);

    for _ in 0..10 {
        h.capture(48_000, 1, &signal.speech(3000.0));
    }
    assert_eq!(h.fake_counts(pipeline).process_calls, 10, "it runs from the next frame");
    let stats = c.root_audio_stats();
    assert_eq!((stats.denoiser_installs, stats.denoiser_frames), (1, 10));
    assert!(!stats.webrtc_ns_fallback);
}

#[test]
fn root_voice_an_install_lands_at_the_3_s_limit_while_the_user_keeps_talking() {
    if !root_denoiser_supported() {
        return;
    }
    let h = started();
    let c = h.controller();
    let mut signal = Signal::new(48_000);
    speak_until_speaking(&h, &mut signal);
    let pipeline = h.add_fake_pipeline(0.5, 0);
    let asked = Instant::now();
    assert!(c.set_root_denoiser(1, 11, pipeline, 48_000));
    // Frames keep coming and the user keeps talking.
    while !c.root_denoiser_state().installed {
        h.capture(48_000, 1, &signal.speech(3000.0));
        assert!(speaking(c), "talking throughout");
        assert!(asked.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(8));
    }
    let waited = asked.elapsed();
    assert!(
        waited >= Duration::from_millis(2_900) && waited < Duration::from_millis(4_000),
        "{waited:?}"
    );
    assert_eq!(completions(c, 1), [ok(1, 11)]);
}

/// Frames that stop mid-word without a stop (a device that died) leave
/// nobody speaking: an install waits for no pause that will never come.
#[test]
fn root_voice_an_install_does_not_wait_for_a_speaker_whose_frames_stopped() {
    if !root_denoiser_supported() {
        return;
    }
    let h = started();
    let c = h.controller();
    let mut signal = Signal::new(48_000);
    speak_until_speaking(&h, &mut signal);
    let pipeline = h.add_fake_pipeline(0.5, 0);
    let asked = Instant::now();
    assert!(c.set_root_denoiser(1, 12, pipeline, 48_000));
    wait_until("the install", || c.root_denoiser_state().installed);
    let waited = asked.elapsed();
    assert!(waited < Duration::from_millis(1_000), "{waited:?}");
    assert!(!speaking(c), "the poll cleared the stale flag");
    assert_eq!(completions(c, 1), [ok(1, 12)]);
}

/// A host AEC change while the user talks, with frames flowing in real time,
/// lands at the next pause.
#[test]
fn root_voice_a_host_aec_change_waits_for_a_pause_while_frames_flow() {
    let h = started();
    let c = h.controller();
    let mut signal = Signal::new(48_000);
    speak_until_speaking(&h, &mut signal);
    control(
        &h,
        RootAudioControlUpdate {
            has_echo_cancellation: true,
            echo_cancellation: false,
            ..Default::default()
        },
    );
    keep_talking(&h, &mut signal, Duration::from_millis(600));
    let waiting = c.root_audio_stats();
    assert!(waiting.apm_policy_pending, "deferred while talking");
    assert_eq!(waiting.apm_policy_applies, 0);

    quiet_until_released(&h, &mut signal);
    let paused = Instant::now();
    while c.root_audio_stats().apm_policy_pending {
        h.capture(48_000, 1, &signal.quiet());
        assert!(paused.elapsed() < Duration::from_millis(500), "at the pause");
        std::thread::sleep(Duration::from_millis(8));
    }
    let stats = c.root_audio_stats();
    assert!(!stats.echo_cancellation_active);
    assert_eq!(
        (stats.apm_policy_applies, stats.apm_policy_deferred, stats.apm_policy_forced),
        (1, 1, 0)
    );
}

/// A mid-call switch to a 16 kHz headset with the Effects SDK at 48 kHz: the
/// stream holds while no denoiser runs at the new rate, WebRTC NS covers it
/// after the grace, the host's 16 kHz pipeline takes over, and NS goes off
/// again only at a pause in speech.
#[test]
fn root_voice_a_rate_change_mid_call_is_covered_until_a_pipeline_at_the_new_rate_runs() {
    if !root_denoiser_supported() {
        return;
    }
    let h = started();
    let c = h.controller();
    denoiser_wanted(&h, true);
    let old = install(&h, 48_000, 0.5);
    let mut signal = Signal::new(48_000);
    for _ in 0..HOLD_FRAMES + 10 {
        h.capture(48_000, 1, &signal.speech(3000.0));
    }
    assert!(speaking(c));
    let ran = h.fake_counts(old).process_calls;
    assert_eq!(ran, HOLD_FRAMES as u64 + 10);

    let mut signal = Signal::new(16_000);
    let switched = Instant::now();
    let mut frames = 0;
    while !h.webrtc_ns_applied() {
        let out = h.capture(16_000, 1, &signal.speech(3000.0));
        frames += 1;
        if frames <= 100 {
            assert!(silent(&out), "{frames}: held, nothing denoises it yet");
        }
        assert!(switched.elapsed() < Duration::from_secs(4));
        std::thread::sleep(Duration::from_millis(8));
    }
    let covered = switched.elapsed();
    assert!(covered >= Duration::from_millis(950), "after the 1 s grace: {covered:?}");
    let state = c.root_denoiser_state();
    assert!(state.webrtc_ns_fallback && state.installed);
    assert_eq!((state.rate, state.bypass_reason), (48_000, 1));
    assert_eq!(h.fake_counts(old).process_calls, ran, "bypassed for its rate");

    quiet_until_released(&h, &mut signal);
    let new = h.add_fake_pipeline(0.5, 0);
    assert!(c.set_root_denoiser(1, 20, new, 16_000));
    assert_eq!(completions(c, 1), [ok(1, 20)], "between words: at once");
    let returned = c.take_root_denoiser_returned();
    assert!(returned.iter().any(|r| r.pipeline == old), "{returned:?}");

    let before = c.root_audio_stats();
    speak_until_speaking(&h, &mut signal);
    keep_talking(&h, &mut signal, Duration::from_millis(1_500));
    assert!(!c.root_denoiser_state().webrtc_ns_fallback, "the 16 kHz pipeline runs");
    assert!(h.webrtc_ns_applied(), "NS stays on while the user talks");
    assert!(c.root_audio_stats().apm_policy_pending);
    quiet_until_released(&h, &mut signal);
    let paused = Instant::now();
    while h.webrtc_ns_applied() {
        h.capture(16_000, 1, &signal.quiet());
        assert!(paused.elapsed() < Duration::from_millis(500), "at the pause");
        std::thread::sleep(Duration::from_millis(8));
    }
    let after = c.root_audio_stats();
    assert_eq!(after.apm_policy_deferred, before.apm_policy_deferred + 1);
    assert_eq!(after.apm_policy_forced, before.apm_policy_forced);
    assert_eq!(after.denoiser_rate, 16_000);
    assert!(h.fake_counts(new).process_calls > 100);
}

#[test]
fn root_voice_runs_at_16_and_8_khz() {
    for rate in [16_000u32, 8_000] {
        let h = started();
        let c = h.controller();
        let denoiser = root_denoiser_supported();
        let pipeline = if denoiser {
            denoiser_wanted(&h, true);
            Some(install(&h, 16_000, 0.5))
        } else {
            None
        };
        let mut signal = Signal::new(rate);
        let samples = (rate / 100) as usize;
        for _ in 0..HOLD_FRAMES {
            let out = h.capture(rate as i32, 1, &signal.speech(3000.0));
            assert_eq!(out.len(), samples, "{rate}");
            assert!(silent(&out), "{rate}: the startup hold");
        }
        let rose = speak_until_speaking(&h, &mut signal);
        assert!(rose < 3, "{rate}: {rose}");
        let format = c.root_audio_capture_format();
        assert_eq!(
            (format.sample_rate, format.channels),
            (16_000, 1),
            "{rate}: the APM runs at 16 kHz"
        );
        let mut loud = 0;
        for _ in 0..30 {
            let out = h.capture(rate as i32, 1, &signal.speech(3000.0));
            if sumsq(&out) > samples as f64 * 300.0 * 300.0 {
                loud += 1;
            }
        }
        assert!(loud >= 15, "{rate}: {loud} loud frames of 30");
        let quiet = quiet_until_released(&h, &mut signal);
        assert!((40..=80).contains(&quiet), "{rate}: {quiet}");

        let stats = c.root_audio_stats();
        assert_eq!(stats.process_rate, 16_000);
        assert_eq!(stats.stage_samples, stats.processed_frames * 160, "{rate}");
        assert_eq!(stats.startup_hold_frames, HOLD_FRAMES as u64, "{rate}");
        if let Some(pipeline) = pipeline {
            let counts = h.fake_counts(pipeline);
            assert_eq!(
                counts.process_calls, stats.processed_frames,
                "{rate}: every frame denoised"
            );
            assert_eq!((stats.denoiser_rate, stats.denoiser_bypass_reason), (16_000, 0), "{rate}");
            assert_eq!(stats.denoiser_frames, counts.process_calls, "{rate}");
            assert_eq!((stats.ns_gap_frames, stats.webrtc_ns_fallback), (0, false), "{rate}");
        }

        control(
            &h,
            RootAudioControlUpdate { has_mic_muted: true, mic_muted: true, ..Default::default() },
        );
        // The fade, and at 8 kHz the APM's resampler still holding a little.
        h.capture(rate as i32, 1, &signal.speech(3000.0));
        h.capture(rate as i32, 1, &signal.speech(3000.0));
        for _ in 0..20 {
            assert!(silent(&h.capture(rate as i32, 1, &signal.speech(3000.0))), "{rate}");
            assert!(!speaking(c), "{rate}");
        }
    }
}

#[test]
fn root_voice_a_new_recording_flushes_the_chain_and_the_denoiser() {
    let h = started();
    let c = h.controller();
    let denoiser = root_denoiser_supported();
    let pipeline = if denoiser {
        denoiser_wanted(&h, true);
        Some(install(&h, 48_000, 0.5))
    } else {
        None
    };
    let epoch = h.stream_epoch();
    let mut signal = Signal::new(48_000);
    speak_until_speaking(&h, &mut signal);
    let before = c.root_audio_stats();
    let seq = c.root_audio_speaking().seq;

    h.recording(false);
    assert!(!speaking(c), "no frame will clear it, so stopping does");
    assert_eq!(c.root_audio_speaking().seq, seq + 1);
    h.recording(true);
    assert_eq!(h.stream_epoch(), epoch + 1);
    for _ in 0..HOLD_FRAMES {
        assert!(silent(&h.capture(48_000, 1, &signal.speech(3000.0))), "the hold starts over");
    }
    speak_until_speaking(&h, &mut signal);

    let after = c.root_audio_stats();
    assert_eq!(after.dsp_flushes - before.dsp_flushes, 1);
    assert_eq!(after.dsp_resets, before.dsp_resets);
    assert_eq!(after.startup_hold_frames - before.startup_hold_frames, HOLD_FRAMES as u64);
    if let Some(pipeline) = pipeline {
        let counts = h.fake_counts(pipeline);
        assert_eq!(counts.flush_calls, 1, "the pipeline starts over with the stream");
        assert_eq!(counts.process_calls, after.processed_frames, "and runs on every frame");
        assert_eq!((after.denoiser_installs, after.denoiser_bypass_reason), (1, 0));
        assert_eq!(after.ns_gap_frames, 0);
    }
}

#[test]
fn root_voice_stats_agree_with_what_the_chain_sent() {
    let h = started();
    let c = h.controller();
    control(
        &h,
        RootAudioControlUpdate {
            has_auto_gain: true,
            auto_gain: false,
            has_auto_sensitivity: true,
            auto_sensitivity: false,
            has_gate_threshold: true,
            gate_threshold: 0.0,
            ..Default::default()
        },
    );
    let denoiser = root_denoiser_supported();
    let pipeline = if denoiser {
        denoiser_wanted(&h, true);
        Some(install(&h, 48_000, 0.5))
    } else {
        None
    };
    let mut signal = Signal::new(48_000);
    for _ in 0..HOLD_FRAMES {
        assert!(silent(&h.capture(48_000, 1, &signal.speech(3000.0))));
    }
    // A threshold of 0 opens the gate fully within two blocks.
    h.capture(48_000, 1, &signal.speech(3000.0));
    let s0 = c.root_audio_stats();
    let f0 = pipeline.map(|p| h.fake_counts(p));

    const FRAMES: u64 = 60;
    let mut sent = 0.0;
    let mut rounding = 0.0;
    let mut peak_db = f64::NEG_INFINITY;
    for _ in 0..FRAMES {
        let out = h.capture(48_000, 1, &signal.speech(3000.0));
        sent += sumsq(&out);
        rounding += out.iter().map(|&s| f64::from(s).abs() + 0.25).sum::<f64>();
        for block in out.chunks(128) {
            let rms = (sumsq(block) / block.len() as f64).sqrt() / 32768.0;
            peak_db = peak_db.max(20.0 * rms.log10());
        }
    }
    let s1 = c.root_audio_stats();

    assert_eq!(s1.processed_frames - s0.processed_frames, FRAMES);
    assert_eq!(s1.processed_frames, HOLD_FRAMES as u64 + 1 + FRAMES);
    assert_eq!(s1.stage_samples - s0.stage_samples, FRAMES * 480);
    assert_eq!(s1.transmit_frames - s0.transmit_frames, FRAMES);
    assert_eq!(s1.gate_open_frames - s0.gate_open_frames, FRAMES);
    assert_eq!(s1.gate_input_above_frames - s0.gate_input_above_frames, FRAMES);
    assert_eq!(s1.startup_hold_frames, HOLD_FRAMES as u64);
    assert_eq!((s1.unsupported_frames, s1.non_finite_samples, s1.ns_gap_frames), (0, 0, 0));

    let apm = s1.apm_out_sumsq - s0.apm_out_sumsq;
    let denoised = s1.denoised_sumsq - s0.denoised_sumsq;
    let gate_in = s1.gate_in_sumsq - s0.gate_in_sumsq;
    let chain_sent = s1.sent_sumsq - s0.sent_sumsq;
    // What left the chain is what the APM sent, before its int16 rounding.
    assert!((chain_sent - sent).abs() <= rounding, "{chain_sent} vs {sent} (bound {rounding})");
    // Gate fully open, unity input volume, transmit gate open, no voice gain.
    assert!((gate_in - chain_sent).abs() <= 1e-9 * chain_sent, "{gate_in} vs {chain_sent}");
    assert!((denoised - gate_in).abs() <= 1e-9 * gate_in, "{denoised} vs {gate_in}");
    if denoiser {
        assert!(
            (denoised - 0.25 * apm).abs() <= 0.02 * denoised,
            "half the amplitude: {denoised} vs {apm}"
        );
    } else {
        assert!((denoised - apm).abs() <= 1e-9 * apm);
    }

    assert!(
        (f64::from(s1.gate_in_block_peak_db) - peak_db).abs() < 0.05,
        "{} vs {peak_db}",
        s1.gate_in_block_peak_db
    );
    assert_eq!(s1.envelope, 1.0);
    assert_eq!((s1.threshold, s1.voice_gain_db, s1.voice_gain_settled), (0.0, 0.0, false));
    let published = c.root_audio_speaking();
    assert_eq!((s1.speaking, s1.speaking_seq), (published.speaking, published.seq));
    assert!(s1.speaking && s1.gate_speaking);
    assert!(s1.dsp_available && s1.policy_available);

    if let (Some(pipeline), Some(f0)) = (pipeline, f0) {
        let f1 = h.fake_counts(pipeline);
        assert_eq!(f1.process_calls - f0.process_calls, FRAMES);
        assert_eq!(s1.denoiser_frames, f1.process_calls);
        assert_eq!(s1.denoiser_frames, s1.processed_frames, "the denoiser ran on every frame");
        assert_eq!(s1.denoiser_process_us_buckets.iter().sum::<u64>(), s1.denoiser_frames);
        assert_eq!(
            (s1.denoiser_installs, s1.denoiser_rate, s1.denoiser_bypass_reason),
            (1, 48_000, 0)
        );
        assert_eq!(
            (s1.denoiser_misses, s1.denoiser_empty_outputs, s1.denoiser_bypassed_frames),
            (0, 0, 0)
        );
        assert_eq!(s1.denoiser_frames_until_first_output, 0);
        assert_eq!(
            (s1.denoiser_latency_ms, s1.denoiser_buffered_samples, s1.denoiser_underruns),
            (0.0, 0, 0)
        );
        assert_eq!(f1.last_power, 0.75, "the session's default power");
        assert!(s1.denoiser_available && !s1.webrtc_ns_fallback);
    }
}

const MONITOR_APPLIED: u8 = 0;
const MONITOR_SENDING: u8 = 2;
/// What libwebrtc's render side plays (and gives the APM as its reference).
const REMOTE: i16 = 1000;

fn without_remote(played: &[i16]) -> Vec<i16> {
    played.iter().map(|&s| s - REMOTE).collect()
}

#[test]
fn root_voice_monitor_plays_what_a_call_would_send_outside_the_aec_reference() {
    let h = new_root_voice_harness();
    level_gate_and_chain_gain(&h);
    let c = h.controller();
    assert!(!h.platform_playing() && !h.platform_recording());
    assert_eq!(h.monitor(true), MONITOR_APPLIED);
    assert!(h.platform_playing() && h.platform_recording(), "both devices open for it");
    h.set_remote_level(REMOTE);

    let mut signal = Signal::new(48_000);
    let mut sent_frames: Vec<Vec<i16>> = Vec::new();
    let mut heard = 0usize;
    for frame in 0..60 {
        let sent = h.capture(48_000, 1, &signal.speech(3000.0));
        let played = h.render(48_000, 2);
        assert!(
            h.reverse_input().iter().all(|&s| s == REMOTE),
            "frame {frame}: the echo canceller's reference is the remote mix alone"
        );
        let left: Vec<i16> = played.iter().step_by(2).copied().collect();
        let right: Vec<i16> = played.iter().skip(1).step_by(2).copied().collect();
        assert_eq!(left, right, "frame {frame}");
        if frame < 2 {
            assert!(silent(&without_remote(&left)), "30 ms are queued before it plays");
        } else {
            // During the startup hold, so its fade-in is silence too.
            assert_eq!(without_remote(&left), sent_frames[frame - 2], "frame {frame}");
        }
        if sumsq(&sent) > 0.0 {
            heard += 1;
        }
        sent_frames.push(sent);
    }
    assert!(heard >= 30, "{heard} frames carried the voice");

    let stats = c.root_audio_stats();
    assert!(stats.monitor_available && stats.monitor_active);
    assert_eq!((stats.monitor_frames, stats.monitor_dropped_frames), (58, 0));
    assert_eq!(stats.startup_hold_frames, HOLD_FRAMES as u64, "processed as in a call");

    // Muted, what a call would send is silence, and so is the monitor.
    control(
        &h,
        RootAudioControlUpdate { has_mic_muted: true, mic_muted: true, ..Default::default() },
    );
    for _ in 0..3 {
        h.capture(48_000, 1, &signal.speech(3000.0));
        h.render(48_000, 1);
    }
    for _ in 0..10 {
        assert!(silent(&h.capture(48_000, 1, &signal.speech(3000.0))));
        assert!(silent(&without_remote(&h.render(48_000, 1))));
    }

    assert_eq!(h.monitor(false), MONITOR_APPLIED);
    assert!(!h.platform_playing() && !h.platform_recording());
    assert!(!c.root_audio_stats().monitor_active);
}

#[test]
fn root_voice_monitor_converts_16_khz_capture_to_48_khz_playout() {
    let h = new_root_voice_harness();
    control(
        &h,
        RootAudioControlUpdate {
            has_auto_gain: true,
            auto_gain: false,
            has_auto_sensitivity: true,
            auto_sensitivity: false,
            has_gate_threshold: true,
            gate_threshold: 0.0,
            ..Default::default()
        },
    );
    assert_eq!(h.monitor(true), MONITOR_APPLIED);
    let mut signal = Signal::new(16_000);
    let mut sent_energy = 0.0;
    let mut played_energy = 0.0;
    let mut played_all = Vec::new();
    for frame in 0..80 {
        let sent = h.capture(16_000, 1, &signal.tone(4000.0));
        let played = h.render(48_000, 1);
        assert_eq!(played.len(), 480);
        if frame >= 40 {
            sent_energy += sumsq(&sent) / sent.len() as f64;
            played_energy += sumsq(&played) / played.len() as f64;
            played_all.extend(played);
        }
    }
    assert!(sent_energy > 0.0);
    let ratio_db = 10.0 * (played_energy / sent_energy).log10();
    assert!(ratio_db.abs() < 0.5, "played {ratio_db:.2} dB from what was sent");
    // 1 kHz for 400 ms: 800 sign changes.
    let crossings = played_all.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
    assert!((797..=803).contains(&crossings), "{crossings} sign changes in 400 ms");
    let stats = h.controller().root_audio_stats();
    assert_eq!(stats.process_rate, 16_000);
    assert_eq!(stats.monitor_dropped_frames, 0);
}

/// A mic test over 60 frames of a 1 kHz tone: on the right channel of a
/// stereo device (the mic on input 2), or as one channel holding what
/// libwebrtc's downmix makes of that. Returns what was sent and played.
fn stereo_mic_test(stereo: bool) -> (UniquePtr<RootVoiceHarness>, Vec<i16>, Vec<i16>) {
    let h = new_root_voice_harness();
    control(
        &h,
        RootAudioControlUpdate {
            has_auto_gain: true,
            auto_gain: false,
            has_auto_sensitivity: true,
            auto_sensitivity: false,
            has_gate_threshold: true,
            gate_threshold: 0.0,
            ..Default::default()
        },
    );
    assert_eq!(h.monitor(true), MONITOR_APPLIED);
    let mut signal = Signal::new(48_000);
    let (mut sent_all, mut played_all) = (Vec::new(), Vec::new());
    for frame in 0..60 {
        let tone = signal.tone(8000.0);
        let sent = if stereo {
            let input: Vec<i16> = tone.iter().flat_map(|&s| [0, s]).collect();
            h.capture(48_000, 2, &input)
        } else {
            let average: Vec<i16> = tone.iter().map(|&s| s / 2).collect();
            h.capture(48_000, 1, &average)
        };
        assert_eq!(sent.len(), 480, "frame {frame}: processed as one channel");
        sent_all.extend(sent);
        played_all.extend(h.render(48_000, 1));
    }
    (h, sent_all, played_all)
}

/// A call sends the average of a stereo mic's channels (libwebrtc remixes to
/// its mono send stream); the mic test, with no send stream, hears exactly
/// that instead of the left channel alone.
#[test]
fn root_voice_monitor_hears_a_stereo_mic_averaged_as_a_call_sends_it() {
    let (h, sent, played) = stereo_mic_test(true);
    let (_, sent_mono, played_mono) = stereo_mic_test(false);
    assert!(sumsq(&sent[24_000..]) > 0.0, "the right channel is heard");
    assert_eq!(sent, sent_mono, "processed as the mono average");
    assert_eq!(played, played_mono);
    let stats = h.controller().root_audio_stats();
    assert_eq!((stats.capture_channels, stats.process_channels), (2, 1));
    assert_eq!(stats.monitor_dropped_frames, 0);

    // A call that sends: libwebrtc gets both channels and remixes them to its
    // send streams' format itself.
    h.engine_recording(true);
    let both: Vec<i16> = (0..960).map(|i| if i % 2 == 1 { 1000 } else { 0 }).collect();
    assert_eq!(h.capture(48_000, 2, &both).len(), 960);
    h.engine_recording(false);
    assert_eq!(h.capture(48_000, 2, &both).len(), 480);
}

#[test]
fn root_voice_monitor_gives_way_to_a_call_that_sends() {
    let h = new_root_voice_harness();
    let c = h.controller();
    h.engine_recording(true);
    assert_eq!(h.monitor(true), MONITOR_SENDING);
    assert!(!h.platform_playing(), "a refused monitor opens nothing");
    h.engine_recording(false);
    assert!(!h.platform_recording());

    assert_eq!(h.monitor(true), MONITOR_APPLIED);
    let mut signal = Signal::new(48_000);
    for _ in 0..30 {
        h.capture(48_000, 1, &signal.speech(3000.0));
        h.render(48_000, 1);
    }
    assert!(c.root_audio_stats().monitor_frames > 0);

    h.engine_recording(true);
    let stats = c.root_audio_stats();
    assert!(!stats.monitor_active, "native turned the monitor off");
    assert!(h.platform_recording(), "the call keeps the microphone");
    assert!(!h.platform_playing(), "nothing else wanted playout");
    let frames = stats.monitor_frames;
    h.capture(48_000, 1, &signal.speech(3000.0));
    h.capture(48_000, 1, &signal.speech(3000.0));
    assert!(silent(&h.render(48_000, 1)));
    assert_eq!(c.root_audio_stats().monitor_frames, frames);
    assert_eq!(h.monitor(true), MONITOR_SENDING);
}
