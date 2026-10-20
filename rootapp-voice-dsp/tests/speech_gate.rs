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
    sanitize, Aligned, DenoiserOutput, DspChain, DspCommands, DspOutputs, DspSettings, GainEngine,
    PushToTalk, SpeechDetection, MAX_LOOKAHEAD_FRAMES, STARTUP_HOLD_FRAMES,
};
use support::{silent, sine, Rng};

const N: usize = 480;
const SPEECH: f32 = 0.9;
const NOISE: f32 = 0.0;
/// The default look-ahead (SPEECH_LOOKAHEAD_MS) in frames.
const LOOKAHEAD: usize = 4;

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
}

fn frame_of(sound: Sound, f: usize, rng: &mut Rng) -> Vec<f32> {
    (0..N)
        .map(|i| {
            let v = match sound {
                Sound::Quiet => 0.001 * rng.symmetric(),
                Sound::Noise => 0.03 * rng.symmetric(),
                Sound::Voice => sine(0.1, 220.0, f * N + i, 48_000),
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

/// Frames of `range` Gate sends (LOOKAHEAD frames later) at under a quarter of Off's energy
/// (6 dB less).
fn lost_delayed(off: &Run, gate: &Run, range: std::ops::Range<usize>) -> Vec<usize> {
    range.filter(|&f| gate.energy(f + LOOKAHEAD) < 0.25 * off.energy(f)).collect()
}

#[test]
fn off_and_no_probability_match_the_level_gate_exactly() {
    let script = speech_then_noise();
    let off = run(SpeechDetection::Off, &script, |_, s| s.speech_probability = -1.0);
    let none = run(SpeechDetection::Gate, &script, |_, s| s.speech_probability = -1.0);
    assert_eq!(off.sent, none.sent);
    assert_eq!(off.speaking, none.speaking);
    let nan = run(SpeechDetection::Gate, &script, |_, s| s.speech_probability = f32::NAN);
    assert_eq!(off.sent, nan.sent);
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
fn every_frame_that_sends_audio_rings_in_every_mode() {
    let mut script = speech_then_noise();
    script.extend(std::iter::repeat_n((true, 0.4), 200));
    script.extend(std::iter::repeat_n((true, 0.1), 200));
    script.extend(std::iter::repeat_n((false, NOISE), 100));
    script.extend(std::iter::repeat_n((true, SPEECH), 100));
    for detection in [SpeechDetection::Off, SpeechDetection::Gate] {
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

/// One frame through pre_denoise, post_denoise_align, the host's gain (External, `gain_db`) or
/// none, and post_gain: with External the audio does not depend on the probability (the chain's
/// own voice gain only measures).
fn run_frame(
    chain: &mut DspChain,
    frame: &mut [f32],
    settings: &DspSettings,
    gain_db: Option<f32>,
    out: &mut DspOutputs,
) {
    chain.pre_denoise(frame, settings, &DspCommands::default(), out);
    let aligned: Aligned = chain.post_denoise_align(frame, DenoiserOutput::NotInUse, settings, out);
    let applied = gain_db.unwrap_or(0.0);
    let scale = 10f32.powf(applied / 20.0);
    for s in frame.iter_mut() {
        *s *= scale;
    }
    chain.post_gain(frame, aligned.pre_gain_rms, applied, settings, out);
}

#[test]
fn gate_never_sends_more_than_off() {
    // Gate only ever closes what the level gate opens: its threshold is the level gate's own and,
    // at every frame the gate decides on, its envelope never exceeds the level gate's. With the
    // look-ahead it applies that envelope to the audio LOOKAHEAD frames older, so each sample it
    // sends is checked against the same level gate applied to the same older audio: Gate mode with
    // every frame that has a probability called speech (its speech gate then never closes and
    // never holds an open back but in the two frames a verdict needs after it starts over), on the
    // same look-ahead. That reference's envelope never exceeds Off's either.
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
    for (auto_sensitivity, threshold, host_gain) in configs {
        for run_index in 0..12 {
            let script = random_script(&mut rng, frames);
            let settings = DspSettings {
                auto_gain: host_gain,
                auto_sensitivity,
                gate_threshold: threshold,
                gain_engine: if host_gain { GainEngine::External } else { GainEngine::Legacy },
                ..DspSettings::default()
            };
            let mut off = DspChain::new(48_000);
            let mut gate = DspChain::new(48_000);
            let mut reference = DspChain::new(48_000);
            let (mut out_off, mut out_gate, mut out_ref) =
                (DspOutputs::default(), DspOutputs::default(), DspOutputs::default());
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
                let gain_db = host_gain.then(|| 9.0 + 9.0 * (f as f32 / 40.0).sin());
                let (mut a, mut b, mut c) = (x.clone(), x.clone(), x);
                let s_off = DspSettings { speech_probability: p, ..settings };
                let s_gate = DspSettings { speech_detection: SpeechDetection::Gate, ..s_off };
                let s_ref = DspSettings {
                    speech_probability: if probability(p).is_some() { 1.0 } else { p },
                    ..s_gate
                };
                run_frame(&mut off, &mut a, &s_off, gain_db, &mut out_off);
                run_frame(&mut gate, &mut b, &s_gate, gain_db, &mut out_gate);
                run_frame(&mut reference, &mut c, &s_ref, gain_db, &mut out_ref);
                let context = format!(
                    "auto {auto_sensitivity} threshold {threshold} host gain {host_gain} run {run_index} frame {f}"
                );
                assert_eq!(
                    out_off.threshold, out_gate.threshold,
                    "{context}: the threshold is the level gate's"
                );
                assert!(out_gate.envelope <= out_ref.envelope, "{context}");
                assert!(out_ref.envelope <= out_off.envelope, "{context}");
                assert_eq!(out_gate.speech_lookahead_ms, out_ref.speech_lookahead_ms, "{context}");
                for (i, (&r, &g)) in c.iter().zip(b.iter()).enumerate() {
                    assert!(
                        g.abs() <= r.abs(),
                        "{context} sample {i}: Gate {g} vs the level gate {r}"
                    );
                }
                assert!(!out_gate.speaking || out_off.speaking, "{context}");
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
    assert_eq!(SpeechDetection::from_wire(1), SpeechDetection::Gate, "the removed ring mode");
    assert_eq!(SpeechDetection::from_wire(2), SpeechDetection::Gate);
    assert_eq!(SpeechDetection::from_wire(3), SpeechDetection::Off);
}

#[test]
fn muting_keeps_the_detector_judging_and_unmuting_mid_speech_rings_with_the_first_frame_after_it() {
    let mut script = vec![(false, 0.0f32); START];
    script.extend(std::iter::repeat_n((true, SPEECH), 60));
    let unmute = START + 40;
    for (ms, lookahead) in [(0.0, 0usize), (40.0, LOOKAHEAD)] {
        let gate = run(SpeechDetection::Gate, &script, |f, s| {
            s.gate_threshold = if f < START { 0.5 } else { 0.01 };
            s.mic_muted = f < unmute;
            s.speech_lookahead_ms = ms;
        });
        assert!(
            (0..unmute + lookahead).all(|f| !gate.speaking[f] && silent(&gate.sent[f])),
            "{ms} ms: nothing captured while muted goes out"
        );
        assert!(gate.speaking[unmute + lookahead], "{ms} ms: confirmed while muted");
        assert!(!silent(&gate.sent[unmute + lookahead]));
    }
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
    assert!(
        lost_delayed(&off, &gate, confirmed + 1..uncertain_end + HANG_FRAMES as usize - LOOKAHEAD)
            .is_empty(),
        "sent through the hang"
    );
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

/// One scripted frame of a fingerprinted stream.
#[derive(Clone, Copy, Debug)]
struct Step {
    rate: u32,
    len: Option<usize>,
    level: f32,
    p: f32,
    detection: SpeechDetection,
    push_to_talk: PushToTalk,
    muted: bool,
    lookahead_ms: f32,
    flush: bool,
}

impl Default for Step {
    fn default() -> Self {
        Self {
            rate: 48_000,
            len: None,
            level: 0.05,
            p: SPEECH,
            detection: SpeechDetection::Gate,
            push_to_talk: PushToTalk::VoiceActivity,
            muted: false,
            lookahead_ms: f32::NAN,
            flush: false,
        }
    }
}

/// Frame `k`: every sample ±level, the signs repeating k + 1 in 16 bits, so whatever gain a sent
/// frame got, its signs say which frame went in.
fn fingerprint(k: usize, level: f32, len: usize) -> Vec<f32> {
    let code = (k + 1) as u32;
    (0..len)
        .map(|i| if (code >> (i % 16)) & 1 == 1 { level * 32_768.0 } else { -level * 32_768.0 })
        .collect()
}

/// The frame a sent frame came from, read from its first 16 nonzero samples on a 16-sample
/// boundary (None: silence).
fn source(sent: &[f32]) -> Option<usize> {
    if silent(sent) {
        return None;
    }
    let run = (0..sent.len() / 16)
        .map(|b| &sent[b * 16..b * 16 + 16])
        .find(|run| run.iter().all(|&v| v != 0.0))
        .unwrap();
    let code = run.iter().enumerate().fold(0usize, |c, (i, &v)| c | (usize::from(v > 0.0) << i));
    Some(code - 1)
}

struct Sent {
    sent: Vec<Vec<f32>>,
    outs: Vec<DspOutputs>,
}

/// Manual threshold 0.01, no voice gain, fingerprinted frames.
fn run_steps(steps: &[Step]) -> Sent {
    let mut chain = DspChain::new(48_000);
    let mut r = Sent { sent: vec![], outs: vec![] };
    let mut out = DspOutputs::default();
    for (k, step) in steps.iter().enumerate() {
        let len = step.len.unwrap_or((step.rate / 100) as usize);
        let mut x = fingerprint(k, step.level, len);
        let settings = DspSettings {
            sample_rate: step.rate,
            auto_gain: false,
            auto_sensitivity: false,
            gate_threshold: 0.01,
            speech_detection: step.detection,
            speech_probability: step.p,
            push_to_talk: step.push_to_talk,
            mic_muted: step.muted,
            speech_lookahead_ms: step.lookahead_ms,
            ..DspSettings::default()
        };
        let commands = DspCommands { flush: step.flush, ..DspCommands::default() };
        chain.process(&mut x, &settings, &commands, &mut out);
        r.sent.push(x);
        r.outs.push(out);
    }
    r
}

#[test]
fn the_look_ahead_follows_its_setting_in_whole_frames() {
    for (ms, frames) in [
        (f32::NAN, 4usize),
        (f32::INFINITY, 4),
        (f32::NEG_INFINITY, 4),
        (-10.0, 0),
        (0.0, 0),
        (4.9, 0),
        (5.0, 1),
        (14.9, 1),
        (34.9, 3),
        (35.0, 4),
        (40.0, 4),
        (45.0, 5),
        (60.0, 6),
        (1e9, 6),
    ] {
        assert_eq!(sanitize::speech_lookahead_frames(ms), frames, "{ms} ms");
        let steps = vec![Step { lookahead_ms: ms, ..Step::default() }; 60];
        let r = run_steps(&steps);
        assert_eq!(r.outs[59].speech_lookahead_ms, frames as u32 * 10, "{ms} ms");
        assert_eq!(source(&r.sent[59]), Some(59 - frames), "{ms} ms: the frame sent");
    }
    assert_eq!(MAX_LOOKAHEAD_FRAMES, 6);
}

#[test]
fn the_look_ahead_runs_only_where_the_speech_gate_decides_the_audio() {
    for (step, gated) in [
        (Step::default(), true),
        (Step { detection: SpeechDetection::Off, ..Step::default() }, false),
        (Step { p: -1.0, ..Step::default() }, false),
        (Step { p: f32::NAN, ..Step::default() }, false),
        (Step { push_to_talk: PushToTalk::Held, ..Step::default() }, false),
    ] {
        let r = run_steps(&vec![step; 60]);
        let expected = if gated { 40 } else { 0 };
        assert_eq!(r.outs[59].speech_lookahead_ms, expected, "{step:?}");
        let lag = if gated { LOOKAHEAD } else { 0 };
        assert_eq!(source(&r.sent[59]), Some(59 - lag), "{step:?}");
    }
}

#[test]
fn the_look_ahead_sends_the_start_of_a_word_the_detector_confirms_within_it() {
    // Room noise under the threshold, then a word whose first `uncertain` frames the detector rates
    // 0.1: it confirms CONFIRM_FRAMES - 1 frames after its first frame at 0.9. The gate decides on
    // the confirming frame and applies it to the audio `lookahead` frames older.
    let onset = START + 20;
    for (ms, lookahead) in [(0.0, 0usize), (40.0, LOOKAHEAD), (60.0, 6)] {
        for uncertain in 0..=5usize {
            let confirm = onset + uncertain + CONFIRM_FRAMES as usize - 1;
            let mut steps =
                vec![Step { level: 0.002, p: 0.0, lookahead_ms: ms, ..Step::default() }; onset];
            steps.extend(vec![Step { p: 0.1, lookahead_ms: ms, ..Step::default() }; uncertain]);
            steps.extend(vec![Step { lookahead_ms: ms, ..Step::default() }; 60]);
            let r = run_steps(&steps);
            let context = format!("{ms} ms, confirmed {} frames into the word", confirm - onset);
            let sources: Vec<usize> = r.sent.iter().filter_map(|s| source(s)).collect();
            let first = sources[0];
            assert_eq!(first, confirm - lookahead, "{context}: the first frame sent");
            assert_eq!(
                (0..r.sent.len()).find(|&f| !silent(&r.sent[f])),
                Some(confirm),
                "{context}: nothing goes out before the confirmation"
            );
            assert!(
                sources.windows(2).all(|w| w[1] == w[0] + 1),
                "{context}: then every frame, in order: {sources:?}"
            );
            if confirm < onset + lookahead {
                let at = onset + lookahead;
                assert_eq!(
                    r.sent[at],
                    fingerprint(onset, 0.05, N),
                    "{context}: the word's first frame at full gain"
                );
            } else {
                assert!(first >= onset, "{context}");
            }
        }
    }
}

#[test]
fn the_startup_hold_holds_what_the_look_ahead_sends_first() {
    let r = run_steps(&vec![Step::default(); 60]);
    let held = STARTUP_HOLD_FRAMES as usize + LOOKAHEAD;
    for f in 0..held {
        assert!(r.outs[f].held && !r.outs[f].speaking && silent(&r.sent[f]), "frame {f}");
    }
    assert!(!r.outs[held].held && r.outs[held].speaking);
    assert_eq!(source(&r.sent[held]), Some(STARTUP_HOLD_FRAMES as usize), "the first live frame");
}

#[test]
fn the_look_ahead_starts_with_silence_ends_by_dropping_and_never_sends_old_audio() {
    let d = Step::default();
    let mut steps = Vec::new();
    let mut add = |count: usize, step: Step| steps.extend(std::iter::repeat_n(step, count));
    add(60, d);
    add(30, Step { detection: SpeechDetection::Off, ..d });
    add(30, d);
    add(30, Step { push_to_talk: PushToTalk::Held, ..d });
    add(30, d);
    add(30, Step { lookahead_ms: 60.0, ..d });
    add(30, Step { lookahead_ms: 20.0, ..d });
    add(30, d);
    add(1, Step { flush: true, ..d });
    add(49, d);
    add(50, Step { rate: 16_000, ..d });
    add(50, d);
    add(1, Step { len: Some(479), ..d });
    add(29, d);
    add(20, Step { muted: true, ..d });
    add(30, d);
    add(5, Step { p: f32::NAN, ..d });
    add(25, d);
    add(50, Step { lookahead_ms: 0.0, ..d });
    let r = run_steps(&steps);
    let mut last = None;
    for (f, sent) in r.sent.iter().enumerate() {
        let Some(k) = source(sent) else { continue };
        assert!(last.is_none_or(|l| k > l), "frame {f} sends frame {k} after frame {last:?}");
        assert!(!steps[k].muted, "frame {f} sends frame {k}, captured while muted");
        assert!(r.outs[f].speaking || steps[f].muted, "frame {f} sends audio without ringing");
        last = Some(k);
    }
    for (f, lag) in [
        (59, 4),
        (89, 0),
        (119, 4),
        (149, 0),
        (179, 4),
        (209, 6),
        (239, 2),
        (269, 4),
        (319, 4),
        (369, 4),
        (419, 4),
        (449, 4),
        (499, 4),
        (504, 0),
        (529, 4),
        (579, 0),
    ] {
        assert_eq!(source(&r.sent[f]), Some(f - lag), "frame {f}");
        assert_eq!(r.outs[f].speech_lookahead_ms, lag as u32 * 10, "frame {f}");
    }
    for (start, pad) in
        [(90usize, 4usize), (150, 4), (180, 6), (210, 2), (240, 4), (421, 4), (505, 4)]
    {
        assert!(
            (start..start + pad).all(|f| silent(&r.sent[f])),
            "silence where it starts at {start}"
        );
        assert_eq!(source(&r.sent[start + pad]), Some(start), "then the frame it started on");
    }
    assert_eq!(source(&r.sent[420]), Some(420), "an unsupported frame passes as it is");
}
