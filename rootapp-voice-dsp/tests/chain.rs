#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects,
    clippy::manual_is_multiple_of
)]

mod support;

use rootapp_voice_dsp::auto_sensitivity::AutoSensitivityOptions;
use rootapp_voice_dsp::{
    Agc2Tuning, DenoiserOutput, DspChain, DspCommands, DspOutputs, DspSettings, GainEngine,
    GainLimit, PreDenoise, PushToTalk, SpeechDetection, ThresholdTerm, AUTO_MIN_OPEN_BLOCKS,
    DENOISER_STALL_FRAMES, MAX_DENOISER_WAIT_FRAMES, STARTUP_HOLD_FRAMES,
};
use support::{in_range, near, silent, sine, Rng};

const HOLD: usize = STARTUP_HOLD_FRAMES as usize;

struct Harness {
    chain: DspChain,
    settings: DspSettings,
    commands: DspCommands,
    out: DspOutputs,
    position: usize,
}

impl Harness {
    fn new(rate: u32) -> Self {
        Self {
            chain: DspChain::new(48_000),
            settings: DspSettings { sample_rate: rate, ..DspSettings::default() },
            commands: DspCommands::default(),
            out: DspOutputs::default(),
            position: 0,
        }
    }

    /// No voice gain and a manual threshold of 0.
    fn pass_through(rate: u32) -> Self {
        let mut h = Self::new(rate);
        h.settings.auto_gain = false;
        h.settings.auto_sensitivity = false;
        h.settings.gate_threshold = 0.0;
        h
    }

    fn n(&self) -> usize {
        (self.settings.sample_rate / 100) as usize
    }

    fn pre(&mut self, frame: &mut [f32]) -> PreDenoise {
        let pre = self.chain.pre_denoise(frame, &self.settings, &self.commands, &mut self.out);
        self.commands = DspCommands::default();
        pre
    }

    fn run_with(&mut self, input: &[f32], denoised: DenoiserOutput<'_>) -> Vec<f32> {
        let mut frame = input.to_vec();
        self.pre(&mut frame);
        self.chain.post_denoise(&mut frame, denoised, &self.settings, &mut self.out);
        self.position += input.len();
        frame
    }

    fn run_input(&mut self, input: &[f32]) -> Vec<f32> {
        self.run_with(input, DenoiserOutput::NotInUse)
    }

    fn run(&mut self, value: f32) -> Vec<f32> {
        let input = vec![value; self.n()];
        self.run_input(&input)
    }

    /// A continuous sine in FloatS16.
    fn tone(&self, amplitude: f32, hz: f64) -> Vec<f32> {
        let rate = self.settings.sample_rate;
        (0..self.n()).map(|i| sine(amplitude, hz, self.position + i, rate)).collect()
    }

    fn run_tone(&mut self, amplitude: f32) -> Vec<f32> {
        let input = self.tone(amplitude, 440.0);
        self.run_input(&input)
    }
}

/// Managed TheVoiceActivityThresholdDecidesWhatIsSent: frames leave only while the gate input is
/// above the threshold.
#[test]
fn the_voice_activity_threshold_decides_what_is_sent() {
    for (threshold, expect_audio) in [(0.001f32, true), (0.5, false)] {
        let mut h = Harness::new(48000);
        h.settings.auto_sensitivity = false;
        h.settings.gate_threshold = threshold;
        let mut sent = Vec::new();
        for _ in 0..40 {
            sent.push(h.run_tone(0.1 * 32768.0));
        }
        assert!(in_range(f64::from(h.out.gate_in_rms), 0.06, 0.08), "{}", h.out.gate_in_rms);
        let audio = sent.iter().skip(HOLD + 5).any(|f| !silent(f));
        assert_eq!(audio, expect_audio, "{threshold}");
        assert_eq!(h.out.gate_speaking, expect_audio);
        assert_eq!(h.out.speaking, expect_audio);
        assert_eq!(h.out.gate_input_above, expect_audio);
    }
}

/// Managed AStreamsFirstFramesAreNeverSentOrCountedAsSpeech.
#[test]
fn a_streams_first_frames_are_never_sent_or_counted_as_speech() {
    let mut h = Harness::new(48000);
    h.settings.auto_sensitivity = false;
    h.settings.gate_threshold = 0.001;
    h.commands.gain_reset = true;
    h.commands.gain_reset_db = 10.0;
    let mut first_speaking = None;
    let mut sent = Vec::new();
    for i in 0..40 {
        sent.push(h.run_tone(0.05 * 32768.0));
        assert_eq!(h.out.held, i < HOLD, "{i}");
        if h.out.speaking && first_speaking.is_none() {
            first_speaking = Some(i);
        }
    }
    assert!(sent[..HOLD].iter().all(|f| silent(f)));
    assert!(sent[HOLD..].iter().any(|f| !silent(f)));
    assert!(first_speaking.unwrap() >= HOLD);
    assert_eq!(h.out.voice_gain_db, 10.0);
}

/// Managed GainControlFollowsTheSettingAndStartsFromTheRememberedGain.
#[test]
fn gain_control_follows_the_setting_and_starts_from_the_remembered_gain() {
    let mut h = Harness::new(48000);
    h.commands.gain_reset = true;
    h.commands.gain_reset_db = 12.0;
    h.run(0.0);
    assert_eq!(h.out.voice_gain_db, 12.0);
    assert!(!h.out.voice_gain_settled);

    h.run(0.0);
    assert_eq!(h.out.voice_gain_db, 12.0);

    h.commands.gain_reset = true;
    h.commands.gain_reset_db = 4.0;
    h.run(0.0);
    assert_eq!(h.out.voice_gain_db, 4.0);

    h.settings.auto_gain = false;
    let input = h.tone(1000.0, 300.0);
    let mut frame = input.clone();
    h.pre(&mut frame);
    h.chain.post_denoise(&mut frame, DenoiserOutput::NotInUse, &h.settings, &mut h.out);
    assert_eq!(h.out.voice_gain_db, 4.0, "off keeps the last value");
    assert!(!h.out.voice_gain_settled);

    h.commands.gain_reset = true;
    h.commands.gain_reset_db = 30.0;
    h.run(0.0);
    assert_eq!(h.out.voice_gain_db, 4.0, "a reset while off only sets the start");
    h.settings.auto_gain = true;
    h.run(0.0);
    assert_eq!(h.out.voice_gain_db, 24.0, "back on: from the newest reset, clamped to the maximum");

    for db in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        h.commands.gain_reset = true;
        h.commands.gain_reset_db = db;
        h.run(0.0);
        assert_eq!(h.out.voice_gain_db, 24.0, "{db} is ignored");
    }
    h.commands.gain_reset = true;
    h.commands.gain_reset_db = -3.0;
    h.run(0.0);
    assert_eq!(h.out.voice_gain_db, 0.0);
}

/// Managed AutoSensitivityGatesRoomNoiseButSendsSpeech.
#[test]
fn auto_sensitivity_gates_room_noise_but_sends_speech() {
    for noise_db in [-62.0f64, -48.0, -40.0] {
        let mut h = Harness::new(48000);
        h.settings.gate_threshold = 0.5;
        let mut rng = Rng::new(1);
        let amplitude = (10f64.powf(noise_db / 20.0) * 3f64.sqrt()) as f32 * 32768.0;
        let mut noise =
            |n: usize| -> Vec<f32> { (0..n).map(|_| amplitude * rng.symmetric()).collect() };
        for _ in 0..600 {
            let input = noise(480);
            h.run_input(&input);
        }
        for _ in 0..200 {
            let input = noise(480);
            assert!(silent(&h.run_input(&input)), "{noise_db}");
        }
        assert!(!h.out.speaking, "{noise_db}");

        let mut speech = Vec::new();
        for _ in 0..30 {
            let tone = h.tone(0.1 * 32768.0, 440.0);
            let input: Vec<f32> = noise(480).iter().zip(tone.iter()).map(|(a, b)| a + b).collect();
            speech.push(h.run_input(&input));
        }
        assert!(speech.iter().skip(5).any(|f| !silent(f)), "{noise_db}");
        assert!(h.out.speaking, "{noise_db}");
    }
}

