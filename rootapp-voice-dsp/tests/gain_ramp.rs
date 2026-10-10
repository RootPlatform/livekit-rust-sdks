#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects
)]

use rootapp_voice_dsp::gain_ramp::{GainRamp, DEFAULT_RAMP_SAMPLES};

fn near(actual: f32, expected: f32, tolerance: f32) -> bool {
    (actual - expected).abs() <= tolerance
}

#[test]
fn gain_ramp_reaches_target_after_twenty_milliseconds() {
    let mut ramp = GainRamp::new(0.0, DEFAULT_RAMP_SAMPLES);
    ramp.set_target(1.0);
    let mut buffer = vec![1.0f32; 960];
    ramp.apply(&mut buffer);

    assert_eq!(ramp.current(), 1.0);
    assert!(near(buffer[479], 0.5, 0.005), "{}", buffer[479]);
    assert!(near(buffer[959], 1.0, 0.00005));
    assert!(buffer.windows(2).all(|w| w[1] >= w[0]));
}

#[test]
fn gain_ramp_holds_after_reaching_target() {
    let mut ramp = GainRamp::new(1.0, DEFAULT_RAMP_SAMPLES);
    ramp.set_target(0.25);
    ramp.apply(&mut [0.0; 960]);
    let mut buffer = vec![1.0f32; 100];
    ramp.apply(&mut buffer);
    assert!(buffer.iter().all(|&v| near(v, 0.25, 0.000005)));
}

#[test]
fn gain_ramp_retargets_mid_ramp() {
    let mut ramp = GainRamp::new(0.0, DEFAULT_RAMP_SAMPLES);
    ramp.set_target(1.0);
    ramp.apply(&mut [0.0; 480]);
    assert!(near(ramp.current(), 0.5, 0.0005));
    ramp.set_target(0.0);
    ramp.apply(&mut [0.0; 960]);
    assert_eq!(ramp.current(), 0.0);
    assert!(ramp.is_silent());
}

#[test]
fn gain_ramp_unity_and_zero_shortcuts() {
    let mut ramp = GainRamp::new(1.0, DEFAULT_RAMP_SAMPLES);
    let mut buffer = vec![0.3f32; 50];
    ramp.apply(&mut buffer);
    assert!(buffer.iter().all(|&v| v == 0.3));

    ramp.set_target(0.0);
    let mut buffer = vec![1.0f32; 1000];
    ramp.apply(&mut buffer);
    assert!(buffer[960..].iter().all(|&v| v == 0.0));
    assert!(buffer[..959].iter().all(|&v| v > 0.0));
    assert!(ramp.is_silent());
}

#[test]
fn gain_ramp_same_target_does_not_restart() {
    let mut ramp = GainRamp::new(0.0, 100);
    ramp.set_target(1.0);
    ramp.apply(&mut [0.0; 50]);
    let halfway = ramp.current();
    ramp.set_target(1.0);
    ramp.apply(&mut [0.0; 50]);
    assert!(near(halfway, 0.5, 0.001));
    assert_eq!(ramp.current(), 1.0);
}

#[test]
fn gain_ramp_length_is_at_least_one_sample() {
    let mut ramp = GainRamp::new(0.0, 0);
    ramp.set_target(2.0);
    let mut buffer = [1.0f32; 4];
    ramp.apply(&mut buffer);
    assert_eq!(buffer, [2.0; 4]);
}

#[test]
fn gain_ramp_snap_lands_on_the_value() {
    let mut ramp = GainRamp::new(0.0, 960);
    ramp.set_target(1.0);
    ramp.apply(&mut [0.0; 10]);
    ramp.snap(0.5, 320);
    assert_eq!((ramp.current(), ramp.target()), (0.5, 0.5));
    ramp.set_target(1.0);
    ramp.apply(&mut [0.0; 160]);
    assert!(near(ramp.current(), 0.75, 0.001), "{}", ramp.current());
}
