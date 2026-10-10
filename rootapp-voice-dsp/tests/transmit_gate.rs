#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects
)]

use rootapp_voice_dsp::transmit_gate::TransmitGate;
use rootapp_voice_dsp::{DspChain, DspCommands, DspOutputs, DspSettings, PushToTalk};

const RATES: [u32; 8] = [8000, 11025, 16000, 22050, 24000, 32000, 44100, 48000];
/// The startup hold and the two gate blocks a manual threshold of 0 needs to open fully.
const WARM_FRAMES: usize = 22;

struct Harness {
    chain: DspChain,
    settings: DspSettings,
    commands: DspCommands,
    out: DspOutputs,
}

impl Harness {
    /// No voice gain and a manual threshold of 0: anything not silent passes unchanged.
    fn new(rate: u32) -> Self {
        let settings = DspSettings {
            sample_rate: rate,
            auto_gain: false,
            auto_sensitivity: false,
            gate_threshold: 0.0,
            ..DspSettings::default()
        };
        Self {
            chain: DspChain::new(48_000),
            settings,
            commands: DspCommands::default(),
            out: DspOutputs::default(),
        }
    }

    fn warmed(rate: u32, value: f32) -> Self {
        let mut h = Self::new(rate);
        for _ in 0..WARM_FRAMES {
            h.run(value);
        }
        h
    }

    fn frame_len(&self) -> usize {
        (self.settings.sample_rate / 100) as usize
    }

    fn run_input(&mut self, input: &[f32]) -> Vec<f32> {
        let mut frame = input.to_vec();
        self.chain.process(&mut frame, &self.settings, &self.commands, &mut self.out);
        self.commands = DspCommands::default();
        frame
    }

    fn run(&mut self, value: f32) -> Vec<f32> {
        let input = vec![value; self.frame_len()];
        self.run_input(&input)
    }
}

fn fade(rate: u32) -> usize {
    (rate * 5 / 1000) as usize
}

fn silent(samples: &[f32]) -> bool {
    samples.iter().all(|&v| v == 0.0)
}

#[test]
fn transmit_gate_unit_fades_and_zero_fills() {
    let mut gate = TransmitGate::new(4, true);
    let mut buffer = [1.0f32; 6];
    assert!(gate.apply(&mut buffer, false));
    assert_eq!(buffer, [0.75, 0.5, 0.25, 0.0, 0.0, 0.0]);
    let mut buffer = [1.0f32; 3];
    assert!(!gate.apply(&mut buffer, false));
    assert_eq!(buffer, [0.0; 3]);
    let mut buffer = [1.0f32; 5];
    assert!(gate.apply(&mut buffer, true));
    assert_eq!(buffer, [0.25, 0.5, 0.75, 1.0, 1.0]);
    let mut buffer = [0.5f32; 2];
    assert!(gate.apply(&mut buffer, true));
    assert_eq!(buffer, [0.5; 2]);
    assert!(!gate.apply(&mut [], true));
}

#[test]
fn transmit_gate_unit_hard_lands_on_the_target() {
    let mut gate = TransmitGate::new(4, true);
    let mut buffer = [1.0f32; 2];
    assert!(!gate.apply_hard(&mut buffer, false));
    assert_eq!(buffer, [0.0; 2]);
    assert_eq!(gate.gain(), 0.0);
    let mut buffer = [1.0f32; 2];
    assert!(!gate.apply(&mut buffer, false), "already closed: no fade-out burst");
    assert!(gate.apply_hard(&mut buffer, true));
    assert_eq!(gate.gain(), 1.0);
}

#[test]
fn chain_passes_audio_through_when_open() {
    for rate in RATES {
        let mut h = Harness::warmed(rate, 1000.0);
        assert_eq!(h.run(1000.0), vec![1000.0; h.frame_len()], "{rate}");
        assert!(h.out.transmit_open && h.out.transmitting);
    }
}

#[test]
fn chain_mute_fades_over_5_ms_each_way() {
    for rate in RATES {
        let mut h = Harness::warmed(rate, 1000.0);
        let f = fade(rate);
        h.settings.mic_muted = true;
        let closing = h.run(1000.0);
        assert!(closing.windows(2).all(|w| w[1] <= w[0]), "{rate}");
        assert!(closing[0] > 950.0 && closing[0] < 1000.0, "{rate}: {}", closing[0]);
        assert!(closing[f - 2] > 0.0, "{rate}");
        assert!(silent(&closing[f..]), "{rate}");
        assert!(h.out.transmitting && !h.out.transmit_open);
        assert!(silent(&h.run(1000.0)), "{rate}");
        assert!(!h.out.transmitting);

        h.settings.mic_muted = false;
        let opening = h.run(1000.0);
        assert!(opening.windows(2).all(|w| w[1] >= w[0]), "{rate}");
        assert!(opening[0] > 0.0 && opening[0] < 50.0, "{rate}: {}", opening[0]);
        assert!(opening[f + 1..].iter().all(|&v| v == 1000.0), "{rate}");
        assert!((opening[f] - 1000.0).abs() < 0.01, "{rate}");
        assert_eq!(h.run(1000.0), vec![1000.0; h.frame_len()], "{rate}");
    }
}

#[test]
fn chain_starts_closed_when_muted_from_the_first_frame() {
    let mut h = Harness::new(48000);
    h.settings.mic_muted = true;
    for _ in 0..WARM_FRAMES + 5 {
        assert!(silent(&h.run(1000.0)));
        assert!(!h.out.transmitting);
    }
    h.settings.mic_muted = false;
    h.run(1000.0);
    assert_eq!(h.run(1000.0), vec![1000.0; 480]);
}