/// Managed AVoiceGainJumpDoesNotOpenTheAutoGateOnRoomNoise: the floor is learned before the voice
/// gain and the threshold moves with the gain the same frame.
#[test]
fn a_voice_gain_jump_does_not_open_the_auto_gate_on_room_noise() {
    let mut h = Harness::new(48000);
    let mut rng = Rng::new(2);
    let amplitude = (10f64.powf(-70.0 / 20.0) * 3f64.sqrt()) as f32 * 32768.0;
    let mut noise =
        |n: usize| -> Vec<f32> { (0..n).map(|_| amplitude * rng.symmetric()).collect() };
    for _ in 0..600 {
        let input = noise(480);
        h.run_input(&input);
    }
    assert_eq!(h.out.voice_gain_db, 0.0);
    h.commands.gain_reset = true;
    h.commands.gain_reset_db = 12.0;
    for _ in 0..300 {
        let input = noise(480);
        assert!(silent(&h.run_input(&input)));
    }
    assert_eq!(h.out.voice_gain_db, 12.0);
    assert!(!h.out.speaking && !h.out.gate_open);
    let options = AutoSensitivityOptions::default();
    let threshold_db = 20.0 * f64::from(h.out.threshold).log10();
    let minimum_db = options.open_minimum_db + options.voice_gain_cap_db.min(12.0);
    assert!(
        threshold_db >= minimum_db
            && threshold_db <= (-70.0 + options.open_margin_db + 12.0).max(minimum_db) + 0.5,
        "{threshold_db}"
    );
}

fn threshold_db(h: &Harness) -> f64 {
    20.0 * f64::from(h.out.threshold).log10()
}

fn run_noise(h: &mut Harness, rng: &mut Rng, level_db: f64, frames: usize) {
    let amplitude = (10f64.powf(level_db / 20.0) * 3f64.sqrt()) as f32 * 32768.0;
    for _ in 0..frames {
        let input: Vec<f32> = (0..h.n()).map(|_| amplitude * rng.symmetric()).collect();
        h.run_input(&input);
    }
}

/// A hardware mute: 30 s of digital silence (capture_silent) and room noise at -50 dBFS after it.
/// The silence leaves the threshold where the room put it, and the unmute relearns at the startup
/// rate: the threshold is back at the noise + 8 dB within 3 s (about 40 s when the silence was
/// learned as a level).
#[test]
fn a_hardware_mute_neither_drops_the_threshold_nor_slows_the_unmute() {
    for flagged in [true, false] {
        let mut h = Harness::new(48000);
        h.settings.auto_gain = false;
        let mut rng = Rng::new(7);
        run_noise(&mut h, &mut rng, -60.0, 500);
        let before = threshold_db(&h);
        assert!(in_range(before, -53.0, -51.0), "{before}");
        h.settings.capture_silent = flagged;
        for _ in 0..3000 {
            h.run(0.0);
        }
        let muted = threshold_db(&h);
        h.settings.capture_silent = false;
        let mut seconds = f64::INFINITY;
        for frame in 1..=6000u32 {
            run_noise(&mut h, &mut rng, -50.0, 1);
            if !h.out.gate_open && (threshold_db(&h) - -42.0).abs() <= 1.0 {
                seconds = f64::from(frame) / 100.0;
                break;
            }
        }
        if flagged {
            assert_eq!(muted, before, "the silence taught nothing");
            assert!(seconds <= 3.0, "{seconds} s");
        } else {
            assert!(in_range(muted, -54.01, -53.99), "the minimum: {muted}");
            assert!(seconds > 30.0, "{seconds} s");
        }
    }
}

/// Mute in the app closes the transmit gate after the chain: the mic still delivers the room (the
/// raw capture is live), and auto sensitivity follows it exactly as unmuted.
#[test]
fn a_software_mute_leaves_auto_sensitivity_as_it_is() {
    let mut muted = Harness::new(48000);
    let mut open = Harness::new(48000);
    muted.settings.mic_muted = true;
    let (mut a, mut b) = (Rng::new(8), Rng::new(8));
    for (level_db, frames) in [(-50.0, 300), (-65.0, 500), (-45.0, 300)] {
        for _ in 0..frames {
            run_noise(&mut muted, &mut a, level_db, 1);
            run_noise(&mut open, &mut b, level_db, 1);
            assert_eq!(muted.out.threshold, open.out.threshold);
            assert!(!muted.out.transmitting);
        }
        if level_db == -50.0 {
            assert!(in_range(threshold_db(&muted), -43.0, -41.0), "{}", threshold_db(&muted));
        }
    }
}

#[test]
fn the_auto_sensitivity_minimum_follows_the_setting_within_its_range() {
    for (setting, expected) in [
        (f32::NAN, -54.0),
        (-54.0, -54.0),
        (-40.0, -40.0),
        (-62.5, -62.5),
        (-100.0, -80.0),
        (0.0, -30.0),
        (f32::INFINITY, -54.0),
    ] {
        let mut h = Harness::new(48000);
        h.settings.auto_gain = false;
        h.settings.auto_sensitivity_minimum_db = setting;
        for _ in 0..500 {
            h.run(0.0);
        }
        assert!(near(threshold_db(&h), expected, 1e-3), "{setting}: {}", threshold_db(&h));
    }
}

#[test]
fn auto_sensitivity_starts_over_when_switched_back_on() {
    let mut h = Harness::new(48000);
    for _ in 0..300 {
        h.run(0.01 * 32768.0);
    }
    let learned = h.out.threshold;
    h.settings.auto_sensitivity = false;
    h.settings.gate_threshold = 0.3;
    h.run(0.0);
    assert_eq!(h.out.threshold, 0.3);
    h.settings.auto_sensitivity = true;
    h.run(0.0);
    assert!(h.out.threshold < learned, "{} vs {learned}", h.out.threshold);
    assert_eq!(h.out.envelope, 0.0, "the steady level was learned and the gate closed");
    let silence_db = 20.0 * f64::from(1e-7f32).log10();
    let floor = -60.0 + (silence_db + 60.0) * (1.0 - (-0.01f64).exp());
    let fresh = (10f64.powf((floor + 8.0) / 20.0)) as f32;
    assert!((h.out.threshold - fresh).abs() < 1e-5 * fresh, "a fresh floor: {}", h.out.threshold);
}

#[test]
fn speaking_is_gate_speaking_while_the_transmit_gate_is_open() {
    let mut h = Harness::new(48000);
    for _ in 0..40 {
        h.run_tone(3000.0);
    }
    assert!(h.out.speaking && h.out.gate_speaking && h.out.transmit_open);
    h.settings.mic_muted = true;
    h.run_tone(3000.0);
    assert!(!h.out.speaking && h.out.gate_speaking, "mute stops speaking the same frame");
    for _ in 0..20 {
        assert!(silent(&h.run_tone(3000.0)));
    }
    assert!(h.out.gate_speaking && h.out.gate_open, "the gate keeps running while muted");
    h.settings.mic_muted = false;
    h.run_tone(3000.0);
    assert!(h.out.speaking, "unmuting while talking speaks at once");
}

