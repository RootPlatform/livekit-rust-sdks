#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects
)]

mod support;

use rootapp_voice_dsp::speech_gate::{
    probability, GateControl, SpeechGate, CONFIRM_FRAMES, HANG_FRAMES, ONSET_QUIET_FRAMES,
    ONSET_RUN_FRAMES, PROVISIONAL_FRAMES, REARM_FRAMES, SPEECH_OFF, SPEECH_ON,
};
use rootapp_voice_dsp::{
    DspChain, DspCommands, DspOutputs, DspSettings, PushToTalk, SpeechDetection,
    STARTUP_HOLD_FRAMES,
};
use support::{silent, sine, Rng};

const N: usize = 480;
const SPEECH: f32 = 0.9;
const NOISE: f32 = 0.0;

/// Manual sensitivity at 0.01 (-40 dBFS), no voice gain: the level gate is predictable.
fn chain(detection: SpeechDetection) -> (DspChain, DspSettings) {
    let settings = DspSettings {
        auto_gain: false,
        auto_sensitivity: false,
        gate_threshold: 0.01,
        speech_detection: detection,
        ..DspSettings::default()
    };
    (DspChain::new(48_000), settings)
}

/// What a scripted frame holds (±1 RMS).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Sound {
    /// 0.0006, under the 0.01 threshold.
    Quiet,
    /// 0.017 uniform noise, above it.
    Noise,
    /// A 0.07 tone, 12 dB above the noise.
    Voice,
    /// A 0.014 tone, just above the threshold.
    Soft,
    /// 0.0001, under QUIET_RMS.
    Hush,
}

fn frame_of(sound: Sound, f: usize, rng: &mut Rng) -> Vec<f32> {
    (0..N)
        .map(|i| {
            let v = match sound {
                Sound::Quiet => 0.001 * rng.symmetric(),
                Sound::Noise => 0.03 * rng.symmetric(),
                Sound::Voice => sine(0.1, 220.0, f * N + i, 48_000),
                Sound::Soft => sine(0.02, 220.0, f * N + i, 48_000),
                Sound::Hush => 0.000_17 * rng.symmetric(),
            };
            v * 32_768.0
        })
        .collect()
}

/// FloatS16 frame: a 0.1 tone ("speech") or 0.03 uniform noise (RMS 0.017, above the threshold).
fn frame(loud: bool, f: usize, rng: &mut Rng) -> Vec<f32> {
    frame_of(if loud { Sound::Voice } else { Sound::Noise }, f, rng)
}

struct Run {
    sent: Vec<Vec<f32>>,
    speaking: Vec<bool>,
}

impl Run {
    fn energy(&self, f: usize) -> f64 {
        self.sent[f].iter().map(|&v| f64::from(v) * f64::from(v)).sum()
    }
}

fn run_sounds(
    detection: SpeechDetection,
    script: &[(Sound, f32)],
    tweak: impl Fn(usize, &mut DspSettings),
) -> Run {
    let (mut chain, mut settings) = chain(detection);
    let mut out = DspOutputs::default();
    let commands = DspCommands::default();
    let mut rng = Rng::new(7);
    let mut r = Run { sent: vec![], speaking: vec![] };
    for (f, &(sound, p)) in script.iter().enumerate() {
        let mut x = frame_of(sound, f, &mut rng);
        settings.speech_probability = p;
        tweak(f, &mut settings);
        chain.process(&mut x, &settings, &commands, &mut out);
        r.sent.push(x);
        r.speaking.push(out.speaking);
    }
    r
}

/// `script[f]`: (loud, probability).
fn run(
    detection: SpeechDetection,
    script: &[(bool, f32)],
    tweak: impl Fn(usize, &mut DspSettings),
) -> Run {
    let sounds: Vec<(Sound, f32)> = script
        .iter()
        .map(|&(loud, p)| (if loud { Sound::Voice } else { Sound::Noise }, p))
        .collect();
    run_sounds(detection, &sounds, tweak)
}

const START: usize = STARTUP_HOLD_FRAMES as usize + 5;

/// Speech for 100 frames, then 400 frames of noise above the threshold that the detector rejects.
fn speech_then_noise() -> Vec<(bool, f32)> {
    let mut s = vec![(false, NOISE); START];
    s.extend(std::iter::repeat_n((true, SPEECH), 100));
    s.extend(std::iter::repeat_n((false, NOISE), 400));
    s
}