#[test]
fn chain_push_to_talk_gates_on_the_key_and_fades_the_release() {
    let mut h = Harness::new(48000);
    h.settings.push_to_talk = PushToTalk::Released;
    for _ in 0..WARM_FRAMES + 5 {
        assert!(silent(&h.run(1000.0)));
    }
    h.settings.push_to_talk = PushToTalk::Held;
    let pressed = h.run(1000.0);
    assert!(pressed[0] > 0.0 && pressed[0] < 10.0, "{}", pressed[0]);
    assert!(pressed[fade(48000)..].iter().all(|&v| v == 1000.0));

    h.settings.push_to_talk = PushToTalk::Released;
    let released = h.run(1000.0);
    assert!(released[0] > 990.0, "the key-up fades instead of clicking: {}", released[0]);
    assert!(released.windows(2).all(|w| w[1] <= w[0]));
    assert!(silent(&released[fade(48000)..]));
    assert!(!h.out.speaking && !h.out.transmit_open);
    assert!(silent(&h.run(1000.0)));

    h.settings.push_to_talk = PushToTalk::from_wire(9);
    assert!(silent(&h.run(1000.0)), "unknown values close");
    h.settings.push_to_talk = PushToTalk::VoiceActivity;
    h.run(1000.0);
    h.run(1000.0);
    assert_eq!(h.run(1000.0), vec![1000.0; 480]);
}

#[test]
fn chain_ramps_the_input_volume_over_20_ms_at_any_rate() {
    for rate in RATES {
        let mut h = Harness::warmed(rate, 1.0);
        let n = h.frame_len();
        h.settings.input_volume = 0.0;
        let first = h.run(1.0);
        let second = h.run(1.0);
        let ramp: Vec<f32> = first.iter().chain(second.iter()).copied().collect();
        assert!(ramp.windows(2).all(|w| w[1] <= w[0]), "{rate}");
        assert!((first[n - 1] - 0.5).abs() < 1e-2, "{rate}: {}", first[n - 1]);
        let ramp_samples = rate / 50;
        assert!(second[n - 1] <= 1.0 / ramp_samples as f32, "{rate}: {}", second[n - 1]);
        assert!(silent(&h.run(1.0)), "{rate}");

        h.settings.input_volume = 2.0;
        h.run(1.0);
        h.run(1.0);
        assert_eq!(h.run(1.0), vec![2.0; n], "{rate}");
    }
}

#[test]
fn chain_sanitises_the_input_volume() {
    let mut h = Harness::warmed(16000, 1.0);
    h.settings.input_volume = f32::NAN;
    h.run(1.0);
    h.run(1.0);
    assert!(silent(&h.run(1.0)));
    h.settings.input_volume = 9.0;
    h.run(1.0);
    h.run(1.0);
    assert_eq!(h.run(1.0), vec![2.0; 160]);
    h.settings.input_volume = f32::NEG_INFINITY;
    h.run(1.0);
    h.run(1.0);
    assert!(silent(&h.run(1.0)));
}

#[test]
fn chain_clamps_what_leaves_to_full_scale() {
    let mut h = Harness::warmed(48000, 1000.0);
    h.settings.input_volume = 2.0;
    h.run(30000.0);
    h.run(30000.0);
    assert_eq!(h.run(30000.0), vec![32768.0; 480]);
    assert_eq!(h.run(-30000.0), vec![-32768.0; 480]);
}

#[test]
fn chain_snaps_the_gates_to_the_settings_on_a_reset() {
    let mut h = Harness::new(48000);
    h.settings.mic_muted = true;
    h.settings.push_to_talk = PushToTalk::Released;
    h.settings.input_volume = 0.5;
    for _ in 0..WARM_FRAMES {
        h.run(1.0);
    }
    h.settings = DspSettings {
        sample_rate: 48000,
        auto_gain: false,
        auto_sensitivity: false,
        gate_threshold: 0.0,
        ..DspSettings::default()
    };
    h.commands.reset = true;
    for _ in 0..WARM_FRAMES {
        h.run(1.0);
    }
    assert_eq!(h.run(1.0), vec![1.0; 480]);
}

#[test]
fn chain_gates_unsupported_shapes_without_a_fade() {
    let mut h = Harness::new(96000);
    let input: Vec<f32> = (0..960).map(|i| (i % 7) as f32 * 100.0).collect();
    for _ in 0..3 {
        assert_eq!(h.run_input(&input), input, "no startup hold, no gate");
        assert!(h.out.unsupported && h.out.transmitting && h.out.transmit_open);
        assert!(!h.out.held && !h.out.gate_open && !h.out.denoiser_ran);
    }
    h.settings.mic_muted = true;
    assert!(silent(&h.run_input(&input)));
    assert!(!h.out.transmitting && !h.out.speaking);
    h.settings.mic_muted = false;
    assert_eq!(h.run_input(&input), input);

    let mut wrong = Harness::warmed(48000, 1000.0);
    wrong.settings.mic_muted = true;
    assert!(silent(&wrong.run_input(&[1000.0; 479])));
    assert!(silent(&wrong.run(1000.0)), "closed hard: no fade-out burst after it");
    wrong.settings.mic_muted = false;
    let loud = [1e30f32, -1e30, f32::NAN, f32::INFINITY, 5.0e4, -40000.0];
    let out = wrong.run_input(&loud);
    assert_eq!(out, vec![32768.0, -32768.0, 0.0, 0.0, 32768.0, -32768.0]);
}
