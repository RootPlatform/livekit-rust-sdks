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
        threshold = auto.update(rms, gain_db, gate_open, false);
    }
    threshold
}

fn feed(auto: &mut AutoSensitivity, level_db: f64, seconds: f64, gate_open: bool) {
    feed_gain(auto, level_db, seconds, gate_open, 0.0);
}

/// Frames whose raw capture was digital silence (their level 0, as a muted device delivers).
fn feed_silent(auto: &mut AutoSensitivity, frames: usize, gate_open: bool, gain_db: f64) -> f32 {
    let mut threshold = 0.0;
    for _ in 0..frames {
        threshold = auto.update(0.0, gain_db, gate_open, true);
    }
    threshold
}

/// Seconds of room noise at `level_db` until the open threshold is within 1 dB of level + margin.
fn seconds_to_learn(auto: &mut AutoSensitivity, level_db: f64) -> f64 {
    let target = level_db + AutoSensitivityOptions::default().open_margin_db;
    for frame in 1..=6000u32 {
        let threshold = auto.update(to_rms(level_db), 0.0, false, false);
        if (to_db(threshold) - target).abs() <= 1.0 {
            return f64::from(frame) / 100.0;
        }
    }
    f64::INFINITY
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
    let open = auto.update(to_rms(-50.0), 0.0, false, false);
    let close = auto.update(to_rms(-20.0), 0.0, true, false);
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
    let close = auto.update(to_rms(-100.0), 10.0, true, false);
    assert!(near(to_db(close), auto.close_db() + 10.0, 1e-3));
}

/// Managed AGainChangeMovesTheThresholdAtOnce.
#[test]
fn a_gain_change_moves_the_threshold_at_once() {
    let mut auto = new_auto();
    feed(&mut auto, -50.0, 10.0, false);
    let floor = auto.floor_db();
    let open = auto.open_db();
    let threshold = auto.update(to_rms(-50.0), 15.0, false, false);
    assert!(near(to_db(threshold), open + 15.0, 1e-3));
    assert!(in_range(auto.floor_db(), floor - 0.1, floor + 0.1));
    assert!(to_db(threshold) > -50.0 + 15.0, "the boosted noise stays under the threshold");
}

/// Managed LeakedNoiseStaysUnderTheThresholdUpToTheCap.
#[test]
fn leaked_noise_stays_under_the_threshold_up_to_the_cap() {
    for gain in [0.0, 6.0, AutoSensitivityOptions::default().voice_gain_cap_db] {
        let mut auto = new_auto();
        feed_gain(&mut auto, -100.0, 5.0, false, gain);
        for i in 0..300 {
            let leak_db = -66.0 + 3.0 * (f64::from(i) * 0.37).sin();
            let threshold = auto.update(to_rms(leak_db), gain, false, false);
            assert!(
                leak_db + gain < to_db(threshold),
                "gain {gain}: {leak_db} dB crossed {}",
                to_db(threshold)
            );
        }
    }
}

/// Managed TheMinimumRisesWithAtMostTheCapOfTheGain.
#[test]
fn the_minimum_rises_with_at_most_the_cap_of_the_gain() {
    let options = AutoSensitivityOptions::default();
    let cap = options.voice_gain_cap_db;
    for gain in [0.0, 6.0, cap, cap + 1.0, 18.0] {
        let mut auto = new_auto();
        let open = feed_gain(&mut auto, -100.0, 5.0, false, gain);
        let close = auto.update(to_rms(-100.0), gain, true, false);
        let expected = options.open_minimum_db + gain.min(cap);
        assert!(near(to_db(open), expected, 1e-4), "gain {gain}: {}", to_db(open));
        assert!(near(to_db(close), expected - options.hysteresis_db, 1e-4), "gain {gain}");
        assert!(near(auto.open_db_at(gain) + gain, expected, 1e-9));
    }
    let mut loud = new_auto();
    let open = feed_gain(&mut loud, -45.0, 10.0, false, 18.0);
    assert!(
        near(to_db(open), loud.open_db() + 18.0, 1e-4),
        "above the minimum the whole gain still applies: {}",
        to_db(open)
    );
}

/// Managed ANonFiniteGainCountsAsNone.
#[test]
fn a_non_finite_gain_counts_as_none() {
    let mut auto = new_auto();
    feed(&mut auto, -100.0, 5.0, false);
    for gain in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let threshold = auto.update(to_rms(-100.0), gain, false, false);
        assert!(near(to_db(threshold), auto.open_db(), 1e-3), "{gain}");
    }
}

#[test]
fn reset_starts_over_from_the_initial_floor_with_an_empty_window() {
    let mut auto = new_auto();
    feed(&mut auto, -80.0, 10.0, false);
    auto.reset();
    assert_eq!(auto.floor_db(), AutoSensitivityOptions::default().initial_floor_db);
    let threshold = auto.update(to_rms(-50.0), 0.0, false, false);
    assert!(near(to_db(threshold), auto.open_db(), 1e-3));
    assert!(near(auto.floor_db(), -59.9, 1e-6), "startup rise: {}", auto.floor_db());
    let close = auto.update(to_rms(-20.0), 0.0, true, false);
    assert!(near(to_db(close), auto.close_db(), 1e-3));
    assert!(near(auto.floor_db(), -59.8, 1e-6), "no -80 dB frame left: {}", auto.floor_db());
}

#[test]
fn silence_and_zero_rms_are_floored_at_minus_140_db() {
    assert!(near(to_db(0.0), 20.0 * f64::from(1e-7f32).log10(), 1e-12));
    let mut auto = new_auto();
    let threshold = auto.update(0.0, 0.0, false, false);
    assert!(threshold.is_finite() && threshold > 0.0);
}