#[test]
fn push_to_talk_release_stops_speaking_at_once_and_fades_the_audio() {
    let mut h = Harness::pass_through(48000);
    h.settings.gate_threshold = 0.01;
    h.settings.push_to_talk = PushToTalk::Held;
    for _ in 0..30 {
        h.run(1000.0);
    }
    assert!(h.out.speaking);
    h.settings.push_to_talk = PushToTalk::Released;
    let released = h.run(1000.0);
    assert!(!h.out.speaking && !h.out.gate_speaking && h.out.envelope == 0.0);
    assert!(released[0] > 990.0 && silent(&released[240..]));
}

#[test]
fn push_to_talk_press_in_auto_sensitivity_opens_on_the_fifth_block() {
    let mut h = Harness::pass_through(48000);
    h.settings.auto_sensitivity = true;
    h.settings.push_to_talk = PushToTalk::Released;
    for _ in 0..30 {
        h.run(1000.0);
    }
    h.settings.push_to_talk = PushToTalk::Held;
    assert!(silent(&h.run(1000.0)), "four blocks are a click");
    let second = h.run(1000.0);
    assert!((second[0] - 1000.0 * 2.0 / 3.0).abs() < 1.0, "{}", second[0]);
    assert!(second[128..].iter().all(|&v| v == 1000.0));
    assert!(h.out.speaking);
    assert_eq!(AUTO_MIN_OPEN_BLOCKS, 5);
}

#[test]
fn a_flush_restarts_the_startup_hold_and_keeps_the_voice_gain() {
    let mut h = Harness::pass_through(48000);
    h.settings.auto_gain = true;
    h.commands.gain_reset = true;
    h.commands.gain_reset_db = 9.0;
    for _ in 0..30 {
        h.run(100.0);
    }
    h.commands.flush = true;
    let mut frame = vec![100.0; 480];
    let pre = h.pre(&mut frame);
    assert!(pre.flush_denoiser && pre.supported);
    assert!(h.out.flushed && !h.out.reset);
    h.chain.post_denoise(&mut frame, DenoiserOutput::NotInUse, &h.settings, &mut h.out);
    assert!(h.out.held && silent(&frame));
    for _ in 1..HOLD {
        assert!(silent(&h.run(100.0)));
    }
    h.run(100.0);
    assert!(!h.out.held);
    assert_eq!(h.out.voice_gain_db, 9.0, "the voice gain is per session");
}

#[test]
fn a_reset_starts_the_session_over() {
    let mut h = Harness::new(48000);
    h.commands.gain_reset = true;
    h.commands.gain_reset_db = 12.0;
    for _ in 0..30 {
        h.run_tone(3000.0);
    }
    assert!(h.out.gate_speaking);
    h.commands.reset = true;
    let tone = h.tone(3000.0, 440.0);
    let mut frame = tone.clone();
    let pre = h.pre(&mut frame);
    assert!(pre.flush_denoiser && h.out.reset && h.out.flushed);
    h.chain.post_denoise(&mut frame, DenoiserOutput::NotInUse, &h.settings, &mut h.out);
    assert!(h.out.held && !h.out.gate_speaking && h.out.envelope == 0.0);
    assert_eq!(h.out.voice_gain_db, 0.0, "the voice gain restarts from 0 dB");

    h.settings.auto_gain = false;
    h.commands.gain_reset = true;
    h.commands.gain_reset_db = 6.0;
    h.run(0.0);
    assert_eq!(h.out.voice_gain_db, 0.0);
    h.settings.auto_gain = true;
    h.run(0.0);
    assert_eq!(h.out.voice_gain_db, 6.0);
    h.settings.auto_gain = false;
    h.run(0.0);
    h.commands.reset = true;
    h.run(0.0);
    assert_eq!(h.out.voice_gain_db, 0.0, "reset with auto gain off reports 0 dB too");
    h.settings.auto_gain = true;
    h.run(0.0);
    assert_eq!(h.out.voice_gain_db, 0.0, "and starts from 0 dB, not the earlier reset");
}

#[test]
fn a_gain_reset_with_a_reset_in_the_same_frame_survives_it() {
    let mut h = Harness::new(48000);
    h.run(0.0);
    h.commands.reset = true;
    h.commands.gain_reset = true;
    h.commands.gain_reset_db = 7.0;
    h.run(0.0);
    assert_eq!(h.out.voice_gain_db, 7.0);
}

/// The host's maximum applies in the frame it arrives in, before that frame's gain reset: a reset
/// clamps to it (and never above 24 dB), unset or non-finite is 24 dB.
#[test]
fn a_gain_reset_is_clamped_to_the_hosts_maximum_from_the_same_frame() {
    for (max, reset, expected) in [
        (f32::NAN, 30.0f32, 24.0f32),
        (18.0, 30.0, 18.0),
        (40.0, 30.0, 24.0),
        (6.0, 10.0, 6.0),
        (-5.0, 10.0, 0.0),
        (f32::INFINITY, 10.0, 10.0),
    ] {
        let mut h = Harness::new(48000);
        h.run(0.0);
        h.settings.voice_gain.max_gain_db = max;
        h.commands.gain_reset = true;
        h.commands.gain_reset_db = reset;
        h.run(0.0);
        assert_eq!(h.out.voice_gain_db, expected, "max {max}, reset {reset}");
    }
}

/// 1 s phrases of syllables of a 440 Hz tone at `level_db` RMS (200 ms on, 100 ms off) with half a
/// second between phrases (where the room is measured) and `probability` on every frame; returns
/// the voice gain after `seconds`.
fn talk_with(h: &mut Harness, level_db: f64, seconds: f64, probability: f32) -> f32 {
    let amplitude = (10f64.powf(level_db / 20.0) * 2f64.sqrt()) as f32 * 32768.0;
    for f in 0..(seconds * 100.0) as usize {
        h.settings.speech_probability = probability;
        if f % 150 < 100 && f % 30 < 20 {
            h.run_tone(amplitude);
        } else {
            h.run(0.0);
        }
    }
    h.out.voice_gain_db
}

/// The voice gain takes the speech probability whenever there is one, whatever the speech
/// detection mode: frames under its minimum are not speech, so a sound the detector rejects never
/// sets the level, and without a probability the level alone decides.
#[test]
fn the_voice_gain_learns_speech_only_from_frames_the_detector_accepts() {
    for detection in [SpeechDetection::Off, SpeechDetection::Ring, SpeechDetection::Gate] {
        for (probability, boosted) in [
            (0.1f32, false),
            (0.49, false),
            (0.5, true),
            (0.9, true),
            (-1.0, true),
            (f32::NAN, true),
        ] {
            let mut h = Harness::new(48000);
            h.settings.speech_detection = detection;
            let gain = talk_with(&mut h, -42.0, 10.0, probability);
            assert_eq!(gain > 11.0, boosted, "{detection:?} p {probability}: {gain}");
            if !boosted {
                assert_eq!(gain, 0.0, "{detection:?} p {probability}");
            }
        }
    }
}

/// The host's minimum probability and target apply: a minimum of 0 takes every frame with a
/// probability, a higher target asks for more gain.
#[test]
fn the_hosts_minimum_probability_and_target_apply() {
    let mut h = Harness::new(48000);
    h.settings.voice_gain.min_speech_probability = 0.0;
    assert!(talk_with(&mut h, -42.0, 10.0, 0.1) > 11.0);

    let mut quiet = Harness::new(48000);
    let default = talk_with(&mut quiet, -30.0, 12.0, 0.9);
    let mut louder = Harness::new(48000);
    louder.settings.voice_gain.target_db = -21.0;
    let raised = talk_with(&mut louder, -30.0, 12.0, 0.9);
    assert!(in_range(f64::from(raised - default), 3.5, 4.5), "{default} -> {raised}");
}