/// Frames of `range` in which Gate sends under a quarter of Off's energy (6 dB less).
fn lost(off: &Run, gate: &Run, range: std::ops::Range<usize>) -> Vec<usize> {
    range.filter(|&f| gate.energy(f) < 0.25 * off.energy(f)).collect()
}

#[test]
fn off_and_no_probability_match_the_level_gate_exactly() {
    let script = speech_then_noise();
    let off = run(SpeechDetection::Off, &script, |_, _| {});
    for detection in [SpeechDetection::Ring, SpeechDetection::Gate] {
        let none = run(detection, &script, |_, s| s.speech_probability = -1.0);
        assert_eq!(off.sent, none.sent);
        assert_eq!(off.speaking, none.speaking);
        let nan = run(detection, &script, |_, s| s.speech_probability = f32::NAN);
        assert_eq!(off.sent, nan.sent);
    }
}

#[test]
fn push_to_talk_ignores_the_detector() {
    let script = speech_then_noise();
    for ptt in [PushToTalk::Held, PushToTalk::Released] {
        let off = run(SpeechDetection::Off, &script, |_, s| s.push_to_talk = ptt);
        let gate = run(SpeechDetection::Gate, &script, |_, s| s.push_to_talk = ptt);
        assert_eq!(off.sent, gate.sent);
        assert_eq!(off.speaking, gate.speaking);
    }
}

#[test]
fn level_gate_holds_open_on_noise_the_detector_rejects() {
    let script = speech_then_noise();
    let off = run(SpeechDetection::Off, &script, |_, _| {});
    let end = START + 100;
    assert!(off.speaking[end + 399], "the level gate alone keeps the ring on through the noise");
    assert!(!silent(&off.sent[end + 399]));
}

#[test]
fn ring_ends_a_hang_after_speech_and_the_gate_closes() {
    let script = speech_then_noise();
    let end = START + 100;
    let gate = run(SpeechDetection::Gate, &script, |_, _| {});
    assert!(gate.speaking[end - 1]);
    let off_at = (end..end + 400).find(|&f| !gate.speaking[f]).unwrap();
    assert_eq!(off_at - end, HANG_FRAMES as usize);
    let closed_at = (end..end + 400).find(|&f| silent(&gate.sent[f])).unwrap();
    assert!(
        closed_at - end <= HANG_FRAMES as usize + 7,
        "closed {} frames after speech",
        closed_at - end
    );
    assert!(
        (closed_at..end + 400).all(|f| silent(&gate.sent[f])),
        "stays shut while the noise lasts"
    );
}

#[test]
fn ring_mode_changes_only_the_ring() {
    let script = speech_then_noise();
    let off = run(SpeechDetection::Off, &script, |_, _| {});
    let ring = run(SpeechDetection::Ring, &script, |_, _| {});
    assert_eq!(off.sent, ring.sent);
    let end = START + 100;
    assert!(!ring.speaking[end + HANG_FRAMES as usize + 1]);
}

#[test]
fn noise_burst_never_lights_the_ring_and_closes_after_the_provisional_window() {
    let burst = START + 50;
    let script = vec![(false, 0.0f32); burst + 200];
    let gate = run(SpeechDetection::Gate, &script, |f, s| {
        s.gate_threshold = if f < burst { 0.5 } else { 0.01 };
    });
    assert!(gate.speaking.iter().all(|&s| !s));
    let open: Vec<usize> = (burst..burst + 200).filter(|&f| !silent(&gate.sent[f])).collect();
    assert!(!open.is_empty(), "the level opens provisionally");
    assert!(open.len() <= PROVISIONAL_FRAMES as usize + 7, "open {} frames", open.len());
}

#[test]
fn confirmed_speech_lights_the_ring_after_the_confirm_frames() {
    let mut script = vec![(false, 0.0f32); START];
    script.extend(std::iter::repeat_n((true, 0.1), 3));
    script.extend(std::iter::repeat_n((true, SPEECH), 50));
    let gate = run(SpeechDetection::Gate, &script, |f, s| {
        s.gate_threshold = if f < START { 0.5 } else { 0.01 };
    });
    let first = (0..script.len()).find(|&f| gate.speaking[f]).unwrap();
    assert_eq!(first, START + 3 + CONFIRM_FRAMES as usize - 1);
    assert!(!silent(&gate.sent[START + 1]), "the level opened before the detector confirmed");
}

