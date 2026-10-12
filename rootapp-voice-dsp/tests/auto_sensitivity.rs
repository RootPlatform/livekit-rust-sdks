#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::disallowed_macros,
    clippy::arithmetic_side_effects
)]

mod support;

use rootapp_voice_dsp::auto_sensitivity::{to_db, to_rms, AutoSensitivity, AutoSensitivityOptions};
use support::{in_range, near};

fn new_auto() -> AutoSensitivity {
    AutoSensitivity::new(AutoSensitivityOptions::default())
}

fn feed_gain(
    auto: &mut AutoSensitivity,
    level_db: f64,
    seconds: f64,
    gate_open: bool,
    gain_db: f64,
) -> f32 {
    let rms = to_rms(level_db);
    let mut threshold = 0.0;
    for _ in 0..(seconds * 100.0) as usize {
        threshold = auto.update(rms, gain_db, gate_open);
    }
    threshold
}

fn feed(auto: &mut AutoSensitivity, level_db: f64, seconds: f64, gate_open: bool) {
    feed_gain(auto, level_db, seconds, gate_open, 0.0);
}

#[test]
fn the_floor_settles_on_the_room_noise() {
    let mut auto = new_auto();
    feed(&mut auto, -45.0, 10.0, false);
    assert!(in_range(auto.floor_db(), -46.0, -44.0), "{}", auto.floor_db());
    assert!(in_range(auto.open_db(), -38.0, -36.0), "{}", auto.open_db());
    let hysteresis = AutoSensitivityOptions::default().hysteresis_db;
    assert!(near(auto.close_db(), auto.open_db() - hysteresis, 1e-6));
}

#[test]
fn a_quiet_room_never_drops_the_threshold_below_the_minimum() {
    let mut auto = new_auto();
    feed(&mut auto, -100.0, 10.0, false);
    let options = AutoSensitivityOptions::default();
    assert!(near(auto.floor_db(), options.floor_minimum_db, 1e-6));
    assert!(near(auto.open_db(), options.open_minimum_db, 1e-6));
}

#[test]
fn talking_is_never_learned_as_noise() {
    let mut auto = new_auto();
    feed(&mut auto, -70.0, 5.0, false);
    let floor = auto.floor_db();
    for _ in 0..60 {
        feed(&mut auto, -20.0, 0.4, true);
        feed(&mut auto, -70.0, 0.1, true);
    }
    assert!(in_range(auto.floor_db(), floor - 0.5, floor + 0.5), "{} vs {floor}", auto.floor_db());
}

#[test]
fn a_steady_noise_that_opens_the_gate_is_learned_until_it_closes() {
    let mut auto = new_auto();
    feed(&mut auto, -45.0, 2.0, true);
    assert!(
        auto.close_db() > -45.0,
        "close threshold {} dB should be above the noise",
        auto.close_db()
    );
}

#[test]
fn after_startup_the_floor_rises_slowly() {
    let mut auto = new_auto();
    feed(&mut auto, -70.0, 5.0, false);
    feed(&mut auto, -40.0, 5.0, false);
    assert!(in_range(auto.floor_db(), -66.0, -64.0), "{}", auto.floor_db());
}

#[test]
fn an_open_gate_gets_the_lower_close_threshold() {
    let mut auto = new_auto();
    feed(&mut auto, -50.0, 10.0, false);
    let open = auto.update(to_rms(-50.0), 0.0, false);
    let close = auto.update(to_rms(-20.0), 0.0, true);
    assert!(near(auto.open_db(), to_db(open), 1e-3));
    assert!(near(auto.close_db(), to_db(close), 1e-3));
    assert!(close < open);
}

/// Managed TheThresholdIsRaisedByTheVoiceGain: the floor and the minimum are before the voice
/// gain, the gate gets the threshold after it.
#[test]
fn the_threshold_is_raised_by_the_voice_gain() {
    let mut auto = new_auto();
    let threshold = feed_gain(&mut auto, -100.0, 10.0, false, 10.0);
    let minimum = AutoSensitivityOptions::default().open_minimum_db;
    assert!(near(auto.open_db(), minimum, 1e-6));
    assert!(near(to_db(threshold), minimum + 10.0, 1e-3), "{}", to_db(threshold));
    let close = auto.update(to_rms(-100.0), 10.0, true);
    assert!(near(to_db(close), auto.close_db() + 10.0, 1e-3));
}

/// Managed AGainChangeMovesTheThresholdAtOnce.
#[test]
fn a_gain_change_moves_the_threshold_at_once() {
    let mut auto = new_auto();
    feed(&mut auto, -50.0, 10.0, false);
    let floor = auto.floor_db();
    let open = auto.open_db();
    let threshold = auto.update(to_rms(-50.0), 15.0, false);
    assert!(near(to_db(threshold), open + 15.0, 1e-3));
    assert!(in_range(auto.floor_db(), floor - 0.1, floor + 0.1));
    assert!(to_db(threshold) > -50.0 + 15.0, "the boosted noise stays under the threshold");
}

/// Managed LeakedNoiseStaysUnderTheThresholdAtAnyGain.
#[test]
fn leaked_noise_stays_under_the_threshold_at_any_gain() {
    for gain in [0.0, 6.0, 12.0, 18.0] {
        let mut auto = new_auto();
        feed_gain(&mut auto, -100.0, 5.0, false, gain);
        for i in 0..300 {
            let leak_db = -66.0 + 3.0 * (f64::from(i) * 0.37).sin();
            let threshold = auto.update(to_rms(leak_db), gain, false);
            assert!(
                leak_db + gain < to_db(threshold),
                "gain {gain}: {leak_db} dB crossed {}",
                to_db(threshold)
            );
        }
    }
}

/// Managed ANonFiniteGainCountsAsNone.
#[test]
fn a_non_finite_gain_counts_as_none() {
    let mut auto = new_auto();
    feed(&mut auto, -100.0, 5.0, false);
    for gain in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let threshold = auto.update(to_rms(-100.0), gain, false);
        assert!(near(to_db(threshold), auto.open_db(), 1e-3), "{gain}");
    }
}

#[test]
fn reset_starts_over_from_the_initial_floor_with_an_empty_window() {
    let mut auto = new_auto();
    feed(&mut auto, -80.0, 10.0, false);
    auto.reset();
    assert_eq!(auto.floor_db(), AutoSensitivityOptions::default().initial_floor_db);
    let threshold = auto.update(to_rms(-50.0), 0.0, false);
    assert!(near(to_db(threshold), auto.open_db(), 1e-3));
    assert!(near(auto.floor_db(), -59.9, 1e-6), "startup rise: {}", auto.floor_db());
    let close = auto.update(to_rms(-20.0), 0.0, true);
    assert!(near(to_db(close), auto.close_db(), 1e-3));
    assert!(near(auto.floor_db(), -59.8, 1e-6), "no -80 dB frame left: {}", auto.floor_db());
}

#[test]
fn silence_and_zero_rms_are_floored_at_minus_140_db() {
    assert!(near(to_db(0.0), 20.0 * f64::from(1e-7f32).log10(), 1e-12));
    let mut auto = new_auto();
    let threshold = auto.update(0.0, 0.0, false);
    assert!(threshold.is_finite() && threshold > 0.0);
}