#[test]
fn commands_on_unsupported_frames_wait_for_the_next_good_one() {
    let mut h = Harness::new(48000);
    for _ in 0..30 {
        h.run(0.0);
    }
    h.commands.gain_reset = true;
    h.commands.gain_reset_db = 5.0;
    h.run_input(&[0.0; 479]);
    assert!(h.out.unsupported && !h.out.flushed);
    h.commands.flush = true;
    let mut odd = vec![0.0f32; 481];
    assert!(!h.pre(&mut odd).supported);
    h.chain.post_denoise(&mut odd, DenoiserOutput::NotInUse, &h.settings, &mut h.out);
    let mut frame = vec![0.0f32; 480];
    let pre = h.pre(&mut frame);
    assert!(pre.supported && pre.flush_denoiser && h.out.flushed && !h.out.reset);
    h.chain.post_denoise(&mut frame, DenoiserOutput::NotInUse, &h.settings, &mut h.out);
    assert!(h.out.held, "the flush restarted the hold");
    assert_eq!(h.out.voice_gain_db, 5.0);

    h.commands.gain_reset = true;
    h.commands.gain_reset_db = 8.0;
    h.run_input(&[0.0; 10]);
    h.commands.reset = true;
    h.run_input(&[0.0; 10]);
    h.run(0.0);
    assert!(h.out.reset);
    assert_eq!(h.out.voice_gain_db, 0.0, "a later reset wipes an earlier gain reset");

    h.commands.reset = true;
    h.run_input(&[0.0; 10]);
    h.commands.gain_reset = true;
    h.commands.gain_reset_db = 8.0;
    h.run_input(&[0.0; 10]);
    h.commands.gain_reset = true;
    h.commands.gain_reset_db = f32::NAN;
    h.run_input(&[0.0; 10]);
    h.run(0.0);
    assert!(h.out.reset);
    assert_eq!(h.out.voice_gain_db, 8.0, "a gain reset after the reset survives it");
}

#[test]
fn the_first_frame_and_a_rate_change_flush_and_snap() {
    let mut h = Harness::pass_through(16000);
    let mut frame = vec![0.0f32; 160];
    let pre = h.pre(&mut frame);
    assert!(pre.flush_denoiser && h.out.flushed && !h.out.reset);
    h.chain.post_denoise(&mut frame, DenoiserOutput::NotInUse, &h.settings, &mut h.out);
    for _ in 1..30 {
        h.run(1000.0);
    }
    assert_eq!(h.run(1000.0), vec![1000.0; 160]);
    h.settings.sample_rate = 16000;
    let mut same = vec![1000.0f32; 160];
    assert!(!h.pre(&mut same).flush_denoiser);
    h.chain.post_denoise(&mut same, DenoiserOutput::NotInUse, &h.settings, &mut h.out);

    h.settings.sample_rate = 48000;
    h.settings.input_volume = 0.5;
    h.settings.mic_muted = true;
    let mut frame = vec![1000.0f32; 480];
    assert!(h.pre(&mut frame).flush_denoiser);
    h.chain.post_denoise(&mut frame, DenoiserOutput::NotInUse, &h.settings, &mut h.out);
    assert!(h.out.held && !h.out.transmitting, "muted at the switch: closed at once, no fade");
    h.settings.mic_muted = false;
    for _ in 0..HOLD + 2 {
        h.run(1000.0);
    }
    assert_eq!(h.run(1000.0), vec![500.0; 480], "the volume ramp snapped to 0.5");
}

#[test]
fn the_startup_hold_waits_for_the_denoiser_until_it_first_runs() {
    let mut h = Harness::pass_through(48000);
    h.settings.denoiser_wanted = true;
    for i in 0..MAX_DENOISER_WAIT_FRAMES as usize {
        assert!(silent(&h.run(1000.0)), "{i}");
        assert!(h.out.held);
    }
    h.run(1000.0);
    assert!(!h.out.held, "at most 100 frames");

    let mut h = Harness::pass_through(48000);
    h.settings.denoiser_wanted = true;
    for _ in 0..50 {
        h.run(1000.0);
    }
    let input = vec![1000.0f32; 480];
    h.run_with(&input, DenoiserOutput::Missed);
    assert!(!h.out.held, "a pipeline is installed: the wait is over");
    h.run(1000.0);
    assert!(!h.out.held);

    h.settings.sample_rate = 16000;
    for i in 0..MAX_DENOISER_WAIT_FRAMES {
        h.run(1000.0);
        assert!(h.out.held, "{i}: a rate change waits for a pipeline at the new rate");
    }
    h.run(1000.0);
    assert!(!h.out.held, "at most 100 frames");

    let input = vec![1000.0f32; 160];
    h.run_with(&input, DenoiserOutput::Missed);
    h.commands.flush = true;
    for i in 0..HOLD {
        h.run(1000.0);
        assert!(h.out.held, "{i}");
    }
    h.run(1000.0);
    assert!(!h.out.held, "a flush at the same rate keeps the pipeline: 20 frames only");

    h.commands.reset = true;
    for _ in 0..MAX_DENOISER_WAIT_FRAMES {
        h.run(1000.0);
        assert!(h.out.held);
    }
    h.run(1000.0);
    assert!(!h.out.held, "a reset waits for the denoiser again");
}

/// A mid-call switch to a 16 kHz headset: the 48 kHz pipeline is NotInUse until the host installs
/// one at 16 kHz, and the frames in between stay held rather than going out with no noise
/// suppression at all.
#[test]
fn a_rate_change_holds_until_a_pipeline_at_the_new_rate_runs() {
    let mut h = Harness::pass_through(48000);
    h.settings.denoiser_wanted = true;
    let input = vec![1000.0f32; 480];
    for _ in 0..30 {
        h.run_with(&input, DenoiserOutput::Ran { samples: &input, restart: false });
    }
    assert!(!h.out.held && h.out.denoiser_ran);

    h.settings.sample_rate = 16000;
    for i in 0..60 {
        assert!(silent(&h.run(1000.0)), "{i}");
        assert!(h.out.held && !h.out.denoiser_ran, "{i}");
    }
    let input = vec![1000.0f32; 160];
    h.run_with(&input, DenoiserOutput::Ran { samples: &input, restart: true });
    assert!(!h.out.held && h.out.denoiser_ran, "the pipeline at 16 kHz ends the wait");
    h.run(1000.0);
    assert!(!h.out.held, "and the latch holds while it is NotInUse again");
}

#[test]
fn the_hold_does_not_wait_when_the_denoiser_is_not_wanted_or_already_running() {
    let mut h = Harness::pass_through(48000);
    for _ in 0..HOLD {
        h.run(1000.0);
    }
    h.run(1000.0);
    assert!(!h.out.held);

    let mut h = Harness::pass_through(48000);
    h.settings.denoiser_wanted = true;
    let input = vec![1000.0f32; 480];
    let out = vec![500.0f32; 480];
    for _ in 0..HOLD {
        h.run_with(&input, DenoiserOutput::Ran { samples: &out, restart: false });
        assert!(h.out.held);
    }
    let opened = h.run_with(&input, DenoiserOutput::Ran { samples: &out, restart: false });
    assert!(opened[0] < 500.0 && opened[479] == 500.0);
    assert!(!h.out.held && h.out.denoiser_ran);
}