#[test]
fn after_a_detector_close_the_level_reopens_only_after_a_quiet_spell() {
    let closed = START + 100 + 100;
    for (quiet, reopens) in [(REARM_FRAMES as usize - 1, false), (REARM_FRAMES as usize, true)] {
        let mut script = speech_then_noise();
        script.truncate(closed + quiet + 60);
        let gate = run(SpeechDetection::Gate, &script, |f, s| {
            s.gate_threshold = if (closed..closed + quiet).contains(&f) { 0.5 } else { 0.01 };
        });
        assert!(silent(&gate.sent[closed - 1]), "loud but rejected: shut");
        let after = closed + quiet;
        let opened = (after..after + 60).any(|f| !silent(&gate.sent[f]));
        assert_eq!(opened, reopens, "{quiet} quiet frames");
    }
}

/// Noise held shut by the detector from `held` on (speech, then rejected noise).
fn held_noise(extra: usize) -> (Vec<(Sound, f32)>, usize) {
    let mut s = vec![(Sound::Quiet, NOISE); START];
    s.extend(std::iter::repeat_n((Sound::Voice, SPEECH), 100));
    s.extend(std::iter::repeat_n((Sound::Noise, NOISE), 100 + extra));
    (s, START + 200)
}

#[test]
fn a_held_gate_opens_on_a_frame_at_speech_off() {
    let (mut script, held) = held_noise(0);
    script.extend(std::iter::repeat_n((Sound::Noise, 0.1), 4));
    script.push((Sound::Noise, SPEECH_OFF));
    script.extend(std::iter::repeat_n((Sound::Noise, 0.1), 10));
    let gate = run_sounds(SpeechDetection::Gate, &script, |_, _| {});
    assert!(silent(&gate.sent[held - 1]), "held");
    assert!(
        (held..held + 4).all(|f| silent(&gate.sent[f])),
        "input as loud as what was rejected, below SPEECH_OFF, stays held"
    );
    assert!(!silent(&gate.sent[held + 4]), "the frame at SPEECH_OFF opens it");
}

#[test]
fn a_held_gate_opens_on_input_6_db_above_what_was_rejected() {
    let (mut script, held) = held_noise(0);
    // The noise is held; a tone 12 dB louder rises above it.
    script.extend(std::iter::repeat_n((Sound::Voice, 0.05), 10));
    let gate = run_sounds(SpeechDetection::Gate, &script, |_, _| {});
    assert!(silent(&gate.sent[held - 1]));
    assert!(!silent(&gate.sent[held]), "the louder input opens it at once");
}

#[test]
fn an_onset_that_lasts_opens_a_held_gate_and_clicks_or_flicker_do_not() {
    // Held noise, then the input goes quiet (under REARM_FRAMES) and comes back.
    let quiet = ONSET_QUIET_FRAMES as usize + 1;
    let cases: [(&str, Sound, Vec<Sound>, Option<usize>); 3] = [
        ("an onset", Sound::Quiet, vec![Sound::Soft; 8], Some(ONSET_RUN_FRAMES as usize - 1)),
        (
            "a click",
            Sound::Quiet,
            vec![Sound::Soft, Sound::Quiet, Sound::Quiet, Sound::Quiet, Sound::Quiet],
            None,
        ),
        ("flicker", Sound::Noise, vec![Sound::Noise; 8], None),
    ];
    for (name, between, after, opens_at) in cases {
        let (mut script, held) = held_noise(0);
        script.extend(std::iter::repeat_n((between, NOISE), quiet));
        script.extend(after.iter().map(|&s| (s, 0.1f32)));
        let gate = run_sounds(SpeechDetection::Gate, &script, |f, s| {
            // Flicker: the "quiet" frames are the same noise under a raised threshold.
            if between == Sound::Noise && (held..held + quiet).contains(&f) {
                s.gate_threshold = 0.02;
            }
        });
        let start = held + quiet;
        let first = (start..script.len()).find(|&f| !silent(&gate.sent[f]));
        match opens_at {
            Some(k) => assert_eq!(first, Some(start + k), "{name}"),
            None => assert_eq!(first, None, "{name}"),
        }
    }
}