#[test]
fn the_minimum_is_54_db_and_the_host_can_move_it() {
    let mut auto = new_auto();
    assert_eq!(auto.options().open_minimum_db, -54.0);
    feed(&mut auto, -100.0, 5.0, false);
    assert!(near(auto.open_db(), -54.0, 1e-9));
    auto.set_open_minimum_db(-40.0);
    let threshold = auto.update(to_rms(-100.0), 0.0, false, false);
    assert!(near(to_db(threshold), -40.0, 1e-4), "{}", to_db(threshold));
}

/// A hardware mute delivers digital silence. Counted as a level it pulled the floor to -90 dB and,
/// after the unmute, the floor climbed back at 1 dB/s (39 s to -50 dB room noise); now those frames
/// teach nothing and the unmute starts a relearn at the startup rate.
#[test]
fn thirty_seconds_of_hardware_mute_then_room_noise_is_learned_within_three_seconds() {
    for learned in [None, Some(-60.0), Some(-70.0)] {
        let mut auto = new_auto();
        if let Some(db) = learned {
            feed(&mut auto, db, 5.0, false);
        }
        let floor = auto.floor_db();
        feed_silent(&mut auto, 3000, false, 0.0);
        assert_eq!(auto.floor_db(), floor, "{learned:?}");
        let seconds = seconds_to_learn(&mut auto, -50.0);
        assert!(seconds <= 3.0, "{learned:?}: {seconds} s");
    }

    let mut counted = new_auto();
    feed(&mut counted, -60.0, 5.0, false);
    for _ in 0..3000 {
        counted.update(0.0, 0.0, false, false);
    }
    assert!(near(counted.floor_db(), AutoSensitivityOptions::default().floor_minimum_db, 1e-9));
    let seconds = seconds_to_learn(&mut counted, -50.0);
    assert!(seconds > 35.0, "silence counted as a level: {seconds} s");
}

#[test]
fn digital_silence_keeps_the_floor_and_the_thresholds() {
    for gain in [0.0, 6.0, 15.0] {
        let mut auto = new_auto();
        feed(&mut auto, -48.0, 10.0, false);
        let floor = auto.floor_db();
        let open = feed_silent(&mut auto, 200, false, gain);
        assert_eq!(auto.floor_db(), floor);
        assert_eq!(open, to_rms(auto.open_db_at(gain) + gain), "gain {gain}");
        let close = feed_silent(&mut auto, 1, true, gain);
        assert_eq!(close, to_rms(auto.close_db_at(gain) + gain), "gain {gain}");
        assert_eq!(auto.floor_db(), floor);
    }
}

#[test]
fn silent_frames_never_enter_the_window_of_recent_levels() {
    let mut auto = new_auto();
    feed(&mut auto, -60.0, 3.0, false);
    feed(&mut auto, -45.0, 2.5, true);
    feed_silent(&mut auto, 30, true, 0.0);
    let floor = auto.floor_db();
    feed(&mut auto, -45.0, 1.0, true);
    assert!(
        in_range(auto.floor_db(), floor + 0.9, floor + 1.1),
        "{} from {floor}",
        auto.floor_db()
    );

    let mut counted = new_auto();
    feed(&mut counted, -60.0, 3.0, false);
    feed(&mut counted, -45.0, 2.5, true);
    for _ in 0..30 {
        counted.update(0.0, 0.0, true, false);
    }
    let floor = counted.floor_db();
    feed(&mut counted, -45.0, 1.0, true);
    assert_eq!(counted.floor_db(), floor, "a counted silence holds the window's minimum down");
}

#[test]
fn a_silence_under_half_a_second_does_not_start_a_relearn() {
    let mut short = new_auto();
    feed(&mut short, -60.0, 5.0, false);
    feed_silent(&mut short, 49, false, 0.0);
    feed(&mut short, -50.0, 1.0, false);
    assert!(in_range(short.floor_db(), -59.1, -58.9), "1 dB/s: {}", short.floor_db());

    let mut long = new_auto();
    feed(&mut long, -60.0, 5.0, false);
    feed_silent(&mut long, 50, false, 0.0);
    feed(&mut long, -50.0, 1.0, false);
    assert!(near(long.floor_db(), -50.0, 1e-4), "10 dB/s: {}", long.floor_db());
}

/// The relearn empties the window: the quietest recent frame before the mute no longer holds the
/// floor down while the gate is open after it.
#[test]
fn the_relearn_empties_the_window_and_restarts_the_startup_rise() {
    let mut auto = new_auto();
    feed(&mut auto, -80.0, 5.0, false);
    feed(&mut auto, -45.0, 0.5, true);
    feed_silent(&mut auto, 100, false, 0.0);
    let floor = auto.floor_db();
    feed(&mut auto, -45.0, 0.5, true);
    assert!(near(auto.floor_db(), floor + 5.0, 1e-6), "{} from {floor}", auto.floor_db());
}

/// The Effects SDK can return near-silence on a live mic: the raw capture decides, never the level.
#[test]
fn a_near_silent_level_from_a_live_capture_is_learned() {
    let mut auto = new_auto();
    feed(&mut auto, -60.0, 5.0, false);
    feed(&mut auto, -125.0, 10.0, false);
    let options = AutoSensitivityOptions::default();
    assert!(near(auto.floor_db(), options.floor_minimum_db, 1e-9));
    assert!(near(auto.open_db(), options.open_minimum_db, 1e-9));
}
