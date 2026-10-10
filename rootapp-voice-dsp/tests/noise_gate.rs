#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects
)]

mod support;

use rootapp_voice_dsp::noise_gate::{
    block_samples_for, NoiseGate, ATTACK_SECONDS, BLOCK_SAMPLES, FULL_ON_ENVELOPE,
};
use support::{near, silent};

const RATE: u32 = 48000;
const FRAME: usize = 480;

fn new_gate() -> NoiseGate {
    NoiseGate::new(RATE, BLOCK_SAMPLES)
}

fn tone(amplitude: f32, samples: usize) -> Vec<f32> {
    (0..samples)
        .map(|i| {
            amplitude
                * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / f64::from(RATE)).sin() as f32
        })
        .collect()
}

fn run(gate: &mut NoiseGate, amplitude: f32, milliseconds: usize) {
    for _ in 0..milliseconds / 10 {
        gate.process(&mut tone(amplitude, FRAME));
    }
}

#[test]
fn silence_stays_closed() {
    let mut gate = new_gate();
    let mut buffer = tone(0.0, FRAME);
    gate.process(&mut buffer);
    assert!(!gate.is_speaking());
    assert_eq!(gate.envelope(), 0.0);
    assert!(silent(&buffer));
}

#[test]
fn quiet_noise_below_threshold_is_attenuated_to_silence() {
    let mut gate = new_gate();
    let mut buffer = tone(0.02, FRAME);
    gate.process(&mut buffer);
    assert!(!gate.is_speaking());
    assert!(silent(&buffer));
}

#[test]
fn speech_opens_with_twenty_millisecond_attack() {
    let mut gate = new_gate();
    let input = tone(0.1, FRAME);
    let mut buffer = input.clone();
    gate.process(&mut buffer);

    assert!(gate.is_speaking());
    let first_block_multiplier =
        (128.0 / f64::from(RATE) / ATTACK_SECONDS / f64::from(FULL_ON_ENVELOPE)) as f32;
    assert!(near(f64::from(buffer[10]), f64::from(input[10] * first_block_multiplier), 1e-5));
    assert!(near(f64::from(buffer[300]), f64::from(input[300]), 1e-6));
}

#[test]
fn release_takes_four_hundred_milliseconds_then_vad_holds_three_hundred() {
    let mut gate = new_gate();
    run(&mut gate, 0.2, 100);
    assert_eq!(gate.envelope(), 1.0);

    run(&mut gate, 0.0, 380);
    assert!(gate.envelope() > 0.0);
    assert!(gate.is_speaking());

    run(&mut gate, 0.0, 30);
    assert_eq!(gate.envelope(), 0.0);
    assert!(gate.is_speaking());

    run(&mut gate, 0.0, 270);
    assert!(gate.is_speaking());

    run(&mut gate, 0.0, 40);
    assert!(!gate.is_speaking());
}

#[test]
fn full_gain_reached_at_envelope_zero_point_two_during_release() {
    let mut gate = new_gate();
    run(&mut gate, 0.2, 100);
    run(&mut gate, 0.0, 320);
    let input = vec![0.01f32; FRAME];
    let mut buffer = input.clone();
    gate.process(&mut buffer);
    assert!(gate.envelope() < FULL_ON_ENVELOPE);
    assert!(buffer[FRAME - 1] < input[FRAME - 1]);
    assert!(buffer[FRAME - 1] > 0.0);
}

#[test]
fn push_to_talk_held_opens_regardless_of_level() {
    let mut gate = new_gate();
    gate.set_push_to_talk(Some(true));
    let quiet = tone(0.005, FRAME);
    let mut buffer = quiet.clone();
    gate.process(&mut buffer);
    assert!(gate.is_speaking());
    let mut buffer = quiet.clone();
    gate.process(&mut buffer);
    assert!(near(f64::from(buffer[200]), f64::from(quiet[200]), 1e-6));
}

#[test]
fn push_to_talk_idle_silences_and_clears_speaking_immediately() {
    let mut gate = new_gate();
    run(&mut gate, 0.3, 50);
    assert!(gate.is_speaking());

    gate.set_push_to_talk(Some(false));
    assert!(!gate.is_speaking());
    assert_eq!(gate.envelope(), 0.0);

    let mut buffer = tone(0.5, FRAME);
    gate.process(&mut buffer);
    assert!(!gate.is_speaking());
    assert!(silent(&buffer));
}