#[test]
fn frames_at_speech_off_do_not_use_up_the_provisional_window() {
    // The first 20 frames of the open alternate SPEECH_OFF and 0.1: only 10 of them count.
    let burst = START + 50;
    let window = PROVISIONAL_FRAMES as usize;
    let script: Vec<(Sound, f32)> = (0..burst + 4 * window)
        .map(|f| {
            let p = if (burst..burst + window).contains(&f) && f % 2 == 0 { SPEECH_OFF } else { 0.1 };
            (if f < burst { Sound::Quiet } else { Sound::Noise }, p)
        })
        .collect();
    let gate = run_sounds(SpeechDetection::Gate, &script, |_, _| {});
    let open = (burst..script.len()).filter(|&f| !silent(&gate.sent[f])).count();
    assert!(open >= window + window / 2, "only the frames below SPEECH_OFF count: open {open}");
    assert!(open <= window + window / 2 + 8, "the window still ends: open {open}");
}

/// Speech, a soft tail, a quiet gap of `gap` frames, then a word whose detector needs `latency`
/// frames (p 0.1) before p 0.9.
fn two_words(tail: usize, gap: usize, latency: usize) -> (Vec<(Sound, f32)>, usize) {
    let mut s = vec![(Sound::Quiet, 0.0f32); START];
    s.extend(std::iter::repeat_n((Sound::Voice, SPEECH), 100 - tail));
    s.extend(std::iter::repeat_n((Sound::Soft, 0.2), tail));
    s.extend(std::iter::repeat_n((Sound::Quiet, 0.0), gap));
    let word = s.len();
    s.extend(std::iter::repeat_n((Sound::Voice, 0.1), latency));
    s.extend(std::iter::repeat_n((Sound::Voice, SPEECH), 100 - latency));
    (s, word)
}

#[test]
fn a_word_where_the_hang_runs_out_is_sent_whole() {
    // The hang ends HANG_FRAMES + 1 frames after the last frame at SPEECH_OFF: on the word's first
    // frame after a quiet gap of that length, or inside its first frames after shorter gaps and
    // soft tails (the review's trace: a word at the hang's end muted until p >= 0.5).
    let hang = HANG_FRAMES as usize;
    for tail in [0usize, 10, 16] {
        for gap in [hang - tail - 12, hang - tail - 6, hang - tail - 3, hang - tail, hang - tail + 2, hang + 10] {
            for latency in [3usize, 6, 9] {
                let (script, word) = two_words(tail, gap, latency);
                let off = run_sounds(SpeechDetection::Off, &script, |_, _| {});
                let gate = run_sounds(SpeechDetection::Gate, &script, |_, _| {});
                let lost = lost(&off, &gate, word..word + 30);
                assert!(lost.is_empty(), "tail {tail} gap {gap} latency {latency}: lost {lost:?}");
            }
        }
    }
}

#[test]
fn a_hang_that_runs_out_in_a_pause_releases_the_tail_fast() {
    let (script, word) = two_words(0, 60, 3);
    let off = run_sounds(SpeechDetection::Off, &script, |_, _| {});
    let gate = run_sounds(SpeechDetection::Gate, &script, |_, _| {});
    let end = word - 60;
    let hang_end = end + HANG_FRAMES as usize;
    assert!(!silent(&off.sent[hang_end + 5]), "the level gate alone is still releasing");
    assert!(silent(&gate.sent[hang_end + 3]), "the tail is released over 50 ms");
    assert!(lost(&off, &gate, word..word + 30).is_empty(), "the next word is sent whole");
}

#[test]
fn a_louder_word_after_held_noise_is_sent_at_once() {
    // Noise-filled pauses of 200-700 ms between two words: whether the hang runs out in the
    // noise or on the next word, a word 12 dB above the noise loses at most its first frame.
    for gap in [20usize, 25, 30, 35, 40, 50, 70] {
        for latency in [3usize, 6, 9] {
            let mut script = vec![(Sound::Quiet, 0.0f32); START];
            script.extend(std::iter::repeat_n((Sound::Voice, SPEECH), 100));
            script.extend(std::iter::repeat_n((Sound::Noise, 0.0), gap));
            let word = script.len();
            script.extend(std::iter::repeat_n((Sound::Voice, 0.1), latency));
            script.extend(std::iter::repeat_n((Sound::Voice, SPEECH), 60));
            let off = run_sounds(SpeechDetection::Off, &script, |_, _| {});
            let gate = run_sounds(SpeechDetection::Gate, &script, |_, _| {});
            let lost = lost(&off, &gate, word..word + 30);
            assert!(lost.len() <= 1, "gap {gap} latency {latency}: lost {lost:?}");
        }
    }
}

