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

fn feed(auto: &mut AutoSensitivity, level_db: f64, seconds: f64, gate_open: bool) {
    let rms = to_rms(level_db);
    for _ in 0..(seconds * 100.0) as usize {
        auto.update(rms, gate_open);
    }
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
    let open = auto.update(to_rms(-50.0), false);
    let close = auto.update(to_rms(-20.0), true);
    assert!(near(auto.open_db(), to_db(open), 1e-3));
    assert!(near(auto.close_db(), to_db(close), 1e-3));
    assert!(close < open);
}

#[test]
fn reset_starts_over_from_the_initial_floor_with_an_empty_window() {
    let mut auto = new_auto();
    feed(&mut auto, -80.0, 10.0, false);
    auto.reset();
    assert_eq!(auto.floor_db(), AutoSensitivityOptions::default().initial_floor_db);
    let threshold = auto.update(to_rms(-50.0), false);
    assert!(near(to_db(threshold), auto.open_db(), 1e-3));
    assert!(near(auto.floor_db(), -59.9, 1e-6), "startup rise: {}", auto.floor_db());
    let close = auto.update(to_rms(-20.0), true);
    assert!(near(to_db(close), auto.close_db(), 1e-3));
    assert!(near(auto.floor_db(), -59.8, 1e-6), "no -80 dB frame left: {}", auto.floor_db());
}

#[test]
fn silence_and_zero_rms_are_floored_at_minus_140_db() {
    assert!(near(to_db(0.0), 20.0 * f64::from(1e-7f32).log10(), 1e-12));
    let mut auto = new_auto();
    let threshold = auto.update(0.0, false);
    assert!(threshold.is_finite() && threshold > 0.0);
}