#[test]
fn push_to_talk_idle_without_applying_leaves_the_samples_to_the_transmit_gate() {
    let mut gate = new_gate();
    run(&mut gate, 0.3, 50);
    gate.set_push_to_talk(Some(false));
    let input = tone(0.5, FRAME);
    let mut buffer = input.clone();
    gate.process_gated(&mut buffer, false);
    assert_eq!(buffer, input);
    assert!(!gate.is_speaking());
    assert_eq!(gate.envelope(), 0.0);
}

#[test]
fn threshold_is_adjustable() {
    let mut gate = new_gate();
    gate.set_threshold(0.2);
    gate.process(&mut tone(0.1, FRAME));
    assert!(!gate.is_speaking());
    gate.set_threshold(0.01);
    gate.process(&mut tone(0.1, FRAME));
    assert!(gate.is_speaking());
}

/// A key click: a few loud blocks with silence around them.
fn click(loud_blocks: usize) -> Vec<f32> {
    let mut buffer = vec![0.0f32; FRAME];
    let loud = tone(0.5, loud_blocks * BLOCK_SAMPLES);
    buffer[..loud.len()].copy_from_slice(&loud);
    buffer
}

#[test]
fn with_min_open_blocks_a_short_click_never_opens() {
    let mut gate = new_gate();
    gate.set_min_open_blocks(5);
    for _ in 0..20 {
        let mut buffer = click(2);
        gate.process(&mut buffer);
        assert!(silent(&buffer));
    }
    assert!(!gate.is_speaking());
}

#[test]
fn with_min_open_blocks_sustained_sound_opens_after_them() {
    let mut gate = new_gate();
    gate.set_min_open_blocks(5);
    let mut buffer = tone(0.5, BLOCK_SAMPLES * 8);
    gate.process(&mut buffer);
    assert!(gate.is_speaking());
    assert!(silent(&buffer[..BLOCK_SAMPLES * 4]));
    assert!(buffer[BLOCK_SAMPLES * 5..].iter().any(|&v| v != 0.0));
}

#[test]
fn without_min_open_blocks_the_first_loud_block_opens() {
    let mut gate = new_gate();
    let mut buffer = click(1);
    gate.process(&mut buffer);
    assert!(gate.is_speaking());
    assert!(buffer[..BLOCK_SAMPLES].iter().any(|&v| v != 0.0));
}

#[test]
fn the_block_is_the_same_time_at_every_rate() {
    let expected = [
        (48000, 128),
        (44100, 118),
        (32000, 85),
        (24000, 64),
        (22050, 59),
        (16000, 43),
        (11025, 29),
        (8000, 21),
        (1, 1),
        (0, 1),
    ];
    for (rate, block) in expected {
        assert_eq!(block_samples_for(rate), block, "{rate}");
    }
}

#[test]
fn the_vad_hold_rounds_like_math_round() {
    assert_eq!(NoiseGate::new(48000, 128).hold_samples(), 14400);
    assert_eq!(NoiseGate::new(11025, 29).hold_samples(), 3308, "3307.5 rounds to even");
    assert_eq!(NoiseGate::new(8015, 21).hold_samples(), 2404, "2404.5 rounds to even");
    assert_eq!(NoiseGate::new(16000, 43).hold_samples(), 4800);
    assert_eq!(NoiseGate::new(0, 0).sample_rate(), 1);
    assert_eq!(NoiseGate::new(0, 0).block_samples(), 1);
}

/// At 16 kHz the gate keeps managed's 48 kHz timing: five blocks of about 2.7 ms before a
/// closed gate opens in auto mode, 20 ms attack, 400 ms release and a 300 ms hold.
#[test]
fn rate_scaled_blocks_keep_the_timing() {
    let rate = 16000;
    let block = block_samples_for(rate);
    let mut gate = NoiseGate::new(rate, block);
    gate.set_min_open_blocks(5);
    let mut buffer: Vec<f32> = (0..block * 8)
        .map(|i| {
            0.5 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / f64::from(rate)).sin() as f32
        })
        .collect();
    gate.process(&mut buffer);
    assert!(silent(&buffer[..block * 4]));
    assert!(buffer[block * 5..].iter().any(|&v| v != 0.0));

    let mut open = NoiseGate::new(rate, block);
    for _ in 0..10 {
        open.process(&mut vec![0.2; 160]);
    }
    assert_eq!(open.envelope(), 1.0);
    let mut frames = 0;
    while open.is_speaking() {
        open.process(&mut [0.0; 160]);
        frames += 1;
        assert!(frames < 100);
    }
    assert!((69..=71).contains(&frames), "{frames}");
}