#[test]
fn a_mode_change_starts_the_verdict_over() {
    let (script, held) = held_noise(100);
    let gate = run_sounds(SpeechDetection::Gate, &script, |_, _| {});
    let switched = run_sounds(SpeechDetection::Gate, &script, |f, s| {
        if f == held + 20 {
            s.speech_detection = SpeechDetection::Ring;
        }
    });
    assert!((held..script.len()).all(|f| silent(&gate.sent[f])), "held while Gate stays on");
    assert!(
        (held + 21..held + 30).any(|f| !silent(&switched.sent[f])),
        "back in Gate, the held state is gone and the level opens again"
    );
}

#[test]
fn a_threshold_of_zero_still_rearms_and_near_silence_never_opens() {
    let (mut script, held) = held_noise(0);
    script.extend(std::iter::repeat_n((Sound::Hush, 0.0), REARM_FRAMES as usize + 20));
    script.extend(std::iter::repeat_n((Sound::Noise, 0.0), 10));
    let gate = run_sounds(SpeechDetection::Gate, &script, |_, s| s.gate_threshold = 0.0);
    let hush = held;
    let noise = hush + REARM_FRAMES as usize + 20;
    assert!(silent(&gate.sent[hush - 1]), "held at threshold 0");
    assert!((hush..noise).all(|f| silent(&gate.sent[f])), "near silence does not open it");
    assert!(!silent(&gate.sent[noise + 1]), "after the quiet spell the level opens again");
}

/// One random run for the property test: segments of noise floors and (modulated) tones with a
/// probability each.
fn random_script(rng: &mut Rng, frames: usize) -> Vec<(f32, Option<f32>, bool, f32)> {
    let mut script = Vec::with_capacity(frames);
    while script.len() < frames {
        let len = 2 + (rng.next_f64() * 150.0) as usize;
        let floor = 10f32.powf((-90.0 + rng.next_f64() * 40.0) as f32 / 20.0);
        let tone = (rng.next_f64() < 0.6)
            .then(|| 10f32.powf((-75.0 + rng.next_f64() * 55.0) as f32 / 20.0));
        let modulated = rng.next_f64() < 0.5;
        let p = [0.0, 0.2, SPEECH_OFF, 0.4, SPEECH_ON, 0.9, -1.0, f32::NAN]
            [(rng.next_f64() * 8.0) as usize % 8];
        for _ in 0..len {
            script.push((floor, tone, modulated, p));
        }
    }
    script.truncate(frames);
    script
}

#[test]
fn gate_never_sends_more_than_off() {
    // Gate only ever closes what the level gate opens: its threshold is the level gate's own and
    // its envelope never exceeds the level gate's, so no sample leaves louder than with Off.
    let mut rng = Rng::new(2026);
    let frames = 900;
    let configs: [(bool, f32, bool); 6] = [
        (true, 0.025, true),
        (true, 0.025, false),
        (false, 0.0, false),
        (false, 0.001, true),
        (false, 0.003, false),
        (false, 0.02, false),
    ];
    for (auto_sensitivity, threshold, auto_gain) in configs {
        for run_index in 0..12 {
            let script = random_script(&mut rng, frames);
            let settings = DspSettings {
                auto_gain,
                auto_sensitivity,
                gate_threshold: threshold,
                ..DspSettings::default()
            };
            let mut off = DspChain::new(48_000);
            let mut gate = DspChain::new(48_000);
            let (mut out_off, mut out_gate) = (DspOutputs::default(), DspOutputs::default());
            let commands = DspCommands::default();
            let mut n = 0usize;
            for (f, &(floor, tone, modulated, p)) in script.iter().enumerate() {
                let x: Vec<f32> = (0..N)
                    .map(|i| {
                        let t = n + i;
                        let mut v = floor * 3f32.sqrt() * rng.symmetric();
                        if let Some(a) = tone {
                            let m = if modulated {
                                (0.5 - 0.5 * (2.0 * std::f64::consts::PI * 4.0 * t as f64 / 48_000.0).cos()) as f32
                            } else {
                                1.0
                            };
                            v += m * sine(a * std::f32::consts::SQRT_2, 220.0, t, 48_000);
                        }
                        v * 32_768.0
                    })
                    .collect();
                n += N;
                let (mut a, mut b) = (x.clone(), x);
                let s_off = DspSettings { speech_probability: p, ..settings };
                let s_gate = DspSettings { speech_detection: SpeechDetection::Gate, ..s_off };
                off.process(&mut a, &s_off, &commands, &mut out_off);
                gate.process(&mut b, &s_gate, &commands, &mut out_gate);
                assert_eq!(out_off.threshold, out_gate.threshold, "the threshold is the level gate's");
                assert!(out_gate.envelope <= out_off.envelope);
                for (i, (&o, &g)) in a.iter().zip(b.iter()).enumerate() {
                    assert!(
                        g.abs() <= o.abs(),
                        "auto {auto_sensitivity} threshold {threshold} gain {auto_gain} run {run_index} frame {f} sample {i}: Gate {g} vs Off {o}"
                    );
                }
                assert!(!out_gate.speaking || out_off.speaking);
            }
        }
    }
}