#[test]
fn the_denoiser_output_goes_through_the_aligner() {
    let mut h = Harness::pass_through(48000);
    let input = vec![1000.0f32; 480];
    let mut fifo: Vec<f32> = vec![0.0; 480];
    for call in 1..=40 {
        fifo.extend(input.iter().map(|v| (v * 0.5).round()));
        let take = if call == 6 { 960 } else { 480 };
        let out: Vec<f32> = fifo.drain(..take).collect();
        let restart = call == 1;
        h.run_with(&input, DenoiserOutput::Ran { samples: &out, restart });
        assert!(h.out.denoiser_ran && !h.out.denoiser_stalled);
    }
    assert_eq!(
        h.run_with(&input, DenoiserOutput::Ran { samples: &input[..0], restart: false }),
        vec![0.0; 480]
    );
    let a = h.out.aligner;
    assert!(a.primed);
    assert_eq!(a.underruns, 1);
    assert_eq!(a.drained_samples, 480, "the SDK's extra pre-roll block");
    assert_eq!(a.padded_samples, 480);
    assert_eq!(a.latency_ms, 0.0, "480 padded, 480 drained");
}

#[test]
fn a_denoiser_that_stops_returning_output_stalls_after_100_calls() {
    let mut h = Harness::pass_through(48000);
    h.settings.denoiser_wanted = true;
    let input = vec![1000.0f32; 480];
    let out = vec![500.0f32; 480];
    for _ in 0..30 {
        h.run_with(&input, DenoiserOutput::Ran { samples: &out, restart: false });
    }
    for i in 1..=DENOISER_STALL_FRAMES {
        h.run_with(&input, DenoiserOutput::Ran { samples: &[], restart: false });
        assert_eq!(h.out.denoiser_stalled, i >= DENOISER_STALL_FRAMES, "{i}");
    }
    h.run_with(&input, DenoiserOutput::Missed);
    assert!(h.out.denoiser_stalled, "a miss leaves the count alone");
    h.run_with(&input, DenoiserOutput::Ran { samples: &out, restart: false });
    assert!(!h.out.denoiser_stalled, "any output clears it");
    for _ in 0..DENOISER_STALL_FRAMES {
        h.run_with(&input, DenoiserOutput::Ran { samples: &[], restart: false });
    }
    assert!(h.out.denoiser_stalled);
    h.run(1000.0);
    assert!(!h.out.denoiser_stalled, "not in use clears it");
    for _ in 0..DENOISER_STALL_FRAMES - 1 {
        h.run_with(&input, DenoiserOutput::Ran { samples: &[], restart: false });
    }
    h.run_with(&input, DenoiserOutput::Ran { samples: &[], restart: true });
    assert!(!h.out.denoiser_stalled, "a restart starts the count over");
    h.commands.flush = true;
    for _ in 0..DENOISER_STALL_FRAMES - 1 {
        h.run_with(&input, DenoiserOutput::Ran { samples: &[], restart: false });
    }
    assert!(!h.out.denoiser_stalled, "a flush starts the count over");
}

#[test]
fn denoiser_output_is_capped_at_8_frames_and_sanitised() {
    let mut h = Harness::pass_through(16000);
    let input = vec![1000.0f32; 160];
    let mut out = vec![700.0f32; 160 * 8 + 500];
    out[0] = f32::NAN;
    out[1] = f32::INFINITY;
    out[2] = 1e30;
    out[160 * 8] = f32::NAN;
    let sent = h.run_with(&input, DenoiserOutput::Ran { samples: &out, restart: true });
    assert_eq!(h.out.non_finite_samples, 2, "the two in the first 8 frames");
    assert!(silent(&sent), "held");
    assert_eq!(h.out.aligner.buffered_samples, 0, "the pre-roll drained the surplus");
    assert_eq!(h.out.aligner.drained_samples, 160 * 7);
    assert!(h.out.denoised_sumsq > 0.0);
    assert_eq!(
        h.out.denoised_sumsq,
        160.0 * 700.0 * 700.0,
        "the newest frame; the oldest seven drained"
    );
}

#[test]
fn not_in_use_resets_the_aligner_once_and_passes_the_frame_dry() {
    let mut h = Harness::pass_through(48000);
    let input = vec![1000.0f32; 480];
    let out = vec![500.0f32; 960];
    h.run_with(&input, DenoiserOutput::Ran { samples: &out, restart: true });
    assert!(h.out.aligner.primed);
    h.run(1000.0);
    assert!(!h.out.aligner.primed && h.out.aligner.buffered_samples == 0);
    assert!(!h.out.denoiser_ran);
    let missed = h.run_with(&input, DenoiserOutput::Missed);
    assert!(!h.out.denoiser_ran);
    assert_eq!(missed.len(), 480);
}

#[test]
fn every_output_is_written_on_every_call() {
    let mut reused = Harness::new(48000);
    let mut fresh = Harness::new(48000);
    let mut rng = Rng::new(9);
    for i in 0..200 {
        let len = if i % 17 == 5 { 479 } else { 480 };
        let input: Vec<f32> = (0..len).map(|_| 3000.0 * rng.symmetric()).collect();
        reused.settings.mic_muted = i % 50 > 40;
        fresh.settings.mic_muted = reused.settings.mic_muted;
        let a = reused.run_input(&input);
        fresh.out = DspOutputs {
            threshold: f32::NAN,
            gate_in_rms: 9.0,
            held: true,
            gate_open: true,
            gate_input_above: true,
            denoiser_ran: true,
            denoiser_stalled: true,
            transmitting: true,
            speaking: true,
            ..DspOutputs::default()
        };
        let b = fresh.run_input(&input);
        assert_eq!(a, b, "{i}");
        assert_eq!(format!("{:?}", reused.out), format!("{:?}", fresh.out), "{i}");
    }
}

#[test]
fn unsupported_frames_report_the_last_state_and_no_per_frame_flags() {
    let mut h = Harness::new(48000);
    for _ in 0..40 {
        h.run_tone(3000.0);
    }
    let before = h.out;
    assert!(before.gate_open && before.gate_input_above && before.gate_speaking);
    let input = vec![100.0f32; 479];
    let out = h.run_input(&input);
    assert_eq!(out, input);
    let o = h.out;
    assert!(o.unsupported);
    assert_eq!((o.threshold, o.gate_speaking), (before.threshold, true));
    assert!(o.envelope < before.envelope, "the gate heard 10 ms of silence");
    assert_eq!(
        (o.voice_gain_db, o.voice_gain_settled),
        (before.voice_gain_db, before.voice_gain_settled)
    );
    assert_eq!(o.aligner, before.aligner);
    assert!(
        !o.held && !o.gate_open && !o.gate_input_above && !o.denoiser_ran && !o.denoiser_stalled
    );
    assert!(o.transmitting && o.transmit_open && o.speaking);
    assert_eq!(o.gate_in_rms, 0.0);
    assert_eq!(o.gate_in_block_peak_db, f32::NEG_INFINITY);
    let sumsq = 479.0 * 100.0 * 100.0;
    assert_eq!(
        (o.apm_out_sumsq, o.denoised_sumsq, o.gate_in_sumsq, o.sent_sumsq),
        (sumsq, sumsq, sumsq, sumsq)
    );

    h.settings.mic_muted = true;
    assert!(silent(&h.run_input(&input)));
    assert!(!h.out.transmitting && !h.out.transmit_open && !h.out.speaking && h.out.gate_speaking);
    assert_eq!(h.out.sent_sumsq, 0.0);
}

