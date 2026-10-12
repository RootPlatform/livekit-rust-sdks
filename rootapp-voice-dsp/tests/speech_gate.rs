#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects
)]

mod support;

use rootapp_voice_dsp::speech_gate::{
    probability, GateControl, SpeechGate, CONFIRM_FRAMES, HANG_FRAMES, PROVISIONAL_FRAMES,
    REARM_FRAMES, SPEECH_OFF, SPEECH_ON,
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

/// FloatS16 frame: a 0.1 tone ("speech") or 0.03 uniform noise (RMS 0.017, above the threshold).
fn frame(loud: bool, f: usize, rng: &mut Rng) -> Vec<f32> {
    (0..N)
        .map(|i| {
            let v = if loud { sine(0.1, 220.0, f * N + i, 48_000) } else { 0.03 * rng.symmetric() };
            v * 32_768.0
        })
        .collect()
}

struct Run {
    sent: Vec<Vec<f32>>,
    speaking: Vec<bool>,
}

/// `script[f]`: (loud, probability).
fn run(
    detection: SpeechDetection,
    script: &[(bool, f32)],
    tweak: impl Fn(usize, &mut DspSettings),
) -> Run {
    let (mut chain, mut settings) = chain(detection);
    let mut out = DspOutputs::default();
    let commands = DspCommands::default();
    let mut rng = Rng::new(7);
    let mut r = Run { sent: vec![], speaking: vec![] };
    for (f, &(loud, p)) in script.iter().enumerate() {
        let mut x = frame(loud, f, &mut rng);
        settings.speech_probability = p;
        tweak(f, &mut settings);
        chain.process(&mut x, &settings, &commands, &mut out);
        r.sent.push(x);
        r.speaking.push(out.speaking);
    }
    r
}

const START: usize = STARTUP_HOLD_FRAMES as usize + 5;

/// Speech for 100 frames, then 400 frames of noise above the threshold that the detector rejects.
fn speech_then_noise() -> Vec<(bool, f32)> {
    let mut s = vec![(false, NOISE); START];
    s.extend(std::iter::repeat_n((true, SPEECH), 100));
    s.extend(std::iter::repeat_n((false, NOISE), 400));
    s
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
    assert!(g.observe(1.0, false, true), "confirmed speech is no floor to learn");
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

#[test]
fn speech_gate_hang_out_with_the_level_open_forces_a_close_in_gate_mode() {
    let mut g = SpeechGate::default();
    for _ in 0..CONFIRM_FRAMES {
        g.observe(0.9, true, true);
    }
    for _ in 0..=HANG_FRAMES {
        g.observe(0.0, true, true);
    }
    assert!(!g.confirmed() && g.detector_required());
    let control = g.control(0.0, true, true);
    assert!(control.fast_close && !control.allow_open);
    g.after_gate(0.0);
    assert_eq!(g.control(0.0, true, false), GateControl { allow_open: false, fast_close: false });

    let mut closed = SpeechGate::default();
    for _ in 0..CONFIRM_FRAMES {
        closed.observe(0.9, false, true);
    }
    for _ in 0..=HANG_FRAMES {
        closed.observe(0.0, false, true);
    }
    assert!(!closed.detector_required(), "a hang that ends with the gate shut needs nothing");
}

#[test]
fn speech_gate_releases_an_open_it_never_confirmed_after_the_provisional_window() {
    let mut g = SpeechGate::default();
    for f in 1..=PROVISIONAL_FRAMES {
        assert!(g.observe(0.1, true, true), "an open not judged yet does not teach the floor");
        assert_eq!(g.control(0.1, true, true), GateControl::LEVEL, "frame {f}");
    }
    assert!(g.observe(0.1, true, true));
    let control = g.control(0.1, true, true);
    assert!(control.fast_close && !control.allow_open && g.detector_required());
    assert!(!g.observe(0.1, true, true), "rejected noise teaches the floor");
    assert!(!g.observe(0.1, false, true));
}

#[test]
fn speech_gate_lets_speech_or_a_quiet_spell_open_again() {
    let rejected = || {
        let mut g = SpeechGate::default();
        for _ in 0..=PROVISIONAL_FRAMES {
            g.observe(0.0, true, true);
            g.control(0.0, true, true);
        }
        g.after_gate(0.0);
        assert!(g.detector_required());
        g
    };

    let mut g = rejected();
    assert!(g.control(SPEECH_ON, true, false).allow_open, "a speech frame may open it");
    for _ in 0..CONFIRM_FRAMES {
        g.observe(0.9, false, true);
    }
    assert!(g.confirmed() && !g.detector_required());
    assert_eq!(g.control(0.9, true, true), GateControl::LEVEL);

    let mut g = rejected();
    for _ in 0..REARM_FRAMES - 1 {
        assert!(!g.control(0.0, false, false).allow_open);
    }
    assert!(g.control(0.0, false, false).allow_open, "REARM_FRAMES below the threshold");
    assert!(!g.detector_required());
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

fn db(rms: f32) -> f32 {
    20.0 * rms.log10()
}

#[test]
fn auto_sensitivity_learns_no_floor_from_confirmed_speech() {
    // Floor about -66 dBFS (threshold -58), a quiet talker at -59.5 dBFS under it.
    let (noise, tone) = (10f32.powf(-66.0 / 20.0), 10f32.powf(-59.5 / 20.0));
    let off = thresholds(SpeechDetection::Off, noise, tone, SPEECH);
    let gate = thresholds(SpeechDetection::Gate, noise, tone, SPEECH);
    let climb = |t: &[f32]| db(t[149]) - db(t[10]);
    assert!(climb(&off) > 1.0, "the level gate learns the talker: {}", climb(&off));
    assert!(climb(&gate).abs() < 0.05, "confirmed speech teaches nothing: {}", climb(&gate));
}

#[test]
fn auto_sensitivity_learns_the_noise_the_detector_rejected() {
    // Floor about -66 dBFS, then steady noise-like input at -50 dBFS the detector rejects.
    let (noise, loud) = (10f32.powf(-66.0 / 20.0), 10f32.powf(-50.0 / 20.0));
    let off = thresholds(SpeechDetection::Off, noise, loud, 0.0);
    let gate = thresholds(SpeechDetection::Gate, noise, loud, 0.0);
    let climb = |t: &[f32]| db(t[199]) - db(t[PROVISIONAL_FRAMES as usize + 5]);
    assert!(climb(&gate) > 1.5, "the floor follows rejected noise: {}", climb(&gate));
    assert!(climb(&off) < 0.5, "held open, the level gate waits for its window: {}", climb(&off));
}