/// Quiet room noise (RMS `noise`), then `tone` (RMS) frames marked `p`, auto sensitivity on.
fn thresholds(detection: SpeechDetection, noise: f32, tone: f32, p: f32) -> Vec<f32> {
    let settings = DspSettings {
        auto_gain: false,
        auto_sensitivity: true,
        speech_detection: detection,
        ..DspSettings::default()
    };
    let mut chain = DspChain::new(48_000);
    let mut out = DspOutputs::default();
    let commands = DspCommands::default();
    let mut rng = Rng::new(11);
    let quiet = 300;
    let mut thresholds = vec![];
    for f in 0..quiet + 300 {
        let loud = f >= quiet;
        let mut x: Vec<f32> = (0..N)
            .map(|i| {
                let v = if loud {
                    sine(tone * std::f32::consts::SQRT_2, 220.0, f * N + i, 48_000)
                } else {
                    noise * 3f32.sqrt() * rng.symmetric()
                };
                v * 32_768.0
            })
            .collect();
        let settings = DspSettings { speech_probability: if loud { p } else { 0.0 }, ..settings };
        chain.process(&mut x, &settings, &commands, &mut out);
        thresholds.push(out.threshold);
    }
    thresholds.split_off(quiet)
}

#[test]
fn gate_mode_leaves_auto_sensitivity_as_off_has_it() {
    // A quiet talker the detector confirms under the threshold (the review's case: Gate opened
    // 3 dB lower than Off, and sent background speech Off never sends), and steady noise the
    // detector rejects: the thresholds are Off's, bit for bit.
    let db = |v: f32| 10f32.powf(v / 20.0);
    for (noise, tone, p) in [(-66.0, -59.5, SPEECH), (-66.0, -50.0, 0.0), (-75.0, -62.0, SPEECH)] {
        let off = thresholds(SpeechDetection::Off, db(noise), db(tone), p);
        let gate = thresholds(SpeechDetection::Gate, db(noise), db(tone), p);
        assert_eq!(off, gate, "noise {noise} tone {tone} p {p}");
    }
}

#[test]
fn speech_gate_confirms_only_a_run_of_confirm_frames() {
    let mut g = SpeechGate::default();
    for _ in 0..CONFIRM_FRAMES - 1 {
        g.observe(SPEECH_ON, false, true);
    }
    g.observe(SPEECH_ON - 0.01, false, true);
    for _ in 0..CONFIRM_FRAMES - 1 {
        g.observe(1.0, false, true);
        assert!(!g.confirmed(), "a frame below SPEECH_ON starts the run over");
    }
    g.observe(1.0, false, true);
    assert!(g.confirmed());
}