/// A stream the chain cannot process: the gate releases and its hold runs out as on silence, and
/// speaking follows what is actually sent (the audio goes out ungated, so it rings until muted or
/// silent).
#[test]
fn unsupported_frames_let_the_gate_run_out_and_ring_while_sent() {
    let mut h = Harness::new(48000);
    for _ in 0..40 {
        h.run_tone(3000.0);
    }
    assert!(h.out.speaking);
    let input = vec![3000.0f32; 479];
    let mut frames = 0;
    while h.out.gate_speaking {
        assert_eq!(h.run_input(&input), input, "the audio itself still goes out");
        assert!(h.out.speaking, "what goes out rings");
        frames += 1;
        assert!(frames < 100);
    }
    assert!((68..=72).contains(&frames), "{frames}: as many as for supported silence");
    assert!(h.out.speaking && h.out.transmitting);
    assert!(silent(&h.run_input(&[0.0f32; 479])));
    assert!(!h.out.speaking, "silence sent does not ring");
    h.settings.mic_muted = true;
    assert!(silent(&h.run_input(&input)));
    assert!(!h.out.speaking && !h.out.transmitting);
}

#[test]
fn unsupported_frames_follow_the_input_volume() {
    let mut h = Harness::pass_through(48000);
    for _ in 0..30 {
        h.run(1000.0);
    }
    let input = vec![1000.0f32; 479];
    h.settings.input_volume = 0.5;
    for _ in 0..3 {
        h.run_input(&input);
    }
    assert_eq!(h.run_input(&input), vec![500.0; 479], "the 20 ms ramp lands on 0.5");

    h.settings.input_volume = 0.0;
    for _ in 0..3 {
        h.run_input(&input);
    }
    assert!(silent(&h.run_input(&input)));
    assert!(h.out.transmitting && h.out.sent_sumsq == 0.0, "open, at volume 0");

    h.settings.input_volume = 2.0;
    for _ in 0..3 {
        h.run_input(&input);
    }
    assert_eq!(h.run_input(&[20000.0; 479]), vec![32768.0; 479], "clamped after the volume");
    assert_eq!(h.run(1000.0), vec![2000.0; 480], "the next supported frame continues at 2");
}

#[test]
fn post_without_a_matching_pre_is_unsupported() {
    let mut h = Harness::pass_through(48000);
    for _ in 0..30 {
        h.run(1000.0);
    }
    let mut frame = vec![1000.0f32; 480];
    h.chain.post_denoise(&mut frame, DenoiserOutput::NotInUse, &h.settings, &mut h.out);
    assert!(h.out.unsupported);
    assert_eq!(frame, vec![1000.0; 480]);

    let mut frame = vec![1000.0f32; 480];
    h.pre(&mut frame);
    let mut other = vec![1000.0f32; 160];
    h.chain.post_denoise(&mut other, DenoiserOutput::NotInUse, &h.settings, &mut h.out);
    assert!(h.out.unsupported, "a different length");
    h.chain.post_denoise(&mut frame, DenoiserOutput::NotInUse, &h.settings, &mut h.out);
    assert!(h.out.unsupported, "the pre was used up");

    let mut frame = vec![f32::NAN; 480];
    h.chain.post_denoise(&mut frame, DenoiserOutput::NotInUse, &h.settings, &mut h.out);
    assert!(silent(&frame), "non-finite never leaves");
}

#[test]
fn the_chain_runs_at_every_rate_with_the_managed_timing() {
    for rate in [8000u32, 11025, 16000, 22050, 24000, 32000, 44100, 48000] {
        let mut h = Harness::new(rate);
        let mut first_speaking = None;
        for i in 0..60 {
            let input = h.tone(3000.0, 300.0);
            let out = h.run_input(&input);
            assert_eq!(out.len(), h.n());
            if h.out.speaking && first_speaking.is_none() {
                first_speaking = Some(i);
            }
        }
        let first = first_speaking.unwrap_or(usize::MAX);
        assert!((HOLD..HOLD + 3).contains(&first), "{rate}: {first}");
        assert!(
            h.out.gate_in_block_peak_db > -30.0 && h.out.gate_in_block_peak_db < -12.0,
            "{rate}"
        );
        let mut silent_frames = 0;
        while h.out.gate_speaking {
            h.run(0.0);
            silent_frames += 1;
            assert!(silent_frames < 100, "{rate}");
        }
        assert!((68..=72).contains(&silent_frames), "{rate}: {silent_frames}");
    }
}

#[test]
fn max_rate_bounds_what_is_supported() {
    let mut chain = DspChain::new(16000);
    assert_eq!(chain.max_rate(), 16000);
    let mut out = DspOutputs::default();
    let settings = DspSettings { sample_rate: 48000, ..DspSettings::default() };
    let mut frame = vec![0.0f32; 480];
    assert!(!chain.pre_denoise(&mut frame, &settings, &DspCommands::default(), &mut out).supported);
    let settings = DspSettings { sample_rate: 16000, ..DspSettings::default() };
    let mut frame = vec![0.0f32; 160];
    assert!(chain.pre_denoise(&mut frame, &settings, &DspCommands::default(), &mut out).supported);
    assert_eq!(DspChain::new(1).max_rate(), 8000);
    assert_eq!(DspChain::new(1_000_000).max_rate(), 48000);
}

#[test]
fn input_is_sanitised_in_place_before_the_sdk_sees_it() {
    let mut h = Harness::new(48000);
    let mut frame = vec![1000.0f32; 480];
    frame[0] = f32::NAN;
    frame[1] = f32::NEG_INFINITY;
    frame[2] = 1e-30;
    frame[3] = -1e30;
    frame[4] = 2e5;
    h.pre(&mut frame);
    assert_eq!(&frame[..5], &[0.0, 0.0, 0.0, -131072.0, 131072.0]);
    assert_eq!(h.out.non_finite_samples, 2);
    assert_eq!(h.out.samples, 480);
}

/// A host's gain (GainEngine::External): `db` applied to the aligned frame between
/// post_denoise_align and post_gain, as libwebrtc's AGC2 does in the capture processor.
fn run_external(h: &mut Harness, input: &[f32], db: f32) -> Vec<f32> {
    let mut frame = input.to_vec();
    h.pre(&mut frame);
    let aligned =
        h.chain.post_denoise_align(&mut frame, DenoiserOutput::NotInUse, &h.settings, &mut h.out);
    if aligned.supported {
        let scale = 10f32.powf(db / 20.0);
        for s in frame.iter_mut() {
            *s *= scale;
        }
    }
    h.chain.post_gain(&mut frame, aligned.pre_gain_rms, db, &h.settings, &mut h.out);
    h.position += input.len();
    frame
}

/// With the host's gain the chain sends what the host made of the frame, and auto sensitivity
/// takes that gain: a 0 dB host gain sends exactly what the chain sends with its own gain off, and
/// with 12 dB the threshold is 12 dB higher on the same input.
#[test]
fn an_external_gain_is_what_the_frame_and_auto_sensitivity_get() {
    let mut off = Harness::new(48000);
    off.settings.auto_gain = false;
    let mut external = Harness::new(48000);
    external.settings.gain_engine = GainEngine::External;
    let mut boosted = Harness::new(48000);
    boosted.settings.gain_engine = GainEngine::External;
    let mut rng = Rng::new(4);
    for f in 0..600 {
        let level = if f % 30 < 20 { 0.02 } else { 0.0005 };
        let input: Vec<f32> = (0..480).map(|_| level * 32768.0 * rng.symmetric()).collect();
        let a = off.run_input(&input);
        let b = run_external(&mut external, &input, 0.0);
        let c = run_external(&mut boosted, &input, 12.0);
        assert_eq!(a, b, "frame {f}");
        assert_eq!(format!("{:?}", off.out.threshold), format!("{:?}", external.out.threshold));
        if f > 300 {
            let up = 20.0 * f64::from(boosted.out.threshold / external.out.threshold).log10();
            assert!(near(up, 12.0, 0.6), "frame {f}: {up}");
        }
        assert!(c.iter().zip(&b).all(|(c, b)| c.abs() >= b.abs()));
    }
    assert_eq!(boosted.out.voice_gain_db, 12.0);
    assert_eq!(boosted.out.gain_engine, GainEngine::External);
    assert_eq!(off.out.gain_engine, GainEngine::Legacy);
}

