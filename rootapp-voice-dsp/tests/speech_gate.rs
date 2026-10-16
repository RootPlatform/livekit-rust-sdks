#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects
)]

mod support;

use rootapp_voice_dsp::speech_gate::{
    probability, SpeechGate, CONFIRM_FRAMES, HANG_FRAMES, QUIET_RMS, SPEECH_OFF, SPEECH_ON,
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
    let off = run(SpeechDetection::Off, &script, |_, s| s.speech_probability = -1.0);
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
fn ring_mode_matches_the_level_gate() {
    let script = speech_then_noise();
    let off = run(SpeechDetection::Off, &script, |_, _| {});
    let ring = run(SpeechDetection::Ring, &script, |_, _| {});
    assert_eq!(off.sent, ring.sent);
    assert_eq!(off.speaking, ring.speaking);
}

#[test]
fn every_frame_that_sends_audio_rings_in_every_mode() {
    let mut script = speech_then_noise();
    script.extend(std::iter::repeat_n((true, 0.4), 200));
    script.extend(std::iter::repeat_n((true, 0.1), 200));
    script.extend(std::iter::repeat_n((false, NOISE), 100));
    script.extend(std::iter::repeat_n((true, SPEECH), 100));
    for detection in [SpeechDetection::Off, SpeechDetection::Ring, SpeechDetection::Gate] {
        let r = run(detection, &script, |f, s| {
            s.gate_threshold = if f % 700 < 350 { 0.01 } else { 0.03 };
        });
        let sent = (0..script.len()).filter(|&f| !silent(&r.sent[f])).count();
        assert!(sent > 100, "{detection:?} sent {sent} frames");
        for f in 0..script.len() {
            assert!(silent(&r.sent[f]) || r.speaking[f], "{detection:?}: frame {f} sends audio without ringing");
        }
    }
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

#[test]
fn gate_sends_nothing_before_speech_is_confirmed() {
    let mut script = vec![(false, 0.0f32); START];
    script.extend(std::iter::repeat_n((true, 0.1), 3));
    script.extend(std::iter::repeat_n((true, SPEECH), 50));
    let gate = run(SpeechDetection::Gate, &script, |f, s| {
        s.gate_threshold = if f < START { 0.5 } else { 0.01 };
    });
    let confirmed = START + 3 + CONFIRM_FRAMES as usize - 1;
    assert!((0..confirmed).all(|f| silent(&gate.sent[f]) && !gate.speaking[f]), "nothing before the confirmation");
    let first = (0..script.len()).find(|&f| !silent(&gate.sent[f])).unwrap();
    assert_eq!(first, confirmed, "the confirmed frame opens the gate at once");
    assert!(gate.speaking[first]);
    assert!((first + 2..script.len()).all(|f| !silent(&gate.sent[f])), "confirmed speech goes out");
}

#[test]
fn noise_clicks_and_maybe_speech_never_go_out_unconfirmed() {
    for (sound, p, frames) in [
        (Sound::Noise, NOISE, 1usize),
        (Sound::Noise, NOISE, 2),
        (Sound::Voice, 0.0, 5),
        (Sound::Noise, NOISE, 400),
        (Sound::Voice, SPEECH_OFF, 400),
        (Sound::Voice, 0.45, 400),
        (Sound::Voice, SPEECH_ON, (CONFIRM_FRAMES - 1) as usize),
    ] {
        let mut script = vec![(Sound::Quiet, NOISE); START];
        for _ in 0..20 {
            script.extend(std::iter::repeat_n((sound, p), frames));
            script.extend(std::iter::repeat_n((Sound::Quiet, NOISE), 15));
        }
        let gate = run_sounds(SpeechDetection::Gate, &script, |_, _| {});
        let off = run_sounds(SpeechDetection::Off, &script, |_, _| {});
        assert!(
            (START..script.len()).any(|f| !silent(&off.sent[f])),
            "{sound:?} p {p} x{frames}: the level gate alone sends it"
        );
        assert!(
            (0..script.len()).all(|f| silent(&gate.sent[f]) && !gate.speaking[f]),
            "{sound:?} p {p} x{frames}: Gate sends none of it and never rings"
        );
    }
}

#[test]
fn confirmed_speech_continues_through_uncertain_frames_and_ends_a_hang_after() {
    let mut script = vec![(false, 0.0f32); START];
    script.extend(std::iter::repeat_n((true, SPEECH), 20));
    script.extend(std::iter::repeat_n((true, SPEECH_OFF), 100));
    let uncertain_end = script.len();
    script.extend(std::iter::repeat_n((true, 0.0), 200));
    let gate = run(SpeechDetection::Gate, &script, |f, s| {
        s.gate_threshold = if f < START { 0.5 } else { 0.01 };
    });
    let off = run(SpeechDetection::Off, &script, |f, s| {
        s.gate_threshold = if f < START { 0.5 } else { 0.01 };
    });
    let confirmed = START + CONFIRM_FRAMES as usize - 1;
    assert!(lost(&off, &gate, confirmed + 1..uncertain_end + HANG_FRAMES as usize).is_empty(), "sent through the hang");
    let closed = (uncertain_end..script.len()).find(|&f| silent(&gate.sent[f])).unwrap();
    assert!(
        closed - uncertain_end <= HANG_FRAMES as usize + 7,
        "closed {} frames after the last uncertain frame",
        closed - uncertain_end
    );
    assert!((closed..script.len()).all(|f| silent(&gate.sent[f]) && !gate.speaking[f]), "stays shut");
}

#[test]
fn after_speech_rejected_noise_is_cut_and_a_new_word_opens_on_its_confirmation() {
    let mut script = speech_then_noise();
    let word = script.len();
    script.extend(std::iter::repeat_n((true, SPEECH), 50));
    let gate = run(SpeechDetection::Gate, &script, |_, _| {});
    let end = START + 100;
    let closed = (end..word).find(|&f| silent(&gate.sent[f])).unwrap();
    assert!(closed - end <= HANG_FRAMES as usize + 7, "closed {} frames after speech", closed - end);
    assert!((closed..word).all(|f| silent(&gate.sent[f])), "the noise stays cut");
    let reopened = (word..script.len()).find(|&f| !silent(&gate.sent[f])).unwrap();
    assert_eq!(reopened, word + CONFIRM_FRAMES as usize - 1, "the word goes out from its confirmation");
}

#[test]
fn speech_gate_confirms_only_a_run_of_confirm_frames() {
    let mut g = SpeechGate::default();
    for _ in 0..CONFIRM_FRAMES - 1 {
        g.observe(SPEECH_ON);
    }
    g.observe(SPEECH_ON - 0.01);
    for _ in 0..CONFIRM_FRAMES - 1 {
        g.observe(1.0);
        assert!(!g.confirmed(), "a frame below SPEECH_ON starts the run over");
    }
    g.observe(1.0);
    assert!(g.confirmed());
}

#[test]
fn speech_gate_continues_at_the_lower_level_and_ends_a_hang_after() {
    let mut g = SpeechGate::default();
    for _ in 0..CONFIRM_FRAMES {
        g.observe(0.9);
    }
    for _ in 0..200 {
        g.observe(SPEECH_OFF);
        assert!(g.confirmed(), "SPEECH_OFF and above continue");
    }
    for _ in 0..HANG_FRAMES {
        g.observe(SPEECH_OFF - 0.01);
        assert!(g.confirmed());
    }
    g.observe(0.0);
    assert!(!g.confirmed());
}

#[test]
fn speech_gate_opens_only_on_confirmed_speech_and_closes_fast_once_it_ends() {
    let mut g = SpeechGate::default();
    let loud = 0.02;
    let closed = g.control(loud, false);
    assert!(!closed.allow_open && !closed.fast_close, "unconfirmed: the gate may not open");
    assert!(!g.control(QUIET_RMS / 2.0, false).allow_open);

    for _ in 0..CONFIRM_FRAMES {
        g.observe(0.9);
    }
    let open = g.control(loud, false);
    assert!(open.allow_open && open.eager_open && !open.fast_close);
    assert!(!g.control(QUIET_RMS, false).allow_open, "near silence opens nothing");

    for _ in 0..=HANG_FRAMES {
        g.observe(0.0);
    }
    assert!(!g.confirmed());
    let closing = g.control(loud, true);
    assert!(closing.fast_close && !closing.allow_open, "an open gate past the hang closes fast");
    g.after_gate(0.5);
    assert!(g.control(loud, true).fast_close, "until it is shut");
    g.after_gate(0.0);
    let shut = g.control(loud, false);
    assert!(!shut.fast_close && !shut.allow_open);

    for _ in 0..CONFIRM_FRAMES {
        g.observe(0.9);
    }
    assert!(g.control(loud, true).allow_open && !g.control(loud, true).fast_close, "confirmed again");
}