#[test]
fn speech_gate_continues_at_the_lower_level_and_ends_a_hang_after() {
    let mut g = SpeechGate::default();
    for _ in 0..CONFIRM_FRAMES {
        g.observe(0.9, true, false);
    }
    for _ in 0..200 {
        g.observe(SPEECH_OFF, true, false);
        assert!(g.confirmed(), "SPEECH_OFF and above continue");
    }
    for _ in 0..HANG_FRAMES {
        g.observe(SPEECH_OFF - 0.01, true, false);
        assert!(g.confirmed());
    }
    g.observe(0.0, true, false);
    assert!(!g.confirmed());
    assert!(!g.detector_required(), "Ring mode never closes the gate");
}

const THRESHOLD: f32 = 0.01;
const ABOVE: f32 = 0.02;
const BELOW: f32 = 0.002;

/// Confirmed speech whose hang runs out on the next frame, with `before` gate inputs first.
fn hang_running_out(before: &[f32]) -> SpeechGate {
    let mut g = SpeechGate::default();
    for _ in 0..CONFIRM_FRAMES {
        g.observe(0.9, true, true);
        g.control(0.9, ABOVE, THRESHOLD, true);
    }
    for k in 0..HANG_FRAMES as usize {
        g.observe(0.0, true, true);
        let rms = before.get(k + before.len().saturating_sub(HANG_FRAMES as usize)).copied();
        g.control(0.0, rms.unwrap_or(ABOVE), THRESHOLD, true);
    }
    assert!(g.confirmed());
    g.observe(0.0, true, true);
    assert!(!g.confirmed(), "the hang ran out");
    g
}

#[test]
fn speech_gate_decides_the_end_of_the_hang_with_the_frame_s_level() {
    // Held above the threshold: close at once and hold.
    let mut g = hang_running_out(&[ABOVE; 30]);
    let control = g.control(0.0, ABOVE, THRESHOLD, true);
    assert!(control.fast_close && !control.allow_open && g.detector_required());
    g.after_gate(0.0);
    assert_eq!(
        g.control(0.0, ABOVE, THRESHOLD, false),
        GateControl { allow_open: false, fast_close: false, eager_open: false }
    );

    // Already quiet: release the tail fast, without a hold.
    let mut g = hang_running_out(&[BELOW; 30]);
    let control = g.control(0.0, BELOW, THRESHOLD, true);
    assert!(control.fast_close && control.allow_open && !g.detector_required());

    // A new word after a pause: its own provisional window, no close.
    let mut quiet_then_word = [BELOW; 30];
    quiet_then_word[29] = BELOW;
    let mut g = hang_running_out(&quiet_then_word);
    let control = g.control(0.1, ABOVE, THRESHOLD, true);
    assert!(!control.fast_close && control.allow_open && control.eager_open);
    let mut closed_after = None;
    for f in 1..PROVISIONAL_FRAMES + 5 {
        g.observe(0.1, true, true);
        if g.control(0.1, ABOVE, THRESHOLD, true).fast_close {
            closed_after = Some(f);
            break;
        }
    }
    // The step from the quiet gap counts as a rise for its first frames (they join the
    // rejected level two frames late), which restarts the window: at most 2 frames later.
    let closed_after = closed_after.unwrap();
    assert!(
        (PROVISIONAL_FRAMES..=PROVISIONAL_FRAMES + 2).contains(&closed_after),
        "the window still ends: {closed_after}"
    );

    // A hang that ends with the gate shut needs nothing.
    let mut closed = SpeechGate::default();
    for _ in 0..CONFIRM_FRAMES {
        closed.observe(0.9, false, true);
    }
    for _ in 0..=HANG_FRAMES {
        closed.observe(0.0, false, true);
    }
    assert_eq!(closed.control(0.0, ABOVE, THRESHOLD, false), GateControl::LEVEL);
}

#[test]
fn speech_gate_ends_a_tail_release_on_a_new_onset() {
    let mut g = hang_running_out(&[BELOW; 30]);
    assert!(g.control(0.0, BELOW, THRESHOLD, true).fast_close);
    for _ in 0..ONSET_QUIET_FRAMES - 1 {
        g.observe(0.0, true, true);
        assert!(g.control(0.0, BELOW, THRESHOLD, true).fast_close);
    }
    g.observe(0.1, true, true);
    let control = g.control(0.1, ABOVE, THRESHOLD, true);
    assert!(!control.fast_close && control.allow_open && control.eager_open);
}