/// The chain's own voice gain keeps measuring under a host's gain: the room, the talker's level,
/// the floor and settled come from it, a gain reset starts it over, and the limit says which of the
/// host's limits holds its gain.
#[test]
fn the_chain_gain_measures_for_an_external_gain() {
    let mut h = Harness::new(48000);
    h.settings.gain_engine = GainEngine::External;
    h.settings.agc2 = Agc2Tuning::DEFAULT;
    let amplitude = (10f64.powf(-40.0 / 20.0) * 2f64.sqrt()) as f32 * 32768.0;
    for f in 0..600 {
        let input = if f % 30 < 20 { h.tone(amplitude, 220.0) } else { vec![0.0; 480] };
        run_external(&mut h, &input, 14.0);
    }
    assert!(h.out.voice_gain_settled);
    assert!(
        near(f64::from(h.out.voice_gain_speech_level_db), -40.0, 1.5),
        "{}",
        h.out.voice_gain_speech_level_db
    );
    assert_eq!(h.out.voice_gain_limit, GainLimit::Target);
    assert_eq!(h.out.voice_gain_db, 14.0);
    let floor = h.out.voice_gain_floor_db;
    assert!(floor <= -80.0, "{floor}");

    let input = h.tone(amplitude, 220.0);
    run_external(&mut h, &input, 29.5);
    assert_eq!(h.out.voice_gain_limit, GainLimit::Max, "within 1 dB of AGC2's 30 dB");
    let mut noisy = Harness::new(48000);
    noisy.settings.gain_engine = GainEngine::External;
    let hiss: Vec<f32> = (0..480).map(|i| if i % 2 == 0 { 30.0 } else { -30.0 }).collect();
    for _ in 0..50 {
        run_external(&mut noisy, &hiss, 20.0);
    }
    let floor = noisy.out.voice_gain_floor_db;
    assert!(floor > -62.0 && floor < -60.0, "{floor}");
    assert_eq!(
        noisy.out.voice_gain_limit,
        GainLimit::Noise,
        "AGC2's -50 dBFS over a -61 dBFS floor"
    );

    h.commands.gain_reset = true;
    h.commands.gain_reset_db = 6.0;
    run_external(&mut h, &input, 6.0);
    assert!(!h.out.voice_gain_settled);
    assert!(h.out.voice_gain_room_db.is_nan());

    h.settings.auto_gain = false;
    run_external(&mut h, &input, 0.0);
    assert_eq!(h.out.voice_gain_db, 6.0, "off keeps the last value");
    for db in [f32::NAN, f32::INFINITY, -4.0, 80.0] {
        h.settings.auto_gain = true;
        run_external(&mut h, &input, db);
        let expected = if db == 80.0 { 50.0 } else { 0.0 };
        assert_eq!(h.out.voice_gain_db, expected, "{db}");
    }
}

/// post_denoise is post_denoise_align then post_gain with the chain's own gain, whatever engine
/// the settings name; an unsupported frame passes post_denoise_align untouched and post_gain treats
/// it as unsupported, as post_gain without post_denoise_align does.
#[test]
fn post_denoise_is_align_then_the_chain_gain() {
    let mut whole = Harness::new(48000);
    let mut split = Harness::new(48000);
    split.settings.gain_engine = GainEngine::External;
    whole.settings.gain_engine = GainEngine::External;
    let mut rng = Rng::new(12);
    for f in 0..400 {
        let len = if f % 37 == 5 { 479 } else { 480 };
        let level = if f % 30 < 20 { 0.03 } else { 0.001 };
        let input: Vec<f32> = (0..len).map(|_| level * 32768.0 * rng.symmetric()).collect();
        let a = whole.run_input(&input);
        let mut frame = input.clone();
        split.pre(&mut frame);
        let legacy = DspSettings { gain_engine: GainEngine::Legacy, ..split.settings };
        let aligned = split.chain.post_denoise_align(
            &mut frame,
            DenoiserOutput::NotInUse,
            &legacy,
            &mut split.out,
        );
        assert_eq!(aligned.supported, len == 480);
        if !aligned.supported {
            assert_eq!(frame, input);
        }
        split.chain.post_gain(&mut frame, aligned.pre_gain_rms, 99.0, &legacy, &mut split.out);
        assert_eq!(a, frame, "frame {f}");
        assert_eq!(format!("{:?}", whole.out), format!("{:?}", split.out), "frame {f}");
    }
    let mut frame = vec![100.0f32; 480];
    split.chain.post_gain(&mut frame, 0.5, 12.0, &split.settings, &mut split.out);
    assert!(split.out.unsupported, "post_gain without post_denoise_align");
}

/// A frame of `value` everywhere.
fn flat(value: f32) -> Vec<f32> {
    vec![value; 480]
}

/// A hardware mute with the denoiser in use: the frames the host does not hand to the pipeline are
/// silence, the aligner keeps what it held and pops nothing, so the denoiser's stream carries on
/// where it stopped; no frame counts as an empty output, however long the mute.
#[test]
fn a_frame_the_denoiser_skipped_is_silence_and_its_stream_carries_on_where_it_stopped() {
    let mut h = Harness::pass_through(48000);
    h.settings.denoiser_wanted = true;
    let input = flat(1000.0);
    let first: Vec<f32> = [flat(1.0), flat(2.0)].concat()[..720].to_vec();
    h.run_with(&input, DenoiserOutput::Ran { samples: &first, restart: true });
    for call in 2..=40 {
        h.run_with(
            &input,
            DenoiserOutput::Ran { samples: &flat(call as f32 * 10.0), restart: false },
        );
    }
    let before = h.out.aligner;
    assert_eq!(before.buffered_samples, 240, "half of the last output waits in the aligner");
    for i in 0..DENOISER_STALL_FRAMES as usize + 50 {
        let mut frame = input.clone();
        h.pre(&mut frame);
        let aligned = h.chain.post_denoise_align(
            &mut frame,
            DenoiserOutput::Skipped,
            &h.settings,
            &mut h.out,
        );
        assert_eq!((aligned.supported, aligned.pre_gain_rms), (true, 0.0), "{i}");
        assert!(silent(&frame), "{i}: the frame before the gain is silence");
        h.chain.post_gain(&mut frame, aligned.pre_gain_rms, f32::NAN, &h.settings, &mut h.out);
        assert!(silent(&frame), "{i}");
        assert!(h.out.denoiser_skipped && !h.out.denoiser_ran && !h.out.denoiser_stalled, "{i}");
        assert_eq!(h.out.denoised_sumsq, 0.0);
        assert_eq!(h.out.aligner, before, "{i}: the aligner is untouched");
    }
    let mut resumed = input.clone();
    h.pre(&mut resumed);
    let next = flat(410.0);
    let denoised = DenoiserOutput::Ran { samples: &next, restart: false };
    h.chain.post_denoise_align(&mut resumed, denoised, &h.settings, &mut h.out);
    assert_eq!(&resumed[..240], &flat(400.0)[..240], "what the aligner held first");
    assert_eq!(&resumed[240..], &flat(410.0)[..240], "then the pipeline's next output");
    assert!(!h.out.denoiser_skipped && h.out.denoiser_ran && !h.out.denoiser_stalled);
    assert_eq!(h.out.aligner.underruns, before.underruns);
}