#[test]
fn speech_gate_releases_an_open_it_never_confirmed_after_the_provisional_window() {
    let mut g = SpeechGate::default();
    for f in 1..=PROVISIONAL_FRAMES {
        g.observe(0.1, true, true);
        assert_eq!(g.control(0.1, ABOVE, THRESHOLD, true), GateControl::LEVEL, "frame {f}");
    }
    g.observe(0.1, true, true);
    let control = g.control(0.1, ABOVE, THRESHOLD, true);
    assert!(control.fast_close && !control.allow_open && g.detector_required());
}

#[test]
fn speech_gate_lets_speech_quiet_or_a_louder_input_open_again() {
    let rejected = || {
        let mut g = SpeechGate::default();
        for _ in 0..=PROVISIONAL_FRAMES {
            g.observe(0.0, true, true);
            g.control(0.0, ABOVE, THRESHOLD, true);
        }
        g.after_gate(0.0);
        assert!(g.detector_required());
        g
    };

    let mut g = rejected();
    assert!(!g.control(SPEECH_OFF - 0.01, ABOVE, THRESHOLD, false).allow_open);
    assert!(g.control(SPEECH_OFF, ABOVE, THRESHOLD, false).allow_open, "a frame at SPEECH_OFF");
    g.after_gate(0.5);
    assert!(!g.detector_required(), "reopened: a new provisional open");

    let mut g = rejected();
    for _ in 0..CONFIRM_FRAMES {
        g.observe(0.9, false, true);
    }
    assert!(g.confirmed() && !g.detector_required());

    let mut g = rejected();
    for _ in 0..REARM_FRAMES - 1 {
        assert!(!g.control(0.0, BELOW, THRESHOLD, false).allow_open);
    }
    assert!(g.control(0.0, BELOW, THRESHOLD, false).allow_open, "REARM_FRAMES quiet frames");
    assert!(!g.detector_required());

    let mut g = rejected();
    assert!(!g.control(0.0, ABOVE * 1.9, THRESHOLD, false).allow_open, "under 6 dB louder");
    let mut g2 = rejected();
    let control = g2.control(0.0, ABOVE * 2.1, THRESHOLD, false);
    assert!(control.allow_open && control.eager_open, "6 dB louder than what was rejected");
    assert!(g.detector_required() && !g2.detector_required());
}

#[test]
fn speech_gate_reads_only_finite_non_negative_probabilities() {
    assert_eq!(probability(-1.0), None);
    assert_eq!(probability(-0.0), Some(0.0));
    assert_eq!(probability(f32::NAN), None);
    assert_eq!(probability(f32::INFINITY), None);
    assert_eq!(probability(1.5), Some(1.0));
    assert_eq!(probability(0.4), Some(0.4));
    assert_eq!(SpeechDetection::from_wire(0), SpeechDetection::Off);
    assert_eq!(SpeechDetection::from_wire(1), SpeechDetection::Ring);
    assert_eq!(SpeechDetection::from_wire(2), SpeechDetection::Gate);
    assert_eq!(SpeechDetection::from_wire(3), SpeechDetection::Off);
}

#[test]
fn muting_keeps_the_detector_judging_and_unmuting_mid_speech_rings_at_once() {
    let mut script = vec![(false, 0.0f32); START];
    script.extend(std::iter::repeat_n((true, SPEECH), 60));
    let unmute = START + 40;
    let gate = run(SpeechDetection::Gate, &script, |f, s| {
        s.gate_threshold = if f < START { 0.5 } else { 0.01 };
        s.mic_muted = f < unmute;
    });
    assert!((0..unmute).all(|f| !gate.speaking[f] && silent(&gate.sent[f])));
    assert!(gate.speaking[unmute], "confirmed while muted");
}

#[test]
fn a_flush_starts_the_verdict_over() {
    let (mut chain, mut settings) = chain(SpeechDetection::Gate);
    let mut out = DspOutputs::default();
    let mut rng = Rng::new(3);
    let mut speaking = vec![];
    for f in 0..START + 60 {
        let mut x = frame(true, f, &mut rng);
        settings.speech_probability = SPEECH;
        let commands = DspCommands { flush: f == START + 30, ..DspCommands::default() };
        chain.process(&mut x, &settings, &commands, &mut out);
        speaking.push(out.speaking);
        assert_eq!(out.speech_probability, SPEECH);
    }
    assert!(speaking[START + 29]);
    assert!(!speaking[START + 30], "the startup hold and the confirmation start over");
}