#[test]
fn skipped_frames_are_no_denoiser_for_the_startup_hold() {
    let mut h = Harness::pass_through(48000);
    h.settings.denoiser_wanted = true;
    let input = flat(1000.0);
    for i in 0..60 {
        let sent = h.run_with(&input, DenoiserOutput::Skipped);
        assert!(silent(&sent) && h.out.held, "{i}: the hold still waits for the denoiser");
    }
    h.run_with(&input, DenoiserOutput::Ran { samples: &input, restart: true });
    assert!(!h.out.held, "a pipeline ran");
}

/// Gate mode, manual threshold 0.01 (-40 dBFS), no voice gain, a denoiser that returns this frame
/// scaled by `kept` (no latency). Returns the frames counted erased and cut.
fn speech_loss(script: &[(f32, f32, f32, bool)]) -> (Vec<usize>, Vec<usize>) {
    let mut h = Harness::new(48000);
    h.settings.auto_gain = false;
    h.settings.auto_sensitivity = false;
    h.settings.gate_threshold = 0.01;
    h.settings.denoiser_wanted = true;
    h.settings.speech_detection = SpeechDetection::Gate;
    let (mut erased, mut cut) = (vec![], vec![]);
    for (f, &(level_db, kept, p, muted)) in script.iter().enumerate() {
        let amplitude = 10f32.powf(level_db / 20.0) * std::f32::consts::SQRT_2 * 32768.0;
        let input = h.tone(amplitude, 220.0);
        let denoised: Vec<f32> = input.iter().map(|v| v * kept).collect();
        h.settings.speech_probability = p;
        h.settings.mic_muted = muted;
        h.run_with(&input, DenoiserOutput::Ran { samples: &denoised, restart: f == 0 });
        if h.out.speech_denoiser_erased {
            erased.push(f);
        }
        if h.out.speech_gate_cut {
            cut.push(f);
        }
    }
    (erased, cut)
}

/// Frames a..b.
fn frames(a: usize, b: usize) -> Vec<usize> {
    (a..b).collect()
}

#[test]
fn steady_confirmed_speech_the_denoiser_erases_or_the_level_gate_cuts_is_counted() {
    const QUIET: (f32, f32, f32, bool) = (-80.0, 1.0, 0.0, false);
    const LOOKAHEAD: usize = 4;
    let steady = |start: usize| start + 9;
    let script = |speech: (f32, f32, f32, bool)| {
        let mut s = vec![QUIET; 40];
        s.extend(std::iter::repeat_n(speech, 50));
        s.extend(std::iter::repeat_n(QUIET, 60));
        s
    };

    let (erased, cut) = speech_loss(&script((-20.0, 0.01, 0.9, false)));
    assert_eq!(erased, frames(steady(40), 90), "40 dB under its input");
    assert!(cut.is_empty(), "what the denoiser erased is not the level gate's");

    let (erased, cut) = speech_loss(&script((-46.0, 0.5, 0.9, false)));
    assert!(erased.is_empty(), "6 dB under its input is kept");
    assert_eq!(cut, frames(steady(40) + LOOKAHEAD, 90 + LOOKAHEAD), "-52 dBFS under -40");

    let (erased, cut) = speech_loss(&script((-20.0, 0.5, 0.9, false)));
    assert!(erased.is_empty() && cut.is_empty(), "sent");

    let (erased, cut) = speech_loss(&script((-46.0, 0.01, 0.4, false)));
    assert!(erased.is_empty() && cut.is_empty(), "never confirmed");

    let (erased, cut) = speech_loss(&script((-46.0, 0.01, 0.9, true)));
    assert!(erased.is_empty() && cut.is_empty(), "muted");

    let mut s = script((-20.0, 1.0, 0.9, false));
    for frame in s.iter_mut().skip(90).take(30) {
        *frame = (-50.0, 0.01, 0.0, false);
    }
    let (erased, cut) = speech_loss(&s);
    assert!(
        erased.is_empty() && cut.is_empty(),
        "noise the denoiser removes in the hang is no speech"
    );

    let mut s = script((-20.0, 0.01, 0.9, false));
    s[60] = (-20.0, 0.01, 0.2, false);
    let (erased, _) = speech_loss(&s);
    assert_eq!(
        erased,
        [frames(steady(40), 60), frames(steady(61), 90)].concat(),
        "a frame under 0.3 starts the run over"
    );
}

#[test]
fn the_threshold_stats_say_what_set_the_open_threshold() {
    let mut h = Harness::new(48000);
    h.settings.gain_engine = GainEngine::External;
    h.settings.auto_sensitivity = false;
    h.settings.gate_threshold = 0.01;
    let input = flat(0.0);
    run_external(&mut h, &input, 6.0);
    assert_eq!(h.out.threshold_term, ThresholdTerm::Manual);
    assert!(near(f64::from(h.out.open_threshold_db), -46.0, 1e-4), "{}", h.out.open_threshold_db);
    assert!(h.out.auto_sensitivity_floor_db.is_nan());
    h.settings.gate_threshold = 0.0;
    run_external(&mut h, &input, 6.0);
    assert_eq!(h.out.open_threshold_db, -140.0, "an open mic");

    h.settings.auto_sensitivity = true;
    let mut rng = Rng::new(8);
    let mut room = |h: &mut Harness, db: f64, gain: f32, frames: usize| {
        let level = (10f64.powf(db / 20.0) * 3f64.sqrt()) as f32 * 32768.0;
        for _ in 0..frames {
            let input: Vec<f32> = (0..480).map(|_| level * rng.symmetric()).collect();
            run_external(h, &input, gain);
        }
    };
    room(&mut h, -70.0, 0.0, 600);
    assert!(
        near(f64::from(h.out.auto_sensitivity_floor_db), -70.0, 1.5),
        "{}",
        h.out.auto_sensitivity_floor_db
    );
    assert_eq!(h.out.threshold_term, ThresholdTerm::Minimum, "-62 under -54");
    assert_eq!(h.out.open_threshold_db, -54.0);
    room(&mut h, -70.0, 20.0, 50);
    assert_eq!(
        h.out.threshold_term,
        ThresholdTerm::Floor,
        "the minimum gives way by 9 dB of the gain"
    );
    assert!(near(f64::from(h.out.open_threshold_db), -62.0, 1.5), "{}", h.out.open_threshold_db);
    room(&mut h, -45.0, 0.0, 6000);
    assert_eq!(h.out.threshold_term, ThresholdTerm::Floor);
    assert!(near(f64::from(h.out.open_threshold_db), -37.0, 1.5), "{}", h.out.open_threshold_db);

    let mut unsupported = vec![0.0f32; 479];
    h.pre(&mut unsupported);
    let kept = (h.out.open_threshold_db, h.out.threshold_term);
    h.chain.post_denoise(&mut unsupported, DenoiserOutput::NotInUse, &h.settings, &mut h.out);
    assert_eq!((h.out.open_threshold_db, h.out.threshold_term), kept, "the last state");
    h.commands.reset = true;
    let mut unsupported = vec![0.0f32; 479];
    h.pre(&mut unsupported);
    h.chain.post_denoise(&mut unsupported, DenoiserOutput::NotInUse, &h.settings, &mut h.out);
    assert_eq!(h.out.threshold_term, ThresholdTerm::Floor, "commands wait for a supported frame");
}
